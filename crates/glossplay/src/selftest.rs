//! `--selftest` drives the real player without a human: it opens a file, plays it for a few
//! seconds, prints what happened as `key=value` lines and sets the process exit code.

use crate::app::App;
use crate::session::{Load, Origin, Secondary};
use crate::video;
use eframe::egui;
use gp_core::{Ms, TokenKind};
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
    pub setup_dialog: bool,
    pub settings: bool,
    pub setup_install: bool,
    /// Turn the voice-over on when playback starts; pause once in the middle, and turn it
    /// off again at the end.
    pub dub: bool,
    /// A copy of every voice-over clip played goes here.
    pub dub_save: Option<PathBuf>,
    pub tts_dialog: bool,
    pub settings_voice: bool,
    /// Keep the Subtitles menu open for the screenshot.
    pub subs_menu: bool,
}

/// The pause in the middle of a voice-over run: the clip must stop with the video.
enum PauseCheck {
    Waiting,
    Paused {
        since: Instant,
        /// The voice player's position through the pause: (ms since pausing, seconds).
        samples: Vec<(u128, f64)>,
    },
    Done,
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
    /// The voice-over is off; the original sound must come back to its volume.
    DubOff {
        since: Instant,
    },
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
    pause_check: PauseCheck,
    dub_started: Option<Instant>,
    dub_ready_at_start: Option<usize>,
    /// The voice player's audio output and mute, read while a clip plays.
    voice_output: Option<(String, bool)>,
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
            pause_check: PauseCheck::Waiting,
            dub_started: None,
            dub_ready_at_start: None,
            voice_output: None,
        }
    }
}

pub fn step(app: &mut App, ctx: &egui::Context) {
    let Some(mut st) = app.selftest.take() else {
        return;
    };
    ctx.request_repaint();
    if st.plan.settings {
        // The section is below the fold of the settings window; it has to be in the screenshot.
        app.asr_setup.scroll_to = true;
    }
    if st.plan.settings_voice {
        app.tts_setup.scroll_to = true;
    }
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
                if st.plan.setup_dialog {
                    app.open_asr_setup(false);
                }
                if st.plan.settings {
                    app.settings_draft = Some(app.cfg.clone());
                }
                if st.plan.tts_dialog {
                    app.open_tts_setup(false);
                }
                if st.plan.settings_voice {
                    app.settings_draft = Some(app.cfg.clone());
                }
                if st.plan.setup_install {
                    // The Install button of the dialog opened from the Subtitles menu: when the
                    // install is done, recognition of this video starts by itself.
                    app.snap = app.player.snapshot();
                    app.open_asr_setup(true);
                    let hw = crate::asr_setup::hardware_now();
                    let cfg = &app.cfg.asr;
                    let v = crate::asr_setup::resolve(
                        &cfg.install_engine,
                        &cfg.install_model,
                        hw.gpu.as_ref(),
                    );
                    app.install_asr(v);
                }
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
            if st.plan.dub {
                // Only now: turned on earlier, it could stop playback to wait for a clip
                // and the line above would undo that.
                app.set_dub(true);
                st.dub_started = Some(Instant::now());
                if app.dub.is_none() {
                    st.failures
                        .push(format!("the voice-over did not start: {}", app.last_toast));
                    report(st, app);
                    return finish(st, ctx);
                }
            }
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
            if st.plan.dub {
                check_pause(st, app);
                if st.dub_ready_at_start.is_none()
                    && st
                        .dub_started
                        .is_some_and(|t| t.elapsed() > Duration::from_secs(1))
                {
                    st.dub_ready_at_start = app.dub.as_ref().map(|d| d.clips_ready());
                }
            }
            // With the voice-over the run is measured in video played: waiting for the first
            // clips and the pause in the middle do not count.
            let over = if st.plan.dub {
                (app.snap.pos - st.start_pos) as f64 >= st.plan.seconds * 1000.0
                    || elapsed.as_secs_f64() > st.plan.seconds + 120.0
            } else {
                elapsed.as_secs_f64() >= st.plan.seconds
            };
            if over {
                report(st, app);
                if st.plan.screenshot.is_some() {
                    let layer = egui::LayerId::new(egui::Order::Debug, egui::Id::unique("capture"));
                    ctx.layer_painter(layer).add(video::capture_callback(
                        ctx.content_rect(),
                        st.capture.clone(),
                    ));
                    st.phase = Phase::Capture;
                } else {
                    after_capture(st, app, ctx);
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
                after_capture(st, app, ctx);
            }
        }
        Phase::DubOff { since } => {
            let since = *since;
            if since.elapsed() >= Duration::from_secs(2) {
                let main = app.player.mpv.get_f64("volume").unwrap_or(-1.0);
                println!("main_volume_after_off={main:.0}");
                if (main - app.volume).abs() > 0.5 {
                    st.failures.push(format!(
                        "the original sound stayed at {main:.0} after the voice-over was turned off, not {:.0}",
                        app.volume
                    ));
                }
                let left = app.dub.is_some() || app.tts_setup.running();
                println!("dub_after_off={}", if left { "on" } else { "off" });
                finish(st, ctx);
            }
        }
        Phase::Done => {}
    }
}

/// After the screenshot: a voice-over run turns it off and watches the volume come back.
fn after_capture(st: &mut State, app: &mut App, ctx: &egui::Context) {
    if st.plan.dub && app.dub.is_some() {
        app.set_dub(false);
        st.phase = Phase::DubOff {
            since: Instant::now(),
        };
    } else {
        finish(st, ctx);
    }
}

/// Pauses the video for 1.5 s while a clip plays; the voice must stop with it.
fn check_pause(st: &mut State, app: &mut App) {
    let Some(d) = &app.dub else { return };
    match st.pause_check {
        PauseCheck::Waiting => {
            let halfway = (app.snap.pos - st.start_pos) as f64 >= st.plan.seconds * 1000.0 * 0.4;
            let into_clip = d.voice_pos().unwrap_or(0.0) >= 0.3;
            if halfway && d.playing() && into_clip && !d.holding() && !app.snap.paused {
                st.voice_output = Some(d.voice_output());
                app.player.set_paused(true);
                st.pause_check = PauseCheck::Paused {
                    since: Instant::now(),
                    samples: Vec::new(),
                };
            }
        }
        PauseCheck::Paused {
            since,
            ref mut samples,
        } => {
            let ms = since.elapsed().as_millis();
            if samples.last().is_none_or(|(t, _)| ms >= t + 100) {
                samples.push((ms, d.voice_pos().unwrap_or(-1.0)));
            }
            if ms >= 1_800 {
                // The pause reaches the voice player one tick later; from 300 ms on it must
                // stand still.
                let settled: Vec<f64> = samples
                    .iter()
                    .filter(|(t, _)| *t >= 300)
                    .map(|(_, p)| *p)
                    .collect();
                let b = settled.first().copied().unwrap_or(-1.0);
                let a = settled.last().copied().unwrap_or(-1.0);
                let still = (a - b).abs() < 0.02;
                let series: Vec<String> =
                    samples.iter().map(|(t, p)| format!("{t}:{p:.3}")).collect();
                println!("dub_pause_series={}", series.join(" "));
                println!(
                    "dub_pause_check=voice_pos_at_300ms={b:.3} voice_pos_at_1800ms={a:.3} {}",
                    if still { "ok" } else { "MOVED" }
                );
                if !still {
                    st.failures
                        .push("the voice kept playing while the video was paused".into());
                }
                app.player.set_paused(false);
                st.pause_check = PauseCheck::Done;
            }
        }
        PauseCheck::Done => {}
    }
}

fn report_dub(st: &mut State, app: &App) {
    let Some(d) = &app.dub else {
        st.failures
            .push("the voice-over was off at the end of the run".into());
        return;
    };
    let s = d.stats();
    let (ao, voice_muted) = st.voice_output.clone().unwrap_or_else(|| d.voice_output());
    let main_muted = app.player.mpv.get_flag("mute").unwrap_or(false);
    println!("dub_voice_ao={ao} voice_muted={voice_muted} main_muted={main_muted}");
    if ao != "null" || !voice_muted || !main_muted {
        st.failures
            .push("a self-test must not be heard, and the voice-over could be".into());
    }
    println!("dub_engine={}", s.engine);
    println!("dub_status={}", d.status());
    println!(
        "dub_clips_ready_after_1s={}",
        st.dub_ready_at_start
            .map_or("-".to_owned(), |n| n.to_string())
    );
    println!("dub_clips_ready={}", d.clips_total());
    println!(
        "dub_first_clip_wall_ms={}",
        s.first_clip
            .map_or("-".to_owned(), |t| t.as_millis().to_string())
    );
    println!("dub_synth_requests={}", s.synth_requests);
    println!("dub_cache_hits={}", s.cache_hits);
    println!("dub_server_starts={}", s.server_starts);
    if let Some(pid) = d.server_pid() {
        println!("dub_server_pid={pid}");
    }
    println!("dub_played={}", s.played.len());
    let mut bad = Vec::new();
    for p in &s.played {
        let offset = p.started_at.map(|a| a - p.start);
        println!(
            "dub={} started_at={} offset_ms={} clip_ms={} window_ms={} speed={:.2}{}{} text={}",
            p.start,
            p.started_at.map_or("-".to_owned(), |a| a.to_string()),
            offset.map_or("-".to_owned(), |o| o.to_string()),
            p.clip_ms,
            p.window,
            p.compression,
            if p.catch_up { " catch_up" } else { "" },
            if p.clip_ms > p.window + 300 {
                " over_window"
            } else {
                ""
            },
            p.text
        );
        let on_time = offset.is_some_and(|o| (0..=400).contains(&o));
        if !p.catch_up && !on_time {
            bad.push(p.start);
        }
    }
    println!("dub_skipped={}", s.skipped);
    println!("dub_max_lag_ms={}", s.max_lag());
    println!("dub_overlaps={}", s.overlaps);
    println!("dub_hold_ms={}", s.hold.as_millis());
    if !s.latency_ms.is_empty() {
        let n = s.latency_ms.len() as f64;
        let avg = s.latency_ms.iter().sum::<f64>() / n;
        let max = s.latency_ms.iter().copied().fold(f64::MIN, f64::max);
        let min = s.latency_ms.iter().copied().fold(f64::MAX, f64::min);
        println!("dub_start_latency_ms=avg {avg:.0} min {min:.0} max {max:.0}");
    }
    let main = app.player.mpv.get_f64("volume").unwrap_or(-1.0);
    let expected =
        gp_core::dub_plan::original_volume(app.volume, true, app.cfg.tts.original_volume);
    println!(
        "volumes: user={:.0} original_percent={:.0} main_player={main:.0} voice_player={:.0}",
        app.volume,
        app.cfg.tts.original_volume,
        d.voice_volume()
    );
    if s.played.is_empty() {
        st.failures.push("the voice-over played nothing".into());
    }
    if s.overlaps > 0 {
        st.failures
            .push(format!("{} clips overlapped another", s.overlaps));
    }
    if !bad.is_empty() {
        st.failures.push(format!(
            "clips started outside 0…400 ms of their line: {bad:?}"
        ));
    }
    if (main - expected).abs() > 0.5 {
        st.failures.push(format!(
            "the original sound plays at {main:.0}, not {expected:.0}"
        ));
    }
    if (d.voice_volume() - app.cfg.tts.voice_volume).abs() > 0.5 {
        st.failures
            .push("the voice plays at the wrong volume".into());
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
    println!(
        "ytdl_path={}",
        mpv.get_string("user-data/mpv/ytdl/path")
            .unwrap_or_default()
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
    if st.plan.dub {
        report_dub(st, app);
    }
    if st.plan.setup_install {
        let installed = app.asr_setup.installed.as_ref();
        println!(
            "asr_installed={}",
            installed.map_or("no".to_owned(), |i| i.status())
        );
        if installed.is_none() {
            st.failures
                .push("speech recognition did not get installed".into());
        }
    }
    if st.plan.asr || st.plan.setup_install {
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
    let srt = gp_core::write_srt(&track.track.cues);
    let back = gp_core::parse_subtitles(srt.as_bytes(), Some(gp_core::SubFormat::Srt));
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
        match std::fs::write(&tr_path, gp_core::write_srt(&translated)) {
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
