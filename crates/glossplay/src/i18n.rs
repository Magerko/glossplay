//! Interface languages. The texts are compiled in (see `messages.rs`); English is the
//! default and the fallback, so a missing translation shows the English line.

mod messages;

pub use messages::Tr;
use std::fmt::Display;
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    En,
    Ru,
    Uk,
}

impl Lang {
    fn index(self) -> usize {
        self as usize
    }

    fn from_index(i: u8) -> Lang {
        match i {
            1 => Lang::Ru,
            2 => Lang::Uk,
            _ => Lang::En,
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Ru => "ru",
            Lang::Uk => "uk",
        }
    }

    pub fn parse(code: &str) -> Option<Lang> {
        match code.trim().to_ascii_lowercase().as_str() {
            "en" => Some(Lang::En),
            "ru" => Some(Lang::Ru),
            "uk" => Some(Lang::Uk),
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
        Lang::En => number,
        Lang::Ru | Lang::Uk => number.replace('.', ","),
    }
}

/// Windows language id (LANGID) to an interface language; only the primary part counts.
pub fn from_langid(id: u16) -> Lang {
    match id & 0x3ff {
        0x19 => Lang::Ru,
        0x22 => Lang::Uk,
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
    use super::*;

    const LANGS: [Lang; 3] = [Lang::En, Lang::Ru, Lang::Uk];

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
        assert_eq!(from_langid(0), Lang::En);
    }

    #[test]
    fn locale_names_are_understood() {
        assert_eq!(from_locale("ru_RU.UTF-8"), Lang::Ru);
        assert_eq!(from_locale("uk_UA"), Lang::Uk);
        assert_eq!(from_locale("uk-UA"), Lang::Uk);
        assert_eq!(from_locale("ru"), Lang::Ru);
        assert_eq!(from_locale("de_DE.UTF-8"), Lang::En);
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
        assert_eq!(
            resolve("de", Lang::Ru),
            Lang::En,
            "an unknown code means English"
        );
    }
}
