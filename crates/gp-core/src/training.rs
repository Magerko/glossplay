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
    /// The end of the line auto-pause stopped after, and where playback was when it did.
    held: Option<(Ms, Ms)>,
}

// A jump of more than a second in one frame is a seek, not playback:
// even at 4× speed a 60 Hz frame moves the position by about 67 ms.
const SEEK_JUMP: Ms = 1000;
// Within the first half second of a line, "previous" goes one line further back.
const PREV_GRACE: Ms = 500;
// The pause lands a frame or two after the end of the line, and mpv takes a moment more to
// stop; past this the position has moved on for some other reason.
const HOLD_SLACK: Ms = 300;

impl Trainer {
    pub fn new(autopause: bool) -> Self {
        Trainer {
            autopause,
            paused_at: None,
            held: None,
        }
    }

    /// Call every frame. Returns Pause once, when playback crosses the end of a line.
    pub fn tick(&mut self, track: &Track, prev_pos: Ms, pos: Ms) -> Option<TrainAction> {
        if pos < prev_pos || pos - prev_pos > SEEK_JUMP {
            self.paused_at = None;
            self.held = None;
            return None;
        }
        if self.held.is_some_and(|(_, at)| pos > at + HOLD_SLACK) {
            self.held = None;
        }
        if !self.autopause {
            return None;
        }
        let idx = *track.ended_between(prev_pos, pos).last()?;
        if self.paused_at == Some(idx) {
            return None;
        }
        self.paused_at = Some(idx);
        self.held = Some((track.cues[idx].end, pos));
        Some(TrainAction::Pause)
    }

    /// The moment to show subtitles for. The pause comes only after the line has ended, so
    /// while playback stands where auto-pause stopped it the line is kept on screen: that is
    /// the line the pause is for.
    pub fn shown_at(&self, pos: Ms, paused: bool) -> Ms {
        match self.held {
            Some((end, at)) if paused && pos >= end && pos <= at + HOLD_SLACK => end - 1,
            _ => pos,
        }
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
    fn the_line_stays_on_screen_while_auto_pause_holds() {
        let mut t = track();
        // The next line starts right where this one ends, as speech recognition makes them.
        t.cues[1].start = 2000;
        let mut tr = Trainer::new(true);
        assert_eq!(tr.tick(&t, 1990, 2016), Some(TrainAction::Pause));
        // mpv is still playing for a frame after the pause was asked for.
        assert_eq!(tr.tick(&t, 2016, 2040), None);
        assert_eq!(tr.shown_at(2040, false), 2040, "not held before it stops");
        let shown = tr.shown_at(2060, true);
        assert!(
            t.active_at(shown).contains(&0),
            "the line paused for is shown"
        );
        assert!(!t.active_at(shown).contains(&1), "not the next one");
        // Seeking away while paused, or playing on, lets go of it.
        assert_eq!(tr.shown_at(1500, true), 1500);
        assert_eq!(tr.shown_at(2900, true), 2900);
        assert_eq!(tr.tick(&t, 2060, 2080), None);
        assert_eq!(tr.tick(&t, 2080, 2400), None);
        assert_eq!(
            tr.shown_at(2400, true),
            2400,
            "a later manual pause shows its own moment"
        );
    }

    #[test]
    fn a_seek_drops_the_held_line() {
        let t = track();
        let mut tr = Trainer::new(true);
        assert_eq!(tr.tick(&t, 1990, 2010), Some(TrainAction::Pause));
        assert_eq!(tr.shown_at(2010, true), 1999);
        assert_eq!(tr.tick(&t, 2010, 1200), None);
        assert_eq!(tr.shown_at(2010, true), 2010);
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
