//! Splits a media file into fixed windows for speech recognition, so windows can be
//! transcribed in any order (the one under the playhead first), and puts what the windows
//! heard back together into cues.

use crate::sentences::{ends_sentence, is_cjk};
use crate::track::{Cue, Ms};
use std::collections::BTreeMap;

pub const WINDOW: Ms = 30_000;
/// Audio before the window, so the first words are not cut mid-syllable.
pub const PRE_ROLL: Ms = 3_000;
/// Audio after the window, so a sentence that starts inside it can finish.
pub const POST_ROLL: Ms = 8_000;

/// Cached transcripts are keyed by this as well as by the engine, so cues saved by a build
/// that cut windows another way are never mixed with words.
pub const CACHE_FORMAT: &str = "words1";

/// A longer silence between two words always starts a new cue.
const MAX_GAP: Ms = 1_500;
/// A cue longer than this is cut in two at its best place.
const LONG_CHARS: usize = 100;
const LONG_MS: Ms = 10_000;

pub fn window_count(duration: Ms) -> usize {
    (duration.max(0) as usize).div_ceil(WINDOW as usize)
}

pub fn window_of(pos: Ms) -> usize {
    (pos.max(0) / WINDOW) as usize
}

/// The audio range to decode for window `k`: (start, length).
pub fn audio_range(k: usize) -> (Ms, Ms) {
    let window_start = k as Ms * WINDOW;
    let start = (window_start - PRE_ROLL).max(0);
    (start, window_start + WINDOW + POST_ROLL - start)
}

/// The window under the playhead if it is still missing, otherwise the next missing one
/// after it, otherwise the earliest missing one.
pub fn next_window(done: &[bool], playhead: Ms) -> Option<usize> {
    let from = window_of(playhead).min(done.len());
    (from..done.len()).chain(0..from).find(|&k| !done[k])
}

/// A token as whisper.cpp reports it: a piece of a word, with a leading space when it
/// starts one, or a punctuation mark. Times are relative to the submitted audio.
#[derive(Debug, Clone, PartialEq)]
pub struct Piece {
    pub start: Ms,
    pub end: Ms,
    pub text: String,
}

/// A segment of Whisper's answer, relative to the submitted audio.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub start: Ms,
    pub end: Ms,
    pub text: String,
    /// Empty when the server did not report them.
    pub pieces: Vec<Piece>,
}

/// A word with its punctuation, in media time.
#[derive(Debug, Clone, PartialEq)]
pub struct Word {
    pub start: Ms,
    pub end: Ms,
    pub text: String,
    /// The first word of a Whisper segment.
    pub opens: bool,
    /// Written apart from the word before it (not so inside CJK text).
    pub spaced: bool,
}

impl Word {
    fn mid(&self) -> Ms {
        (self.start + self.end) / 2
    }

    fn key(&self) -> String {
        self.text
            .chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect()
    }
}

/// The words of one window's answer, in media time. Whisper cuts segments and even words
/// into pieces ("pol" / "itely."), so pieces are glued back into whole words first.
pub fn words(audio_start: Ms, segments: &[Segment]) -> Vec<Word> {
    let mut out: Vec<Word> = Vec::new();
    for seg in segments {
        let pieces = if seg.pieces.iter().any(|p| !p.text.trim().is_empty()) {
            fit(seg)
        } else {
            spread(seg)
        };
        let mut open = true;
        for p in pieces {
            let text = p.text.trim();
            let Some(first) = text.chars().next() else {
                continue;
            };
            let spaced = p.text.starts_with(char::is_whitespace);
            let (start, end) = (p.start + audio_start, p.end + audio_start);
            let starts_word = spaced || (first.is_alphanumeric() && is_cjk(first));
            match out.last_mut() {
                Some(last) if !starts_word => {
                    last.text.push_str(text);
                    // Punctuation carries the pause after the word in its time; the word does
                    // not last longer for it.
                    if first.is_alphanumeric() {
                        last.end = last.end.max(end);
                    }
                }
                _ => {
                    out.push(Word {
                        start,
                        end: end.max(start),
                        text: text.to_owned(),
                        opens: open,
                        spaced: spaced || !is_cjk(first),
                    });
                    open = false;
                }
            }
        }
    }
    out
}

/// The pieces of a segment moved into its time. With voice activity detection on,
/// whisper-server puts segments back on the audio's timeline but leaves word times on the
/// timeline with the silences cut out, seconds earlier by the end of a window.
fn fit(seg: &Segment) -> Vec<Piece> {
    let from = seg.pieces.iter().map(|p| p.start).min().unwrap_or(0);
    let to = seg.pieces.iter().map(|p| p.end).max().unwrap_or(from);
    let map = |t: Ms| {
        if to > from {
            seg.start + (t - from) * (seg.end - seg.start) / (to - from)
        } else {
            seg.start + t - from
        }
    };
    seg.pieces
        .iter()
        .map(|p| Piece {
            start: map(p.start),
            end: map(p.end),
            text: p.text.clone(),
        })
        .collect()
}

/// Words of a segment without word times, spread over it by length.
fn spread(seg: &Segment) -> Vec<Piece> {
    let parts: Vec<&str> = seg.text.split_whitespace().collect();
    let total: usize = parts
        .iter()
        .map(|w| w.chars().count())
        .sum::<usize>()
        .max(1);
    let span = seg.end - seg.start;
    let mut at = 0;
    parts
        .iter()
        .map(|w| {
            let len = w.chars().count();
            let p = Piece {
                start: seg.start + span * at as Ms / total as Ms,
                end: seg.start + span * (at + len) as Ms / total as Ms,
                text: format!(" {w}"),
            };
            at += len;
            p
        })
        .collect()
}

/// Everything heard in the finished windows, each with the overlap into its neighbours.
/// Cues are worked out from all of it at once, so they do not depend on the order the
/// windows came in, and adding the same window again changes nothing.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Transcript {
    windows: BTreeMap<usize, Vec<Word>>,
}

impl Transcript {
    pub fn insert(&mut self, k: usize, words: Vec<Word>) {
        self.windows.insert(k, words);
    }

    pub fn windows(&self) -> impl Iterator<Item = (usize, &[Word])> {
        self.windows.iter().map(|(k, w)| (*k, w.as_slice()))
    }

    pub fn retain(&mut self, keep: impl Fn(usize) -> bool) {
        self.windows.retain(|k, _| keep(*k));
    }

    pub fn cues(&self) -> Vec<Cue> {
        // Each window owns the words whose middle is inside it, unless it and its neighbour
        // agree on a better place to hand over.
        let mut ranges: BTreeMap<usize, (usize, usize)> = self
            .windows
            .iter()
            .map(|(&k, w)| {
                let (lo, hi) = (k as Ms * WINDOW, (k as Ms + 1) * WINDOW);
                let from = w.iter().position(|x| x.mid() >= lo).unwrap_or(w.len());
                let to = w.iter().position(|x| x.mid() >= hi).unwrap_or(w.len());
                (k, (from, to.max(from)))
            })
            .collect();
        for (&k, a) in &self.windows {
            let Some(b) = self.windows.get(&(k + 1)) else {
                continue;
            };
            if let Some((i, j)) = seam(a, b, (k as Ms + 1) * WINDOW) {
                let (af, bt) = (ranges[&k].0, ranges[&(k + 1)].1);
                ranges.insert(k, (af, i.max(af)));
                ranges.insert(k + 1, (j.min(bt), bt));
            }
        }

        let mut stream: Vec<Word> = Vec::new();
        for (&k, w) in &self.windows {
            let (from, to) = ranges[&k];
            for (n, word) in w[from..to].iter().enumerate() {
                let mut word = word.clone();
                if n == 0 && k > 0 {
                    // Where the windows hand over, a segment that either of them started here
                    // starts a cue; one that runs through the border stays one cue.
                    if let (Some(prev), Some(&(_, pt))) =
                        (self.windows.get(&(k - 1)), ranges.get(&(k - 1)))
                    {
                        word.opens |= prev.get(pt).is_some_and(|p| p.opens);
                    }
                }
                stream.push(word);
            }
        }
        build(&stream)
    }
}

/// Where two neighbouring windows hand over: the first word left out of the earlier one and
/// the first word taken from the later one. Both heard the audio around the border; two
/// words in a row that both heard at the same time are text they agree on, so cutting
/// there neither drops nor repeats anything. `None` when they agree nowhere near it.
fn seam(a: &[Word], b: &[Word], border: Ms) -> Option<(usize, usize)> {
    let (ka, kb): (Vec<String>, Vec<String>) = (
        a.iter().map(Word::key).collect(),
        b.iter().map(Word::key).collect(),
    );
    // Whisper is least sure at the very edges of the audio it was given.
    let near = |w: &Word| (border - 2_000..border + 5_000).contains(&w.mid());
    let mut best: Option<(Ms, usize, usize)> = None;
    for i in 0..a.len().saturating_sub(1) {
        if !near(&a[i]) || ka[i].is_empty() || ka[i + 1].is_empty() {
            continue;
        }
        for j in 0..b.len().saturating_sub(1) {
            let agree = ka[i] == kb[j]
                && ka[i + 1] == kb[j + 1]
                && (a[i].mid() - b[j].mid()).abs() <= 1_500;
            if !agree {
                continue;
            }
            let off = ((a[i].mid() + b[j].mid()) / 2 - border).abs();
            if best.is_none_or(|(o, _, _)| off < o) {
                best = Some((off, i, j));
            }
        }
    }
    best.map(|(_, i, j)| (i, j))
}

/// Cues from a stream of words: Whisper's segments, also cut at the end of a sentence and
/// at long silences, and cut in two when too long to read.
fn build(words: &[Word]) -> Vec<Cue> {
    let mut groups: Vec<&[Word]> = Vec::new();
    let mut start = 0;
    for i in 1..words.len() {
        let (last, w) = (&words[i - 1], &words[i]);
        let cur = &words[start..i];
        let sentence = stops(&last.text)
            && w.text
                .chars()
                .find(|c| c.is_alphanumeric())
                .is_some_and(|c| !c.is_lowercase())
            && last.end - cur[0].start >= 1_000
            && text_of(cur).chars().count() >= 8;
        // "...well enough. When" / "I find a thing": a word left over from a segment cut
        // after a full stop belongs to the segment it runs into.
        let stub = cur.len() <= 2
            && !stops(&last.text)
            && text_of(cur).chars().count() < 8
            && w.start - last.end < 500
            && start > 0
            && stops(&words[start - 1].text);
        if (w.opens && !stub) || w.start - last.end > MAX_GAP || sentence {
            groups.push(cur);
            start = i;
        }
    }
    if start < words.len() {
        groups.push(&words[start..]);
    }
    let mut out = Vec::new();
    for g in groups {
        split_long(g, &mut out);
    }
    out
}

/// A word that ends a sentence; "W." in "Roger W. Barnett" or "Mr." does not.
fn stops(word: &str) -> bool {
    let body = word.trim_end_matches(|c: char| !c.is_alphanumeric());
    let from = body
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_alphabetic())
        .last()
        .map_or(body.len(), |(i, _)| i);
    let core = &body[from..];
    let initial = core.chars().count() == 1 && core.chars().all(char::is_uppercase);
    let title = ["Mr", "Mrs", "Ms", "Dr", "St", "Jr", "Sr", "Prof", "Mt"].contains(&core);
    ends_sentence(word) && !(word.trim_end().ends_with('.') && (initial || title))
}

fn text_of(words: &[Word]) -> String {
    let mut out = String::new();
    for w in words {
        if w.spaced && !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&w.text);
    }
    out
}

fn split_long(words: &[Word], out: &mut Vec<Cue>) {
    let text = text_of(words);
    let (first, last) = (&words[0], &words[words.len() - 1]);
    let len = text.chars().count();
    if words.len() >= 2 && (len > LONG_CHARS || last.end - first.start > LONG_MS) {
        let mut at = 0;
        let best = (1..words.len())
            .map(|b| {
                at += words[b - 1].text.chars().count() + 1;
                let share = at as f64 / len.max(1) as f64;
                let end = words[b - 1]
                    .text
                    .trim_end_matches(['"', '\'', '”', '’', '»', ')']);
                let punct = if stops(end) {
                    3.0
                } else if end.ends_with([',', ';', ':', '—', '–']) {
                    2.0
                } else {
                    0.0
                };
                let gap = (words[b].start - words[b - 1].end).max(0) as f64 / 400.0;
                (b, punct + gap.min(3.0) - (share - 0.5).abs() * 8.0)
            })
            .max_by(|x, y| x.1.total_cmp(&y.1).then(y.0.cmp(&x.0)))
            .map_or(1, |(b, _)| b);
        split_long(&words[..best], out);
        split_long(&words[best..], out);
        return;
    }
    // Whisper stretches the last word before music or silence until the next speech.
    // Nobody reads slower than this, so I end the line earlier.
    let readable = 2_000 + 90 * len as Ms;
    out.push(Cue {
        start: first.start,
        end: last.end.min(first.start + readable).max(first.start + 300),
        text,
    });
}

/// End of the fully transcribed prefix; this is what gets cached between runs.
pub fn covered_prefix(done: &[bool], duration: Ms) -> Ms {
    let n = done.iter().take_while(|d| **d).count();
    if n == done.len() {
        duration
    } else {
        n as Ms * WINDOW
    }
}

/// Windows already transcribed, from what was cached. A saved mask counts only if it fits the
/// current window count; otherwise I fall back to the prefix up to `covered`. The flag says
/// whether the mask was used, i.e. whether cues past `covered` can be trusted.
pub fn restore_done(duration: Ms, covered: Ms, mask: &[bool]) -> (Vec<bool>, bool) {
    let total = window_count(duration);
    if !mask.is_empty() && mask.len() == total {
        return (mask.to_vec(), true);
    }
    let n = if covered >= duration {
        total
    } else {
        window_of(covered)
    };
    let mut done = vec![false; total];
    done.iter_mut().take(n).for_each(|d| *d = true);
    (done, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_and_ranges() {
        assert_eq!(window_count(0), 0);
        assert_eq!(window_count(30_000), 1);
        assert_eq!(window_count(30_001), 2);
        assert_eq!(audio_range(0), (0, 38_000));
        assert_eq!(audio_range(2), (57_000, 41_000));
    }

    #[test]
    fn playhead_window_goes_first_then_wraps() {
        let mut done = vec![false; 5];
        assert_eq!(next_window(&done, 70_000), Some(2));
        done[2] = true;
        assert_eq!(next_window(&done, 70_000), Some(3));
        done[3] = true;
        done[4] = true;
        assert_eq!(next_window(&done, 70_000), Some(0));
        assert_eq!(next_window(&[true, true], 0), None);
        assert_eq!(
            next_window(&done, 999_000_000),
            Some(0),
            "playhead past the end"
        );
    }

    fn piece(start: Ms, end: Ms, text: &str) -> Piece {
        Piece {
            start,
            end,
            text: text.into(),
        }
    }

    fn seg(pieces: Vec<Piece>) -> Segment {
        Segment {
            start: pieces[0].start,
            end: pieces[pieces.len() - 1].end,
            text: pieces.iter().map(|p| p.text.as_str()).collect(),
            pieces,
        }
    }

    #[test]
    fn pieces_are_glued_into_words() {
        // From a real answer: a word cut between two segments, punctuation as its own piece.
        let segs = [
            seg(vec![
                piece(0, 300, " I"),
                piece(300, 700, " beg"),
                piece(700, 900, ","),
                piece(4_220, 4_500, " pol"),
            ]),
            seg(vec![piece(4_500, 5_000, "itely"), piece(5_000, 5_180, ".")]),
            seg(vec![
                piece(6_460, 6_460, " Did"),
                piece(6_460, 6_500, " \""),
                piece(6_500, 6_900, "you"),
                piece(6_900, 7_200, "?\""),
            ]),
        ];
        let w = words(10_000, &segs);
        let texts: Vec<(&str, bool)> = w.iter().map(|w| (w.text.as_str(), w.opens)).collect();
        assert_eq!(
            texts,
            [
                ("I", true),
                ("beg,", false),
                ("politely.", false),
                ("Did", true),
                ("\"you?\"", false)
            ]
        );
        assert_eq!((w[2].start, w[2].end), (14_220, 15_000));
        assert_eq!(w[1].end, 10_700, "a comma does not make a word longer");

        let ja = [seg(vec![
            piece(0, 200, "彼"),
            piece(200, 400, "は"),
            piece(400, 500, "。"),
        ])];
        let w = words(0, &ja);
        assert_eq!(text_of(&w), "彼は。");

        // With voice activity detection the words come on a timeline without the silences.
        let vad = [Segment {
            start: 20_000,
            end: 22_000,
            text: " Hello there.".into(),
            pieces: vec![
                piece(15_000, 15_500, " Hello"),
                piece(15_500, 16_000, " there."),
            ],
        }];
        let w = words(0, &vad);
        assert_eq!(
            w.iter().map(|w| (w.start, w.end)).collect::<Vec<_>>(),
            [(20_000, 21_000), (21_000, 22_000)]
        );

        // A server without word times: words are spread over the segment.
        let plain = [Segment {
            start: 1_000,
            end: 2_000,
            text: " ab cd".into(),
            pieces: Vec::new(),
        }];
        let w = words(0, &plain);
        assert_eq!(
            w.iter().map(|w| (w.start, w.end)).collect::<Vec<_>>(),
            [(1_000, 1_500), (1_500, 2_000)]
        );
    }

    /// Words one after another from `at`, `step` apart, the first opening a segment.
    fn said(at: Ms, step: Ms, text: &str) -> Vec<Word> {
        text.split_whitespace()
            .enumerate()
            .map(|(i, t)| Word {
                start: at + i as Ms * step,
                end: at + i as Ms * step + step - 50,
                text: t.into(),
                opens: i == 0,
                spaced: true,
            })
            .collect()
    }

    fn texts(cues: &[Cue]) -> Vec<&str> {
        cues.iter().map(|c| c.text.as_str()).collect()
    }

    fn all_words(cues: &[Cue]) -> String {
        texts(cues).join(" ")
    }

    #[test]
    fn a_word_on_the_border_goes_to_one_window_only() {
        // "Canterbury" is said right at 30 s. Window 0 heard it just after the border,
        // window 1 just before, so by its middle alone neither would take it.
        let a = said(
            28_000,
            500,
            "the patriotic archbishop of Canterbury, found it",
        );
        let mut b = said(
            28_100,
            500,
            "patriotic archbishop of Canterbury, found it advisable.",
        );
        b[0].opens = false;
        for (k, w) in [(0, &a), (1, &b)] {
            let mut alone = Transcript::default();
            alone.insert(k, w.clone());
            assert!(!all_words(&alone.cues()).contains("Canterbury"));
        }
        let mut t = Transcript::default();
        t.insert(0, a);
        t.insert(1, b);
        let got = all_words(&t.cues());
        assert_eq!(got.matches("Canterbury").count(), 1, "{got}");
        assert!(got.ends_with("of Canterbury, found it advisable."), "{got}");
        assert_eq!(
            t.cues().len(),
            1,
            "one segment cut by the border stays one cue"
        );
    }

    #[test]
    fn segments_cut_differently_in_two_windows_lose_and_repeat_nothing() {
        // From 01:30 of the tester's file: both windows heard "I am older than you" before
        // the border at 90 s, but each ended or started its segments elsewhere.
        let mut a = said(86_000, 500, "and would only say,");
        a.extend(said(88_000, 500, "I am older than you"));
        a.extend(said(90_500, 500, "and must know"));
        let mut b = said(87_100, 500, "only say,");
        b.extend(said(
            88_100,
            500,
            "I am older than you and must know better.",
        ));
        let mut t = Transcript::default();
        t.insert(2, a);
        t.insert(3, b);
        let got = all_words(&t.cues());
        assert_eq!(
            got,
            "and would only say, I am older than you and must know better."
        );

        // The other way round: both windows have the same words, but in segments that start
        // on different sides of the border.
        let a = said(207_000, 600, "Not I, said the lorry hastily. I thought you");
        let b = said(
            209_000,
            600,
            "the lorry hastily. I thought you did, said the mouse.",
        );
        let mut t = Transcript::default();
        t.insert(6, a);
        t.insert(7, b);
        let got = all_words(&t.cues());
        assert_eq!(got.matches("I thought you").count(), 1, "{got}");
        assert!(
            got.ends_with("hastily. I thought you did, said the mouse."),
            "{got}"
        );
    }

    #[test]
    fn without_common_words_the_border_splits_by_time() {
        let a = said(25_000, 1_000, "one two three four five six seven");
        let b = said(28_000, 1_000, "uno dos tres cuatro cinco");
        let mut t = Transcript::default();
        t.insert(0, a);
        t.insert(1, b);
        // Window 0 keeps what ends before 30 s by its middle, window 1 the rest.
        assert_eq!(
            all_words(&t.cues()),
            "one two three four five tres cuatro cinco"
        );
    }

    fn sample() -> Vec<(usize, Vec<Word>)> {
        let mut out = Vec::new();
        for k in 0..4 {
            let at = k as Ms * WINDOW - if k == 0 { 0 } else { PRE_ROLL };
            let mut w = Vec::new();
            let mut t = at;
            let mut n = 0;
            while t < k as Ms * WINDOW + WINDOW + POST_ROLL - 700 {
                // Each window hears the same words at slightly different times and cuts its
                // segments at its own places.
                let jitter = ((k * 7 + n) % 3) as Ms * 40;
                let word = (t + 350) / 700;
                w.push(Word {
                    start: t + jitter,
                    end: t + 600 + jitter,
                    text: if word % 9 == 8 {
                        format!("w{word}.")
                    } else {
                        format!("w{word}")
                    },
                    opens: (n + k) % 11 == 0,
                    spaced: true,
                });
                t += 700;
                n += 1;
            }
            out.push((k, w));
        }
        out
    }

    #[test]
    fn any_order_gives_the_same_cues_and_repeating_a_window_changes_nothing() {
        let windows = sample();
        let mut forward = Transcript::default();
        for (k, w) in windows.iter().cloned() {
            forward.insert(k, w);
        }
        let expect = forward.cues();
        let all = all_words(&expect);
        let words: Vec<&str> = all.split_whitespace().collect();
        // Every word once, in order, with nothing missing at the borders.
        for pair in words.windows(2) {
            let n = |w: &str| w.trim_matches(|c: char| !c.is_ascii_digit()).parse::<i64>();
            assert_eq!(n(pair[1]).unwrap(), n(pair[0]).unwrap() + 1, "{all}");
        }

        for order in [[3, 2, 1, 0], [1, 3, 0, 2], [2, 0, 3, 1]] {
            let mut t = Transcript::default();
            for k in order {
                t.insert(k, windows[k].1.clone());
                let before = t.cues();
                t.insert(k, windows[k].1.clone());
                assert_eq!(t.cues(), before, "the same window again");
            }
            assert_eq!(t.cues(), expect, "order {order:?}");
        }
    }

    #[test]
    fn cues_follow_segments_sentences_and_reading_time() {
        let initials = said(0, 400, "Recording by Roger W. Barnett. Mr. Smith read it.");
        assert_eq!(
            texts(&build(&initials)),
            ["Recording by Roger W. Barnett.", "Mr. Smith read it."]
        );

        let mut stub = said(0, 400, "I know what it means well enough. When");
        stub.extend(said(3_000, 400, "I find a thing, said the duck,"));
        assert_eq!(
            texts(&build(&stub)),
            [
                "I know what it means well enough.",
                "When I find a thing, said the duck,"
            ]
        );

        let mut w = said(0, 400, "Did you speak? Not I, said the lorry hastily.");
        w.extend(said(3_600, 400, "I proceed."));
        // A word stretched over the music that follows.
        w.extend(said(10_000, 400, "Just"));
        w.last_mut().unwrap().end = 28_000;
        let cues = build(&w);
        assert_eq!(
            texts(&cues),
            [
                "Did you speak?",
                "Not I, said the lorry hastily.",
                "I proceed.",
                "Just"
            ]
        );
        assert_eq!(cues[3].end, 10_000 + 2_000 + 90 * 4);

        // A long run without segment breaks is cut at its best place.
        let long = said(
            0,
            450,
            "They were indeed a queer-looking party that assembled on the bank, the birds with \
             draggled feathers and the animals with their fur clinging close to them",
        );
        let cues = build(&long);
        assert_eq!(cues.len(), 2, "{:?}", texts(&cues));
        assert!(cues[0].text.ends_with("bank,"), "{:?}", texts(&cues));
    }

    #[test]
    fn restoring_done_windows() {
        assert_eq!(
            restore_done(100_000, 60_000, &[]),
            (vec![true, true, false, false], false)
        );
        assert_eq!(restore_done(45_000, 45_000, &[]).0, [true, true]);
        let mask = [false, false, true, true];
        assert_eq!(restore_done(100_000, 0, &mask), (mask.to_vec(), true));
        // The file is longer than when it was cached: the mask no longer fits.
        assert_eq!(
            restore_done(130_000, 30_000, &mask),
            (vec![true, false, false, false, false], false)
        );
    }

    #[test]
    fn covered_prefix_stops_at_first_gap() {
        assert_eq!(covered_prefix(&[true, true, false, true], 100_000), 60_000);
        assert_eq!(covered_prefix(&[true, true], 45_000), 45_000);
        assert_eq!(covered_prefix(&[], 0), 0);
    }
}
