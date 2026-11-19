//! Background work. The UI thread never touches the network or the disk: it sends a job,
//! keeps drawing, and picks up a `Msg` when the job is done.

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
        items: Vec<(usize, String)>,
    },
    TranslateFailed {
        generation: u64,
        track: usize,
        indices: Vec<usize>,
        error: String,
    },
    AsrCues {
        generation: u64,
        cues: Vec<Cue>,
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
    Anki {
        vocab_id: i64,
        result: Result<i64, String>,
    },
}

pub struct TranslateJob {
    pub generation: u64,
    pub track: usize,
    pub indices: Vec<usize>,
    pub lines: Vec<String>,
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
        // Lines just before the batch give an LLM the context of the dialogue.
        let first = batch[0];
        let mut ctx: Vec<String> = context.to_vec();
        ctx.extend(lines[first.saturating_sub(3)..first].iter().cloned());
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
                let outcome = translate_cached(
                    &worker_store,
                    job.translator.as_ref(),
                    job.src.as_deref(),
                    &job.dst,
                    &job.lines,
                    &job.context,
                );
                let msg = match outcome {
                    Ok(o) => Msg::Translated {
                        generation: job.generation,
                        track: job.track,
                        items: job.indices.into_iter().zip(o.results).collect(),
                    },
                    Err(e) => Msg::TranslateFailed {
                        generation: job.generation,
                        track: job.track,
                        indices: job.indices,
                        error: e.to_string(),
                    },
                };
                let _ = worker_tx.send(msg);
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
        }
    }

    fn spawn(&self, work: impl FnOnce() -> Msg + Send + 'static) {
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
        let (tx, ctx, store, whisper) = (
            self.tx.clone(),
            self.ctx.clone(),
            self.store.clone(),
            self.whisper.clone(),
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
                params, &playhead, &cancel, &store, &whisper, &tx, &ctx, &status,
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
    store: &Mutex<Store>,
    whisper: &Mutex<Option<WhisperProcess>>,
    tx: &Sender<Msg>,
    ctx: &egui::Context,
    status: &dyn Fn(String, bool),
) -> Result<(), String> {
    let model_name = p
        .model
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let url = {
        let mut slot = whisper.lock().unwrap();
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
        if cancel.load(Ordering::Relaxed) {
            return Ok(());
        }
        if started.elapsed() > Duration::from_secs(120) {
            return Err("whisper-server did not become ready; check the model path".into());
        }
        std::thread::sleep(Duration::from_millis(150));
    }

    let total = p.done.len();
    let lang_key = p.lang.clone().unwrap_or_default();
    let mut last_save = Instant::now();
    while let Some(k) = asr_plan::next_window(&p.done, playhead.load(Ordering::Relaxed)) {
        if cancel.load(Ordering::Relaxed) {
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
            let out = engine
                .transcribe(&pcm, p.lang.as_deref())
                .map_err(|e| e.to_string())?;
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
            language,
        });
        ctx.request_repaint();

        let all_done = p.done.iter().all(|d| *d);
        if all_done || last_save.elapsed() > Duration::from_secs(20) {
            let covered = asr_plan::covered_prefix(&p.done, p.duration);
            let prefix: Vec<Cue> = p
                .cues
                .iter()
                .filter(|c| c.start < covered)
                .cloned()
                .collect();
            let _ = store.lock().unwrap().save_transcript(
                &p.media_key,
                &engine.id(),
                &lang_key,
                covered,
                &prefix,
            );
            last_save = Instant::now();
        }
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
