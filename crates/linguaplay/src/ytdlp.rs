//! Keeps a private copy of yt-dlp up to date. Sites change often and an old yt-dlp simply
//! stops working, so I check for a new release once a day and replace the binary.

use crate::paths;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const LATEST: &str = "https://api.github.com/repos/yt-dlp/yt-dlp/releases/latest";
const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

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

    let release = match fetch_release() {
        Ok(r) => r,
        // Offline or rate-limited: an older yt-dlp is still better than none.
        Err(_) if have => return Ok(Outcome::Fresh { version: installed }),
        Err(e) => return Err(format!("Could not reach GitHub to download yt-dlp: {e}")),
    };
    let latest = release["tag_name"]
        .as_str()
        .ok_or("GitHub returned no release tag")?
        .to_owned();
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

    let url = release["assets"]
        .as_array()
        .and_then(|assets| assets.iter().find(|a| a["name"] == asset_name()))
        .and_then(|a| a["browser_download_url"].as_str())
        .ok_or_else(|| format!("The release has no {} file", asset_name()))?;
    let bytes = lp_ai::fetch(url).map_err(|e| e.to_string())?;
    // Download next to the target and rename, so a broken download never replaces a working copy.
    let partial = binary.with_extension("part");
    std::fs::write(&partial, bytes).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&partial, std::fs::Permissions::from_mode(0o755));
    }
    std::fs::rename(&partial, &binary).map_err(|e| e.to_string())?;
    save(&latest);
    Ok(Outcome::Updated { version: latest })
}

fn fetch_release() -> Result<Value, String> {
    let bytes = lp_ai::fetch(LATEST).map_err(|e| e.to_string())?;
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}
