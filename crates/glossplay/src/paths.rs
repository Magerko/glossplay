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
    match std::fs::read_to_string(config_path()) {
        Ok(text) => match Config::from_toml(&text) {
            Ok(cfg) => (cfg, None),
            Err(e) => (
                Config::default(),
                Some(format!(
                    "{}, {}",
                    config_path().display(),
                    Config::describe_error(&text, &e)
                )),
            ),
        },
        Err(_) => (Config::default(), None),
    }
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
