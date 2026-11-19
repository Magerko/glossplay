//! `--selftest` drives the real player without a human: it opens a file, plays it for a few
//! seconds, prints what happened as `key=value` lines and sets the process exit code.

use crate::app::App;
use crate::session::{Load, Origin, Secondary};
use crate::video;
use eframe::egui;
use lp_core::{Ms, TokenKind};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

pub static EXIT_CODE: AtomicI32 = AtomicI32::new(0);

#[derive(Default, Clone)]
pub struct Plan {
    pub seconds: f64,
    pub at: Option<f64>,
    pub subs: Vec<String>,
    pub screenshot: Option<PathBuf>,
    pub show_controls: bool,
    pub sidebar: bool,
    pub word: Option<usize>,
    pub autopause: bool,
    pub asr: bool,
    pub paused: bool,
    pub anki: bool,
    pub strict: bool,
    pub export_srt: Option<PathBuf>,
}

enum Phase {
    Loading,
    Tracks,
    Play,
    Autopause {
        cue_start: Ms,
        cue_end: Ms,
        paused_at: Option<(Ms, Instant)>,
        replayed: bool,
    },
    Capture,
    Done,
}

pub struct State {
    pub plan: Plan,
    phase: Phase,
    launched: Instant,
    started: Instant,
    start_pos: Ms,
    seen_primary: Option<String>,
    seen_secondary: Option<String>,
    word_opened: bool,
    subs_applied: bool,
    anki_sent: bool,
    srt_exported: bool,
    drops_at_start: Option<i64>,
    asr_first_cue: Option<Duration>,
    asr_minute_ready: Option<Duration>,
    asr_done: Option<Duration>,
    capture: video::Capture,
    failures: Vec<String>,
}

impl State {
    pub fn new(plan: Plan) -> State {
        State {
            plan,
            phase: Phase::Loading,
            launched: Instant::now(),
            started: Instant::now(),
            start_pos: 0,
            seen_primary: None,
            seen_secondary: None,
            word_opened: false,
            subs_applied: false,
            anki_sent: false,
            srt_exported: false,
            drops_at_start: None,
            asr_first_cue: None,
            asr_minute_ready: None,
            asr_done: None,
            capture: Arc::default(),
            failures: Vec::new(),
        }
    }
}

pub fn step(app: &mut App, ctx: &egui::Context) {
    let Some(mut st) = app.selftest.take() else {
        return;
    };
    ctx.request_repaint();
    advance(&mut st, app, ctx);
    app.selftest = Some(st);
}

fn finish(st: &mut State, ctx: &egui::Context) {
    for f in &st.failures {
        println!("FAIL={f}");
    }
    println!(
        "result={}",
        if st.failures.is_empty() {
            "ok"
        } else {
            "failed"
        }
    );
    EXIT_CODE.store(if st.failures.is_empty() { 0 } else { 1 }, Ordering::SeqCst);
    st.phase = Phase::Done;
    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
}

fn advance(st: &mut State, app: &mut App, ctx: &egui::Context) {
    match &mut st.phase {
        Phase::Loading => {
            let ready = app.session.as_ref().is_some_and(|s| s.tracks_discovered);
            if ready {
                if let Some(at) = st.plan.at {
                    app.player.seek((at * 1000.0) as Ms);
                }
                app.sidebar_open = st.plan.sidebar;
                if st.plan.asr {
                    app.snap = app.player.snapshot();
                    app.start_asr();
                }
                st.started = Instant::now();
                st.phase = Phase::Tracks;
            } else if st.launched.elapsed() > Duration::from_secs(20) {
                st.failures.push("the file did not load within 20 s".into());
                finish(st, ctx);
            }
        }
        Phase::Tracks => {
            if !st.subs_applied {
                st.subs_applied = true;
                let s = app.session.as_ref().unwrap();
                let find = |lang: &str| {
                    s.tracks
                        .iter()
                        .position(|t| t.track.lang.as_deref() == Some(lang) && t.is_text())
                };
                let primary = st.plan.subs.first().and_then(|l| find(l));
                let secondary = st.plan.subs.get(1).and_then(|l| find(l));
                if let Some(p) = primary {
                    app.select_primary(Some(p));
                }
                if let Some(sec) = secondary {
                    app.set_secondary(Secondary::Track(sec));
                } else if st.plan.subs.get(1).is_some_and(|l| l == "mt") {
                    app.set_secondary(Secondary::Translate);
                }
            }
            let s = app.session.as_ref().unwrap();
            let loading = s
                .tracks
                .iter()
                .any(|t| t.load == Load::Loading && t.origin != Origin::Asr);
            if loading && st.started.elapsed() < Duration::from_secs(60) {
                return;
            }
            if !st.plan.asr {
                export_srt(st, app);
            }
            st.started = Instant::now();
            st.start_pos = app.snap.pos;
            app.player.set_paused(st.plan.paused);
            st.phase = Phase::Play;
            if st.plan.autopause {
                let cue = app
                    .session
                    .as_ref()
                    .and_then(|s| s.primary_track())
                    .and_then(|t| {
                        let i = t.track.index_after(app.snap.pos)?;
                        Some((t.track.cues[i].start, t.track.cues[i].end))
                    });
                match cue {
                    Some((cue_start, cue_end)) => {
                        app.trainer.autopause = true;
                        app.player.seek(cue_end - 1_200);
                        st.phase = Phase::Autopause {
                            cue_start,
                            cue_end,
                            paused_at: None,
                            replayed: false,
                        };
                    }
                    None => {
                        st.failures
                            .push("no subtitle line to test auto-pause on".into());
                        finish(st, ctx);
                    }
                }
            }
        }
        Phase::Autopause {
            cue_start,
            cue_end,
            paused_at,
            replayed,
        } => {
            let (cue_start, cue_end) = (*cue_start, *cue_end);
            let waited = st.started.elapsed();
            match *paused_at {
                None if app.snap.paused && waited > Duration::from_millis(600) => {
                    let delta = app.snap.pos - cue_end;
                    println!("autopause_delta_ms={delta}");
                    if !(0..=150).contains(&delta) {
                        st.failures.push(format!(
                            "auto-pause landed {delta} ms from the end of the line"
                        ));
                    }
                    *paused_at = Some((app.snap.pos, Instant::now()));
                }
                None if waited > Duration::from_secs(8) => {
                    st.failures.push("auto-pause never fired".into());
                    finish(st, ctx);
                }
                Some((_, at)) if !*replayed && at.elapsed() > Duration::from_millis(300) => {
                    app.trainer.autopause = false;
                    app.cue_nav(0);
                    *replayed = true;
                    *paused_at = Some((app.snap.pos, Instant::now()));
                }
                Some((_, at)) if *replayed && at.elapsed() > Duration::from_millis(250) => {
                    // The replay starts playing at once, so the position is a little past the start.
                    let delta = app.snap.pos - cue_start;
                    println!("replay_delta_ms={delta}");
                    if !(0..=600).contains(&delta) {
                        st.failures.push(format!(
                            "repeat landed {delta} ms from the start of the line"
                        ));
                    }
                    finish(st, ctx);
                }
                _ => {}
            }
        }
        Phase::Play => {
            let elapsed = st.started.elapsed();
            // The first moments of playback drop frames while the video output warms up; only
            // what is dropped after that is judged.
            if st.drops_at_start.is_none() && elapsed > Duration::from_secs(1) {
                st.drops_at_start = app.player.mpv.get_i64("frame-drop-count");
            }
            let pos = app.snap.pos;
            if let Some(s) = &app.session {
                if st.seen_primary.is_none() {
                    st.seen_primary = s
                        .primary_lines(pos)
                        .first()
                        .map(|l| format!("{}-{} {}", l.start, l.end, l.text));
                }
                if st.seen_secondary.is_none() {
                    st.seen_secondary = s.secondary_lines(pos).first().cloned();
                }
                if let Some(t) = s.tracks.iter().find(|t| t.origin == Origin::Asr) {
                    let since = st.launched.elapsed();
                    if st.asr_first_cue.is_none() && !t.track.cues.is_empty() {
                        st.asr_first_cue = Some(since);
                    }
                    if st.asr_minute_ready.is_none() && t.track.cues.iter().any(|c| c.end >= 60_000)
                    {
                        st.asr_minute_ready = Some(since);
                    }
                    if st.asr_done.is_none() && !s.asr_running && !t.track.cues.is_empty() {
                        st.asr_done = Some(since);
                    }
                }
            }
            if let (Some(n), false) = (st.plan.word, st.word_opened)
                && elapsed.as_secs_f64() > st.plan.seconds * 0.5
            {
                st.word_opened = open_nth_word(app, n, ctx);
            }
            if st.plan.anki && st.word_opened && !st.anki_sent {
                let translated = app
                    .popup
                    .as_ref()
                    .is_some_and(|p| matches!(p.translation, crate::app::Async::Ready(_)));
                if translated {
                    if let Some(id) = app.save_popup_word() {
                        app.export_to_anki(id);
                    }
                    st.anki_sent = true;
                }
            }
            if elapsed.as_secs_f64() >= st.plan.seconds {
                report(st, app);
                if st.plan.screenshot.is_some() {
                    let layer = egui::LayerId::new(egui::Order::Debug, egui::Id::unique("capture"));
                    ctx.layer_painter(layer).add(video::capture_callback(
                        ctx.content_rect(),
                        st.capture.clone(),
                    ));
                    st.phase = Phase::Capture;
                } else {
                    finish(st, ctx);
                }
            }
        }
        Phase::Capture => {
            let shot = st.capture.lock().unwrap().take();
            if let (Some((w, h, pixels)), Some(path)) = (shot, &st.plan.screenshot) {
                match video::save_png(path, w, h, &pixels) {
                    Ok(()) => println!("screenshot={}", path.display()),
                    Err(e) => st
                        .failures
                        .push(format!("could not save the screenshot: {e}")),
                }
                finish(st, ctx);
            }
        }
        Phase::Done => {}
    }
}

fn open_nth_word(app: &mut App, n: usize, ctx: &egui::Context) -> bool {
    let Some(s) = &app.session else { return false };
    let Some(line) = s.primary_lines(app.snap.pos).into_iter().next() else {
        return false;
    };
    let lang = s.primary_track().and_then(|t| t.track.lang.clone());
    let track = s.primary.unwrap_or(0);
    let tokens = app.tokens_for(track, line.cue, &line.text, lang.as_deref());
    let words: Vec<(usize, &str)> = tokens
        .iter()
        .enumerate()
        .filter(|(_, t)| t.kind == TokenKind::Word)
        .map(|(i, t)| (i, &line.text[t.range.clone()]))
        .collect();
    let Some(&(token, word)) = words.get(n.min(words.len().saturating_sub(1))) else {
        return false;
    };
    let screen = ctx.content_rect();
    app.open_word(
        word.to_owned(),
        line.cue,
        token,
        egui::pos2(screen.center().x + 180.0, screen.bottom() - 190.0),
    );
    true
}

fn report(st: &mut State, app: &App) {
    let mpv = &app.player.mpv;
    let int = |name| mpv.get_i64(name).unwrap_or(0);
    let (width, height) = (int("width"), int("height"));
    let advanced = app.snap.pos - st.start_pos;
    println!("duration_ms={}", app.snap.duration);
    println!("video_size={width}x{height}");
    println!(
        "hwdec_current={}",
        mpv.get_string("hwdec-current").unwrap_or_default()
    );
    println!(
        "video_codec={}",
        mpv.get_string("video-codec").unwrap_or_default()
    );
    println!(
        "audio_codec={}",
        mpv.get_string("audio-codec-name").unwrap_or_default()
    );
    println!("pos_start_ms={}", st.start_pos);
    println!("pos_end_ms={}", app.snap.pos);
    println!("advanced_ms={advanced}");
    println!("frame_drop_count={}", int("frame-drop-count"));
    let dropped = int("frame-drop-count");
    println!(
        "decoder_frame_drop_count={}",
        int("decoder-frame-drop-count")
    );
    let fps = mpv.get_f64("estimated-vf-fps").unwrap_or(0.0);
    println!("fps={fps:.2}");
    let has_video = app
        .player
        .tracks()
        .iter()
        .any(|t| t.kind == crate::player::TrackKind::Video);
    if has_video && (width == 0 || height == 0) {
        st.failures.push("the video has no frame size".into());
    }
    if st.plan.strict && has_video {
        let hwdec = mpv.get_string("hwdec-current").unwrap_or_default();
        if hwdec.is_empty() || hwdec == "no" {
            st.failures
                .push("the video is decoded in software (hwdec-current is empty or no)".into());
        }
        let fps = if fps > 0.0 {
            fps
        } else {
            mpv.get_f64("container-fps").unwrap_or(0.0)
        };
        let allowed = (fps * st.plan.seconds * 0.01).max(3.0);
        let late_drops = dropped - st.drops_at_start.unwrap_or(dropped);
        if late_drops as f64 > allowed {
            st.failures.push(format!(
                "{late_drops} frames dropped while playing, more than the {allowed:.0} allowed"
            ));
        }
    }
    if !st.plan.paused && advanced < 1_000 {
        st.failures
            .push(format!("playback advanced only {advanced} ms"));
    }
    report_vocab(st, app);
    println!("toast={}", app.last_toast);
    let Some(s) = &app.session else { return };
    for (i, t) in s.tracks.iter().enumerate() {
        println!(
            "track{i}={} | lang={} | {:?} | cues={}",
            t.label,
            t.track.lang.as_deref().unwrap_or("-"),
            t.load,
            t.track.cues.len()
        );
    }
    println!(
        "primary_line={}",
        st.seen_primary
            .clone()
            .unwrap_or_default()
            .replace('\n', " / ")
    );
    println!(
        "secondary_line={}",
        st.seen_secondary
            .clone()
            .unwrap_or_default()
            .replace('\n', " / ")
    );
    report_pairs(st, app);
    // Speech recognition has no lines yet when playback starts; they are exported at the end.
    export_srt(st, app);
    export_translation(st, app);
    if !st.plan.subs.is_empty() {
        if st.seen_primary.is_none() {
            st.failures
                .push("no primary subtitle line was shown".into());
        }
        if st.plan.subs.len() > 1 && st.seen_secondary.is_none() {
            st.failures
                .push("no secondary subtitle line was shown".into());
        }
    }
    if st.plan.asr {
        let ms = |d: Option<Duration>| d.map_or("-".to_owned(), |d| d.as_millis().to_string());
        let cues = s
            .tracks
            .iter()
            .find(|t| t.origin == Origin::Asr)
            .map_or(0, |t| t.track.cues.len());
        println!("asr_cues={cues}");
        println!("asr_status={}", s.asr_status);
        println!("asr_first_cue_wall_ms={}", ms(st.asr_first_cue));
        println!("asr_minute_ready_wall_ms={}", ms(st.asr_minute_ready));
        println!("asr_done_wall_ms={}", ms(st.asr_done));
        if let Some(done) = st.asr_done {
            println!(
                "asr_rtf={:.4}",
                done.as_secs_f64() / (app.snap.duration.max(1) as f64 / 1000.0)
            );
        }
        if cues == 0 {
            st.failures
                .push("speech recognition produced no lines".into());
        }
    }
}

/// The first translated lines from where playback started, so a run shows whether the
/// translation of neighbouring lines reads as one text. `sentence=` lines show the groups.
fn report_pairs(st: &State, app: &App) {
    let Some(s) = &app.session else { return };
    let Some(t) = s.primary_track() else { return };
    let from = t.track.index_at_or_before(st.start_pos).unwrap_or(0);
    let pairs: Vec<(usize, String)> = (from..t.track.cues.len())
        .filter_map(|i| Some((i, s.translation_of(i)?)))
        .take(12)
        .collect();
    let mut last_group = None;
    for (i, tr) in pairs {
        let g = t.group_of(i);
        if s.secondary == Secondary::Translate && last_group != Some(g) {
            println!("sentence={}", t.sentence_text(g));
            last_group = Some(g);
        }
        println!(
            "pair={} => {}",
            t.track.cues[i].text.replace('\n', " / "),
            tr.replace('\n', " / ")
        );
    }
}

fn export_srt(st: &mut State, app: &App) {
    let Some(path) = st.plan.export_srt.clone().filter(|_| !st.srt_exported) else {
        return;
    };
    st.srt_exported = true;
    let Some(track) = app.session.as_ref().and_then(|s| s.primary_track()) else {
        st.failures
            .push("--export-srt: there is no primary subtitle track".into());
        return;
    };
    let srt = lp_core::write_srt(&track.track.cues);
    let back = lp_core::parse_subtitles(srt.as_bytes(), Some(lp_core::SubFormat::Srt));
    if !back
        .as_ref()
        .is_ok_and(|b| b.len() == track.track.cues.len())
    {
        st.failures
            .push("the exported SRT does not parse back to the same lines".into());
        return;
    }
    match std::fs::write(&path, srt) {
        Ok(()) => println!(
            "exported_srt={} cues={}",
            path.display(),
            track.track.cues.len()
        ),
        Err(e) => st
            .failures
            .push(format!("could not write {}: {e}", path.display())),
    }
}

/// The translation is only there after playing for a while, so it is written at the end.
fn export_translation(st: &mut State, app: &App) {
    let Some(path) = st.plan.export_srt.clone() else {
        return;
    };
    let translated = app
        .session
        .as_ref()
        .map_or(Vec::new(), |s| s.translation_cues());
    if !translated.is_empty() {
        let tr_path = path.with_extension("tr.srt");
        match std::fs::write(&tr_path, lp_core::write_srt(&translated)) {
            Ok(()) => println!(
                "exported_translation_srt={} cues={}",
                tr_path.display(),
                translated.len()
            ),
            Err(e) => st
                .failures
                .push(format!("could not write {}: {e}", tr_path.display())),
        }
    }
}

fn report_vocab(st: &mut State, app: &App) {
    for v in &app.vocab {
        println!(
            "word={} | {} | {} | anki={} | media_path={}",
            v.word.word,
            v.word.translation,
            v.word.sentence,
            v.anki_note_id.map_or("-".to_owned(), |id| id.to_string()),
            v.word.media_path
        );
    }
    if st.plan.anki && !app.vocab.iter().any(|v| v.anki_note_id.is_some()) {
        st.failures.push("the word did not reach Anki".into());
    }
}
