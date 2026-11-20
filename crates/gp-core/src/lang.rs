//! The languages the pickers offer, and the code each service wants for one of them.
//!
//! The config keeps a plain ISO 639-1 code ("zh", "pt"). Services disagree about the rest:
//! Google wants "zh-CN", DeepL a written variant as a target, YouTube lists "zh-Hans". All
//! of that is decided here, in `code_for`, so no caller spells a service's code itself.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Language {
    /// What the config stores.
    pub code: &'static str,
    /// The language's name for itself.
    pub native: &'static str,
    pub english: &'static str,
    /// The script needs shaping (joined Arabic letters, Devanagari conjuncts), which the
    /// interface's text renderer does not do; the native name would come out broken.
    pub shaped: bool,
}

const fn lang(code: &'static str, native: &'static str, english: &'static str) -> Language {
    Language {
        code,
        native,
        english,
        shaped: false,
    }
}

const fn shaped(code: &'static str, native: &'static str, english: &'static str) -> Language {
    Language {
        code,
        native,
        english,
        shaped: true,
    }
}

/// In the order the pickers show them: the most asked for first.
pub const LANGUAGES: [Language; 20] = [
    lang("en", "English", "English"),
    lang("ru", "Русский", "Russian"),
    lang("uk", "Українська", "Ukrainian"),
    lang("zh", "中文 (简体)", "Chinese (Simplified)"),
    lang("ja", "日本語", "Japanese"),
    lang("es", "Español", "Spanish"),
    lang("de", "Deutsch", "German"),
    lang("fr", "Français", "French"),
    lang("pt", "Português", "Portuguese"),
    lang("it", "Italiano", "Italian"),
    lang("ko", "한국어", "Korean"),
    lang("tr", "Türkçe", "Turkish"),
    lang("pl", "Polski", "Polish"),
    lang("nl", "Nederlands", "Dutch"),
    shaped("ar", "العربية", "Arabic"),
    shaped("hi", "हिन्दी", "Hindi"),
    lang("id", "Bahasa Indonesia", "Indonesian"),
    lang("vi", "Tiếng Việt", "Vietnamese"),
    lang("cs", "Čeština", "Czech"),
    lang("sv", "Svenska", "Swedish"),
];

/// The table entry for exactly this code. "zh-TW" is not "zh": a code with a region or
/// script the table does not have stays the user's own and is passed on as it is.
pub fn find(code: &str) -> Option<&'static Language> {
    let code = code.trim();
    LANGUAGES.iter().find(|l| l.code.eq_ignore_ascii_case(code))
}

/// The bare language of a tag: "en-US", "eng" and "EN" are all "en".
pub fn normalize(tag: &str) -> String {
    let tag = tag.trim().to_ascii_lowercase();
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
        "ces" | "cze" => "cs",
        "vie" => "vi",
        "ind" => "id",
        other => other,
    };
    short.to_owned()
}

/// Who a language code is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    /// Google Translate, both directions.
    Google,
    DeepLSource,
    DeepLTarget,
    /// A language model told the code in its prompt; it reads BCP-47.
    Llm,
    /// whisper.cpp: ISO 639-1 only, no region.
    Whisper,
    /// The voice-over server (OmniVoice): ISO 639-1 only; "ru-RU" is an error there.
    Speech,
    /// The caption key YouTube lists for a language, the first one to look for.
    YouTube,
}

/// The code `service` wants for the configured `code`. An empty code stays empty: it means
/// "detect" wherever a service accepts that.
pub fn code_for(service: Service, code: &str) -> String {
    let code = code.trim();
    if code.is_empty() {
        return String::new();
    }
    let lower = code.to_ascii_lowercase();
    let base = normalize(code);
    let traditional = matches!(lower.as_str(), "zh-tw" | "zh-hk" | "zh-hant" | "zh-mo");
    match service {
        Service::Google => match lower.as_str() {
            "zh" | "zh-hans" | "zh-sg" => "zh-CN".to_owned(),
            _ if traditional => "zh-TW".to_owned(),
            _ => code.to_owned(),
        },
        Service::DeepLSource => base.to_ascii_uppercase(),
        // A bare EN, PT or ZH as a target still works but is kept only for old clients.
        Service::DeepLTarget => match lower.as_str() {
            "zh" | "zh-hans" | "zh-cn" | "zh-sg" => "ZH-HANS".to_owned(),
            _ if traditional => "ZH-HANT".to_owned(),
            "en" => "EN-US".to_owned(),
            "pt" => "PT-BR".to_owned(),
            _ => code.to_ascii_uppercase(),
        },
        Service::Llm => match lower.as_str() {
            "zh" => "zh-Hans".to_owned(),
            _ => code.to_owned(),
        },
        Service::Whisper | Service::Speech => base,
        Service::YouTube => match lower.as_str() {
            "zh" | "zh-cn" => "zh-Hans".to_owned(),
            _ if traditional => "zh-Hant".to_owned(),
            _ => code.to_owned(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_holds_plain_unique_codes() {
        for (i, l) in LANGUAGES.iter().enumerate() {
            assert_eq!(l.code.len(), 2, "{}", l.code);
            assert!(l.code.bytes().all(|b| b.is_ascii_lowercase()), "{}", l.code);
            assert_eq!(normalize(l.code), l.code);
            assert_eq!(find(l.code), Some(l));
            assert!(!l.native.is_empty() && !l.english.is_empty());
            assert!(
                LANGUAGES[..i].iter().all(|o| o.code != l.code),
                "{} twice",
                l.code
            );
        }
        for must in ["ru", "uk", "en", "zh", "ja"] {
            assert!(find(must).is_some(), "{must} is missing");
        }
    }

    #[test]
    fn a_code_outside_the_table_is_not_taken_for_another() {
        assert_eq!(find(" EN ").map(|l| l.code), Some("en"));
        assert!(find("zh-TW").is_none());
        assert!(find("eo").is_none());
        assert!(find("").is_none());
    }

    #[test]
    fn tags_are_reduced_to_the_language() {
        assert_eq!(normalize("eng"), "en");
        assert_eq!(normalize("en-US"), "en");
        assert_eq!(normalize("JPN"), "ja");
        assert_eq!(normalize("zh_Hans"), "zh");
        assert_eq!(normalize("cze"), "cs");
        assert_eq!(normalize("uk"), "uk");
    }

    #[test]
    fn chinese_gets_the_code_each_service_knows() {
        let zh = |s| code_for(s, "zh");
        assert_eq!(zh(Service::Google), "zh-CN");
        assert_eq!(zh(Service::DeepLSource), "ZH");
        assert_eq!(zh(Service::DeepLTarget), "ZH-HANS");
        assert_eq!(zh(Service::Llm), "zh-Hans");
        assert_eq!(zh(Service::Whisper), "zh");
        assert_eq!(zh(Service::Speech), "zh");
        assert_eq!(zh(Service::YouTube), "zh-Hans");
    }

    #[test]
    fn traditional_chinese_stays_traditional() {
        assert_eq!(code_for(Service::Google, "zh-TW"), "zh-TW");
        assert_eq!(code_for(Service::DeepLTarget, "zh-Hant"), "ZH-HANT");
        assert_eq!(code_for(Service::YouTube, "zh-TW"), "zh-Hant");
        assert_eq!(code_for(Service::Whisper, "zh-TW"), "zh");
    }

    #[test]
    fn deepl_targets_name_a_variant() {
        assert_eq!(code_for(Service::DeepLTarget, "en"), "EN-US");
        assert_eq!(code_for(Service::DeepLTarget, "pt"), "PT-BR");
        assert_eq!(code_for(Service::DeepLTarget, "ru"), "RU");
        assert_eq!(code_for(Service::DeepLTarget, "en-GB"), "EN-GB");
        assert_eq!(code_for(Service::DeepLSource, "pt-BR"), "PT");
    }

    #[test]
    fn the_rest_pass_through() {
        for service in [
            Service::Google,
            Service::Llm,
            Service::Whisper,
            Service::Speech,
            Service::YouTube,
        ] {
            for l in LANGUAGES.iter().filter(|l| l.code != "zh") {
                assert_eq!(code_for(service, l.code), l.code, "{service:?}");
            }
            assert_eq!(code_for(service, ""), "");
        }
        assert_eq!(code_for(Service::Google, "eo"), "eo");
        assert_eq!(code_for(Service::Speech, "ru-RU"), "ru");
        assert_eq!(code_for(Service::Whisper, " uk "), "uk");
    }
}
