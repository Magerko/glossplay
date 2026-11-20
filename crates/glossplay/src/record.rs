//! A service mode for making demo videos, allowed by `GLOSSPLAY_RECORD_DIR=<dir>`.
//!
//! In this mode nothing is played aloud: both players write their sound into files. Recording
//! itself runs in sessions, started and stopped by a control driving the player (the
//! `record_start` and `record_stop` tools of `--mcp`); each session has a folder of its own, a
//! new `rec-NNN` in `<dir>` unless the control names one. In it the window goes through ffmpeg
//! into `window.mkv` (30 frames a second unless asked otherwise), the film's sound into
//! `video.wav`, every voice-over clip played into a `voice-NNN-<line start>.wav` of its own (NNN
//! counts the clips of the session, so a line said again gets a new file), and
//! `timeline.jsonl` holds the wall-clock moments that tie them together. Volumes, pauses and the
//! voice-over timing are the ones of normal playback. A red REC sign shows in the window while
//! a session runs; it is drawn after the frame is taken, so the recording does not have it.
//!
//! Sample 0 of `video.wav` is where the film was at the `start` line, give or take a frame;
//! a voice file starts one frame (1/30 s) into its clip, which mpv trims to meet the first
//! frame of the voice player's picture, and plays from the `started_at` of its `clip_started`
//! line, which names the file.
//!
//! As before sessions, a session can also start by itself and write straight into `<dir>`:
//! `GLOSSPLAY_RECORD_NOW=1` starts it at once, on the start screen too, and
//! `GLOSSPLAY_RECORD_FROM` and `GLOSSPLAY_RECORD_TO` (seconds of the film) start it when
//! playback reaches the first and stop it at the second. `GLOSSPLAY_RECORD_FPS` sets the
//! frame rate a session gets when it is not asked for one.
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
    /// A session starts by itself, as it did before sessions could be asked for.
    auto: bool,
    from_ms: i64,
    to_ms: Option<i64>,
    at_once: bool,
    fps: f64,
    epoch: Instant,
}

/// The session being recorded.
struct Session {
    dir: PathBuf,
    log: Option<std::fs::File>,
    clips: usize,
}

static SESSION: Mutex<Option<Session>> = Mutex::new(None);

fn session() -> std::sync::MutexGuard<'static, Option<Session>> {
    SESSION.lock().unwrap_or_else(|e| e.into_inner())
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
            let from_ms = secs("GLOSSPLAY_RECORD_FROM");
            let to_ms = secs("GLOSSPLAY_RECORD_TO");
            let at_once = std::env::var("GLOSSPLAY_RECORD_NOW").is_ok_and(|v| v == "1");
            Some(Config {
                auto: at_once || from_ms.is_some() || to_ms.is_some(),
                from_ms: from_ms.unwrap_or(0),
                to_ms,
                at_once,
                fps: std::env::var("GLOSSPLAY_RECORD_FPS")
                    .ok()
                    .and_then(|v| v.parse::<f64>().ok())
                    .filter(|f| (1.0..=120.0).contains(f))
                    .unwrap_or(30.0),
                dir,
                epoch: Instant::now(),
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

/// Appends a line to the timeline of the session, with the time it happened.
pub fn event(kind: &str, mut fields: Value) {
    let Some(c) = config() else { return };
    fields["t"] = json!(now(c));
    fields["ev"] = json!(kind);
    if let Some(f) = session().as_mut().and_then(|s| s.log.as_mut()) {
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
        /// Select all that is in the field first, so the text takes its place.
        replace: bool,
    },
    /// A key with its modifiers, such as ctrl+a or backspace, pressed and released.
    Key(crate::keys::Combo),
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
    Glide {
        to: egui::Pos2,
        secs: f64,
    },
    Button {
        pressed: bool,
    },
    Paste(String),
    /// Input events as the keyboard would send them in one frame.
    Events(Vec<egui::Event>),
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
    /// What is left to type, when typing began, how long it takes and how much it was.
    typing: Option<(Vec<char>, f64, f64, usize)>,
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
        PointerAction::Key(_) => "key",
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
        PointerAction::Type {
            text,
            secs,
            replace,
        } => {
            if replace {
                let all = crate::keys::parse_combo("ctrl+a").expect("a known combo");
                push_key(p, &all);
            }
            let chars: Vec<char> = text.chars().collect();
            let total = chars.len();
            p.typing = Some((chars, now(c), secs.max(0.0), total));
        }
        PointerAction::Key(combo) => push_key(p, &combo),
        PointerAction::Paste(text) => p.steps.push_back(Step::Paste(text)),
        PointerAction::Hide => {}
    }
    drop(guard);
    event("pointer", json!({"action": what}));
    Ok(())
}

/// A key press and its release, a frame apart, as the keyboard sends them.
fn push_key(p: &mut Pointer, combo: &crate::keys::Combo) {
    let synth = crate::keys::synthesize(crate::keys::Layout::En, combo, "");
    p.steps.push_back(Step::Events(synth.press));
    p.steps.push_back(Step::Events(synth.release));
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
            Some(Step::Events(events)) => raw.events.extend(events),
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
        && let Some((chars, start, secs, total)) = &mut p.typing
    {
        let typed = *total - chars.len();
        let take = typed_by(t - *start, *secs, *total).saturating_sub(typed);
        let text: String = chars.drain(..take.min(chars.len())).collect();
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

/// How many of `total` characters are typed `elapsed` seconds into typing that takes `secs`:
/// the first at once, the rest evenly after it.
fn typed_by(elapsed: f64, secs: f64, total: usize) -> usize {
    if secs <= 0.0 || total == 0 {
        return total;
    }
    let k = (elapsed / secs).clamp(0.0, 1.0);
    ((k * total as f64).floor() as usize + 1).min(total)
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

/// The file the next voice clip is written to: a new one in the session for every clip
/// played, so a line said twice keeps both, and a scratch file between sessions.
pub fn clip_file(line_start: i64) -> Option<String> {
    let c = config()?;
    let file = match session().as_mut() {
        Some(s) => {
            s.clips += 1;
            s.dir
                .join(format!("voice-{:03}-{line_start:07}.wav", s.clips))
        }
        None => c.dir.join("voice-before.wav"),
    };
    let file = file.to_string_lossy().into_owned();
    *VOICE_FILE.lock().unwrap_or_else(|e| e.into_inner()) = Some(file.clone());
    Some(file)
}

/// How much the sound files have grown, which shows when their samples were really written.
fn written(dir: &Path) -> Value {
    let len = |p: &Path| std::fs::metadata(p).map_or(0, |m| m.len());
    let voice = VOICE_FILE.lock().unwrap_or_else(|e| e.into_inner()).clone();
    json!({
        "main": len(&dir.join("video.wav")),
        "voice_file": voice,
        "voice": voice.as_deref().map_or(0, |v| len(Path::new(v))),
    })
}

struct Capture {
    dir: PathBuf,
    fps: f64,
    /// Started by itself, so `GLOSSPLAY_RECORD_TO` ends it.
    auto: bool,
    t0: f64,
    /// The last slot that has a frame, and the one a read-back is on its way for.
    slot: i64,
    pending: Option<i64>,
    frames: SyncSender<Frame>,
    shot: video::Capture,
    writer: Option<std::thread::JoinHandle<()>>,
    paused: Option<bool>,
    volumes: (f64, f64),
    lines: Option<(Vec<String>, Vec<String>)>,
    /// Frames handed to ffmpeg; the slots between them repeat the frame before.
    sent: i64,
}

impl Capture {
    /// Slots that got no frame of their own because ffmpeg was not keeping up.
    fn repeated(&self) -> i64 {
        (self.slot + 1 - self.sent).max(0)
    }
}

struct Frame {
    slot: i64,
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

struct Recorder {
    capture: Option<Box<Capture>>,
    /// The session that starts by itself has not started yet.
    auto_pending: bool,
    /// Writers of stopped sessions still feeding ffmpeg the last frames.
    finishing: Vec<std::thread::JoinHandle<()>>,
    /// When the scratch sound file was last looked at.
    scratch_checked: Instant,
}

/// Between sessions the film's sound goes on into a scratch file; past this size I start it
/// afresh, so a long time without recording does not fill the disk.
const SCRATCH_LIMIT: u64 = 64 << 20;

static RECORDER: Mutex<Option<Recorder>> = Mutex::new(None);

fn recorder() -> std::sync::MutexGuard<'static, Option<Recorder>> {
    RECORDER.lock().unwrap_or_else(|e| e.into_inner())
}

/// Runs once a frame, after everything is drawn.
pub fn frame(app: &mut App, ctx: &egui::Context) {
    let Some(c) = config() else { return };
    let mut guard = recorder();
    let r = guard.get_or_insert_with(|| Recorder {
        capture: None,
        auto_pending: c.auto,
        finishing: Vec::new(),
        scratch_checked: Instant::now(),
    });

    r.finishing.retain(|w| !w.is_finished());
    match &mut r.capture {
        None if r.auto_pending => {
            let mpv = &app.player.mpv;
            let ready = c.at_once
                || app.session.is_some()
                    && !app.snap.paused
                    && mpv.get_flag("seeking") == Some(false)
                    && mpv.get_flag("paused-for-cache") == Some(false)
                    && app.snap.pos >= c.from_ms
                    && c.to_ms.is_none_or(|to| app.snap.pos < to);
            if ready {
                r.auto_pending = false;
                match start(c, app, ctx, c.dir.clone(), c.fps, true) {
                    Ok(cap) => r.capture = Some(cap),
                    Err(e) => eprintln!("recording: {e}"),
                }
            }
        }
        None => {
            if r.scratch_checked.elapsed().as_secs() >= 10 {
                r.scratch_checked = Instant::now();
                let scratch = path(c, "video-before.wav");
                if std::fs::metadata(&scratch).is_ok_and(|m| m.len() > SCRATCH_LIMIT) {
                    // A reload opens the file anew, which empties it.
                    app.player.mpv.set("ao-pcm-file", &scratch);
                    app.player.mpv.command(&["ao-reload"]).ok();
                }
            }
        }
        Some(cap) => {
            if cap.auto && c.to_ms.is_some_and(|to| app.snap.pos >= to) {
                if let Some(cap) = r.capture.take()
                    && let Some(w) = stop(*cap, app)
                {
                    r.finishing.push(w);
                }
                return;
            }
            step(c, cap, app, ctx);
        }
    }
}

/// Starts a session at once, whatever is on screen; `dir` is its folder, a new one in the
/// recording folder when not given. Answers with the folder and the frame rate.
pub fn start_session(
    app: &mut App,
    ctx: &egui::Context,
    dir: Option<PathBuf>,
    fps: Option<f64>,
) -> Result<Value, String> {
    let c = config().ok_or(NOT_ON)?;
    let fps = fps.unwrap_or(c.fps);
    if !(1.0..=120.0).contains(&fps) {
        return Err(format!("fps must be from 1 to 120, not {fps}"));
    }
    let mut guard = recorder();
    let r = guard.get_or_insert_with(|| Recorder {
        capture: None,
        auto_pending: c.auto,
        finishing: Vec::new(),
        scratch_checked: Instant::now(),
    });

    if let Some(cap) = &r.capture {
        return Err(format!(
            "already recording into {}; record_stop first",
            cap.dir.display()
        ));
    }
    let dir = match dir {
        Some(d) if d.join("timeline.jsonl").exists() => {
            return Err(format!(
                "{} already holds a recording; name a new folder",
                d.display()
            ));
        }
        Some(d) => d,
        None => new_session_dir(&c.dir),
    };
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    // An asked-for session replaces the one that would have started by itself.
    r.auto_pending = false;
    let cap = start(c, app, ctx, dir.clone(), fps, false)?;
    r.capture = Some(cap);
    Ok(json!({"dir": dir, "fps": fps}))
}

/// Stops the session; ffmpeg finishes the video in the background, see [`writers_done`].
pub fn stop_session(app: &App) -> Result<Value, String> {
    config().ok_or(NOT_ON)?;
    let mut guard = recorder();
    let r = guard.as_mut().ok_or("nothing is being recorded")?;
    let cap = r.capture.take().ok_or("nothing is being recorded")?;
    let (dir, slot, fps, dropped) = (cap.dir.clone(), cap.slot, cap.fps, cap.repeated());
    if let Some(w) = stop(*cap, app) {
        r.finishing.push(w);
    }
    Ok(json!({
        "dir": dir,
        "seconds": (slot + 1).max(0) as f64 / fps,
        "frames_dropped": dropped,
    }))
}

/// Whether the video of every stopped session is closed.
pub fn writers_done() -> bool {
    recorder()
        .as_ref()
        .is_none_or(|r| r.finishing.iter().all(|w| w.is_finished()))
}

/// The session being recorded, for `status`.
pub fn recording() -> Option<Value> {
    let c = config()?;
    let guard = recorder();
    let cap = guard.as_ref()?.capture.as_ref()?;
    Some(json!({
        "dir": cap.dir,
        "fps": cap.fps,
        "seconds": now(c) - cap.t0,
        "frames_dropped": cap.repeated(),
    }))
}

const NOT_ON: &str = "recording works only in the recording mode (GLOSSPLAY_RECORD_DIR)";

/// `rec-001`, `rec-002`… in the recording folder, the first that is not there.
fn new_session_dir(base: &Path) -> PathBuf {
    (1..)
        .map(|n| base.join(format!("rec-{n:03}")))
        .find(|d| !d.exists())
        .expect("some number is free")
}

/// Called from `on_exit`: closes the video and the film's sound file.
pub fn finish(app: &App) {
    let mut guard = recorder();
    let Some(r) = guard.as_mut() else { return };
    if let Some(cap) = r.capture.take()
        && let Some(w) = stop(*cap, app)
    {
        r.finishing.push(w);
    }
    for w in r.finishing.drain(..) {
        let _ = w.join();
    }
}

fn start(
    c: &Config,
    app: &mut App,
    ctx: &egui::Context,
    dir: PathBuf,
    fps: f64,
    auto: bool,
) -> Result<Box<Capture>, String> {
    let log = std::fs::File::create(dir.join("timeline.jsonl"))
        .map_err(|e| format!("cannot write in {}: {e}", dir.display()))?;
    *session() = Some(Session {
        dir: dir.clone(),
        log: Some(log),
        clips: 0,
    });
    let mpv = &app.player.mpv;
    // A fresh output makes the first sample of video.wav the position playback is at now.
    mpv.set("ao-pcm-file", &dir.join("video.wav").to_string_lossy());
    let reloaded = mpv.command(&["ao-reload"]).is_ok();
    let (tx, rx) = std::sync::mpsc::sync_channel::<Frame>(6);
    let writer_dir = dir.clone();
    let ffmpeg = app.ffmpeg.clone();
    let writer = std::thread::spawn(move || write_frames(&writer_dir, ffmpeg.as_deref(), fps, rx));
    let t = now(c);
    // The wall clock lines this recording up with one made by another program at the same time.
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64());
    event(
        "start",
        json!({"pos": app.snap.pos, "ao_reload": reloaded, "fps": fps, "unix": unix,
            "media": app.session.is_some()}),
    );
    ctx.request_repaint();
    Ok(Box::new(Capture {
        dir,
        fps,
        auto,
        t0: t,
        slot: -1,
        pending: None,
        frames: tx,
        shot: Arc::default(),
        writer: Some(writer),
        paused: None,
        volumes: (-1.0, -1.0),
        lines: None,
        sent: 0,
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
            Ok(()) => {
                cap.slot = slot;
                cap.sent += 1;
            }
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {}
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
            .primary_lines(app.subtitle_pos())
            .into_iter()
            .map(|l| l.text)
            .collect();
        (primary, s.secondary_lines(app.subtitle_pos()))
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
    let slot = ((now(c) - cap.t0) * cap.fps).round() as i64;
    if slot > cap.slot {
        cap.pending = Some(slot);
        painter.add(video::capture_callback(
            ctx.content_rect(),
            cap.shot.clone(),
        ));
    }
    // Painted after the read-back above, so the window shows it and the recording does not.
    draw_rec_sign(&painter, ctx.content_rect(), now(c));
    event(
        "pos",
        json!({"pos": app.snap.pos, "paused": paused, "bytes": written(&cap.dir)}),
    );
    ctx.request_repaint();
}

/// A red dot and REC in the top right corner, the dot slowly pulsing.
fn draw_rec_sign(painter: &egui::Painter, screen: egui::Rect, t: f64) {
    let red = egui::Color32::from_rgb(230, 40, 40);
    let plate = egui::Rect::from_min_size(
        egui::pos2(screen.right() - 86.0, screen.top() + 10.0),
        egui::vec2(72.0, 26.0),
    );
    painter.rect_filled(plate, 6.0, egui::Color32::from_black_alpha(170));
    let pulse = 0.6 + 0.4 * (t * std::f64::consts::PI).sin().abs() as f32;
    painter.circle_filled(
        egui::pos2(plate.left() + 15.0, plate.center().y),
        6.0,
        red.gamma_multiply(pulse),
    );
    painter.text(
        egui::pos2(plate.left() + 28.0, plate.center().y),
        egui::Align2::LEFT_CENTER,
        "REC",
        egui::FontId::proportional(14.0),
        egui::Color32::WHITE,
    );
}

/// Ends the session: closes the film's sound file and lets ffmpeg finish the video.
fn stop(mut cap: Capture, app: &App) -> Option<std::thread::JoinHandle<()>> {
    // A fresh output closes video.wav, which writes its header; until the next session the
    // sound goes to a scratch file again.
    if let Some(c) = config() {
        app.player
            .mpv
            .set("ao-pcm-file", &path(c, "video-before.wav"));
        app.player.mpv.command(&["ao-reload"]).ok();
    }
    event(
        "stop",
        json!({"pos": app.snap.pos, "last_slot": cap.slot, "dropped": cap.repeated()}),
    );
    *session() = None;
    let (tx, _) = std::sync::mpsc::sync_channel(1);
    drop(std::mem::replace(&mut cap.frames, tx));
    cap.writer.take()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_takes_the_time_asked_for() {
        // 8 characters over 0.64 s: one at once, then one every 0.08 s.
        assert_eq!(typed_by(0.0, 0.64, 8), 1);
        assert_eq!(typed_by(0.05, 0.64, 8), 1);
        assert_eq!(typed_by(0.08, 0.64, 8), 2);
        assert_eq!(typed_by(0.33, 0.64, 8), 5);
        assert_eq!(typed_by(0.64, 0.64, 8), 8);
        assert_eq!(typed_by(9.0, 0.64, 8), 8);
        assert_eq!(
            typed_by(0.0, 0.0, 8),
            8,
            "no time given types it all at once"
        );
    }
}
