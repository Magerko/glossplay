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
            MediaError::LibraryNotFound { tried } => write_library_not_found(f, tried),
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

/// The search covers the whole PATH, which can be a hundred folders; the message names the
/// program folder (searched first) and counts the rest. `Mpv::new` prints the full list.
fn write_library_not_found(f: &mut fmt::Formatter<'_>, tried: &[String]) -> fmt::Result {
    if let [only] = tried {
        return write!(f, "libmpv was not found at {only}");
    }
    let mut dirs: Vec<&str> = Vec::new();
    for dir in tried
        .iter()
        .filter_map(|t| std::path::Path::new(t).parent()?.to_str())
        .filter(|d| !d.is_empty())
    {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    match dirs.split_first() {
        None => write!(f, "libmpv was not found; I tried {}", tried.join(", ")),
        Some((first, [])) => write!(f, "libmpv was not found in {first}"),
        Some((first, rest)) => write!(
            f,
            "libmpv was not found in {first} and {} folders from PATH",
            rest.len()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_message_stays_short() {
        let tried: Vec<String> = ["app", "p1", "p2", "p3"]
            .iter()
            .flat_map(|d| ["libmpv-2.dll", "mpv-2.dll"].map(|n| format!("{d}/{n}")))
            .collect();
        let text = MediaError::LibraryNotFound { tried }.to_string();
        assert_eq!(text, "libmpv was not found in app and 3 folders from PATH");
    }
}
