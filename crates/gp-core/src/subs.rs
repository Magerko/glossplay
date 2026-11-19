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
        SubFormat::WebVtt => parse_srt_like(&text),
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
    if cues.is_empty() {
        return Err(SubError::Empty);
    }
    Ok(cues)
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
