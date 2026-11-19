use crate::AiError;
use serde_json::Value;
use std::sync::OnceLock;
use std::time::Duration;

const BODY_SNIPPET: usize = 300;

fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(120)))
            .http_status_as_error(false)
            .build()
            .into()
    })
}

fn finish(
    result: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> Result<String, AiError> {
    let mut resp = result.map_err(|e| AiError::Network(e.to_string()))?;
    let status = resp.status().as_u16();
    let body = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| AiError::Network(e.to_string()))?;
    if (200..300).contains(&status) {
        Ok(body)
    } else {
        Err(AiError::Http {
            status,
            body: body.chars().take(BODY_SNIPPET).collect(),
        })
    }
}

pub fn post_json(url: &str, headers: &[(&str, &str)], body: &Value) -> Result<Value, AiError> {
    let mut req = agent().post(url);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    parse(&finish(req.send_json(body))?)
}

pub fn post_form(url: &str, form: &[(&str, &str)]) -> Result<Value, AiError> {
    parse(&finish(agent().post(url).send_form(form.iter().copied()))?)
}

pub fn post_bytes(url: &str, content_type: &str, body: &[u8]) -> Result<Value, AiError> {
    parse(&finish(
        agent()
            .post(url)
            .header("Content-Type", content_type)
            .send(body),
    )?)
}

pub fn get(url: &str) -> Result<String, AiError> {
    finish(agent().get(url).call())
}

fn parse(body: &str) -> Result<Value, AiError> {
    serde_json::from_str(body).map_err(|e| {
        AiError::BadResponse(format!(
            "{e}: {}",
            body.chars().take(BODY_SNIPPET).collect::<String>()
        ))
    })
}

pub fn get_bytes(url: &str) -> Result<Vec<u8>, AiError> {
    let mut resp = agent()
        .get(url)
        .call()
        .map_err(|e| AiError::Network(e.to_string()))?;
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(AiError::Http {
            status,
            body: String::new(),
        });
    }
    // The default cap is 10 MB; yt-dlp alone is about 18.
    resp.body_mut()
        .with_config()
        .limit(256 * 1024 * 1024)
        .read_to_vec()
        .map_err(|e| AiError::Network(e.to_string()))
}
