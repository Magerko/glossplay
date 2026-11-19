use crate::jobs::{AsrParams, CONTEXT_SENTENCES, Jobs, Msg, TranslateJob};
use crate::player::{Player, Snapshot, TrackInfo, TrackKind};
use crate::session::{
    Load, Origin, Secondary, Session, SubTrack, normalize_lang, sidecar_subtitles,
};
use crate::video::VideoSurface;
use crate::{paths, selftest, theme};
use eframe::egui;
use lp_ai::translate::{Translator, build_translator};
use lp_core::asr_plan;
use lp_core::config::Config;
use lp_core::training::{TrainAction, Trainer};
use lp_core::{Ms, Token};
use lp_media::mpv::{Event, RenderContext};
use lp_media::tools;
use lp_store::{NewWord, Store, VocabEntry};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

const NO_FFMPEG_TOAST: &str = "ffmpeg was not found — subtitles are shown without word lookup. Install ffmpeg or set its path in Settings.";

pub enum Async {
    Idle,
    Pending,
    Ready(String),
    Failed(String),
}

pub struct WordPopup {
    pub request: u64,
    pub word: String,
    /// (cue, token index) of the clicked word, to highlight exactly that one.
    pub token: (usize, usize),
    pub sentence: String,
    pub sentence_translation: String,
    pub cue: (Ms, Ms),
    pub anchor: egui::Pos2,
    pub translation: Async,
    pub explanation: Async,
    pub saved_id: Option<i64>,
    pub resume_on_close: bool,
}

#[derive(PartialEq, Clone, Copy)]
pub enum SidebarTab {
    Transcript,
    Words,
}

pub struct Launch {
    pub media: Option<String>,
    pub selftest: Option<selftest::Plan>,
}

pub struct App {
    pub cfg: Config,
    pub player: Player,
    pub video: VideoSurface,
    pub jobs: Jobs,
    pub session: Option<Session>,
    generation: u64,
    pub snap: Snapshot,
    prev_pos: Ms,
    pub trainer: Trainer,
    pub translator: Result<Arc<dyn Translator>, String>,
    pub ffmpeg: Option<PathBuf>,
    pub saved_words: HashSet<String>,
    pub vocab: Vec<VocabEntry>,
    pub tokens: HashMap<(usize, usize), Rc<Vec<Token>>>,
    pub popup: Option<WordPopup>,
    next_request: u64,
    pub sidebar_open: bool,
    pub sidebar_tab: SidebarTab,
    pub search: String,
    pub scrolled_to: Option<usize>,
    pub settings_draft: Option<Config>,
    pub url_input: String,
    /// A link opened before yt-dlp finished downloading; played as soon as it is there.
    pending_url: Option<String>,
    pub status: Option<(String, Instant)>,
    /// The last message shown, kept for the self-test report.
    pub last_toast: String,
    pub last_activity: Instant,
    pub fullscreen: bool,
    pub hwdec_current: String,
    pub audio_tracks: Vec<TrackInfo>,
    pub selftest: Option<selftest::State>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, launch: Launch) -> Result<App, String> {
        let (cfg, config_error) = paths::load_config();
        theme::install_fonts(&cc.egui_ctx);
        theme::apply(&cc.egui_ctx, cfg.general.ui_scale);

        let player = Player::new(&cfg.player).map_err(|e| e.to_string())?;
        if launch.selftest.is_some() {
            player.mpv.set_flag("mute", true);
        }
        let loader = cc
            .get_proc_address
            .clone()
            .ok_or("the OpenGL backend is not available")?;
        let repaint = cc.egui_ctx.clone();
        let render = RenderContext::new(
            player.mpv.clone(),
            loader,
            Box::new(move || repaint.request_repaint()),
        )
        .map_err(|e| e.to_string())?;

        // A damaged database only costs the cache, so I set it aside and start a fresh one.
        let store = Store::open(&paths::db_path()).or_else(|_| {
            let _ = std::fs::rename(
                paths::db_path(),
                paths::db_path().with_extension("db.broken"),
            );
            Store::open(&paths::db_path())
        });
        let store = store
            .or_else(|_| Store::open_in_memory())
            .map_err(|e| e.to_string())?;
        let vocab = store.list_words().unwrap_or_default();
        std::thread::spawn(lp_core::tokens::warm_up);
        let jobs = Jobs::new(cc.egui_ctx.clone(), store);
        if cfg.player.ytdl_path.is_empty() {
            jobs.spawn(|| Msg::YtDlp(crate::ytdlp::ensure(false)));
        }

        let mut app = App {
            translator: make_translator(&cfg),
            ffmpeg: tools::locate("ffmpeg", &cfg.player.ffmpeg_path).ok(),
            trainer: Trainer::new(cfg.training.autopause),
            saved_words: vocab.iter().map(|v| v.word.word.to_lowercase()).collect(),
            vocab,
            player,
            video: VideoSurface::new(render),
            jobs,
            pending_url: None,
            session: None,
            generation: 0,
            snap: Snapshot::default(),
            prev_pos: 0,
            tokens: HashMap::new(),
            popup: None,
            next_request: 0,
            sidebar_open: false,
            sidebar_tab: SidebarTab::Transcript,
            search: String::new(),
            scrolled_to: None,
            settings_draft: None,
            url_input: String::new(),
            status: None,
            last_toast: String::new(),
            last_activity: Instant::now(),
            fullscreen: false,
            hwdec_current: String::new(),
            audio_tracks: Vec::new(),
            selftest: launch.selftest.map(selftest::State::new),
            cfg,
        };
        if let Some(e) = config_error {
            app.toast(e);
        }
        if let Some(media) = launch.media {
            app.open(&media);
        }
        Ok(app)
    }

    pub fn toast(&mut self, text: impl Into<String>) {
        let text = text.into();
        self.last_toast.clone_from(&text);
        self.status = Some((text, Instant::now()));
    }

    pub fn open(&mut self, media: &str) {
        let is_subtitle = media
            .rsplit('.')
            .next()
            .and_then(lp_core::SubFormat::from_extension)
            .is_some();
        if is_subtitle && self.session.is_some() {
            self.add_external_subtitle(media.to_owned(), None, true);
            return;
        }
        let needs_ytdlp = media.starts_with("http")
            && self.cfg.player.ytdl_path.is_empty()
            && !crate::ytdlp::binary_path().is_file();
        if needs_ytdlp {
            self.pending_url = Some(media.to_owned());
            if !crate::ytdlp::is_busy() {
                self.jobs.spawn(|| Msg::YtDlp(crate::ytdlp::ensure(true)));
            }
            self.toast("Downloading yt-dlp, the video will start in a moment…");
            return;
        }
        // `canonicalize` would add a `\?` prefix on Windows that mpv and ffmpeg do not accept.
        let media = if media.contains("://") {
            media.to_owned()
        } else {
            std::path::absolute(media)
                .map_or_else(|_| media.to_owned(), |p| p.to_string_lossy().into_owned())
        };
        let media = media.as_str();
        self.generation += 1;
        self.tokens.clear();
        self.popup = None;
        self.scrolled_to = None;
        self.session = Some(Session::new(self.generation, media.to_owned()));
        self.player.mpv.set("sid", "no");
        self.player.mpv.set_flag("sub-visibility", false);
        if let Err(e) = self.player.load(media) {
            self.toast(e.to_string());
        }
        self.player.set_paused(false);
    }

    fn add_external_subtitle(&mut self, path: String, lang: Option<String>, make_primary: bool) {
        let Some(s) = &mut self.session else { return };
        let name = std::path::Path::new(&path)
            .file_name()
            .map_or(path.clone(), |n| n.to_string_lossy().into_owned());
        let index = s.tracks.len();
        s.tracks.push(SubTrack::new(
            name,
            lang,
            Origin::External { path: path.clone() },
            Load::Loading,
        ));
        if make_primary {
            s.primary = Some(index);
        }
        self.jobs.load_external(s.generation, index, path);
    }

    /// Called once mpv has opened the file and knows its tracks.
    fn discover_tracks(&mut self) {
        let all = self.player.tracks();
        self.audio_tracks = all
            .iter()
            .filter(|t| t.kind == TrackKind::Audio)
            .cloned()
            .collect();
        self.hwdec_current = self
            .player
            .mpv
            .get_string("hwdec-current")
            .unwrap_or_default();
        let Some(s) = &mut self.session else { return };
        if s.tracks_discovered {
            return;
        }
        s.tracks_discovered = true;
        if let Some(title) = self
            .player
            .mpv
            .get_string("media-title")
            .filter(|_| s.is_url())
        {
            s.title = title;
        }
        for t in all.iter().filter(|t| t.kind == TrackKind::Sub) {
            let lang = t.lang.as_deref().map(normalize_lang);
            let index = s.tracks.len();
            let (origin, load) = match (&t.external_filename, t.ff_index) {
                _ if t.is_bitmap_sub() => (
                    Origin::Embedded {
                        mpv_id: t.id,
                        stream: 0,
                    },
                    Load::Bitmap,
                ),
                (Some(file), _) => (
                    Origin::External {
                        path: crate::session::subtitle_url(file),
                    },
                    Load::NotLoaded,
                ),
                (None, Some(stream)) => (
                    Origin::Embedded {
                        mpv_id: t.id,
                        stream,
                    },
                    Load::NotLoaded,
                ),
                (None, None) => continue,
            };
            let _ = index;
            s.tracks.push(SubTrack::new(t.label(), lang, origin, load));
        }
        if !s.is_url() {
            for (path, lang) in sidecar_subtitles(&s.media) {
                let index = s.tracks.len();
                let name = std::path::Path::new(&path)
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                s.tracks.push(SubTrack::new(
                    name,
                    lang,
                    Origin::External { path: path.clone() },
                    Load::Loading,
                ));
                self.jobs.load_external(s.generation, index, path);
            }
        }
        self.choose_default_tracks();
        if let Some((primary, secondary)) = self.session.as_ref().map(|s| (s.primary, s.secondary))
        {
            if let Some(i) = primary {
                self.ensure_loaded(i);
            }
            if let Secondary::Track(i) = secondary {
                self.ensure_loaded(i);
            }
        }
        let no_text = self
            .session
            .as_ref()
            .is_some_and(|s| !s.tracks.iter().any(SubTrack::is_text));
        if no_text && self.cfg.asr.enabled {
            self.start_asr();
        }
    }

    fn choose_default_tracks(&mut self) {
        let Some(s) = &mut self.session else { return };
        let (learn, native) = (&self.cfg.general.learn_lang, &self.cfg.general.native_lang);
        let text: Vec<usize> = (0..s.tracks.len())
            .filter(|&i| s.tracks[i].is_text())
            .collect();
        let lang_of = |i: usize| s.tracks[i].track.lang.clone().unwrap_or_default();
        let primary = text
            .iter()
            .find(|&&i| !learn.is_empty() && &lang_of(i) == learn)
            .or_else(|| text.iter().find(|&&i| &lang_of(i) != native))
            .or(text.first())
            .copied();
        let native_track = text
            .iter()
            .copied()
            .find(|&i| Some(i) != primary && &lang_of(i) == native);
        s.primary = primary;
        s.secondary = match native_track {
            Some(i) => Secondary::Track(i),
            None if self.cfg.subtitles.auto_translate => Secondary::Translate,
            None => Secondary::Off,
        };
        // Image subtitles are the only kind mpv still draws itself.
        let bitmap = s.tracks.iter().find_map(|t| match (&t.origin, &t.load) {
            (Origin::Embedded { mpv_id, .. }, Load::Bitmap) if primary.is_none() => Some(*mpv_id),
            _ => None,
        });
        self.mpv_draws(bitmap);
    }

    /// mpv renders subtitles itself only for tracks I cannot show as text: image tracks and
    /// tracks that failed to load. `None` turns its renderer off.
    pub fn mpv_draws(&self, track: Option<i64>) {
        match track {
            Some(id) => {
                self.player.mpv.set("sid", &id.to_string());
                self.player.mpv.set_flag("sub-visibility", true);
            }
            None => self.player.mpv.set_flag("sub-visibility", false),
        }
    }

    /// Reads a track the first time it is needed. An online video can offer dozens of
    /// languages, and every embedded track costs a pass over the file.
    pub fn ensure_loaded(&mut self, index: usize) {
        let Some(s) = &mut self.session else { return };
        let Some(t) = s
            .tracks
            .get_mut(index)
            .filter(|t| t.load == Load::NotLoaded)
        else {
            return;
        };
        t.load = Load::Loading;
        match &t.origin {
            Origin::External { path } => self.jobs.load_external(s.generation, index, path.clone()),
            Origin::Embedded { stream, .. } => self.jobs.load_embedded(
                s.generation,
                index,
                self.ffmpeg.clone(),
                s.media.clone(),
                *stream,
            ),
            Origin::Asr => {}
        }
    }

    pub fn set_secondary(&mut self, choice: Secondary) {
        if let Some(s) = &mut self.session {
            s.secondary = choice;
        }
        if let Secondary::Track(i) = choice {
            self.ensure_loaded(i);
        }
    }

    pub fn asr_configured(&self) -> Result<(PathBuf, PathBuf), String> {
        let server = tools::locate("whisper-server", &self.cfg.asr.server_path).map_err(|_| {
            "Set the path to whisper-server in Settings → Speech recognition".to_owned()
        })?;
        let model = PathBuf::from(&self.cfg.asr.model_path);
        if !model.is_file() {
            return Err(
                "Set the path to a Whisper model (ggml-*.bin) in Settings → Speech recognition"
                    .into(),
            );
        }
        Ok((server, model))
    }

    pub fn start_asr(&mut self) {
        let (server, model) = match self.asr_configured() {
            Ok(paths) => paths,
            Err(e) => return self.toast(e),
        };
        let Some(ffmpeg) = self.ffmpeg.clone() else {
            return self.toast("ffmpeg is required for transcription");
        };
        let duration = self.snap.duration.max(self.player.snapshot().duration);
        let Some(s) = &mut self.session else { return };
        if s.is_url() {
            return self.toast("Live transcription works with local files only for now");
        }
        if duration <= 0 {
            return self.toast("The file has no known duration yet — try again in a moment");
        }
        let index = match s.tracks.iter().position(|t| t.origin == Origin::Asr) {
            Some(i) => i,
            None => {
                let lang = Some(self.cfg.general.learn_lang.clone()).filter(|l| !l.is_empty());
                s.tracks.push(SubTrack::new(
                    "Speech recognition".into(),
                    lang,
                    Origin::Asr,
                    Load::Loading,
                ));
                s.tracks.len() - 1
            }
        };
        s.primary = Some(index);
        if s.secondary == Secondary::Off && self.cfg.subtitles.auto_translate {
            s.secondary = Secondary::Translate;
        }
        self.player.mpv.set_flag("sub-visibility", false);

        let lang = Some(self.cfg.general.learn_lang.clone()).filter(|l| !l.is_empty());
        let engine_id = format!(
            "whisper.cpp:{}",
            model.file_stem().unwrap_or_default().to_string_lossy()
        );
        let cached = self
            .jobs
            .store
            .lock()
            .unwrap()
            .load_transcript(&s.media_key, &engine_id, lang.as_deref().unwrap_or(""))
            .ok()
            .flatten();
        let (mut done, mut cues) = (vec![false; asr_plan::window_count(duration)], Vec::new());
        if let Some(saved) = cached {
            let (restored, mask_used) =
                asr_plan::restore_done(duration, saved.covered, &saved.done);
            done = restored;
            cues = saved.cues;
            if !mask_used {
                // Without a matching mask only the prefix is known to be complete.
                cues.retain(|c| c.start < saved.covered);
            }
        }
        s.tracks[index].set_cues(cues.clone());
        s.tracks[index].growing = done.contains(&false).then(|| done.clone());

        // Silero VAD keeps Whisper from inventing text over silence; I use it when the model
        // file sits next to the Whisper model.
        let vad_model = model.parent().and_then(|dir| {
            std::fs::read_dir(dir)
                .ok()?
                .flatten()
                .map(|e| e.path())
                .find(|p| {
                    p.file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with("ggml-silero"))
                })
        });
        let audio_index = self
            .audio_tracks
            .iter()
            .find(|t| t.selected)
            .and_then(|t| t.ff_index);
        s.asr_running = true;
        s.asr = Some(self.jobs.start_asr(AsrParams {
            generation: s.generation,
            media: s.media.clone(),
            media_key: s.media_key.clone(),
            audio_index,
            duration,
            lang,
            ffmpeg,
            server,
            model,
            vad_model,
            done,
            cues,
        }));
    }

    pub fn stop_asr(&mut self) {
        if let Some(s) = &mut self.session {
            s.asr = None;
            s.asr_running = false;
            s.asr_status = String::new();
        }
    }

    fn handle_messages(&mut self) {
        while let Ok(msg) = self.jobs.rx.try_recv() {
            let current = self.session.as_ref().map(|s| s.generation);
            match msg {
                Msg::TrackLoaded {
                    generation,
                    track,
                    result,
                } if Some(generation) == current => {
                    let s = self.session.as_mut().unwrap();
                    match result {
                        Ok(cues) => s.tracks[track].set_cues(cues),
                        Err(e) => {
                            let t = &mut s.tracks[track];
                            t.load = Load::Failed(e.clone());
                            let mpv_id = match t.origin {
                                Origin::Embedded { mpv_id, .. } if s.primary == Some(track) => {
                                    Some(mpv_id)
                                }
                                _ => None,
                            };
                            let label = t.label.clone();
                            let embedded = matches!(t.origin, Origin::Embedded { .. });
                            let no_ffmpeg = self.ffmpeg.is_none();
                            let text = match (mpv_id, no_ffmpeg) {
                                (Some(_), true) => Some(NO_FFMPEG_TOAST.to_owned()),
                                (Some(_), false) => Some(format!(
                                    "Could not read the subtitles \"{label}\": {e} — they are shown without word lookup."
                                )),
                                // Same cause as the message for the main track; one is enough.
                                (None, true) if embedded => None,
                                (None, _) => {
                                    Some(format!("Could not load the subtitles \"{label}\": {e}"))
                                }
                            };
                            if mpv_id.is_some() {
                                self.mpv_draws(mpv_id);
                            }
                            if let Some(text) = text {
                                self.toast(text);
                            }
                        }
                    }
                    self.tokens.retain(|(t, _), _| *t != track);
                }
                Msg::Translated {
                    generation,
                    track,
                    epoch,
                    sentences,
                    result,
                } if Some(generation) == current => {
                    let s = self.session.as_mut().unwrap();
                    let t = &mut s.tracks[track];
                    t.apply_translations(&sentences, result.as_deref().ok(), epoch);
                    if let Err(error) = result {
                        s.translate_failed_at = Some(Instant::now());
                        self.toast(format!("Translation failed: {error}"));
                    }
                }
                Msg::AsrCues {
                    generation,
                    cues,
                    window,
                    language,
                } if Some(generation) == current => {
                    let s = self.session.as_mut().unwrap();
                    if let Some(t) = s.tracks.iter_mut().find(|t| t.origin == Origin::Asr) {
                        if t.track.lang.is_none() {
                            t.track.lang = language;
                        }
                        if let Some(done) = t.growing.as_mut().and_then(|d| d.get_mut(window)) {
                            *done = true;
                        }
                        t.merge_cues(cues);
                    }
                    self.tokens.clear();
                }
                Msg::AsrStatus {
                    generation,
                    text,
                    running,
                } if Some(generation) == current => {
                    let s = self.session.as_mut().unwrap();
                    s.asr_status = text;
                    s.asr_running = running;
                    if !running {
                        // Finished or failed: no more cues will come to continue a sentence.
                        for t in s.tracks.iter_mut().filter(|t| t.origin == Origin::Asr) {
                            t.growing = None;
                        }
                    }
                }
                Msg::WordTranslated { request, result } => {
                    if let Some(p) = self.popup.as_mut().filter(|p| p.request == request) {
                        p.translation = result.map_or_else(Async::Failed, Async::Ready);
                    }
                }
                Msg::WordExplained { request, result } => {
                    if let Some(p) = self.popup.as_mut().filter(|p| p.request == request) {
                        p.explanation = match result {
                            Ok(text) => Async::Ready(text),
                            // Plain translators cannot explain; that is not worth an error message.
                            Err(e) if e.contains("does not support") => Async::Idle,
                            Err(e) => Async::Failed(e),
                        };
                    }
                }
                Msg::YtDlp(result) => {
                    match result {
                        Ok(crate::ytdlp::Outcome::Updated { version }) => {
                            self.toast(format!("yt-dlp updated to {version}"));
                        }
                        Ok(_) => {}
                        // A failed start-up check stays quiet; only a link waiting for yt-dlp needs to hear.
                        Err(e) if self.pending_url.take().is_some() => self.toast(e),
                        Err(_) => {}
                    }
                    let ready = crate::ytdlp::binary_path().is_file();
                    if let Some(url) = self.pending_url.take().filter(|_| ready) {
                        self.open(&url);
                    }
                }
                Msg::Anki { vocab_id, result } => match result {
                    Ok(note_id) => {
                        let _ = self
                            .jobs
                            .store
                            .lock()
                            .unwrap()
                            .set_anki_id(vocab_id, note_id);
                        self.reload_vocab();
                        self.toast("Added to Anki");
                    }
                    Err(e) => self.toast(format!("Anki: {e}")),
                },
                _ => {}
            }
        }
    }

    /// Asks for translations a little ahead of the playhead, so they are ready on time.
    fn request_translations(&mut self) {
        let Ok(translator) = self.translator.clone() else {
            return;
        };
        let Some(s) = &mut self.session else { return };
        if s.secondary != Secondary::Translate {
            return;
        }
        if s.translate_failed_at
            .is_some_and(|t| t.elapsed() < Duration::from_secs(30))
        {
            return;
        }
        let Some(index) = s.primary.filter(|&i| s.tracks[i].load == Load::Ready) else {
            return;
        };
        let generation = s.generation;
        let t = &mut s.tracks[index];
        let pos = self.snap.pos;
        let Some(from) = t
            .track
            .index_at_or_before(pos)
            .or_else(|| t.track.index_after(pos))
        else {
            return;
        };
        // The window ahead of the playhead, widened to whole sentences.
        let lookahead = self.cfg.translation.lookahead.max(1);
        let first = t.group_of(from);
        let (mut wanted, mut wanted_cues, mut covered) = (Vec::new(), 0, 0);
        for g in first..t.groups.len() {
            if covered >= lookahead {
                break;
            }
            covered += t.groups[g].len();
            if t.ready_to_translate(g, pos) {
                wanted.push(g);
                wanted_cues += t.groups[g].len();
            }
        }
        // Waiting until half of the window is missing turns many one-line requests into a few batches.
        let urgent = wanted
            .first()
            .is_some_and(|&g| t.track.cues[t.groups[g].start].start <= pos + 5_000);
        if wanted.is_empty() || (!urgent && wanted_cues < lookahead / 2) {
            return;
        }
        let context = (wanted[0].saturating_sub(CONTEXT_SENTENCES)..wanted[0])
            .map(|g| t.sentence_text(g))
            .collect();
        let sentences = t.request_translations(&wanted);
        let src = t
            .track
            .lang
            .clone()
            .or(Some(self.cfg.general.learn_lang.clone()))
            .filter(|l| !l.is_empty());
        self.jobs.translate(TranslateJob {
            generation,
            track: index,
            epoch: t.epoch,
            sentences,
            context,
            src,
            dst: self.cfg.general.native_lang.clone(),
            translator,
        });
    }

    pub fn open_word(&mut self, word: String, cue: usize, token: usize, anchor: egui::Pos2) {
        let Some(s) = &self.session else { return };
        let Some(t) = s.primary_track() else { return };
        let c = &t.track.cues[cue];
        self.next_request += 1;
        let was_playing = !self.snap.paused;
        let (translation, explanation) = match &self.translator {
            Ok(tr) => {
                let src = t
                    .track
                    .lang
                    .clone()
                    .or(Some(self.cfg.general.learn_lang.clone()))
                    .filter(|l| !l.is_empty());
                self.jobs.lookup_word(
                    self.next_request,
                    tr.clone(),
                    word.clone(),
                    c.text.replace('\n', " "),
                    src,
                    self.cfg.general.native_lang.clone(),
                );
                (Async::Pending, Async::Pending)
            }
            Err(e) => (Async::Failed(e.clone()), Async::Idle),
        };
        let saved_id = self
            .vocab
            .iter()
            .find(|v| v.word.word.to_lowercase() == word.to_lowercase())
            .map(|v| v.id);
        self.popup = Some(WordPopup {
            request: self.next_request,
            sentence: c.text.replace('\n', " "),
            sentence_translation: s.translation_of(cue).unwrap_or_default(),
            cue: (c.start, c.end),
            token: (cue, token),
            word,
            anchor,
            translation,
            explanation,
            saved_id,
            resume_on_close: was_playing,
        });
        self.player.set_paused(true);
    }

    pub fn close_popup(&mut self) {
        if let Some(p) = self.popup.take()
            && p.resume_on_close
        {
            self.player.set_paused(false);
        }
    }

    pub fn save_popup_word(&mut self) -> Option<i64> {
        let (p, s) = (self.popup.as_ref()?, self.session.as_ref()?);
        let text = |a: &Async| match a {
            Async::Ready(t) => t.clone(),
            _ => String::new(),
        };
        let lang = s
            .primary_track()
            .and_then(|t| t.track.lang.clone())
            .unwrap_or_default();
        let word = NewWord {
            word: p.word.clone(),
            lang,
            translation: text(&p.translation),
            note: text(&p.explanation),
            sentence: p.sentence.clone(),
            sentence_translation: p.sentence_translation.clone(),
            media_title: s.title.clone(),
            media_path: s.media.clone(),
            pos_ms: self.snap.pos,
            cue_start_ms: p.cue.0,
            cue_end_ms: p.cue.1,
        };
        let id = self.jobs.store.lock().unwrap().save_word(&word).ok()?;
        // Taken now, while the word's moment is on screen, so a later export has a picture.
        let _ = self
            .player
            .screenshot(&paths::media_dir().join(format!("linguaplay_{id}.jpg")));
        self.reload_vocab();
        if let Some(p) = &mut self.popup {
            p.saved_id = Some(id);
        }
        Some(id)
    }

    pub fn reload_vocab(&mut self) {
        self.vocab = self
            .jobs
            .store
            .lock()
            .unwrap()
            .list_words()
            .unwrap_or_default();
        self.saved_words = self
            .vocab
            .iter()
            .map(|v| v.word.word.to_lowercase())
            .collect();
    }

    pub fn delete_word(&mut self, id: i64) {
        let _ = self.jobs.store.lock().unwrap().delete_word(id);
        self.reload_vocab();
    }

    pub fn export_to_anki(&mut self, vocab_id: i64) {
        let Some(entry) = self.vocab.iter().find(|v| v.id == vocab_id).cloned() else {
            return;
        };
        let dir = paths::media_dir();
        // A screenshot only makes sense while the saved moment is still on screen.
        let same_media = self
            .session
            .as_ref()
            .is_some_and(|s| s.media == entry.word.media_path);
        let on_screen = same_media && (self.snap.pos - entry.word.pos_ms).abs() < 3_000;
        let image = dir.join(format!("linguaplay_{vocab_id}.jpg"));
        let image = (image.is_file() || (on_screen && self.player.screenshot(&image).is_ok()))
            .then_some(image);
        let local = !entry.word.media_path.contains("://");
        let stream = self
            .audio_tracks
            .iter()
            .find(|t| t.selected)
            .and_then(|t| t.ff_index)
            .filter(|_| same_media);
        let audio = self
            .ffmpeg
            .clone()
            .filter(|_| local)
            .map(|f| (f, stream, dir.join(format!("linguaplay_{vocab_id}.mp3"))));
        self.toast("Sending to Anki…");
        self.jobs.export_anki(
            vocab_id,
            self.cfg.anki.url.clone(),
            self.cfg.anki.deck.clone(),
            entry.word,
            image,
            audio,
        );
    }

    pub fn apply_settings(&mut self, new: Config) {
        let rescale = new.general.ui_scale != self.cfg.general.ui_scale;
        self.cfg = new;
        self.translator = make_translator(&self.cfg);
        self.trainer.autopause = self.cfg.training.autopause;
        self.ffmpeg = tools::locate("ffmpeg", &self.cfg.player.ffmpeg_path).ok();
        if let Some(s) = &mut self.session {
            // A new provider or language makes the old translations stale.
            for t in &mut s.tracks {
                t.reset_translations();
            }
            s.translate_failed_at = None;
        }
        if let Err(e) = paths::save_config(&self.cfg) {
            self.toast(format!("Could not save settings: {e}"));
        }
        if rescale {
            self.toast("The interface scale applies after a restart");
        }
    }

    pub fn train(&mut self, action: Option<TrainAction>) {
        match action {
            Some(TrainAction::Pause) => self.player.set_paused(true),
            Some(TrainAction::SeekTo(ms)) => {
                self.player.seek(ms);
                self.player.set_paused(false);
            }
            None => {}
        }
    }

    pub fn cue_nav(&mut self, which: i32) {
        let Some(t) = self.session.as_ref().and_then(Session::primary_track) else {
            return self.player.seek_relative(which as Ms * 5_000);
        };
        let pos = self.snap.pos;
        let action = match which {
            -1 => self.trainer.prev(&t.track, pos),
            0 => self.trainer.replay(&t.track, pos),
            _ => self.trainer.next(&t.track, pos),
        };
        self.train(action);
    }

    pub fn toggle_fullscreen(&mut self, ctx: &egui::Context) {
        self.fullscreen = !self.fullscreen;
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.fullscreen));
    }

    pub fn open_dialog(&mut self) {
        let picked = rfd::FileDialog::new()
            .set_title("Open video or subtitles")
            .pick_file();
        if let Some(path) = picked {
            self.open(&path.to_string_lossy());
        }
    }

    /// Per-frame bookkeeping that is not drawing.
    pub fn tick(&mut self, ctx: &egui::Context) {
        self.video.render.report_swap();
        while let Some(event) = self.player.mpv.poll_event() {
            match event {
                Event::FileLoaded => self.discover_tracks(),
                Event::EndFile { error: true } => self.toast("Could not play this file"),
                _ => {}
            }
        }
        self.handle_messages();
        self.prev_pos = self.snap.pos;
        self.snap = self.player.snapshot();
        if let Some(s) = &self.session {
            if let Some(asr) = &s.asr {
                asr.playhead.store(self.snap.pos, Ordering::Relaxed);
            }
            if !self.snap.paused {
                let action = s
                    .primary_track()
                    .and_then(|t| self.trainer.tick(&t.track, self.prev_pos, self.snap.pos));
                self.train(action);
            }
        }
        self.request_translations();
        if !self.snap.paused && !self.snap.idle {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        if self
            .status
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() > Duration::from_secs(5))
        {
            self.status = None;
        }
    }

    pub fn tokens_for(
        &mut self,
        track: usize,
        cue: usize,
        text: &str,
        lang: Option<&str>,
    ) -> Rc<Vec<Token>> {
        self.tokens
            .entry((track, cue))
            .or_insert_with(|| Rc::new(lp_core::tokenize(text, lang)))
            .clone()
    }
}

fn make_translator(cfg: &Config) -> Result<Arc<dyn Translator>, String> {
    build_translator(&cfg.translation)
        .map(Arc::from)
        .map_err(|e| e.to_string())
}
