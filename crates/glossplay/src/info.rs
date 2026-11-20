//! "Video info": what is playing and how well, read from mpv, for the window of that name,
//! its "Copy all" text and the `video_info` tool of MCP.
//!
//! Links are shown by their host only: a page or stream address can carry keys and tokens,
//! and a TorrServer address names the torrent.

use crate::app::App;
use crate::buffering::{self, Cache};
use crate::i18n::{self, Lang, Tr, t};
use crate::session::Secondary;
use crate::theme;
use eframe::egui::{self, Id, RichText};
use gp_media::mpv::Mpv;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

/// How often the open window reads mpv again.
const REFRESH: Duration = Duration::from_millis(500);

/// Where the values come from; mpv in the player, a table in the tests.
pub trait Props {
    fn text(&self, name: &str) -> Option<String>;
    fn num(&self, name: &str) -> Option<f64>;
}

impl Props for Mpv {
    fn text(&self, name: &str) -> Option<String> {
        self.get_string(name).filter(|s| !s.trim().is_empty())
    }

    fn num(&self, name: &str) -> Option<f64> {
        self.get_f64(name).filter(|n| n.is_finite())
    }
}

/// What the player knows besides mpv.
pub struct Input<'a> {
    pub media: &'a str,
    pub title: &'a str,
    pub torrserver: bool,
    pub audio_track: Option<String>,
    pub primary: Option<String>,
    pub secondary: Option<String>,
    /// The stream's cache, for a network source.
    pub cache: Option<&'a Cache>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub key: &'static str,
    pub label: Tr,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Section {
    pub key: &'static str,
    pub title: Tr,
    pub rows: Vec<Row>,
}

/// The source as it may be shown: a local path as it is, a link by its scheme and host.
pub fn source(media: &str, torrserver: bool) -> String {
    if torrserver {
        return "TorrServer".to_owned();
    }
    if media.trim().to_ascii_lowercase().starts_with("magnet:") {
        return "magnet".to_owned();
    }
    match (media.split_once("://"), crate::frame::host_of(media)) {
        (Some((scheme, _)), Some(host)) => format!("{}://{host}", scheme.to_ascii_lowercase()),
        (Some(_), None) if !media.to_ascii_lowercase().starts_with("file:") => "—".to_owned(),
        _ => media.to_owned(),
    }
}

/// mpv names a demuxer by its whole family, "mov,mp4,m4a,3gp,3g2,mj2"; the member the file's
/// extension names says more, and the first one stands for the rest.
fn container(format: &str, media: &str) -> String {
    let path = media.split(['?', '#']).next().unwrap_or(media);
    let ext = std::path::Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase());
    let names: Vec<&str> = format.split(',').map(str::trim).collect();
    let pick = ext
        .as_deref()
        .and_then(|e| names.iter().find(|n| **n == e))
        .or(names.first())
        .copied()
        .unwrap_or(format);
    pick.to_owned()
}

fn fill(lang: Lang, key: Tr, value: &dyn std::fmt::Display) -> String {
    i18n::fill(i18n::text_in(lang, key), &[value])
}

/// A number with at most `places` decimals and no trailing zeros, in the language's style.
fn number(lang: Lang, value: f64, places: usize) -> String {
    let mut s = format!("{value:.places$}");
    if s.contains('.') {
        s = s.trim_end_matches('0').trim_end_matches('.').to_owned();
    }
    match lang {
        Lang::Ru | Lang::Uk => s.replace('.', ","),
        Lang::En | Lang::Zh => s,
    }
}

pub fn bytes(lang: Lang, n: f64) -> String {
    const K: f64 = 1024.0;
    if n >= K * K * K {
        fill(lang, Tr::UnitGb, &number(lang, n / (K * K * K), 2))
    } else if n >= K * K {
        fill(lang, Tr::UnitMb, &number(lang, n / (K * K), 1))
    } else {
        fill(lang, Tr::UnitKb, &number(lang, (n / K).max(0.0), 0))
    }
}

fn bitrate(lang: Lang, bits: f64) -> String {
    if bits >= 1_000_000.0 {
        fill(lang, Tr::UnitMbps, &number(lang, bits / 1_000_000.0, 1))
    } else {
        fill(lang, Tr::UnitKbps, &number(lang, bits / 1000.0, 0))
    }
}

/// The values of the moment, grouped. A value mpv does not have is left out.
pub fn gather(p: &dyn Props, input: &Input, lang: Lang) -> Vec<Section> {
    let mut out = Vec::new();
    let mut add = |key, title, rows: Vec<(&'static str, Tr, Option<String>)>| {
        let rows: Vec<Row> = rows
            .into_iter()
            .filter_map(|(key, label, value)| {
                Some(Row {
                    key,
                    label,
                    value: value?,
                })
            })
            .collect();
        if !rows.is_empty() {
            out.push(Section { key, title, rows });
        }
    };
    let clock = |secs: f64| crate::theme::clock((secs * 1000.0) as gp_core::Ms);

    add(
        "file",
        Tr::InfoFile,
        vec![
            (
                "title",
                Tr::InfoTitleRow,
                Some(input.title.to_owned()).filter(|t| !t.trim().is_empty()),
            ),
            (
                "source",
                Tr::InfoSource,
                Some(source(input.media, input.torrserver)),
            ),
            (
                "format",
                Tr::InfoContainer,
                p.text("file-format").map(|f| container(&f, input.media)),
            ),
            (
                "duration",
                Tr::InfoDuration,
                p.num("duration").filter(|d| *d > 0.0).map(clock),
            ),
            (
                "size",
                Tr::InfoSize,
                p.num("file-size")
                    .filter(|n| *n > 0.0)
                    .map(|n| bytes(lang, n)),
            ),
        ],
    );

    let codec = p
        .text("video-format")
        .or_else(|| p.text("current-tracks/video/codec"));
    let codec = codec.map(|c| match p.text("current-tracks/video/codec-profile") {
        Some(profile) => format!("{c} ({profile})"),
        None => c,
    });
    let size = match (p.num("video-params/w"), p.num("video-params/h")) {
        (Some(w), Some(h)) if w > 0.0 && h > 0.0 => Some(format!("{w:.0}×{h:.0}")),
        _ => None,
    };
    let pixels =
        p.text("video-params/pixelformat")
            .map(|f| match p.text("video-params/hw-pixelformat") {
                Some(hw) => format!("{hw} ({f})"),
                None => f,
            });
    let gamma = p.text("video-params/gamma");
    let colour = {
        let parts: Vec<String> = [
            p.text("video-params/colormatrix"),
            p.text("video-params/primaries"),
            gamma.clone(),
        ]
        .into_iter()
        .flatten()
        .collect();
        let hdr = match gamma.as_deref() {
            Some("pq") => Some("HDR (PQ)"),
            Some("hlg") => Some("HDR (HLG)"),
            _ => None,
        };
        (!parts.is_empty()).then(|| match hdr {
            Some(h) => format!("{h} · {}", parts.join(" / ")),
            None => format!("SDR · {}", parts.join(" / ")),
        })
    };
    let hwdec = p.text("hwdec-current").map(|h| {
        if h == "no" {
            i18n::text_in(lang, Tr::InfoSoftware).to_owned()
        } else {
            h
        }
    });
    if codec.is_some() || size.is_some() {
        add(
            "video",
            Tr::InfoVideo,
            vec![
                ("codec", Tr::InfoCodec, codec),
                ("resolution", Tr::InfoResolution, size),
                (
                    "fps",
                    Tr::InfoFps,
                    p.num("container-fps")
                        .filter(|f| *f > 0.0)
                        .map(|f| fill(lang, Tr::UnitFps, &number(lang, f, 3))),
                ),
                (
                    "bitrate",
                    Tr::InfoBitrate,
                    p.num("video-bitrate")
                        .filter(|b| *b > 0.0)
                        .map(|b| bitrate(lang, b)),
                ),
                ("pixel_format", Tr::InfoPixelFormat, pixels),
                ("colour", Tr::InfoColour, colour),
                ("hardware_decoding", Tr::HwdecRow, hwdec),
            ],
        );
    }

    if let Some(codec) = p.text("audio-codec-name") {
        let channels = p
            .text("audio-params/hr-channels")
            .or_else(|| p.text("audio-params/channels"))
            .or_else(|| {
                p.num("audio-params/channel-count")
                    .map(|n| format!("{n:.0}"))
            });
        add(
            "audio",
            Tr::InfoAudio,
            vec![
                ("track", Tr::InfoTrack, input.audio_track.clone()),
                ("codec", Tr::InfoCodec, Some(codec)),
                ("channels", Tr::InfoChannels, channels),
                (
                    "sample_rate",
                    Tr::InfoSampleRate,
                    p.num("audio-params/samplerate")
                        .filter(|r| *r > 0.0)
                        .map(|r| fill(lang, Tr::UnitKhz, &number(lang, r / 1000.0, 1))),
                ),
                (
                    "bitrate",
                    Tr::InfoBitrate,
                    p.num("audio-bitrate")
                        .filter(|b| *b > 0.0)
                        .map(|b| bitrate(lang, b)),
                ),
            ],
        );
    }

    let none = || Some(i18n::text_in(lang, Tr::NoneChoice).to_owned());
    add(
        "subtitles",
        Tr::InfoSubtitles,
        vec![
            ("primary", Tr::Learning, input.primary.clone().or_else(none)),
            (
                "secondary",
                Tr::InfoSecondLine,
                input.secondary.clone().or_else(none),
            ),
        ],
    );

    let count = |name| p.num(name).map(|n| format!("{n:.0}"));
    add(
        "playback",
        Tr::InfoPlayback,
        vec![
            ("dropped_frames", Tr::InfoDropped, count("frame-drop-count")),
            (
                "decoder_dropped_frames",
                Tr::InfoDecoderDropped,
                count("decoder-frame-drop-count"),
            ),
            (
                "delayed_frames",
                Tr::InfoMistimed,
                count("mistimed-frame-count"),
            ),
            (
                "output_fps",
                Tr::InfoOutputFps,
                p.num("estimated-vf-fps")
                    .filter(|f| *f > 0.0)
                    .map(|f| fill(lang, Tr::UnitFps, &number(lang, f, 2))),
            ),
        ],
    );

    if let Some(cache) = input.cache {
        let state = if cache.paused_for_cache {
            buffering::label_in(lang, cache)
        } else {
            i18n::text_in(lang, Tr::InfoCacheOk).to_owned()
        };
        add(
            "network",
            Tr::InfoNetwork,
            vec![
                ("state", Tr::InfoCacheState, Some(state)),
                (
                    "buffered_seconds",
                    Tr::InfoBuffered,
                    cache
                        .state
                        .ahead
                        .map(|s| fill(lang, Tr::UnitSeconds, &number(lang, s, 1))),
                ),
                (
                    "buffered_bytes",
                    Tr::InfoBufferedBytes,
                    cache.state.bytes_ahead.map(|b| bytes(lang, b)),
                ),
                (
                    "speed",
                    Tr::InfoSpeed,
                    cache.state.rate.map(|r| buffering::speed_in(lang, r)),
                ),
            ],
        );
    }
    out
}

/// The sections as plain text, for a bug report.
pub fn as_text(sections: &[Section], lang: Lang) -> String {
    let mut out = String::new();
    for s in sections {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(i18n::text_in(lang, s.title));
        out.push('\n');
        for r in &s.rows {
            out.push_str(&format!(
                "  {}: {}\n",
                i18n::text_in(lang, r.label),
                r.value
            ));
        }
    }
    out
}

pub fn as_json(sections: &[Section]) -> Value {
    let mut out = serde_json::Map::new();
    for s in sections {
        let rows: serde_json::Map<String, Value> = s
            .rows
            .iter()
            .map(|r| (r.key.to_owned(), json!(r.value)))
            .collect();
        out.insert(s.key.to_owned(), Value::Object(rows));
    }
    Value::Object(out)
}

pub struct InfoView {
    read_at: Instant,
    sections: Vec<Section>,
}

impl App {
    /// The info of the moment, in `lang`.
    pub fn video_info(&self, lang: Lang) -> Vec<Section> {
        let Some(s) = &self.session else {
            return Vec::new();
        };
        let label = |i: usize| s.tracks.get(i).map(|t| t.label.clone());
        let secondary = match s.secondary {
            Secondary::Off => None,
            Secondary::Track(i) => label(i),
            Secondary::Translate => Some(match &self.translator {
                Ok(tr) => i18n::fill(i18n::text_in(lang, Tr::TranslateAuto), &[&tr.id()]),
                Err(_) => i18n::text_in(lang, Tr::TranslationLabel).to_owned(),
            }),
        };
        let input = Input {
            media: &s.media,
            title: &s.title,
            torrserver: self.torrserver_stream(&s.media).is_some(),
            audio_track: self
                .audio_tracks
                .iter()
                .find(|a| a.selected)
                .map(|a| a.label()),
            primary: s.primary.and_then(label),
            secondary,
            cache: self.cache.observing().then_some(&self.cache),
        };
        gather(self.player.mpv.as_ref(), &input, lang)
    }

    pub fn toggle_info(&mut self) {
        self.info_open = !self.info_open && self.session.is_some();
        self.info_view = None;
    }

    pub fn info_window(&mut self, ctx: &egui::Context) {
        if !self.info_open || self.session.is_none() {
            self.info_open = false;
            return;
        }
        // A frame being saved shows the picture alone.
        if self.window_shot.is_some() {
            return;
        }
        let stale = self
            .info_view
            .as_ref()
            .is_none_or(|v| v.read_at.elapsed() >= REFRESH);
        if stale {
            self.info_view = Some(InfoView {
                read_at: Instant::now(),
                sections: self.video_info(i18n::lang()),
            });
        }
        ctx.request_repaint_after(REFRESH);
        let Some(view) = &self.info_view else { return };
        let mut open = true;
        let mut copy = false;
        let screen = ctx.content_rect();
        egui::Window::new(t(Tr::InfoWindowTitle))
            .id(Id::unique("video-info"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_pos(egui::pos2(screen.left() + 24.0, screen.top() + 60.0))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(crate::ui::dialog_height(ctx))
                    .show(ui, |ui| {
                        // One grid for all sections keeps the values in one column.
                        egui::Grid::new("info-rows")
                            .num_columns(2)
                            .spacing([16.0, 3.0])
                            .show(ui, |ui| {
                                for (i, s) in view.sections.iter().enumerate() {
                                    if i > 0 {
                                        ui.allocate_space(egui::vec2(0.0, 4.0));
                                        ui.end_row();
                                    }
                                    ui.label(
                                        RichText::new(t(s.title)).strong().color(theme::accent()),
                                    );
                                    ui.end_row();
                                    for r in &s.rows {
                                        ui.label(RichText::new(t(r.label)).color(theme::muted()));
                                        ui.add(
                                            egui::Label::new(&r.value)
                                                .wrap_mode(egui::TextWrapMode::Wrap),
                                        );
                                        ui.end_row();
                                    }
                                }
                            });
                    });
                ui.separator();
                let button = ui.button(t(Tr::InfoCopyAll));
                crate::record::place("info_copy_all", button.rect);
                copy = button.clicked();
            });
        if copy {
            let text = as_text(&view.sections, i18n::lang());
            if !self.quiet {
                ctx.copy_text(text);
            }
            self.toast_for(t(Tr::InfoCopied), Duration::from_secs(2));
        }
        if !open {
            self.info_open = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct Table(HashMap<&'static str, &'static str>);

    impl Props for Table {
        fn text(&self, name: &str) -> Option<String> {
            self.0.get(name).map(|s| (*s).to_owned())
        }
        fn num(&self, name: &str) -> Option<f64> {
            self.0.get(name)?.parse().ok()
        }
    }

    fn props() -> Table {
        Table(HashMap::from([
            ("file-format", "mp4"),
            ("duration", "754.2"),
            ("file-size", "52428800"),
            ("video-format", "hevc"),
            ("current-tracks/video/codec-profile", "Main 10"),
            ("video-params/w", "3840"),
            ("video-params/h", "2160"),
            ("container-fps", "23.976023"),
            ("video-bitrate", "18500000"),
            ("video-params/pixelformat", "p010"),
            ("video-params/hw-pixelformat", "cuda"),
            ("video-params/colormatrix", "bt.2020-ncl"),
            ("video-params/primaries", "bt.2020"),
            ("video-params/gamma", "pq"),
            ("hwdec-current", "nvdec"),
            ("audio-codec-name", "aac"),
            ("audio-params/hr-channels", "stereo"),
            ("audio-params/samplerate", "48000"),
            ("audio-bitrate", "128000"),
            ("frame-drop-count", "3"),
            ("decoder-frame-drop-count", "0"),
            ("mistimed-frame-count", "1"),
            ("estimated-vf-fps", "23.98"),
        ]))
    }

    fn input(media: &str) -> Input<'_> {
        Input {
            media,
            title: "Sintel",
            torrserver: false,
            audio_track: Some("#1 · English".into()),
            primary: Some("#3 · English".into()),
            secondary: None,
            cache: None,
        }
    }

    fn value<'a>(sections: &'a [Section], section: &str, row: &str) -> Option<&'a str> {
        sections
            .iter()
            .find(|s| s.key == section)?
            .rows
            .iter()
            .find(|r| r.key == row)
            .map(|r| r.value.as_str())
    }

    #[test]
    fn the_container_is_named_by_the_extension() {
        let family = "mov,mp4,m4a,3gp,3g2,mj2";
        assert_eq!(container(family, r"E:\v\a.MP4"), "mp4");
        assert_eq!(container(family, "https://example.org/v.m4a?x=1"), "m4a");
        assert_eq!(container(family, "https://example.org/watch"), "mov");
        assert_eq!(container("matroska,webm", "a.mkv"), "matroska");
        assert_eq!(container("hls", "a.m3u8"), "hls");
    }

    #[test]
    fn links_lose_everything_but_the_host() {
        assert_eq!(
            source(
                "https://user:pw@rr3.cdn.test/videoplayback?sig=SECRET&key=k",
                false
            ),
            "https://rr3.cdn.test"
        );
        assert_eq!(
            source(
                "http://192.168.1.5:8090/stream/a.mkv?link=HASH&index=1&play",
                true
            ),
            "TorrServer"
        );
        assert_eq!(source("magnet:?xt=urn:btih:HASH&tr=x", false), "magnet");
        assert_eq!(source(r"C:\Videos\a.mkv", false), r"C:\Videos\a.mkv");
        let all = as_text(
            &gather(
                &props(),
                &input("https://www.youtube.com/watch?v=abc&token=SECRET"),
                Lang::En,
            ),
            Lang::En,
        );
        assert!(all.contains("https://www.youtube.com"), "{all}");
        assert!(!all.contains("SECRET") && !all.contains("abc"), "{all}");
    }

    #[test]
    fn values_read_as_people_write_them() {
        let s = gather(&props(), &input(r"C:\v.mp4"), Lang::En);
        assert_eq!(value(&s, "file", "duration"), Some("12:34"));
        assert_eq!(value(&s, "file", "size"), Some("50 MB"));
        assert_eq!(value(&s, "video", "codec"), Some("hevc (Main 10)"));
        assert_eq!(value(&s, "video", "resolution"), Some("3840×2160"));
        assert_eq!(value(&s, "video", "fps"), Some("23.976 fps"));
        assert_eq!(value(&s, "video", "bitrate"), Some("18.5 Mbit/s"));
        assert_eq!(value(&s, "video", "pixel_format"), Some("cuda (p010)"));
        assert_eq!(
            value(&s, "video", "colour"),
            Some("HDR (PQ) · bt.2020-ncl / bt.2020 / pq")
        );
        assert_eq!(value(&s, "audio", "sample_rate"), Some("48 kHz"));
        assert_eq!(value(&s, "audio", "bitrate"), Some("128 kbit/s"));
        assert_eq!(value(&s, "playback", "dropped_frames"), Some("3"));
        assert_eq!(value(&s, "subtitles", "secondary"), Some("None"));
        assert!(s.iter().all(|s| s.key != "network"), "a local file");
        let ru = gather(&props(), &input(r"C:\v.mp4"), Lang::Ru);
        assert_eq!(value(&ru, "video", "fps"), Some("23,976 к/с"));
        assert_eq!(value(&ru, "video", "bitrate"), Some("18,5 Мбит/с"));
    }

    #[test]
    fn software_decoding_and_missing_values() {
        let mut p = props();
        p.0.insert("hwdec-current", "no");
        p.0.remove("video-params/gamma");
        p.0.remove("audio-codec-name");
        p.0.remove("file-size");
        let s = gather(&p, &input(r"C:\v.mp4"), Lang::En);
        assert_eq!(
            value(&s, "video", "hardware_decoding"),
            Some("No (software)")
        );
        assert_eq!(
            value(&s, "video", "colour"),
            Some("SDR · bt.2020-ncl / bt.2020")
        );
        assert!(s.iter().all(|s| s.key != "audio"));
        assert_eq!(value(&s, "file", "size"), None);
        let json = as_json(&s);
        assert_eq!(json["video"]["resolution"], "3840×2160");
    }

    #[test]
    fn a_stream_has_a_network_section() {
        let mut cache = Cache::default();
        cache.state.ahead = Some(31.24);
        cache.state.rate = Some(2.5 * 1024.0 * 1024.0);
        let mut i = input("https://www.youtube.com/watch?v=abc");
        i.cache = Some(&cache);
        let s = gather(&props(), &i, Lang::En);
        assert_eq!(value(&s, "network", "buffered_seconds"), Some("31.2 s"));
        assert_eq!(value(&s, "network", "speed"), Some("2.5 MB/s"));
        assert_eq!(value(&s, "network", "state"), Some("Playing"));
    }
}
