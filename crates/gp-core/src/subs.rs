use crate::track::{Cue, Ms};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubFormat {
    Srt,
    WebVtt,
    Ass,
}

impl SubFormat {
    pub fn from_extension(ext: &str) -> Option<SubFormat> {
        match ext.to_ascii_lowercase().as_str() {
            "srt" => Some(SubFormat::Srt),
            "vtt" | "webvtt" => Some(SubFormat::WebVtt),
            "ass" | "ssa" => Some(SubFormat::Ass),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubError {
    UnknownFormat,
    Empty,
    TooLarge,
    Malformed { line: usize, reason: String },
}

/// Larger subtitle files are refused outright; real ones are well under a megabyte.
pub const MAX_SUBTITLE_BYTES: usize = 20 * 1024 * 1024;
/// A longer cue text is cut: the UI lays out every word of the cue on every frame.
pub const MAX_CUE_CHARS: usize = 2_000;

impl fmt::Display for SubError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SubError::UnknownFormat => write!(f, "unknown subtitle format"),
            SubError::Empty => write!(f, "subtitle file has no cues"),
            SubError::TooLarge => write!(
                f,
                "subtitle file is larger than {} MB",
                MAX_SUBTITLE_BYTES >> 20
            ),
            SubError::Malformed { line, reason } => write!(f, "line {line}: {reason}"),
        }
    }
}

impl std::error::Error for SubError {}

pub fn parse_subtitles(bytes: &[u8], hint: Option<SubFormat>) -> Result<Vec<Cue>, SubError> {
    if bytes.len() > MAX_SUBTITLE_BYTES {
        return Err(SubError::TooLarge);
    }
    let text = decode(bytes);
    let format = detect(&text).or(hint).ok_or(SubError::UnknownFormat)?;
    let mut cues = match format {
        SubFormat::Srt => parse_srt_like(&text),
        SubFormat::WebVtt => rolling_phrases(&text).unwrap_or_else(|| parse_srt_like(&text)),
        SubFormat::Ass => parse_ass(&text)?,
    };
    for c in &mut cues {
        if let Some((cut, _)) = c.text.char_indices().nth(MAX_CUE_CHARS) {
            c.text.truncate(cut);
            c.text.push('…');
        }
    }
    cues.retain(|c| c.end > c.start && !c.text.is_empty());
    cues.sort_by_key(|c| c.start);
    cues.dedup();
    let cues = collapse_rolling(cues);
    if cues.is_empty() {
        return Err(SubError::Empty);
    }
    Ok(cues)
}

/// Roll-up captions, the way YouTube writes its automatic ones, repeat the line before at the
/// top of every cue and add one line below it, with a 10 ms cue in between that shows only
/// the finished line. Read as they are, every line is on screen twice and a sentence group is
/// half repeats. I keep only what each cue adds. Ordinary subtitles are returned unchanged.
pub fn collapse_rolling(cues: Vec<Cue>) -> Vec<Cue> {
    if !is_rolling(&cues) {
        return cues;
    }
    let mut out: Vec<Cue> = Vec::new();
    let mut shown: Vec<&str> = Vec::new();
    for c in &cues {
        let lines: Vec<&str> = c.text.lines().collect();
        if c.end - c.start <= FLASH {
            shown = lines;
            continue;
        }
        let repeated = (1..=lines.len().min(shown.len()))
            .rev()
            .find(|&k| lines[..k] == shown[shown.len() - k..])
            .unwrap_or(0);
        shown.clone_from(&lines);
        let new = &lines[repeated..];
        if new.is_empty() {
            continue;
        }
        out.push(Cue {
            start: c.start,
            end: c.end,
            text: new.join("\n"),
        });
    }
    // A line stays up until the next one starts; two at once would only be the roll-up again.
    for i in 1..out.len() {
        let next = out[i].start;
        let prev = &mut out[i - 1];
        if prev.end > next && next > prev.start {
            prev.end = next;
        }
    }
    out
}

/// The cues between two roll-up steps, which only show the finished line again.
const FLASH: Ms = 50;
/// From one word to the next this long inside a roll-up line is a pause worth a cue of its own.
const PHRASE_PAUSE: Ms = 1_500;
/// A gap shorter than this between a line's last word and the next line is just speech.
const LINE_PAUSE: Ms = 300;

/// Roll-up captions with the time of every word, as YouTube writes its automatic ones: one cue
/// per line the way the site breaks them, also split where the speaker pauses, and ending with
/// its last word. Its own timing hides the pauses, which say where a phrase ends when the text
/// has no punctuation. `None` when the text is not of that kind.
fn rolling_phrases(text: &str) -> Option<Vec<Cue>> {
    if !text.contains("<c>") {
        return None;
    }
    let mut blocks: Vec<(Ms, Ms, Vec<&str>)> = Vec::new();
    let mut lines = text.lines().map(|l| l.trim_end_matches('\r'));
    while let Some(line) = lines.next() {
        if let Some((start, end)) = parse_timing_line(line) {
            // A line holding only a space is part of the cue here: the first cue starts with one.
            let body = lines.by_ref().take_while(|l| !l.is_empty()).collect();
            blocks.push((start, end, body));
        }
    }
    let plain: Vec<Cue> = blocks
        .iter()
        .map(|(start, end, body)| Cue {
            start: *start,
            end: *end,
            text: tidy(&strip_markup(&body.join("\n"))),
        })
        .collect();
    if !is_rolling(&plain) {
        return None;
    }
    // (time, word) for each line a cue adds, and when that line's cue ends.
    let mut said: Vec<(Vec<(Ms, String)>, Ms)> = Vec::new();
    let mut previous = String::new();
    for (start, end, body) in &blocks {
        if end - start <= FLASH {
            continue;
        }
        let Some(added) = body.iter().rev().find(|l| !l.trim().is_empty()) else {
            continue;
        };
        let words = timed_words(added, *start);
        let flat = words
            .iter()
            .map(|(_, w)| w.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        if words.is_empty() || flat == previous {
            continue;
        }
        previous = flat;
        said.push((words, *end));
    }
    let spoken = |w: &str| (w.chars().count() as Ms * 70 + 150).clamp(250, 800);
    let mut out = Vec::new();
    for (n, (words, line_end)) in said.iter().enumerate() {
        let next_line = said.get(n + 1).map(|(w, _)| w[0].0);
        let mut from = 0;
        for i in 1..=words.len() {
            let next = words.get(i).map(|w| w.0).or(next_line);
            let last = &words[i - 1];
            let gap = next.map_or(Ms::MAX, |t| t - last.0);
            let (cut, end) = if i == words.len() {
                let end = if gap - spoken(&last.1) >= LINE_PAUSE {
                    last.0 + spoken(&last.1)
                } else {
                    *line_end
                };
                (true, end.min(*line_end))
            } else {
                (gap >= PHRASE_PAUSE, last.0 + spoken(&last.1))
            };
            if !cut {
                continue;
            }
            let text: Vec<&str> = words[from..i].iter().map(|(_, w)| w.as_str()).collect();
            let start = words[from].0;
            out.push(Cue {
                start,
                end: end.max(start + 1),
                text: text.join(" "),
            });
            from = i;
        }
    }
    Some(out)
}

/// The words of a roll-up line, `first<00:00:01.200><c> second</c>…`, each with its time.
fn timed_words(line: &str, start: Ms) -> Vec<(Ms, String)> {
    let mut out = Vec::new();
    let mut at = start;
    let mut rest = line;
    loop {
        let stamp = rest.match_indices('<').find_map(|(i, _)| {
            let close = i + rest[i..].find('>')?;
            let inner = &rest[i + 1..close];
            parse_time(inner)
                .filter(|_| inner.contains(':'))
                .map(|t| (i, close, t))
        });
        let piece = stamp.map_or(rest, |(i, _, _)| &rest[..i]);
        let text = decode_entities(&strip_markup(piece));
        out.extend(text.split_whitespace().map(|w| (at, w.to_owned())));
        match stamp {
            Some((_, close, t)) => {
                at = t.max(at);
                rest = &rest[close + 1..];
            }
            None => break,
        }
    }
    out
}

/// Nearly every cue starts with the last line of the cue before it.
fn is_rolling(cues: &[Cue]) -> bool {
    if cues.len() < 6 {
        return false;
    }
    let repeats = cues
        .windows(2)
        .filter(|w| {
            let (Some(last), Some(first)) = (w[0].text.lines().last(), w[1].text.lines().next())
            else {
                return false;
            };
            last == first
        })
        .count();
    repeats * 4 >= (cues.len() - 1) * 3
}

pub fn write_srt(cues: &[Cue]) -> String {
    let mut out = String::new();
    for (i, c) in cues.iter().enumerate() {
        out.push_str(&format!(
            "{}\n{} --> {}\n{}\n\n",
            i + 1,
            srt_time(c.start),
            srt_time(c.end),
            c.text
        ));
    }
    out
}

fn srt_time(ms: Ms) -> String {
    let ms = ms.max(0);
    format!(
        "{:02}:{:02}:{:02},{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}

fn decode(bytes: &[u8]) -> String {
    if let Some((enc, bom_len)) = encoding_rs::Encoding::for_bom(bytes) {
        return enc
            .decode_without_bom_handling(&bytes[bom_len..])
            .0
            .into_owned();
    }
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_owned();
    }
    // Not UTF-8 and no BOM: an old single-byte file. In Cyrillic text nearly every letter
    // is in 0xC0..=0xFF, while Western European text has only a few such bytes.
    let high = bytes.iter().filter(|&&b| b >= 0xC0).count();
    let letters = bytes
        .iter()
        .filter(|&&b| b.is_ascii_alphabetic() || b >= 0xC0)
        .count();
    let enc = if high * 3 > letters {
        encoding_rs::WINDOWS_1251
    } else {
        encoding_rs::WINDOWS_1252
    };
    enc.decode_without_bom_handling(bytes).0.into_owned()
}

fn detect(text: &str) -> Option<SubFormat> {
    let head = text.trim_start();
    if head.starts_with("WEBVTT") {
        return Some(SubFormat::WebVtt);
    }
    if head.starts_with("[Script Info]")
        || text
            .lines()
            .any(|l| l.starts_with("Dialogue:") || l.trim() == "[Events]")
    {
        return Some(SubFormat::Ass);
    }
    // I accept SRT by its timing line: web servers hand it over without a name or header.
    if text.lines().take(20).any(|l| l.contains(" --> ")) {
        return Some(SubFormat::Srt);
    }
    None
}

/// `H:MM:SS,mmm`, `MM:SS.mmm` or `H:MM:SS.cc`.
fn parse_time(s: &str) -> Option<Ms> {
    let s = s.trim();
    let (clock, frac) = match s.rfind([',', '.']) {
        Some(i) => (&s[..i], &s[i + 1..]),
        None => (s, ""),
    };
    let mut total: Ms = 0;
    let mut parts = 0;
    for p in clock.split(':') {
        total = total
            .checked_mul(60)?
            .checked_add(p.trim().parse::<Ms>().ok()?)?;
        parts += 1;
    }
    if !(2..=3).contains(&parts) || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut ms: Ms = 0;
    for i in 0..3 {
        ms = ms * 10 + frac.as_bytes().get(i).map_or(0, |b| (b - b'0') as Ms);
    }
    total.checked_mul(1000)?.checked_add(ms)
}

fn parse_timing_line(line: &str) -> Option<(Ms, Ms)> {
    let (a, b) = line.split_once("-->")?;
    let end = b.split_whitespace().next()?;
    Some((parse_time(a)?, parse_time(end)?))
}

// SRT and WebVTT share the cue structure. I skip broken blocks instead of failing:
// subtitles from the internet are often malformed, and losing one line beats
// refusing the whole track.
fn parse_srt_like(text: &str) -> Vec<Cue> {
    let mut cues = Vec::new();
    let mut lines = text.lines().map(|l| l.trim_end_matches('\r')).peekable();
    while let Some(line) = lines.next() {
        let Some((start, end)) = parse_timing_line(line) else {
            continue;
        };
        let mut body: Vec<&str> = Vec::new();
        while let Some(next) = lines.peek() {
            if next.trim().is_empty() {
                break;
            }
            if parse_timing_line(next).is_some() {
                // No blank line between blocks, so the last text line is really the number
                // of the next block.
                if body
                    .last()
                    .is_some_and(|l| l.trim().bytes().all(|b| b.is_ascii_digit()))
                {
                    body.pop();
                }
                break;
            }
            body.push(lines.next().unwrap());
        }
        let clean = decode_entities(&strip_markup(&body.join("\n")));
        cues.push(Cue {
            start,
            end,
            text: tidy(&clean),
        });
    }
    cues
}

fn parse_ass(text: &str) -> Result<Vec<Cue>, SubError> {
    let mut cues = Vec::new();
    let mut in_events = false;
    // The ASS v4+ default field order; a Format line overrides it.
    let (mut i_start, mut i_end, mut i_text, mut n_fields) = (1usize, 2usize, 9usize, 10usize);
    for (n, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.starts_with('[') {
            in_events = line.eq_ignore_ascii_case("[Events]");
            continue;
        }
        if !in_events {
            continue;
        }
        if let Some(fmt) = line.strip_prefix("Format:") {
            let names: Vec<String> = fmt
                .split(',')
                .map(|s| s.trim().to_ascii_lowercase())
                .collect();
            let find = |name: &str| names.iter().position(|x| x == name);
            match (find("start"), find("end"), find("text")) {
                (Some(s), Some(e), Some(t)) => {
                    (i_start, i_end, i_text, n_fields) = (s, e, t, names.len());
                }
                _ => {
                    return Err(SubError::Malformed {
                        line: n + 1,
                        reason: "Format line lacks Start/End/Text".into(),
                    });
                }
            }
            continue;
        }
        let Some(rest) = line.strip_prefix("Dialogue:") else {
            continue;
        };
        let fields: Vec<&str> = rest.splitn(n_fields, ',').collect();
        if fields.len() < n_fields {
            continue;
        }
        let (Some(start), Some(end)) = (parse_time(fields[i_start]), parse_time(fields[i_end]))
        else {
            continue;
        };
        if let Some(body) = clean_ass_text(fields[i_text]) {
            cues.push(Cue {
                start,
                end,
                text: tidy(&body),
            });
        }
    }
    Ok(cues)
}

/// None when the event is a vector drawing (`p1`) rather than text.
fn clean_ass_text(raw: &str) -> Option<String> {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let Some(close) = rest[open..].find('}') else {
            rest = "";
            break;
        };
        let tags = &rest[open + 1..open + close];
        if tags.split('\\').any(|t| {
            t.strip_prefix('p')
                .is_some_and(|d| d.parse::<u32>().is_ok_and(|v| v > 0))
        }) {
            return None;
        }
        rest = &rest[open + close + 1..];
    }
    out.push_str(rest);
    let out = out
        .replace("\\N", "\n")
        .replace("\\n", "\n")
        .replace("\\h", " ");
    Some(strip_markup(&out))
}

fn strip_markup(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.char_indices();
    // A closer is only looked for up to the end of the line. When a search fails I remember
    // where the line ends, so a run of unclosed openers costs one scan, not one per opener.
    let mut line_end = 0;
    let mut no_closer_until = [0usize; 2];
    while let Some((i, c)) = chars.next() {
        let (closer, slot) = match c {
            // A tag starts with a letter, a digit or '/', so "2 < 3 and 5 > 4" stays intact.
            '<' if s[i + 1..].starts_with(|n: char| n.is_ascii_alphanumeric() || n == '/') => {
                ('>', 0)
            }
            '{' if s[i + 1..].starts_with('\\') => ('}', 1),
            _ => {
                out.push(c);
                continue;
            }
        };
        if i >= line_end {
            line_end = s[i..].find('\n').map_or(s.len(), |n| i + n);
        }
        let found = if i < no_closer_until[slot] {
            None
        } else {
            let found = s[i..line_end].find(closer);
            if found.is_none() {
                no_closer_until[slot] = line_end;
            }
            found
        };
        match found {
            Some(len) => {
                for _ in 0..s[i + 1..=i + len].chars().count() {
                    chars.next();
                }
            }
            None => out.push(c),
        }
    }
    out
}

/// One pass, so "&amp;lt;" becomes "&lt;" and not "<".
fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let decoded = tail
            .find(';')
            .filter(|&end| end <= 10)
            .and_then(|end| entity(&tail[1..end]).map(|text| (text, end + 1)));
        match decoded {
            Some((text, len)) => {
                out.push_str(&text);
                rest = &tail[len..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn entity(name: &str) -> Option<String> {
    let text = match name {
        "lt" => "<".to_owned(),
        "gt" => ">".to_owned(),
        "amp" => "&".to_owned(),
        "quot" => "\"".to_owned(),
        "apos" => "'".to_owned(),
        "nbsp" => " ".to_owned(),
        "lrm" | "rlm" => String::new(),
        _ => {
            let digits = name.strip_prefix('#')?;
            let code = match digits.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => digits.parse().ok()?,
            };
            char::from_u32(code).filter(|c| *c != '\0')?.to_string()
        }
    };
    Some(text)
}

/// `strip_markup` leaves a tag that is split across lines, and callers later join the lines
/// with spaces, which would turn it back into a real tag. I drop the opening `<` of any
/// tag-looking start with no `>` after it on its line.
fn drop_unclosed_tag_starts(line: &str) -> String {
    let last_gt = line.rfind('>');
    let mut out = String::with_capacity(line.len());
    let mut chars = line.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let tag_start = c == '<'
            && chars
                .peek()
                .is_some_and(|&(_, n)| n.is_ascii_alphanumeric() || n == '/');
        if !(tag_start && last_gt.is_none_or(|g| g < i)) {
            out.push(c);
        }
    }
    out
}

/// Text a player handed over, cleaned the way cues from a file are.
pub fn plain_text(s: &str) -> String {
    tidy(&decode_entities(&strip_markup(s)))
}

fn tidy(s: &str) -> String {
    s.lines()
        .map(drop_unclosed_tag_starts)
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
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
    fn srt_basic_with_tags_and_crlf() {
        let src = "1\r\n00:00:01,000 --> 00:00:02,500\r\n<i>Hello</i>, {\\an8}world\r\nsecond line\r\n\r\n2\r\n00:01:00,000 --> 00:01:01,000\r\nBye\r\n";
        let sniffed = parse_subtitles(src.as_bytes(), None).unwrap();
        assert_eq!(sniffed.len(), 2, "SRT is recognised by its timing line");
        assert_eq!(
            parse_subtitles(b"just some text", None).unwrap_err(),
            SubError::UnknownFormat
        );
        let cues = parse_subtitles(src.as_bytes(), Some(SubFormat::Srt)).unwrap();
        assert_eq!(
            cues,
            vec![
                cue(1000, 2500, "Hello, world\nsecond line"),
                cue(60_000, 61_000, "Bye")
            ]
        );
    }

    #[test]
    fn srt_tolerates_missing_blank_line_and_dot_separator() {
        let src = "1\n00:00:01.000 --> 00:00:02.000\nOne\n2\n00:00:03.000 --> 00:00:04.000\nTwo\n";
        let cues = parse_subtitles(src.as_bytes(), Some(SubFormat::Srt)).unwrap();
        assert_eq!(cues.len(), 2);
        assert_eq!(cues, vec![cue(1000, 2000, "One"), cue(3000, 4000, "Two")]);
    }

    #[test]
    fn srt_decodes_html_entities_once() {
        let src = "1
00:00:01,000 --> 00:00:02,000
Tom &amp; Jerry&nbsp;&lt; &quot;hi&quot; &#39;x&#x27; AT&T &amp;lt; &bogus;
";
        let cues = parse_subtitles(src.as_bytes(), Some(SubFormat::Srt)).unwrap();
        assert_eq!(cues[0].text, "Tom & Jerry < \"hi\" 'x' AT&T &lt; &bogus;");
    }

    #[test]
    fn srt_keeps_lone_angle_bracket() {
        let src = "1\n00:00:01,000 --> 00:00:02,000\n2 < 3 and 5 > 4\n";
        let cues = parse_subtitles(src.as_bytes(), Some(SubFormat::Srt)).unwrap();
        assert_eq!(cues[0].text, "2 < 3 and 5 > 4");
    }

    #[test]
    fn vtt_header_notes_settings_entities() {
        let src = "WEBVTT - title\n\nNOTE this is\na comment\n\nSTYLE\n::cue { color: red }\n\nintro\n00:01.000 --> 00:02.000 align:start position:10%\n<v Bob>Tom &amp; Jerry</v> <00:01.500><c>ok</c>\n\n01:00:00.000 --> 01:00:01.250\nLate\n";
        let cues = parse_subtitles(src.as_bytes(), None).unwrap();
        assert_eq!(
            cues,
            vec![
                cue(1000, 2000, "Tom & Jerry ok"),
                cue(3_600_000, 3_601_250, "Late")
            ]
        );
    }

    #[test]
    fn ass_format_order_commas_overrides_drawings() {
        let src = "[Script Info]\nTitle: x\n\n[V4+ Styles]\nFormat: Name, Fontname\nStyle: Default,Arial\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nComment: 0,0:00:00.00,0:00:01.00,Default,,0,0,0,,skip me\nDialogue: 0,0:00:01.50,0:00:03.00,Default,,0,0,0,,{\\i1}Hello{\\i0}, world\\Nnext\\hline\nDialogue: 0,0:00:04.00,0:00:05.00,Default,,0,0,0,,{\\p1}m 0 0 l 10 10{\\p0}\nDialogue: 0,1:00:00.05,1:00:01.00,Default,,0,0,0,,Late\n";
        let cues = parse_subtitles(src.as_bytes(), None).unwrap();
        assert_eq!(
            cues,
            vec![
                cue(1500, 3000, "Hello, world\nnext line"),
                cue(3_600_050, 3_601_000, "Late")
            ]
        );
    }

    #[test]
    fn ass_custom_field_order() {
        let src = "[Events]\nFormat: Start, End, Text\nDialogue: 0:00:01.00,0:00:02.00,a, b\n";
        let cues = parse_subtitles(src.as_bytes(), None).unwrap();
        assert_eq!(cues, vec![cue(1000, 2000, "a, b")]);
    }

    #[test]
    fn sorts_dedups_and_drops_invalid() {
        let src = "1\n00:00:05,000 --> 00:00:06,000\nB\n\n2\n00:00:01,000 --> 00:00:02,000\nA\n\n3\n00:00:01,000 --> 00:00:02,000\nA\n\n4\n00:00:09,000 --> 00:00:08,000\nbackwards\n\n5\n00:00:10,000 --> 00:00:11,000\n<i></i>\n";
        let cues = parse_subtitles(src.as_bytes(), Some(SubFormat::Srt)).unwrap();
        assert_eq!(cues, vec![cue(1000, 2000, "A"), cue(5000, 6000, "B")]);
    }

    #[test]
    fn overlapping_cues_are_kept() {
        let src =
            "1\n00:00:01,000 --> 00:00:05,000\nlong\n\n2\n00:00:02,000 --> 00:00:03,000\nshort\n";
        let cues = parse_subtitles(src.as_bytes(), Some(SubFormat::Srt)).unwrap();
        assert_eq!(cues.len(), 2);
    }

    #[test]
    fn encodings() {
        let body = "1\n00:00:01,000 --> 00:00:02,000\nПривет, мир\n";
        let mut utf8_bom = vec![0xEF, 0xBB, 0xBF];
        utf8_bom.extend_from_slice(body.as_bytes());
        let mut utf16 = vec![0xFF, 0xFE];
        utf16.extend(body.encode_utf16().flat_map(|u| u.to_le_bytes()));
        let cp1251 = encoding_rs::WINDOWS_1251.encode(body).0.into_owned();
        for bytes in [utf8_bom, utf16, cp1251] {
            let cues = parse_subtitles(&bytes, Some(SubFormat::Srt)).unwrap();
            assert_eq!(cues[0].text, "Привет, мир");
        }
        let latin = encoding_rs::WINDOWS_1252
            .encode("1\n00:00:01,000 --> 00:00:02,000\nCafé déjà vu, the long english part\n")
            .0
            .into_owned();
        let cues = parse_subtitles(&latin, Some(SubFormat::Srt)).unwrap();
        assert_eq!(cues[0].text, "Café déjà vu, the long english part");
    }

    fn srt_one(text: &str) -> String {
        format!("1\n00:00:01,000 --> 00:00:02,000\n{text}\n")
    }

    #[test]
    fn unclosed_tags_are_parsed_in_linear_time() {
        let started = std::time::Instant::now();
        for opener in ["<a ", "{\\a "] {
            let src = srt_one(&opener.repeat(200_000));
            let cues = parse_subtitles(src.as_bytes(), Some(SubFormat::Srt)).unwrap();
            assert!(!cues.is_empty());
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn oversized_file_is_refused() {
        let big = vec![b' '; MAX_SUBTITLE_BYTES + 1];
        assert_eq!(
            parse_subtitles(&big, Some(SubFormat::Srt)),
            Err(SubError::TooLarge)
        );
        assert!(SubError::TooLarge.to_string().contains("20 MB"));
    }

    #[test]
    fn long_cue_is_cut_at_a_char_boundary() {
        let src = srt_one(&"я".repeat(MAX_CUE_CHARS + 500));
        let cues = parse_subtitles(src.as_bytes(), Some(SubFormat::Srt)).unwrap();
        assert_eq!(cues[0].text.chars().count(), MAX_CUE_CHARS + 1);
        assert!(cues[0].text.ends_with('…'));
        let exact = srt_one(&"я".repeat(MAX_CUE_CHARS));
        let cues = parse_subtitles(exact.as_bytes(), Some(SubFormat::Srt)).unwrap();
        assert!(!cues[0].text.ends_with('…'));
    }

    #[test]
    fn huge_time_values_do_not_overflow() {
        assert_eq!(parse_time("99999999999999999:00:00,000"), None);
        assert_eq!(parse_time("9223372036854775807:00,000"), None);
        let src = "1\n99999999999999999:00:00,000 --> 99999999999999999:00:01,000\nx\n";
        assert_eq!(
            parse_subtitles(src.as_bytes(), Some(SubFormat::Srt)),
            Err(SubError::Empty)
        );
        assert_eq!(parse_time("01:02:03,004"), Some(3_723_004));
    }

    #[test]
    fn ass_strips_angle_tags() {
        let src = "[Events]\nFormat: Start, End, Text\nDialogue: 0:00:01.00,0:00:02.00,Hello <img src=x onerror=alert(1)> world\n";
        let cues = parse_subtitles(src.as_bytes(), None).unwrap();
        assert_eq!(cues[0].text, "Hello world");
    }

    #[test]
    fn tag_split_by_newline_does_not_reassemble_after_joining() {
        let src = srt_one("Hello <i\nmg src=x onerror=alert(1)> world");
        let cues = parse_subtitles(src.as_bytes(), Some(SubFormat::Srt)).unwrap();
        let joined = cues[0].text.replace('\n', " ");
        assert_eq!(strip_markup(&joined), joined, "joined: {joined}");
        assert!(!joined.contains('<'), "joined: {joined}");
    }

    /// Automatic captions of a lecture on YouTube (ByAn0MC0iWk, 9:50–10:42), as yt-dlp
    /// downloads them with --write-auto-subs --sub-format vtt.
    const AUTO_VTT: &str = r#"WEBVTT
Kind: captions
Language: en

00:09:50.630 --> 00:09:50.640 align:start position:0%
a database and the columns correspondent
 

00:09:50.640 --> 00:09:52.130 align:start position:0%
a database and the columns correspondent
two<00:09:50.730><c> fields</c><00:09:51.120><c> well</c><00:09:51.300><c> if</c><00:09:51.450><c> those</c><00:09:51.600><c> fields</c>

00:09:52.130 --> 00:09:52.140 align:start position:0%
two fields well if those fields
 

00:09:52.140 --> 00:09:56.510 align:start position:0%
two fields well if those fields
themselves<00:09:52.800><c> were</c><00:09:53.750><c> sort</c><00:09:54.750><c> of</c><00:09:54.840><c> simple</c><00:09:55.740><c> data</c><00:09:56.130><c> like</c>

00:09:56.510 --> 00:09:56.520 align:start position:0%
themselves were sort of simple data like
 

00:09:56.520 --> 00:09:59.210 align:start position:0%
themselves were sort of simple data like
binary<00:09:57.090><c> values</c><00:09:57.750><c> or</c><00:09:58.020><c> multi</c><00:09:58.290><c> class</c><00:09:58.410><c> values</c><00:09:58.950><c> then</c>

00:09:59.210 --> 00:09:59.220 align:start position:0%
binary values or multi class values then
 

00:09:59.220 --> 00:10:01.130 align:start position:0%
binary values or multi class values then
sure<00:09:59.430><c> we</c><00:09:59.730><c> have</c><00:09:59.880><c> something</c><00:10:00.090><c> structured</c><00:10:00.630><c> but</c><00:10:00.780><c> as</c>

00:10:01.130 --> 00:10:01.140 align:start position:0%
sure we have something structured but as
 

00:10:01.140 --> 00:10:02.600 align:start position:0%
sure we have something structured but as
soon<00:10:01.320><c> as</c><00:10:01.380><c> you</c><00:10:01.500><c> open</c><00:10:01.740><c> up</c><00:10:01.830><c> the</c><00:10:01.980><c> spreadsheet</c><00:10:02.250><c> to</c>

00:10:02.600 --> 00:10:02.610 align:start position:0%
soon as you open up the spreadsheet to
 

00:10:02.610 --> 00:10:06.080 align:start position:0%
soon as you open up the spreadsheet to
allow<00:10:02.820><c> arbitrary</c><00:10:03.480><c> text</c><00:10:03.750><c> in</c><00:10:03.990><c> it</c><00:10:04.110><c> or</c><00:10:04.910><c> even</c><00:10:05.910><c> you</c>

00:10:06.080 --> 00:10:06.090 align:start position:0%
allow arbitrary text in it or even you
 

00:10:06.090 --> 00:10:07.520 align:start position:0%
allow arbitrary text in it or even you
know<00:10:06.120><c> pasting</c><00:10:06.660><c> the</c><00:10:06.930><c> the</c><00:10:07.170><c> binary</c>

00:10:07.520 --> 00:10:07.530 align:start position:0%
know pasting the the binary
 

00:10:07.530 --> 00:10:09.200 align:start position:0%
know pasting the the binary
representation<00:10:08.130><c> of</c><00:10:08.190><c> an</c><00:10:08.310><c> image</c><00:10:08.640><c> into</c><00:10:08.820><c> a</c><00:10:08.940><c> cell</c>

00:10:09.200 --> 00:10:09.210 align:start position:0%
representation of an image into a cell
 

00:10:09.210 --> 00:10:11.030 align:start position:0%
representation of an image into a cell
well<00:10:09.690><c> now</c><00:10:09.900><c> we're</c><00:10:10.080><c> back</c><00:10:10.200><c> to</c><00:10:10.260><c> unstructured</c><00:10:10.770><c> and</c>

00:10:11.030 --> 00:10:11.040 align:start position:0%
well now we're back to unstructured and
 

00:10:11.040 --> 00:10:12.890 align:start position:0%
well now we're back to unstructured and
so<00:10:11.190><c> in</c><00:10:11.400><c> general</c><00:10:11.850><c> these</c><00:10:12.150><c> kind</c><00:10:12.390><c> of</c><00:10:12.420><c> correspond</c>

00:10:12.890 --> 00:10:12.900 align:start position:0%
so in general these kind of correspond
 

00:10:12.900 --> 00:10:20.780 align:start position:0%
so in general these kind of correspond
to<00:10:13.910><c> semi</c><00:10:14.910><c> structured</c><00:10:15.330><c> data</c><00:10:15.660><c> and</c><00:10:19.100><c> we'll</c><00:10:20.100><c> see</c>

00:10:20.780 --> 00:10:20.790 align:start position:0%
to semi structured data and we'll see
 

00:10:20.790 --> 00:10:26.900 align:start position:0%
to semi structured data and we'll see
whether<00:10:21.240><c> I</c><00:10:21.420><c> can</c><00:10:21.720><c> actually</c><00:10:21.900><c> use</c><00:10:25.850><c> okay</c><00:10:26.850><c> so</c>

00:10:26.900 --> 00:10:26.910 align:start position:0%
whether I can actually use okay so
 

00:10:26.910 --> 00:10:28.370 align:start position:0%
whether I can actually use okay so
slight<00:10:27.330><c> glitch</c><00:10:27.660><c> is</c><00:10:27.870><c> that</c><00:10:27.990><c> I</c><00:10:28.020><c> don't</c><00:10:28.230><c> actually</c>

00:10:28.370 --> 00:10:28.380 align:start position:0%
slight glitch is that I don't actually
 

00:10:28.380 --> 00:10:31.250 align:start position:0%
slight glitch is that I don't actually
know<00:10:28.620><c> how</c><00:10:28.740><c> to</c><00:10:28.770><c> get</c><00:10:28.920><c> to</c><00:10:29.040><c> hidden</c><00:10:29.400><c> sides</c><00:10:30.260><c> while</c>

00:10:31.250 --> 00:10:31.260 align:start position:0%
know how to get to hidden sides while
 

00:10:31.260 --> 00:10:35.330 align:start position:0%
know how to get to hidden sides while
also<00:10:31.710><c> screen</c><00:10:32.220><c> casting</c><00:10:32.670><c> so</c><00:10:32.910><c> yeah</c><00:10:33.440><c> so</c><00:10:34.440><c> there's</c><00:10:35.220><c> a</c>

00:10:35.330 --> 00:10:35.340 align:start position:0%
also screen casting so yeah so there's a
 

00:10:35.340 --> 00:10:36.740 align:start position:0%
also screen casting so yeah so there's a
hidden<00:10:35.580><c> slide</c><00:10:35.850><c> in</c><00:10:36.000><c> here</c><00:10:36.030><c> that</c><00:10:36.330><c> says</c><00:10:36.360><c> something</c>

00:10:36.740 --> 00:10:36.750 align:start position:0%
hidden slide in here that says something
 

00:10:36.750 --> 00:10:38.660 align:start position:0%
hidden slide in here that says something
about<00:10:36.930><c> semi</c><00:10:37.710><c> structured</c><00:10:38.100><c> data</c><00:10:38.130><c> but</c><00:10:38.460><c> it's</c><00:10:38.550><c> not</c>

00:10:38.660 --> 00:10:38.670 align:start position:0%
about semi structured data but it's not
 

00:10:38.670 --> 00:10:41.840 align:start position:0%
about semi structured data but it's not
terribly<00:10:39.000><c> important</c><00:10:39.480><c> and</c><00:10:40.520><c> and</c><00:10:41.520><c> it</c><00:10:41.610><c> would</c><00:10:41.730><c> have</c>

00:10:41.840 --> 00:10:41.850 align:start position:0%
terribly important and and it would have
 

"#;

    #[test]
    fn automatic_captions_lose_their_roll_up_repeats() {
        let raw = parse_srt_like(&decode(AUTO_VTT.as_bytes()));
        assert_eq!(raw.len(), 37);
        // Every word is said once: the repeats are gone and nothing was lost.
        let said: Vec<&str> = raw
            .iter()
            .filter(|c| c.end - c.start > FLASH)
            .flat_map(|c| c.text.lines().last().unwrap().split(' '))
            .collect();
        let words = |cues: &[Cue]| -> Vec<String> {
            cues.iter()
                .flat_map(|c| c.text.split(' ').map(str::to_owned))
                .collect()
        };

        // Without word times only the repeats go: one cue per line, as the site shows them.
        let lines = collapse_rolling(raw.clone());
        assert_eq!(lines.len(), 18, "{lines:#?}");
        assert_eq!(
            lines[0],
            cue(590_640, 592_130, "two fields well if those fields")
        );
        assert_eq!(
            lines[17],
            cue(638_670, 641_840, "terribly important and and it would have")
        );
        assert_eq!(words(&lines), said);

        // With them, a line is also cut where the lecturer stops to think, and a cue ends
        // with its last word.
        let cues = parse_subtitles(AUTO_VTT.as_bytes(), None).unwrap();
        assert_eq!(cues.len(), 20, "{cues:#?}");
        assert!(cues.iter().all(|c| !c.text.contains('\n')));
        assert!(
            cues.windows(2).all(|w| w[0].end <= w[1].start),
            "no overlaps"
        );
        assert_eq!(
            cues[2],
            cue(596_520, 599_210, "binary values or multi class values then")
        );
        assert_eq!(
            cues[10],
            cue(612_900, 616_020, "to semi structured data and")
        );
        assert_eq!(cues[11], cue(619_100, 620_460, "we'll see"));
        assert_eq!(cues[13], cue(625_850, 626_900, "okay so"));
        assert_eq!(words(&cues), said);
        assert!(!words(&cues).join(" ").contains('<'));
    }

    #[test]
    fn ordinary_subtitles_are_not_collapsed() {
        let cues = vec![
            cue(0, 1_000, "Hello"),
            cue(1_000, 2_000, "Hello"),
            cue(3_000, 4_000, "I said\nhello"),
            cue(5_000, 6_000, "hello\nthere"),
            cue(7_000, 8_000, "Bye"),
        ];
        assert_eq!(collapse_rolling(cues.clone()), cues);
    }

    #[test]
    fn empty_and_roundtrip() {
        assert_eq!(parse_subtitles(b"WEBVTT\n\n", None), Err(SubError::Empty));
        let cues = vec![
            cue(1000, 2500, "Hello\nworld"),
            cue(3_723_004, 3_724_000, "Late"),
        ];
        let srt = write_srt(&cues);
        assert!(srt.contains("01:02:03,004 --> 01:02:04,000"));
        assert_eq!(
            parse_subtitles(srt.as_bytes(), Some(SubFormat::Srt)).unwrap(),
            cues
        );
    }
}
