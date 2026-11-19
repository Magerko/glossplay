use gp_core::Ms;
use gp_core::config::Player as PlayerConfig;
use gp_media::MediaError;
use gp_media::mpv::Mpv;
use serde_json::Value;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackKind {
    Video,
    Audio,
    Sub,
}

#[derive(Debug, Clone)]
pub struct TrackInfo {
    pub id: i64,
    pub kind: TrackKind,
    pub lang: Option<String>,
    pub title: Option<String>,
    pub codec: String,
    pub ff_index: Option<u32>,
    pub external_filename: Option<String>,
    pub selected: bool,
}

impl TrackInfo {
    /// Image-based subtitles cannot be turned into text cues; mpv has to draw them.
    pub fn is_bitmap_sub(&self) -> bool {
        matches!(
            self.codec.as_str(),
            "hdmv_pgs_subtitle" | "dvd_subtitle" | "dvb_subtitle" | "xsub" | "dvb_teletext"
        )
    }

    pub fn label(&self) -> String {
        let mut parts = vec![format!("#{}", self.id)];
        parts.extend(self.lang.clone());
        parts.extend(self.title.clone());
        parts.join(" · ")
    }
}

#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub pos: Ms,
    pub duration: Ms,
    pub paused: bool,
    pub idle: bool,
    pub volume: f64,
    pub speed: f64,
    pub muted: bool,
}

/// The hwdec list I use when the config says "auto".
fn default_hwdec() -> &'static str {
    // With the OpenGL render API on Windows only nvdec decodes without a copy; plain
    // d3d11va silently falls back to software, so the copy variant is the safe second choice.
    if cfg!(windows) {
        "nvdec,d3d11va-copy"
    } else {
        "auto-safe"
    }
}

/// ytdl_hook splits its ytdl_path on `;` (Windows) or `:` (elsewhere) with no way to escape it,
/// so a path containing that character is never found. For such a path I hand mpv the folder
/// as its config directory and give the hook just the file name, which it looks up there.
fn ytdl_location(path: &str) -> (String, Option<String>) {
    let separator = if cfg!(windows) { ';' } else { ':' };
    let p = std::path::Path::new(path);
    match (p.parent(), p.file_name()) {
        (Some(dir), Some(name)) if path.contains(separator) => (
            name.to_string_lossy().into_owned(),
            Some(dir.to_string_lossy().into_owned()),
        ),
        _ => (path.to_owned(), None),
    }
}

pub struct Player {
    pub mpv: Arc<Mpv>,
}

impl Player {
    pub fn new(cfg: &PlayerConfig) -> Result<Player, MediaError> {
        let hwdec = match cfg.hwdec.as_str() {
            "" | "auto" => default_hwdec(),
            other => other,
        };
        let volume = cfg.volume.to_string();
        let mut options = vec![
            ("vo", "libmpv"),
            ("hwdec", hwdec),
            ("keep-open", "yes"),
            ("idle", "yes"),
            ("terminal", "no"),
            ("osc", "no"),
            ("input-default-bindings", "no"),
            ("sub-auto", "no"),
            ("sub-visibility", "no"),
            ("volume", volume.as_str()),
            ("volume-max", "150"),
        ];
        // Unless the user points at their own yt-dlp, mpv uses the copy I keep up to date.
        let ytdl = if cfg.ytdl_path.is_empty() {
            crate::ytdlp::binary_path().to_string_lossy().into_owned()
        } else {
            cfg.ytdl_path.clone()
        };
        let (ytdl, config_dir) = ytdl_location(&ytdl);
        // A comma separates entries in a script-opts list; the %len% form takes the value verbatim.
        let ytdl_opt = format!("ytdl_hook-ytdl_path=%{}%{ytdl}", ytdl.len());
        options.push(("ytdl", "yes"));
        options.push(("script-opts", &ytdl_opt));
        if let Some(dir) = &config_dir {
            // mpv ignores config-dir unless config is on; the folder holds no mpv.conf.
            options.push(("config", "yes"));
            options.push(("config-dir", dir));
        }
        Ok(Player {
            mpv: Arc::new(Mpv::new(&cfg.mpv_path, &options)?),
        })
    }

    pub fn load(&self, media: &str) -> Result<(), MediaError> {
        self.mpv.command(&["loadfile", media])
    }

    pub fn snapshot(&self) -> Snapshot {
        let ms = |name| {
            self.mpv
                .get_f64(name)
                .map_or(0, |s| (s * 1000.0).round() as Ms)
        };
        Snapshot {
            pos: ms("time-pos"),
            duration: ms("duration"),
            paused: self.mpv.get_flag("pause").unwrap_or(true),
            idle: self.mpv.get_flag("idle-active").unwrap_or(true),
            volume: self.mpv.get_f64("volume").unwrap_or(100.0),
            speed: self.mpv.get_f64("speed").unwrap_or(1.0),
            muted: self.mpv.get_flag("mute").unwrap_or(false),
        }
    }

    pub fn tracks(&self) -> Vec<TrackInfo> {
        let json = self.mpv.get_string("track-list").unwrap_or_default();
        let list: Vec<Value> = serde_json::from_str(&json).unwrap_or_default();
        list.iter()
            .filter_map(|t| {
                let kind = match t["type"].as_str()? {
                    "video" => TrackKind::Video,
                    "audio" => TrackKind::Audio,
                    "sub" => TrackKind::Sub,
                    _ => return None,
                };
                let text = |key: &str| t[key].as_str().map(str::to_owned);
                Some(TrackInfo {
                    id: t["id"].as_i64()?,
                    kind,
                    lang: text("lang"),
                    title: text("title"),
                    codec: text("codec").unwrap_or_default(),
                    ff_index: t["ff-index"].as_u64().map(|i| i as u32),
                    external_filename: text("external-filename"),
                    selected: t["selected"].as_bool().unwrap_or(false),
                })
            })
            .collect()
    }

    pub fn set_paused(&self, paused: bool) {
        self.mpv.set_flag("pause", paused);
    }

    pub fn seek(&self, to: Ms) {
        let secs = format!("{:.3}", to.max(0) as f64 / 1000.0);
        self.mpv.command_async(&["seek", &secs, "absolute+exact"]);
    }

    pub fn seek_relative(&self, by: Ms) {
        let secs = format!("{:.3}", by as f64 / 1000.0);
        self.mpv.command_async(&["seek", &secs, "relative+exact"]);
    }

    pub fn screenshot(&self, path: &std::path::Path) -> Result<(), MediaError> {
        self.mpv
            .command(&["screenshot-to-file", &path.to_string_lossy(), "video"])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_ytdl_path_goes_straight_to_the_hook() {
        assert_eq!(ytdl_location("yt-dlp"), ("yt-dlp".to_owned(), None));
    }

    #[cfg(windows)]
    #[test]
    fn semicolon_path_becomes_config_dir_and_name() {
        let (name, dir) = ytdl_location(r"E:\a;b\tools\yt-dlp.exe");
        assert_eq!(name, "yt-dlp.exe");
        assert_eq!(dir.as_deref(), Some(r"E:\a;b\tools"));
    }
}
