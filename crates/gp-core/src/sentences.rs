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
/// A two-cue sentence shorter than this, spoken in one breath, may show its whole translation
/// under both cues when there is no natural place to cut it. Anything longer shown twice
/// reads as a mistake.
const SHORT_SOURCE: usize = 40;
const SHORT_PAUSE: Ms = 700;
/// How far from its proportional place a cut may wander, as a share of the translation.
const SLACK: f64 = 0.15;

const CLOSERS: &[char] = &[
    '"', '\'', '”', '’', '»', ')', ']', '」', '』', '）', '】', '〕', '〉', '》',
];
const OPENERS: &[char] = &['"', '«', '„', '“', '‘', '‹', '「', '『', '（', '('];
const TERMINALS: &[char] = &['.', '!', '?', '…', '。', '！', '？', '‼', '⁉', '؟'];
const SOFT: &[char] = &[',', ';', ':', '，', '、', '；', '：'];
const DASHES: &[char] = &['—', '–'];

/// How a track writes its text, which decides where its sentences end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Style {
    #[default]
    Punctuated,
    /// Words with no full stop or comma anywhere, all lower case: the automatic captions of
    /// older YouTube videos. Only pauses and length say where a phrase ends.
    Bare,
}

/// For bare text: a group closes at a pause this long once it has a couple of lines…
const BARE_SHORT_PAUSE: Ms = 250;
/// …and always at one this long.
const BARE_PAUSE: Ms = 700;
/// Shorter than a punctuated sentence: without punctuation a long group is hard to translate.
const BARE_MAX_CHARS: usize = 200;

pub fn style_of(cues: &[Cue]) -> Style {
    if cues.len() < 20 {
        return Style::Punctuated;
    }
    let marked = cues
        .iter()
        .filter(|c| c.text.contains(TERMINALS) || c.text.contains(SOFT))
        .count();
    if marked * 20 < cues.len() {
        Style::Bare
    } else {
        Style::Punctuated
    }
}

/// Splits cues into sentence groups. Each break is decided from the two cues around it and
/// the size of the group so far, so cues added far ahead never change earlier groups.
pub fn group(cues: &[Cue]) -> Vec<Range<usize>> {
    group_as(cues, Style::Punctuated)
}

pub fn group_as(cues: &[Cue], style: Style) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let (mut start, mut chars) = (0, 0);
    for i in 0..cues.len() {
        chars += cue_len(&cues[i].text) + usize::from(i > start);
        let ends = match cues.get(i + 1) {
            None => true,
            Some(next) if style == Style::Bare => bare_breaks(&cues[i], next, i + 1 - start, chars),
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
        || exclamation(&next.text)
        || (ends_sentence(&cur.text) && !continues(&cur.text, &next.text))
}

fn bare_breaks(cur: &Cue, next: &Cue, count: usize, chars: usize) -> bool {
    let pause = next.start - cur.end;
    count >= MAX_CUES
        || chars + 1 + cue_len(&next.text) > BARE_MAX_CHARS
        || pause >= BARE_PAUSE
        || (count >= 2 && pause >= BARE_SHORT_PAUSE)
        || next.start + OVERLAP < cur.end
}

/// "Ugh!", "Really?", "Oh, John!": a short cue that is a sentence of its own even when the
/// cue before it was cut off mid-sentence.
fn exclamation(text: &str) -> bool {
    let flat = flatten(text);
    strip_closers(&flat).ends_with(['!', '?', '！', '？', '‼', '⁉'])
        && flat.split_whitespace().count() <= 2
        && flat
            .chars()
            .find(|c| c.is_alphabetic())
            .is_some_and(char::is_uppercase)
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

/// Punctuation at the end of a word or a cue.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Mark {
    None,
    Dash,
    Soft,
    Stop,
}

fn mark(text: &str) -> Mark {
    let end = strip_closers(text);
    if end.ends_with(TERMINALS) {
        Mark::Stop
    } else if end.ends_with(SOFT) {
        Mark::Soft
    } else if end.ends_with(DASHES) || end.ends_with('-') {
        Mark::Dash
    } else {
        Mark::None
    }
}

fn is_dash(unit: &str) -> bool {
    unit.starts_with(DASHES) || unit == "-" || unit == "--"
}

/// The cues of a sentence, or their translation, as units with the places to cut between them.
struct Side {
    units: Vec<String>,
    /// pos[b]: where a cut before unit b falls, in characters, in the middle of the separator.
    pos: Vec<f64>,
    /// The unit each sentence starts with; the first is always 0.
    starts: Vec<usize>,
    /// Capitalised words inside a sentence. Names keep their capital in any language I
    /// translate to with spaces, so they say which cue a piece of translation was spoken in.
    names: Vec<usize>,
}

impl Side {
    fn new(units: Vec<String>, seps: &[usize]) -> Side {
        let w = units.len();
        let mut pos = vec![0.0; w + 1];
        let mut at = 0.0;
        for (j, u) in units.iter().enumerate() {
            at += u.chars().count() as f64;
            let sep = if j + 1 < w { seps[j] as f64 } else { 0.0 };
            pos[j + 1] = at + sep / 2.0;
            at += sep;
        }
        let starts = sentence_starts(&units);
        let names = names(&units, &starts);
        Side {
            units,
            pos,
            starts,
            names,
        }
    }

    fn len(&self) -> usize {
        self.units.len()
    }

    fn total(&self) -> f64 {
        self.pos[self.len()].max(1.0)
    }

    fn sentence_of(&self, unit: usize) -> usize {
        self.starts.partition_point(|&s| s <= unit) - 1
    }

    fn sentence_end(&self, s: usize) -> usize {
        self.starts.get(s + 1).copied().unwrap_or(self.len())
    }
}

/// A full stop followed by a capital (or a script without capitals) starts a sentence;
/// "Ugh! said the Lory" and "Угг! - сказал" go on.
fn sentence_starts(units: &[String]) -> Vec<usize> {
    let mut out = vec![0];
    for j in 1..units.len() {
        if mark(&units[j - 1]) == Mark::Stop
            && units[j..]
                .iter()
                .flat_map(|u| u.chars())
                .find(|c| c.is_alphanumeric())
                .is_some_and(|c| !c.is_lowercase())
        {
            out.push(j);
        }
    }
    out
}

fn names(units: &[String], starts: &[usize]) -> Vec<usize> {
    (1..units.len())
        .filter(|&j| {
            let (u, prev) = (&units[j], &units[j - 1]);
            // "I", "I'll" are not names; a capital after a colon, a quote or a dash opens
            // quoted speech, which translations punctuate their own way.
            starts.binary_search(&j).is_err()
                && u.starts_with(char::is_uppercase)
                && u.chars().take_while(|c| c.is_alphabetic()).count() >= 2
                && !is_dash(prev)
                && !prev.ends_with([
                    ':', ';', '"', '\'', '«', '»', '“', '”', '—', '–', '-', '…', '.', '!', '?',
                ])
        })
        .collect()
}

/// Whether a capitalised word of the translation can be the same name as one of the source,
/// by its first two letters: "Alice" and "Алисе", not "Normans" and "Вильгельма". Only for
/// Latin and Cyrillic, the scripts with capitals that I can tell apart this way.
fn alike(src: &str, tr: &str) -> bool {
    let letters = |w: &str| -> Vec<char> {
        w.chars()
            .filter(|c| c.is_alphabetic())
            .take(2)
            .flat_map(char::to_lowercase)
            .collect()
    };
    let (s, t) = (letters(src), letters(tr));
    !s.is_empty() && !t.is_empty() && s.iter().zip(&t).all(|(&s, &t)| sounds(t).contains(s))
}

/// The Latin letters a letter of a name may be written with.
fn sounds(t: char) -> String {
    if t.is_ascii() {
        return t.to_string();
    }
    match t {
        'а' => "a",
        'б' => "b",
        'в' => "vw",
        'г' | 'ґ' => "gh",
        'д' => "d",
        'е' | 'ё' | 'э' | 'є' => "eaiyj",
        'ж' => "zjg",
        'з' => "zs",
        'и' | 'і' | 'ї' => "iey",
        'й' => "yj",
        'к' => "kcq",
        'л' => "l",
        'м' => "m",
        'н' => "n",
        'о' => "o",
        'п' => "p",
        'р' => "r",
        'с' => "sc",
        'т' => "t",
        'у' => "uw",
        'ф' => "fp",
        'х' => "hkc",
        'ц' => "ctz",
        'ч' | 'ш' | 'щ' => "cs",
        'ю' => "juy",
        'я' => "jy",
        _ => "",
    }
    .to_owned()
}

/// Names of the source matched to names of the translation, in order, skipping those
/// without a match ("North Umbria" is "Северной Умбрии"): (source cue, translation unit).
fn name_pairs(src: &Source, tr: &Side) -> Vec<(usize, usize)> {
    let (a, b) = (&src.side.names, &tr.names);
    let same = |i: usize, j: usize| alike(&src.side.units[a[i]], &tr.units[b[j]]);
    // Longest common subsequence: lcs[i][j] for a[i..] and b[j..].
    let mut lcs = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i][j] = if same(i, j) {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let (mut i, mut j, mut out) = (0, 0, Vec::new());
    while i < a.len() && j < b.len() {
        if same(i, j) && lcs[i][j] == lcs[i + 1][j + 1] + 1 {
            out.push((src.cue_of[a[i]], b[j]));
            (i, j) = (i + 1, j + 1);
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

struct Source {
    side: Side,
    /// The unit after the last one of each cue.
    cue_end: Vec<usize>,
    cue_of: Vec<usize>,
    marks: Vec<Mark>,
    /// Whether each cue starts with a capital or a quote, as quoted speech does.
    speech: Vec<bool>,
}

impl Source {
    fn new(sources: &[&str]) -> Source {
        let (mut all, mut seps, mut cue_end, mut cue_of) = (vec![], vec![], vec![], vec![]);
        for (k, text) in sources.iter().enumerate() {
            let (us, sep) = units(&flatten(text));
            for u in us {
                all.push(u);
                seps.push(sep.len());
                cue_of.push(k);
            }
            cue_end.push(all.len());
        }
        Source {
            side: Side::new(all, &seps),
            cue_end,
            cue_of,
            marks: sources.iter().map(|t| mark(&flatten(t))).collect(),
            speech: sources
                .iter()
                .map(|t| {
                    t.trim_start()
                        .starts_with(|c: char| c.is_uppercase() || OPENERS.contains(&c))
                })
                .collect(),
        }
    }
}

/// Where to cut the translation (unit indices, with 0 and its length at the ends). Each cut
/// k has a window it must fall in and a place it is drawn to:
/// - by sentence (both sides have as many sentences): a cue boundary that ends a sentence
///   is cut exactly where the translation of that sentence ends, and one inside a sentence
///   is cut inside its translation, at the same share of it;
/// - otherwise anywhere, at the cue's share of the whole.
///
/// By name, every name of the translation stays with the cue its source name is in.
fn cuts(
    tr: &Side,
    src: &Source,
    by_sentence: bool,
    anchors: &[(usize, usize)],
) -> Option<Vec<usize>> {
    let (w, n) = (tr.len(), src.cue_end.len());
    let sw = src.side.len();
    let mut spans = Vec::with_capacity(n - 1);
    for &e in &src.cue_end[..n - 1] {
        let span = if !by_sentence {
            (1, w - 1, tr.total() * src.side.pos[e] / src.side.total())
        } else {
            if e == 0 || e >= sw {
                return None;
            }
            let s = src.side.sentence_of(e);
            if src.side.starts[s] == e {
                let b = tr.starts[s];
                (b, b, tr.pos[b])
            } else {
                let (lo, hi) = (tr.starts[s], tr.sentence_end(s));
                let (slo, shi) = (src.side.starts[s], src.side.sentence_end(s));
                let r = (src.side.pos[e] - src.side.pos[slo])
                    / (src.side.pos[shi] - src.side.pos[slo]).max(1.0);
                (lo + 1, hi - 1, tr.pos[lo] + r * (tr.pos[hi] - tr.pos[lo]))
            }
        };
        spans.push(span);
    }
    let cost = |k: usize, b: usize| -> f64 {
        let (lo, hi, target) = spans[k];
        if b < lo.max(1) || b > hi || anchors.iter().any(|&(c, t)| (c <= k) != (t < b)) {
            return f64::INFINITY;
        }
        let off = (tr.pos[b] - target) / tr.total() / SLACK;
        off * off
            - boundary_bonus(
                &tr.units[b - 1],
                &tr.units[b],
                src.marks[k],
                src.speech[k + 1],
            )
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
            let c = cost(k, b);
            if c.is_infinite() {
                continue;
            }
            for p in k..b {
                let total = best[k - 1][p] + c;
                if total < best[k][b] {
                    best[k][b] = total;
                    from[k][b] = p;
                }
            }
        }
    }
    let mut b = (cuts..w).min_by(|&x, &y| best[cuts - 1][x].total_cmp(&best[cuts - 1][y]))?;
    if best[cuts - 1][b].is_infinite() {
        return None;
    }
    let mut bounds = vec![0usize; n + 1];
    bounds[n] = w;
    for k in (0..cuts).rev() {
        bounds[k + 1] = b;
        b = from[k][b];
    }
    Some(bounds)
}

/// Cuts the translation of a sentence group into one part per source cue, at word
/// boundaries, preferring a boundary after punctuation like the one the cue ends with.
/// Parts joined with a space (nothing for CJK) give back the translation. With fewer words
/// than cues the extra parts are empty.
pub fn distribute(translation: &str, sources: &[&str]) -> Vec<String> {
    let n = sources.len();
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

    let tr = Side::new(units, &vec![sep.len(); w]);
    let src = Source::new(sources);
    let by_sentence = tr.starts.len() > 1 && tr.starts.len() == src.side.starts.len();
    let names = name_pairs(&src, &tr);
    let bounds = [
        (by_sentence, &names[..]),
        (by_sentence, &[][..]),
        (false, &names[..]),
        (false, &[][..]),
    ]
    .into_iter()
    .find_map(|(s, anchors)| cuts(&tr, &src, s, anchors))
    .unwrap_or_else(|| (0..=n).map(|k| k * w / n).collect());
    (0..n)
        .map(|k| tr.units[bounds[k]..bounds[k + 1]].join(sep))
        .collect()
}

/// How good a cut between two translation units is, given how the source cue ends and
/// whether the next one opens quoted speech.
fn boundary_bonus(before: &str, after: &str, source: Mark, speech: bool) -> f64 {
    let here = mark(before);
    let mut bonus = match (here, source) {
        (Mark::Stop, Mark::Stop) => 2.0,
        // "Umbria, | Ugh!": an exclamation inside the translation is not where a cue that
        // ends with a comma ends.
        (Mark::Stop, Mark::Soft) => 0.8,
        (Mark::Stop, _) => 1.5,
        (Mark::Soft, Mark::Soft) => 1.5,
        (Mark::Soft, _) => 1.0,
        (Mark::Dash, _) => 0.8,
        // "не |", "to |", "a |": a part should not end on a word that leans on the next one.
        (Mark::None, _)
            if before.chars().count() <= 2 && before.chars().all(char::is_alphabetic) =>
        {
            -0.4
        }
        (Mark::None, _) => 0.0,
    };
    if is_dash(after) && here != Mark::Dash {
        bonus += 0.3;
    }
    // "called out, | Sit down" is "крикнула: | «Садитесь»".
    if speech && after.starts_with(OPENERS) {
        bonus += 0.5;
    }
    bonus
}

fn ends_at_punctuation(part: &str, next: &str) -> bool {
    mark(part) != Mark::None || next.split_whitespace().next().is_some_and(is_dash)
}

/// What is shown under each cue of a sentence group. Usually the cut translation; the whole
/// translation under every cue when there are fewer words than cues, or when a short
/// two-cue sentence said in one breath has no natural place to cut.
pub fn layout(translation: &str, cues: &[Cue]) -> Vec<String> {
    let sources: Vec<&str> = cues.iter().map(|c| c.text.as_str()).collect();
    let parts = distribute(translation, &sources);
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
    let short = sources.iter().map(|t| cue_len(t)).sum::<usize>() < SHORT_SOURCE
        && cues[1].start - cues[0].end <= SHORT_PAUSE;
    if parts.len() == 2 && short && !ends_at_punctuation(&parts[0], &parts[1]) {
        return whole();
    }
    fill_bare_parts(parts)
}

/// The other language may say a cue's words in the part before it, as "the success of |
/// machine learning | is compute" becomes "успех машинного обучения, | — | это вычисления":
/// the middle cue would show a lone dash. Such a part hands its punctuation to the part before
/// and shows that part again, which already holds its words; the line stays as it was.
fn fill_bare_parts(mut parts: Vec<String>) -> Vec<String> {
    let wordy: Vec<bool> = parts
        .iter()
        .map(|p| p.chars().any(char::is_alphanumeric))
        .collect();
    if wordy.iter().all(|&w| w) || !wordy.iter().any(|&w| w) {
        return parts;
    }
    let before = |i: usize| (0..i).rev().find(|&j| wordy[j]);
    let after = |i: usize| (i + 1..wordy.len()).find(|&j| wordy[j]);
    for i in (0..parts.len()).filter(|&i| !wordy[i]) {
        let bare = std::mem::take(&mut parts[i]);
        let bare = bare.trim();
        // A leading dash would read as another speaker, so the mark goes to the end before.
        if let Some(j) = before(i) {
            parts[j] = format!("{} {bare}", parts[j]);
        } else if let Some(j) = after(i) {
            parts[j] = format!("{bare} {}", parts[j]);
        }
    }
    for i in (0..parts.len()).filter(|&i| !wordy[i]) {
        if let Some(j) = before(i).or_else(|| after(i)) {
            parts[i] = parts[j].clone();
        }
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
    fn bare_captions_are_grouped_by_pauses_and_length() {
        // A lecture's automatic captions after the roll-up is undone (ByAn0MC0iWk, 10:09).
        let cues = vec![
            cue(609_210, 611_030, "well now we're back to unstructured and"),
            cue(611_040, 612_890, "so in general these kind of correspond"),
            cue(612_900, 616_020, "to semi structured data and"),
            cue(619_100, 620_460, "we'll see"),
            cue(620_790, 622_260, "whether I can actually use"),
            cue(625_850, 626_900, "okay so"),
            cue(626_910, 628_370, "slight glitch is that I don't actually"),
            cue(628_380, 630_760, "know how to get to hidden sides while"),
            cue(631_260, 635_330, "also screen casting so yeah so there's a"),
            cue(635_340, 636_740, "hidden slide in here that says something"),
            cue(636_750, 638_660, "about semi structured data but it's not"),
        ];
        assert_eq!(
            texts(&cues, &group_as(&cues, Style::Bare)),
            [
                "well now we're back to unstructured and so in general these kind of correspond to semi structured data and",
                "we'll see whether I can actually use",
                "okay so slight glitch is that I don't actually know how to get to hidden sides while",
                "also screen casting so yeah so there's a hidden slide in here that says something about semi structured data but it's not",
            ]
        );
        // The punctuated rules would run on across the pauses, four lines at a time.
        assert_eq!(group(&cues)[0], 0..4);

        let many = |text: &str| -> Vec<Cue> {
            (0..30)
                .map(|i| cue(i * 2_000, i * 2_000 + 1_990, text))
                .collect()
        };
        assert_eq!(style_of(&many("and so we go on")), Style::Bare);
        assert_eq!(style_of(&many("And so, we go on.")), Style::Punctuated);
        assert_eq!(
            style_of(&seq(&["no", "marks", "here"])),
            Style::Punctuated,
            "too few to tell"
        );
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

    /// Sources of the given lengths without punctuation, so only the lengths decide.
    fn parts(t: &str, lens: &[usize]) -> Vec<String> {
        let sources: Vec<String> = lens.iter().map(|&n| "x".repeat(n)).collect();
        let sources: Vec<&str> = sources.iter().map(String::as_str).collect();
        let p = distribute(t, &sources);
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
        let p = distribute(t, &["I told him that", "he didn't do it,", "she said."]);
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

    /// Cues with the given texts, `gap` apart.
    fn spoken(texts: &[&str], gap: Ms) -> Vec<Cue> {
        let mut at = 0;
        texts
            .iter()
            .map(|t| {
                let c = cue(at, at + 1_500, t);
                at += 1_500 + gap;
                c
            })
            .collect()
    }

    #[test]
    fn layout_cuts_long_sentences_and_keeps_short_ones_whole() {
        let cues = spoken(&["I told him that", "he didn't do it,", "she said."], 100);
        let t = "Я сказала ему, что он этого не делал, — сказала она.";
        let texts: Vec<&str> = cues.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(layout(t, &cues), distribute(t, &texts));
        // Nowhere natural to cut a short two-cue sentence: both cues show all of it.
        let cues = spoken(&["Where are you", "going?"], 100);
        assert_eq!(layout("Куда ты идёшь?", &cues), ["Куда ты идёшь?"; 2]);
        let cues = spoken(&["I think", "you're right."], 100);
        assert_eq!(layout("Думаю, ты прав.", &cues), ["Думаю,", "ты прав."]);
        // Too few words for the cues: nobody is left without text.
        let cues = spoken(&["Oh", "yes", "sure"], 100);
        assert_eq!(layout("Ну да", &cues), ["Ну да"; 3]);
        assert_eq!(layout("Привет", &spoken(&["Hello"], 0)), ["Привет"]);
    }

    #[test]
    fn a_sentence_is_shown_twice_only_when_short_and_said_at_once() {
        let t = "А потом он пошёл домой.";
        let joined = spoken(&["And then he", "went home."], 500);
        assert_eq!(layout(t, &joined), [t; 2]);
        let paused = spoken(&["And then he", "went home."], 900);
        let p = layout(t, &paused);
        assert_ne!(p[0], p[1]);
        assert_eq!(p.join(" "), t);
        let long = spoken(&["And then he turned around", "and went back home."], 0);
        assert_ne!(layout(t, &long)[0], layout(t, &long)[1]);
    }

    /// Checks what every layout must give: each cue gets something, and the parts are the
    /// translation, nothing repeated, nothing lost.
    fn cut(cues: &[Cue], t: &str) -> Vec<String> {
        let p = layout(t, cues);
        assert!(p.iter().all(|s| !s.is_empty()), "{p:?}");
        assert_eq!(p.join(" "), t);
        p
    }

    // The cases below are from speech recognition of "Alice in Wonderland", chapter 3, with
    // Google's translation, as the tester saw them.

    #[test]
    fn a_long_sentence_ending_in_a_short_cue_is_cut() {
        let cues = [
            cue(
                97_350,
                104_400,
                "And this Alice would not allow without knowing how old it",
            ),
            cue(104_400, 105_150, "was."),
        ];
        let p = cut(
            &cues,
            "И этого Алиса не позволила бы, не зная, сколько ей лет.",
        );
        assert_eq!(p[1], "ей лет.");

        let cues = [
            cue(
                133_220,
                139_060,
                "They all sat down at once, in a large ring, with the mouse",
            ),
            cue(139_060, 140_560, "in the middle."),
        ];
        let p = cut(
            &cues,
            "Все сели сразу, большим кольцом, с мышкой посередине.",
        );
        assert_eq!(p[1], "посередине.");
    }

    #[test]
    fn an_exclamation_stays_with_the_cue_it_is_said_in() {
        let cues = [
            cue(192_880, 196_630, "the earls of Mercia and North Umbria,"),
            cue(
                197_720,
                203_580,
                "Ugg! said the lorry with a shiver. I beg your pardon, said",
            ),
            cue(203_580, 207_360, "the mouse, crowning, but very politely."),
            cue(
                208_280,
                215_590,
                "Did you speak? Not I, said the lorry hastily. I thought you",
            ),
        ];
        assert_eq!(group(&cues), [0..3, 3..4]);
        let t = "графы Мерсии и Северной Умбрии, Угг! - сказал грузовик с дрожью. - Прошу \
                 прощения, - коронованно сказала мышка, но очень вежливо.";
        assert_eq!(
            cut(&cues[..3], t),
            [
                "графы Мерсии и Северной Умбрии,",
                "Угг! - сказал грузовик с дрожью. - Прошу прощения,",
                "- коронованно сказала мышка, но очень вежливо.",
            ]
        );

        // As separate cues the exclamation is a sentence of its own.
        let cues = vec![
            cue(0, 3_000, "the earls of Mercia and Northumbria,"),
            cue(3_100, 3_600, "Ugh!"),
            cue(3_700, 5_000, "said the Lory with a shiver."),
            cue(5_100, 6_000, "and then"),
            cue(6_100, 7_000, "Really, John?"),
        ];
        assert_eq!(group(&cues), [0..1, 1..2, 2..3, 3..4, 4..5]);
    }

    #[test]
    fn a_name_stays_with_the_cue_it_is_said_in() {
        let cues = [
            cue(
                62_520,
                68_810,
                "They had a consultation about this, and after a few minutes",
            ),
            cue(68_810, 72_010, "it seemed quite natural to Alice"),
            cue(
                72_010,
                77_860,
                "to find herself talking familiarly with them as if she had",
            ),
            cue(77_860, 80_330, "known them all her life."),
        ];
        assert_eq!(group(&cues), one(0..4));
        let t = "Они посовещались по этому поводу, и через несколько минут Алисе показалось \
                 вполне естественным обнаружить, что она разговаривает с ними так \
                 фамильярно, как если бы она знала их всю свою жизнь.";
        let p = cut(&cues, t);
        assert_eq!(
            p[..2],
            [
                "Они посовещались по этому поводу, и через несколько минут",
                "Алисе показалось вполне естественным обнаружить,",
            ]
        );

        // One capital on each side, but not the same name: no anchor.
        let cues = [
            cue(
                291_470,
                297_070,
                "William's conduct was at first moderate, but the insolence",
            ),
            cue(297_070, 298_330, "of his Normans,"),
        ];
        let p = cut(
            &cues,
            "Поведение Вильгельма поначалу было умеренным, но наглость его норманнов",
        );
        assert!(p[0].starts_with("Поведение Вильгельма поначалу"), "{p:?}");
        assert!(!p[1].contains("Вильгельма"), "{p:?}");

        // Names on both sides, but not all of them: the ones that match still hold.
        let cues = [
            cue(221_491, 226_821, "Edwin and Morcar, the earls of Mercia"),
            cue(226_940, 229_450, "and Northumbria, declared"),
            cue(
                229_450,
                236_594,
                "for him, and even Stigand, the patriotic Archbishop of Canterbury,",
            ),
            cue(237_105, 238_607, "found it advisable."),
        ];
        assert_eq!(group(&cues), one(0..4));
        let p = cut(
            &cues,
            "Эдвин и Моркар, графы Мерсии и Нортумбрии, высказались за него, и даже Стиганд, \
             патриотически настроенный архиепископ Кентерберийский, счел это целесообразным.",
        );
        assert_eq!(p[0], "Эдвин и Моркар, графы Мерсии", "{p:?}");
        assert!(p[1].starts_with("и Нортумбрии,"), "{p:?}");
        assert!(p[2].ends_with("Кентерберийский,"), "{p:?}");
        assert_eq!(p[3], "счел это целесообразным.");
    }

    #[test]
    fn sentences_of_a_group_go_to_their_own_cues() {
        let cues = [
            cue(
                117_530,
                122_260,
                "At last the mouse, who seemed to be a person of authority",
            ),
            cue(122_230, 124_320, "among them, called out,"),
            cue(
                124_320,
                131_260,
                "Sit down, all of you, and listen to me. I'll soon make you",
            ),
            cue(131_260, 132_640, "dry enough."),
        ];
        assert_eq!(group(&cues), one(0..4));
        let t = "Наконец мышь, которая, по-видимому, пользовалась среди них авторитетом, \
                 крикнула: «Садитесь все и слушайте меня». Скоро я тебя высушу достаточно.";
        let p = cut(&cues, t);
        assert!(p[2].starts_with("«Садитесь"), "{p:?}");
        assert!(
            p[3].ends_with("достаточно.") && !p[3].contains("Скоро я"),
            "{p:?}"
        );

        // The same words cut by the new recognition: the quote opens where the speech does.
        let cues = [
            cue(
                117_530,
                122_230,
                "At last the mouse, who seemed to be a person of authority",
            ),
            cue(122_230, 124_214, "among them, called out,"),
            cue(124_320, 128_715, "Sit down, all of you, and listen to me."),
        ];
        let t = "Наконец мышь, которая, по-видимому, пользовалась среди них авторитетом, \
                 крикнула: «Садитесь все и слушайте меня».";
        let p = cut(&cues, t);
        assert!(p[1].ends_with("крикнула:"), "{p:?}");
        assert_eq!(p[2], "«Садитесь все и слушайте меня».");
    }

    #[test]
    fn no_cue_is_left_with_a_lone_dash() {
        // Whisper's three lines of one sentence; Russian says "machine learning" in the first.
        let cues = vec![
            cue(
                668_600,
                673_500,
                "And finally, the final factor that impacts the success of",
            ),
            cue(673_600, 674_900, "machine learning"),
            cue(
                675_000,
                678_000,
                "is really compute and scaling of compute.",
            ),
        ];
        let t = "И, наконец, последний фактор, влияющий на успех машинного обучения, — это \
                 вычисления и масштабирование вычислений.";
        let texts: Vec<&str> = cues.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(distribute(t, &texts)[1], "—", "the cut this is about");
        let p = layout(t, &cues);
        assert_eq!(p.len(), 3);
        assert!(
            p.iter().all(|s| s.chars().any(char::is_alphanumeric)),
            "{p:?}"
        );
        assert!(!p[2].starts_with('—'), "{p:?}");
        assert!(p[1].contains("машинного обучения"), "{p:?}");
        // Nothing is lost or said twice when the parts are read in order.
        let mut said: Vec<&str> = Vec::new();
        for s in &p {
            if said.last() != Some(&s.as_str()) {
                said.push(s);
            }
        }
        assert_eq!(said.join(" "), t);
    }

    #[test]
    fn bare_parts_borrow_from_the_nearest_words() {
        let p = |v: &[&str]| fill_bare_parts(v.iter().map(|s| (*s).to_owned()).collect());
        assert_eq!(p(&["Да,", "—", "конечно."]), ["Да, —", "Да, —", "конечно."]);
        assert_eq!(p(&["…", "Нет."]), ["… Нет.", "… Нет."]);
        assert_eq!(p(&["Раз", "два"]), ["Раз", "два"]);
        assert_eq!(p(&["—", "…"]), ["—", "…"]);
    }
}
