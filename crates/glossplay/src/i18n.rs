//! Interface languages. The texts are compiled in (see `messages.rs` and `messages_zh.rs`);
//! English is the default and the fallback, so a missing translation shows the English line.

mod messages;
mod messages_zh;

pub use messages::Tr;
use std::fmt::Display;
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    En,
    Ru,
    Uk,
    /// Simplified Chinese.
    Zh,
}

impl Lang {
    fn index(self) -> usize {
        self as usize
    }

    fn from_index(i: u8) -> Lang {
        match i {
            1 => Lang::Ru,
            2 => Lang::Uk,
            3 => Lang::Zh,
            _ => Lang::En,
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Ru => "ru",
            Lang::Uk => "uk",
            Lang::Zh => "zh",
        }
    }

    /// "zh-CN", "zh_Hans" and other Chinese tags all mean the one Chinese interface there is.
    pub fn parse(code: &str) -> Option<Lang> {
        let code = code.trim().to_ascii_lowercase();
        match code.as_str() {
            "en" => Some(Lang::En),
            "ru" => Some(Lang::Ru),
            "uk" => Some(Lang::Uk),
            "zh" => Some(Lang::Zh),
            _ if code.starts_with("zh-") || code.starts_with("zh_") => Some(Lang::Zh),
            _ => None,
        }
    }
}

static CURRENT: AtomicU8 = AtomicU8::new(0);

pub fn lang() -> Lang {
    Lang::from_index(CURRENT.load(Ordering::Relaxed))
}

pub fn set_lang(lang: Lang) {
    CURRENT.store(lang as u8, Ordering::Relaxed);
}

/// The text of `key` in `lang`; an empty translation falls back to English.
pub fn text_in(lang: Lang, key: Tr) -> &'static str {
    let texts = key.texts();
    let text = texts[lang.index()];
    if text.is_empty() { texts[0] } else { text }
}

/// A message without values.
pub fn t(key: Tr) -> &'static str {
    text_in(lang(), key)
}

/// A message that names a key chord: on a Mac the main modifier is Command, not Ctrl. I write
/// "Cmd" rather than the ⌘ sign, which the fonts bundled with egui do not have.
pub fn tk(key: Tr) -> std::borrow::Cow<'static, str> {
    main_modifier(t(key), cfg!(target_os = "macos"))
}

fn main_modifier(text: &'static str, mac: bool) -> std::borrow::Cow<'static, str> {
    if mac && text.contains("Ctrl+") {
        text.replace("Ctrl+", "Cmd+").into()
    } else {
        text.into()
    }
}

/// The name of a language by its two-letter code, for the languages of the pickers.
pub fn language_name(code: &str) -> Option<Tr> {
    Some(match code {
        "en" => Tr::LangEnglish,
        "ru" => Tr::LangRussian,
        "uk" => Tr::LangUkrainian,
        "de" => Tr::LangGerman,
        "fr" => Tr::LangFrench,
        "es" => Tr::LangSpanish,
        "it" => Tr::LangItalian,
        "pt" => Tr::LangPortuguese,
        "pl" => Tr::LangPolish,
        "tr" => Tr::LangTurkish,
        "ja" => Tr::LangJapanese,
        "ko" => Tr::LangKorean,
        "zh" => Tr::LangChinese,
        "nl" => Tr::LangDutch,
        "ar" => Tr::LangArabic,
        "hi" => Tr::LangHindi,
        "id" => Tr::LangIndonesian,
        "vi" => Tr::LangVietnamese,
        "cs" => Tr::LangCzech,
        "sv" => Tr::LangSwedish,
        _ => return None,
    })
}

/// A language tag as text for people: "eng" and "en" are "English" in the interface
/// language, "pt-BR" is "Portuguese (pt-BR)", and a tag I have no name for stays as it is.
pub fn language_label(code: &str) -> String {
    language_label_in(lang(), code)
}

fn language_label_in(lang: Lang, code: &str) -> String {
    let code = code.trim();
    let exact = gp_core::lang::find(code).is_some() || code.len() == 3;
    match language_name(&gp_core::lang::normalize(code)) {
        Some(name) if exact => text_in(lang, name).to_owned(),
        Some(name) => format!("{} ({code})", text_in(lang, name)),
        None => code.to_owned(),
    }
}

/// A message whose `{0}`, `{1}`… are replaced by `args`.
pub fn tf(key: Tr, args: &[&dyn Display]) -> String {
    fill(t(key), args)
}

pub fn fill(template: &str, args: &[&dyn Display]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let tail = &rest[open + 1..];
        let slot = tail
            .find('}')
            .and_then(|close| Some((tail[..close].parse::<usize>().ok()?, close)))
            .and_then(|(i, close)| Some((args.get(i)?, close)));
        match slot {
            Some((arg, close)) => {
                out.push_str(&arg.to_string());
                rest = &tail[close + 1..];
            }
            None => {
                out.push('{');
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    out
}

/// A number written with a point, in the decimal separator of the interface language.
pub fn decimal(number: String) -> String {
    match lang() {
        Lang::En | Lang::Zh => number,
        Lang::Ru | Lang::Uk => number.replace('.', ","),
    }
}

/// Windows language id (LANGID) to an interface language; only the primary part counts.
pub fn from_langid(id: u16) -> Lang {
    match id & 0x3ff {
        0x19 => Lang::Ru,
        0x22 => Lang::Uk,
        0x04 => Lang::Zh,
        _ => Lang::En,
    }
}

/// "ru_RU.UTF-8", "uk-UA", "ru" and the like.
#[cfg_attr(windows, allow(dead_code))]
pub fn from_locale(locale: &str) -> Lang {
    let name = locale.split(['.', '@']).next().unwrap_or("");
    let primary = name.split(['_', '-']).next().unwrap_or("");
    Lang::parse(primary).unwrap_or(Lang::En)
}

/// The first of LC_ALL, LC_MESSAGES and LANG that is set decides, as in POSIX.
#[cfg_attr(windows, allow(dead_code))]
pub fn from_env(get: impl Fn(&str) -> Option<String>) -> Lang {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|name| get(name).filter(|v| !v.is_empty()))
        .map_or(Lang::En, |v| from_locale(&v))
}

#[cfg(windows)]
pub fn system_langid() -> u16 {
    // SAFETY: a plain query with no arguments.
    unsafe { windows_sys::Win32::Globalization::GetUserDefaultUILanguage() }
}

pub fn system_lang() -> Lang {
    #[cfg(windows)]
    {
        from_langid(system_langid())
    }
    #[cfg(not(windows))]
    {
        from_env(|name| std::env::var(name).ok())
    }
}

/// What the `ui_lang` setting means: "auto" and an empty value follow the system, a known
/// code picks that language, anything else is English.
pub fn resolve(setting: &str, system: Lang) -> Lang {
    let setting = setting.trim();
    if setting.is_empty() || setting.eq_ignore_ascii_case("auto") {
        system
    } else {
        Lang::parse(setting).unwrap_or(Lang::En)
    }
}

pub fn apply_setting(setting: &str) {
    set_lang(resolve(setting, system_lang()));
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_main_modifier_follows_the_platform() {
        let hint = "press Ctrl+V or Ctrl+F";
        assert_eq!(super::main_modifier(hint, false), hint);
        assert_eq!(super::main_modifier(hint, true), "press Cmd+V or Cmd+F");
        assert_eq!(super::main_modifier("Space pause", true), "Space pause");
    }

    use super::*;

    const LANGS: [Lang; 4] = [Lang::En, Lang::Ru, Lang::Uk, Lang::Zh];

    fn placeholders(text: &str) -> Vec<usize> {
        let mut found = Vec::new();
        let mut rest = text;
        while let Some(open) = rest.find('{') {
            let tail = &rest[open + 1..];
            let close = tail.find('}').expect("an unclosed brace");
            found.push(tail[..close].parse().expect("a placeholder is {N}"));
            rest = &tail[close + 1..];
        }
        assert!(!rest.contains('}'), "a stray closing brace in {text:?}");
        found.sort_unstable();
        found
    }

    #[test]
    fn every_key_is_translated_everywhere() {
        for &key in Tr::ALL {
            for lang in LANGS {
                assert!(
                    !key.texts()[lang.index()].trim().is_empty(),
                    "{key:?} has no {lang:?} text"
                );
            }
        }
    }

    #[test]
    fn translations_keep_the_placeholders_of_the_english_text() {
        for &key in Tr::ALL {
            let want = placeholders(key.texts()[0]);
            for lang in LANGS {
                assert_eq!(
                    placeholders(key.texts()[lang.index()]),
                    want,
                    "{key:?} in {lang:?}"
                );
            }
        }
    }

    #[test]
    fn texts_have_no_stray_spaces() {
        for &key in Tr::ALL {
            for lang in LANGS {
                let text = key.texts()[lang.index()];
                assert_eq!(text, text.trim(), "{key:?} in {lang:?} has edge spaces");
                assert!(
                    !text.contains("  "),
                    "{key:?} in {lang:?} has a double space"
                );
                assert!(
                    !text.contains(" \n"),
                    "{key:?} in {lang:?}: a space before a break"
                );
            }
        }
    }

    #[test]
    fn the_chinese_column_is_chinese_or_left_as_in_english() {
        let han = |c: char| {
            ('\u{3000}'..='\u{9fff}').contains(&c) || ('\u{ff00}'..='\u{ffef}').contains(&c)
        };
        for &key in Tr::ALL {
            let [en, .., zh] = key.texts();
            assert!(zh == en || zh.chars().any(han), "{key:?}: {zh:?}");
        }
    }

    #[test]
    fn the_boson_notice_stays_in_english() {
        for lang in LANGS {
            let text = text_in(lang, Tr::TtsLicenceBoson);
            assert!(
                text.starts_with("Built with Higgs Materials licensed from Boson AI USA, Inc."),
                "{lang:?}"
            );
        }
    }

    #[test]
    fn every_language_of_the_pickers_has_a_name() {
        for l in &gp_core::lang::LANGUAGES {
            let name = language_name(l.code).unwrap_or_else(|| panic!("{} has no name", l.code));
            assert_eq!(
                text_in(Lang::En, name),
                l.english.split(" (").next().unwrap()
            );
        }
    }

    #[test]
    fn tags_read_as_language_names() {
        let en = |code| language_label_in(Lang::En, code);
        assert_eq!(en("eng"), "English");
        assert_eq!(en("zh"), "Chinese");
        assert_eq!(en("pt-BR"), "Portuguese (pt-BR)");
        assert_eq!(en("eo"), "eo");
        assert_eq!(language_label_in(Lang::Ru, "JPN"), "Японский");
    }

    #[test]
    fn values_fill_their_slots_in_any_order() {
        assert_eq!(fill("{0} of {1}", &[&1, &"two"]), "1 of two");
        assert_eq!(fill("{1}/{0}", &[&"a", &"b"]), "b/a");
        assert_eq!(fill("no slots", &[]), "no slots");
        assert_eq!(fill("{0}", &[&"{1}"]), "{1}");
        assert_eq!(fill("{5} stays", &[&1]), "{5} stays");
    }

    #[test]
    fn windows_language_ids_pick_the_primary_language() {
        assert_eq!(from_langid(0x0419), Lang::Ru);
        assert_eq!(from_langid(0x0422), Lang::Uk);
        assert_eq!(from_langid(0x0409), Lang::En);
        assert_eq!(from_langid(0x0407), Lang::En);
        assert_eq!(from_langid(0x0804), Lang::Zh);
        assert_eq!(from_langid(0x0404), Lang::Zh);
        assert_eq!(from_langid(0), Lang::En);
    }

    #[test]
    fn locale_names_are_understood() {
        assert_eq!(from_locale("ru_RU.UTF-8"), Lang::Ru);
        assert_eq!(from_locale("uk_UA"), Lang::Uk);
        assert_eq!(from_locale("uk-UA"), Lang::Uk);
        assert_eq!(from_locale("ru"), Lang::Ru);
        assert_eq!(from_locale("de_DE.UTF-8"), Lang::En);
        assert_eq!(from_locale("zh_CN.UTF-8"), Lang::Zh);
        assert_eq!(from_locale("zh-Hans"), Lang::Zh);
        assert_eq!(from_locale("C"), Lang::En);
        assert_eq!(from_locale(""), Lang::En);
    }

    #[test]
    fn the_environment_is_read_in_posix_order() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| (*v).to_owned())
            }
        };
        assert_eq!(from_env(env(&[("LANG", "ru_RU.UTF-8")])), Lang::Ru);
        assert_eq!(
            from_env(env(&[("LC_ALL", "uk_UA.UTF-8"), ("LANG", "ru_RU.UTF-8")])),
            Lang::Uk
        );
        assert_eq!(
            from_env(env(&[
                ("LC_ALL", ""),
                ("LC_MESSAGES", "ru_RU"),
                ("LANG", "en_US")
            ])),
            Lang::Ru
        );
        assert_eq!(from_env(env(&[])), Lang::En);
    }

    #[test]
    fn the_setting_decides_the_language() {
        assert_eq!(resolve("auto", Lang::Ru), Lang::Ru);
        assert_eq!(resolve("", Lang::Uk), Lang::Uk);
        assert_eq!(resolve("en", Lang::Ru), Lang::En);
        assert_eq!(resolve("ru", Lang::En), Lang::Ru);
        assert_eq!(resolve("uk", Lang::En), Lang::Uk);
        assert_eq!(resolve("UK", Lang::En), Lang::Uk);
        assert_eq!(resolve("zh", Lang::En), Lang::Zh);
        assert_eq!(resolve("zh-CN", Lang::Ru), Lang::Zh);
        assert_eq!(resolve("auto", Lang::Zh), Lang::Zh);
        assert_eq!(
            resolve("de", Lang::Ru),
            Lang::En,
            "an unknown code means English"
        );
    }
}
