pub mod anki;
pub mod asr;
mod http;
pub mod translate;

#[cfg(test)]
mod testserver;

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AiError {
    MissingKey { provider: String },
    Http { status: u16, body: String },
    Network(String),
    BadResponse(String),
    Unsupported,
    Duplicate,
}

impl AiError {
    /// Worth retrying after a delay: the request itself was fine.
    pub fn is_transient(&self) -> bool {
        match self {
            AiError::Network(_) => true,
            AiError::Http { status, .. } => *status == 429 || *status >= 500,
            _ => false,
        }
    }
}

impl fmt::Display for AiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AiError::MissingKey { provider } => write!(f, "{provider}: API key is not set"),
            AiError::Http { status, body } => write!(f, "HTTP {status}: {body}"),
            AiError::Network(e) => write!(f, "network error: {e}"),
            AiError::BadResponse(e) => write!(f, "unexpected response: {e}"),
            AiError::Unsupported => write!(f, "this provider does not support the operation"),
            AiError::Duplicate => write!(f, "this note already exists"),
        }
    }
}

impl std::error::Error for AiError {}

/// Downloads a small file, such as a subtitle track of an online video.
pub fn fetch(url: &str) -> Result<Vec<u8>, AiError> {
    http::get_bytes(url)
}
