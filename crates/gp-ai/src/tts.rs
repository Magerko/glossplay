//! Speech synthesis through an OpenAI-style `POST /v1/audio/speech` server, in the dialect of
//! audio.cpp: a `language` field and `options.duration_sec` for the length of the result.

use crate::{AiError, http};
use gp_core::Ms;
use serde_json::{Map, Value, json};
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SpeakOptions {
    /// Ask for a clip of about this length.
    pub target_ms: Option<Ms>,
    pub speed: Option<f32>,
    /// The same seed gives the same clip for the same text and voice.
    pub seed: Option<u32>,
    /// A voice to copy (audio.cpp `voice_ref`); without one OmniVoice makes up a speaker for
    /// every request.
    pub reference: Option<Arc<Reference>>,
    /// A described voice ("male, middle-aged") for when there is nothing to copy.
    pub instruct: Option<String>,
}

/// A sample of a voice: a WAV file and the words said in it, which the engine needs too.
#[derive(Debug, Clone, PartialEq)]
pub struct Reference {
    base64: String,
    pub text: String,
}

impl Reference {
    pub fn new(wav: &[u8], text: &str) -> Reference {
        Reference {
            base64: base64(wav),
            text: text.trim().to_owned(),
        }
    }
}

fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for k in 0..4 {
            if k <= chunk.len() {
                out.push(ABC[(n >> (18 - 6 * k) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// A synthesized clip: the WAV file as it came, and what its header says.
#[derive(Debug, Clone, PartialEq)]
pub struct Wav {
    pub bytes: Vec<u8>,
    pub sample_rate: u32,
    pub duration_ms: Ms,
}

pub trait TtsEngine: Send + Sync {
    /// Names the engine, model and voice, so cached clips of another one are not reused.
    fn id(&self) -> String;
    fn synthesize(&self, text: &str, lang: &str, opts: &SpeakOptions) -> Result<Wav, AiError>;
}

pub struct SpeechServer {
    base_url: String,
    model: String,
    voice: String,
}

impl SpeechServer {
    /// `base_url` may end in `/v1` the way OpenAI clients are configured, or not.
    pub fn new(base_url: &str, model: &str, voice: &str) -> SpeechServer {
        let base = base_url.trim().trim_end_matches('/');
        SpeechServer {
            base_url: base.strip_suffix("/v1").unwrap_or(base).to_owned(),
            model: model.trim().to_owned(),
            voice: voice.trim().to_owned(),
        }
    }

    pub fn is_ready(&self) -> bool {
        http::get(&format!("{}/health", self.base_url)).is_ok()
    }

    fn request(&self, text: &str, lang: &str, opts: &SpeakOptions) -> Value {
        let mut body = Map::new();
        if !self.model.is_empty() {
            body.insert("model".into(), json!(self.model));
        }
        body.insert("input".into(), json!(text));
        body.insert("response_format".into(), json!("wav"));
        let code = gp_core::lang::code_for(gp_core::lang::Service::Speech, lang);
        if !code.is_empty() {
            body.insert("language".into(), json!(code));
        }
        if !self.voice.is_empty() {
            body.insert("voice".into(), json!(self.voice));
        }
        if let Some(speed) = opts.speed.filter(|s| (*s - 1.0).abs() > f32::EPSILON) {
            body.insert("speed".into(), json!(speed));
        }
        if let Some(seed) = opts.seed {
            body.insert("seed".into(), json!(seed));
        }
        if let Some(r) = &opts.reference {
            body.insert(
                "voice_ref".into(),
                json!({ "type": "base64", "data": r.base64 }),
            );
            body.insert("reference_text".into(), json!(r.text));
        }
        if let Some(instruct) = &opts.instruct {
            body.insert("instruct".into(), json!(instruct));
        }
        if let Some(ms) = opts.target_ms {
            // Only inside `options` does audio.cpp take it; at the top level it is ignored.
            body.insert(
                "options".into(),
                json!({ "duration_sec": (ms.max(1) as f64 / 1000.0) }),
            );
        }
        Value::Object(body)
    }
}

impl TtsEngine for SpeechServer {
    fn id(&self) -> String {
        format!("{}|{}|{}", self.base_url, self.model, self.voice)
    }

    fn synthesize(&self, text: &str, lang: &str, opts: &SpeakOptions) -> Result<Wav, AiError> {
        let url = format!("{}/v1/audio/speech", self.base_url);
        let bytes = http::post_json_for_bytes(&url, &self.request(text, lang, opts))?;
        parse_wav(bytes)
    }
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// Reads the length of a PCM WAV file from its header.
pub fn parse_wav(bytes: Vec<u8>) -> Result<Wav, AiError> {
    let bad = |why: &str| AiError::BadResponse(format!("not a WAV file: {why}"));
    if bytes.get(0..4) != Some(b"RIFF") || bytes.get(8..12) != Some(b"WAVE") {
        let head = String::from_utf8_lossy(&bytes[..bytes.len().min(120)]).into_owned();
        return Err(bad(&head));
    }
    let (mut at, mut format) = (12, None);
    while let (Some(id), Some(len)) = (bytes.get(at..at + 4), u32_at(&bytes, at + 4)) {
        let body = at + 8;
        match id {
            b"fmt " => {
                let rate = u32_at(&bytes, body + 4).ok_or_else(|| bad("short fmt chunk"))?;
                let byte_rate = u32_at(&bytes, body + 8).ok_or_else(|| bad("short fmt chunk"))?;
                let channels = u16_at(&bytes, body + 2).unwrap_or(1);
                format = Some((rate, byte_rate, channels));
            }
            b"data" => {
                let (rate, byte_rate, _) = format.ok_or_else(|| bad("data before fmt"))?;
                if byte_rate == 0 || rate == 0 {
                    return Err(bad("zero sample rate"));
                }
                // Streaming writers leave the size at its maximum; the file says how much is there.
                let len = (len as usize).min(bytes.len().saturating_sub(body));
                return Ok(Wav {
                    duration_ms: (len as u64 * 1000 / u64::from(byte_rate)) as Ms,
                    sample_rate: rate,
                    bytes,
                });
            }
            _ => {}
        }
        at = body + len as usize + (len as usize & 1);
    }
    Err(bad("no data chunk"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testserver::{Reply, TestServer};

    /// A mono 16-bit WAV of `ms` milliseconds of silence, with an extra chunk before the data
    /// the way some writers add one.
    fn wav(rate: u32, ms: u32) -> Vec<u8> {
        let data_len = rate * 2 * ms / 1000;
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(4 + 24 + 14 + 8 + data_len).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * 2).to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"LIST");
        out.extend_from_slice(&5u32.to_le_bytes());
        out.extend_from_slice(b"INFO!\0");
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_len.to_le_bytes());
        out.resize(out.len() + data_len as usize, 0);
        out
    }

    fn wav_reply(bytes: Vec<u8>) -> Reply {
        Reply {
            status: 200,
            body: bytes,
            headers: "Content-Type: audio/wav\r\nx-audiocpp-audio-duration-ms: 1250.000000\r\n"
                .into(),
        }
    }

    #[test]
    fn a_request_in_the_audio_cpp_dialect_and_the_length_of_the_answer() {
        let srv = TestServer::start_replies(vec![wav_reply(wav(24_000, 1_250))]);
        let tts = SpeechServer::new(&format!("{}/v1/", srv.url), "omnivoice", "");
        let out = tts
            .synthesize(
                "Птицы с перепачканными перьями,",
                "ru-RU",
                &SpeakOptions {
                    target_ms: Some(2_300),
                    ..SpeakOptions::default()
                },
            )
            .unwrap();
        assert_eq!((out.sample_rate, out.duration_ms), (24_000, 1_250));
        assert_eq!(&out.bytes[..4], b"RIFF");
        let got = srv.received.lock().unwrap();
        assert_eq!(got[0].path, "/v1/audio/speech");
        assert!(got[0].headers.to_lowercase().contains("application/json"));
        let body: Value = serde_json::from_str(&got[0].body).unwrap();
        assert_eq!(
            body,
            json!({
                "model": "omnivoice",
                "input": "Птицы с перепачканными перьями,",
                "response_format": "wav",
                "language": "ru",
                "options": { "duration_sec": 2.3 }
            })
        );
    }

    #[test]
    fn natural_length_sends_no_duration_and_a_voice_when_set() {
        let srv = TestServer::start_replies(vec![wav_reply(wav(16_000, 500))]);
        let tts = SpeechServer::new(&srv.url, "", "anna");
        let out = tts
            .synthesize(
                "Да, конечно.",
                "uk",
                &SpeakOptions {
                    speed: Some(1.25),
                    ..SpeakOptions::default()
                },
            )
            .unwrap();
        assert_eq!(out.duration_ms, 500);
        let body: Value = serde_json::from_str(&srv.received.lock().unwrap()[0].body).unwrap();
        assert_eq!(
            body,
            json!({"input": "Да, конечно.", "response_format": "wav", "language": "uk",
                   "voice": "anna", "speed": 1.25})
        );
        assert_ne!(tts.id(), SpeechServer::new(&srv.url, "", "boris").id());
    }

    #[test]
    fn a_voice_to_copy_goes_with_its_words_and_a_seed() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar\xff"), "Zm9vYmFy/w==");
        let srv = TestServer::start_replies(vec![wav_reply(wav(24_000, 800))]);
        let tts = SpeechServer::new(&srv.url, "omnivoice", "");
        let opts = SpeakOptions {
            seed: Some(1),
            reference: Some(Arc::new(Reference::new(b"RIFF", " Hello there. "))),
            ..SpeakOptions::default()
        };
        tts.synthesize("Привет.", "ru", &opts).unwrap();
        let body: Value = serde_json::from_str(&srv.received.lock().unwrap()[0].body).unwrap();
        assert_eq!(
            body,
            json!({"model": "omnivoice", "input": "Привет.", "response_format": "wav",
                   "language": "ru", "seed": 1,
                   "voice_ref": {"type": "base64", "data": "UklGRg=="},
                   "reference_text": "Hello there."})
        );
    }

    #[test]
    fn server_errors_and_bodies_that_are_not_wav() {
        let err = r#"{"error":{"message":"unsupported OmniVoice language 'ru-RU'"}}"#;
        let srv = TestServer::start_replies(vec![
            Reply {
                status: 500,
                body: err.as_bytes().to_vec(),
                headers: "Content-Type: application/json\r\n".into(),
            },
            Reply {
                status: 200,
                body: br#"{"audio":"..."}"#.to_vec(),
                headers: "Content-Type: application/json\r\n".into(),
            },
        ]);
        let tts = SpeechServer::new(&srv.url, "omnivoice", "");
        let e = tts
            .synthesize("x", "ru", &SpeakOptions::default())
            .unwrap_err();
        assert!(
            matches!(&e, AiError::Http { status: 500, body } if body.contains("unsupported")),
            "{e:?}"
        );
        assert!(
            e.is_transient(),
            "a 500 from a crashed model is worth one more try"
        );
        let e = tts
            .synthesize("x", "ru", &SpeakOptions::default())
            .unwrap_err();
        assert!(matches!(e, AiError::BadResponse(_)), "{e:?}");
    }

    #[test]
    fn wav_headers() {
        let w = parse_wav(wav(24_000, 3_810)).unwrap();
        assert_eq!((w.sample_rate, w.duration_ms), (24_000, 3_810));
        // A streaming writer's "unknown" size.
        let mut streamed = wav(24_000, 1_000);
        let data = streamed.len() - 48_000 - 4;
        streamed[data..data + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(parse_wav(streamed).unwrap().duration_ms, 1_000);
        assert!(parse_wav(b"RIFF\0\0\0\0WAVE".to_vec()).is_err());
        assert!(parse_wav(b"<html>".to_vec()).is_err());
        assert!(parse_wav(Vec::new()).is_err());
    }
}
