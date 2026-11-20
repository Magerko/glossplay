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
//! `GLOSSPLAY_RECORD_NOW=1` starts it at once, on the start screen too, and
//! `GLOSSPLAY_RECORD_FPS` sets the frame rate (30 by default).
//!
//! A control driving the player (the `pointer` tool of `--mcp`) can move a pointer of its own:
//! its moves, presses and typing go into the window as input, the way a mouse and keyboard
//! would send them, and an arrow is drawn where it is, so the recording shows what was done.

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

struct Config {
    dir: PathBuf,
    from_ms: i64,
    to_ms: Option<i64>,
    at_once: bool,
    fps: f64,
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
                at_once: std::env::var("GLOSSPLAY_RECORD_NOW").is_ok_and(|v| v == "1"),
                fps: std::env::var("GLOSSPLAY_RECORD_FPS")
                    .ok()
                    .and_then(|v| v.parse::<f64>().ok())
                    .filter(|f| (1.0..=120.0).contains(f))
                    .unwrap_or(30.0),
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

/// Places on screen a click may land on without a pointer going there: a control driving
/// the player presses buttons directly, and a viewer of the recording should still see where.
static PLACES: Mutex<Vec<(String, egui::Rect)>> = Mutex::new(Vec::new());
/// Clicks to show, with the moment each happened.
static CLICKS: Mutex<Vec<(f64, egui::Pos2)>> = Mutex::new(Vec::new());
const CLICK_SHOWN: f64 = 0.7;

/// Remembers where something that can be clicked is drawn this frame.
pub fn place(name: &str, rect: egui::Rect) {
    if !on() {
        return;
    }
    let mut places = PLACES.lock().unwrap_or_else(|e| e.into_inner());
    places.retain(|(n, _)| n != name);
    places.push((name.to_owned(), rect));
}

/// Shows a click at `pos` in the recording.
pub fn click(pos: egui::Pos2) {
    let Some(c) = config() else { return };
    // The scripted pointer shows its own clicks.
    if POINTER
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .is_some_and(|p| p.shown)
    {
        return;
    }
    drop_click(now(c), pos);
}

/// The places [`place`] knows of, in window pixels.
pub fn places(ppp: f32) -> Value {
    let places = PLACES.lock().unwrap_or_else(|e| e.into_inner());
    places
        .iter()
        .map(|(name, r)| {
            json!({
                "name": name,
                "x": r.center().x * ppp,
                "y": r.center().y * ppp,
                "width": r.width() * ppp,
                "height": r.height() * ppp,
            })
        })
        .collect()
}

/// Shows a click on a place given to [`place`].
pub fn click_place(name: &str) {
    let rect = PLACES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, r)| *r);
    if let Some(r) = rect {
        click(r.center());
    }
}

/// A ring that spreads and fades where a click landed.
fn draw_clicks(c: &Config, painter: &egui::Painter) {
    let mut clicks = CLICKS.lock().unwrap_or_else(|e| e.into_inner());
    let t = now(c);
    clicks.retain(|(at, _)| t - at < CLICK_SHOWN);
    for (at, pos) in clicks.iter() {
        let k = ((t - at) / CLICK_SHOWN) as f32;
        let alpha = ((1.0 - k) * 230.0) as u8;
        let color = egui::Color32::from_rgba_unmultiplied(255, 214, 64, alpha);
        painter.circle_filled(*pos, 7.0 * (1.0 - k), color);
        painter.circle_stroke(*pos, 10.0 + 22.0 * k, egui::Stroke::new(3.0, color));
    }
}

/// What the scripted pointer is asked to do; `x` and `y` are window pixels.
pub enum PointerAction {
    Move {
        x: f32,
        y: f32,
        secs: f64,
    },
    Click {
        x: f32,
        y: f32,
        secs: f64,
        count: u8,
    },
    Drag {
        x: f32,
        y: f32,
        secs: f64,
    },
    Type {
        text: String,
        secs: f64,
    },
    /// What Ctrl+V gives a text field: the whole text at once.
    Paste(String),
    Hide,
}

struct Glide {
    from: egui::Pos2,
    to: egui::Pos2,
    start: f64,
    secs: f64,
}

enum Step {
    Glide { to: egui::Pos2, secs: f64 },
    Button { pressed: bool },
    Paste(String),
}

#[derive(Default)]
struct Pointer {
    /// In points; `None` until the pointer is first used.
    pos: Option<egui::Pos2>,
    shown: bool,
    glide: Option<Glide>,
    /// One step a frame: egui sees a press and its release in different frames, as it would
    /// from a real mouse.
    steps: std::collections::VecDeque<Step>,
    typing: Option<(Vec<char>, f64, f64)>,
}

static POINTER: Mutex<Option<Pointer>> = Mutex::new(None);

fn ease(k: f64) -> f64 {
    let k = k.clamp(0.0, 1.0);
    k * k * (3.0 - 2.0 * k)
}

/// Starts a pointer action; [`pointer_busy`] tells when it is over. An error outside a
/// recording.
pub fn pointer(action: PointerAction, ppp: f32) -> Result<(), String> {
    let Some(c) = config() else {
        return Err("the pointer works only in the recording mode (GLOSSPLAY_RECORD_DIR)".into());
    };
    let mut guard = POINTER.lock().unwrap_or_else(|e| e.into_inner());
    let p = guard.get_or_insert_with(Pointer::default);
    let at = |x: f32, y: f32| egui::pos2(x / ppp, y / ppp);
    let what = match &action {
        PointerAction::Move { .. } => "move",
        PointerAction::Click { count: 2, .. } => "double_click",
        PointerAction::Click { .. } => "click",
        PointerAction::Drag { .. } => "drag",
        PointerAction::Type { .. } => "type",
        PointerAction::Paste(_) => "paste",
        PointerAction::Hide => "hide",
    };
    p.shown = !matches!(action, PointerAction::Hide);
    match action {
        PointerAction::Move { x, y, secs } => p.steps.push_back(Step::Glide { to: at(x, y), secs }),
        PointerAction::Click { x, y, secs, count } => {
            p.steps.push_back(Step::Glide { to: at(x, y), secs });
            for _ in 0..count {
                p.steps.push_back(Step::Button { pressed: true });
                p.steps.push_back(Step::Button { pressed: false });
            }
        }
        PointerAction::Drag { x, y, secs } => {
            if p.pos.is_none() {
                return Err("move the pointer to where the drag starts first".into());
            }
            p.steps.push_back(Step::Button { pressed: true });
            p.steps.push_back(Step::Glide { to: at(x, y), secs });
            p.steps.push_back(Step::Button { pressed: false });
        }
        PointerAction::Type { text, secs } => {
            p.typing = Some((text.chars().collect(), now(c), secs.max(0.0)));
        }
        PointerAction::Paste(text) => p.steps.push_back(Step::Paste(text)),
        PointerAction::Hide => {}
    }
    drop(guard);
    event("pointer", json!({"action": what}));
    Ok(())
}

pub fn pointer_busy() -> bool {
    POINTER
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .is_some_and(|p| p.glide.is_some() || !p.steps.is_empty() || p.typing.is_some())
}

/// Runs before every frame: the scripted pointer's input goes in, and a real mouse's stays
/// out while it is in use.
pub fn inject(ctx: &egui::Context, raw: &mut egui::RawInput) {
    let Some(c) = config() else { return };
    let mut guard = POINTER.lock().unwrap_or_else(|e| e.into_inner());
    let Some(p) = guard.as_mut() else { return };
    raw.events.retain(|e| {
        !matches!(
            e,
            egui::Event::PointerMoved(_)
                | egui::Event::PointerButton { .. }
                | egui::Event::PointerGone
                | egui::Event::MouseWheel { .. }
                | egui::Event::MouseMoved(_)
        )
    });
    let t = now(c);
    let busy = p.glide.is_some() || !p.steps.is_empty() || p.typing.is_some();
    if p.glide.is_none() {
        match p.steps.pop_front() {
            Some(Step::Glide { to, secs }) => {
                let from = p.pos.unwrap_or(to);
                p.glide = Some(Glide {
                    from,
                    to,
                    start: t,
                    secs: if from == to { 0.0 } else { secs },
                });
            }
            Some(Step::Paste(text)) => raw.events.push(egui::Event::Paste(text)),
            Some(Step::Button { pressed }) => {
                let pos = p.pos.unwrap_or_default();
                if pressed {
                    drop_click(t, pos);
                }
                raw.events.push(egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                });
            }
            None => {}
        }
    }
    if let Some(g) = &p.glide {
        let k = if g.secs <= 0.0 {
            1.0
        } else {
            (t - g.start) / g.secs
        };
        let pos = g.from + (g.to - g.from) * ease(k) as f32;
        p.pos = Some(pos);
        raw.events.push(egui::Event::PointerMoved(pos));
        if k >= 1.0 {
            p.glide = None;
        }
    } else if p.steps.is_empty()
        && let Some((chars, start, secs)) = &mut p.typing
    {
        let n = chars.len().max(1) as f64;
        let due = (((t - *start) / secs.max(1e-3)) * n).floor() as usize + 1;
        let take = due.min(chars.len());
        let text: String = chars.drain(..take).collect();
        *start += take as f64 * *secs / n;
        if !text.is_empty() {
            raw.events.push(egui::Event::Text(text));
        }
        if chars.is_empty() {
            p.typing = None;
        }
    }
    if busy {
        if p.glide.is_none() && p.steps.is_empty() && p.typing.is_none() {
            let pos = p.pos.unwrap_or_default();
            event("pointer_done", json!({"x": pos.x, "y": pos.y}));
        }
        ctx.request_repaint();
    }
}

fn drop_click(t: f64, pos: egui::Pos2) {
    event("click", json!({"x": pos.x, "y": pos.y}));
    CLICKS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((t, pos));
}

/// The arrow of the scripted pointer, its tip at `pos`.
fn draw_pointer(painter: &egui::Painter) {
    let guard = POINTER.lock().unwrap_or_else(|e| e.into_inner());
    let Some(pos) = guard.as_ref().filter(|p| p.shown).and_then(|p| p.pos) else {
        return;
    };
    let at = |x: f32, y: f32| pos + egui::vec2(x, y) * 1.15;
    let white = egui::Color32::WHITE;
    let none = egui::Stroke::NONE;
    painter.add(egui::Shape::convex_polygon(
        vec![at(0.0, 0.0), at(0.0, 17.0), at(12.0, 12.5)],
        white,
        none,
    ));
    painter.add(egui::Shape::convex_polygon(
        vec![at(4.0, 12.0), at(6.8, 19.5), at(9.4, 18.4), at(6.8, 11.6)],
        white,
        none,
    ));
    let outline = [
        (0.0, 0.0),
        (0.0, 17.0),
        (4.2, 13.2),
        (6.8, 19.5),
        (9.4, 18.4),
        (6.9, 12.6),
        (12.0, 12.5),
    ];
    painter.add(egui::Shape::closed_line(
        outline.iter().map(|&(x, y)| at(x, y)).collect(),
        egui::Stroke::new(1.3, egui::Color32::BLACK),
    ));
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
    lines: Option<(Vec<String>, Vec<String>)>,
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
            let ready = c.at_once
                || app.session.is_some()
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
    let fps = c.fps;
    let writer = std::thread::spawn(move || write_frames(&dir, ffmpeg.as_deref(), fps, rx));
    let t = now(c);
    // The wall clock lines this recording up with one made by another program at the same time.
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64());
    event(
        "start",
        json!({"pos": app.snap.pos, "ao_reload": reloaded, "fps": c.fps, "unix": unix}),
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
        lines: None,
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
    let lines = app.session.as_ref().map_or((Vec::new(), Vec::new()), |s| {
        let primary = s
            .primary_lines(app.snap.pos)
            .into_iter()
            .map(|l| l.text)
            .collect();
        (primary, s.secondary_lines(app.snap.pos))
    });
    if cap.lines.as_ref() != Some(&lines) {
        event(
            "lines",
            json!({"pos": app.snap.pos, "primary": lines.0, "secondary": lines.1}),
        );
        cap.lines = Some(lines);
    }
    let layer = egui::LayerId::new(egui::Order::Debug, egui::Id::unique("record"));
    let painter = ctx.layer_painter(layer);
    draw_clicks(c, &painter);
    draw_pointer(&painter);
    let slot = ((now(c) - cap.t0) * c.fps).round() as i64;
    if slot > cap.slot {
        cap.pending = Some(slot);
        painter.add(video::capture_callback(
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
fn write_frames(dir: &Path, ffmpeg: Option<&Path>, fps: f64, rx: std::sync::mpsc::Receiver<Frame>) {
    let mut child: Option<Child> = None;
    let mut last: Option<Frame> = None;
    while let Ok(frame) = rx.recv() {
        if child.is_none() {
            child = spawn_ffmpeg(dir, ffmpeg, fps, frame.width, frame.height);
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

fn spawn_ffmpeg(dir: &Path, ffmpeg: Option<&Path>, fps: f64, w: u32, h: u32) -> Option<Child> {
    let program = ffmpeg.map_or_else(|| PathBuf::from("ffmpeg"), Path::to_owned);
    let mut cmd = Command::new(program);
    cmd.args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "rawvideo", "-pix_fmt", "rgba"])
        .args(["-s", &format!("{w}x{h}"), "-r", &fps.to_string(), "-i", "-"])
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
