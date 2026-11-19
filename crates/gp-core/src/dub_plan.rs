//! Voice-over planning: which stretch of the translation is spoken as one clip, how long that
//! clip may last, and when each clip plays so that two voices never overlap.
//!
//! All times are positions in the video. A clip played at the video's speed covers as much
//! video time as its own length, so nothing here depends on the playback speed.

use crate::track::Ms;

/// Silence kept between the end of a clip and the next line.
pub const GAP_BEFORE_NEXT: Ms = 100;
/// The room the last line gets after its own end, when no line follows it.
pub const LAST_TAIL: Ms = 1_000;
/// How far a clip may be sped up. Past about ×1.75 the engine starts dropping words.
pub const MAX_COMPRESSION: f64 = 1.75;
/// A clip this much longer than its window still plays as it is.
pub const FIT_TOLERANCE: Ms = 150;
/// The engine makes clips about this much longer than the duration asked for.
pub const DURATION_OVERSHOOT: Ms = 200;
pub const MIN_REQUEST: Ms = 400;
/// The furthest a line may start after its time before short lines get dropped.
pub const MAX_LAG: Ms = 1_500;
/// Lines of one sentence this close together are spoken in one breath.
pub const MERGE_PAUSE: Ms = 700;

/// One primary subtitle line and the translation shown under it.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub start: Ms,
    pub end: Ms,
    pub text: String,
    /// Sentence group of the line; lines are only ever joined within one.
    pub group: usize,
    /// Start of the next subtitle line, translated or not.
    pub next_start: Option<Ms>,
}

/// What is spoken as one clip.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Unit {
    pub start: Ms,
    pub end: Ms,
    pub text: String,
    /// The time the clip has before the next line begins; never shorter than the line.
    pub window: Ms,
}

/// "Да.", "Нет.", "Ok": the engine says these unreliably on their own, as a sound of a fixed
/// length rather than the word.
pub fn is_short(text: &str) -> bool {
    let letters = text.chars().filter(|c| c.is_alphanumeric()).count();
    let words = text
        .split_whitespace()
        .filter(|w| w.chars().any(char::is_alphanumeric))
        .count();
    letters <= 5 || (words <= 1 && letters <= 8)
}

struct Span {
    start: Ms,
    end: Ms,
    text: String,
    group: usize,
    next_start: Option<Ms>,
}

/// Turns translated lines, in start order, into the clips to synthesize.
pub fn units(lines: &[Line]) -> Vec<Unit> {
    let mut spans: Vec<Span> = Vec::new();
    for l in lines {
        let text = l.text.split_whitespace().collect::<Vec<_>>().join(" ");
        if !text.chars().any(char::is_alphanumeric) {
            continue;
        }
        // A short sentence without a place to cut it shows whole under each of its lines;
        // spoken twice it would sound like a stutter.
        if let Some(last) = spans.last_mut()
            && last.group == l.group
            && last.text == text
        {
            last.end = last.end.max(l.end);
            last.next_start = l.next_start;
            continue;
        }
        spans.push(Span {
            start: l.start,
            end: l.end,
            text,
            group: l.group,
            next_start: l.next_start,
        });
    }

    let mut out: Vec<Span> = Vec::new();
    let mut pending: Option<Span> = None;
    for span in spans {
        let span = match pending.take() {
            Some(short) if short.group == span.group && span.start - short.end <= MERGE_PAUSE => {
                Span {
                    start: short.start,
                    text: format!("{} {}", short.text, span.text),
                    ..span
                }
            }
            Some(short) => {
                push_or_join(&mut out, short);
                span
            }
            None => span,
        };
        if is_short(&span.text) {
            pending = Some(span);
        } else {
            out.push(span);
        }
    }
    if let Some(short) = pending {
        push_or_join(&mut out, short);
    }
    out.into_iter()
        .map(|s| Unit {
            window: window(s.start, s.end, s.next_start),
            start: s.start,
            end: s.end,
            text: s.text,
        })
        .collect()
}

/// A short span that could not join the line after it joins the one before, if close.
fn push_or_join(out: &mut Vec<Span>, short: Span) {
    match out.last_mut() {
        Some(prev) if prev.group == short.group && short.start - prev.end <= MERGE_PAUSE => {
            prev.text = format!("{} {}", prev.text, short.text);
            prev.end = short.end;
            prev.next_start = short.next_start;
        }
        _ => out.push(short),
    }
}

pub fn window(start: Ms, end: Ms, next_start: Option<Ms>) -> Ms {
    let own = (end - start).max(0);
    match next_start {
        Some(next) => (next - GAP_BEFORE_NEXT - start).max(own),
        None => own + LAST_TAIL,
    }
}

/// The duration to ask the engine for when a clip of `natural` length does not fit its
/// window; `None` when it fits as it is. The clip is squeezed into the window, but never
/// harder than `MAX_COMPRESSION`; what is still too long runs into the next pause.
pub fn fit(natural: Ms, window: Ms) -> Option<Ms> {
    if natural <= window + FIT_TOLERANCE {
        return None;
    }
    let floor = (natural as f64 / MAX_COMPRESSION).ceil() as Ms;
    Some((window.max(floor) - DURATION_OVERSHOOT).max(MIN_REQUEST))
}

/// A clip that is ready to play: where its line starts and how long it lasts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Item {
    pub start: Ms,
    pub len: Ms,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Start at `at`, `lag` after the line itself starts.
    Play {
        at: Ms,
        lag: Ms,
    },
    Skip,
}

/// When each clip plays. `free_at` is when the clip playing now ends. A clip never starts
/// before its line nor before the previous clip has ended; when that would make one start
/// more than `MAX_LAG` late, the shortest clip of the late run is dropped and the run is
/// planned again.
pub fn schedule(free_at: Ms, items: &[Item]) -> Vec<Decision> {
    let mut skipped = vec![false; items.len()];
    loop {
        let mut out = Vec::with_capacity(items.len());
        let (mut t, mut run_start, mut overflow) = (free_at, 0, None);
        for (i, item) in items.iter().enumerate() {
            if skipped[i] {
                out.push(Decision::Skip);
                continue;
            }
            let at = t.max(item.start);
            let lag = at - item.start;
            if lag == 0 {
                run_start = i;
            }
            if lag > MAX_LAG {
                overflow = Some((run_start, i));
                break;
            }
            out.push(Decision::Play { at, lag });
            t = at + item.len;
        }
        let Some((from, to)) = overflow else {
            return out;
        };
        let victim = (from..=to)
            .filter(|&i| !skipped[i])
            .min_by_key(|&i| (items[i].len, std::cmp::Reverse(i)))
            .expect("the late clip itself is a candidate");
        skipped[victim] = true;
    }
}

/// Volume of the original sound: the user's volume, turned down while the voice-over is on.
pub fn original_volume(volume: f64, dubbing: bool, original_percent: f64) -> f64 {
    if dubbing {
        (volume * original_percent.clamp(0.0, 150.0) / 100.0).clamp(0.0, 150.0)
    } else {
        volume
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(start: Ms, end: Ms, text: &str, group: usize, next: Option<Ms>) -> Line {
        Line {
            start,
            end,
            text: text.into(),
            group,
            next_start: next,
        }
    }

    fn texts(u: &[Unit]) -> Vec<&str> {
        u.iter().map(|u| u.text.as_str()).collect()
    }

    #[test]
    fn windows_reach_to_the_next_line() {
        let u = units(&[
            line(2_000, 5_500, "Компания на берегу.", 0, Some(6_000)),
            line(6_000, 9_000, "Птицы с перьями,", 1, Some(9_500)),
            line(28_000, 32_000, "Первым делом.", 2, None),
        ]);
        assert_eq!(
            texts(&u),
            ["Компания на берегу.", "Птицы с перьями,", "Первым делом."]
        );
        assert_eq!(u[0].window, 3_900);
        assert_eq!(u[1].window, 3_400);
        assert_eq!(u[2].window, 4_000 + LAST_TAIL);
        // Lines on screen together: the window is never shorter than the line.
        assert_eq!(window(1_000, 3_000, Some(1_000)), 2_000);
    }

    #[test]
    fn one_word_lines_join_a_close_neighbour_of_the_same_sentence() {
        let u = units(&[
            line(0, 600, "Да.", 0, Some(800)),
            line(800, 3_000, "Я приду завтра.", 0, Some(5_000)),
            line(5_000, 6_000, "Хорошо", 1, Some(6_300)),
            line(6_300, 6_800, "Нет.", 1, Some(9_000)),
            line(9_000, 9_500, "Ну да.", 2, Some(20_000)),
            line(20_000, 22_000, "Он ушёл, а потом", 3, Some(22_400)),
            line(22_400, 23_000, "вернулся.", 3, None),
        ]);
        assert_eq!(
            texts(&u),
            [
                "Да. Я приду завтра.",
                "Хорошо Нет.",
                "Ну да.",
                "Он ушёл, а потом вернулся."
            ]
        );
        assert_eq!((u[0].start, u[0].end), (0, 3_000));
        assert_eq!(u[0].window, 5_000 - GAP_BEFORE_NEXT);
        // Alone in its sentence, far from anything: spoken as it is.
        assert_eq!((u[2].start, u[2].window), (9_000, 20_000 - 100 - 9_000));
        assert_eq!(u[3].window, 3_000 + LAST_TAIL, "joined to the last line");
    }

    #[test]
    fn short_lines_do_not_cross_sentences_or_long_pauses() {
        let u = units(&[
            line(0, 2_000, "Он сказал всё.", 0, Some(2_100)),
            line(2_100, 2_500, "Да.", 1, Some(4_000)),
            line(4_000, 6_000, "Мы пойдём домой.", 1, Some(9_000)),
        ]);
        assert_eq!(texts(&u), ["Он сказал всё.", "Да.", "Мы пойдём домой."]);
    }

    #[test]
    fn a_sentence_shown_whole_under_two_lines_is_spoken_once() {
        let u = units(&[
            line(0, 1_000, "А потом он пошёл домой.", 4, Some(1_100)),
            line(1_100, 2_500, "А потом он пошёл домой.", 4, Some(4_000)),
            line(4_000, 5_000, "А потом он пошёл домой.", 5, None),
        ]);
        assert_eq!(u.len(), 2, "{u:?}");
        assert_eq!((u[0].start, u[0].end, u[0].window), (0, 2_500, 3_900));
    }

    #[test]
    fn empty_and_multiline_texts() {
        let u = units(&[
            line(0, 1_000, " … ", 0, Some(2_000)),
            line(2_000, 4_000, "Компания\nна берегу", 1, None),
        ]);
        assert_eq!(texts(&u), ["Компания на берегу"]);
    }

    #[test]
    fn fitting_squeezes_no_harder_than_the_limit() {
        assert_eq!(fit(3_000, 3_900), None);
        assert_eq!(fit(4_000, 3_900), None, "within tolerance");
        assert_eq!(fit(5_000, 3_900), Some(3_900 - DURATION_OVERSHOOT));
        // 7 s into 2 s would be ×3.5; it gets ×1.75 and runs over.
        assert_eq!(fit(7_000, 2_000), Some(4_000 - DURATION_OVERSHOOT));
        assert_eq!(fit(900, 100), Some(MIN_REQUEST));
    }

    fn item(start: Ms, len: Ms) -> Item {
        Item { start, len }
    }

    #[test]
    fn clips_on_time_play_at_their_lines() {
        let d = schedule(0, &[item(2_000, 3_000), item(6_000, 2_500)]);
        assert_eq!(
            d,
            [
                Decision::Play { at: 2_000, lag: 0 },
                Decision::Play { at: 6_000, lag: 0 }
            ]
        );
    }

    #[test]
    fn a_long_clip_pushes_the_next_one_back_without_overlap() {
        let d = schedule(0, &[item(0, 4_000), item(3_000, 2_000), item(9_000, 1_000)]);
        assert_eq!(
            d,
            [
                Decision::Play { at: 0, lag: 0 },
                Decision::Play {
                    at: 4_000,
                    lag: 1_000
                },
                Decision::Play { at: 9_000, lag: 0 }
            ]
        );
        // The clip still playing counts too.
        assert_eq!(
            schedule(5_200, &[item(5_000, 1_000)]),
            [Decision::Play {
                at: 5_200,
                lag: 200
            }]
        );
    }

    fn assert_no_overlap(d: &[Decision], items: &[Item]) {
        let mut end = Ms::MIN;
        for (dec, it) in d.iter().zip(items) {
            if let Decision::Play { at, lag } = *dec {
                assert!(at >= end, "overlap at {at}: {d:?}");
                assert!((0..=MAX_LAG).contains(&lag), "{lag}");
                end = at + it.len;
            }
        }
    }

    #[test]
    fn falling_too_far_behind_drops_the_shortest_clip_of_the_run() {
        // The lag grows line by line and passes the limit at the fourth; the shortest clip
        // of the run is the second, so that one goes and the rest catch up.
        let items = [
            item(0, 3_000),
            item(2_000, 800),
            item(3_000, 3_000),
            item(5_000, 3_000),
            item(20_000, 1_000),
        ];
        let d = schedule(0, &items);
        assert_eq!(
            d,
            [
                Decision::Play { at: 0, lag: 0 },
                Decision::Skip,
                Decision::Play { at: 3_000, lag: 0 },
                Decision::Play {
                    at: 6_000,
                    lag: 1_000
                },
                Decision::Play { at: 20_000, lag: 0 },
            ]
        );
        assert_no_overlap(&d, &items);
    }

    #[test]
    fn the_late_clip_itself_goes_when_it_is_the_shortest() {
        let items = [item(0, 3_000), item(2_000, 3_000), item(4_000, 800)];
        let d = schedule(0, &items);
        assert_eq!(d[2], Decision::Skip, "{d:?}");
        assert_no_overlap(&d, &items);

        let items = [item(1_000, 500), item(2_000, 2_000)];
        let d = schedule(2_600, &items);
        assert_eq!(
            d,
            [
                Decision::Skip,
                Decision::Play {
                    at: 2_600,
                    lag: 600
                }
            ]
        );
    }

    #[test]
    fn a_long_backlog_never_overlaps_or_lags_past_the_limit() {
        // Thirty lines a second apart, each clip 1.6 s long.
        let items: Vec<Item> = (0..30).map(|i| item(i * 1_000, 1_600)).collect();
        let d = schedule(0, &items);
        assert_no_overlap(&d, &items);
        let played = d
            .iter()
            .filter(|d| matches!(d, Decision::Play { .. }))
            .count();
        assert!((15..30).contains(&played), "{played}");
    }

    #[test]
    fn volumes() {
        assert_eq!(original_volume(80.0, true, 25.0), 20.0);
        assert_eq!(original_volume(80.0, false, 25.0), 80.0);
        assert_eq!(original_volume(100.0, true, 150.0), 150.0);
        assert_eq!(original_volume(100.0, true, -5.0), 0.0);
    }
}
