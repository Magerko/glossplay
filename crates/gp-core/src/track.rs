/// Milliseconds from the start of the media.
pub type Ms = i64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cue {
    pub start: Ms,
    pub end: Ms,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackSource {
    Embedded {
        stream_index: u32,
    },
    External {
        path: String,
    },
    Asr {
        engine: String,
        model: String,
    },
    Translation {
        provider: String,
        of: Box<TrackSource>,
    },
}

#[derive(Debug, Clone)]
pub struct Track {
    pub source: TrackSource,
    pub lang: Option<String>,
    pub title: Option<String>,
    pub cues: Vec<Cue>,
}

// Cues are sorted by start, but a long cue can overlap several later ones. Scanning the
// whole track every frame is too slow for thousands of cues, and I have never seen
// overlaps deeper than this in real subtitles.
const OVERLAP_SCAN: usize = 32;

impl Track {
    pub fn new(
        source: TrackSource,
        lang: Option<String>,
        title: Option<String>,
        mut cues: Vec<Cue>,
    ) -> Self {
        cues.sort_by_key(|c| c.start);
        Track {
            source,
            lang,
            title,
            cues,
        }
    }

    /// Cues on screen at `t`, in start order.
    pub fn active_at(&self, t: Ms) -> Vec<usize> {
        let upper = self.cues.partition_point(|c| c.start <= t);
        let lower = upper.saturating_sub(OVERLAP_SCAN);
        (lower..upper).filter(|&i| self.cues[i].end > t).collect()
    }

    pub fn index_at_or_before(&self, t: Ms) -> Option<usize> {
        self.cues.partition_point(|c| c.start <= t).checked_sub(1)
    }

    pub fn index_after(&self, t: Ms) -> Option<usize> {
        let i = self.cues.partition_point(|c| c.start <= t);
        (i < self.cues.len()).then_some(i)
    }

    /// Cues whose end falls in (from, to].
    pub fn ended_between(&self, from: Ms, to: Ms) -> Vec<usize> {
        let upper = self.cues.partition_point(|c| c.start <= to);
        let lower = upper.saturating_sub(OVERLAP_SCAN);
        (lower..upper)
            .filter(|&i| self.cues[i].end > from && self.cues[i].end <= to)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(spans: &[(Ms, Ms)]) -> Track {
        let cues = spans
            .iter()
            .map(|&(s, e)| Cue {
                start: s,
                end: e,
                text: format!("{s}"),
            })
            .collect();
        Track::new(TrackSource::External { path: "t".into() }, None, None, cues)
    }

    #[test]
    fn active_handles_overlap_and_gaps() {
        let t = track(&[(0, 5000), (1000, 2000), (3000, 4000), (6000, 7000)]);
        assert_eq!(t.active_at(1500), vec![0, 1]);
        assert_eq!(t.active_at(2500), vec![0]);
        assert_eq!(t.active_at(5500), Vec::<usize>::new());
        assert_eq!(t.active_at(2000), vec![0], "end is exclusive");
        assert_eq!(t.active_at(6000), vec![3], "start is inclusive");
    }

    #[test]
    fn index_navigation() {
        let t = track(&[(1000, 2000), (3000, 4000)]);
        assert_eq!(t.index_at_or_before(500), None);
        assert_eq!(t.index_at_or_before(3500), Some(1));
        assert_eq!(t.index_after(500), Some(0));
        assert_eq!(t.index_after(1000), Some(1));
        assert_eq!(t.index_after(3000), None);
        assert_eq!(t.ended_between(1900, 2000), vec![0]);
        assert_eq!(t.ended_between(2000, 2100), Vec::<usize>::new());
    }

    #[test]
    fn new_sorts_cues() {
        let t = track(&[(3000, 4000), (1000, 2000)]);
        assert_eq!(t.cues[0].start, 1000);
    }
}
