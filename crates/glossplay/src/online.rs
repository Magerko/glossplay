//! What yt-dlp told mpv about an online video. mpv's ytdl_hook keeps yt-dlp's whole answer in
//! `user-data/mpv/ytdl/json-subprocess-result` until the file ends, so I read the automatic
//! captions and the video's language from there instead of asking
//! yt-dlp a second time.

use crate::session::normalize_lang;
use serde::Deserialize;
use std::collections::BTreeMap;

pub const HOOK_RESULT: &str = "user-data/mpv/ytdl/json-subprocess-result";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Auto {
    /// Written by people, or at least not marked as automatic.
    No,
    /// The site's speech recognition, in the language spoken.
    Speech,
    /// The site's machine translation of its speech recognition.
    Translated,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AutoSub {
    pub lang: String,
    /// The site's own name for it, "English" or "Russian".
    pub name: String,
    pub url: String,
    pub kind: Auto,
}

#[derive(Debug, Clone, Default)]
pub struct OnlineInfo {
    pub id: String,
    pub extractor: String,
    /// The language spoken, as the site says.
    pub language: Option<String>,
    captions: BTreeMap<String, Vec<RawSub>>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawSub {
    ext: Option<String>,
    url: Option<String>,
    name: Option<String>,
    protocol: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawVideo {
    id: Option<String>,
    extractor_key: Option<String>,
    extractor: Option<String>,
    language: Option<String>,
    #[serde(default)]
    automatic_captions: Option<BTreeMap<String, Vec<RawSub>>>,
}

#[derive(Deserialize)]
struct HookResult {
    stdout: Option<String>,
}

impl OnlineInfo {
    /// `hook` is the property as mpv prints it: JSON with yt-dlp's output in `stdout`.
    pub fn from_hook_result(hook: &str) -> Option<OnlineInfo> {
        let outer: HookResult = serde_json::from_str(hook).ok()?;
        Self::from_ytdlp_json(&outer.stdout?)
    }

    pub fn from_ytdlp_json(json: &str) -> Option<OnlineInfo> {
        let v: RawVideo = serde_json::from_str(json).ok()?;
        Some(OnlineInfo {
            id: v.id?,
            extractor: v.extractor_key.or(v.extractor).unwrap_or_default(),
            language: v.language.filter(|l| !l.is_empty()),
            captions: v.automatic_captions.unwrap_or_default(),
        })
    }

    /// Survives the expiry of the stream addresses, which change on every opening.
    pub fn media_key(&self) -> String {
        gp_core::media_key::online_key(&self.extractor, &self.id)
    }

    /// The automatic captions worth offering: the language spoken, and the languages being
    /// learnt and spoken natively. A video can have a hundred and fifty of them, nearly all
    /// machine translations nobody here asked for.
    pub fn auto_subs(&self, learn: &str, native: &str) -> Vec<AutoSub> {
        let spoken = self.spoken_key();
        let spoken_lang = spoken.as_deref().map(|k| normalize_lang(base_of(k)));
        let mut out = Vec::new();
        if let Some(key) = &spoken
            && let Some(sub) = self.pick(key, Auto::Speech)
        {
            out.push(sub);
        }
        for want in [learn, native] {
            let want = normalize_lang(want);
            if want.is_empty()
                || spoken_lang.as_deref() == Some(want.as_str())
                || out.iter().any(|s| s.lang == want)
            {
                continue;
            }
            let mut keys: Vec<&String> = self
                .captions
                .keys()
                .filter(|k| !k.ends_with("-orig") && normalize_lang(k) == want)
                .collect();
            // "ru" before "ru-RU": the plain code is the one the site lists for everybody.
            keys.sort_by_key(|k| (k.len(), *k));
            if let Some(sub) = keys.first().and_then(|k| self.pick(k, Auto::Translated)) {
                out.push(sub);
            }
        }
        out
    }

    /// YouTube lists the recognised speech as `<lang>-orig`; other sites under the plain code.
    fn spoken_key(&self) -> Option<String> {
        let lang = self.language.as_deref().map(normalize_lang);
        let origs: Vec<&String> = self
            .captions
            .keys()
            .filter(|k| k.ends_with("-orig"))
            .collect();
        let matching = |k: &String| lang.as_deref() == Some(normalize_lang(base_of(k)).as_str());
        if let Some(k) = origs.iter().find(|k| matching(k)) {
            return Some((*k).clone());
        }
        // A dubbed video lists one per audio track; without the language I cannot tell.
        if origs.len() == 1 && lang.is_none() {
            return Some(origs[0].clone());
        }
        let lang = lang?;
        let mut plain: Vec<&String> = self
            .captions
            .keys()
            .filter(|k| !k.ends_with("-orig") && normalize_lang(k) == lang)
            .collect();
        plain.sort_by_key(|k| (k.len(), *k));
        plain.first().map(|k| (*k).clone())
    }

    fn pick(&self, key: &str, kind: Auto) -> Option<AutoSub> {
        let formats = self.captions.get(key)?;
        let usable = |ext: &str| {
            formats.iter().find(|f| {
                f.ext.as_deref() == Some(ext)
                    && f.protocol.as_deref().is_none_or(|p| p.starts_with("http"))
                    && f.url.as_deref().is_some_and(|u| u.starts_with("http"))
            })
        };
        let f = usable("vtt").or_else(|| usable("srt"))?;
        let url = f.url.clone()?;
        // The site's machine translation of its own captions carries the target language.
        let translated = kind == Auto::Translated || url.contains("&tlang=");
        let name = f.name.clone().unwrap_or_else(|| key.to_owned());
        Some(AutoSub {
            lang: normalize_lang(base_of(key)),
            name: name.trim_end_matches(" (Original)").to_owned(),
            url,
            kind: if translated {
                Auto::Translated
            } else {
                Auto::Speech
            },
        })
    }
}

fn base_of(key: &str) -> &str {
    key.strip_suffix("-orig").unwrap_or(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caption(ext: &str, url: &str, name: &str) -> serde_json::Value {
        serde_json::json!({"ext": ext, "url": url, "name": name})
    }

    fn formats(key: &str, name: &str, tlang: Option<&str>) -> serde_json::Value {
        let base = format!(
            "https://www.youtube.com/api/timedtext?v=X&kind=asr&lang=en{}",
            tlang.map_or(String::new(), |t| format!("&tlang={t}"))
        );
        let _ = key;
        serde_json::json!([
            caption("json3", &format!("{base}&fmt=json3"), name),
            caption("srt", &format!("{base}&fmt=srt"), name),
            caption("vtt", &format!("{base}&fmt=vtt"), name),
        ])
    }

    /// The shape of `yt-dlp -J` for a YouTube lecture in English with no captions by people.
    fn lecture() -> String {
        let mut auto = serde_json::Map::new();
        auto.insert(
            "en-orig".into(),
            formats("en-orig", "English (Original)", None),
        );
        auto.insert("en".into(), formats("en", "English", None));
        for (k, n) in [
            ("ru", "Russian"),
            ("uk", "Ukrainian"),
            ("de", "German"),
            ("pt-BR", "Portuguese (Brazil)"),
            ("pt", "Portuguese"),
        ] {
            auto.insert(k.into(), formats(k, n, Some(k)));
        }
        serde_json::json!({
            "id": "ByAn0MC0iWk",
            "extractor": "youtube",
            "extractor_key": "Youtube",
            "language": "en",
            "subtitles": {},
            "automatic_captions": auto,
            "requested_formats": [
                {"format_id": "247", "url": "https://rr1.googlevideo.com/videoplayback?itag=247", "vcodec": "vp9", "acodec": "none", "protocol": "https"},
                {"format_id": "251", "url": "https://rr1.googlevideo.com/videoplayback?itag=251", "vcodec": "none", "acodec": "opus", "protocol": "https",
                 "http_headers": {"User-Agent": "Mozilla/5.0"}}
            ]
        })
        .to_string()
    }

    #[test]
    fn the_hook_result_gives_id_and_language() {
        let hook = serde_json::json!({"status": 0, "stdout": lecture(), "stderr": ""}).to_string();
        let info = OnlineInfo::from_hook_result(&hook).unwrap();
        assert_eq!(info.media_key(), "o:youtube:ByAn0MC0iWk");
        assert_eq!(info.language.as_deref(), Some("en"));
        assert!(OnlineInfo::from_hook_result("{\"status\":1,\"stdout\":\"\"}").is_none());
        assert!(OnlineInfo::from_hook_result("not json").is_none());
    }

    #[test]
    fn only_the_spoken_learnt_and_native_languages_are_offered() {
        let info = OnlineInfo::from_ytdlp_json(&lecture()).unwrap();
        let subs = info.auto_subs("en", "ru");
        let got: Vec<(&str, &str, Auto)> = subs
            .iter()
            .map(|s| (s.lang.as_str(), s.name.as_str(), s.kind))
            .collect();
        assert_eq!(
            got,
            [
                ("en", "English", Auto::Speech),
                ("ru", "Russian", Auto::Translated)
            ]
        );
        assert!(subs[0].url.ends_with("lang=en&fmt=vtt"), "{}", subs[0].url);
        assert!(subs[1].url.contains("tlang=ru&fmt=vtt"));

        // Learning Portuguese: the plain code wins over the regional one.
        let pt = info.auto_subs("pt", "uk");
        assert_eq!(
            pt.iter().map(|s| s.lang.as_str()).collect::<Vec<_>>(),
            ["en", "pt", "uk"]
        );
        assert!(pt[1].url.contains("tlang=pt&"));
        assert!(
            info.auto_subs("ja", "").len() == 1,
            "no Japanese: only the spoken track"
        );
    }

    #[test]
    fn a_dubbed_video_takes_the_track_of_its_language() {
        let mut auto = serde_json::Map::new();
        for (k, n) in [
            ("de-orig", "German (Original)"),
            ("en-orig", "English (Original)"),
        ] {
            auto.insert(k.into(), formats(k, n, None));
        }
        auto.insert("ru".into(), formats("ru", "Russian", Some("ru")));
        let json = serde_json::json!({"id": "x", "extractor_key": "Youtube", "language": "en-US", "automatic_captions": auto}).to_string();
        let info = OnlineInfo::from_ytdlp_json(&json).unwrap();
        let subs = info.auto_subs("en", "ru");
        assert_eq!(subs.len(), 2);
        assert_eq!((subs[0].lang.as_str(), subs[0].kind), ("en", Auto::Speech));
        assert_eq!(subs[0].name, "English");
        assert_eq!(subs[1].kind, Auto::Translated);
    }
}
