//! The voice-over at run time. A worker synthesizes the lines just ahead of the playhead and
//! keeps the clips on disk; a player speaks them through a second mpv without video, in step
//! with the film. What plays when is decided by `gp_core::dub_plan`.

use crate::i18n::{Tr, t};
use crate::setup_text as text;
use crate::tts_setup::{self, TtsInstall};
use eframe::egui;
use gp_ai::AiError;
use gp_ai::tts::{SpeakOptions, SpeechServer, TtsEngine, Wav, parse_wav};
use gp_core::Ms;
use gp_core::dub_plan::{self, Decision, Item, MAX_LAG, Unit};
use gp_media::mpv::Mpv;
use gp_media::tools::{self, ServerCommand, ServerProcess};
use sha2::{Digest, Sha256};
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

pub enum Source {
    Installed(TtsInstall),
    /// A speech server of the user's own.
    Remote {
        base_url: String,
        model: String,
    },
}

pub struct DubConfig {
    pub source: Source,
    pub voice: String,
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
        let volume = format!("{:.0}", cfg.voice_volume.clamp(0.0, 150.0));
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
        if cfg.silent {
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
    pub fn set_units(&self, units: Vec<Unit>) {
        let mut s = lock(&self.shared);
        if s.units != units {
            s.units = units;
        }
    }

    pub fn set_voice_volume(&self, volume: f64) {
        self.voice.set_f64("volume", volume.clamp(0.0, 150.0));
    }

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

struct Worker {
    cfg: Arc<DubConfig>,
    engine_id: String,
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
        let key = cache_key(
            &self.engine_id,
            &self.cfg.voice,
            &self.cfg.lang,
            text,
            target,
        );
        self.cfg.cache_dir.join(format!("{key}.wav"))
    }

    fn produce(
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
        let opts = SpeakOptions {
            target_ms: target,
            speed: None,
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
                    let _ = write_clip(path, &wav.bytes);
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
}
