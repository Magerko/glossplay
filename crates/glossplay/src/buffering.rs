//! Waiting for data on a network stream: what mpv reports about its cache, when the player
//! shows that it waits, and the part of the file loaded ahead, for the seek bar.
//!
//! mpv tells about the cache through observed properties, so nothing here is read once a
//! frame, and nothing is observed at all while a local file plays.

use crate::i18n::{self, Lang, Tr};
use gp_media::mpv::{Format, Mpv, Value};
use serde_json::Value as Json;
use std::time::{Duration, Instant};

/// A wait shorter than this is not shown, so the overlay does not flash on every small stall.
pub const SHOW_AFTER: Duration = Duration::from_millis(300);

const PAUSED_FOR_CACHE: u64 = 101;
const BUFFERING_STATE: u64 = 102;
const SEEKING: u64 = 103;
const CORE_IDLE: u64 = 104;
const CACHE_STATE: u64 = 105;
const OBSERVED: [(u64, &str, Format); 5] = [
    (PAUSED_FOR_CACHE, "paused-for-cache", Format::Flag),
    (BUFFERING_STATE, "cache-buffering-state", Format::Int),
    (SEEKING, "seeking", Format::Flag),
    (CORE_IDLE, "core-idle", Format::Flag),
    (CACHE_STATE, "demuxer-cache-state", Format::Text),
];

/// Whether a media address is read over the network: a link of any kind but a local file.
pub fn is_network(media: &str) -> bool {
    let lower = media.trim().to_ascii_lowercase();
    lower.contains("://") && !lower.starts_with("file:")
}

/// What `demuxer-cache-state` says, in seconds of the media.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CacheState {
    /// The stretches the demuxer holds and can seek in without asking the server again.
    pub ranges: Vec<(f64, f64)>,
    /// Seconds buffered ahead of the playhead.
    pub ahead: Option<f64>,
    /// Bytes a second coming from the network.
    pub rate: Option<f64>,
    /// Bytes buffered ahead.
    pub bytes_ahead: Option<f64>,
}

impl CacheState {
    pub fn parse(json: &str) -> CacheState {
        let Ok(v) = serde_json::from_str::<Json>(json) else {
            return CacheState::default();
        };
        let ranges = v["seekable-ranges"]
            .as_array()
            .map(|list| {
                list.iter()
                    .filter_map(|r| Some((r["start"].as_f64()?, r["end"].as_f64()?)))
                    .filter(|(a, b)| b > a)
                    .collect()
            })
            .unwrap_or_default();
        CacheState {
            ranges,
            ahead: v["cache-duration"].as_f64(),
            rate: v["raw-input-rate"].as_f64(),
            bytes_ahead: v["fw-bytes"].as_f64(),
        }
    }
}

/// The cache of the stream playing now, as mpv last reported it.
#[derive(Debug, Default)]
pub struct Cache {
    observing: bool,
    pub paused_for_cache: bool,
    /// 0 to 100 while mpv fills the cache before going on.
    pub percent: Option<i64>,
    pub seeking: bool,
    /// The playback core stands still, for whatever reason, a user's pause included.
    pub core_idle: bool,
    pub state: CacheState,
    waiting_since: Option<Instant>,
}

impl Cache {
    /// Starts or stops listening to mpv as the media changes between a stream and a file.
    pub fn follow(&mut self, mpv: &Mpv, network: bool) {
        if self.observing {
            for (id, ..) in OBSERVED {
                mpv.unobserve(id);
            }
        }
        *self = Cache::default();
        if network {
            for (id, name, format) in OBSERVED {
                if let Err(e) = mpv.observe(id, name, format) {
                    eprintln!("{e}");
                }
            }
            self.observing = true;
        }
    }

    pub fn observing(&self) -> bool {
        self.observing
    }

    /// Takes a property event; false when it is not one of mine.
    pub fn on_property(&mut self, id: u64, value: &Value) -> bool {
        match id {
            PAUSED_FOR_CACHE => self.paused_for_cache = value.flag().unwrap_or(false),
            BUFFERING_STATE => self.percent = value.number().map(|n| n.round() as i64),
            SEEKING => self.seeking = value.flag().unwrap_or(false),
            CORE_IDLE => self.core_idle = value.flag().unwrap_or(false),
            CACHE_STATE => {
                self.state = value.text().map(CacheState::parse).unwrap_or_default();
            }
            _ => return false,
        }
        true
    }

    /// The player waits for data rather than playing: it said so, it is seeking, or it stands
    /// still while nobody paused it.
    pub fn waiting(&self, paused: bool) -> bool {
        self.observing && (self.paused_for_cache || self.seeking || (self.core_idle && !paused))
    }

    /// Whether the overlay is up at `now`; call it once a frame.
    pub fn shown(&mut self, waiting: bool, now: Instant) -> bool {
        if !self.observing || !waiting {
            self.waiting_since = None;
            return false;
        }
        let since = *self.waiting_since.get_or_insert(now);
        now.duration_since(since) >= SHOW_AFTER
    }
}

/// "1.8 MB/s" in the interface language.
pub fn speed_in(lang: Lang, bytes_per_second: f64) -> String {
    const KB: f64 = 1024.0;
    let (template, value) = if bytes_per_second >= KB * KB {
        (
            Tr::SpeedMb,
            decimal_in(lang, bytes_per_second / (KB * KB), 1),
        )
    } else {
        (Tr::SpeedKb, format!("{:.0}", bytes_per_second / KB))
    };
    i18n::fill(i18n::text_in(lang, template), &[&value])
}

pub fn decimal_in(lang: Lang, value: f64, places: usize) -> String {
    let s = format!("{value:.places$}");
    match lang {
        Lang::Ru | Lang::Uk => s.replace('.', ","),
        Lang::En | Lang::Zh => s,
    }
}

/// "Buffering 42 % · 1.8 MB/s": the percent only while mpv fills the cache before going on,
/// the speed only once some data has come.
pub fn label_in(lang: Lang, cache: &Cache) -> String {
    let mut text = i18n::text_in(lang, Tr::Buffering).to_owned();
    if let Some(p) = cache.percent.filter(|_| cache.paused_for_cache) {
        text.push(' ');
        text.push_str(&i18n::fill(
            i18n::text_in(lang, Tr::PercentValue),
            &[&p.clamp(0, 100)],
        ));
    }
    if let Some(rate) = cache.state.rate.filter(|r| *r > 0.0) {
        text.push_str(" · ");
        text.push_str(&speed_in(lang, rate));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn watching() -> Cache {
        Cache {
            observing: true,
            ..Default::default()
        }
    }

    #[test]
    fn only_links_count_as_network() {
        assert!(is_network("https://www.youtube.com/watch?v=x"));
        assert!(is_network(
            "http://127.0.0.1:8090/stream/a.mkv?link=x&index=1&play"
        ));
        assert!(!is_network(r"C:\Videos\a.mkv"));
        assert!(!is_network("/home/me/a.mkv"));
        assert!(!is_network("file:///home/me/a.mkv"));
    }

    #[test]
    fn a_wait_shows_only_after_the_delay() {
        let mut c = watching();
        let t0 = Instant::now();
        assert!(!c.shown(true, t0));
        assert!(!c.shown(true, t0 + Duration::from_millis(299)));
        assert!(c.shown(true, t0 + SHOW_AFTER));
        // A short stall that ends resets the clock.
        assert!(!c.shown(false, t0 + Duration::from_millis(400)));
        assert!(!c.shown(true, t0 + Duration::from_millis(500)));
        assert!(!c.shown(true, t0 + Duration::from_millis(700)));
        assert!(c.shown(true, t0 + Duration::from_millis(800)));
    }

    #[test]
    fn a_local_file_never_shows_it() {
        let mut c = Cache {
            paused_for_cache: true,
            seeking: true,
            core_idle: true,
            ..Default::default()
        };
        assert!(!c.waiting(false), "nothing is observed for a local file");
        let t0 = Instant::now();
        assert!(!c.shown(true, t0));
        assert!(!c.shown(true, t0 + Duration::from_secs(5)));
    }

    #[test]
    fn what_counts_as_waiting() {
        let mut c = watching();
        assert!(!c.waiting(false), "playing");
        c.core_idle = true;
        assert!(!c.waiting(true), "the user paused");
        assert!(c.waiting(false), "standing still while nobody paused");
        c.core_idle = false;
        c.seeking = true;
        assert!(c.waiting(true), "a seek waits even when paused");
        c.seeking = false;
        assert!(c.on_property(PAUSED_FOR_CACHE, &Value::Flag(true)));
        assert!(c.waiting(false));
        assert!(!c.on_property(7, &Value::Flag(true)), "not one of mine");
    }

    #[test]
    fn the_cache_state_gives_ranges_and_speed() {
        let json = r#"{"seekable-ranges":[{"start":0.0,"end":42.5},{"start":60,"end":60}],
            "cache-duration":30.2,"raw-input-rate":1887436,"fw-bytes":5000000,"eof":false}"#;
        let s = CacheState::parse(json);
        assert_eq!(s.ranges, vec![(0.0, 42.5)], "an empty range is dropped");
        assert_eq!(s.ahead, Some(30.2));
        assert_eq!(s.rate, Some(1887436.0));
        assert_eq!(CacheState::parse("not json"), CacheState::default());
        let mut c = watching();
        c.on_property(CACHE_STATE, &Value::Text(json.into()));
        assert_eq!(c.state.ahead, Some(30.2));
        c.on_property(CACHE_STATE, &Value::None);
        assert_eq!(c.state, CacheState::default());
    }

    #[test]
    fn the_label_names_percent_and_speed() {
        let mut c = watching();
        c.paused_for_cache = true;
        c.percent = Some(42);
        c.state.rate = Some(1.8 * 1024.0 * 1024.0);
        assert_eq!(label_in(Lang::Ru, &c), "Буферизация 42 % · 1,8 МБ/с");
        assert_eq!(label_in(Lang::En, &c), "Buffering 42% · 1.8 MB/s");
        c.paused_for_cache = false;
        c.state.rate = Some(300.0 * 1024.0);
        assert_eq!(label_in(Lang::En, &c), "Buffering · 300 KB/s");
        c.state.rate = None;
        assert_eq!(label_in(Lang::En, &c), "Buffering");
    }
}
