use crate::{AiError, http};
use lp_core::Ms;
use serde_json::Value;

pub const SAMPLE_RATE: u32 = 16_000;

#[derive(Debug, Clone, PartialEq)]
pub struct AsrSegment {
    /// Relative to the start of the submitted audio.
    pub start: Ms,
    pub end: Ms,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct AsrOutput {
    pub language: Option<String>,
    pub segments: Vec<AsrSegment>,
}

pub trait AsrEngine: Send + Sync {
    fn id(&self) -> String;
    /// `pcm` is 16 kHz mono.
    fn transcribe(&self, pcm: &[i16], lang: Option<&str>) -> Result<AsrOutput, AiError>;
}

/// Client for whisper.cpp's `whisper-server`.
pub struct WhisperServer {
    base_url: String,
    model_name: String,
}

impl WhisperServer {
    pub fn new(base_url: &str, model_name: &str) -> Self {
        WhisperServer {
            base_url: base_url.trim_end_matches('/').to_owned(),
            model_name: model_name.to_owned(),
        }
    }

    pub fn is_ready(&self) -> bool {
        // The server answers 503 while the model is loading, which `get` reports as an error.
        http::get(&format!("{}/health", self.base_url)).is_ok()
    }
}

impl AsrEngine for WhisperServer {
    fn id(&self) -> String {
        format!("whisper.cpp:{}", self.model_name)
    }

    fn transcribe(&self, pcm: &[i16], lang: Option<&str>) -> Result<AsrOutput, AiError> {
        let boundary = "----linguaplay-boundary-7d9f2c";
        let mut body = Vec::with_capacity(pcm.len() * 2 + 1024);
        let fields = [
            ("response_format", "verbose_json"),
            ("language", lang.filter(|l| !l.is_empty()).unwrap_or("auto")),
            ("temperature", "0.0"),
        ];
        for (name, value) in fields {
            body.extend_from_slice(
                format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n")
                    .as_bytes(),
            );
        }
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.wav\"\r\n\
                 Content-Type: audio/wav\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(&wav_bytes(pcm));
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

        let v = http::post_bytes(
            &format!("{}/inference", self.base_url),
            &format!("multipart/form-data; boundary={boundary}"),
            &body,
        )?;
        parse_verbose_json(&v)
    }
}

fn parse_verbose_json(v: &Value) -> Result<AsrOutput, AiError> {
    let raw = v["segments"]
        .as_array()
        .ok_or_else(|| AiError::BadResponse(v.to_string().chars().take(200).collect()))?;
    let segments = raw
        .iter()
        .filter_map(|s| {
            let text = s["text"].as_str()?.trim();
            let (start, end) = (to_ms(&s["start"])?, to_ms(&s["end"])?);
            (!text.is_empty() && end > start).then(|| AsrSegment {
                start,
                end,
                text: text.to_owned(),
            })
        })
        .collect();
    // The server reports the language by its English name; the probability map is keyed by code.
    let language = v["language_probabilities"].as_object().and_then(|probs| {
        probs
            .iter()
            .filter_map(|(code, p)| Some((code, p.as_f64()?)))
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(code, _)| code.clone())
    });
    Ok(AsrOutput { language, segments })
}

fn to_ms(v: &Value) -> Option<Ms> {
    Some((v.as_f64()? * 1000.0).round() as Ms)
}

pub fn wav_bytes(pcm: &[i16]) -> Vec<u8> {
    let data_len = (pcm.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + pcm.len() * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    out.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in pcm {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testserver::TestServer;

    #[test]
    fn transcribe_posts_wav_and_parses_segments() {
        let reply = r#"{"task":"transcribe","language":"english","duration":10.0,
            "segments":[{"id":0,"text":" Hello there.","start":0.0,"end":1.5},
                        {"id":1,"text":"  ","start":1.5,"end":2.0},
                        {"id":2,"text":" General Kenobi.","start":2.25,"end":4.004}],
            "language_probabilities":{"en":0.97,"de":0.02}}"#;
        let srv = TestServer::start(vec![(200, reply.into())]);
        let engine = WhisperServer::new(&srv.url, "large-v3-turbo");
        assert_eq!(engine.id(), "whisper.cpp:large-v3-turbo");
        let out = engine.transcribe(&[0i16; 1600], None).unwrap();
        assert_eq!(out.language.as_deref(), Some("en"));
        assert_eq!(
            out.segments,
            vec![
                AsrSegment {
                    start: 0,
                    end: 1500,
                    text: "Hello there.".into()
                },
                AsrSegment {
                    start: 2250,
                    end: 4004,
                    text: "General Kenobi.".into()
                },
            ]
        );
        let got = srv.received.lock().unwrap();
        assert_eq!(got[0].path, "/inference");
        assert!(
            got[0]
                .headers
                .to_lowercase()
                .contains("multipart/form-data; boundary=")
        );
        assert!(got[0].body.contains("name=\"language\"\r\n\r\nauto"));
        assert!(got[0].body.contains("verbose_json") && got[0].body.contains("RIFF"));
    }

    #[test]
    fn wav_header_is_valid() {
        let wav = wav_bytes(&[1, -1, 256]);
        assert_eq!(wav.len(), 44 + 6);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(wav[4..8].try_into().unwrap()), 36 + 6);
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 16_000);
        assert_eq!(&wav[44..50], &[1, 0, 255, 255, 0, 1]);
    }
}
