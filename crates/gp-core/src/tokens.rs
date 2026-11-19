use std::ops::Range;
use std::sync::OnceLock;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Word,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub range: Range<usize>,
    pub kind: TokenKind,
}

#[derive(Clone, Copy, PartialEq)]
enum Script {
    Japanese,
    Chinese,
    Spaced,
}

/// The tokens cover the whole string with no gaps.
pub fn tokenize(text: &str, lang: Option<&str>) -> Vec<Token> {
    let script = match lang.map(primary_subtag) {
        Some("ja") => Script::Japanese,
        Some("zh") => Script::Chinese,
        Some(_) => Script::Spaced,
        None => guess_script(text),
    };
    let pieces: Vec<&str> = match script {
        Script::Spaced => text.split_word_bounds().collect(),
        Script::Chinese => jieba()
            .cut(text, true)
            .into_iter()
            .map(|t| t.word)
            .collect(),
        Script::Japanese => return align(text, &japanese(text)),
    };
    align(text, &pieces)
}

/// Loading the dictionaries takes a few hundred milliseconds, so I do it on a background
/// thread at startup rather than on the first subtitle.
pub fn warm_up() {
    jieba();
    lindera();
}

fn lindera() -> Option<&'static lindera::segmenter::Segmenter> {
    static SEGMENTER: OnceLock<Option<lindera::segmenter::Segmenter>> = OnceLock::new();
    SEGMENTER
        .get_or_init(|| {
            let dict = lindera::dictionary::load_dictionary("embedded://ipadic").ok()?;
            Some(lindera::segmenter::Segmenter::new(
                lindera::mode::Mode::Normal,
                dict,
                None,
            ))
        })
        .as_ref()
}

fn japanese(text: &str) -> Vec<String> {
    let segmented = lindera().and_then(|s| s.segment(std::borrow::Cow::Borrowed(text)).ok());
    match segmented {
        Some(tokens) => tokens.into_iter().map(|t| t.surface.into_owned()).collect(),
        None => text.split_word_bounds().map(str::to_owned).collect(),
    }
}

fn jieba() -> &'static jieba_rs::Jieba {
    static JIEBA: OnceLock<jieba_rs::Jieba> = OnceLock::new();
    JIEBA.get_or_init(jieba_rs::Jieba::new)
}

fn primary_subtag(lang: &str) -> &str {
    let tag = lang.split(['-', '_']).next().unwrap_or(lang);
    match tag {
        "jpn" => "ja",
        "zho" | "chi" | "cmn" | "yue" => "zh",
        other => other,
    }
}

fn guess_script(text: &str) -> Script {
    let mut han = false;
    for c in text.chars() {
        match c {
            '\u{3040}'..='\u{30FF}' => return Script::Japanese,
            '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}' => han = true,
            _ => {}
        }
    }
    if han { Script::Chinese } else { Script::Spaced }
}

// Segmenters return pieces in order but may drop whitespace. Searching from a cursor
// recovers the byte ranges and turns whatever was dropped into `Other` tokens.
fn align<S: AsRef<str>>(text: &str, pieces: &[S]) -> Vec<Token> {
    let mut tokens = Vec::with_capacity(pieces.len());
    let mut cursor = 0;
    for piece in pieces {
        let piece = piece.as_ref();
        if piece.is_empty() {
            continue;
        }
        let Some(offset) = text[cursor..].find(piece) else {
            continue;
        };
        let start = cursor + offset;
        if start > cursor {
            tokens.push(Token {
                range: cursor..start,
                kind: TokenKind::Other,
            });
        }
        let end = start + piece.len();
        tokens.push(Token {
            range: start..end,
            kind: kind_of(piece),
        });
        cursor = end;
    }
    if cursor < text.len() {
        tokens.push(Token {
            range: cursor..text.len(),
            kind: TokenKind::Other,
        });
    }
    tokens
}

fn kind_of(piece: &str) -> TokenKind {
    if piece.chars().any(char::is_alphanumeric) {
        TokenKind::Word
    } else {
        TokenKind::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words<'a>(text: &'a str, lang: Option<&str>) -> Vec<&'a str> {
        let tokens = tokenize(text, lang);
        assert_covering(text, &tokens);
        tokens
            .iter()
            .filter(|t| t.kind == TokenKind::Word)
            .map(|t| &text[t.range.clone()])
            .collect()
    }

    fn assert_covering(text: &str, tokens: &[Token]) {
        let mut pos = 0;
        for t in tokens {
            assert_eq!(t.range.start, pos, "gap or overlap in {text:?}");
            assert!(t.range.end > t.range.start);
            assert!(text.is_char_boundary(t.range.end));
            pos = t.range.end;
        }
        assert_eq!(pos, text.len());
    }

    #[test]
    fn english_keeps_apostrophes_and_drops_punctuation() {
        assert_eq!(
            words("Don't panic — it's 42, okay?\nYes.", Some("en")),
            vec!["Don't", "panic", "it's", "42", "okay", "Yes"]
        );
    }

    #[test]
    fn cyrillic_and_accents() {
        assert_eq!(
            words("Привет, мир! Café.", None),
            vec!["Привет", "мир", "Café"]
        );
    }

    #[test]
    fn chinese_is_segmented_into_words() {
        let w = words("我来到北京清华大学。", Some("zh"));
        assert!(w.contains(&"北京"), "{w:?}");
        assert!(w.len() >= 4 && w.len() < 9, "{w:?}");
        assert_eq!(
            words("我来到北京清华大学。", None),
            w,
            "script auto-detection"
        );
    }

    #[test]
    fn japanese_is_segmented_into_words() {
        let w = words("私の名前は中野です。", Some("ja"));
        assert!(w.contains(&"名前"), "{w:?}");
        assert!(w.len() >= 4, "{w:?}");
        assert_eq!(
            words("私の名前は中野です。", None),
            w,
            "kana triggers Japanese"
        );
        assert_eq!(
            words("私の名前は中野です。", Some("jpn")),
            w,
            "ISO 639-2 code"
        );
    }

    #[test]
    fn mixed_and_empty() {
        assert!(tokenize("", Some("en")).is_empty());
        assert_eq!(words("  ...  ", None), Vec::<&str>::new());
        let w = words("これは test です", Some("ja"));
        assert!(w.contains(&"test"), "{w:?}");
    }
}
