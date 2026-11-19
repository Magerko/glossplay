//! Keeps a private copy of yt-dlp up to date. Sites change often and an old yt-dlp simply
//! stops working, so I check for a new release once a day and replace the binary.

use crate::paths;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const RELEASES: &str = "https://github.com/yt-dlp/yt-dlp/releases";
const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);
static BUSY: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Checked recently; nothing was requested from the network.
    Fresh {
        version: String,
    },
    UpToDate {
        version: String,
    },
    Updated {
        version: String,
    },
}

impl Outcome {
    pub fn version(&self) -> &str {
        match self {
            Outcome::Fresh { version }
            | Outcome::UpToDate { version }
            | Outcome::Updated { version } => version,
        }
    }
}

fn asset_name() -> &'static str {
    if cfg!(windows) {
        "yt-dlp.exe"
    } else if cfg!(target_os = "macos") {
        "yt-dlp_macos"
    } else if cfg!(target_arch = "aarch64") {
        "yt-dlp_linux_aarch64"
    } else {
        "yt-dlp_linux"
    }
}

pub fn binary_path() -> PathBuf {
    paths::tools_dir().join(format!("yt-dlp{}", std::env::consts::EXE_SUFFIX))
}

fn state_path() -> PathBuf {
    paths::tools_dir().join("yt-dlp.json")
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Makes sure yt-dlp is present and current. `force` skips the once-a-day limit.
pub fn ensure(force: bool) -> Result<Outcome, String> {
    BUSY.store(true, Ordering::Relaxed);
    let result = ensure_inner(force);
    BUSY.store(false, Ordering::Relaxed);
    result
}

fn ensure_inner(force: bool) -> Result<Outcome, String> {
    let binary = binary_path();
    let state: Value = std::fs::read_to_string(state_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let installed = state["version"].as_str().unwrap_or("").to_owned();
    let checked_at = state["checked_at"].as_u64().unwrap_or(0);
    let have = binary.is_file() && !installed.is_empty();
    if have && !force && now().saturating_sub(checked_at) < CHECK_EVERY.as_secs() {
        return Ok(Outcome::Fresh { version: installed });
    }
    let latest = match latest_version() {
        Ok(v) => v,
        // Offline or rate-limited: an older yt-dlp is still better than none. I keep the old
        // check time so the next launch tries again.
        Err(_) if have => return Ok(Outcome::Fresh { version: installed }),
        Err(e) => return Err(format!("Could not reach GitHub to download yt-dlp: {e}")),
    };
    let save = |version: &str| {
        let _ = std::fs::write(
            state_path(),
            json!({ "version": version, "checked_at": now() }).to_string(),
        );
    };
    if have && latest == installed {
        save(&latest);
        return Ok(Outcome::UpToDate { version: latest });
    }

    let base = format!("{RELEASES}/download/{latest}");
    let bytes = gp_ai::fetch(&format!("{base}/{}", asset_name()))
        .map_err(|e| format!("Could not download yt-dlp: {e}"))?;
    // This catches a damaged or truncated download; it is no defence against a compromised GitHub.
    let sums = gp_ai::fetch(&format!("{base}/SHA2-256SUMS"))
        .map_err(|e| format!("Could not download the yt-dlp checksums: {e}"))?;
    let sums = String::from_utf8_lossy(&sums);
    let expected = find_hash(&sums, asset_name())
        .ok_or_else(|| format!("The checksum list has no entry for {}", asset_name()))?;
    if !hash_matches(&bytes, expected) {
        return Err("The downloaded yt-dlp does not match its checksum; not installing it".into());
    }
    // Download next to the target and rename, so a broken download never replaces a working copy.
    // The pid keeps two running players from writing the same file.
    let partial = binary.with_extension(format!("{}.part", std::process::id()));
    install(&partial, &binary, &bytes).inspect_err(|_| {
        let _ = std::fs::remove_file(&partial);
    })?;
    save(&latest);
    Ok(Outcome::Updated { version: latest })
}

fn install(partial: &Path, binary: &Path, bytes: &[u8]) -> Result<(), String> {
    std::fs::write(partial, bytes).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(partial, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
    }
    std::fs::rename(partial, binary).map_err(|e| e.to_string())
}

/// The API is rate-limited per IP, but the plain `releases/latest` page redirects to the tag.
fn latest_version() -> Result<String, String> {
    let location = gp_ai::redirect_target(&format!("{RELEASES}/latest"))
        .map_err(|e| e.to_string())?
        .ok_or("GitHub did not redirect to a release")?;
    parse_version(&location)
}

fn parse_version(location: &str) -> Result<String, String> {
    location
        .split_once("/releases/tag/")
        .map(|(_, tag)| tag)
        .filter(|tag| !tag.is_empty() && !tag.contains(['/', '?', '#']))
        .map(str::to_owned)
        .ok_or_else(|| format!("unexpected redirect target: {location}"))
}

/// Finds the hash of exactly `file` in `<hex>  <name>` lines.
fn find_hash<'a>(sums: &'a str, file: &str) -> Option<&'a str> {
    sums.lines().find_map(|line| {
        let (hash, name) = line.split_once(char::is_whitespace)?;
        (name.trim().trim_start_matches('*') == file).then_some(hash)
    })
}

fn hash_matches(bytes: &[u8], expected_hex: &str) -> bool {
    let hex: String = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    hex.eq_ignore_ascii_case(expected_hex)
}

/// True while `ensure` is running, so the UI does not start a second download.
pub fn is_busy() -> bool {
    BUSY.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_from_redirect() {
        assert_eq!(
            parse_version("https://github.com/yt-dlp/yt-dlp/releases/tag/2026.10.26").as_deref(),
            Ok("2026.10.26")
        );
        assert!(parse_version("https://github.com/login?return_to=x").is_err());
        assert!(parse_version("https://github.com/yt-dlp/yt-dlp/releases/tag/").is_err());
    }

    const SUMS: &str = "aaa111  yt-dlp_x86.exe\nbbb222  yt-dlp.exe\nccc333  yt-dlp_linux\n";

    #[test]
    fn hash_lookup_is_exact() {
        assert_eq!(find_hash(SUMS, "yt-dlp.exe"), Some("bbb222"));
        assert_eq!(find_hash(SUMS, "yt-dlp_x86.exe"), Some("aaa111"));
        assert_eq!(find_hash(SUMS, "yt-dlp_macos"), None);
        assert_eq!(find_hash("bbb222  my-yt-dlp.exe\n", "yt-dlp.exe"), None);
    }

    #[test]
    fn hash_comparison() {
        let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert!(hash_matches(b"abc", abc));
        assert!(hash_matches(b"abc", &abc.to_uppercase()));
        assert!(!hash_matches(b"abd", abc));
        assert!(!hash_matches(b"abc", ""));
    }
}
