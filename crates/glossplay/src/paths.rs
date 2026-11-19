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
    match std::fs::read(&path) {
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

pub fn save_config(cfg: &Config) -> std::io::Result<()> {
    std::fs::write(config_path(), cfg.to_toml())
}

/// Helper programs the player downloads and updates by itself.
pub fn tools_dir() -> PathBuf {
    let dir = data_dir().join("tools");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

#[cfg(test)]
mod tests {
    use super::parse_config;

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
