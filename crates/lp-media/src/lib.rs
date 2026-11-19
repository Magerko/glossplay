pub mod mpv;
pub mod tools;

use std::fmt;

#[derive(Debug)]
pub enum MediaError {
    LibraryNotFound { tried: Vec<String> },
    Mpv { code: i32, what: String },
    ToolNotFound { tool: String },
    ToolFailed { tool: String, stderr: String },
    Io(std::io::Error),
}

impl fmt::Display for MediaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MediaError::LibraryNotFound { tried } => {
                write!(f, "libmpv was not found; looked in: {}", tried.join(", "))
            }
            MediaError::Mpv { code, what } => write!(f, "mpv error {code} in {what}"),
            MediaError::ToolNotFound { tool } => write!(f, "{tool} was not found"),
            MediaError::ToolFailed { tool, stderr } => write!(f, "{tool} failed: {stderr}"),
            MediaError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for MediaError {}

impl From<std::io::Error> for MediaError {
    fn from(e: std::io::Error) -> Self {
        MediaError::Io(e)
    }
}
