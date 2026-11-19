use crate::{AiError, http};
use lp_core::config::Translation as TranslationConfig;
use serde_json::{Value, json};

pub struct TranslateRequest<'a> {
    pub src_lang: Option<&'a str>,
    pub dst_lang: &'a str,
    pub lines: &'a [String],
    /// Earlier lines of the original, for LLM providers only.
    pub context_before: &'a [String],
}

pub trait Translator: Send + Sync {
    /// Stable id used as the cache key, e.g. "google" or "openai:gpt-x".
    fn id(&self) -> String;
    fn batch_size(&self) -> usize;
    /// Returns exactly `lines.len()` translations in order.
    fn translate(&self, req: &TranslateRequest) -> Result<Vec<String>, AiError>;
    /// Explains a word in the context of its sentence. LLM providers only.
    fn explain(&self, _word: &str, _sentence: &str, _ui_lang: &str) -> Result<String, AiError> {
        Err(AiError::Unsupported)
    }
}

pub const ENV_API_KEY: &str = "LINGUAPLAY_API_KEY";

pub fn build_translator(cfg: &TranslationConfig) -> Result<Box<dyn Translator>, AiError> {
    let key = std::env::var(ENV_API_KEY)
        .ok()
        .filter(|k| !k.is_empty())
        .unwrap_or(cfg.api_key.clone());
    let need_key = |provider: &str| {
        if key.is_empty() {
            Err(AiError::MissingKey {
                provider: provider.to_owned(),
            })
        } else {
            Ok(key.clone())
        }
    };
    let or = |value: &str, default: &str| {
        if value.is_empty() {
            default.to_owned()
        } else {
            value.trim_end_matches('/').to_owned()
        }
    };
    match cfg.provider.as_str() {
        "google" => Ok(Box::new(Google {
            base_url: or(&cfg.base_url, "https://translate.googleapis.com"),
        })),
        "deepl" => {
            let key = need_key("deepl")?;
            // Free-plan keys end with ":fx" and live on a separate host.
            let host = if key.ends_with(":fx") {
                "https://api-free.deepl.com"
            } else {
                "https://api.deepl.com"
            };
            Ok(Box::new(DeepL {
                base_url: or(&cfg.base_url, host),
                key,
            }))
        }
        "openai" => {
            if cfg.model.is_empty() {
                return Err(AiError::Config(
                    "set translation.model for the openai provider".into(),
                ));
            }
            let local = !cfg.base_url.is_empty();
            Ok(Box::new(OpenAiCompat {
                base_url: or(&cfg.base_url, "https://api.openai.com/v1"),
                // Local servers (Ollama, LM Studio) need no key.
                key: if local {
                    key.clone()
                } else {
                    need_key("openai")?
                },
                model: cfg.model.clone(),
            }))
        }
        "anthropic" => Ok(Box::new(Anthropic {
            base_url: or(&cfg.base_url, "https://api.anthropic.com"),
            key: need_key("anthropic")?,
            model: or(&cfg.model, "claude-opus-5-5"),
        })),
        other => Err(AiError::Config(format!(
            "unknown translation provider \"{other}\""
        ))),
    }
}

/// Translates a batch; if the provider returns the wrong number of lines, falls back to
/// one request per line so a single odd reply never loses the whole batch.
pub fn translate_resilient(
    t: &dyn Translator,
    req: &TranslateRequest,
) -> Result<Vec<String>, AiError> {
    match t.translate(req) {
        Err(AiError::BadResponse(_)) if req.lines.len() > 1 => req
            .lines
            .iter()
            .map(|line| {
                let single = TranslateRequest {
                    lines: std::slice::from_ref(line),
                    ..*req
                };
                Ok(t.translate(&single)?.remove(0))
            })
            .collect(),
        other => other,
    }
}

fn check_len(out: Vec<String>, expected: usize) -> Result<Vec<String>, AiError> {
    if out.len() == expected {
        Ok(out)
    } else {
        Err(AiError::BadResponse(format!(
            "expected {expected} lines, got {}",
            out.len()
        )))
    }
}

pub struct Google {
    base_url: String,
}

impl Translator for Google {
    fn id(&self) -> String {
        "google".into()
    }

    fn batch_size(&self) -> usize {
        25
    }

    fn translate(&self, req: &TranslateRequest) -> Result<Vec<String>, AiError> {
        // One line per cue: the endpoint keeps newlines, which lets me send a batch in one call.
        let q = req
            .lines
            .iter()
            .map(|l| l.replace('\n', " "))
            .collect::<Vec<_>>()
            .join("\n");
        let url = format!("{}/translate_a/single", self.base_url);
        let form = [
            ("client", "gtx"),
            ("sl", req.src_lang.unwrap_or("auto")),
            ("tl", req.dst_lang),
            ("dt", "t"),
            ("q", q.as_str()),
        ];
        let v = http::post_form(&url, &form)?;
        let chunks = v
            .get(0)
            .and_then(Value::as_array)
            .ok_or_else(|| bad("google", &v))?;
        let text: String = chunks
            .iter()
            .filter_map(|c| c.get(0).and_then(Value::as_str))
            .collect();
        check_len(
            text.split('\n').map(|s| s.trim().to_owned()).collect(),
            req.lines.len(),
        )
    }
}

pub struct DeepL {
    base_url: String,
    key: String,
}

impl Translator for DeepL {
    fn id(&self) -> String {
        "deepl".into()
    }

    fn batch_size(&self) -> usize {
        40
    }

    fn translate(&self, req: &TranslateRequest) -> Result<Vec<String>, AiError> {
        let mut body = json!({ "text": req.lines, "target_lang": req.dst_lang.to_uppercase() });
        if let Some(src) = req.src_lang {
            body["source_lang"] = json!(src.to_uppercase());
        }
        let auth = format!("DeepL-Auth-Key {}", self.key);
        let v = http::post_json(
            &format!("{}/v2/translate", self.base_url),
            &[("Authorization", &auth)],
            &body,
        )?;
        let items = v["translations"]
            .as_array()
            .ok_or_else(|| bad("deepl", &v))?;
        check_len(
            items
                .iter()
                .filter_map(|t| t["text"].as_str().map(str::to_owned))
                .collect(),
            req.lines.len(),
        )
    }
}

fn bad(provider: &str, v: &Value) -> AiError {
    AiError::BadResponse(format!(
        "{provider}: {}",
        v.to_string().chars().take(200).collect::<String>()
    ))
}

fn llm_translate_prompt(req: &TranslateRequest) -> (String, String) {
    let src = req
        .src_lang
        .map_or("the source language".to_owned(), |l| format!("\"{l}\""));
    let system = format!(
        "You translate film and video subtitles from {src} into \"{dst}\" (language codes are BCP-47). \
         Each input line is one subtitle cue. Translate the way people actually speak: natural, \
         concise, keeping tone, humour and register; keep names as they are usually rendered in the \
         target language. A cue may be a fragment of a sentence that continues in the next cue — \
         translate it as a fragment so the cues still line up. Never merge, split, drop or reorder cues.\n\
         Reply with a JSON array of exactly {n} strings and nothing else.",
        dst = req.dst_lang,
        n = req.lines.len(),
    );
    let mut user = String::new();
    if !req.context_before.is_empty() {
        user.push_str("Preceding cues, for context only (do not translate):\n");
        for line in req.context_before {
            user.push_str(&format!("- {}\n", line.replace('\n', " ")));
        }
        user.push('\n');
    }
    let lines: Vec<String> = req.lines.iter().map(|l| l.replace('\n', " ")).collect();
    user.push_str("Cues to translate, as a JSON array:\n");
    user.push_str(&serde_json::to_string(&lines).expect("strings serialize"));
    (system, user)
}

fn llm_explain_prompt(word: &str, sentence: &str, ui_lang: &str) -> (String, String) {
    let system = format!(
        "You help a language learner who is watching a video. Explain the given word as it is used \
         in the given sentence. Answer in the language with BCP-47 code \"{ui_lang}\", in plain text \
         without markdown, in at most 60 words: first the meaning in this context, then the \
         dictionary form with part of speech, then one short note on usage or nuance if there is \
         anything a learner would likely miss."
    );
    (system, format!("Word: {word}\nSentence: {sentence}"))
}

/// LLMs sometimes wrap the array in prose or a code fence; I take the outermost brackets.
fn parse_json_array(text: &str, expected: usize) -> Result<Vec<String>, AiError> {
    let slice = match (text.find('['), text.rfind(']')) {
        (Some(a), Some(b)) if b > a => &text[a..=b],
        _ => {
            return Err(AiError::BadResponse(format!(
                "no JSON array in: {}",
                snippet(text)
            )));
        }
    };
    let out: Vec<String> = serde_json::from_str(slice)
        .map_err(|e| AiError::BadResponse(format!("{e}: {}", snippet(text))))?;
    check_len(out, expected)
}

fn snippet(text: &str) -> String {
    text.chars().take(200).collect()
}

pub struct OpenAiCompat {
    base_url: String,
    key: String,
    model: String,
}

impl OpenAiCompat {
    fn chat(&self, system: String, user: String) -> Result<String, AiError> {
        let body = json!({
            "model": self.model,
            "messages": [
                { "role": "system", "content": system },
                { "role": "user", "content": user },
            ],
        });
        let auth = format!("Bearer {}", self.key);
        let headers: &[(&str, &str)] = if self.key.is_empty() {
            &[]
        } else {
            &[("Authorization", &auth)]
        };
        let v = http::post_json(
            &format!("{}/chat/completions", self.base_url),
            headers,
            &body,
        )?;
        v["choices"][0]["message"]["content"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| bad("openai", &v))
    }
}

impl Translator for OpenAiCompat {
    fn id(&self) -> String {
        format!("openai:{}", self.model)
    }

    fn batch_size(&self) -> usize {
        20
    }

    fn translate(&self, req: &TranslateRequest) -> Result<Vec<String>, AiError> {
        let (system, user) = llm_translate_prompt(req);
        parse_json_array(&self.chat(system, user)?, req.lines.len())
    }

    fn explain(&self, word: &str, sentence: &str, ui_lang: &str) -> Result<String, AiError> {
        let (system, user) = llm_explain_prompt(word, sentence, ui_lang);
        Ok(self.chat(system, user)?.trim().to_owned())
    }
}

pub struct Anthropic {
    base_url: String,
    key: String,
    model: String,
}

impl Anthropic {
    fn message(&self, system: String, user: String) -> Result<String, AiError> {
        let mut body = json!({
            "model": self.model,
            "max_tokens": 16000,
            "system": system,
            "messages": [{ "role": "user", "content": user }],
        });
        let mut headers = vec![
            ("x-api-key", self.key.as_str()),
            ("anthropic-version", "2026-10-21"),
        ];
        // Subtitle lines are short and simple, so low effort keeps latency and cost down.
        // Models before the 4.6 generation reject the effort field.
        if !self.model.contains("-4-5") {
            body["output_config"] = json!({ "effort": "low" });
        }
        // Film dialogue occasionally trips a safety classifier; a server-side fallback lets
        // another model answer instead of leaving a hole in the subtitles.
        if [
            "claude-opus-5",
            "claude-fable-5-1",
            "claude-fable-5-2",
            "claude-sonnet-5-5",
        ]
        .iter()
        .any(|p| self.model.starts_with(p))
        {
            body["fallbacks"] = json!("default");
            headers.push(("anthropic-beta", "server-side-fallback-2026-07-01"));
        }
        let v = http::post_json(&format!("{}/v1/messages", self.base_url), &headers, &body)?;
        if v["stop_reason"] == "refusal" {
            return Err(AiError::BadResponse(
                "the model declined this request".into(),
            ));
        }
        let text: String = v["content"]
            .as_array()
            .ok_or_else(|| bad("anthropic", &v))?
            .iter()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .collect();
        Ok(text)
    }
}

impl Translator for Anthropic {
    fn id(&self) -> String {
        format!("anthropic:{}", self.model)
    }

    fn batch_size(&self) -> usize {
        20
    }

    fn translate(&self, req: &TranslateRequest) -> Result<Vec<String>, AiError> {
        let (system, user) = llm_translate_prompt(req);
        parse_json_array(&self.message(system, user)?, req.lines.len())
    }

    fn explain(&self, word: &str, sentence: &str, ui_lang: &str) -> Result<String, AiError> {
        let (system, user) = llm_explain_prompt(word, sentence, ui_lang);
        Ok(self.message(system, user)?.trim().to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testserver::TestServer;

    fn cfg(provider: &str, url: &str) -> TranslationConfig {
        TranslationConfig {
            provider: provider.into(),
            base_url: url.into(),
            api_key: "secret-key".into(),
            model: "m1".into(),
            ..Default::default()
        }
    }

    fn lines(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn req<'a>(lines: &'a [String]) -> TranslateRequest<'a> {
        TranslateRequest {
            src_lang: Some("en"),
            dst_lang: "ru",
            lines,
            context_before: &[],
        }
    }

    #[test]
    fn bad_settings_are_config_errors() {
        let e = build_translator(&cfg("nope", "")).err().unwrap();
        assert!(matches!(e, AiError::Config(_)) && !e.is_transient());
        assert_eq!(e.to_string(), "unknown translation provider \"nope\"");
        let mut no_model = cfg("openai", "");
        no_model.model.clear();
        assert!(matches!(
            build_translator(&no_model).err(),
            Some(AiError::Config(_))
        ));
    }

    #[test]
    fn google_joins_segments_and_splits_by_line() {
        let reply = r#"[[["Привет\n","Hello\n",null,null,1],["Как ","How ",null,null,1],["дела?","are you?",null,null,1]],null,"en"]"#;
        let srv = TestServer::start(vec![(200, reply.into())]);
        let t = build_translator(&cfg("google", &srv.url)).unwrap();
        let input = lines(&["Hello", "How are\nyou?"]);
        assert_eq!(
            t.translate(&req(&input)).unwrap(),
            lines(&["Привет", "Как дела?"])
        );
        let got = srv.received.lock().unwrap();
        assert_eq!(got[0].path, "/translate_a/single");
        assert!(got[0].body.contains("sl=en") && got[0].body.contains("tl=ru"));
        assert!(
            got[0].body.contains("q=Hello%0AHow+are+you%3F"),
            "{}",
            got[0].body
        );
    }

    #[test]
    fn deepl_sends_key_and_uppercase_langs() {
        let reply = r#"{"translations":[{"detected_source_language":"EN","text":"Привет"}]}"#;
        let srv = TestServer::start(vec![(200, reply.into())]);
        let t = build_translator(&cfg("deepl", &srv.url)).unwrap();
        let input = lines(&["Hello"]);
        assert_eq!(t.translate(&req(&input)).unwrap(), lines(&["Привет"]));
        let got = srv.received.lock().unwrap();
        assert!(
            got[0]
                .headers
                .to_lowercase()
                .contains("authorization: deepl-auth-key secret-key")
        );
        let body: Value = serde_json::from_str(&got[0].body).unwrap();
        assert_eq!(
            body,
            json!({ "text": ["Hello"], "target_lang": "RU", "source_lang": "EN" })
        );
    }

    #[test]
    fn openai_compatible_parses_fenced_array_and_explains() {
        let chat = |content: &str| {
            json!({ "choices": [{ "message": { "content": content } }] }).to_string()
        };
        let srv = TestServer::start(vec![
            (200, chat("```json\n[\"Привет\", \"Пока\"]\n```")),
            (200, chat(" значение слова ")),
        ]);
        let t = build_translator(&cfg("openai", &srv.url)).unwrap();
        assert_eq!(t.id(), "openai:m1");
        let input = lines(&["Hello", "Bye"]);
        let ctx = lines(&["Earlier line"]);
        let r = TranslateRequest {
            context_before: &ctx,
            ..req(&input)
        };
        assert_eq!(t.translate(&r).unwrap(), lines(&["Привет", "Пока"]));
        assert_eq!(
            t.explain("Hello", "Hello there", "ru").unwrap(),
            "значение слова"
        );
        let got = srv.received.lock().unwrap();
        assert_eq!(got[0].path, "/chat/completions");
        assert!(got[0].body.contains("Earlier line") && got[0].body.contains("exactly 2 strings"));
        assert!(
            got[0]
                .headers
                .to_lowercase()
                .contains("authorization: bearer secret-key")
        );
    }

    #[test]
    fn anthropic_request_shape_and_refusal() {
        let ok = json!({ "stop_reason": "end_turn", "content": [
            { "type": "thinking", "thinking": "" },
            { "type": "text", "text": "[\"Привет\"]" } ] })
        .to_string();
        let refusal = json!({ "stop_reason": "refusal", "content": [] }).to_string();
        let srv = TestServer::start(vec![(200, ok), (200, refusal)]);
        let mut c = cfg("anthropic", &srv.url);
        c.model = String::new();
        let t = build_translator(&c).unwrap();
        assert_eq!(t.id(), "anthropic:claude-opus-5-5");
        let input = lines(&["Hello"]);
        assert_eq!(t.translate(&req(&input)).unwrap(), lines(&["Привет"]));
        assert!(matches!(
            t.translate(&req(&input)),
            Err(AiError::BadResponse(_))
        ));
        let got = srv.received.lock().unwrap();
        assert_eq!(got[0].path, "/v1/messages");
        let h = got[0].headers.to_lowercase();
        assert!(h.contains("x-api-key: secret-key") && h.contains("anthropic-version: 2026-10-21"));
        let body: Value = serde_json::from_str(&got[0].body).unwrap();
        assert_eq!(body["output_config"]["effort"], "low");
        assert_eq!(body["fallbacks"], "default");
        assert!(body.get("temperature").is_none() && body.get("thinking").is_none());
    }

    #[test]
    fn wrong_line_count_falls_back_to_single_lines() {
        let chat = |content: &str| {
            json!({ "choices": [{ "message": { "content": content } }] }).to_string()
        };
        let srv = TestServer::start(vec![
            (200, chat("[\"only one\"]")),
            (200, chat("[\"A\"]")),
            (200, chat("[\"B\"]")),
        ]);
        let t = build_translator(&cfg("openai", &srv.url)).unwrap();
        let input = lines(&["a", "b"]);
        assert_eq!(
            translate_resilient(t.as_ref(), &req(&input)).unwrap(),
            lines(&["A", "B"])
        );
        assert_eq!(srv.count(), 3);
    }

    #[test]
    fn errors_are_classified() {
        let srv = TestServer::start(vec![
            (429, r#"{"error":"slow down"}"#.into()),
            (401, "{}".into()),
        ]);
        let t = build_translator(&cfg("deepl", &srv.url)).unwrap();
        let input = lines(&["x"]);
        let e = t.translate(&req(&input)).unwrap_err();
        assert!(e.is_transient(), "{e:?}");
        let e = t.translate(&req(&input)).unwrap_err();
        assert!(matches!(e, AiError::Http { status: 401, .. }) && !e.is_transient());

        let dead = cfg("google", "http://127.0.0.1:1");
        let e = build_translator(&dead)
            .unwrap()
            .translate(&req(&input))
            .unwrap_err();
        assert!(matches!(e, AiError::Network(_)) && e.is_transient());

        let mut no_key = cfg("anthropic", "");
        no_key.api_key = String::new();
        if std::env::var(ENV_API_KEY).is_err() {
            assert!(matches!(
                build_translator(&no_key),
                Err(AiError::MissingKey { .. })
            ));
        }
        assert!(matches!(
            t.explain("w", "s", "ru"),
            Err(AiError::Unsupported)
        ));
    }
}
