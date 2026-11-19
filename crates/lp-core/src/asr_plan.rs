//! Splits a media file into fixed windows for speech recognition, so windows can be
//! transcribed in any order (the one under the playhead first).

use crate::track::{Cue, Ms};

pub const WINDOW: Ms = 30_000;
/// Audio before the window, so the first words are not cut mid-syllable.
pub const PRE_ROLL: Ms = 3_000;
/// Audio after the window, so a segment that starts inside it can finish.
pub const POST_ROLL: Ms = 8_000;

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

/// Keeps the segments that belong to window `k`. Every segment belongs to the window
/// its start falls into, so neighbouring windows never both claim it.
pub fn accept(k: usize, audio_start: Ms, segments: impl IntoIterator<Item = Cue>) -> Vec<Cue> {
    let (from, to) = (k as Ms * WINDOW, (k as Ms + 1) * WINDOW);
    segments
        .into_iter()
        .map(|c| {
            // Whisper stretches the last line before music or silence until the next speech.
            // Nobody reads slower than this, so I end the line earlier.
            let readable = 2_000 + 90 * c.text.chars().count() as Ms;
            Cue {
                start: c.start + audio_start,
                end: (c.end + audio_start).min(c.start + audio_start + readable),
                text: c.text,
            }
        })
        .filter(|c| c.start >= from && c.start < to)
        .collect()
}

/// Inserts new cues keeping the list sorted. Two windows may segment speech differently
/// around their shared border, so a new cue that mostly overlaps an existing one is dropped.
pub fn merge(existing: &mut Vec<Cue>, new: Vec<Cue>) {
    for cue in new {
        let at = existing.partition_point(|c| c.start <= cue.start);
        let near = &existing[at.saturating_sub(2)..(at + 2).min(existing.len())];
        let duplicate = near.iter().any(|c| {
            let overlap = cue.end.min(c.end) - cue.start.max(c.start);
            overlap * 2 > cue.end - cue.start
        });
        if !duplicate {
            existing.insert(at, cue);
        }
    }
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

    #[test]
    fn a_segment_belongs_to_the_window_it_starts_in() {
        // Window 1 covers 30–60 s; its audio starts at 27 s.
        let segs = vec![
            cue(500, 2_900, "tail of previous"),
            cue(3_000, 6_000, "first segment here"),
            cue(32_000, 36_000, "runs over the border now"),
        ];
        let got = accept(1, 27_000, segs);
        assert_eq!(
            got,
            vec![
                cue(30_000, 33_000, "first segment here"),
                cue(59_000, 63_000, "runs over the border now")
            ]
        );
        // The same border seen from window 2 (audio from 57 s): the overrunning segment is not taken twice.
        let segs = vec![
            cue(2_000, 6_000, "runs over the border now"),
            cue(6_500, 8_500, "next"),
        ];
        assert_eq!(accept(2, 57_000, segs), vec![cue(63_500, 65_500, "next")]);
    }

    #[test]
    fn a_line_stretched_over_music_is_cut_to_reading_time() {
        let got = accept(0, 0, vec![cue(19_000, 37_000, "Just quest.")]);
        assert_eq!(got, vec![cue(19_000, 21_990, "Just quest.")]);
    }

    #[test]
    fn merge_sorts_and_drops_border_duplicates() {
        let mut cues = vec![cue(0, 2_000, "a"), cue(58_000, 61_000, "border")];
        merge(
            &mut cues,
            vec![
                cue(58_400, 61_200, "border again"),
                cue(62_000, 64_000, "c"),
                cue(10_000, 12_000, "b"),
            ],
        );
        let texts: Vec<&str> = cues.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, ["a", "b", "border", "c"]);
    }

    #[test]
    fn covered_prefix_stops_at_first_gap() {
        assert_eq!(covered_prefix(&[true, true, false, true], 100_000), 60_000);
        assert_eq!(covered_prefix(&[true, true], 45_000), 45_000);
        assert_eq!(covered_prefix(&[], 0), 0);
    }
}
