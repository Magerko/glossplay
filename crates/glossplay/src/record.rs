//! A service mode for making demo videos, switched on by `GLOSSPLAY_RECORD_DIR=<dir>`.
//!
//! Nothing is played aloud: both players write their sound into WAV files in `<dir>` (the
//! film into `video.wav`, every voice-over clip into a `voice-<line start>.wav` of its own),
//! the window goes 30 frames a second through ffmpeg into `<dir>/window.mkv`, and
//! `<dir>/timeline.jsonl` holds the wall-clock moments that tie them together. Volumes,
//! pauses and the voice-over timing are the ones of normal playback.
//!
//! Sample 0 of `video.wav` is where the film was at the `start` line, give or take a frame;
//! a voice file starts one frame (1/30 s) into its clip, which mpv trims to meet the first
//! frame of the voice player's picture, and plays from the clip's `started_at`.
//!
//! `GLOSSPLAY_RECORD_FROM` and `GLOSSPLAY_RECORD_TO` (seconds of the film) limit what is
//! recorded; without them recording runs from the first moment of playback to the exit.

use crate::app::App;
use crate::video;
use eframe::egui;
use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

pub const ENV_DIR: &str = "GLOSSPLAY_RECORD_DIR";
const FPS: f64 = 30.0;

struct Config {
    dir: PathBuf,
    from_ms: i64,
    to_ms: Option<i64>,
    epoch: Instant,
    log: Mutex<Option<std::fs::File>>,
}

fn config() -> Option<&'static Config> {
    static CONFIG: OnceLock<Option<Config>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let dir = PathBuf::from(std::env::var_os(ENV_DIR).filter(|d| !d.is_empty())?);
            std::fs::create_dir_all(&dir).ok()?;
            let secs = |name| {
                std::env::var(name)
                    .ok()
                    .and_then(|v| v.parse::<f64>().ok())
                    .map(|s| (s * 1000.0) as i64)
            };
            let log = std::fs::File::create(dir.join("timeline.jsonl")).ok();
            Some(Config {
                from_ms: secs("GLOSSPLAY_RECORD_FROM").unwrap_or(0),
                to_ms: secs("GLOSSPLAY_RECORD_TO"),
                dir,
                epoch: Instant::now(),
                log: Mutex::new(log),
            })
        })
        .as_ref()
}

pub fn on() -> bool {
    config().is_some()
}

fn path(c: &Config, name: &str) -> String {
    c.dir.join(name).to_string_lossy().into_owned()
}

/// Seconds since the recorder came up: the one clock every line of the timeline uses.
fn now(c: &Config) -> f64 {
    c.epoch.elapsed().as_secs_f64()
}

/// Appends a line to the timeline, with the time it happened.
pub fn event(kind: &str, mut fields: Value) {
    let Some(c) = config() else { return };
    fields["t"] = json!(now(c));
    fields["ev"] = json!(kind);
    if let Some(f) = c.log.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        let _ = writeln!(f, "{fields}");
    }
}

/// Audio options of the film's player. Until recording starts its sound goes to a scratch
/// file, never to a device.
pub fn main_options() -> Vec<(String, String)> {
    let Some(c) = config() else { return Vec::new() };
    audio_options(path(c, "video-before.wav"))
}

/// Audio options of the voice player. `ao=pcm` takes samples as fast as they come and only
/// a player with video keeps it to the clock, the way the film's player does; so the voice
/// gets a tiny picture of its own waveform, which `vo=null` shows in real time.
pub fn voice_options() -> Vec<(String, String)> {
    let Some(c) = config() else { return Vec::new() };
    let mut options = audio_options(path(c, "voice-before.wav"));
    options.push((
        "lavfi-complex".into(),
        "[aid1]asplit[ao][w];[w]showwaves=s=16x16:r=30[vo]".into(),
    ));
    options
}

fn audio_options(file: String) -> Vec<(String, String)> {
    [
        ("ao", "pcm".to_owned()),
        ("ao-pcm-file", file),
        ("ao-pcm-waveheader", "yes".to_owned()),
        ("audio-samplerate", "48000".to_owned()),
        ("audio-channels", "stereo".to_owned()),
        ("audio-format", "float".to_owned()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect()
}

static VOICE_FILE: Mutex<Option<String>> = Mutex::new(None);

/// The file the next voice clip is written to.
pub fn clip_file(line_start: i64) -> Option<String> {
    let file = path(config()?, &format!("voice-{line_start:07}.wav"));
    *VOICE_FILE.lock().unwrap_or_else(|e| e.into_inner()) = Some(file.clone());
    Some(file)
}

/// How much the sound files have grown, which shows when their samples were really written.
fn written(c: &Config) -> Value {
    let len = |p: &str| std::fs::metadata(p).map_or(0, |m| m.len());
    let voice = VOICE_FILE.lock().unwrap_or_else(|e| e.into_inner()).clone();
    json!({
        "main": len(&path(c, "video.wav")),
        "voice_file": voice,
        "voice": voice.as_deref().map_or(0, len),
    })
}

enum Stage {
    Waiting,
    Recording(Box<Capture>),
    Done,
}

struct Capture {
    t0: f64,
    /// The last 30 fps slot that has a frame, and the one a read-back is on its way for.
    slot: i64,
    pending: Option<i64>,
    frames: SyncSender<Frame>,
    shot: video::Capture,
    writer: Option<std::thread::JoinHandle<()>>,
    paused: Option<bool>,
    volumes: (f64, f64),
    dropped: usize,
}

struct Frame {
    slot: i64,
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

static STAGE: Mutex<Option<Stage>> = Mutex::new(None);

/// Runs once a frame, after everything is drawn.
pub fn frame(app: &mut App, ctx: &egui::Context) {
    let Some(c) = config() else { return };
    let mut guard = STAGE.lock().unwrap_or_else(|e| e.into_inner());
    let stage = guard.get_or_insert(Stage::Waiting);
    match stage {
        Stage::Waiting => {
            let mpv = &app.player.mpv;
            let ready = app.session.is_some()
                && !app.snap.paused
                && mpv.get_flag("seeking") == Some(false)
                && mpv.get_flag("paused-for-cache") == Some(false)
                && app.snap.pos >= c.from_ms
                && c.to_ms.is_none_or(|to| app.snap.pos < to);
            if ready {
                *stage = start(c, app, ctx);
            }
        }
        Stage::Recording(cap) => {
            if c.to_ms.is_some_and(|to| app.snap.pos >= to) {
                stop(cap, app);
                *stage = Stage::Done;
                return;
            }
            step(c, cap, app, ctx);
        }
        Stage::Done => {}
    }
}

/// Called from `on_exit`: closes the video and the film's sound file.
pub fn finish(app: &App) {
    let mut guard = STAGE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(Stage::Recording(cap)) = guard.as_mut() {
        stop(cap, app);
    }
    *guard = Some(Stage::Done);
}

fn start(c: &Config, app: &mut App, ctx: &egui::Context) -> Stage {
    let mpv = &app.player.mpv;
    // A fresh output makes the first sample of video.wav the position playback is at now.
    mpv.set("ao-pcm-file", &path(c, "video.wav"));
    let reloaded = mpv.command(&["ao-reload"]).is_ok();
    let (tx, rx) = std::sync::mpsc::sync_channel::<Frame>(6);
    let dir = c.dir.clone();
    let ffmpeg = app.ffmpeg.clone();
    let writer = std::thread::spawn(move || write_frames(&dir, ffmpeg.as_deref(), rx));
    let t = now(c);
    event(
        "start",
        json!({"pos": app.snap.pos, "ao_reload": reloaded, "fps": FPS}),
    );
    ctx.request_repaint();
    Stage::Recording(Box::new(Capture {
        t0: t,
        slot: -1,
        pending: None,
        frames: tx,
        shot: Arc::default(),
        writer: Some(writer),
        paused: None,
        volumes: (-1.0, -1.0),
        dropped: 0,
    }))
}

fn step(c: &Config, cap: &mut Capture, app: &mut App, ctx: &egui::Context) {
    // The pixels read back at the end of the previous frame.
    let shot = cap.shot.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let (Some((width, height, pixels)), Some(slot)) = (shot, cap.pending.take()) {
        if cap.slot < 0 {
            event("size", json!({"width": width, "height": height}));
        }
        let frame = Frame {
            slot,
            width,
            height,
            pixels,
        };
        match cap.frames.try_send(frame) {
            Ok(()) => cap.slot = slot,
            Err(TrySendError::Full(_)) => cap.dropped += 1,
            Err(TrySendError::Disconnected(_)) => {}
        }
    }
    let paused = app.snap.paused;
    if cap.paused != Some(paused) {
        cap.paused = Some(paused);
        event(
            if paused { "pause" } else { "play" },
            json!({"pos": app.snap.pos}),
        );
    }
    let volumes = (
        app.player.mpv.get_f64("volume").unwrap_or(-1.0),
        app.dub.as_ref().map_or(-1.0, |d| d.voice_volume()),
    );
    if volumes != cap.volumes {
        cap.volumes = volumes;
        event("volume", json!({"main": volumes.0, "voice": volumes.1}));
    }
    let slot = ((now(c) - cap.t0) * FPS).round() as i64;
    if slot > cap.slot {
        cap.pending = Some(slot);
        let layer = egui::LayerId::new(egui::Order::Debug, egui::Id::unique("record"));
        ctx.layer_painter(layer).add(video::capture_callback(
            ctx.content_rect(),
            cap.shot.clone(),
        ));
    }
    event(
        "pos",
        json!({"pos": app.snap.pos, "paused": paused, "bytes": written(c)}),
    );
    ctx.request_repaint();
}

fn stop(cap: &mut Capture, app: &App) {
    // Switching the output closes video.wav, which writes its header.
    app.player.mpv.set("ao", "null");
    let (tx, _) = std::sync::mpsc::sync_channel(1);
    drop(std::mem::replace(&mut cap.frames, tx));
    if let Some(w) = cap.writer.take() {
        let _ = w.join();
    }
    event(
        "stop",
        json!({"pos": app.snap.pos, "last_slot": cap.slot, "dropped": cap.dropped}),
    );
}

/// Feeds frames to ffmpeg; a slot without a frame of its own repeats the one before.
fn write_frames(dir: &Path, ffmpeg: Option<&Path>, rx: std::sync::mpsc::Receiver<Frame>) {
    let mut child: Option<Child> = None;
    let mut last: Option<Frame> = None;
    while let Ok(frame) = rx.recv() {
        if child.is_none() {
            child = spawn_ffmpeg(dir, ffmpeg, frame.width, frame.height);
        }
        let Some(stdin) = child.as_mut().and_then(|c| c.stdin.as_mut()) else {
            return;
        };
        if let Some(prev) = &last {
            if (prev.width, prev.height) != (frame.width, frame.height) {
                continue;
            }
            for _ in prev.slot + 1..frame.slot {
                let _ = stdin.write_all(&prev.pixels);
            }
        }
        if stdin.write_all(&frame.pixels).is_err() {
            return;
        }
        last = Some(frame);
    }
    if let Some(mut c) = child {
        drop(c.stdin.take());
        let _ = c.wait();
    }
}

fn spawn_ffmpeg(dir: &Path, ffmpeg: Option<&Path>, w: u32, h: u32) -> Option<Child> {
    let program = ffmpeg.map_or_else(|| PathBuf::from("ffmpeg"), Path::to_owned);
    let mut cmd = Command::new(program);
    cmd.args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "rawvideo", "-pix_fmt", "rgba"])
        .args(["-s", &format!("{w}x{h}"), "-r", "30", "-i", "-"])
        // glReadPixels gives the rows bottom up.
        .args(["-vf", "vflip", "-c:v", "libx264", "-preset", "ultrafast"])
        .args(["-crf", "8", "-pix_fmt", "yuv444p"])
        .arg(dir.join("window.mkv"))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(
            std::fs::File::create(dir.join("ffmpeg.log"))
                .map(Stdio::from)
                .unwrap_or_else(|_| Stdio::null()),
        );
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd.spawn().ok()
}
