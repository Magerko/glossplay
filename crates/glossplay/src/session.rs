//! Everything that belongs to the media file being played: its subtitle tracks,
//! which of them are shown, and their translations.

use crate::jobs::AsrHandle;
use gp_core::{Cue, Ms, Track, TrackSource, asr_plan, sentences};
use std::ops::Range;
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

/// Machine translation of one cue. Cues are translated a sentence at a time, so every cue of
/// a sentence group is always in the same state.
#[derive(Debug, Clone, PartialEq)]
pub enum Tr {
    Missing,
    Requested,
    Done(String),
    /// Translated as part of a sentence that has changed since (speech recognition added its
    /// continuation). Still shown until the new sentence is translated, so nothing blinks.
    Stale(String),
    /// Stale and asked again.
    Refreshing(String),
}

impl Tr {
    pub fn shown(&self) -> Option<&str> {
        match self {
            Tr::Done(t) | Tr::Stale(t) | Tr::Refreshing(t) => Some(t),
            Tr::Missing | Tr::Requested => None,
        }
    }

    fn wanted(&self) -> bool {
        matches!(self, Tr::Missing | Tr::Stale(_))
    }

    fn awaited(&self) -> bool {
        matches!(self, Tr::Requested | Tr::Refreshing(_))
    }

    fn outdated(&self) -> Tr {
        match self.shown() {
            Some(t) => Tr::Stale(t.to_owned()),
            None => Tr::Missing,
        }
    }
}

/// The playhead this close to a sentence that may still grow means it is translated as it is.
const EDGE_LEAD: Ms = 3_000;

pub struct SubTrack {
    pub label: String,
    pub origin: Origin,
    pub load: Load,
    pub track: Track,
    /// Machine translation of each cue, filled lazily around the playhead.
    pub translation: Vec<Tr>,
    /// Sentence groups over `track.cues`, the unit of translation.
    pub groups: Vec<Range<usize>>,
    /// Speech recognition windows already transcribed, while the track is still growing.
    pub growing: Option<Vec<bool>>,
    /// Bumped whenever all translations are thrown away, so a late answer is not taken.
    pub epoch: u64,
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
            groups: Vec::new(),
            growing: None,
            epoch: 0,
        }
    }

    pub fn set_cues(&mut self, cues: Vec<Cue>) {
        self.translation = vec![Tr::Missing; cues.len()];
        self.groups = sentences::group(&cues);
        self.track.cues = cues;
        self.load = Load::Ready;
    }

    /// Takes the newest transcript from speech recognition. A sentence that is made of the
    /// same cues as before keeps its translation; one that changed (it got its continuation,
    /// or the next window redrew its last cue) is translated again.
    pub fn update_cues(&mut self, cues: Vec<Cue>) {
        let old_cues = std::mem::take(&mut self.track.cues);
        let old_tr = std::mem::take(&mut self.translation);
        let old_groups = std::mem::take(&mut self.groups);

        let mut j = 0;
        let was: Vec<Option<usize>> = cues
            .iter()
            .map(|c| {
                let hit = old_cues.get(j).is_some_and(|o| o == c);
                j += usize::from(hit);
                hit.then(|| j - 1)
            })
            .collect();
        let groups = sentences::group(&cues);
        let mut translation = Vec::with_capacity(cues.len());
        for g in &groups {
            let same = was[g.start].is_some_and(|o| {
                let og = &old_groups[old_groups.partition_point(|r| r.end <= o)];
                og.start == o
                    && og.len() == g.len()
                    && g.clone().zip(og.clone()).all(|(n, o)| was[n] == Some(o))
            });
            for i in g.clone() {
                translation.push(match was[i] {
                    Some(o) if same => old_tr[o].clone(),
                    Some(o) => old_tr[o].outdated(),
                    None => Tr::Missing,
                });
            }
        }
        self.translation = translation;
        self.groups = groups;
        self.track.cues = cues;
        self.load = Load::Ready;
    }

    pub fn is_text(&self) -> bool {
        self.load != Load::Bitmap
    }

    pub fn reset_translations(&mut self) {
        self.translation.fill(Tr::Missing);
        self.epoch += 1;
    }

    /// The sentence group a cue belongs to.
    pub fn group_of(&self, cue: usize) -> usize {
        self.groups.partition_point(|g| g.end <= cue)
    }

    pub fn sentence_text(&self, group: usize) -> String {
        sentences::sentence_text(
            self.track.cues[self.groups[group].clone()]
                .iter()
                .map(|c| c.text.as_str()),
        )
    }

    pub fn needs_translation(&self, group: usize) -> bool {
        self.groups[group]
            .clone()
            .any(|i| self.translation[i].wanted())
    }

    /// A sentence at the edge of what speech recognition has done so far, which its next
    /// window may still continue.
    pub fn may_grow(&self, group: usize) -> bool {
        let Some(done) = &self.growing else {
            return false;
        };
        let g = &self.groups[group];
        let last = &self.track.cues[g.end - 1];
        if g.len() >= sentences::MAX_CUES || sentences::ends_firmly(&last.text) {
            return false;
        }
        let reach = last.end + sentences::LINKED_PAUSE;
        let reach = match self.track.cues.get(g.end) {
            // The next cue is close: both windows around the gap are done, so nothing can be
            // inserted between the two and the break before it is final.
            Some(next) if next.start <= reach => return false,
            Some(next) => reach.min(next.start),
            None => reach,
        };
        (asr_plan::window_of(last.start)..=asr_plan::window_of(reach))
            .any(|k| done.get(k) == Some(&false))
    }

    /// Whether a group should be sent for translation now, with the playhead at `pos`.
    pub fn ready_to_translate(&self, group: usize, pos: Ms) -> bool {
        self.needs_translation(group)
            && (!self.may_grow(group)
                || self.track.cues[self.groups[group].start].start <= pos + EDGE_LEAD)
    }

    /// Marks sentence groups as sent for translation. Keys travel with the job, because by
    /// the time the answer comes speech recognition may have inserted cues before these.
    pub fn request_translations(&mut self, groups: &[usize]) -> Vec<SentenceKey> {
        groups
            .iter()
            .filter_map(|&g| {
                let range = self.groups.get(g)?.clone();
                for i in range.clone() {
                    self.translation[i] = match &self.translation[i] {
                        Tr::Stale(t) | Tr::Refreshing(t) | Tr::Done(t) => Tr::Refreshing(t.clone()),
                        Tr::Missing | Tr::Requested => Tr::Requested,
                    };
                }
                Some(SentenceKey {
                    text: self.sentence_text(g),
                    cues: self.track.cues[range]
                        .iter()
                        .map(|c| CueKey {
                            start: c.start,
                            text: c.text.clone(),
                        })
                        .collect(),
                })
            })
            .collect()
    }

    /// Puts the answers to a translation job on the sentences they were asked for; `None`
    /// means the job failed and the sentences are asked again later.
    pub fn apply_translations(
        &mut self,
        asked: &[SentenceKey],
        results: Option<&[String]>,
        epoch: u64,
    ) {
        if epoch != self.epoch {
            return;
        }
        for (n, key) in asked.iter().enumerate() {
            // Only a group still waiting, and still made of the same cues, takes the answer.
            let Some(range) = self.find_group(&key.cues) else {
                continue;
            };
            match results.and_then(|r| r.get(n)) {
                Some(text) => {
                    let parts = sentences::layout(text, &self.track.cues[range.clone()]);
                    for (i, part) in range.zip(parts) {
                        self.translation[i] = Tr::Done(part);
                    }
                }
                None => {
                    for i in range {
                        self.translation[i] = match &self.translation[i] {
                            Tr::Refreshing(t) => Tr::Stale(t.clone()),
                            _ => Tr::Missing,
                        };
                    }
                }
            }
        }
    }

    fn find_group(&self, keys: &[CueKey]) -> Option<Range<usize>> {
        let first = keys.first()?;
        let from = self.track.cues.partition_point(|c| c.start < first.start);
        let to = self.track.cues.partition_point(|c| c.start <= first.start);
        let start = (from..to)
            .find(|&i| self.track.cues[i].text == first.text && self.translation[i].awaited())?;
        let range = self.groups[self.group_of(start)].clone();
        let same = range.start == start
            && range.len() == keys.len()
            && range.clone().zip(keys).all(|(i, k)| {
                let c = &self.track.cues[i];
                c.start == k.start && c.text == k.text && self.translation[i].awaited()
            });
        same.then_some(range)
    }
}

/// Identifies a cue independently of its position in a track that is still growing.
#[derive(Debug, Clone, PartialEq)]
pub struct CueKey {
    pub start: Ms,
    pub text: String,
}

/// A sentence sent for translation: its text and the cues it was made of.
#[derive(Debug, Clone, PartialEq)]
pub struct SentenceKey {
    pub text: String,
    pub cues: Vec<CueKey>,
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
                let mut lines: Vec<String> = t
                    .track
                    .active_at(pos)
                    .into_iter()
                    .filter_map(|i| t.translation[i].shown().map(str::to_owned))
                    .collect();
                // Two cues of one short sentence can both show it whole; once is enough.
                lines.dedup();
                lines
            }),
        }
    }

    /// The primary track's cues with their translation as text, for export. Cues without a
    /// translation yet are left out.
    pub fn translation_cues(&self) -> Vec<Cue> {
        let Some(t) = self.primary_track() else {
            return Vec::new();
        };
        t.track
            .cues
            .iter()
            .enumerate()
            .filter_map(|(i, c)| {
                Some(Cue {
                    start: c.start,
                    end: c.end,
                    text: self.translation_of(i)?,
                })
            })
            .collect()
    }

    /// The text shown as translation for one primary cue, whatever its source.
    pub fn translation_of(&self, cue: usize) -> Option<String> {
        let t = self.primary_track()?;
        match self.secondary {
            Secondary::Off => None,
            Secondary::Translate => t.translation.get(cue)?.shown().map(str::to_owned),
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
        return gp_core::media_key::url_key(media);
    }
    let read = || -> std::io::Result<String> {
        let mut f = std::fs::File::open(media)?;
        let size = f.metadata()?.len();
        let n = gp_core::media_key::SAMPLE_LEN.min(size) as usize;
        let (mut head, mut tail) = (vec![0; n], vec![0; n]);
        f.read_exact(&mut head)?;
        f.seek(SeekFrom::End(-(n as i64)))?;
        f.read_exact(&mut tail)?;
        Ok(gp_core::media_key::file_key(size, &head, &tail))
    };
    read().unwrap_or_else(|_| gp_core::media_key::url_key(media))
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
            gp_core::SubFormat::from_extension(ext)?;
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

    /// Speech recognition hands over the whole transcript with these cues added.
    fn add(t: &mut SubTrack, new: Vec<Cue>) {
        let mut cues = t.track.cues.clone();
        cues.extend(new);
        cues.sort_by_key(|c| c.start);
        t.update_cues(cues);
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
    fn translation_cues_follow_the_shown_translation() {
        let mut s = Session::new(1, "movie.mkv".into());
        s.tracks.push(ready(
            "en",
            vec![cue(1_000, 2_000, "Hello"), cue(3_000, 4_000, "Bye")],
        ));
        s.tracks
            .push(ready("ru", vec![cue(1_100, 2_100, "Привет")]));
        s.primary = Some(0);
        assert!(s.translation_cues().is_empty());

        s.secondary = Secondary::Track(1);
        assert_eq!(s.translation_cues(), [cue(1_000, 2_000, "Привет")]);

        s.secondary = Secondary::Translate;
        s.tracks[0].translation[1] = Tr::Done("Пока".into());
        assert_eq!(s.translation_cues(), [cue(3_000, 4_000, "Пока")]);
    }

    #[test]
    fn merging_asr_cues_keeps_translations_aligned() {
        let mut t = ready("en", vec![cue(0, 1_000, "A"), cue(5_000, 6_000, "C")]);
        t.translation[1] = Tr::Done("в".into());
        add(&mut t, vec![cue(2_000, 3_000, "B")]);
        let texts: Vec<&str> = t.track.cues.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, ["A", "B", "C"]);
        assert_eq!(
            t.translation,
            [Tr::Missing, Tr::Missing, Tr::Done("в".into())]
        );
    }

    #[test]
    fn translations_land_on_their_cues_after_earlier_cues_are_inserted() {
        let mut t = ready(
            "en",
            vec![cue(10_000, 11_000, "A"), cue(20_000, 21_000, "C")],
        );
        let keys = t.request_translations(&[0, 1]);
        add(
            &mut t,
            vec![cue(1_000, 2_000, "B0"), cue(15_000, 16_000, "B")],
        );
        assert_eq!(
            t.translation,
            [Tr::Missing, Tr::Requested, Tr::Missing, Tr::Requested]
        );
        t.apply_translations(&keys, Some(&["а".into(), "в".into()]), 0);
        let pairs: Vec<(&str, &Tr)> = t
            .track
            .cues
            .iter()
            .map(|c| c.text.as_str())
            .zip(&t.translation)
            .collect();
        assert_eq!(
            pairs,
            [
                ("B0", &Tr::Missing),
                ("A", &Tr::Done("а".into())),
                ("B", &Tr::Missing),
                ("C", &Tr::Done("в".into())),
            ]
        );
    }

    #[test]
    fn failed_short_or_stale_translations_release_their_cues() {
        let mut t = ready(
            "en",
            vec![
                cue(0, 1_000, "A"),
                cue(0, 1_000, "A"),
                cue(5_000, 6_000, "C"),
            ],
        );
        assert_eq!(t.groups, [0..1, 1..2, 2..3], "cues on screen together");
        let stale = t.request_translations(&[2]);
        let keys = t.request_translations(&[0, 1]);
        t.apply_translations(&keys, None, 0);
        assert_eq!(t.translation[..2], [Tr::Missing, Tr::Missing]);

        let keys = t.request_translations(&[0, 1]);
        t.apply_translations(&keys, Some(&["x".into()]), 0);
        assert_eq!(t.translation[..2], [Tr::Done("x".into()), Tr::Missing]);

        // A settings change resets the cues while the job for "c" is still out, and "c" is
        // asked again before the old answer comes.
        t.reset_translations();
        let fresh = t.request_translations(&[2]);
        t.apply_translations(&stale, Some(&["old".into()]), 0);
        assert_eq!(t.translation[2], Tr::Requested);
        t.apply_translations(&fresh, Some(&["new".into()]), t.epoch);
        assert_eq!(t.translation[2], Tr::Done("new".into()));
    }

    fn shown(t: &SubTrack) -> Vec<Option<&str>> {
        t.translation.iter().map(Tr::shown).collect()
    }

    #[test]
    fn a_sentence_is_translated_once_and_cut_across_its_cues() {
        let mut t = ready(
            "en",
            vec![
                cue(0, 1_000, "I told him that"),
                cue(1_100, 2_000, "he didn't do it,"),
                cue(2_100, 3_000, "she said."),
                cue(3_100, 4_000, "Then she left."),
            ],
        );
        assert_eq!(t.groups, [0..3, 3..4]);
        let asked = t.request_translations(&[0]);
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0].text, "I told him that he didn't do it, she said.");
        assert_eq!(t.translation[3], Tr::Missing);
        let whole = "Я сказала ему, что он этого не делал, — сказала она.";
        t.apply_translations(&asked, Some(&[whole.into()]), 0);
        let parts: Vec<&str> = shown(&t)[..3].iter().map(|p| p.unwrap()).collect();
        assert!(parts.iter().all(|p| !p.is_empty()), "{parts:?}");
        assert_eq!(parts.join(" "), whole);
    }

    #[test]
    fn a_sentence_at_the_edge_of_recognition_waits_then_is_redone() {
        // Windows of 30 s: the first is transcribed, the second not yet.
        let mut t = ready(
            "en",
            vec![
                cue(20_000, 22_000, "It was late."),
                cue(27_000, 28_500, "And then he"),
            ],
        );
        t.growing = Some(vec![true, false, false]);
        assert!(!t.may_grow(0) && t.may_grow(1));
        assert!(t.ready_to_translate(0, 0));
        assert!(
            !t.ready_to_translate(1, 20_000),
            "the next window may continue it"
        );
        assert!(
            t.ready_to_translate(1, 25_000),
            "the playhead is about to reach it"
        );

        let asked = t.request_translations(&[0, 1]);
        t.apply_translations(
            &asked,
            Some(&["Было поздно.".into(), "А потом он".into()]),
            0,
        );

        // The next window brings the end of the sentence.
        t.growing.as_mut().unwrap()[1] = true;
        add(
            &mut t,
            vec![
                cue(29_000, 31_000, "went home."),
                cue(40_000, 41_000, "Bye."),
            ],
        );
        assert_eq!(t.groups, [0..1, 1..3, 3..4]);
        assert_eq!(
            t.translation[0],
            Tr::Done("Было поздно.".into()),
            "untouched"
        );
        assert_eq!(
            shown(&t)[1..3],
            [Some("А потом он"), None],
            "the old text stays until the new one comes"
        );
        assert!(t.needs_translation(1) && t.needs_translation(2));
        assert!(!t.needs_translation(0));

        let asked = t.request_translations(&[1]);
        assert_eq!(asked[0].text, "And then he went home.");
        assert_eq!(t.translation[1], Tr::Refreshing("А потом он".into()));
        // An answer for the sentence as it was before is not taken.
        let old = SentenceKey {
            text: "And then he".into(),
            cues: vec![CueKey {
                start: 27_000,
                text: "And then he".into(),
            }],
        };
        t.apply_translations(&[old], Some(&["А потом он".into()]), 0);
        assert_eq!(t.translation[2], Tr::Requested);
        t.apply_translations(&asked, Some(&["А потом он пошёл домой.".into()]), 0);
        // Short, and no comma to cut at: both cues show the whole sentence.
        assert_eq!(shown(&t)[1..3], [Some("А потом он пошёл домой."); 2]);

        // Recognition finished: nothing is held back any more.
        t.growing = None;
        assert!(!t.may_grow(2));
    }

    #[test]
    fn merging_keeps_unchanged_sentences_and_their_requests() {
        let mut t = ready(
            "en",
            vec![cue(0, 1_000, "One."), cue(10_000, 11_000, "Two.")],
        );
        let asked = t.request_translations(&[0, 1]);
        add(&mut t, vec![cue(5_000, 6_000, "Between.")]);
        assert_eq!(
            t.translation,
            [Tr::Requested, Tr::Missing, Tr::Requested],
            "a request still out is not lost"
        );
        t.apply_translations(&asked, Some(&["Раз.".into(), "Два.".into()]), 0);
        assert_eq!(shown(&t), [Some("Раз."), None, Some("Два.")]);
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
        let dir = std::env::temp_dir().join(format!("gp-sidecar-{}", std::process::id()));
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
