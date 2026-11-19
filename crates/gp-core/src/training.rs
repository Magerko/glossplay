use crate::track::{Ms, Track};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrainAction {
    Pause,
    SeekTo(Ms),
}

#[derive(Debug, Default)]
pub struct Trainer {
    pub autopause: bool,
    paused_at: Option<usize>,
}

// A jump of more than a second in one frame is a seek, not playback:
// even at 4× speed a 60 Hz frame moves the position by about 67 ms.
const SEEK_JUMP: Ms = 1000;
// Within the first half second of a line, "previous" goes one line further back.
const PREV_GRACE: Ms = 500;

impl Trainer {
    pub fn new(autopause: bool) -> Self {
        Trainer {
            autopause,
            paused_at: None,
        }
    }

    /// Call every frame. Returns Pause once, when playback crosses the end of a line.
    pub fn tick(&mut self, track: &Track, prev_pos: Ms, pos: Ms) -> Option<TrainAction> {
        if pos < prev_pos || pos - prev_pos > SEEK_JUMP {
            self.paused_at = None;
            return None;
        }
        if !self.autopause {
            return None;
        }
        let idx = *track.ended_between(prev_pos, pos).last()?;
        if self.paused_at == Some(idx) {
            return None;
        }
        self.paused_at = Some(idx);
        Some(TrainAction::Pause)
    }

    pub fn replay(&self, track: &Track, pos: Ms) -> Option<TrainAction> {
        let i = track.index_at_or_before(pos)?;
        Some(TrainAction::SeekTo(track.cues[i].start))
    }

    pub fn next(&self, track: &Track, pos: Ms) -> Option<TrainAction> {
        let i = track.index_after(pos)?;
        Some(TrainAction::SeekTo(track.cues[i].start))
    }

    pub fn prev(&self, track: &Track, pos: Ms) -> Option<TrainAction> {
        let i = track.index_at_or_before(pos - PREV_GRACE)?;
        let inside = pos < track.cues[i].end;
        let target = if inside && i > 0 { i - 1 } else { i };
        Some(TrainAction::SeekTo(track.cues[target].start))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::track::{Cue, TrackSource};

    fn track() -> Track {
        let cues = [(1000, 2000), (3000, 4000), (5000, 6000)]
            .iter()
            .map(|&(s, e)| Cue {
                start: s,
                end: e,
                text: "x".into(),
            })
            .collect();
        Track::new(TrackSource::External { path: "t".into() }, None, None, cues)
    }

    #[test]
    fn autopause_fires_once_at_cue_end() {
        let t = track();
        let mut tr = Trainer::new(true);
        assert_eq!(tr.tick(&t, 1900, 1950), None);
        assert_eq!(tr.tick(&t, 1950, 2010), Some(TrainAction::Pause));
        assert_eq!(tr.tick(&t, 2010, 2010), None);
        assert_eq!(tr.tick(&t, 2010, 2030), None);
        assert_eq!(tr.tick(&t, 3990, 4005), Some(TrainAction::Pause));
    }

    #[test]
    fn replay_then_same_cue_pauses_again() {
        let t = track();
        let mut tr = Trainer::new(true);
        assert_eq!(tr.tick(&t, 1990, 2005), Some(TrainAction::Pause));
        assert_eq!(tr.replay(&t, 2005), Some(TrainAction::SeekTo(1000)));
        assert_eq!(tr.tick(&t, 2005, 1000), None);
        assert_eq!(tr.tick(&t, 1990, 2005), Some(TrainAction::Pause));
    }

    #[test]
    fn seek_over_cue_end_does_not_pause() {
        let t = track();
        let mut tr = Trainer::new(true);
        assert_eq!(tr.tick(&t, 1500, 4500), None);
    }

    #[test]
    fn disabled_never_pauses() {
        let t = track();
        let mut tr = Trainer::new(false);
        assert_eq!(tr.tick(&t, 1990, 2005), None);
    }

    #[test]
    fn navigation() {
        let t = track();
        let tr = Trainer::new(false);
        assert_eq!(tr.next(&t, 0), Some(TrainAction::SeekTo(1000)));
        assert_eq!(tr.next(&t, 1500), Some(TrainAction::SeekTo(3000)));
        assert_eq!(tr.next(&t, 5500), None);
        assert_eq!(
            tr.prev(&t, 3700),
            Some(TrainAction::SeekTo(1000)),
            "inside cue: previous"
        );
        assert_eq!(
            tr.prev(&t, 4500),
            Some(TrainAction::SeekTo(3000)),
            "in gap: cue just ended"
        );
        assert_eq!(
            tr.prev(&t, 1700),
            Some(TrainAction::SeekTo(1000)),
            "first cue stays"
        );
        assert_eq!(tr.prev(&t, 500), None);
    }
}
