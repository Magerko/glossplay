//! Background work. The UI thread never touches the network or the disk: it sends a job,
//! keeps drawing, and picks up a `Msg` when the job is done.

use crate::session::SentenceKey;
use eframe::egui;
use lp_ai::AiError;
use lp_ai::anki::{AnkiClient, AnkiNote};
use lp_ai::asr::{AsrEngine, WhisperServer};
use lp_ai::translate::{TranslateRequest, Translator, translate_resilient};
use lp_core::asr_plan;
use lp_core::{Cue, Ms};
use lp_media::tools::{self, WhisperProcess};
use lp_store::Store;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub enum Msg {
    TrackLoaded {
        generation: u64,
        track: usize,
        result: Result<Vec<Cue>, String>,
    },
    Translated {
        generation: u64,
        track: usize,
        epoch: u64,
        sentences: Vec<SentenceKey>,
        result: Result<Vec<String>, String>,
    },
    AsrCues {
        generation: u64,
        cues: Vec<Cue>,
        window: usize,
        language: Option<String>,
    },
    AsrStatus {
        generation: u64,
        text: String,
        running: bool,
    },
    WordTranslated {
        request: u64,
        result: Result<String, String>,
    },
    WordExplained {
        request: u64,
        result: Result<String, String>,
    },
    YtDlp(Result<crate::ytdlp::Outcome, String>),
    Anki {
        vocab_id: i64,
        result: Result<i64, String>,
    },
}

pub struct TranslateJob {
    pub generation: u64,
    pub track: usize,
    pub epoch: u64,
    pub sentences: Vec<SentenceKey>,
    /// Sentences of the original just before the first one asked, for LLM providers.
    pub context: Vec<String>,
    pub src: Option<String>,
    pub dst: String,
    pub translator: Arc<dyn Translator>,
}

pub struct TranslateOutcome {
    pub results: Vec<String>,
    /// How many times the provider was actually called; zero means everything came from cache.
    pub requests: usize,
}

const RETRY_DELAYS: [u64; 3] = [1, 4, 15];
pub const CONTEXT_SENTENCES: usize = 8;

/// Translates `lines`, asking the provider only for what the cache does not have.
pub fn translate_cached(
    store: &Mutex<Store>,
    translator: &dyn Translator,
    src: Option<&str>,
    dst: &str,
    lines: &[String],
    context: &[String],
) -> Result<TranslateOutcome, AiError> {
    let (provider, src_key) = (translator.id(), src.unwrap_or(""));
    let cached = store
        .lock()
        .unwrap()
        .get_translations(&provider, src_key, dst, lines)
        .unwrap_or_else(|_| vec![None; lines.len()]);
    let missing: Vec<usize> = (0..lines.len()).filter(|&i| cached[i].is_none()).collect();
    let mut results: Vec<String> = cached.into_iter().map(Option::unwrap_or_default).collect();
    let mut requests = 0;
    for batch in missing.chunks(translator.batch_size().max(1)) {
        let batch_lines: Vec<String> = batch.iter().map(|&i| lines[i].clone()).collect();
        // Sentences just before the batch give an LLM the context of the dialogue.
        let first = batch[0];
        let mut ctx: Vec<String> = context.to_vec();
        ctx.extend(lines[..first].iter().cloned());
        let ctx = ctx.split_off(ctx.len().saturating_sub(CONTEXT_SENTENCES));
        let req = TranslateRequest {
            src_lang: src,
            dst_lang: dst,
            lines: &batch_lines,
            context_before: &ctx,
        };
        let mut attempt = 0;
        let translated = loop {
            requests += 1;
            match translate_resilient(translator, &req) {
                Err(e) if e.is_transient() && attempt < RETRY_DELAYS.len() => {
                    std::thread::sleep(Duration::from_secs(RETRY_DELAYS[attempt]));
                    attempt += 1;
                }
                other => break other?,
            }
        };
        let pairs: Vec<(String, String)> = batch_lines
            .into_iter()
            .zip(translated.iter().cloned())
            .collect();
        let _ = store
            .lock()
            .unwrap()
            .put_translations(&provider, src_key, dst, &pairs);
        for (&i, text) in batch.iter().zip(translated) {
            results[i] = text;
        }
    }
    Ok(TranslateOutcome { results, requests })
}

pub struct AsrParams {
    pub generation: u64,
    pub media: String,
    pub media_key: String,
    pub audio_index: Option<u32>,
    pub duration: Ms,
    pub lang: Option<String>,
    pub ffmpeg: PathBuf,
    pub server: PathBuf,
    pub model: PathBuf,
    pub vad_model: Option<PathBuf>,
    /// Windows already transcribed in an earlier run.
    pub done: Vec<bool>,
    pub cues: Vec<Cue>,
}

/// Cancels the worker when dropped, so switching files never leaves two of them running.
pub struct AsrHandle {
    pub playhead: Arc<AtomicI64>,
    cancel: Arc<AtomicBool>,
}

impl Drop for AsrHandle {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

pub struct Jobs {
    tx: Sender<Msg>,
    pub rx: Receiver<Msg>,
    ctx: egui::Context,
    pub store: Arc<Mutex<Store>>,
    translate_tx: Sender<TranslateJob>,
    whisper: Arc<Mutex<Option<WhisperProcess>>>,
    /// Set by `shutdown`; an ASR thread still running must not start a new server after it.
    closing: Arc<AtomicBool>,
}

impl Jobs {
    pub fn new(ctx: egui::Context, store: Store) -> Jobs {
        let (tx, rx) = channel();
        let store = Arc::new(Mutex::new(store));
        let (translate_tx, translate_rx) = channel::<TranslateJob>();
        let (worker_tx, worker_ctx, worker_store) = (tx.clone(), ctx.clone(), store.clone());
        // One translation at a time keeps me inside provider rate limits.
        std::thread::spawn(move || {
            for job in translate_rx {
                let lines: Vec<String> = job.sentences.iter().map(|s| s.text.clone()).collect();
                let result = translate_cached(
                    &worker_store,
                    job.translator.as_ref(),
                    job.src.as_deref(),
                    &job.dst,
                    &lines,
                    &job.context,
                )
                .map(|o| o.results)
                .map_err(|e| e.to_string());
                let _ = worker_tx.send(Msg::Translated {
                    generation: job.generation,
                    track: job.track,
                    epoch: job.epoch,
                    sentences: job.sentences,
                    result,
                });
                worker_ctx.request_repaint();
            }
        });
        Jobs {
            tx,
            rx,
            ctx,
            store,
            translate_tx,
            whisper: Arc::default(),
            closing: Arc::default(),
        }
    }

    /// Stops whisper-server now. `main` leaves through `process::exit`, which runs no
    /// destructors, and the ASR thread holds its own reference to the slot anyway.
    pub fn shutdown(&self) {
        self.closing.store(true, Ordering::SeqCst);
        let process = self
            .whisper
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        drop(process);
    }

    pub fn spawn(&self, work: impl FnOnce() -> Msg + Send + 'static) {
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(work());
            ctx.request_repaint();
        });
    }

    pub fn translate(&self, job: TranslateJob) {
        let _ = self.translate_tx.send(job);
    }

    pub fn load_embedded(
        &self,
        generation: u64,
        track: usize,
        ffmpeg: Option<PathBuf>,
        media: String,
        stream: u32,
    ) {
        self.spawn(move || {
            let result = ffmpeg
                .ok_or_else(|| "ffmpeg was not found".to_owned())
                .and_then(|f| {
                    tools::extract_subtitles(&f, &media, stream).map_err(|e| e.to_string())
                })
                .and_then(|bytes| {
                    lp_core::parse_subtitles(&bytes, Some(lp_core::SubFormat::Srt))
                        .map_err(|e| e.to_string())
                });
            Msg::TrackLoaded {
                generation,
                track,
                result,
            }
        });
    }

    pub fn load_external(&self, generation: u64, track: usize, path: String) {
        self.spawn(move || {
            let hint = path
                .rsplit('.')
                .next()
                .and_then(lp_core::SubFormat::from_extension);
            let bytes = if path.starts_with("http://") || path.starts_with("https://") {
                lp_ai::fetch(&path).map_err(|e| e.to_string())
            } else {
                std::fs::read(&path).map_err(|e| e.to_string())
            };
            let result =
                bytes.and_then(|b| lp_core::parse_subtitles(&b, hint).map_err(|e| e.to_string()));
            Msg::TrackLoaded {
                generation,
                track,
                result,
            }
        });
    }

    pub fn lookup_word(
        &self,
        request: u64,
        translator: Arc<dyn Translator>,
        word: String,
        sentence: String,
        src: Option<String>,
        dst: String,
    ) {
        let (store, t) = (self.store.clone(), translator.clone());
        let (w, d) = (word.clone(), dst.clone());
        self.spawn(move || {
            let result = translate_cached(&store, t.as_ref(), src.as_deref(), &d, &[w], &[])
                .map(|mut o| o.results.remove(0))
                .map_err(|e| e.to_string());
            Msg::WordTranslated { request, result }
        });
        self.spawn(move || {
            let result = translator
                .explain(&word, &sentence, &dst)
                .map_err(|e| e.to_string());
            Msg::WordExplained { request, result }
        });
    }

    #[allow(clippy::too_many_arguments)]
    pub fn export_anki(
        &self,
        vocab_id: i64,
        url: String,
        deck: String,
        word: lp_store::NewWord,
        image: Option<PathBuf>,
        audio_job: Option<(PathBuf, Option<u32>, PathBuf)>,
    ) {
        self.spawn(move || {
            // The clip is optional: a card without audio is better than no card.
            let audio = audio_job.and_then(|(ffmpeg, stream, out)| {
                let len = (word.cue_end_ms - word.cue_start_ms).clamp(500, 20_000);
                tools::extract_clip(
                    &ffmpeg,
                    &word.media_path,
                    stream,
                    word.cue_start_ms,
                    len,
                    &out,
                )
                .ok()
                .map(|_| out)
            });
            let anki = AnkiClient::new(&url);
            let source = format!(
                "{} — {}",
                word.media_title,
                crate::theme::clock(word.pos_ms)
            );
            let result = anki
                .ensure_setup(&deck)
                .and_then(|_| {
                    anki.add_note(&AnkiNote {
                        deck: &deck,
                        word: &word.word,
                        translation: &word.translation,
                        sentence: &word.sentence,
                        sentence_translation: &word.sentence_translation,
                        note: &word.note,
                        source: &source,
                        image: image.as_deref().filter(|p| p.is_file()),
                        audio: audio.as_deref(),
                    })
                })
                .map_err(|e| match e {
                    AiError::Network(_) => {
                        "Anki is not running or AnkiConnect is not installed".to_owned()
                    }
                    AiError::Duplicate => "this word is already in the deck".to_owned(),
                    other => other.to_string(),
                });
            Msg::Anki { vocab_id, result }
        });
    }

    pub fn start_asr(&self, params: AsrParams) -> AsrHandle {
        let handle = AsrHandle {
            playhead: Arc::new(AtomicI64::new(0)),
            cancel: Arc::default(),
        };
        let (playhead, cancel) = (handle.playhead.clone(), handle.cancel.clone());
        let (tx, ctx, store, whisper, closing) = (
            self.tx.clone(),
            self.ctx.clone(),
            self.store.clone(),
            self.whisper.clone(),
            self.closing.clone(),
        );
        std::thread::spawn(move || {
            let generation = params.generation;
            let status = |text: String, running: bool| {
                let _ = tx.send(Msg::AsrStatus {
                    generation,
                    text,
                    running,
                });
                ctx.request_repaint();
            };
            if let Err(e) = run_asr(
                params, &playhead, &cancel, &closing, &store, &whisper, &tx, &ctx, &status,
            ) {
                status(e, false);
            }
        });
        handle
    }
}

#[allow(clippy::too_many_arguments)]
fn run_asr(
    mut p: AsrParams,
    playhead: &AtomicI64,
    cancel: &AtomicBool,
    closing: &AtomicBool,
    store: &Mutex<Store>,
    whisper: &Mutex<Option<WhisperProcess>>,
    tx: &Sender<Msg>,
    ctx: &egui::Context,
    status: &dyn Fn(String, bool),
) -> Result<(), String> {
    let stopped = || cancel.load(Ordering::Relaxed) || closing.load(Ordering::SeqCst);
    if p.done.iter().all(|d| *d) {
        status("Transcript ready".into(), false);
        return Ok(());
    }
    let model_name = p
        .model
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let url = {
        let mut slot = whisper.lock().unwrap_or_else(|e| e.into_inner());
        // Checked under the lock: `shutdown` sets the flag before it empties the slot.
        if closing.load(Ordering::SeqCst) {
            return Ok(());
        }
        if slot.as_mut().is_none_or(|w| w.has_exited()) {
            status("Starting speech recognition…".into(), true);
            let process = WhisperProcess::spawn(&p.server, &p.model, p.vad_model.as_deref())
                .map_err(|e| format!("Cannot start whisper-server: {e}"))?;
            *slot = Some(process);
        }
        slot.as_ref().unwrap().url.clone()
    };
    let engine = WhisperServer::new(&url, &model_name);
    let started = Instant::now();
    while !engine.is_ready() {
        if stopped() {
            return Ok(());
        }
        if started.elapsed() > Duration::from_secs(120) {
            return Err("whisper-server did not become ready; check the model path".into());
        }
        std::thread::sleep(Duration::from_millis(150));
    }

    let total = p.done.len();
    let lang_key = p.lang.clone().unwrap_or_default();
    while let Some(k) = asr_plan::next_window(&p.done, playhead.load(Ordering::Relaxed)) {
        if stopped() {
            return Ok(());
        }
        let finished = p.done.iter().filter(|d| **d).count();
        status(
            format!("Transcribing {}%", finished * 100 / total.max(1)),
            true,
        );
        let (start, len) = asr_plan::audio_range(k);
        let pcm = tools::extract_pcm(&p.ffmpeg, &p.media, p.audio_index, start, len)
            .map_err(|e| e.to_string())?;
        let mut language = None;
        let cues = if pcm.is_empty() {
            Vec::new()
        } else {
            let out = match engine.transcribe(&pcm, p.lang.as_deref()) {
                Ok(out) => out,
                // The server was killed under me on purpose; that is not an error to show.
                Err(_) if stopped() => return Ok(()),
                Err(e) => return Err(e.to_string()),
            };
            language = out.language;
            let segments = out.segments.into_iter().map(|s| Cue {
                start: s.start,
                end: s.end,
                text: s.text,
            });
            asr_plan::accept(k, start, segments)
        };
        p.done[k] = true;
        asr_plan::merge(&mut p.cues, cues.clone());
        let _ = tx.send(Msg::AsrCues {
            generation: p.generation,
            cues,
            window: k,
            language,
        });
        ctx.request_repaint();

        // Saved after every window: the player may be closed at any moment.
        let covered = asr_plan::covered_prefix(&p.done, p.duration);
        let _ = store
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .save_transcript(
                &p.media_key,
                &engine.id(),
                &lang_key,
                covered,
                &p.done,
                &p.cues,
            );
    }
    status("Transcript ready".into(), false);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    struct Fake {
        calls: AtomicUsize,
        fail_first: AtomicUsize,
    }

    impl Translator for Fake {
        fn id(&self) -> String {
            "fake".into()
        }
        fn batch_size(&self) -> usize {
            20
        }
        fn translate(&self, req: &TranslateRequest) -> Result<Vec<String>, AiError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail_first.load(Ordering::SeqCst) > 0 {
                self.fail_first.fetch_sub(1, Ordering::SeqCst);
                return Err(AiError::Http {
                    status: 400,
                    body: "bad".into(),
                });
            }
            Ok(req.lines.iter().map(|l| format!("ru:{l}")).collect())
        }
    }

    /// Remembers every line it was asked to translate.
    #[derive(Default)]
    struct Recorder {
        lines: Mutex<Vec<String>>,
        context: Mutex<Vec<String>>,
    }

    impl Translator for Recorder {
        fn id(&self) -> String {
            "recorder".into()
        }
        fn batch_size(&self) -> usize {
            20
        }
        fn translate(&self, req: &TranslateRequest) -> Result<Vec<String>, AiError> {
            self.lines.lock().unwrap().extend(req.lines.iter().cloned());
            *self.context.lock().unwrap() = req.context_before.to_vec();
            Ok(req
                .lines
                .iter()
                .map(|l| match l.as_str() {
                    "I told him that he didn't do it, she said." => {
                        "Я сказала ему, что он этого не делал, — сказала она.".to_owned()
                    }
                    other => format!("ru:{other}"),
                })
                .collect())
        }
    }

    #[test]
    fn fragments_of_one_sentence_go_out_as_one_line_and_come_back_cut() {
        use crate::session::{Load, Origin, SubTrack};
        let cue = |start: Ms, text: &str| Cue {
            start,
            end: start + 1_000,
            text: text.into(),
        };
        let mut track = SubTrack::new("en".into(), Some("en".into()), Origin::Asr, Load::Loading);
        track.set_cues(vec![
            cue(0, "Hello."),
            cue(1_100, "I told him that"),
            cue(2_200, "he didn't do it,"),
            cue(3_300, "she said."),
        ]);
        let groups: Vec<usize> = (1..track.groups.len()).collect();
        let context: Vec<String> = vec![track.sentence_text(0)];
        let asked = track.request_translations(&groups);
        let lines: Vec<String> = asked.iter().map(|s| s.text.clone()).collect();

        let store = Mutex::new(Store::open_in_memory().unwrap());
        let recorder = Recorder::default();
        let out = translate_cached(&store, &recorder, Some("en"), "ru", &lines, &context).unwrap();
        assert_eq!(
            *recorder.lines.lock().unwrap(),
            ["I told him that he didn't do it, she said."]
        );
        assert_eq!(*recorder.context.lock().unwrap(), ["Hello."]);

        track.apply_translations(&asked, Some(&out.results), track.epoch);
        let whole = "Я сказала ему, что он этого не делал, — сказала она.";
        let parts: Vec<String> = (1..4)
            .map(|i| match &track.translation[i] {
                crate::session::Tr::Done(p) => p.clone(),
                other => panic!("cue {i}: {other:?}"),
            })
            .collect();
        assert!(parts.iter().all(|p| !p.is_empty()), "{parts:?}");
        assert_eq!(parts.join(" "), whole);

        // The sentence is cached as a whole: asking again costs nothing.
        let again = translate_cached(&store, &recorder, Some("en"), "ru", &lines, &[]).unwrap();
        assert_eq!((again.requests, again.results[0].as_str()), (0, whole));
    }

    fn fifty() -> Vec<String> {
        (0..50).map(|i| format!("line {i}")).collect()
    }

    #[test]
    fn second_pass_makes_no_requests() {
        let store = Mutex::new(Store::open_in_memory().unwrap());
        let fake = Fake {
            calls: AtomicUsize::new(0),
            fail_first: AtomicUsize::new(0),
        };
        let lines = fifty();
        let first = translate_cached(&store, &fake, Some("en"), "ru", &lines, &[]).unwrap();
        assert_eq!(first.results.len(), 50);
        assert!(
            first
                .results
                .iter()
                .enumerate()
                .all(|(i, r)| r == &format!("ru:line {i}"))
        );
        assert_eq!(first.requests, 3, "50 lines in batches of 20");
        let second = translate_cached(&store, &fake, Some("en"), "ru", &lines, &[]).unwrap();
        assert_eq!(second.requests, 0);
        assert_eq!(second.results, first.results);
        assert_eq!(fake.calls.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn only_missing_lines_are_requested_and_errors_are_not_cached() {
        let store = Mutex::new(Store::open_in_memory().unwrap());
        let fake = Fake {
            calls: AtomicUsize::new(0),
            fail_first: AtomicUsize::new(1),
        };
        let lines = fifty();
        assert!(translate_cached(&store, &fake, None, "ru", &lines[..10], &[]).is_err());
        let ok = translate_cached(&store, &fake, None, "ru", &lines[..10], &[]).unwrap();
        assert_eq!(ok.requests, 1);
        let more = translate_cached(&store, &fake, None, "ru", &lines[..15], &[]).unwrap();
        assert_eq!(more.requests, 1, "ten cached, five new in one batch");
        assert_eq!(more.results[14], "ru:line 14");
        let other_lang = translate_cached(&store, &fake, None, "de", &lines[..10], &[]).unwrap();
        assert_eq!(other_lang.requests, 1, "cache is per target language");
    }
}
