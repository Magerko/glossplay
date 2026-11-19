//! Everything that belongs to the media file being played: its subtitle tracks,
//! which of them are shown, and their translations.

use crate::jobs::AsrHandle;
use lp_core::{Cue, Ms, Track, TrackSource};
use std::time::Instant;

#[derive(Debug, Clone, PartialEq)]
pub enum Origin {
    Embedded { mpv_id: i64, stream: u32 },
    External { path: String },
    Asr,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Load {
    /// Known to exist but not read yet; a track is only read once it is selected.
    NotLoaded,
    Loading,
    Ready,
    Failed(String),
    /// Image subtitles: mpv draws them, there is no text to work with.
    Bitmap,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Tr {
    Missing,
    Requested,
    Done(String),
}

pub struct SubTrack {
    pub label: String,
    pub origin: Origin,
    pub load: Load,
    pub track: Track,
    /// Machine translation of each cue, filled lazily around the playhead.
    pub translation: Vec<Tr>,
}

impl SubTrack {
    pub fn new(label: String, lang: Option<String>, origin: Origin, load: Load) -> Self {
        let source = match &origin {
            Origin::Embedded { stream, .. } => TrackSource::Embedded {
                stream_index: *stream,
            },
            Origin::External { path } => TrackSource::External { path: path.clone() },
            Origin::Asr => TrackSource::Asr {
                engine: "whisper.cpp".into(),
                model: String::new(),
            },
        };
        SubTrack {
            label,
            origin,
            load,
            track: Track::new(source, lang, None, Vec::new()),
            translation: Vec::new(),
        }
    }

    pub fn set_cues(&mut self, cues: Vec<Cue>) {
        self.translation = vec![Tr::Missing; cues.len()];
        self.track.cues = cues;
        self.load = Load::Ready;
    }

    /// Adds cues from speech recognition, keeping existing translations aligned.
    pub fn merge_cues(&mut self, new: Vec<Cue>) {
        let old: Vec<(Cue, Tr)> = self
            .track
            .cues
            .drain(..)
            .zip(self.translation.drain(..))
            .collect();
        let mut cues: Vec<Cue> = old.iter().map(|(c, _)| c.clone()).collect();
        lp_core::asr_plan::merge(&mut cues, new);
        let mut old = old.into_iter().peekable();
        self.translation = cues
            .iter()
            .map(|c| match old.peek() {
                Some((oc, _)) if oc == c => old.next().unwrap().1,
                _ => Tr::Missing,
            })
            .collect();
        self.track.cues = cues;
        self.load = Load::Ready;
    }

    pub fn is_text(&self) -> bool {
        self.load != Load::Bitmap
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Secondary {
    Off,
    Track(usize),
    /// Machine translation of the primary track.
    Translate,
}

pub struct Session {
    pub generation: u64,
    pub media: String,
    pub title: String,
    pub media_key: String,
    pub tracks: Vec<SubTrack>,
    pub primary: Option<usize>,
    pub secondary: Secondary,
    pub asr: Option<AsrHandle>,
    pub asr_status: String,
    pub asr_running: bool,
    /// When translation last failed; requests pause for a while after a failure.
    pub translate_failed_at: Option<Instant>,
    pub tracks_discovered: bool,
}

pub struct Line {
    pub cue: usize,
    pub start: Ms,
    pub end: Ms,
    pub text: String,
}

impl Session {
    pub fn new(generation: u64, media: String) -> Session {
        let is_url = media.contains("://");
        let title = if is_url {
            media.clone()
        } else {
            std::path::Path::new(&media)
                .file_stem()
                .map_or(media.clone(), |s| s.to_string_lossy().into_owned())
        };
        Session {
            generation,
            media_key: media_key(&media),
            media,
            title,
            tracks: Vec::new(),
            primary: None,
            secondary: Secondary::Off,
            asr: None,
            asr_status: String::new(),
            asr_running: false,
            translate_failed_at: None,
            tracks_discovered: false,
        }
    }

    pub fn is_url(&self) -> bool {
        self.media.contains("://")
    }

    pub fn primary_track(&self) -> Option<&SubTrack> {
        self.tracks
            .get(self.primary?)
            .filter(|t| t.load == Load::Ready)
    }

    pub fn primary_lines(&self, pos: Ms) -> Vec<Line> {
        self.primary_track()
            .map_or(Vec::new(), |t| lines_of(t, pos))
    }

    pub fn secondary_lines(&self, pos: Ms) -> Vec<String> {
        match self.secondary {
            Secondary::Off => Vec::new(),
            Secondary::Track(i) => self
                .tracks
                .get(i)
                .filter(|t| t.load == Load::Ready)
                .map_or(Vec::new(), |t| {
                    lines_of(t, pos).into_iter().map(|l| l.text).collect()
                }),
            Secondary::Translate => self.primary_track().map_or(Vec::new(), |t| {
                t.track
                    .active_at(pos)
                    .into_iter()
                    .filter_map(|i| match &t.translation[i] {
                        Tr::Done(text) => Some(text.clone()),
                        _ => None,
                    })
                    .collect()
            }),
        }
    }

    /// The text shown as translation for one primary cue, whatever its source.
    pub fn translation_of(&self, cue: usize) -> Option<String> {
        let t = self.primary_track()?;
        match self.secondary {
            Secondary::Off => None,
            Secondary::Translate => match t.translation.get(cue)? {
                Tr::Done(text) => Some(text.clone()),
                _ => None,
            },
            Secondary::Track(i) => {
                let c = t.track.cues.get(cue)?;
                let other = self.tracks.get(i).filter(|t| t.load == Load::Ready)?;
                let text: Vec<&str> = other
                    .track
                    .active_at((c.start + c.end) / 2)
                    .into_iter()
                    .map(|j| other.track.cues[j].text.as_str())
                    .collect();
                (!text.is_empty()).then(|| text.join(" "))
            }
        }
    }
}

fn lines_of(t: &SubTrack, pos: Ms) -> Vec<Line> {
    t.track
        .active_at(pos)
        .into_iter()
        .map(|i| {
            let c = &t.track.cues[i];
            Line {
                cue: i,
                start: c.start,
                end: c.end,
                text: c.text.clone(),
            }
        })
        .collect()
}

fn media_key(media: &str) -> String {
    use std::io::{Read, Seek, SeekFrom};
    if media.contains("://") {
        return lp_core::media_key::url_key(media);
    }
    let read = || -> std::io::Result<String> {
        let mut f = std::fs::File::open(media)?;
        let size = f.metadata()?.len();
        let n = lp_core::media_key::SAMPLE_LEN.min(size) as usize;
        let (mut head, mut tail) = (vec![0; n], vec![0; n]);
        f.read_exact(&mut head)?;
        f.seek(SeekFrom::End(-(n as i64)))?;
        f.read_exact(&mut tail)?;
        Ok(lp_core::media_key::file_key(size, &head, &tail))
    };
    read().unwrap_or_else(|_| lp_core::media_key::url_key(media))
}

/// Subtitle files lying next to the video: `movie.srt`, `movie.en.srt`, `movie.ru.ass`…
pub fn sidecar_subtitles(media: &str) -> Vec<(String, Option<String>)> {
    let path = std::path::Path::new(media);
    let (Some(dir), Some(stem)) = (
        path.parent(),
        path.file_stem().map(|s| s.to_string_lossy().into_owned()),
    ) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<(String, Option<String>)> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let (base, ext) = name.rsplit_once('.')?;
            lp_core::SubFormat::from_extension(ext)?;
            let rest = base.strip_prefix(stem.as_str())?;
            let lang = match rest.strip_prefix('.') {
                Some(tag) if !tag.is_empty() => {
                    Some(normalize_lang(tag.rsplit('.').next().unwrap_or(tag)))
                }
                None if rest.is_empty() => None,
                _ => return None,
            };
            Some((e.path().to_string_lossy().into_owned(), lang))
        })
        .collect();
    found.sort();
    found
}

/// mpv hands over the subtitles of online videos as an EDL entry:
/// `edl://!no_clip;!delay_open,media_type=sub,codec=vtt;%<length>%<url>`.
pub fn subtitle_url(name: &str) -> String {
    let Some(rest) = name.strip_prefix("edl://") else {
        return name.to_owned();
    };
    let Some(at) = rest.rfind(";%") else {
        return name.to_owned();
    };
    let escaped = &rest[at + 2..];
    match escaped.split_once('%') {
        Some((len, url)) => match len.parse::<usize>() {
            Ok(n) if url.is_char_boundary(n.min(url.len())) => url[..n.min(url.len())].to_owned(),
            _ => url.to_owned(),
        },
        None => name.to_owned(),
    }
}

/// Maps the language tags found in the wild ("eng", "en-US", "rus") to two-letter codes.
pub fn normalize_lang(tag: &str) -> String {
    let tag = tag.to_ascii_lowercase();
    let primary = tag.split(['-', '_']).next().unwrap_or(&tag);
    let short = match primary {
        "eng" => "en",
        "rus" => "ru",
        "ukr" => "uk",
        "jpn" => "ja",
        "zho" | "chi" | "cmn" => "zh",
        "kor" => "ko",
        "ger" | "deu" => "de",
        "fre" | "fra" => "fr",
        "spa" => "es",
        "ita" => "it",
        "por" => "pt",
        "pol" => "pl",
        "tur" => "tr",
        "ara" => "ar",
        "hin" => "hi",
        "nld" | "dut" => "nl",
        "swe" => "sv",
        other => other,
    };
    short.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cue(start: Ms, end: Ms, text: &str) -> Cue {
        Cue {
            start,
            end,
            text: text.into(),
        }
    }

    fn ready(lang: &str, cues: Vec<Cue>) -> SubTrack {
        let mut t = SubTrack::new(lang.into(), Some(lang.into()), Origin::Asr, Load::Loading);
        t.set_cues(cues);
        t
    }

    #[test]
    fn dual_lines_from_two_tracks_and_from_translation() {
        let mut s = Session::new(1, "movie.mkv".into());
        s.tracks.push(ready(
            "en",
            vec![cue(29_000, 31_000, "Hello"), cue(40_000, 41_000, "Bye")],
        ));
        s.tracks
            .push(ready("ru", vec![cue(29_500, 31_500, "Привет")]));
        s.primary = Some(0);
        s.secondary = Secondary::Track(1);
        assert_eq!(s.primary_lines(30_000)[0].text, "Hello");
        assert_eq!(s.secondary_lines(30_000), ["Привет"]);
        assert_eq!(s.translation_of(0).as_deref(), Some("Привет"));
        assert_eq!(s.translation_of(1), None);
        assert!(s.primary_lines(35_000).is_empty());

        s.secondary = Secondary::Translate;
        assert!(s.secondary_lines(30_000).is_empty(), "not translated yet");
        s.tracks[0].translation[0] = Tr::Done("Здравствуй".into());
        assert_eq!(s.secondary_lines(30_000), ["Здравствуй"]);
    }

    #[test]
    fn merging_asr_cues_keeps_translations_aligned() {
        let mut t = ready("en", vec![cue(0, 1_000, "a"), cue(5_000, 6_000, "c")]);
        t.translation[1] = Tr::Done("в".into());
        t.merge_cues(vec![cue(2_000, 3_000, "b")]);
        let texts: Vec<&str> = t.track.cues.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, ["a", "b", "c"]);
        assert_eq!(
            t.translation,
            [Tr::Missing, Tr::Missing, Tr::Done("в".into())]
        );
    }

    #[test]
    fn online_subtitle_urls_are_unwrapped() {
        let url = "https://example.com/api/timedtext?v=abc&fmt=vtt;x=1";
        let edl = format!(
            "edl://!no_clip;!delay_open,media_type=sub,codec=vtt;%{}%{url}",
            url.len()
        );
        assert_eq!(subtitle_url(&edl), url);
        assert_eq!(subtitle_url("C:/films/a.srt"), "C:/films/a.srt");
        assert_eq!(subtitle_url(url), url);
    }

    #[test]
    fn language_tags() {
        assert_eq!(normalize_lang("eng"), "en");
        assert_eq!(normalize_lang("en-US"), "en");
        assert_eq!(normalize_lang("JPN"), "ja");
        assert_eq!(normalize_lang("uk"), "uk");
    }

    #[test]
    fn sidecar_files_are_found_with_languages() {
        let dir = std::env::temp_dir().join(format!("lp-sidecar-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in [
            "My Film.mkv",
            "My Film.srt",
            "My Film.en.srt",
            "My Film.rus.ass",
            "My Film 2.srt",
            "My Film.txt",
        ] {
            std::fs::write(dir.join(name), "x").unwrap();
        }
        let found = sidecar_subtitles(&dir.join("My Film.mkv").to_string_lossy());
        let langs: Vec<Option<&str>> = found.iter().map(|(_, l)| l.as_deref()).collect();
        assert_eq!(found.len(), 3, "{found:?}");
        assert!(
            langs.contains(&None) && langs.contains(&Some("en")) && langs.contains(&Some("ru"))
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
