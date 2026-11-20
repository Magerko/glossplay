//! The voice-over at run time. A worker synthesizes the lines just ahead of the playhead and
//! keeps the clips on disk; a player speaks them through a second mpv without video, in step
//! with the film. What plays when is decided by `gp_core::dub_plan`.

use crate::i18n::{Tr, t};
use crate::setup_text as text;
use crate::tts_setup::{self, TtsInstall};
use eframe::egui;
use gp_ai::AiError;
use gp_ai::tts::{Reference, SpeakOptions, SpeechServer, TtsEngine, Wav, parse_wav};
use gp_core::asr_plan::Loudness;
use gp_core::dub_plan::{self, Decision, Item, MAX_LAG, Unit};
use gp_core::{Cue, Ms, loudness, voice_sample};
use gp_media::mpv::Mpv;
use gp_media::tools::{self, ServerCommand, ServerProcess};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

/// How many lines ahead are synthesized, and how far ahead at most.
const AHEAD_LINES: usize = 8;
pub const AHEAD_MS: Ms = 60_000;
const CACHE_LIMIT: u64 = 500 * 1024 * 1024;
const CACHE_KEEP: u64 = 400 * 1024 * 1024;
const MAX_RESTARTS: usize = 2;
/// Playback stops for a line whose clip is not ready yet, the way a video stops to buffer,
/// but not for longer than this.
const MAX_HOLD: Duration = Duration::from_secs(25);
/// A jump of the playhead this far from where it should be is a seek.
const SEEK_JUMP: Ms = 600;
/// Clips are started this much after the exact moment rather than before it: a voice a
/// little late reads as natural, one that starts before the actor does not.
const AIM_LATE: f64 = 80.0;
const TICK: Duration = Duration::from_millis(10);
const SERVER_THREADS: usize = 8;
/// Every request of the installed engine has the same seed, so a line said again sounds the same.
const SEED: u32 = 1;
/// The standard voice is made once, from a description, and then copied like a speaker's.
/// A seed alone does not keep OmniVoice to one speaker: ten lines with seed 42 came out in
/// voices from 113 to 242 Hz.
const STANDARD_TEXT: &str = "Hello there. I am going to read this text slowly and clearly, so that every word can be heard.";
const STANDARD_INSTRUCT: &str = "male, middle-aged, moderate pitch";
const SAMPLE_RATE: u32 = 24_000;
/// Stretches tried as a sample before settling for the standard voice.
const SAMPLE_TRIES: usize = 12;

pub enum Source {
    Installed(TtsInstall),
    /// A speech server of the user's own.
    Remote {
        base_url: String,
        model: String,
    },
}

/// Where a sample of the speaker's voice comes from: the film's sound, read the way speech
/// recognition reads it, and the lines said in it.
pub struct SpeakerSound {
    pub ffmpeg: PathBuf,
    pub media: String,
    pub input: Vec<String>,
    pub audio_index: Option<u32>,
    pub media_key: String,
    pub cues: Vec<Cue>,
}

pub struct DubConfig {
    pub source: Source,
    pub voice: String,
    /// The installed engine speaks in the voice of the film's speaker (see
    /// [`Dub::set_speaker`]) when this is set, in the standard voice otherwise. A server of the
    /// user's own gets neither: the speaker's voice stays on this computer.
    pub copy_speaker: bool,
    /// Voice samples, kept per film so every session speaks in the same voice.
    pub samples_dir: PathBuf,
    pub lang: String,
    pub voice_volume: f64,
    pub mpv_path: String,
    /// Self-test: the voice goes to no audio device, with real playback timing.
    pub silent: bool,
    pub cache_dir: PathBuf,
    /// Server config, log and temporary files.
    pub work_dir: PathBuf,
    /// Self-test: a copy of every clip played goes here.
    pub save_dir: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct Clip {
    pub path: PathBuf,
    pub ms: Ms,
    pub natural_ms: Ms,
}

/// One clip played, for the self-test report.
#[derive(Debug, Clone, Default)]
pub struct Played {
    pub start: Ms,
    /// The video position when the voice actually started.
    pub started_at: Option<Ms>,
    pub clip_ms: Ms,
    pub window: Ms,
    pub compression: f64,
    /// Planned to start after an earlier clip that ran long.
    pub catch_up: bool,
    pub text: String,
}

#[derive(Debug, Clone, Default)]
pub struct Stats {
    pub engine: String,
    pub synth_requests: usize,
    pub cache_hits: usize,
    pub played: Vec<Played>,
    pub skipped: usize,
    pub overlaps: usize,
    pub hold: Duration,
    pub latency_ms: Vec<f64>,
    /// From the start of the voice-over to the first clip ready to play.
    pub first_clip: Option<Duration>,
    pub server_starts: usize,
    /// Whose voice the clips are in, for the self-test report.
    pub voice: String,
}

impl Stats {
    pub fn max_lag(&self) -> Ms {
        self.played
            .iter()
            .filter_map(|p| Some(p.started_at? - p.start))
            .max()
            .unwrap_or(0)
            .max(0)
    }
}

#[derive(Default)]
struct Shared {
    units: Vec<Unit>,
    /// The film playing now, to copy the voice of.
    speaker: Option<Arc<SpeakerSound>>,
    clips: HashMap<Unit, Clip>,
    failed: HashSet<Unit>,
    pos: Ms,
    /// What the worker is busy with, when that is worth showing.
    status: String,
    /// The worker gave up; the voice-over says nothing more until it is turned on again.
    stopped: bool,
    alert: Option<String>,
    holding: bool,
    playing: bool,
    stats: Stats,
}

fn lock(m: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub struct Dub {
    shared: Arc<Mutex<Shared>>,
    cancel: Arc<AtomicBool>,
    server: Arc<Mutex<Option<ServerProcess>>>,
    voice: Arc<Mpv>,
    main: Arc<Mpv>,
}

impl Dub {
    pub fn start(cfg: DubConfig, main: Arc<Mpv>, ctx: egui::Context) -> Result<Dub, String> {
        let volume = format!("{:.3}", dub_plan::voice_volume(cfg.voice_volume));
        let record = crate::record::voice_options();
        let mut options = vec![
            ("vid", "no"),
            ("vo", "null"),
            ("audio-display", "no"),
            ("idle", "yes"),
            ("keep-open", "no"),
            ("terminal", "no"),
            ("input-default-bindings", "no"),
            ("ytdl", "no"),
            ("load-scripts", "no"),
            ("volume", volume.as_str()),
            ("volume-max", "150"),
            ("gapless-audio", "no"),
        ];
        if !record.is_empty() {
            options.extend(record.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        } else if cfg.silent {
            options.push(("mute", "yes"));
            options.push(("ao", "null"));
        }
        let voice = Mpv::new(&cfg.mpv_path, &options)
            .map(Arc::new)
            .map_err(|e| format!("{}: {e}", t(Tr::DubNoAudioOut)))?;
        let _ = std::fs::create_dir_all(&cfg.cache_dir);
        let _ = std::fs::create_dir_all(&cfg.work_dir);
        if let Some(d) = &cfg.save_dir {
            let _ = std::fs::create_dir_all(d);
        }
        let engine_id = match &cfg.source {
            Source::Installed(_) => format!("audio.cpp/{}", tts_setup::MODEL_ID),
            Source::Remote { base_url, model } => {
                SpeechServer::new(base_url, model, &cfg.voice).id()
            }
        };
        let shared = Arc::new(Mutex::new(Shared {
            stats: Stats {
                engine: engine_id.clone(),
                ..Stats::default()
            },
            ..Shared::default()
        }));
        let dub = Dub {
            shared: shared.clone(),
            cancel: Arc::default(),
            server: Arc::default(),
            voice: voice.clone(),
            main: main.clone(),
        };
        let cfg = Arc::new(cfg);
        let started = Instant::now();
        let worker = Worker {
            cfg: cfg.clone(),
            engine_id,
            voice: RefCell::new(None),
            made: std::cell::Cell::new(0),
            shared: shared.clone(),
            cancel: dub.cancel.clone(),
            server: dub.server.clone(),
            ctx: ctx.clone(),
            started,
        };
        std::thread::spawn(move || worker.run());
        let player = Player {
            cfg,
            main,
            voice,
            shared,
            cancel: dub.cancel.clone(),
            ctx,
        };
        std::thread::spawn(move || player.run());
        Ok(dub)
    }

    /// The lines around the playhead, as the subtitles have them now.
    /// The film whose lines come next: its voice is chosen before the first of them.
    pub fn set_speaker(&self, sound: SpeakerSound) {
        lock(&self.shared).speaker = Some(Arc::new(sound));
    }

    /// The film the voice-over knows of.
    pub fn speaker_key(&self) -> Option<String> {
        lock(&self.shared)
            .speaker
            .as_ref()
            .map(|s| s.media_key.clone())
    }

    pub fn set_units(&self, units: Vec<Unit>) {
        let mut s = lock(&self.shared);
        if s.units != units {
            s.units = units;
        }
    }

    /// The voice volume in percent, as the settings have it.
    pub fn set_voice_volume(&self, percent: f64) {
        self.voice
            .set_f64("volume", dub_plan::voice_volume(percent));
    }

    /// mpv's volume of the voice player.
    pub fn voice_volume(&self) -> f64 {
        self.voice.get_f64("volume").unwrap_or(0.0)
    }

    /// Where the voice player is inside its clip, in seconds.
    pub fn voice_pos(&self) -> Option<f64> {
        self.voice.get_f64("time-pos")
    }

    /// Where the voice goes: the audio output in use and whether it is muted.
    pub fn voice_output(&self) -> (String, bool) {
        (
            self.voice.get_string("current-ao").unwrap_or_default(),
            self.voice.get_flag("mute").unwrap_or(false),
        )
    }

    pub fn playing(&self) -> bool {
        lock(&self.shared).playing
    }

    pub fn holding(&self) -> bool {
        lock(&self.shared).holding
    }

    pub fn stats(&self) -> Stats {
        lock(&self.shared).stats.clone()
    }

    pub fn clips_ready(&self) -> usize {
        let s = lock(&self.shared);
        s.units.iter().filter(|u| s.clips.contains_key(*u)).count()
    }

    /// Every clip made or found in the cache since the voice-over was turned on.
    pub fn clips_total(&self) -> usize {
        lock(&self.shared).clips.len()
    }

    /// The text for the header, empty when there is nothing to say.
    pub fn status(&self) -> String {
        let s = lock(&self.shared);
        if !s.status.is_empty() {
            return s.status.clone();
        }
        if s.holding {
            return t(Tr::DubPreparing).to_owned();
        }
        t(Tr::DubOn).to_owned()
    }

    pub fn take_alert(&self) -> Option<String> {
        lock(&self.shared).alert.take()
    }

    pub fn server_pid(&self) -> Option<u32> {
        self.server
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(ServerProcess::pid)
    }
}

impl Drop for Dub {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
        // Killing the server also ends a request the worker may be waiting on.
        drop(self.server.lock().unwrap_or_else(|e| e.into_inner()).take());
        self.voice.command_async(&["stop"]);
        if lock(&self.shared).holding {
            self.main.set_flag("pause", false);
        }
    }
}

pub fn cache_key(engine: &str, voice: &str, lang: &str, text: &str, target: Option<Ms>) -> String {
    let target = target.map_or("-".to_owned(), |t| t.to_string());
    let digest = Sha256::digest(format!(
        "glossplay-voice-1\0{engine}\0{voice}\0{lang}\0{text}\0{target}"
    ));
    digest[..16].iter().map(|b| format!("{b:02x}")).collect()
}

fn read_clip(path: &Path) -> Option<Wav> {
    let wav = parse_wav(std::fs::read(path).ok()?).ok()?;
    // The modification time is what the size limit goes by, so a clip in use is kept.
    if let Ok(f) = std::fs::File::options().write(true).open(path) {
        let _ = f.set_modified(SystemTime::now());
    }
    Some(wav)
}

fn write_clip(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let temp = path.with_extension("part");
    std::fs::write(&temp, bytes)?;
    std::fs::rename(temp, path)
}

/// Deletes the oldest clips once the cache is over its limit.
pub fn prune_cache(dir: &Path, limit: u64, keep: u64) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(SystemTime, u64, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            m.is_file().then(|| {
                (
                    m.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                    m.len(),
                    e.path(),
                )
            })
        })
        .collect();
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    if total <= limit {
        return;
    }
    files.sort();
    for (_, len, path) in files {
        if total <= keep {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total -= len;
        }
    }
}

enum Failure {
    /// This line could not be spoken; the next one may well be.
    Line(String),
    /// The engine is gone for good.
    Engine(String),
    Cancelled,
}

/// The voice every clip of a session is spoken in.
#[derive(Clone, Default)]
struct Voice {
    /// Clips in another voice are other clips in the cache.
    id: String,
    reference: Option<Arc<Reference>>,
    speed: Option<f32>,
    speaker: bool,
    about: String,
}

impl Voice {
    fn copied(wav: &[u8], text: &str, speed: Option<f32>, speaker: bool, about: String) -> Voice {
        let digest =
            Sha256::digest([wav, text.as_bytes(), format!("{speed:?}").as_bytes()].concat());
        let hex: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
        Voice {
            id: format!("{}-{hex}", if speaker { "speaker" } else { "standard" }),
            reference: Some(Arc::new(Reference::new(wav, text))),
            speed,
            speaker,
            about,
        }
    }
}

/// The loudness a voice sample is brought to, in LUFS.
const SAMPLE_LUFS: f64 = -23.0;

/// The loudness of every clip, in LUFS: about 13 LU over a lecture at the default original
/// volume of 35%, so the voice is clearly on top without drowning the speaker. The engine gives
/// each voice a loudness of its own: at one volume setting the standard voice came out 3-7 dB
/// quieter than one copied from a lecture.
const CLIP_LUFS: f64 = -16.0;

/// Peaks of samples and clips stay under this, in dBFS.
const CEILING: f64 = -1.0;

/// Bumped when clips are made differently, so the ones cached before are made again.
const CLIP_MAKING: &str = "lufs-16";

/// `wav` brought to `lufs`; a file that is not mono 16-bit PCM is left as it is.
fn leveled(wav: &[u8], lufs: f64) -> Option<Vec<u8>> {
    let (mut pcm, rate) = wav_pcm(wav)?;
    loudness::normalize(&mut pcm, rate, lufs, CEILING);
    Some(wav_file(&pcm, rate))
}

/// The samples of a mono 16-bit WAV file, with its rate; `None` for any other kind.
fn wav_pcm(wav: &[u8]) -> Option<(Vec<i16>, u32)> {
    let u32_at = |i: usize| {
        wav.get(i..i + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let u16_at = |i: usize| wav.get(i..i + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let (mut at, mut format) = (12, None);
    while let (Some(id), Some(len)) = (wav.get(at..at + 4), u32_at(at + 4)) {
        let body = at + 8;
        match id {
            b"fmt " => format = Some((u16_at(body + 2)?, u32_at(body + 4)?, u16_at(body + 14)?)),
            b"data" => {
                let (channels, rate, bits) = format?;
                if channels != 1 || bits != 16 {
                    return None;
                }
                let end = (body + len as usize).min(wav.len());
                let pcm = wav[body..end]
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|b| i16::from_le_bytes(*b))
                    .collect();
                return Some((pcm, rate));
            }
            _ => {}
        }
        at = body + len as usize + (len as usize & 1);
    }
    None
}

/// A mono 16-bit WAV file.
fn wav_file(pcm: &[i16], rate: u32) -> Vec<u8> {
    let data = (pcm.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data.to_le_bytes());
    for s in pcm {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// The sample of a film's speaker as saved: where it was taken and what is said in it.
#[derive(serde::Serialize, serde::Deserialize)]
struct SavedSample {
    start: Ms,
    end: Ms,
    text: String,
    speed: f32,
}

/// The files a film's sample is kept in.
fn sample_files(dir: &Path, media_key: &str) -> (PathBuf, PathBuf) {
    let digest = Sha256::digest(media_key.as_bytes());
    let stem: String = digest[..12].iter().map(|b| format!("{b:02x}")).collect();
    (
        dir.join(format!("{stem}.wav")),
        dir.join(format!("{stem}.json")),
    )
}

/// A sample of the speaker from the film: the stretch the transcript offers that sounds
/// like speech the engine can copy, saved for the next time. `None` when there is none.
fn speaker_sample(
    sound: &SpeakerSound,
    dir: &Path,
    cancelled: &dyn Fn() -> bool,
) -> Option<(Vec<u8>, SavedSample)> {
    let (wav_path, json_path) = sample_files(dir, &sound.media_key);
    if let (Ok(wav), Ok(json)) = (std::fs::read(&wav_path), std::fs::read(&json_path))
        && let Ok(saved) = serde_json::from_slice::<SavedSample>(&json)
    {
        return Some((wav, saved));
    }
    for span in voice_sample::candidates(&sound.cues)
        .into_iter()
        .take(SAMPLE_TRIES)
    {
        if cancelled() {
            return None;
        }
        let from = span.start - voice_sample::LEAD.min(span.start);
        let Ok(pcm) = tools::extract_pcm_at(
            &sound.ffmpeg,
            &sound.media,
            &sound.input,
            sound.audio_index,
            from,
            span.end - from + span.room,
            SAMPLE_RATE,
        ) else {
            continue;
        };
        let Some((start, end)) = voice_sample::sample_cut(&Loudness::new(&pcm, SAMPLE_RATE), &span)
        else {
            continue;
        };
        let at = |ms: Ms| ((ms * Ms::from(SAMPLE_RATE) / 1000) as usize).min(pcm.len());
        let mut pcm = pcm[at(start)..at(end)].to_vec();
        if !voice_sample::sound_fits(&Loudness::new(&pcm, SAMPLE_RATE)) {
            continue;
        }
        loudness::normalize(&mut pcm, SAMPLE_RATE, SAMPLE_LUFS, CEILING);
        let wav = wav_file(&pcm, SAMPLE_RATE);
        let saved = SavedSample {
            start: from + start,
            end: from + end,
            speed: span.speed(),
            text: span.text,
        };
        let _ = std::fs::create_dir_all(dir);
        let _ = write_clip(&wav_path, &wav);
        if let Ok(json) = serde_json::to_vec(&saved) {
            let _ = write_clip(&json_path, &json);
        }
        return Some((wav, saved));
    }
    None
}

struct Worker {
    cfg: Arc<DubConfig>,
    engine_id: String,
    /// The voice of the film playing, and that film.
    voice: RefCell<Option<(String, Voice)>>,
    /// Clips made in that voice.
    made: std::cell::Cell<usize>,
    shared: Arc<Mutex<Shared>>,
    cancel: Arc<AtomicBool>,
    server: Arc<Mutex<Option<ServerProcess>>>,
    ctx: egui::Context,
    started: Instant,
}

impl Worker {
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    fn set_status(&self, status: &str) {
        let mut s = lock(&self.shared);
        if s.status != status {
            s.status = status.to_owned();
            drop(s);
            self.ctx.request_repaint();
        }
    }

    /// The first line near the playhead without a clip.
    fn next_job(&self) -> Option<Unit> {
        let s = lock(&self.shared);
        let pos = s.pos;
        let mut ahead: Vec<&Unit> = s
            .units
            .iter()
            .filter(|u| u.start >= pos - 300 && u.start <= pos + AHEAD_MS)
            .collect();
        ahead.sort_by_key(|u| u.start);
        ahead
            .into_iter()
            .take(AHEAD_LINES)
            .find(|u| !s.clips.contains_key(*u) && !s.failed.contains(*u))
            .cloned()
    }

    fn run(self) {
        prune_cache(&self.cfg.cache_dir, CACHE_LIMIT, CACHE_KEEP);
        let mut engine: Option<SpeechServer> = None;
        let (mut restarts, mut written) = (0, 0);
        while !self.cancelled() {
            let Some(unit) = self.next_job() else {
                std::thread::sleep(Duration::from_millis(40));
                continue;
            };
            match self.produce(&unit, &mut engine, &mut restarts, &mut written) {
                Ok(clip) => {
                    let mut s = lock(&self.shared);
                    s.stats.first_clip.get_or_insert(self.started.elapsed());
                    s.clips.insert(unit, clip);
                }
                Err(Failure::Line(e)) => {
                    let mut s = lock(&self.shared);
                    s.failed.insert(unit);
                    s.alert = Some(text::dub_line_failed(&e));
                }
                Err(Failure::Engine(e)) => {
                    let mut s = lock(&self.shared);
                    s.stopped = true;
                    s.status = text::dub_stopped(&e);
                    s.alert = Some(s.status.clone());
                    drop(s);
                    drop(self.server.lock().unwrap_or_else(|e| e.into_inner()).take());
                    self.ctx.request_repaint();
                    return;
                }
                Err(Failure::Cancelled) => return,
            }
            self.ctx.request_repaint();
            if written >= 20 {
                written = 0;
                prune_cache(&self.cfg.cache_dir, CACHE_LIMIT, CACHE_KEEP);
            }
        }
    }

    fn clip_path(&self, target: Option<Ms>, text: &str) -> PathBuf {
        let voice = self.voice.borrow().as_ref().map(|(_, v)| v.id.clone());
        let voice = match voice {
            Some(id) if !id.is_empty() => format!("{}|{id}|{CLIP_MAKING}", self.cfg.voice),
            _ => format!("{}|{CLIP_MAKING}", self.cfg.voice),
        };
        let key = cache_key(&self.engine_id, &voice, &self.cfg.lang, text, target);
        self.cfg.cache_dir.join(format!("{key}.wav"))
    }

    /// The voice of this session, chosen before its first clip.
    fn choose_voice(
        &self,
        sound: Option<&SpeakerSound>,
        engine: &mut Option<SpeechServer>,
        restarts: &mut usize,
    ) -> Result<Voice, Failure> {
        if !matches!(self.cfg.source, Source::Installed(_)) {
            return Ok(Voice {
                about: "server".into(),
                ..Voice::default()
            });
        }
        if let Some(sound) = sound.filter(|_| self.cfg.copy_speaker) {
            self.set_status(t(Tr::DubVoiceSample));
            let found = speaker_sample(sound, &self.cfg.samples_dir, &|| self.cancelled());
            self.set_status("");
            if self.cancelled() {
                return Err(Failure::Cancelled);
            }
            if let Some((wav, s)) = found {
                let pace = voice_sample::Span {
                    start: s.start,
                    end: s.end,
                    text: s.text.clone(),
                    room: 0,
                }
                .pace();
                let about = format!(
                    "speaker {:.2}-{:.2} s, pace {pace:.1}, speed {:.2} | {}",
                    s.start as f64 / 1000.0,
                    s.end as f64 / 1000.0,
                    s.speed,
                    s.text
                );
                return Ok(Voice::copied(&wav, &s.text, Some(s.speed), true, about));
            }
        }
        self.standard_voice(engine, restarts)
    }

    /// The standard voice, made on the first use and kept.
    fn standard_voice(
        &self,
        engine: &mut Option<SpeechServer>,
        restarts: &mut usize,
    ) -> Result<Voice, Failure> {
        let path = self.cfg.samples_dir.join("standard.wav");
        let wav = match std::fs::read(&path) {
            Ok(wav) if parse_wav(wav.clone()).is_ok() => wav,
            _ => {
                if engine.is_none() {
                    *engine = Some(self.start_engine(restarts)?);
                }
                let opts = SpeakOptions {
                    seed: Some(SEED),
                    instruct: Some(STANDARD_INSTRUCT.into()),
                    ..SpeakOptions::default()
                };
                let tts = engine.as_ref().expect("just started");
                let wav = tts
                    .synthesize(STANDARD_TEXT, "en", &opts)
                    .map_err(|e| Failure::Line(e.to_string()))?;
                let bytes = leveled(&wav.bytes, SAMPLE_LUFS).unwrap_or(wav.bytes);
                let _ = std::fs::create_dir_all(&self.cfg.samples_dir);
                let _ = write_clip(&path, &bytes);
                bytes
            }
        };
        Ok(Voice::copied(
            &wav,
            STANDARD_TEXT,
            None,
            false,
            "standard".into(),
        ))
    }

    fn produce(
        &self,
        unit: &Unit,
        engine: &mut Option<SpeechServer>,
        restarts: &mut usize,
        written: &mut usize,
    ) -> Result<Clip, Failure> {
        let sound = lock(&self.shared).speaker.clone();
        let key = sound
            .as_ref()
            .map_or(String::new(), |s| s.media_key.clone());
        if self.voice.borrow().as_ref().is_none_or(|(k, _)| *k != key) {
            let voice = self.choose_voice(sound.as_deref(), engine, restarts)?;
            lock(&self.shared).stats.voice = voice.about.clone();
            *self.voice.borrow_mut() = Some((key.clone(), voice));
            self.made.set(0);
        }
        let first_in_speaker =
            || self.made.get() == 0 && self.voice.borrow().as_ref().is_some_and(|(_, v)| v.speaker);
        let mut result = self.produce_in_voice(unit, engine, restarts, written);
        // The first request after the engine starts has been dropped once with the connection
        // reset; that is no reason to give up the speaker's voice for the whole film.
        if matches!(result, Err(Failure::Line(_))) && first_in_speaker() {
            result = self.produce_in_voice(unit, engine, restarts, written);
        }
        let result = match result {
            // A sample the engine cannot work with shows on the first line; the film is then
            // spoken in the standard voice, never in two.
            Err(Failure::Line(e)) if first_in_speaker() => {
                let voice = self.standard_voice(engine, restarts)?;
                lock(&self.shared).stats.voice = format!("standard (the sample failed: {e})");
                *self.voice.borrow_mut() = Some((key, voice));
                self.produce_in_voice(unit, engine, restarts, written)
            }
            other => other,
        };
        if result.is_ok() {
            self.made.set(self.made.get() + 1);
        }
        result
    }

    fn produce_in_voice(
        &self,
        unit: &Unit,
        engine: &mut Option<SpeechServer>,
        restarts: &mut usize,
        written: &mut usize,
    ) -> Result<Clip, Failure> {
        let natural_path = self.clip_path(None, &unit.text);
        let natural = self.clip(&natural_path, unit, None, engine, restarts, written)?;
        let Some(request) = dub_plan::fit(natural, unit.window) else {
            return Ok(Clip {
                path: natural_path,
                ms: natural,
                natural_ms: natural,
            });
        };
        let path = self.clip_path(Some(request), &unit.text);
        let ms = self.clip(&path, unit, Some(request), engine, restarts, written)?;
        Ok(Clip {
            path,
            ms,
            natural_ms: natural,
        })
    }

    /// The length of the clip at `path`, synthesizing it first if the cache lacks it.
    fn clip(
        &self,
        path: &Path,
        unit: &Unit,
        target: Option<Ms>,
        engine: &mut Option<SpeechServer>,
        restarts: &mut usize,
        written: &mut usize,
    ) -> Result<Ms, Failure> {
        if let Some(wav) = read_clip(path) {
            lock(&self.shared).stats.cache_hits += 1;
            return Ok(wav.duration_ms);
        }
        let voice = self
            .voice
            .borrow()
            .as_ref()
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        let installed = matches!(self.cfg.source, Source::Installed(_));
        let opts = SpeakOptions {
            target_ms: target,
            speed: voice.speed,
            seed: installed.then_some(SEED),
            reference: voice.reference,
            instruct: None,
        };
        let mut retried = false;
        loop {
            if self.cancelled() {
                return Err(Failure::Cancelled);
            }
            if engine.is_none() {
                *engine = Some(self.start_engine(restarts)?);
            }
            let tts = engine.as_ref().expect("just started");
            lock(&self.shared).stats.synth_requests += 1;
            match tts.synthesize(&unit.text, &self.cfg.lang, &opts) {
                Ok(wav) if wav.duration_ms > 0 => {
                    if let Source::Installed(i) = &self.cfg.source
                        && !i.warmed_up
                    {
                        tts_setup::mark_warmed_up();
                    }
                    self.set_status("");
                    let bytes = leveled(&wav.bytes, CLIP_LUFS);
                    let _ = write_clip(path, bytes.as_deref().unwrap_or(&wav.bytes));
                    *written += 1;
                    return Ok(wav.duration_ms);
                }
                Ok(_) => return Err(Failure::Line("an empty clip".into())),
                Err(_) if self.cancelled() => return Err(Failure::Cancelled),
                Err(e) if self.server_died() => {
                    *engine = None;
                    if *restarts >= MAX_RESTARTS {
                        return Err(Failure::Engine(text::dub_engine_quit(*restarts + 1)));
                    }
                    *restarts += 1;
                    let _ = e;
                }
                Err(e) if e.is_transient() && !retried => {
                    retried = true;
                    std::thread::sleep(Duration::from_millis(500));
                }
                Err(AiError::Network(e)) if matches!(self.cfg.source, Source::Remote { .. }) => {
                    return Err(Failure::Engine(e));
                }
                Err(e) => return Err(Failure::Line(e.to_string())),
            }
        }
    }

    /// Whether the server this worker started is gone. A failed request can come a moment
    /// before the process is reported dead, so I give it a second.
    fn server_died(&self) -> bool {
        if !matches!(self.cfg.source, Source::Installed(_)) {
            return false;
        }
        let since = Instant::now();
        loop {
            let gone = self
                .server
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_mut()
                .is_none_or(ServerProcess::has_exited);
            if gone {
                return true;
            }
            if since.elapsed() > Duration::from_secs(1) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn start_engine(&self, restarts: &mut usize) -> Result<SpeechServer, Failure> {
        let install = match &self.cfg.source {
            Source::Remote { base_url, model } => {
                return Ok(SpeechServer::new(base_url, model, &self.cfg.voice));
            }
            Source::Installed(i) => i,
        };
        let (Some(program), Some(kind)) = (&install.server, install.engine) else {
            return Err(Failure::Engine(t(Tr::TtsNoEngineHere).into()));
        };
        loop {
            let note = if install.warmed_up {
                String::new()
            } else {
                format!(" — {}", t(Tr::DubFirstStart))
            };
            self.set_status(&format!("{}{note}", t(Tr::DubStarting)));
            let url = {
                let mut slot = self.server.lock().unwrap_or_else(|e| e.into_inner());
                // Checked under the lock: dropping the voice-over empties the slot after
                // setting the flag, and must not leave a fresh server behind.
                if self.cancelled() {
                    return Err(Failure::Cancelled);
                }
                if slot.as_mut().is_none_or(ServerProcess::has_exited) {
                    let process = self
                        .spawn(program, kind, &install.model)
                        .map_err(|e| Failure::Engine(text::dub_cannot_start(&e)))?;
                    lock(&self.shared).stats.server_starts += 1;
                    *slot = Some(process);
                }
                slot.as_ref().expect("just started").url.clone()
            };
            let tts = SpeechServer::new(&url, tts_setup::MODEL_ID, &self.cfg.voice);
            let started = Instant::now();
            // Loading the model takes a few seconds; the first start on a machine compiles
            // shaders on top of that.
            while !tts.is_ready() {
                if self.cancelled() {
                    return Err(Failure::Cancelled);
                }
                let died = self
                    .server
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .as_mut()
                    .is_none_or(ServerProcess::has_exited);
                if died || started.elapsed() > Duration::from_secs(180) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(150));
            }
            if tts.is_ready() {
                return Ok(tts);
            }
            drop(self.server.lock().unwrap_or_else(|e| e.into_inner()).take());
            if *restarts >= MAX_RESTARTS {
                return Err(Failure::Engine(t(Tr::DubEngineGone).into()));
            }
            *restarts += 1;
        }
    }

    fn spawn(
        &self,
        program: &Path,
        kind: tts_setup::Engine,
        model: &Path,
    ) -> Result<ServerProcess, gp_media::MediaError> {
        let port = tools::free_port()?;
        let dir = &self.cfg.work_dir;
        let config = dir.join("server.json");
        let threads = crate::asr_setup::physical_cores().clamp(1, SERVER_THREADS);
        let json = tts_setup::server_config(kind, model, port, threads);
        std::fs::write(&config, json.to_string())?;
        // The server unpacks parts of the model into TEMP; keeping them with the rest of the
        // player's data keeps them off a system drive that may be small.
        let temp = dir.join("tmp");
        std::fs::create_dir_all(&temp)?;
        ServerProcess::start(ServerCommand {
            name: crate::tts_ui::SERVER_NAME,
            program,
            args: tts_setup::server_args(&config, &dir.join("server.log")),
            envs: vec![("TEMP", temp.clone().into()), ("TMP", temp.into())],
            port,
        })
    }
}

/// The video position between the updates mpv makes once a frame.
struct Clock {
    raw: f64,
    at: Instant,
}

impl Clock {
    fn pos(&mut self, raw: f64, paused: bool, speed: f64) -> Ms {
        if raw != self.raw {
            self.raw = raw;
            self.at = Instant::now();
        }
        let extra = if paused {
            0.0
        } else {
            self.at
                .elapsed()
                .min(Duration::from_millis(100))
                .as_secs_f64()
                * speed
        };
        ((raw + extra) * 1000.0).round() as Ms
    }
}

struct Current {
    unit: Unit,
    clip_ms: Ms,
    cmd_at: Instant,
    cmd_pos: Ms,
    started_at: Option<Ms>,
    index: usize,
}

struct Hold {
    unit: Unit,
    since: Instant,
}

struct Player {
    cfg: Arc<DubConfig>,
    main: Arc<Mpv>,
    voice: Arc<Mpv>,
    shared: Arc<Mutex<Shared>>,
    cancel: Arc<AtomicBool>,
    ctx: egui::Context,
}

impl Player {
    fn run(self) {
        let mut clock = Clock {
            raw: f64::NAN,
            at: Instant::now(),
        };
        let mut st = State {
            handled: HashSet::new(),
            floor: Ms::MIN,
            current: None,
            latency_ms: 30.0,
            hold: None,
            overridden: None,
            last: None,
            voice_paused: None,
            voice_muted: None,
            voice_speed: 1.0,
            last_end: None,
        };
        while !self.cancel.load(Ordering::SeqCst) {
            std::thread::sleep(TICK);
            let Some(raw) = self.main.get_f64("time-pos") else {
                st.last = None;
                continue;
            };
            let paused = self.main.get_flag("pause").unwrap_or(true);
            let speed = self.main.get_f64("speed").unwrap_or(1.0).max(0.01);
            let pos = clock.pos(raw, paused, speed);
            let muted = self.cfg.silent || self.main.get_flag("mute").unwrap_or(false);
            if st.voice_muted != Some(muted) {
                self.voice.set_flag("mute", muted);
                st.voice_muted = Some(muted);
            }
            self.step(&mut st, pos, paused, speed);
        }
        self.voice.command_async(&["stop"]);
    }

    fn step(&self, st: &mut State, pos: Ms, paused: bool, speed: f64) {
        let now = Instant::now();
        let jumped = st.last.is_none_or(|(p, t): (Ms, Instant)| {
            let expected = p + if paused {
                0
            } else {
                (t.elapsed().as_secs_f64() * 1000.0 * speed) as Ms
            };
            (pos - expected).abs() > SEEK_JUMP
        });
        st.last = Some((pos, now));
        if jumped {
            // A line the playhead lands in the first moments of still gets its voice, so a
            // click on a line in the transcript is spoken.
            st.floor = pos - 300;
            st.last_end = None;
            if st.current.take().is_some() {
                self.voice.command(&["stop"]).ok();
            }
            if st.hold.is_some() {
                self.release_hold(st, true);
            }
        }
        lock(&self.shared).pos = pos;
        if st.voice_paused != Some(paused) {
            self.voice.set_flag("pause", paused);
            st.voice_paused = Some(paused);
        }
        if (st.voice_speed - speed).abs() > 1e-6 {
            self.voice.set_f64("speed", speed);
            st.voice_speed = speed;
        }
        self.track_current(st, pos, speed);

        let lead = st.latency_ms * speed - AIM_LATE;
        let (next, items, stopped) = {
            let s = lock(&self.shared);
            let mut next: Vec<&Unit> = s
                .units
                .iter()
                .filter(|u| {
                    u.start >= st.floor && !st.handled.contains(*u) && !s.failed.contains(*u)
                })
                .collect();
            next.sort_by_key(|u| u.start);
            let items: Vec<(Unit, Clip)> = next
                .iter()
                .map_while(|u| Some(((*u).clone(), s.clips.get(*u)?.clone())))
                .collect();
            (next.first().map(|u| (*u).clone()), items, s.stopped)
        };
        let Some(first) = next else {
            return;
        };
        if first.start + MAX_LAG < pos && st.current.as_ref().is_none_or(|c| c.unit != first) {
            st.handled.insert(first);
            lock(&self.shared).stats.skipped += 1;
            return;
        }
        if items.is_empty() {
            if pos as f64 + lead >= first.start as f64 {
                self.wait_for_clip(st, first, paused, stopped);
            }
            return;
        }
        if st.hold.is_some() {
            self.release_hold(st, true);
            return;
        }
        let free_at = st.current.as_ref().map_or(Ms::MIN, |c| {
            c.started_at.unwrap_or(c.cmd_pos + st.latency_ms as Ms) + c.clip_ms
        });
        let plan: Vec<Item> = items
            .iter()
            .map(|(u, c)| Item {
                start: u.start,
                len: c.ms,
            })
            .collect();
        match dub_plan::schedule(free_at, &plan)[0] {
            Decision::Skip => {
                st.handled.insert(first);
                lock(&self.shared).stats.skipped += 1;
            }
            Decision::Play { at, lag } => {
                if st.current.is_none() && !paused && pos as f64 + lead >= at as f64 {
                    let (unit, clip) = items.into_iter().next().expect("not empty");
                    // Landing inside a line after a seek also makes its voice late.
                    let late = lag > 0 || pos - at > 200;
                    self.play(st, unit, clip, pos, late);
                }
            }
        }
    }

    /// Follows the clip that is playing: when it really started, and when it is over.
    fn track_current(&self, st: &mut State, pos: Ms, speed: f64) {
        let Some(c) = &mut st.current else {
            lock(&self.shared).playing = false;
            return;
        };
        let idle = self.voice.get_flag("idle-active").unwrap_or(true);
        if c.started_at.is_none() {
            match self.voice.get_f64("time-pos") {
                Some(t) if t > 0.0 && !idle => {
                    let started_at = pos - (t * 1000.0 * speed).round() as Ms;
                    c.started_at = Some(started_at);
                    let latency = c.cmd_at.elapsed().as_secs_f64() * 1000.0 - t * 1000.0;
                    st.latency_ms = (0.7 * st.latency_ms + 0.3 * latency).clamp(0.0, 500.0);
                    let mut s = lock(&self.shared);
                    s.stats.latency_ms.push(latency);
                    s.playing = true;
                    if let Some(prev_end) = st.last_end
                        && started_at < prev_end - 40
                    {
                        s.stats.overlaps += 1;
                    }
                    st.last_end = Some(started_at + c.clip_ms);
                    if let Some(p) = s.stats.played.get_mut(c.index) {
                        p.started_at = Some(started_at);
                    }
                    crate::record::event(
                        "clip_started",
                        serde_json::json!({"line": c.unit.start, "started_at": started_at,
                            "pos": pos, "voice_pos": t}),
                    );
                }
                _ if c.cmd_at.elapsed() > Duration::from_secs(3) => {
                    st.current = None;
                    lock(&self.shared).playing = false;
                }
                _ => {}
            }
            return;
        }
        if idle {
            st.current = None;
            lock(&self.shared).playing = false;
        }
    }

    fn play(&self, st: &mut State, unit: Unit, clip: Clip, pos: Ms, catch_up: bool) {
        if !self.voice.get_flag("idle-active").unwrap_or(true) {
            // Never two voices at once: whatever still plays is cut, and counted.
            lock(&self.shared).stats.overlaps += 1;
        }
        let path = clip.path.to_string_lossy().into_owned();
        let record = crate::record::clip_file(unit.start);
        if let Some(file) = &record {
            self.voice.set("ao-pcm-file", file);
        }
        if self.voice.command(&["loadfile", &path, "replace"]).is_err() {
            st.handled.insert(unit);
            return;
        }
        if let Some(dir) = &self.cfg.save_dir {
            let name = format!("{:07}.wav", unit.start);
            let _ = std::fs::copy(&clip.path, dir.join(&name));
            let line = format!("{name}\t{}\t{}\t{}\n", unit.start, clip.ms, unit.text);
            let _ = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("clips.tsv"))
                .and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()));
        }
        let index = {
            let mut s = lock(&self.shared);
            s.stats.played.push(Played {
                start: unit.start,
                started_at: None,
                clip_ms: clip.ms,
                window: unit.window,
                compression: clip.natural_ms as f64 / clip.ms.max(1) as f64,
                catch_up,
                text: unit.text.clone(),
            });
            s.stats.played.len() - 1
        };
        if let Some(file) = record {
            crate::record::event(
                "clip",
                serde_json::json!({"line": unit.start, "pos": pos, "file": file,
                    "clip": path, "clip_ms": clip.ms, "natural_ms": clip.natural_ms,
                    "window_ms": unit.window, "catch_up": catch_up, "text": unit.text}),
            );
        }
        st.handled.insert(unit.clone());
        st.current = Some(Current {
            unit,
            clip_ms: clip.ms,
            cmd_at: Instant::now(),
            cmd_pos: pos,
            started_at: None,
            index,
        });
    }

    /// The line is due and its clip is not there: playback waits for it, unless the engine
    /// has given up, the wait has gone on too long, or the user pressed play meanwhile.
    fn wait_for_clip(&self, st: &mut State, unit: Unit, paused: bool, stopped: bool) {
        if let Some(h) = &st.hold {
            if h.unit != unit {
                self.release_hold(st, true);
            } else if !paused {
                // Playing again, and not by me: the user does not want to wait.
                st.overridden = Some(unit);
                self.release_hold(st, false);
            } else if h.since.elapsed() > MAX_HOLD {
                st.handled.insert(unit);
                lock(&self.shared).stats.skipped += 1;
                self.release_hold(st, true);
            }
            return;
        }
        if stopped || paused || st.overridden.as_ref() == Some(&unit) {
            return;
        }
        self.main.set_flag("pause", true);
        st.hold = Some(Hold {
            unit,
            since: Instant::now(),
        });
        lock(&self.shared).holding = true;
        self.ctx.request_repaint();
    }

    fn release_hold(&self, st: &mut State, resume: bool) {
        let since = st.hold.take().map(|h| h.since);
        let mut s = lock(&self.shared);
        if let Some(since) = since {
            s.stats.hold += since.elapsed();
        }
        if s.holding && resume {
            self.main.set_flag("pause", false);
        }
        s.holding = false;
        drop(s);
        self.ctx.request_repaint();
    }
}

struct State {
    /// Lines played or given up on.
    handled: HashSet<Unit>,
    /// Lines starting before this are behind the playhead since the last seek.
    floor: Ms,
    current: Option<Current>,
    /// From `loadfile` to the voice playing, smoothed; clips are started this much early.
    latency_ms: f64,
    hold: Option<Hold>,
    overridden: Option<Unit>,
    last: Option<(Ms, Instant)>,
    voice_paused: Option<bool>,
    voice_muted: Option<bool>,
    voice_speed: f64,
    last_end: Option<Ms>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_keys_change_with_everything_that_changes_the_sound() {
        let k = |e: &str, v: &str, l: &str, t: &str, d: Option<Ms>| cache_key(e, v, l, t, d);
        let base = k("audio.cpp/omnivoice", "", "ru", "Птицы", None);
        assert_eq!(base.len(), 32);
        assert_eq!(base, k("audio.cpp/omnivoice", "", "ru", "Птицы", None));
        for other in [
            k("other", "", "ru", "Птицы", None),
            k("audio.cpp/omnivoice", "anna", "ru", "Птицы", None),
            k("audio.cpp/omnivoice", "", "uk", "Птицы", None),
            k("audio.cpp/omnivoice", "", "ru", "Птицы,", None),
            k("audio.cpp/omnivoice", "", "ru", "Птицы", Some(2_000)),
        ] {
            assert_ne!(base, other);
        }
    }

    #[test]
    fn the_cache_drops_its_oldest_clips_past_the_limit() {
        let dir = gp_core::scratch::TempDir::new("gp-dub-cache");
        let old = SystemTime::now() - Duration::from_secs(3600);
        for (i, name) in ["a.wav", "b.wav", "c.wav", "d.wav"].iter().enumerate() {
            let path = dir.join(name);
            std::fs::write(&path, vec![0u8; 100]).unwrap();
            let f = std::fs::File::options().write(true).open(&path).unwrap();
            f.set_modified(old + Duration::from_secs(i as u64 * 60))
                .unwrap();
        }
        prune_cache(&dir, 1_000, 500);
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            4,
            "under the limit"
        );
        prune_cache(&dir, 350, 200);
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, ["c.wav", "d.wav"]);
    }

    #[test]
    fn the_clock_runs_between_frames_only_while_playing() {
        let mut c = Clock {
            raw: f64::NAN,
            at: Instant::now(),
        };
        assert_eq!(c.pos(10.0, true, 1.0), 10_000);
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(c.pos(10.0, true, 1.0), 10_000);
        let p = c.pos(10.0, false, 2.0);
        assert!((10_040..10_250).contains(&p), "{p}");
        assert_eq!(c.pos(10.5, false, 1.0), 10_500);
    }

    fn worker(source: Source, copy_speaker: bool, samples: &Path) -> Worker {
        let cfg = DubConfig {
            source,
            voice: String::new(),
            copy_speaker,
            samples_dir: samples.to_owned(),
            lang: "ru".into(),
            voice_volume: 100.0,
            mpv_path: String::new(),
            silent: true,
            cache_dir: samples.join("clips"),
            work_dir: samples.join("work"),
            save_dir: None,
        };
        Worker {
            cfg: Arc::new(cfg),
            engine_id: "audio.cpp/omnivoice".into(),
            voice: RefCell::new(None),
            made: std::cell::Cell::new(0),
            shared: Arc::default(),
            cancel: Arc::default(),
            server: Arc::default(),
            ctx: egui::Context::default(),
            started: Instant::now(),
        }
    }

    fn installed() -> Source {
        Source::Installed(TtsInstall {
            server: Some(PathBuf::from("no-such-server.exe")),
            model: PathBuf::from("no-such-model"),
            engine: None,
            warmed_up: true,
            size: 0,
            dir: PathBuf::from("no-such-dir"),
        })
    }

    fn film(key: &str, cues: Vec<Cue>) -> SpeakerSound {
        SpeakerSound {
            ffmpeg: PathBuf::from("no-such-ffmpeg.exe"),
            media: "film.mp4".into(),
            input: Vec::new(),
            audio_index: None,
            media_key: key.into(),
            cues,
        }
    }

    fn save_sample(dir: &Path, key: &str, text: &str) {
        let (wav, json) = sample_files(dir, key);
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(wav, wav_file(&[100, -100, 200, -200], 24_000)).unwrap();
        let saved = SavedSample {
            start: 1_000,
            end: 9_000,
            text: text.into(),
            speed: 1.2,
        };
        std::fs::write(json, serde_json::to_vec(&saved).unwrap()).unwrap();
    }

    fn choose(w: &Worker, sound: Option<&SpeakerSound>) -> Voice {
        let (mut engine, mut restarts) = (None, 0);
        let voice = w
            .choose_voice(sound, &mut engine, &mut restarts)
            .unwrap_or_else(|_| panic!("a voice"));
        assert!(engine.is_none(), "no engine is needed for a kept voice");
        voice
    }

    #[test]
    fn the_speakers_sample_is_kept_per_film_and_its_clips_are_its_own() {
        let dir = gp_core::scratch::TempDir::new("gp-dub-voice");
        save_sample(&dir, "film-a", "They all sat down at once.");
        save_sample(&dir, "film-b", "Another speaker says this.");
        std::fs::write(dir.join("standard.wav"), wav_file(&[1, 2, 3], 24_000)).unwrap();
        let w = worker(installed(), true, &dir);
        let a = choose(&w, Some(&film("film-a", Vec::new())));
        assert!(a.speaker && a.id.starts_with("speaker-"), "{}", a.id);
        assert_eq!(a.speed, Some(1.2));
        assert_eq!(
            a.reference.as_ref().map(|r| r.text.as_str()),
            Some("They all sat down at once.")
        );
        assert_eq!(choose(&w, Some(&film("film-a", Vec::new()))).id, a.id);
        let b = choose(&w, Some(&film("film-b", Vec::new())));
        assert_ne!(a.id, b.id);
        let standard = choose(&w, None);
        assert!(!standard.speaker && standard.id.starts_with("standard-"));
        // Every voice has clips of its own in the cache.
        let mut paths = Vec::new();
        for v in [a, b, standard] {
            *w.voice.borrow_mut() = Some((String::new(), v));
            paths.push(w.clip_path(None, "Птицы"));
        }
        paths.dedup();
        assert_eq!(paths.len(), 3);
    }

    #[test]
    fn without_a_sample_or_the_choice_the_voice_is_the_standard_one() {
        let dir = gp_core::scratch::TempDir::new("gp-dub-standard");
        std::fs::write(dir.join("standard.wav"), wav_file(&[1, 2, 3], 24_000)).unwrap();
        save_sample(&dir, "film-a", "They all sat down at once.");
        // No lines to take a sample from.
        let w = worker(installed(), true, &dir);
        let v = choose(&w, Some(&film("film-new", Vec::new())));
        assert!(!v.speaker && v.reference.is_some() && v.speed.is_none());
        // A sample there is, but the standard voice was chosen.
        let w = worker(installed(), false, &dir);
        assert!(!choose(&w, Some(&film("film-a", Vec::new()))).speaker);
    }

    #[test]
    fn clips_of_any_voice_come_out_equally_loud() {
        let speech = |amp: f64| -> Vec<i16> {
            (0..48_000)
                .map(|i| {
                    let on = (i / 4_800) % 2 == 0;
                    let v = (i as f64 * 0.26).sin() * if on { amp } else { amp / 1000.0 };
                    v as i16
                })
                .collect()
        };
        let quiet = leveled(&wav_file(&speech(900.0), 24_000), CLIP_LUFS).unwrap();
        let loud = leveled(&wav_file(&speech(20_000.0), 24_000), CLIP_LUFS).unwrap();
        for wav in [quiet, loud] {
            let (pcm, rate) = wav_pcm(&wav).unwrap();
            let l = loudness::integrated(&pcm, rate).unwrap();
            assert!((l - CLIP_LUFS).abs() < 0.2, "{l}");
        }
        // Another kind of file goes to the cache as the engine made it.
        assert_eq!(leveled(b"RIFF....WAVEdata", CLIP_LUFS), None);
    }

    #[test]
    fn a_server_of_the_users_own_never_gets_the_speakers_voice() {
        let dir = gp_core::scratch::TempDir::new("gp-dub-remote");
        save_sample(&dir, "film-a", "They all sat down at once.");
        let remote = Source::Remote {
            base_url: "http://127.0.0.1:1/v1".into(),
            model: "m".into(),
        };
        let w = worker(remote, true, &dir);
        let v = choose(&w, Some(&film("film-a", Vec::new())));
        assert!(v.reference.is_none() && v.id.is_empty() && !v.speaker);
        // Its clips carry no voice id, only how clips are made.
        *w.voice.borrow_mut() = Some(("film-a".into(), v));
        let voice = format!("|{CLIP_MAKING}");
        let key = cache_key("audio.cpp/omnivoice", &voice, "ru", "Птицы", None);
        assert_eq!(
            w.clip_path(None, "Птицы"),
            dir.join("clips").join(format!("{key}.wav"))
        );
    }
}
