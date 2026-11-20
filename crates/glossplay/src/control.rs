//! `--mcp`: an AI agent drives the real player through the Model Context Protocol.
//!
//! A reader thread takes requests from stdin (see `mcp.rs`); tool calls come to the UI thread
//! through a channel and run in `step`, once a frame, against the same `App` methods the
//! mouse and keyboard use. A call that has to wait for something (a file to open, a seek to
//! land, a translation) stays pending and is answered by a later frame.

// Declared here rather than in main.rs, to keep the MCP code out of the files other work
// touches at the same time.
#[path = "mcp.rs"]
mod mcp;

use crate::app::{App, Async, SidebarTab};
use crate::i18n::{self, Tr, t};
use crate::session::{Load, Origin, Secondary, Session, normalize_lang};
use crate::video;
use eframe::egui;
use gp_core::{Ms, TokenKind};
use mcp::{Action, Outbox, Protocol, ToolCall};
use serde_json::{Value, json};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

type Reply = Result<Value, String>;
/// Polled once a frame; `true` means the call's time is up and it must answer now.
type Wait = Box<dyn FnMut(&mut App, &egui::Context, bool) -> Option<Reply>>;

enum Outcome {
    Now(Reply),
    Later { wait: Wait, limit: Duration },
}

enum Incoming {
    Call(ToolCall),
    Cancel(Value),
}

struct Launch {
    quiet: bool,
    media: Option<String>,
}

static ACTIVE: AtomicBool = AtomicBool::new(false);
static STDIN_CLOSED: AtomicBool = AtomicBool::new(false);
static LAUNCH: Mutex<Option<Launch>> = Mutex::new(None);
static INBOX: Mutex<Option<Receiver<Incoming>>> = Mutex::new(None);
static OUTBOX: OnceLock<Arc<Outbox>> = OnceLock::new();
static CTX: OnceLock<egui::Context> = OnceLock::new();

/// After stdin closes the window should be gone within a second or two; this is for when the
/// UI cannot get there (a minimized window runs no frames).
const EXIT_BACKSTOP: Duration = Duration::from_secs(6);
/// The watchdog answers a call this long after its own time limit, if the UI has not.
const GRACE: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(30);

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    /// Where each subtitle word was drawn this frame: (track, cue, token) to its rectangle.
    static WORDS: RefCell<HashMap<(usize, usize, usize), egui::Rect>> = RefCell::new(HashMap::new());
    static CHROME: Cell<Option<bool>> = const { Cell::new(None) };
}

/// Called by `main` after reading the command line. With `--mcp` it takes stdin and stdout
/// over for the protocol and takes the file to open, so it opens only once `--quiet` has
/// muted the player.
pub fn launch(media: &mut Option<String>) {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.iter().any(|a| a == "--mcp") {
        return;
    }
    let (input, output) = match mcp::take_stdio() {
        Ok(io) => io,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let outbox = Arc::new(Outbox::new(Box::new(output)));
    let _ = OUTBOX.set(outbox.clone());
    let (tx, rx) = channel();
    *INBOX.lock().unwrap() = Some(rx);
    *LAUNCH.lock().unwrap() = Some(Launch {
        quiet: args.iter().any(|a| a == "--quiet"),
        media: media.take(),
    });
    ACTIVE.store(true, Ordering::SeqCst);

    let reader_outbox = outbox.clone();
    std::thread::spawn(move || {
        let mut protocol = Protocol::new(tool_list());
        let input = std::io::BufReader::new(input);
        mcp::serve(input, &mut protocol, &reader_outbox, |action| {
            let msg = match action {
                Action::Call(call) => {
                    reader_outbox.owe(&call, time_limit(&call.name, &call.args) + GRACE);
                    Incoming::Call(call)
                }
                Action::Cancel(id) => {
                    reader_outbox.cancel(&id);
                    Incoming::Cancel(id)
                }
                Action::Send(_) => return,
            };
            let _ = tx.send(msg);
            wake();
        });
        STDIN_CLOSED.store(true, Ordering::SeqCst);
        wake();
        std::thread::sleep(EXIT_BACKSTOP);
        eprintln!("mcp: the window did not close after stdin ended; leaving");
        reader_outbox.fail_all("the player is shutting down");
        gp_media::tools::stop_all();
        std::process::exit(0);
    });
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_millis(500));
            outbox.expire(
                Instant::now(),
                "the player did not finish in time (is its window minimized or busy?)",
            );
        }
    });
}

/// Called by `main` once the window has closed: whatever is still pending gets an error.
pub fn finish() {
    if let Some(outbox) = OUTBOX.get() {
        outbox.fail_all("the player window was closed");
    }
}

fn wake() {
    if let Some(ctx) = CTX.get() {
        ctx.request_repaint();
    }
}

/// Called by the subtitle renderer for every word it draws.
pub fn word_drawn(track: usize, cue: usize, token: usize, rect: egui::Rect) {
    if ACTIVE.load(Ordering::Relaxed) {
        WORDS.with(|w| w.borrow_mut().insert((track, cue, token), rect));
    }
}

/// `Some(visible)` while the agent has fixed the control bar and header on or off.
pub fn controls_override() -> Option<bool> {
    CHROME.with(Cell::get)
}

/// Runs at the end of every frame.
pub fn step(app: &mut App, ctx: &egui::Context) {
    if !ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    STATE.with(|cell| {
        let mut slot = cell.borrow_mut();
        let st = slot.get_or_insert_with(|| State::new(app, ctx));
        st.run(app, ctx);
    });
    WORDS.with(|w| w.borrow_mut().clear());
}

struct Pending {
    call: ToolCall,
    deadline: Instant,
    wait: Wait,
}

struct State {
    rx: Receiver<Incoming>,
    outbox: Arc<Outbox>,
    pending: Vec<Pending>,
    /// Settings scroll to a section only while asked every frame; this keeps asking a while.
    scroll: Option<(Instant, Section)>,
    closing: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Section {
    SpeechRecognition,
    VoiceOver,
}

impl State {
    fn new(app: &mut App, ctx: &egui::Context) -> State {
        let _ = CTX.set(ctx.clone());
        let launch = LAUNCH.lock().unwrap().take();
        if let Some(l) = &launch
            && l.quiet
        {
            app.quiet = true;
            app.player.mpv.set_flag("mute", true);
            // Started by the config before the flag was known; the next one starts silent.
            if app.dub.take().is_some() {
                app.set_dub(true);
            }
        }
        if let Some(media) = launch.and_then(|l| l.media) {
            app.open(&media);
        }
        State {
            rx: INBOX.lock().unwrap().take().expect("launch set the inbox"),
            outbox: OUTBOX.get().expect("launch set the outbox").clone(),
            pending: Vec::new(),
            scroll: None,
            closing: false,
        }
    }

    fn run(&mut self, app: &mut App, ctx: &egui::Context) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Incoming::Call(call) => self.dispatch(app, ctx, call),
                Incoming::Cancel(id) => self.pending.retain(|p| p.call.id != id),
            }
        }
        let now = Instant::now();
        let mut i = 0;
        while i < self.pending.len() {
            let p = &mut self.pending[i];
            let late = now >= p.deadline;
            let answer = (p.wait)(app, ctx, late);
            if answer.is_none() && !late {
                i += 1;
                continue;
            }
            let p = self.pending.remove(i);
            let answer = answer.unwrap_or_else(|| Err(format!("{} timed out", p.call.name)));
            self.outbox.finish(&p.call.id, p.call.modern, answer);
        }
        if let Some((until, section)) = self.scroll {
            if now < until && app.settings_draft.is_some() {
                match section {
                    Section::SpeechRecognition => app.asr_setup.scroll_to = true,
                    Section::VoiceOver => app.tts_setup.scroll_to = true,
                }
            } else {
                self.scroll = None;
            }
        }
        if STDIN_CLOSED.load(Ordering::SeqCst) && !self.closing {
            self.closing = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if !self.pending.is_empty() || self.scroll.is_some() {
            ctx.request_repaint_after(POLL);
        }
    }

    fn dispatch(&mut self, app: &mut App, ctx: &egui::Context, call: ToolCall) {
        let outcome = run_tool(self, app, ctx, &call.name, &call.args)
            .unwrap_or_else(|e| Outcome::Now(Err(e)));
        match outcome {
            Outcome::Now(reply) => {
                self.outbox.finish(&call.id, call.modern, reply);
            }
            Outcome::Later { wait, limit } => self.pending.push(Pending {
                deadline: Instant::now() + limit,
                call,
                wait,
            }),
        }
    }
}

// ----- arguments -----

fn opt_f64(args: &Value, name: &str) -> Result<Option<f64>, String> {
    match args.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_f64()
            .filter(|f| f.is_finite())
            .map(Some)
            .ok_or_else(|| format!("{name} must be a number")),
    }
}

fn need_f64(args: &Value, name: &str) -> Result<f64, String> {
    opt_f64(args, name)?.ok_or_else(|| format!("{name} is required"))
}

fn opt_bool(args: &Value, name: &str) -> Result<Option<bool>, String> {
    match args.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_bool()
            .map(Some)
            .ok_or_else(|| format!("{name} must be true or false")),
    }
}

fn opt_str<'a>(args: &'a Value, name: &str) -> Result<Option<&'a str>, String> {
    match args.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_str()
            .map(Some)
            .ok_or_else(|| format!("{name} must be a string")),
    }
}

fn need_str<'a>(args: &'a Value, name: &str) -> Result<&'a str, String> {
    opt_str(args, name)?.ok_or_else(|| format!("{name} is required"))
}

fn opt_index(args: &Value, name: &str) -> Result<Option<usize>, String> {
    match args.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .map(|n| Some(n as usize))
            .ok_or_else(|| format!("{name} must be a whole number from 0")),
    }
}

fn secs(ms: Ms) -> f64 {
    (ms as f64).round() / 1000.0
}

fn to_ms(seconds: f64) -> Ms {
    (seconds * 1000.0).round() as Ms
}

fn session(app: &App) -> Result<&Session, String> {
    app.session
        .as_ref()
        .ok_or_else(|| "no file is open: call open first".to_owned())
}

/// How long a tool may take before its call is answered with an error.
fn time_limit(name: &str, args: &Value) -> Duration {
    let asked = args
        .get("timeout_seconds")
        .and_then(Value::as_f64)
        .filter(|s| s.is_finite() && *s > 0.0)
        .map(|s| Duration::from_secs_f64(s.min(3600.0)));
    match name {
        "wait_for" => asked.unwrap_or(Duration::from_secs(30)),
        "open" => asked.unwrap_or(Duration::from_secs(90)),
        "select_subtitles" => Duration::from_secs(90),
        "click_word" => Duration::from_secs(20),
        _ => Duration::from_secs(10),
    }
}

fn later(name: &str, args: &Value, wait: Wait) -> Result<Outcome, String> {
    Ok(Outcome::Later {
        wait,
        limit: time_limit(name, args),
    })
}

// ----- the tools -----

fn run_tool(
    st: &mut State,
    app: &mut App,
    ctx: &egui::Context,
    name: &str,
    args: &Value,
) -> Result<Outcome, String> {
    let now = |v: Value| Ok(Outcome::Now(Ok(v)));
    match name {
        "open" => open(app, args),
        "play" => {
            session(app)?;
            app.close_popup();
            app.player.set_paused(false);
            now(json!({"paused": false, "position": secs(app.player.snapshot().pos)}))
        }
        "pause" => {
            session(app)?;
            app.player.set_paused(true);
            now(json!({"paused": true, "position": secs(app.player.snapshot().pos)}))
        }
        "seek" => seek(app, args),
        "set_speed" => {
            let speed = need_f64(args, "speed")?;
            if !(0.25..=4.0).contains(&speed) {
                return Err(format!("speed {speed} is out of range: 0.25 to 4"));
            }
            app.player.mpv.set_f64("speed", speed);
            now(json!({"speed": app.player.mpv.get_f64("speed")}))
        }
        "set_volume" => {
            let percent = need_f64(args, "percent")?;
            if !(0.0..=150.0).contains(&percent) {
                return Err(format!("percent {percent} is out of range: 0 to 150"));
            }
            app.set_volume(percent);
            if let Some(muted) = opt_bool(args, "muted")? {
                app.player.mpv.set_flag("mute", muted || app.quiet);
            }
            now(json!({
                "volume": app.volume,
                "muted": app.player.mpv.get_flag("mute"),
                "quiet": app.quiet,
            }))
        }
        "status" => now(status(app, ctx)),
        "list_tracks" => {
            session(app)?;
            now(tracks_json(app))
        }
        "select_subtitles" => select_subtitles(app, args),
        "set_languages" => set_languages(app, args),
        "current_lines" => now(current_lines(app)?),
        "transcript" => now(transcript(app, args)?),
        "transcribe" => transcribe(app, args),
        "voice_over" => voice_over(app, args),
        "wait_for" => wait_for(app, args),
        "click_word" => click_word(app, ctx, args),
        "close_word_card" => {
            let Some(p) = &app.popup else {
                return Err("no word card is open".into());
            };
            let (word, resumes) = (p.word.clone(), p.resume_on_close);
            app.close_popup();
            now(json!({"closed": word, "playing": resumes}))
        }
        "save_word" => {
            let Some(word) = app.popup.as_ref().map(|p| p.word.clone()) else {
                return Err("no word card is open: click_word first".into());
            };
            let id = app
                .save_popup_word()
                .ok_or_else(|| format!("could not save \"{word}\""))?;
            now(json!({"saved": word, "id": id, "saved_words": app.vocab.len()}))
        }
        "toggle_sidebar" => {
            let open = opt_bool(args, "open")?.unwrap_or(!app.sidebar_open);
            match opt_str(args, "tab")? {
                None => {}
                Some("transcript") => app.sidebar_tab = SidebarTab::Transcript,
                Some("words") => app.sidebar_tab = SidebarTab::Words,
                Some(other) => return Err(format!("unknown tab \"{other}\": transcript or words")),
            }
            app.sidebar_open = open;
            now(json!({"open": open, "tab": tab_name(app.sidebar_tab)}))
        }
        "search_transcript" => search_transcript(app, args),
        "show_controls" => {
            let choice = match args.get("visible") {
                Some(Value::Bool(b)) => Some(*b),
                Some(Value::String(s)) if s == "auto" => None,
                _ => return Err("visible must be true, false or \"auto\"".into()),
            };
            CHROME.with(|c| c.set(choice));
            now(json!({"controls": choice.map_or(json!("auto"), Value::Bool)}))
        }
        "open_settings" => open_settings(st, app, args),
        "close_dialogs" => {
            let closed = json!({
                "settings": app.settings_draft.take().is_some(),
                "speech_recognition_setup": app.asr_setup.dialog_open,
                "voice_over_setup": app.tts_setup.dialog_open,
            });
            if app.asr_setup.dialog_open {
                app.close_asr_setup();
            }
            if app.tts_setup.dialog_open {
                app.close_tts_setup();
            }
            st.scroll = None;
            now(json!({"closed": closed}))
        }
        "set_ui_language" => {
            let lang = need_str(args, "lang")?.trim().to_ascii_lowercase();
            if lang != "auto" && i18n::Lang::parse(&lang).is_none() {
                return Err(format!(
                    "unknown interface language \"{lang}\": en, ru, uk or auto"
                ));
            }
            app.cfg.general.ui_lang.clone_from(&lang);
            i18n::apply_setting(&lang);
            save_config(app);
            now(json!({"ui_lang": i18n::lang().code(), "setting": lang}))
        }
        "set_window" => set_window(app, ctx, args),
        "press_key" => press_key(app, ctx, args),
        "screenshot" => screenshot(app, args),
        _ => Err(format!("{name} is not implemented")),
    }
}

fn save_config(app: &mut App) {
    // A --quiet run is a test run: it leaves the user's settings as they were.
    if !app.quiet
        && let Err(e) = crate::paths::save_config(&app.cfg)
    {
        app.toast(crate::i18n::tf(Tr::CouldNotSaveSettings, &[&e]));
    }
}

fn tab_name(tab: SidebarTab) -> &'static str {
    match tab {
        SidebarTab::Transcript => "transcript",
        SidebarTab::Words => "words",
    }
}

fn open(app: &mut App, args: &Value) -> Result<Outcome, String> {
    let target = need_str(args, "path_or_url")?.trim().to_owned();
    if target.is_empty() {
        return Err("path_or_url is empty".into());
    }
    let expected = if target.contains("://") {
        target.clone()
    } else {
        let path = std::path::absolute(&target).map_err(|e| format!("{target}: {e}"))?;
        if !path.is_file() {
            return Err(format!("{} is not a file", path.display()));
        }
        path.to_string_lossy().into_owned()
    };
    app.last_toast.clear();
    app.open(&target);
    let failed_text = t(Tr::CouldNotPlay);
    let wait: Wait = Box::new(move |app, _, late| {
        if let Some(s) = app.session.as_ref().filter(|s| s.media == expected)
            && s.tracks_discovered
        {
            let mut v = tracks_json(app);
            v["file"] = json!(s.media);
            v["title"] = json!(s.title);
            v["duration"] = json!(secs(app.player.snapshot().duration));
            return Some(Ok(v));
        }
        if app.last_toast == failed_text {
            return Some(Err(format!("could not play {expected}")));
        }
        late.then(|| {
            let note = if app.last_toast.is_empty() {
                String::new()
            } else {
                format!(" (the player says: {})", app.last_toast)
            };
            Err(format!("{expected} did not open in time{note}"))
        })
    });
    later("open", args, wait)
}

fn seek(app: &mut App, args: &Value) -> Result<Outcome, String> {
    session(app)?;
    let seconds = need_f64(args, "seconds")?;
    let exact = opt_bool(args, "exact")?.unwrap_or(true);
    let duration = app.player.snapshot().duration;
    if seconds < 0.0 || (duration > 0 && to_ms(seconds) > duration) {
        return Err(format!(
            "{seconds} s is outside the file: it lasts {} s",
            secs(duration)
        ));
    }
    let mode = if exact {
        "absolute+exact"
    } else {
        "absolute+keyframes"
    };
    app.player
        .mpv
        .command(&["seek", &format!("{seconds:.3}"), mode])
        .map_err(|e| format!("seek failed: {e}"))?;
    let mut frames = 0;
    let wait: Wait = Box::new(move |app, _, late| {
        frames += 1;
        let mpv = &app.player.mpv;
        let pos = mpv.get_f64("time-pos").unwrap_or(-1.0);
        let seeking = mpv.get_flag("seeking").unwrap_or(false);
        let landed = !seeking && frames > 1 && (!exact || (pos - seconds).abs() < 0.25);
        if landed {
            return Some(Ok(json!({
                "position": (pos * 1000.0).round() / 1000.0,
                "paused": mpv.get_flag("pause").unwrap_or(true),
            })));
        }
        late.then(|| {
            Err(format!(
                "the seek to {seconds} s did not land in time (at {pos:.3})"
            ))
        })
    });
    later("seek", args, wait)
}

fn load_name(load: &Load) -> &'static str {
    match load {
        Load::NotLoaded => "not_loaded",
        Load::Loading => "loading",
        Load::Ready => "ready",
        Load::Failed(_) => "failed",
        Load::Bitmap => "image",
    }
}

fn tracks_json(app: &App) -> Value {
    let Some(s) = &app.session else {
        return json!({"tracks": []});
    };
    let tracks: Vec<Value> = s
        .tracks
        .iter()
        .enumerate()
        .map(|(i, tr)| {
            let mut v = json!({
                "index": i,
                "label": tr.label,
                "lang": tr.track.lang,
                "origin": match tr.origin {
                    Origin::Embedded { .. } => "embedded",
                    Origin::External { .. } => "external",
                    Origin::Asr => "speech_recognition",
                },
                "state": load_name(&tr.load),
                "lines": tr.track.cues.len(),
                "primary": s.primary == Some(i),
                "secondary": s.secondary == Secondary::Track(i),
            });
            if let Load::Failed(e) = &tr.load {
                v["error"] = json!(e);
            }
            v
        })
        .collect();
    let audio: Vec<Value> = app
        .audio_tracks
        .iter()
        .map(|a| json!({"id": a.id, "label": a.label(), "lang": a.lang, "selected": a.selected}))
        .collect();
    json!({
        "tracks": tracks,
        "primary": s.primary,
        "secondary": secondary_json(s.secondary),
        "audio": audio,
        "translation": match &app.translator {
            Ok(tr) => json!({"provider": tr.id(), "into": app.cfg.general.native_lang}),
            Err(e) => json!({"error": e}),
        },
        "learn_lang": app.cfg.general.learn_lang,
        "native_lang": app.cfg.general.native_lang,
    })
}

fn secondary_json(s: Secondary) -> Value {
    match s {
        Secondary::Off => json!("off"),
        Secondary::Translate => json!("translate"),
        Secondary::Track(i) => json!(i),
    }
}

/// A track named by index or by language; `except` is the primary, when picking the other.
fn find_track(s: &Session, v: &Value, except: Option<usize>) -> Result<usize, String> {
    if let Some(i) = v.as_u64() {
        let i = i as usize;
        let n = s.tracks.len();
        return match s.tracks.get(i) {
            None => Err(format!("track {i} does not exist: there are {n} tracks")),
            Some(tr) if Some(i) == except => Err(format!(
                "track {i} ({}) is already the primary track",
                tr.label
            )),
            Some(_) => Ok(i),
        };
    }
    let Some(lang) = v.as_str() else {
        return Err("a track is an index or a language code".into());
    };
    let want = normalize_lang(lang);
    s.tracks
        .iter()
        .enumerate()
        .find(|(i, tr)| {
            tr.is_text() && Some(*i) != except && tr.track.lang.as_deref() == Some(want.as_str())
        })
        .map(|(i, _)| i)
        .ok_or_else(|| {
            let have: Vec<String> = s
                .tracks
                .iter()
                .filter(|tr| tr.is_text())
                .filter_map(|tr| tr.track.lang.clone())
                .collect();
            format!(
                "no text subtitle track in \"{lang}\"{}; languages here: {}",
                if except.is_some() {
                    " apart from the primary"
                } else {
                    ""
                },
                if have.is_empty() {
                    "none".to_owned()
                } else {
                    have.join(", ")
                }
            )
        })
}

fn select_subtitles(app: &mut App, args: &Value) -> Result<Outcome, String> {
    let s = session(app)?;
    if !s.tracks_discovered {
        return Err("the file is still opening: wait_for tracks_ready first".into());
    }
    let primary = match args.get("primary") {
        None | Some(Value::Null) => None,
        Some(Value::String(v)) if v == "none" => Some(None),
        Some(v) => Some(Some(find_track(s, v, None)?)),
    };
    let current_primary = primary.unwrap_or(s.primary);
    let secondary = match args.get("secondary") {
        None | Some(Value::Null) => None,
        Some(Value::String(v)) if v == "off" => Some(Secondary::Off),
        Some(Value::String(v)) if v == "translate" => Some(Secondary::Translate),
        Some(v) => Some(Secondary::Track(find_track(s, v, current_primary)?)),
    };
    if primary.is_none() && secondary.is_none() {
        return Err("give primary, secondary or both".into());
    }
    if let Some(p) = primary {
        app.select_primary(p);
        // The other line cannot show the same track twice.
        if let Some(i) = p
            && app
                .session
                .as_ref()
                .is_some_and(|s| s.secondary == Secondary::Track(i))
        {
            app.set_secondary(Secondary::Off);
        }
    }
    if let Some(sec) = secondary {
        app.set_secondary(sec);
    }
    let wait: Wait = Box::new(move |app, _, late| {
        let s = app.session.as_ref()?;
        let watched: Vec<usize> = s
            .primary
            .into_iter()
            .chain(match s.secondary {
                Secondary::Track(i) => Some(i),
                _ => None,
            })
            .collect();
        let loading = watched
            .iter()
            .any(|&i| s.tracks.get(i).is_some_and(|tr| tr.load == Load::Loading));
        if loading && !late {
            return None;
        }
        let failed: Vec<String> = watched
            .iter()
            .filter_map(|&i| match &s.tracks.get(i)?.load {
                Load::Failed(e) => Some(format!("track {i}: {e}")),
                _ => None,
            })
            .collect();
        if !failed.is_empty() {
            return Some(Err(format!("could not read {}", failed.join("; "))));
        }
        if loading {
            return Some(Err(
                "the subtitles are still loading; wait_for tracks_ready".into(),
            ));
        }
        Some(Ok(tracks_json(app)))
    });
    later("select_subtitles", args, wait)
}

fn set_languages(app: &mut App, args: &Value) -> Result<Outcome, String> {
    let check = |name: &str| -> Result<Option<String>, String> {
        let Some(v) = opt_str(args, name)? else {
            return Ok(None);
        };
        let v = v.trim().to_ascii_lowercase();
        let fine = v.len() <= 16 && v.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
        if !fine || (name == "native" && v.is_empty()) {
            return Err(format!(
                "{name} must be a language code such as en, ru, de, pt-br"
            ));
        }
        Ok(Some(v))
    };
    let (learn, native) = (check("learn")?, check("native")?);
    if learn.is_none() && native.is_none() {
        return Err("give learn, native or both".into());
    }
    let mut cfg = app.cfg.clone();
    if let Some(l) = learn {
        cfg.general.learn_lang = l;
    }
    if let Some(n) = native {
        cfg.general.native_lang = n;
    }
    // Same path as Save in the Settings window: translations into the old language are
    // dropped and asked again in the new one.
    let quiet = app.quiet;
    if quiet {
        let (old_learn, old_native) = (
            app.cfg.general.learn_lang.clone(),
            app.cfg.general.native_lang.clone(),
        );
        app.cfg
            .general
            .learn_lang
            .clone_from(&cfg.general.learn_lang);
        app.cfg
            .general
            .native_lang
            .clone_from(&cfg.general.native_lang);
        if old_native != cfg.general.native_lang || old_learn != cfg.general.learn_lang {
            if let Some(s) = &mut app.session {
                for tr in &mut s.tracks {
                    tr.reset_translations();
                }
                s.translate_failed_at = None;
            }
            app.popup = None;
            if old_native != cfg.general.native_lang && app.dub.take().is_some() {
                app.set_dub(true);
            }
        }
    } else {
        app.apply_settings(cfg);
    }
    Ok(Outcome::Now(Ok(json!({
        "learn": app.cfg.general.learn_lang,
        "native": app.cfg.general.native_lang,
    }))))
}

/// The words of a line, in order: (token index, text).
fn words_of(
    app: &mut App,
    track: usize,
    cue: usize,
    text: &str,
    lang: Option<&str>,
) -> Vec<(usize, String)> {
    let tokens = app.tokens_for(track, cue, text, lang);
    tokens
        .iter()
        .enumerate()
        .filter(|(_, tk)| tk.kind == TokenKind::Word)
        .map(|(i, tk)| (i, text[tk.range.clone()].to_owned()))
        .collect()
}

fn current_lines(app: &mut App) -> Result<Value, String> {
    let s = session(app)?;
    let pos = app.snap.pos;
    let track = s.primary.unwrap_or(0);
    let lang = s.primary_track().and_then(|tr| tr.track.lang.clone());
    let lines: Vec<(usize, Ms, Ms, String, Option<String>)> = s
        .primary_lines(pos)
        .into_iter()
        .map(|l| {
            let tr = s.translation_of(l.cue);
            (l.cue, l.start, l.end, l.text, tr)
        })
        .collect();
    let secondary = s.secondary_lines(pos);
    let secondary_mode = secondary_json(s.secondary);
    let mut out = Vec::new();
    for (n, (cue, start, end, text, translation)) in lines.into_iter().enumerate() {
        let words: Vec<Value> = words_of(app, track, cue, &text, lang.as_deref())
            .into_iter()
            .enumerate()
            .map(|(index, (token, word))| {
                let mut w = json!({"index": index, "text": word});
                if let Some(rect) = word_rect(track, cue, token) {
                    w["rect"] = rect_px(app, rect);
                }
                w
            })
            .collect();
        out.push(json!({
            "line": n,
            "cue": cue,
            "start": secs(start),
            "end": secs(end),
            "text": text,
            "translation": translation,
            "words": words,
        }));
    }
    Ok(json!({
        "position": secs(pos),
        "lines": out,
        "secondary": secondary,
        "secondary_source": secondary_mode,
        "shown": {
            "primary": app.cfg.subtitles.show_primary,
            "secondary": app.cfg.subtitles.show_secondary,
        },
    }))
}

fn transcript(app: &App, args: &Value) -> Result<Value, String> {
    let s = session(app)?;
    let Some(tr) = s.primary_track() else {
        return Err("no primary subtitle track is loaded: select_subtitles or transcribe".into());
    };
    let from = opt_f64(args, "from")?.map_or(Ms::MIN, to_ms);
    let to = opt_f64(args, "to")?.map_or(Ms::MAX, to_ms);
    let limit = opt_index(args, "limit")?.unwrap_or(100).clamp(1, 2000);
    let matching: Vec<usize> = (0..tr.track.cues.len())
        .filter(|&i| {
            let c = &tr.track.cues[i];
            c.end >= from && c.start <= to
        })
        .collect();
    let lines: Vec<Value> = matching
        .iter()
        .take(limit)
        .map(|&i| {
            let c = &tr.track.cues[i];
            json!({
                "cue": i,
                "start": secs(c.start),
                "end": secs(c.end),
                "text": c.text,
                "translation": s.translation_of(i),
            })
        })
        .collect();
    Ok(json!({
        "track": s.primary,
        "total": tr.track.cues.len(),
        "matching": matching.len(),
        "truncated": matching.len() > lines.len(),
        "lines": lines,
        "note": "machine translations are made around the playhead and the sidebar view; \
                 lines further away may have none yet",
    }))
}

fn asr_json(app: &App) -> Value {
    let Some(s) = &app.session else {
        return json!({"running": false});
    };
    let track = s.tracks.iter().position(|tr| tr.origin == Origin::Asr);
    json!({
        "running": s.asr_running,
        "status": s.asr_status,
        "track": track,
        "lines": track.map_or(0, |i| s.tracks[i].track.cues.len()),
    })
}

fn transcribe(app: &mut App, args: &Value) -> Result<Outcome, String> {
    let running = session(app)?.asr_running;
    match opt_bool(args, "start")? {
        Some(true) if !running => {
            app.last_toast.clear();
            app.snap = app.player.snapshot();
            app.start_asr();
            if !session(app)?.asr_running {
                let why = if app.last_toast.is_empty() {
                    "speech recognition did not start".to_owned()
                } else {
                    app.last_toast.clone()
                };
                return Err(why);
            }
        }
        Some(false) => app.stop_asr(),
        _ => {}
    }
    Ok(Outcome::Now(Ok(asr_json(app))))
}

fn dub_json(app: &App) -> Value {
    json!({
        "enabled": app.dub.is_some(),
        "status": app.dub.as_ref().map(|d| d.status()),
        "original_volume": app.cfg.tts.original_volume,
        "voice_volume": app.cfg.tts.voice_volume,
        "engine_ready": app.dub_ready(),
    })
}

fn voice_over(app: &mut App, args: &Value) -> Result<Outcome, String> {
    let enabled = opt_bool(args, "enabled")?;
    for name in ["original_volume", "voice_volume"] {
        if let Some(v) = opt_f64(args, name)?
            && !(0.0..=150.0).contains(&v)
        {
            return Err(format!("{name} {v} is out of range: 0 to 150"));
        }
    }
    if let Some(v) = opt_f64(args, "original_volume")? {
        app.cfg.tts.original_volume = v;
    }
    if let Some(v) = opt_f64(args, "voice_volume")? {
        app.cfg.tts.voice_volume = v;
    }
    match enabled {
        Some(true) if app.dub.is_none() => {
            if !app.dub_ready() {
                return Err(
                    "the voice-over engine is not set up: install it in the voice-over \
                            setup (open_settings section voice_over_setup) or set a speech \
                            server in the settings"
                        .into(),
                );
            }
            app.last_toast.clear();
            app.set_dub(true);
            if app.dub.is_none() {
                return Err(format!("the voice-over did not start: {}", app.last_toast));
            }
        }
        Some(false) => app.set_dub(false),
        _ => {}
    }
    app.apply_volume();
    save_config(app);
    Ok(Outcome::Now(Ok(dub_json(app))))
}

fn search_transcript(app: &mut App, args: &Value) -> Result<Outcome, String> {
    let text = need_str(args, "text")?.to_owned();
    app.search.clone_from(&text);
    app.sidebar_open = true;
    app.sidebar_tab = SidebarTab::Transcript;
    let needle = text.to_lowercase();
    let found: Vec<Value> = app
        .session
        .as_ref()
        .and_then(|s| s.primary_track())
        .map(|tr| {
            tr.track
                .cues
                .iter()
                .enumerate()
                .filter(|(_, c)| c.text.to_lowercase().contains(&needle))
                .map(|(i, c)| json!({"cue": i, "start": secs(c.start), "text": c.text}))
                .collect()
        })
        .unwrap_or_default();
    Ok(Outcome::Now(Ok(json!({
        "search": text,
        "matches": found.len(),
        "first": found.iter().take(20).collect::<Vec<_>>(),
    }))))
}

fn open_settings(st: &mut State, app: &mut App, args: &Value) -> Result<Outcome, String> {
    let section = opt_str(args, "section")?.unwrap_or("top");
    match section {
        "top" | "speech_recognition" | "voice_over" => {
            if app.settings_draft.is_none() {
                app.settings_draft = Some(app.cfg.clone());
            }
            st.scroll = match section {
                "speech_recognition" => Some(Section::SpeechRecognition),
                "voice_over" => Some(Section::VoiceOver),
                _ => None,
            }
            .map(|s| (Instant::now() + Duration::from_millis(1500), s));
        }
        "speech_recognition_setup" => app.open_asr_setup(false),
        "voice_over_setup" => app.open_tts_setup(false),
        other => {
            return Err(format!(
                "unknown section \"{other}\": top, speech_recognition, voice_over, \
                 speech_recognition_setup or voice_over_setup"
            ));
        }
    }
    Ok(Outcome::Now(Ok(json!({"opened": section}))))
}

fn word_rect(track: usize, cue: usize, token: usize) -> Option<egui::Rect> {
    WORDS.with(|w| w.borrow().get(&(track, cue, token)).copied())
}

/// The window's drawing area in screen pixels, and the scale from points to pixels.
fn window_geometry(ctx: &egui::Context) -> (Option<egui::Rect>, Option<egui::Rect>, f32) {
    let ppp = ctx.pixels_per_point();
    let (inner, outer) = ctx.input(|i| (i.viewport().inner_rect, i.viewport().outer_rect));
    (inner.map(|r| r * ppp), outer.map(|r| r * ppp), ppp)
}

fn rect_px(app: &App, rect: egui::Rect) -> Value {
    let ctx = app.jobs.context();
    let (inner, _, ppp) = window_geometry(&ctx);
    let r = rect * ppp;
    let mut v = json!({
        "x": r.left().round(), "y": r.top().round(),
        "width": r.width().round(), "height": r.height().round(),
        "center": [r.center().x.round(), r.center().y.round()],
    });
    if let Some(inner) = inner {
        let c = r.center() + inner.min.to_vec2();
        v["screen_center"] = json!([c.x.round(), c.y.round()]);
    }
    v
}

fn async_text(a: &Async) -> (Option<String>, Option<String>) {
    match a {
        Async::Ready(t) => (Some(t.clone()), None),
        Async::Failed(e) => (None, Some(e.clone())),
        Async::Idle | Async::Pending => (None, None),
    }
}

fn click_word(app: &mut App, ctx: &egui::Context, args: &Value) -> Result<Outcome, String> {
    let index = opt_index(args, "index")?.ok_or("index is required")?;
    let line_no = opt_index(args, "line")?.unwrap_or(0);
    let s = session(app)?;
    let pos = app.snap.pos;
    let lines = s.primary_lines(pos);
    if lines.is_empty() {
        return Err(format!(
            "no subtitle line is shown at {} s: seek to a line or wait_for line_shown",
            secs(pos)
        ));
    }
    let Some(line) = lines.get(line_no) else {
        return Err(format!(
            "line {line_no} is out of range: {} line(s) are shown",
            lines.len()
        ));
    };
    let (cue, text) = (line.cue, line.text.clone());
    let track = s.primary.unwrap_or(0);
    let lang = s.primary_track().and_then(|tr| tr.track.lang.clone());
    let words = words_of(app, track, cue, &text, lang.as_deref());
    let Some((token, word)) = words.get(index).cloned() else {
        return Err(format!(
            "word index {index} is out of range: the line has {} words",
            words.len()
        ));
    };
    let rect = word_rect(track, cue, token);
    let screen = ctx.content_rect();
    let anchor = rect.map_or(
        egui::pos2(screen.center().x, screen.bottom() - 190.0),
        |r| r.center_top(),
    );
    app.open_word(word.clone(), cue, token, anchor);
    let request = app.popup.as_ref().map(|p| p.request);
    let where_ = rect.map(|r| rect_px(app, r));
    let wait: Wait = Box::new(move |app, _, late| {
        let p = app.popup.as_ref().filter(|p| Some(p.request) == request);
        let Some(p) = p else {
            return Some(Err(
                "the word card was closed before its translation came".into()
            ));
        };
        if matches!(p.translation, Async::Pending) && !late {
            return None;
        }
        let (translation, translation_error) = async_text(&p.translation);
        let (explanation, _) = async_text(&p.explanation);
        Some(Ok(json!({
            "word": p.word,
            "index": index,
            "translation": translation,
            "translation_error": translation_error,
            "translation_pending": matches!(p.translation, Async::Pending),
            "explanation": explanation,
            "sentence": p.sentence,
            "sentence_translation": p.sentence_translation,
            "rect": where_,
            "paused": true,
        })))
    });
    later("click_word", args, wait)
}

fn wait_for(app: &mut App, args: &Value) -> Result<Outcome, String> {
    let condition = need_str(args, "condition")?.to_owned();
    let value = args.get("value").cloned().unwrap_or(Value::Null);
    let started = Instant::now();
    match condition.as_str() {
        "tracks_ready" | "translation_ready" | "transcript_ready" | "line_shown" | "paused"
        | "playing" => {}
        "position" => {
            if !value.is_number() {
                return Err("position needs value: the position in seconds".into());
            }
        }
        other => {
            return Err(format!(
                "unknown condition \"{other}\": tracks_ready, translation_ready, \
                 transcript_ready, line_shown, position, paused or playing"
            ));
        }
    }
    if condition != "tracks_ready" {
        session(app)?;
    }
    let wait: Wait = Box::new(move |app, _, late| {
        let met = match check(app, &condition, &value) {
            Ok(met) => met,
            Err(e) => return Some(Err(e)),
        };
        if met {
            return Some(Ok(json!({
                "condition": condition,
                "waited": (started.elapsed().as_secs_f64() * 100.0).round() / 100.0,
                "position": secs(app.snap.pos),
                "paused": app.snap.paused,
            })));
        }
        late.then(|| {
            Err(format!(
                "timed out after {:.1} s waiting for {condition}; position {} s, {}",
                started.elapsed().as_secs_f64(),
                secs(app.snap.pos),
                if app.snap.paused { "paused" } else { "playing" }
            ))
        })
    });
    later("wait_for", args, wait)
}

/// Whether a `wait_for` condition holds now; an error when it never can.
fn check(app: &App, condition: &str, value: &Value) -> Result<bool, String> {
    let Some(s) = &app.session else {
        return Ok(false);
    };
    let pos = app.snap.pos;
    Ok(match condition {
        "tracks_ready" => {
            s.tracks_discovered
                && !s
                    .tracks
                    .iter()
                    .any(|tr| tr.load == Load::Loading && tr.origin != Origin::Asr)
        }
        "line_shown" => {
            let needle = value.as_str().map(str::to_lowercase);
            s.primary_lines(pos).iter().any(|l| {
                needle
                    .as_ref()
                    .is_none_or(|n| l.text.to_lowercase().contains(n))
            })
        }
        "translation_ready" => {
            if s.secondary == Secondary::Off {
                return Err(
                    "no translation is shown: select_subtitles with secondary first".into(),
                );
            }
            let Some(tr) = s.primary_track() else {
                return Ok(false);
            };
            let mut cues = tr.track.active_at(pos);
            if cues.is_empty() {
                cues.extend(tr.track.index_after(pos));
            }
            !cues.is_empty() && cues.iter().all(|&c| s.translation_of(c).is_some())
        }
        "transcript_ready" => {
            let Some(tr) = s.tracks.iter().find(|tr| tr.origin == Origin::Asr) else {
                return Err("speech recognition has not been started: transcribe first".into());
            };
            match value.as_f64() {
                Some(until) => {
                    !s.asr_running || tr.track.cues.iter().any(|c| c.end >= to_ms(until))
                }
                None => !s.asr_running,
            }
        }
        "position" => pos >= to_ms(value.as_f64().unwrap_or(0.0)),
        "paused" => app.snap.paused,
        "playing" => !app.snap.paused && !app.snap.idle,
        _ => false,
    })
}

fn window_json(app: &App, ctx: &egui::Context) -> Value {
    let (inner, outer, ppp) = window_geometry(ctx);
    let size = inner.map_or(ctx.content_rect().size() * ppp, |r| r.size());
    let mut v = json!({
        "width": size.x.round(),
        "height": size.y.round(),
        "pixels_per_point": ppp,
        "fullscreen": app.fullscreen,
    });
    if let Some(r) = inner {
        v["x"] = json!(r.left().round());
        v["y"] = json!(r.top().round());
    }
    if let Some(r) = outer {
        v["outer"] = json!({
            "x": r.left().round(), "y": r.top().round(),
            "width": r.width().round(), "height": r.height().round(),
        });
    }
    v
}

fn set_window(app: &mut App, ctx: &egui::Context, args: &Value) -> Result<Outcome, String> {
    let width = opt_f64(args, "width")?;
    let height = opt_f64(args, "height")?;
    let x = opt_f64(args, "x")?;
    let y = opt_f64(args, "y")?;
    let on_top = opt_bool(args, "always_on_top")?;
    if width.is_some() != height.is_some() {
        return Err("give width and height together".into());
    }
    if x.is_some() != y.is_some() {
        return Err("give x and y together".into());
    }
    for v in [width, height].into_iter().flatten() {
        if !(320.0..=7680.0).contains(&v) {
            return Err(format!(
                "{v} is not a usable window size: 320 to 7680 pixels"
            ));
        }
    }
    if width.is_none() && x.is_none() && on_top.is_none() {
        return Err("give width and height, x and y, or always_on_top".into());
    }
    if app.fullscreen {
        app.toggle_fullscreen(ctx);
    }
    ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(false));
    let ppp = ctx.pixels_per_point();
    if let (Some(w), Some(h)) = (width, height) {
        // Windows keeps a window within the desktop unless the app names a larger maximum,
        // and a 1080x1920 window is taller than most screens.
        ctx.send_viewport_cmd(egui::ViewportCommand::MaxInnerSize(egui::vec2(
            16384.0, 16384.0,
        )));
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
            w as f32 / ppp,
            h as f32 / ppp,
        )));
    }
    if let (Some(x), Some(y)) = (x, y) {
        let (inner, outer, _) = window_geometry(ctx);
        let border = match (inner, outer) {
            (Some(i), Some(o)) => i.min - o.min,
            _ => egui::Vec2::ZERO,
        };
        let at = (egui::pos2(x as f32, y as f32) - border) / ppp;
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(at));
    }
    if let Some(top) = on_top {
        ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(if top {
            egui::WindowLevel::AlwaysOnTop
        } else {
            egui::WindowLevel::Normal
        }));
    }
    let started = Instant::now();
    let wait: Wait = Box::new(move |app, ctx, late| {
        let (inner, _, _) = window_geometry(ctx);
        let inner = inner?;
        let size_ok = width.is_none_or(|w| (inner.width() - w as f32).abs() <= 1.5)
            && height.is_none_or(|h| (inner.height() - h as f32).abs() <= 1.5);
        let pos_ok = x.is_none_or(|x| (inner.left() - x as f32).abs() <= 1.5)
            && y.is_none_or(|y| (inner.top() - y as f32).abs() <= 1.5);
        let settled = started.elapsed() > Duration::from_millis(150);
        let mut v = window_json(app, ctx);
        if let Some(top) = on_top {
            v["always_on_top"] = json!(top);
        }
        if size_ok && pos_ok && settled {
            return Some(Ok(v));
        }
        (late || started.elapsed() > Duration::from_secs(4)).then(|| {
            Err(format!(
                "the window did not take the asked geometry (the system may limit it); it is \
                 now {v}"
            ))
        })
    });
    later("set_window", args, wait)
}

fn press_key(app: &mut App, ctx: &egui::Context, args: &Value) -> Result<Outcome, String> {
    let key = need_str(args, "key")?.trim().to_ascii_lowercase();
    let did = match key.as_str() {
        "space" => {
            app.close_popup();
            let paused = app.player.mpv.get_flag("pause").unwrap_or(true);
            app.player.set_paused(!paused);
            if paused { "play" } else { "pause" }
        }
        "left" => {
            app.player.seek_relative(-5_000);
            "back 5 s"
        }
        "right" => {
            app.player.seek_relative(5_000);
            "forward 5 s"
        }
        "up" => {
            app.set_volume(app.volume + 5.0);
            "volume up"
        }
        "down" => {
            app.set_volume(app.volume - 5.0);
            "volume down"
        }
        "a" => {
            app.cue_nav(-1);
            "previous line"
        }
        "s" => {
            app.cue_nav(0);
            "repeat line"
        }
        "d" => {
            app.cue_nav(1);
            "next line"
        }
        "p" => {
            app.trainer.autopause = !app.trainer.autopause;
            app.cfg.training.autopause = app.trainer.autopause;
            app.toast(if app.trainer.autopause {
                t(Tr::AutoPauseOn)
            } else {
                t(Tr::AutoPauseOff)
            });
            "auto-pause"
        }
        "f" => {
            app.toggle_fullscreen(ctx);
            "fullscreen"
        }
        "t" => {
            app.sidebar_open = !app.sidebar_open;
            "sidebar"
        }
        "m" => {
            let muted = app.player.mpv.get_flag("mute").unwrap_or(false);
            app.player.mpv.set_flag("mute", !muted || app.quiet);
            "mute"
        }
        "1" => {
            app.cfg.subtitles.show_primary = !app.cfg.subtitles.show_primary;
            "original subtitles"
        }
        "2" => {
            app.cfg.subtitles.show_secondary = !app.cfg.subtitles.show_secondary;
            "translation line"
        }
        "[" | "]" => {
            let speed = app.player.mpv.get_f64("speed").unwrap_or(1.0);
            let speed = if key == "[" {
                (speed - 0.25).max(0.25)
            } else {
                (speed + 0.25).min(4.0)
            };
            app.player.mpv.set_f64("speed", speed);
            "speed"
        }
        "escape" => {
            if app.asr_setup.dialog_open {
                app.close_asr_setup();
            } else if app.tts_setup.dialog_open {
                app.close_tts_setup();
            } else if app.settings_draft.is_some() {
                app.settings_draft = None;
            } else if app.popup.is_some() {
                app.close_popup();
            } else if app.fullscreen {
                app.toggle_fullscreen(ctx);
            }
            "escape"
        }
        other => {
            return Err(format!(
                "unknown key \"{other}\": space, left, right, up, down, a, s, d, p, f, t, m, \
                 1, 2, [, ], escape"
            ));
        }
    };
    Ok(Outcome::Now(Ok(json!({"key": key, "action": did}))))
}

/// A path the agent asked to write: absolute, in an existing folder, with the extension the
/// format needs, and not over an existing file unless that is allowed.
fn output_path(raw: &str, ext: &str, overwrite: bool) -> Result<PathBuf, String> {
    if raw.trim().is_empty() {
        return Err("path is empty".into());
    }
    let path = std::path::absolute(raw.trim()).map_err(|e| format!("{raw}: {e}"))?;
    let has_ext = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(ext));
    if !has_ext {
        return Err(format!("{} must end in .{ext}", path.display()));
    }
    if !path.parent().is_some_and(Path::is_dir) {
        return Err(format!("the folder of {} does not exist", path.display()));
    }
    if path.is_dir() {
        return Err(format!("{} is a folder", path.display()));
    }
    if path.exists() && !overwrite {
        return Err(format!(
            "{} exists already; pass overwrite: true to replace it",
            path.display()
        ));
    }
    Ok(path)
}

fn screenshot(_app: &mut App, args: &Value) -> Result<Outcome, String> {
    let overwrite = opt_bool(args, "overwrite")?.unwrap_or(false);
    let path = output_path(need_str(args, "path")?, "png", overwrite)?;
    let capture: video::Capture = Arc::default();
    let mut asked = false;
    let wait: Wait = Box::new(move |_, ctx, late| {
        // Asked a frame after the call, so whatever the calls before it changed is drawn.
        if !asked {
            asked = true;
            let layer = egui::LayerId::new(egui::Order::Debug, egui::Id::unique("mcp-capture"));
            ctx.layer_painter(layer)
                .add(video::capture_callback(ctx.content_rect(), capture.clone()));
            return None;
        }
        let shot = capture.lock().unwrap().take();
        match shot {
            Some((w, h, pixels)) => Some(
                video::save_png(&path, w, h, &pixels)
                    .map(|()| json!({"path": path, "width": w, "height": h}))
                    .map_err(|e| format!("could not write {}: {e}", path.display())),
            ),
            None => late.then(|| Err("the window was not drawn in time".into())),
        }
    });
    later("screenshot", args, wait)
}

fn status(app: &App, ctx: &egui::Context) -> Value {
    let snap = app.player.snapshot();
    let mpv = &app.player.mpv;
    let file = app.session.as_ref().map(|s| {
        json!({
            "media": s.media,
            "title": s.title,
            "online": s.is_url(),
            "tracks_ready": s.tracks_discovered,
        })
    });
    json!({
        "file": file,
        "position": secs(snap.pos),
        "duration": secs(snap.duration),
        "paused": snap.paused,
        "idle": snap.idle,
        "speed": snap.speed,
        "volume": app.volume,
        "muted": snap.muted,
        "quiet": app.quiet,
        "video": {
            "width": mpv.get_i64("width"),
            "height": mpv.get_i64("height"),
            "codec": mpv.get_string("video-codec"),
            "decoder": mpv.get_string("hwdec-current"),
        },
        "ui_lang": i18n::lang().code(),
        "learn_lang": app.cfg.general.learn_lang,
        "native_lang": app.cfg.general.native_lang,
        "window": window_json(app, ctx),
        "controls": controls_override().map_or(json!("auto"), Value::Bool),
        "sidebar": {"open": app.sidebar_open, "tab": tab_name(app.sidebar_tab), "search": app.search},
        "word_card": app.popup.as_ref().map(|p| p.word.clone()),
        "dialogs": {
            "settings": app.settings_draft.is_some(),
            "speech_recognition_setup": app.asr_setup.dialog_open,
            "voice_over_setup": app.tts_setup.dialog_open,
        },
        "auto_pause": app.trainer.autopause,
        "voice_over": dub_json(app),
        "speech_recognition": asr_json(app),
        "message": app.last_toast,
    })
}

// ----- tool descriptions -----

fn tool(name: &str, title: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "title": title,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false,
        },
    })
}

fn read_only(mut v: Value) -> Value {
    v["annotations"] = json!({"readOnlyHint": true, "openWorldHint": false});
    v
}

/// Every tool, in a fixed order, with its JSON Schema.
pub fn tool_list() -> Value {
    let none = json!({});
    let track = json!({"anyOf": [{"type": "integer", "minimum": 0}, {"type": "string"}]});
    let mut primary = track.clone();
    primary["description"] = json!(
        "The subtitles to learn from: a track index from list_tracks, a language code \
         (en, ru…) or \"none\"."
    );
    let mut secondary = track;
    secondary["description"] = json!(
        "The line under it: a track index, a language code, \"translate\" for machine \
         translation into the native language, or \"off\"."
    );
    json!([
        tool(
            "open",
            "Open a file or link",
            "Opens a local video file, a subtitle file (added to the open video) or a link \
             (YouTube and other sites yt-dlp knows). Answers once the file plays and its tracks \
             are known, with the track list.",
            json!({
                "path_or_url": {"type": "string", "description": "A file path or an http(s) link."},
                "timeout_seconds": {"type": "number", "exclusiveMinimum": 0, "description": "How long to wait for it to open (default 90)."},
            }),
            &["path_or_url"]
        ),
        tool(
            "play",
            "Play",
            "Starts playback (closing an open word card first, as the play button does).",
            none.clone(),
            &[]
        ),
        tool("pause", "Pause", "Pauses playback.", none.clone(), &[]),
        tool(
            "seek",
            "Seek",
            "Jumps to a position and answers once the new frame is shown.",
            json!({
                "seconds": {"type": "number", "minimum": 0, "description": "The position from the start."},
                "exact": {"type": "boolean", "description": "Land on the exact frame (default) or on the nearest keyframe, which is faster."},
            }),
            &["seconds"]
        ),
        tool(
            "set_speed",
            "Playback speed",
            "Sets the playback speed.",
            json!({"speed": {"type": "number", "minimum": 0.25, "maximum": 4}}),
            &["speed"]
        ),
        tool(
            "set_volume",
            "Volume",
            "Sets the player's volume in percent (100 is unchanged, up to 150). With --quiet the \
             player stays muted whatever this says.",
            json!({
                "percent": {"type": "number", "minimum": 0, "maximum": 150},
                "muted": {"type": "boolean"},
            }),
            &["percent"]
        ),
        read_only(tool(
            "status",
            "Player status",
            "Position, duration, pause, speed, volume, video size and decoder, title, interface \
             and study languages, window geometry in pixels (for a screen recorder), what is \
             open on screen, voice-over and speech recognition state, and the last message the \
             player showed.",
            none.clone(),
            &[]
        )),
        read_only(tool(
            "list_tracks",
            "Subtitle tracks",
            "The subtitle tracks of the open file with language, origin, load state and number \
             of lines, which are primary and secondary, audio tracks, and the translation provider.",
            none.clone(),
            &[]
        )),
        tool(
            "select_subtitles",
            "Choose subtitles",
            "Chooses the primary subtitles (the ones to learn from, with clickable words) and \
             the line shown under them. Answers once the chosen tracks are read.",
            json!({"primary": primary, "secondary": secondary}),
            &[]
        ),
        tool(
            "set_languages",
            "Study languages",
            "Sets the language being learned and the native language translations go into, \
             applied at once: existing machine translations are made again in the new language.",
            json!({
                "learn": {"type": "string", "description": "Language code, e.g. en; empty for any."},
                "native": {"type": "string", "description": "Language code, e.g. ru."},
            }),
            &[]
        ),
        read_only(tool(
            "current_lines",
            "Lines on screen",
            "The subtitle lines shown at the playhead with timings, their translation, the line \
             under them, and each word with its index (for click_word) and its rectangle in \
             window pixels.",
            none.clone(),
            &[]
        )),
        read_only(tool(
            "transcript",
            "Transcript",
            "Lines of the primary subtitles with their translation where there is one.",
            json!({
                "from": {"type": "number", "description": "Seconds; lines ending before are left out."},
                "to": {"type": "number", "description": "Seconds; lines starting after are left out."},
                "limit": {"type": "integer", "minimum": 1, "maximum": 2000, "description": "At most this many lines (default 100)."},
            }),
            &[]
        )),
        tool(
            "transcribe",
            "Speech recognition",
            "Starts or stops speech recognition of the open local file, which becomes the primary \
             subtitles as it grows; without start, only reports its state.",
            json!({"start": {"type": "boolean"}}),
            &[]
        ),
        tool(
            "voice_over",
            "Voice-over",
            "Turns the spoken translation on or off and sets its volumes (percent).",
            json!({
                "enabled": {"type": "boolean"},
                "original_volume": {"type": "number", "minimum": 0, "maximum": 150, "description": "The film's sound while the voice-over is on, in percent of the player volume."},
                "voice_volume": {"type": "number", "minimum": 0, "maximum": 150},
            }),
            &[]
        ),
        tool(
            "wait_for",
            "Wait for an event",
            "Blocks until a condition holds, then answers; an error when the time runs out. \
             tracks_ready: the file is open and its subtitles are read. translation_ready: the \
             line at the playhead (or the next one) has its translation. transcript_ready: \
             speech recognition has finished, or reached value seconds. line_shown: a subtitle \
             line is on screen (value: text it must contain). position: the playhead reached \
             value seconds. paused, playing.",
            json!({
                "condition": {"type": "string", "enum": ["tracks_ready", "translation_ready", "transcript_ready", "line_shown", "position", "paused", "playing"]},
                "value": {"anyOf": [{"type": "number"}, {"type": "string"}]},
                "timeout_seconds": {"type": "number", "exclusiveMinimum": 0, "maximum": 3600, "description": "Default 30."},
            }),
            &["condition"]
        ),
        tool(
            "click_word",
            "Click a word",
            "Clicks a word of the subtitle line on screen, as the user does: playback pauses and \
             the word card opens. Answers when its translation has come, with the word, the \
             translation and the word's rectangle in window and screen pixels.",
            json!({
                "index": {"type": "integer", "minimum": 0, "description": "The word's index from current_lines, counting from 0."},
                "line": {"type": "integer", "minimum": 0, "description": "Which line when several are shown (default 0)."},
            }),
            &["index"]
        ),
        tool(
            "close_word_card",
            "Close the word card",
            "Closes the word card; playback resumes if it was playing when the word was clicked.",
            none.clone(),
            &[]
        ),
        tool(
            "save_word",
            "Save the word",
            "Saves the word on the open card to the word list.",
            none.clone(),
            &[]
        ),
        tool(
            "toggle_sidebar",
            "Sidebar",
            "Opens or closes the sidebar with the transcript or the saved words.",
            json!({
                "open": {"type": "boolean", "description": "Omit to toggle."},
                "tab": {"type": "string", "enum": ["transcript", "words"]},
            }),
            &[]
        ),
        tool(
            "search_transcript",
            "Search the transcript",
            "Types into the sidebar's search box (opening the transcript) and returns the lines \
             found; an empty text clears the search.",
            json!({"text": {"type": "string"}}),
            &["text"]
        ),
        tool(
            "show_controls",
            "Control bar",
            "true or false fixes the control bar and the title strip on or off (off leaves only \
             the video and subtitles, with no messages over them); \"auto\" gives the usual \
             behaviour (shown on mouse movement and while paused).",
            json!({"visible": {"anyOf": [{"type": "boolean"}, {"type": "string", "enum": ["auto"]}]}}),
            &["visible"]
        ),
        tool(
            "open_settings",
            "Open settings",
            "Opens the Settings window (scrolled to a section) or a setup dialog.",
            json!({"section": {"type": "string", "enum": ["top", "speech_recognition", "voice_over", "speech_recognition_setup", "voice_over_setup"]}}),
            &[]
        ),
        tool(
            "close_dialogs",
            "Close dialogs",
            "Closes the Settings window and the setup dialogs without saving.",
            none.clone(),
            &[]
        ),
        tool(
            "set_ui_language",
            "Interface language",
            "Switches the interface language (saved like the Settings window does, except with --quiet).",
            json!({"lang": {"type": "string", "enum": ["auto", "en", "ru", "uk"]}}),
            &["lang"]
        ),
        tool(
            "set_window",
            "Window size and place",
            "Sets the size of the window's drawing area in physical pixels (1920x1080, 1080x1920…), \
             its top-left corner on the screen in pixels, and whether it stays on top. Answers \
             with the geometry it got.",
            json!({
                "width": {"type": "number", "minimum": 320, "maximum": 7680},
                "height": {"type": "number", "minimum": 320, "maximum": 7680},
                "x": {"type": "number", "description": "Screen position of the drawing area's left edge."},
                "y": {"type": "number", "description": "Screen position of the drawing area's top edge."},
                "always_on_top": {"type": "boolean"},
            }),
            &[]
        ),
        tool(
            "press_key",
            "Press a key",
            "Does what a player shortcut does: space (play/pause), left/right (5 s), up/down \
             (volume), a/s/d (previous, repeat, next line), p (auto-pause), f (fullscreen), \
             t (sidebar), m (mute), 1/2 (show the original or the translation line), [ and ] \
             (speed), escape.",
            json!({"key": {"type": "string"}}),
            &["key"]
        ),
        tool(
            "screenshot",
            "Screenshot",
            "Saves the window as it is drawn, with subtitles and anything open over the video, \
             to a PNG file.",
            json!({
                "path": {"type": "string", "description": "Where to write the .png; its folder must exist."},
                "overwrite": {"type": "boolean", "description": "Replace an existing file (default false)."},
            }),
            &["path"]
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every name `run_tool` handles; kept here so a tool cannot be listed but not run.
    const RUN: [&str; 27] = [
        "open",
        "play",
        "pause",
        "seek",
        "set_speed",
        "set_volume",
        "status",
        "list_tracks",
        "select_subtitles",
        "set_languages",
        "current_lines",
        "transcript",
        "transcribe",
        "voice_over",
        "wait_for",
        "click_word",
        "close_word_card",
        "save_word",
        "toggle_sidebar",
        "search_transcript",
        "show_controls",
        "open_settings",
        "close_dialogs",
        "set_ui_language",
        "set_window",
        "press_key",
        "screenshot",
    ];

    fn check_schema(path: &str, s: &Value) {
        let obj = s
            .as_object()
            .unwrap_or_else(|| panic!("{path}: not an object"));
        if let Some(any) = obj.get("anyOf") {
            for (i, sub) in any.as_array().expect("anyOf is a list").iter().enumerate() {
                check_schema(&format!("{path}/anyOf/{i}"), sub);
            }
            return;
        }
        let ty = obj["type"]
            .as_str()
            .unwrap_or_else(|| panic!("{path}: no type"));
        assert!(
            ["object", "string", "number", "integer", "boolean", "array"].contains(&ty),
            "{path}: type {ty}"
        );
        for key in obj.keys() {
            assert!(
                [
                    "type",
                    "description",
                    "properties",
                    "required",
                    "additionalProperties",
                    "enum",
                    "minimum",
                    "maximum",
                    "exclusiveMinimum",
                    "items"
                ]
                .contains(&key.as_str()),
                "{path}: unexpected keyword {key}"
            );
        }
        if let Some(e) = obj.get("enum") {
            let e = e.as_array().expect("enum is a list");
            assert!(
                !e.is_empty() && e.iter().all(|v| v.as_str().is_some()),
                "{path}"
            );
        }
        if ty == "object" {
            let props = obj["properties"].as_object().expect("properties");
            for (k, v) in props {
                check_schema(&format!("{path}/{k}"), v);
            }
            for r in obj["required"].as_array().expect("required") {
                assert!(
                    props.contains_key(r.as_str().unwrap()),
                    "{path}: required {r}"
                );
            }
        }
    }

    #[test]
    fn every_tool_is_described_with_a_valid_schema() {
        let tools = tool_list();
        let tools = tools.as_array().unwrap();
        let mut names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        for t in tools {
            let name = t["name"].as_str().unwrap();
            assert!(
                (1..=64).contains(&name.len())
                    && name.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "{name}"
            );
            assert!(t["description"].as_str().unwrap().len() > 15, "{name}");
            assert!(t["title"].is_string(), "{name}");
            assert_eq!(t["inputSchema"]["additionalProperties"], false, "{name}");
            check_schema(name, &t["inputSchema"]);
        }
        let mut run: Vec<&str> = RUN.to_vec();
        run.sort_unstable();
        run.dedup();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "names are unique");
        assert_eq!(names, run);
    }

    #[test]
    fn the_tool_list_goes_through_the_protocol() {
        let mut p = Protocol::new(tool_list());
        let actions = p.handle(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#);
        let Some(Action::Send(r)) = actions.into_iter().next() else {
            panic!("no answer");
        };
        assert_eq!(r["result"]["tools"].as_array().unwrap().len(), 27);
        let actions = p.handle(
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"click_word","arguments":{"index":3}}}"#,
        );
        assert!(matches!(&actions[0], Action::Call(c) if c.name == "click_word"));
    }

    #[test]
    fn arguments_are_checked_with_readable_errors() {
        let args = json!({"seconds": "ten", "exact": 1, "index": -2, "speed": 1.5});
        assert_eq!(
            need_f64(&args, "seconds").unwrap_err(),
            "seconds must be a number"
        );
        assert_eq!(need_f64(&args, "speed").unwrap(), 1.5);
        assert_eq!(
            need_f64(&args, "missing").unwrap_err(),
            "missing is required"
        );
        assert_eq!(
            opt_bool(&args, "exact").unwrap_err(),
            "exact must be true or false"
        );
        assert!(opt_index(&args, "index").is_err());
        assert_eq!(opt_index(&json!({"index": 3}), "index").unwrap(), Some(3));
    }

    #[test]
    fn time_limits_follow_the_call() {
        let wait = |s: f64| time_limit("wait_for", &json!({"timeout_seconds": s}));
        assert_eq!(wait(5.0), Duration::from_secs(5));
        assert_eq!(wait(1e9), Duration::from_secs(3600));
        assert_eq!(time_limit("wait_for", &json!({})), Duration::from_secs(30));
        assert_eq!(time_limit("open", &json!({})), Duration::from_secs(90));
        assert_eq!(time_limit("play", &json!({})), Duration::from_secs(10));
    }

    #[test]
    fn output_paths_are_checked_before_anything_is_written() {
        let dir = gp_core::scratch::TempDir::new("gp-mcp-out");
        let ok = dir.join("shot.png");
        assert_eq!(output_path(ok.to_str().unwrap(), "png", false).unwrap(), ok);
        std::fs::write(&ok, b"x").unwrap();
        let e = output_path(ok.to_str().unwrap(), "png", false).unwrap_err();
        assert!(e.contains("overwrite: true"), "{e}");
        assert!(output_path(ok.to_str().unwrap(), "png", true).is_ok());
        let e = output_path(dir.join("a.jpg").to_str().unwrap(), "png", false).unwrap_err();
        assert!(e.ends_with("must end in .png"), "{e}");
        let e = output_path(dir.join("no/a.png").to_str().unwrap(), "png", false).unwrap_err();
        assert!(e.contains("does not exist"), "{e}");
        assert!(output_path(" ", "png", false).is_err());
    }
}
