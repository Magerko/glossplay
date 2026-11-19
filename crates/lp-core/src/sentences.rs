//! Subtitle cues often hold a fragment of a sentence ("I told him that" / "he didn't do it,"
//! / "she said."). Translating fragments one by one gives nonsense, so cues are grouped into
//! sentences, the sentence is translated, and its translation is cut back into one part per cue.

use crate::track::{Cue, Ms};
use std::ops::Range;

/// A longer silence than this ends the sentence even without punctuation.
pub const MAX_PAUSE: Ms = 1_500;
/// The pause allowed when the text itself says the sentence goes on: no punctuation at the
/// end, a lowercase word next. Speech recognition cuts a line's end to its reading time, so
/// its pauses look longer than they were.
pub const LINKED_PAUSE: Ms = 4_000;
pub const MAX_CUES: usize = 4;
pub const MAX_CHARS: usize = 350;
/// Cues that overlap by more than this are on screen together (two speakers, a sign), not
/// one after another, so they are never joined.
const OVERLAP: Ms = 250;
/// A short two-cue sentence cut anywhere but at punctuation reads worse than the whole
/// translation shown under both cues.
const SHORT: usize = 60;

const CLOSERS: &[char] = &[
    '"', '\'', '”', '’', '»', ')', ']', '」', '』', '）', '】', '〕', '〉', '》',
];
const TERMINALS: &[char] = &['.', '!', '?', '…', '。', '！', '？', '‼', '⁉', '؟'];
const SOFT: &[char] = &[',', ';', ':', '，', '、', '；', '：'];
const DASHES: &[char] = &['—', '–'];

/// Splits cues into sentence groups. Each break is decided from the two cues around it and
/// the size of the group so far, so cues added far ahead never change earlier groups.
pub fn group(cues: &[Cue]) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let (mut start, mut chars) = (0, 0);
    for i in 0..cues.len() {
        chars += cue_len(&cues[i].text) + usize::from(i > start);
        let ends = match cues.get(i + 1) {
            None => true,
            Some(next) => breaks(&cues[i], next, i + 1 - start, chars),
        };
        if ends {
            out.push(start..i + 1);
            (start, chars) = (i + 1, 0);
        }
    }
    out
}

fn breaks(cur: &Cue, next: &Cue, count: usize, chars: usize) -> bool {
    count >= MAX_CUES
        || chars + 1 + cue_len(&next.text) > MAX_CHARS
        || next.start - cur.end
            > if linked(&cur.text, &next.text) {
                LINKED_PAUSE
            } else {
                MAX_PAUSE
            }
        || next.start + OVERLAP < cur.end
        || new_speaker(&next.text)
        || (ends_sentence(&cur.text) && !continues(&cur.text, &next.text))
}

/// Whether nothing that comes after this cue can join its sentence.
pub fn ends_firmly(text: &str) -> bool {
    ends_sentence(text) && !ends_with_ellipsis(text)
}

pub fn ends_sentence(text: &str) -> bool {
    strip_closers(text).ends_with(TERMINALS)
}

fn ends_with_ellipsis(text: &str) -> bool {
    let t = strip_closers(text);
    t.ends_with("...") || t.ends_with('…')
}

fn strip_closers(text: &str) -> &str {
    text.trim_end().trim_end_matches(CLOSERS).trim_end()
}

fn starts_with_ellipsis(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with("...") || t.starts_with('…')
}

/// "I was going to..." / "...go home" and "I was going to..." / "go home" are one sentence.
fn continues(cur: &str, next: &str) -> bool {
    ends_with_ellipsis(cur)
        && (starts_with_ellipsis(next)
            || next
                .trim_start()
                .chars()
                .find(|c| c.is_alphabetic())
                .is_some_and(char::is_lowercase))
}

fn linked(cur: &str, next: &str) -> bool {
    !ends_sentence(cur)
        && next
            .trim_start()
            .chars()
            .next()
            .is_some_and(char::is_lowercase)
}

fn new_speaker(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with("- ")
        || t.starts_with("– ")
        || t.starts_with('—')
        || (t.starts_with('-') && t[1..].starts_with(char::is_uppercase))
}

pub fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3000..=0x303F // CJK punctuation
        | 0x3040..=0x30FF // kana
        | 0x31F0..=0x31FF
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xF900..=0xFAFF
        | 0xFF00..=0xFFEF // full-width forms
        | 0x20000..=0x2FA1F)
}

fn glue(out: &mut String, piece: &str) {
    if piece.is_empty() {
        return;
    }
    let spaced = !out.is_empty() && !out.ends_with(is_cjk) && !piece.starts_with(is_cjk);
    if spaced {
        out.push(' ');
    }
    out.push_str(piece);
}

/// A cue as one line: its lines joined the way the language joins words.
pub fn flatten(text: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        glue(&mut out, line.trim());
    }
    out
}

/// Length of a cue's text as it appears inside its sentence, in characters.
pub fn cue_len(text: &str) -> usize {
    flatten(text).chars().count()
}

/// The text of a sentence made of these cues.
pub fn sentence_text<'a>(cues: impl IntoIterator<Item = &'a str>) -> String {
    let mut out = String::new();
    let mut prev_ellipsis = false;
    for text in cues {
        let flat = flatten(text);
        let mut piece = flat.as_str();
        if prev_ellipsis {
            // "going to... ...go home" reads as one stammer, not two.
            piece = piece
                .trim_start_matches("...")
                .trim_start_matches('…')
                .trim_start();
        }
        prev_ellipsis = ends_with_ellipsis(&flat);
        glue(&mut out, piece);
    }
    out
}

fn mostly_cjk(text: &str) -> bool {
    let (mut cjk, mut all) = (0, 0);
    for c in text.chars().filter(|c| !c.is_whitespace()) {
        all += 1;
        cjk += usize::from(is_cjk(c));
    }
    cjk * 10 >= all * 3 && cjk > 0
}

/// Pieces of a translation that are never cut apart: words, or for text without spaces
/// single characters with their trailing punctuation and Latin words kept whole.
fn units(text: &str) -> (Vec<String>, &'static str) {
    if !mostly_cjk(text) {
        return (text.split_whitespace().map(str::to_owned).collect(), " ");
    }
    let mut out: Vec<String> = Vec::new();
    let mut prev: Option<char> = None;
    for c in text.trim().chars() {
        let attach = match (out.last(), prev) {
            (None, _) | (_, None) => false,
            (Some(_), Some(p)) => {
                c.is_whitespace()
                    || (!c.is_alphanumeric() && !"「『（【〔〈《\"'“‘(".contains(c))
                    || (c.is_ascii_alphanumeric() && p.is_ascii_alphanumeric())
            }
        };
        match out.last_mut() {
            Some(last) if attach => last.push(c),
            _ => out.push(c.to_string()),
        }
        prev = Some(c);
    }
    (out, "")
}

/// Cuts the translation of a sentence into one part per source cue, in proportion to the
/// cues' lengths, at word boundaries, preferring a boundary after punctuation. Parts joined
/// with a space (nothing for CJK) give back the translation. With fewer words than cues the
/// extra parts are empty.
pub fn distribute(translation: &str, source_lens: &[usize]) -> Vec<String> {
    let n = source_lens.len();
    let (units, sep) = units(translation);
    let w = units.len();
    if n == 0 {
        return Vec::new();
    }
    if n == 1 || w == 0 {
        let mut out = vec![String::new(); n];
        out[0] = units.join(sep);
        return out;
    }
    if w < n {
        let mut out = vec![String::new(); n];
        for (j, u) in units.into_iter().enumerate() {
            let at = if w == 1 { 0 } else { j * (n - 1) / (w - 1) };
            out[at] = u;
        }
        return out;
    }

    // pos[b]: where a cut before unit b falls, in characters, in the middle of the separator.
    let half = sep.len() as f64 / 2.0;
    let mut pos = vec![0.0f64; w + 1];
    for (j, u) in units.iter().enumerate() {
        pos[j + 1] = pos[j] + (u.chars().count() + sep.len()) as f64;
    }
    let total = (pos[w] - 2.0 * half).max(1.0);
    pos.iter_mut().for_each(|p| *p -= half);
    let src_total: usize = source_lens.iter().sum();
    let share = |k: usize| -> f64 {
        if src_total == 0 {
            k as f64 / n as f64
        } else {
            source_lens[..k].iter().sum::<usize>() as f64 / src_total as f64
        }
    };
    let cost = |k: usize, b: usize| -> f64 {
        let off = (pos[b] - total * share(k + 1)) / total / 0.15;
        off * off - boundary_bonus(&units[b - 1], &units[b])
    };

    // best[k][b]: cheapest way to place cuts 0..=k with cut k before unit b.
    let cuts = n - 1;
    let mut best = vec![vec![f64::INFINITY; w]; cuts];
    let mut from = vec![vec![0usize; w]; cuts];
    for (b, slot) in best[0].iter_mut().enumerate().skip(1) {
        *slot = cost(0, b);
    }
    for k in 1..cuts {
        for b in (k + 1)..w {
            for p in k..b {
                let c = best[k - 1][p] + cost(k, b);
                if c < best[k][b] {
                    best[k][b] = c;
                    from[k][b] = p;
                }
            }
        }
    }
    let mut bounds = vec![0usize; n + 1];
    bounds[n] = w;
    let mut b = (cuts..w)
        .min_by(|&x, &y| best[cuts - 1][x].total_cmp(&best[cuts - 1][y]))
        .unwrap_or(w - 1);
    for k in (0..cuts).rev() {
        bounds[k + 1] = b;
        b = from[k][b];
    }
    (0..n)
        .map(|k| units[bounds[k]..bounds[k + 1]].join(sep))
        .collect()
}

fn boundary_bonus(before: &str, after: &str) -> f64 {
    let end = strip_closers(before);
    let mut bonus = 0.0;
    if end.ends_with(TERMINALS) {
        bonus += 1.5;
    } else if end.ends_with(SOFT) {
        bonus += 1.0;
    } else if end.ends_with(DASHES) || end.ends_with("--") {
        bonus += 0.8;
    } else if before.chars().count() <= 2 && before.chars().all(char::is_alphabetic) {
        // "не |", "to |", "a |": a part should not end on a word that leans on the next one.
        bonus -= 0.4;
    }
    if after.starts_with(DASHES) && !end.ends_with(DASHES) {
        bonus += 0.3;
    }
    bonus
}

fn ends_at_punctuation(part: &str, next: &str) -> bool {
    let end = strip_closers(part);
    end.ends_with(TERMINALS)
        || end.ends_with(SOFT)
        || end.ends_with(DASHES)
        || next.starts_with(DASHES)
}

/// What is shown under each cue of a sentence. Usually the cut translation; the whole
/// translation under every cue when cutting it would leave a cue without text, or when a
/// short two-cue sentence has no natural place to cut.
pub fn layout(translation: &str, source_lens: &[usize]) -> Vec<String> {
    let parts = distribute(translation, source_lens);
    if parts.len() < 2 {
        return parts;
    }
    let whole = || {
        let (units, sep) = units(translation);
        vec![units.join(sep); parts.len()]
    };
    if parts.iter().any(String::is_empty) {
        return whole();
    }
    let short = parts.iter().map(|p| p.chars().count()).sum::<usize>() <= SHORT;
    if parts.len() == 2 && short && !ends_at_punctuation(&parts[0], &parts[1]) {
        return whole();
    }
    parts
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

    /// Cues one second long with a short gap, so only the text decides.
    fn seq(texts: &[&str]) -> Vec<Cue> {
        texts
            .iter()
            .enumerate()
            .map(|(i, t)| cue(i as Ms * 1_200, i as Ms * 1_200 + 1_000, t))
            .collect()
    }

    fn one(r: Range<usize>) -> Vec<Range<usize>> {
        vec![r]
    }

    fn texts(cues: &[Cue], groups: &[Range<usize>]) -> Vec<String> {
        groups
            .iter()
            .map(|g| sentence_text(cues[g.clone()].iter().map(|c| c.text.as_str())))
            .collect()
    }

    #[test]
    fn fragments_join_until_the_sentence_ends() {
        let cues = seq(&["I told him that", "he didn't do it,", "she said.", "Okay."]);
        assert_eq!(group(&cues), [0..3, 3..4]);
        assert_eq!(
            texts(&cues, &group(&cues))[0],
            "I told him that he didn't do it, she said."
        );
        for end in ["Really?", "Stop!", "Well…", "好。", "本当！", "何？"] {
            assert_eq!(group(&seq(&[end, "Next"])), [0..1, 1..2], "{end}");
        }
    }

    #[test]
    fn closing_quotes_and_brackets_after_the_stop() {
        let cues = seq(&["\"He didn't do it.\"", "Then what?", "(He left.)", "Fine"]);
        assert_eq!(group(&cues), [0..1, 1..2, 2..3, 3..4]);
        assert!(ends_sentence("«Он ушёл.» "));
        assert!(!ends_sentence("\"He said\""));
    }

    #[test]
    fn ellipsis_ends_unless_the_next_cue_carries_on() {
        let cues = seq(&["I was going to...", "...go home.", "Maybe...", "Yes."]);
        assert_eq!(group(&cues), [0..2, 2..3, 3..4]);
        assert_eq!(texts(&cues, &group(&cues))[0], "I was going to... go home.");
        let lower = seq(&["I thought…", "never mind.", "Hm."]);
        assert_eq!(group(&lower), [0..2, 2..3]);
        assert!(
            !ends_firmly("Maybe..."),
            "a later cue may still continue it"
        );
        assert!(ends_firmly("Yes.\""));
    }

    #[test]
    fn a_pause_or_overlap_breaks_a_sentence() {
        let cues = vec![
            cue(0, 1_000, "and then"),
            cue(2_600, 3_000, "We waited"),
            cue(3_100, 4_000, "for the"),
            cue(3_200, 4_500, "SIGN TEXT"),
        ];
        assert_eq!(group(&cues), [0..1, 1..3, 3..4]);
        // A lowercase word after an unfinished sentence bridges a longer pause.
        let linked = vec![
            cue(0, 1_000, "that assembled on"),
            cue(3_500, 4_000, "the bank,"),
            cue(6_000, 7_000, "the birds"),
            cue(7_500, 8_000, "flew."),
            cue(10_000, 11_000, "and then"),
            cue(12_600, 13_000, "They left"),
            cue(17_500, 18_000, "for good."),
        ];
        assert_eq!(group(&linked), [0..4, 4..5, 5..6, 6..7]);
        // A tiny overlap is just sloppy timing, not two things on screen.
        let sloppy = vec![cue(0, 1_100, "and then"), cue(1_000, 2_000, "we left.")];
        assert_eq!(group(&sloppy), one(0..2));
    }

    #[test]
    fn a_new_speaker_starts_a_new_group() {
        let cues = seq(&[
            "- Where are you",
            "- Going home",
            "– to sleep",
            "—Now",
            "-Fine",
        ]);
        assert_eq!(group(&cues), [0..1, 1..2, 2..3, 3..4, 4..5]);
        let cont = seq(&["- I told him", "that he lied."]);
        assert_eq!(group(&cont), one(0..2));
    }

    #[test]
    fn groups_stay_small() {
        let cues = seq(&["one", "two", "three", "four", "five", "six."]);
        assert_eq!(group(&cues), [0..4, 4..6]);
        let long = "word ".repeat(30);
        let cues = seq(&[&long, &long, &long, "end."]);
        assert_eq!(group(&cues), [0..2, 2..4], "about 150 characters a cue");
    }

    #[test]
    fn multi_line_cues_and_cjk_join() {
        let cues = seq(&["They were indeed\na queer-looking party", "that assembled."]);
        assert_eq!(
            texts(&cues, &group(&cues)),
            ["They were indeed a queer-looking party that assembled."]
        );
        assert_eq!(cue_len("ab\ncd"), 5);
        let ja = seq(&["彼は昨日\n東京に", "行きました。"]);
        assert_eq!(texts(&ja, &group(&ja)), ["彼は昨日東京に行きました。"]);
        assert_eq!(cue_len("彼は\n東京"), 4);
    }

    #[test]
    fn adding_or_inserting_cues_keeps_other_groups() {
        let base = vec![
            cue(0, 1_000, "Hello there."),
            cue(1_200, 2_000, "I told him that"),
            cue(2_200, 3_000, "he didn't do it."),
            cue(10_000, 11_000, "Then we left"),
        ];
        let before = group(&base);
        assert_eq!(before, [0..1, 1..3, 3..4]);

        // Speech recognition appends the next window: only the open last group may grow.
        let mut grown = base.clone();
        grown.push(cue(11_200, 12_000, "for good."));
        grown.push(cue(12_200, 13_000, "Bye."));
        let after = group(&grown);
        assert_eq!(after[..2], before[..2]);
        assert_eq!(after[2..], [3..5, 5..6]);

        // A window filled in later between known cues touches only its neighbours.
        let mut inserted = grown.clone();
        inserted.insert(3, cue(5_000, 6_000, "Later,"));
        inserted.insert(4, cue(6_200, 7_000, "much later."));
        let after = group(&inserted);
        assert_eq!(after[..2], [0..1, 1..3]);
        assert_eq!(after[2..], [3..5, 5..7, 7..8]);

        // Every prefix groups the same way as the whole up to its last closed group.
        for cut in 1..inserted.len() {
            let part = group(&inserted[..cut]);
            let full = group(&inserted);
            let closed = part.len() - 1;
            assert_eq!(part[..closed], full[..closed], "prefix of {cut}");
        }
    }

    fn parts(t: &str, lens: &[usize]) -> Vec<String> {
        let p = distribute(t, lens);
        assert_eq!(p.len(), lens.len());
        p
    }

    #[test]
    fn distribution_follows_source_lengths_and_joins_back() {
        let t = "раз два три четыре пять шесть семь восемь девять десять";
        let p = parts(t, &[10, 10]);
        assert_eq!(p.join(" "), t);
        assert_eq!(
            p,
            ["раз два три четыре пять шесть", "семь восемь девять десять"]
        );
        let p = parts(t, &[30, 10]);
        assert_eq!(p.join(" "), t);
        assert!(p[0].len() > 2 * p[1].len(), "{p:?}");
        let p = parts(t, &[5, 5, 5, 5]);
        assert!(p.iter().all(|s| !s.is_empty()), "{p:?}");
        assert_eq!(p.join(" "), t);
    }

    #[test]
    fn distribution_prefers_a_cut_after_punctuation() {
        let t = "Я сказала ему, что он этого не делал, — сказала она.";
        let p = parts(t, &[15, 16, 9]);
        assert_eq!(
            p,
            ["Я сказала ему,", "что он этого не делал,", "— сказала она."]
        );
        assert_eq!(p.join(" "), t);
        let p = parts("Ну что ж, тогда пойдём домой вместе", &[12, 20]);
        assert_eq!(p[0], "Ну что ж,");
        // A cut far from the punctuation is still chosen when the lengths demand it.
        let p = parts(
            "Да, мы пойдём туда завтра утром все вместе пешком",
            &[30, 5],
        );
        assert_ne!(p[0], "Да,");
    }

    #[test]
    fn distribution_with_few_words() {
        assert_eq!(parts("Да", &[3, 3, 3]), ["Да", "", ""]);
        assert_eq!(parts("Ну да", &[3, 3, 3]), ["Ну", "", "да"]);
        assert_eq!(parts("", &[3, 3]), ["", ""]);
        assert_eq!(parts("  Привет  мир ", &[5]), ["Привет мир"]);
        assert!(parts("x", &[]).is_empty());
    }

    #[test]
    fn distribution_of_text_without_spaces() {
        let t = "我昨天去了东京，今天回来了。";
        let p = parts(t, &[6, 6]);
        assert_eq!(p.concat(), t);
        assert_eq!(p[0], "我昨天去了东京，");
        let p = parts("彼はiPhoneを買った。", &[5, 5, 5]);
        assert_eq!(p.concat(), "彼はiPhoneを買った。");
        assert!(p.iter().all(|s| !s.is_empty() && !s.starts_with('。')));
        assert!(p.iter().all(|s| !s.contains("iP") || s.contains("iPhone")));
    }

    #[test]
    fn layout_cuts_long_sentences_and_keeps_short_ones_whole() {
        let lens = [15, 16, 9];
        let t = "Я сказала ему, что он этого не делал, — сказала она.";
        assert_eq!(layout(t, &lens), distribute(t, &lens));
        // Nowhere natural to cut a short two-cue sentence: both cues show all of it.
        assert_eq!(layout("Куда ты идёшь?", &[13, 6]), ["Куда ты идёшь?"; 2]);
        assert_eq!(layout("Думаю, ты прав.", &[7, 12]), ["Думаю,", "ты прав."]);
        // Too few words for the cues: nobody is left without text.
        assert_eq!(layout("Ну да", &[3, 3, 3]), ["Ну да"; 3]);
        assert_eq!(layout("Привет", &[5]), ["Привет"]);
    }
}
