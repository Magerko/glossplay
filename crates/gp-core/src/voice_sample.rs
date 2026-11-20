//! Choosing a stretch of the film's own speech for the voice-over to speak in that voice.
//!
//! The speech engine copies the voice of a short sample, given its words. It also copies its
//! pace: from lecture samples spoken at 16 characters a second the Russian clips came out
//! slurred (a quarter of the words misheard by Whisper), from samples at 10–11 a second they
//! were as clear as the standard voice. So the sample is a stretch of whole sentences at a
//! moderate pace, and what the sound itself shows is checked before it is used.

use crate::asr_plan::Loudness;
use crate::sentences::ends_sentence;
use crate::track::{Cue, Ms};

pub const MIN_MS: Ms = 6_000;
pub const MAX_MS: Ms = 12_000;
/// Lines further apart than this are not one stretch of speech.
const MAX_GAP: Ms = 300;
/// Characters of text a second, the pace a sample may have.
const PACE: std::ops::RangeInclusive<f64> = 8.0..=13.0;
const BEST_PACE: f64 = 11.0;
/// The pace the voice-over is spoken at, about that of the standard voice.
const VOICE_PACE: f64 = 13.5;
/// How far past its last line a sample may reach for the end of the last word.
const MAX_ROOM: Ms = 1_500;
/// The sound past the end of the transcript is not known to be silence.
const ROOM_AT_END: Ms = 500;
/// Sound taken before a stretch's first line, to find the pause it starts after.
pub const LEAD: Ms = 300;
/// A pause this long at either edge, so the sample holds whole words and only them.
const EDGE_PAUSE: Ms = 150;
/// Silence kept before the first word and after the last.
const MARGIN: Ms = 100;
const TAIL: Ms = 150;

#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    pub start: Ms,
    pub end: Ms,
    pub text: String,
    /// How much sound past `end` may still belong to the last word, before the next line.
    pub room: Ms,
}

impl Span {
    pub fn pace(&self) -> f64 {
        let chars = self.text.chars().filter(|c| !c.is_whitespace()).count()
            + self.text.split_whitespace().count().saturating_sub(1);
        chars as f64 / ((self.end - self.start).max(1) as f64 / 1000.0)
    }

    /// The speed to ask for, so the copied voice keeps the voice-over's pace rather than the
    /// sample's slower one.
    pub fn speed(&self) -> f32 {
        (VOICE_PACE / self.pace()).clamp(1.0, 1.3) as f32
    }
}

fn starts_sentence(text: &str) -> bool {
    text.chars()
        .find(|c| c.is_alphanumeric())
        .is_some_and(|c| !c.is_lowercase())
}

fn ends_clause(text: &str) -> bool {
    text.trim_end()
        .trim_end_matches(['"', '\'', '\u{201d}', '\u{2019}', '\u{bb}', ')'])
        .ends_with([',', ';', ':', '\u{2014}', '\u{2013}'])
}

/// Stretches said without a pause, from the start of a sentence to the end of one, best
/// first. The voice is chosen while the transcript is still growing, and a few minutes of
/// read prose may hold no such stretch; one ending at a comma or before a pause is taken then.
pub fn candidates(cues: &[Cue]) -> Vec<Span> {
    let mut out: Vec<(Span, bool)> = Vec::new();
    for i in 0..cues.len() {
        if !starts_sentence(&cues[i].text) || (i > 0 && !ends_sentence(&cues[i - 1].text)) {
            continue;
        }
        let mut text = String::new();
        for j in i..cues.len() {
            if j > i && cues[j].start - cues[j - 1].end > MAX_GAP {
                break;
            }
            let len = cues[j].end - cues[i].start;
            if len > MAX_MS {
                break;
            }
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(
                &cues[j]
                    .text
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" "),
            );
            let span = Span {
                start: cues[i].start,
                end: cues[j].end,
                text: text.clone(),
                room: cues
                    .get(j + 1)
                    .map_or(ROOM_AT_END, |n| n.start - cues[j].end)
                    .clamp(0, MAX_ROOM),
            };
            let whole = ends_sentence(&cues[j].text);
            let pause_after = cues
                .get(j + 1)
                .is_some_and(|n| n.start - cues[j].end > MAX_GAP);
            if len >= MIN_MS
                && (whole || ends_clause(&cues[j].text) || pause_after)
                && PACE.contains(&span.pace())
            {
                out.push((span, whole));
            }
        }
    }
    let score = |(s, whole): &(Span, bool)| {
        let secs = (s.end - s.start) as f64 / 1000.0;
        (s.pace() - BEST_PACE).abs() + (secs - 9.0).abs() * 0.2 + if *whole { 0.0 } else { 2.0 }
    };
    out.sort_by(|a, b| {
        score(a)
            .total_cmp(&score(b))
            .then(a.0.start.cmp(&b.0.start))
    });
    let out = out.into_iter().map(|(s, _)| s);
    // Overlapping stretches would fail the same way; one of each is enough.
    let mut kept: Vec<Span> = Vec::new();
    for s in out {
        if !kept.iter().any(|k| s.start < k.end && k.start < s.end) {
            kept.push(s);
        }
    }
    kept
}

/// Where to cut a sample taken from [`LEAD`] before a stretch to the end of its room, in
/// the pauses around it; `None` when the speech does not pause there.
///
/// The engine copies a sample as if its words ran on into the line it speaks. Recognition
/// puts the end of a line inside its last word, and a sample cut there made the engine finish
/// the word, and the sentence, first: "The middle." opened every line for a sample ending at
/// "in the mid-". A sample running into the next word had the first words of lines dropped
/// and "...pages" said in front of one.
pub fn sample_cut(sound: &Loudness, span: &Span) -> Option<(Ms, Ms)> {
    let lead = LEAD.min(span.start);
    let said = lead + span.end - span.start;
    let pauses = sound.pauses();
    let end = sound.len_ms();
    let start = if lead == 0 {
        0
    } else {
        pauses
            .iter()
            .filter(|(a, b)| b - a >= EDGE_PAUSE && (lead - 200..=lead + 200).contains(b))
            .map(|(_, b)| (b - MARGIN).max(0))
            .next_back()?
    };
    let (a, b) = pauses
        .into_iter()
        .find(|&(a, b)| b >= said + 50 && (b - a >= EDGE_PAUSE || (b == end && b - a >= MARGIN)))?;
    Some((start, (a.max(said - 300) + TAIL).min(b)))
}

/// Whether the sound of a stretch is speech the engine can copy: words standing out from
/// quiet pauses (no music under them), no long silence, and speech from its first moment.
pub fn sound_fits(sound: &Loudness) -> bool {
    let Some(stats) = sound.speech() else {
        return false;
    };
    stats.pause_share <= 0.45 && stats.longest_pause <= 1_500 && stats.first_sound <= 500
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

    #[test]
    fn whole_sentences_at_a_moderate_pace_without_pauses() {
        let cues = [
            // 16 characters a second: too fast to copy well.
            cue(
                0,
                3_000,
                "And basically, this notion of sort of decomposing a",
            ),
            cue(
                3_000,
                6_000,
                "structure into pieces is going to be that, okay.",
            ),
            cue(9_000, 13_000, "And here we might be in a setting where we"),
            cue(
                13_100,
                18_300,
                "have images and we're trying to predict labels for them.",
            ),
            // A pause of a second inside.
            cue(19_000, 23_000, "I think these were the ones that happened"),
            cue(
                24_000,
                28_000,
                "to pop up on today's random Wikipedia pages.",
            ),
            // Starts inside a sentence.
            cue(
                28_100,
                36_000,
                "and the goal there is to predict some label Y from X.",
            ),
        ];
        let c = candidates(&cues);
        assert_eq!(c.len(), 1, "{c:?}");
        assert_eq!((c[0].start, c[0].end), (9_000, 18_300));
        assert_eq!(
            c[0].text,
            "And here we might be in a setting where we have images and we're trying to predict labels for them."
        );
        assert!((10.0..11.0).contains(&c[0].pace()), "{}", c[0].pace());
        assert!((1.2..=1.3).contains(&c[0].speed()));
    }

    #[test]
    fn the_best_paced_comes_first_and_overlaps_go() {
        let cues = [
            cue(
                0,
                6_000,
                "One two three four five six seven eight nine ten words.",
            ),
            cue(6_100, 8_000, "Eleven more words here."),
            cue(
                20_000,
                26_000,
                "This sentence is said at a very moderate and even pace now.",
            ),
        ];
        let c = candidates(&cues);
        assert_eq!(c.len(), 2, "{c:?}");
        assert!((c[0].pace() - BEST_PACE).abs() <= (c[1].pace() - BEST_PACE).abs());
        assert!(
            c.iter()
                .all(|s| (MIN_MS..=MAX_MS).contains(&(s.end - s.start)))
        );
        assert!(candidates(&[]).is_empty());
        // Recognition without punctuation gives no sentences to take.
        assert!(
            candidates(&[cue(0, 8_000, "so this is what we do in the lab every day")]).is_empty()
        );
    }

    #[test]
    fn a_pause_or_a_comma_ends_a_stretch_when_no_sentence_does() {
        let cues = [
            cue(0, 1_000, "Hello."),
            cue(
                1_400,
                5_000,
                "They had a consultation about this, and after a",
            ),
            cue(
                5_100,
                9_300,
                "few minutes it seemed quite natural to Alice to find",
            ),
            cue(
                9_900,
                14_000,
                "herself talking familiarly with them as if she had",
            ),
        ];
        let c = candidates(&cues);
        assert_eq!(c.len(), 1, "{c:?}");
        assert_eq!((c[0].start, c[0].end), (1_400, 9_300));
        // The same stretch with the next line not yet heard: nothing says it ends there.
        assert!(candidates(&cues[..3]).is_empty());
        // A whole sentence is preferred to an equally paced stretch cut at a pause.
        let mut more = cues.to_vec();
        more.push(cue(19_000, 19_500, "Yes."));
        more.push(cue(
            20_500,
            28_800,
            "They had a consultation about this, and after a few minutes it was fine.",
        ));
        more.push(cue(40_000, 41_000, "End."));
        let c = candidates(&more);
        assert_eq!(c[0].start, 20_500, "{c:?}");
    }

    fn audio(len: Ms, loud: &[(Ms, Ms)]) -> Vec<i16> {
        let mut seed = 7u32;
        (0..len * 16)
            .map(|i| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let sign = if seed >> 31 == 0 { 1 } else { -1 };
                let on = loud.iter().any(|(a, b)| (*a * 16..*b * 16).contains(&i));
                sign * if on { 2_000 } else { 30 }
            })
            .collect()
    }

    #[test]
    fn a_sample_is_cut_in_the_pauses_around_its_words() {
        let cues = [
            cue(0, 1_000, "Hello."),
            cue(1_400, 7_000, "They all sat down at once, in a large ring,"),
            cue(7_000, 8_200, "with the mouse in the middle."),
            cue(9_000, 10_000, "Next."),
        ];
        let c = candidates(&cues);
        let span = c.iter().find(|s| s.start == 1_400).expect("the stretch");
        assert_eq!((span.end, span.room), (8_200, 800));
        assert_eq!(candidates(&cues[..3])[0].room, ROOM_AT_END);
        // Taken from 300 ms before the line: words every second with short gaps, the last
        // one running 300 ms past the line's end.
        let len = LEAD + 6_800 + 800;
        let mut words: Vec<(Ms, Ms)> = (0..6)
            .map(|k| (LEAD + k * 1_000, LEAD + k * 1_000 + 800))
            .collect();
        words.push((6_300, 7_400));
        let sound = Loudness::new(&audio(len, &words), 16_000);
        assert_eq!(
            sample_cut(&sound, span),
            Some((LEAD - MARGIN, 7_400 + TAIL))
        );
        // Speech running on into the next line.
        words.pop();
        words.push((6_300, len));
        let on = Loudness::new(&audio(len, &words), 16_000);
        assert_eq!(sample_cut(&on, span), None);
        // Speech already going when the line starts.
        let mut early: Vec<(Ms, Ms)> = (0..6).map(|k| (k * 1_000, k * 1_000 + 1_100)).collect();
        early.push((6_300, 7_400));
        let early = Loudness::new(&audio(len, &early), 16_000);
        assert_eq!(sample_cut(&early, span), None);
        // Sound that cannot be judged.
        let hum = Loudness::new(&audio(len, &[(0, len)]), 16_000);
        assert_eq!(sample_cut(&hum, span), None);
    }

    #[test]
    fn the_sound_must_be_speech_with_short_pauses() {
        let words: Vec<(Ms, Ms)> = (0..9).map(|k| (k * 1_000, k * 1_000 + 800)).collect();
        assert!(sound_fits(&Loudness::new(&audio(9_000, &words), 16_000)));
        let late = [(1_000, 9_000)];
        assert!(
            !sound_fits(&Loudness::new(&audio(9_000, &late), 16_000)),
            "silence first"
        );
        let gap = [(0, 3_000), (5_000, 9_000)];
        assert!(
            !sound_fits(&Loudness::new(&audio(9_000, &gap), 16_000)),
            "a long pause"
        );
        let music = [(0, 9_000)];
        assert!(
            !sound_fits(&Loudness::new(&audio(9_000, &music), 16_000)),
            "no pauses at all"
        );
    }
}
