use gp_core::config::Config;
use std::path::PathBuf;

pub const ENV_CONFIG_DIR: &str = "GLOSSPLAY_CONFIG_DIR";

/// Where config, the database and exported media live.
pub fn data_dir() -> PathBuf {
    let dir = match std::env::var_os(ENV_CONFIG_DIR) {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => directories::ProjectDirs::from("", "", "Glossplay")
            .map(|d| d.data_dir().to_owned())
            .unwrap_or_else(|| PathBuf::from(".glossplay")),
    };
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn config_path() -> PathBuf {
    data_dir().join("config.toml")
}

pub fn db_path() -> PathBuf {
    data_dir().join("glossplay.db")
}

pub fn media_dir() -> PathBuf {
    let dir = data_dir().join("media");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// A broken config must not keep the player from starting, so I fall back to defaults
/// and report the problem instead.
pub fn load_config() -> (Config, Option<String>) {
    let path = config_path();
    let read = std::fs::read(&path);
    if let Ok(bytes) = &read {
        *seen() = String::from_utf8(bytes.clone()).ok();
    }
    match read {
        Ok(bytes) => match parse_config(&path.display().to_string(), &bytes) {
            Ok(cfg) => (cfg, None),
            Err(message) => (Config::default(), Some(message)),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Config::default(), None),
        Err(e) => (
            Config::default(),
            Some(format!("{}: cannot read ({e})", path.display())),
        ),
    }
}

/// One-line explanation of why a config did not load: file, line, reason.
fn parse_config(name: &str, bytes: &[u8]) -> Result<Config, String> {
    // Windows PowerShell writes UTF-16 by default, which must not look like an empty file.
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Err(format!("{name}: not valid UTF-8 text (save it as UTF-8)"));
    };
    Config::from_toml(text).map_err(|e| {
        let reason = Config::describe_error(text, &e);
        let reason = reason.split_whitespace().collect::<Vec<_>>().join(" ");
        format!("{name}, {reason}")
    })
}

/// Keeps a copy of a config that did not parse, so saving settings never destroys it unseen.
pub fn keep_broken_config() -> std::io::Result<PathBuf> {
    let backup = config_path().with_extension("toml.broken");
    std::fs::copy(config_path(), &backup)?;
    Ok(backup)
}

/// The text of config.toml as the player last read or wrote it.
fn seen() -> std::sync::MutexGuard<'static, Option<String>> {
    static SEEN: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
    SEEN.lock().unwrap_or_else(|e| e.into_inner())
}

/// Saves the settings. When the file was changed by someone else since the player read it,
/// their changes stay and only what was changed here goes over them; `cfg` gets theirs too.
pub fn save_config(cfg: &mut Config) -> std::io::Result<()> {
    save_to(&config_path(), cfg, &mut seen())
}

fn save_to(
    path: &std::path::Path,
    cfg: &mut Config,
    seen: &mut Option<String>,
) -> std::io::Result<()> {
    let disk = std::fs::read_to_string(path).ok();
    if let Some(disk) = disk.filter(|d| seen.as_ref() != Some(d)) {
        let base = match seen.as_deref() {
            Some(text) => Config::from_toml(text).ok(),
            None => Some(Config::default()),
        };
        // A file that does not parse is kept aside elsewhere; here mine is all I can save.
        if let (Some(base), Ok(theirs)) = (base, Config::from_toml(&disk)) {
            *cfg = Config::merge(&base, cfg, &theirs);
        }
    }
    let text = cfg.to_toml();
    std::fs::write(path, &text)?;
    *seen = Some(text);
    Ok(())
}

/// Helper programs the player downloads and updates by itself.
pub fn tools_dir() -> PathBuf {
    let dir = data_dir().join("tools");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

#[cfg(test)]
mod tests {
    use super::{parse_config, save_to};
    use gp_core::config::Config;

    #[test]
    fn a_hand_edit_made_while_the_player_runs_survives_its_saves() {
        let dir = std::env::temp_dir().join(format!("glossplay-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let mut seen = None;
        let mut cfg = Config::default();
        save_to(&path, &mut cfg, &mut seen).unwrap();

        // Someone edits the file by hand, comments and all.
        let edited = std::fs::read_to_string(&path).unwrap().replace(
            "model_path = \"\"",
            "# my own model\nmodel_path = 'E:\\models\\large.bin'",
        );
        assert_ne!(edited, std::fs::read_to_string(&path).unwrap());
        std::fs::write(&path, &edited).unwrap();

        // The player saves a setting of its own, twice.
        cfg.player.volume = 33.0;
        save_to(&path, &mut cfg, &mut seen).unwrap();
        assert_eq!(
            cfg.asr.model_path, r"E:\models\large.bin",
            "the player sees it too"
        );
        cfg.player.volume = 44.0;
        save_to(&path, &mut cfg, &mut seen).unwrap();

        let on_disk = Config::from_toml(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(on_disk.asr.model_path, r"E:\models\large.bin");
        assert_eq!(on_disk.player.volume, 44.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn syntax_error_is_one_line_with_file_and_line() {
        let e = parse_config("config.toml", b"[player\nvolume = = oops\n").unwrap_err();
        assert!(e.starts_with("config.toml, line 1: "), "{e}");
        assert!(!e.contains('\n'), "{e}");
        let e = parse_config("config.toml", b"[general]\nnative_lang = ru\n").unwrap_err();
        assert!(e.starts_with("config.toml, line 2: "), "{e}");
        assert!(!e.contains('\n'), "{e}");
    }

    #[test]
    fn wrong_type_names_the_line() {
        let e = parse_config("c.toml", b"[player]\nvolume = \"loud\"\n").unwrap_err();
        assert!(e.starts_with("c.toml, line 2: "), "{e}");
    }

    #[test]
    fn utf16_file_is_reported() {
        let e = parse_config("c.toml", b"\xff\xfe[\0p\0").unwrap_err();
        assert!(e.contains("UTF-8"), "{e}");
    }

    #[test]
    fn good_config_loads() {
        assert!(parse_config("c.toml", b"[player]\nvolume = 50\n").is_ok());
    }
}
