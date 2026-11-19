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

impl std::fmt::Display for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Outcome::Fresh { version } => write!(f, "yt-dlp {version} (checked recently)"),
            Outcome::UpToDate { version } => write!(f, "yt-dlp {version} is up to date"),
            Outcome::Updated { version } => write!(f, "yt-dlp {version} installed"),
        }
    }
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

/// What I recorded about the installed file when I put it there.
#[derive(Debug, Clone, PartialEq)]
struct Recorded {
    sha256: String,
    size: u64,
    /// When the hash was last compared with the file on disk.
    verified_at: u64,
}

#[derive(Debug, PartialEq)]
enum Check {
    Trust,
    /// An older state file without a hash: I accept the file once and record it.
    Adopt,
    VerifyHash,
    Broken,
}

/// Size is cheap, so it is compared on every launch; the full hash only once a day.
fn decide(recorded: Option<&Recorded>, actual_size: u64, now: u64) -> Check {
    let Some(r) = recorded else {
        return Check::Adopt;
    };
    if r.size != actual_size {
        return Check::Broken;
    }
    if now.saturating_sub(r.verified_at) >= CHECK_EVERY.as_secs() {
        Check::VerifyHash
    } else {
        Check::Trust
    }
}

fn file_sha256(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(to_hex(&hasher.finalize()))
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn read_recorded(state: &Value) -> Option<Recorded> {
    Some(Recorded {
        sha256: state["sha256"].as_str()?.to_owned(),
        size: state["size"].as_u64()?,
        verified_at: state["verified_at"].as_u64().unwrap_or(0),
    })
}

fn write_state(version: &str, checked_at: u64, recorded: Option<&Recorded>) {
    let mut state = json!({ "version": version, "checked_at": checked_at });
    if let Some(r) = recorded {
        state["sha256"] = json!(r.sha256);
        state["size"] = json!(r.size);
        state["verified_at"] = json!(r.verified_at);
    }
    let _ = std::fs::write(state_path(), state.to_string());
}

/// Compares the installed file with what I recorded. Returns false when it is damaged, and
/// then yt-dlp has to be treated as missing.
fn installed_is_intact(binary: &Path, state: &Value, version: &str) -> bool {
    let recorded = read_recorded(state);
    let Ok(size) = std::fs::metadata(binary).map(|m| m.len()) else {
        return false;
    };
    let checked_at = state["checked_at"].as_u64().unwrap_or(0);
    let stamp = |sha256: String| Recorded {
        sha256,
        size,
        verified_at: now(),
    };
    match decide(recorded.as_ref(), size, now()) {
        Check::Trust => true,
        Check::Broken => false,
        Check::Adopt => match file_sha256(binary) {
            Ok(hash) => {
                write_state(version, checked_at, Some(&stamp(hash)));
                true
            }
            Err(_) => false,
        },
        Check::VerifyHash => match (file_sha256(binary), recorded) {
            (Ok(hash), Some(r)) if hash == r.sha256 => {
                write_state(version, checked_at, Some(&stamp(hash)));
                true
            }
            _ => false,
        },
    }
}

fn ensure_inner(force: bool) -> Result<Outcome, String> {
    let binary = binary_path();
    let state: Value = std::fs::read_to_string(state_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let installed = state["version"].as_str().unwrap_or("").to_owned();
    let checked_at = state["checked_at"].as_u64().unwrap_or(0);
    let have = binary.is_file()
        && !installed.is_empty()
        && installed_is_intact(&binary, &state, &installed);
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
    if have && latest == installed {
        // `installed_is_intact` may just have refreshed the record, so I read it back.
        let recorded = std::fs::read_to_string(state_path())
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .and_then(|v| read_recorded(&v));
        write_state(&latest, now(), recorded.as_ref());
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
    let recorded = Recorded {
        sha256: to_hex(&Sha256::digest(&bytes)),
        size: bytes.len() as u64,
        verified_at: now(),
    };
    write_state(&latest, now(), Some(&recorded));
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
    to_hex(&Sha256::digest(bytes)).eq_ignore_ascii_case(expected_hex)
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
    fn integrity_decision() {
        const DAY: u64 = 24 * 60 * 60;
        let r = Recorded {
            sha256: "abc".into(),
            size: 100,
            verified_at: 1_000,
        };
        assert_eq!(decide(Some(&r), 100, 1_000 + 60), Check::Trust);
        assert_eq!(decide(Some(&r), 100, 1_000 + DAY), Check::VerifyHash);
        assert_eq!(decide(Some(&r), 99, 1_000 + 60), Check::Broken);
        assert_eq!(decide(Some(&r), 1, 1_000 + 3 * DAY), Check::Broken);
        assert_eq!(decide(None, 100, 5), Check::Adopt);
    }

    #[test]
    fn recorded_fields_come_from_state() {
        let state = json!({ "version": "v", "sha256": "ff", "size": 7 });
        let r = read_recorded(&state).unwrap();
        assert_eq!((r.sha256.as_str(), r.size, r.verified_at), ("ff", 7, 0));
        assert!(read_recorded(&json!({ "version": "v" })).is_none());
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
