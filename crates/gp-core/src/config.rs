use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    pub general: General,
    pub player: Player,
    pub subtitles: Subtitles,
    pub translation: Translation,
    pub asr: Asr,
    pub training: Training,
    pub anki: Anki,
}

impl Config {
    /// Missing fields take their defaults; unknown ones are ignored.
    pub fn from_toml(s: &str) -> Result<Config, toml::de::Error> {
        toml::from_str(s)
    }

    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).expect("config has only serializable fields")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct General {
    /// Empty means "detect".
    pub learn_lang: String,
    pub native_lang: String,
    pub ui_scale: f32,
}

impl Default for General {
    fn default() -> Self {
        General {
            learn_lang: "en".into(),
            native_lang: "ru".into(),
            ui_scale: 1.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Player {
    pub hwdec: String,
    pub volume: f64,
    pub mpv_path: String,
    pub ffmpeg_path: String,
    pub ytdl_path: String,
}

impl Default for Player {
    fn default() -> Self {
        Player {
            hwdec: "auto".into(),
            volume: 100.0,
            mpv_path: String::new(),
            ffmpeg_path: String::new(),
            ytdl_path: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Subtitles {
    /// In points at a window height of 720; scales with the window.
    pub font_size: f32,
    pub secondary_scale: f32,
    pub show_primary: bool,
    pub show_secondary: bool,
    pub auto_translate: bool,
}

impl Default for Subtitles {
    fn default() -> Self {
        Subtitles {
            font_size: 30.0,
            secondary_scale: 0.8,
            show_primary: true,
            show_secondary: true,
            auto_translate: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Translation {
    /// google | deepl | openai | anthropic
    pub provider: String,
    pub model: String,
    pub base_url: String,
    pub api_key: String,
    /// How many lines ahead of the playhead I translate in advance.
    pub lookahead: usize,
}

impl Default for Translation {
    fn default() -> Self {
        Translation {
            provider: "google".into(),
            model: String::new(),
            base_url: String::new(),
            api_key: String::new(),
            lookahead: 40,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Asr {
    pub enabled: bool,
    pub engine: String,
    pub server_path: String,
    pub model_path: String,
    pub chunk_sec: u32,
}

impl Default for Asr {
    fn default() -> Self {
        Asr {
            enabled: false,
            engine: "whisper-server".into(),
            server_path: String::new(),
            model_path: String::new(),
            chunk_sec: 30,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Training {
    pub autopause: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Anki {
    pub url: String,
    pub deck: String,
}

impl Default for Anki {
    fn default() -> Self {
        Anki {
            url: "http://127.0.0.1:8765".into(),
            deck: "Glossplay".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_and_partial_files_fall_back_to_defaults() {
        assert_eq!(Config::from_toml("").unwrap(), Config::default());
        let c = Config::from_toml("[general]\nnative_lang = \"de\"\nfuture_field = 1\n").unwrap();
        assert_eq!(c.general.native_lang, "de");
        assert_eq!(c.general.learn_lang, "en");
        assert_eq!(c.translation.provider, "google");
    }

    #[test]
    fn roundtrip() {
        let mut c = Config::default();
        c.translation.provider = "openai".into();
        c.training.autopause = true;
        assert_eq!(Config::from_toml(&c.to_toml()).unwrap(), c);
    }

    #[test]
    fn bad_type_is_an_error() {
        assert!(Config::from_toml("[player]\nvolume = \"loud\"\n").is_err());
    }
}
