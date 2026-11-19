use crate::AiError;
use serde_json::Value;
#[cfg(test)]
use serde_json::json;
use std::sync::OnceLock;
use std::time::Duration;

const BODY_SNIPPET: usize = 300;
// Without a connect timeout a dead or filtered host blocks for the OS limit (about 21 s on
// Windows) or, failing that, for the whole 120 s global budget.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(120)))
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .http_status_as_error(false)
            // API calls carry keys in custom headers (x-api-key, DeepL-Auth-Key) that ureq would
            // forward to another host on a redirect; none of these APIs redirect.
            .max_redirects(0)
            .build()
            .into()
    })
}

const MAX_DOWNLOAD_REDIRECTS: u32 = 5;

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

/// A JSON request whose answer is a file, such as synthesized speech.
pub fn post_json_for_bytes(url: &str, body: &Value) -> Result<Vec<u8>, AiError> {
    let mut resp = agent()
        .post(url)
        .send_json(body)
        .map_err(|e| AiError::Network(e.to_string()))?;
    let status = resp.status().as_u16();
    let bytes = resp
        .body_mut()
        .with_config()
        .limit(64 * 1024 * 1024)
        .read_to_vec()
        .map_err(|e| AiError::Network(e.to_string()))?;
    if (200..300).contains(&status) {
        Ok(bytes)
    } else {
        Err(AiError::Http {
            status,
            body: String::from_utf8_lossy(&bytes)
                .chars()
                .take(BODY_SNIPPET)
                .collect(),
        })
    }
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
        .config()
        .max_redirects(MAX_DOWNLOAD_REDIRECTS)
        // An https URL must not be bounced to plain http; an explicit http URL stays allowed.
        .https_only(url.starts_with("https://"))
        .build()
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

/// Returns the `Location` of a 3xx response without following it; `None` if there was no redirect.
pub fn redirect_target(url: &str) -> Result<Option<String>, AiError> {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    let agent = AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(30)))
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .http_status_as_error(false)
            .max_redirects(0)
            .build()
            .into()
    });
    let resp = agent
        .get(url)
        .call()
        .map_err(|e| AiError::Network(e.to_string()))?;
    let status = resp.status().as_u16();
    if (300..400).contains(&status) {
        return Ok(resp
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned));
    }
    if (200..300).contains(&status) {
        return Ok(None);
    }
    Err(AiError::Http {
        status,
        body: String::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testserver::TestServer;

    #[test]
    fn clients_have_a_connect_timeout_shorter_than_the_global_one() {
        let t = agent().config().timeouts();
        assert_eq!(t.connect, Some(CONNECT_TIMEOUT));
        assert!(t.global.unwrap() > CONNECT_TIMEOUT);
    }

    #[test]
    fn api_posts_do_not_follow_redirects() {
        let srv = TestServer::start_with_headers(
            vec![(302, "{}".into())],
            "Location: /elsewhere\r\n".into(),
        );
        let e = post_json(&srv.url, &[("x-api-key", "secret")], &json!({})).unwrap_err();
        assert!(matches!(e, AiError::Http { status: 302, .. }), "{e:?}");
        assert_eq!(srv.count(), 1);
    }

    #[test]
    fn downloads_follow_redirects() {
        let srv = TestServer::start_with_headers(
            vec![(302, "".into()), (200, "data".into())],
            "Location: /next\r\n".into(),
        );
        assert_eq!(get_bytes(&srv.url).unwrap(), b"data");
        assert_eq!(srv.count(), 2);
    }

    #[test]
    fn downloads_stop_after_five_redirects() {
        let srv =
            TestServer::start_with_headers(vec![(302, "".into())], "Location: /again\r\n".into());
        assert!(matches!(get_bytes(&srv.url), Err(AiError::Network(_))));
        assert_eq!(srv.count(), 6);
    }
}
