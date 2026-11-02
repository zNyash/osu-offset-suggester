//! Detects when a play starts and ends, and collects its hit errors.
//!
//! Called once per poll. While a play is running it reads only the hit
//! errors added since the last poll. A play ends when osu! leaves the
//! playing state (finished, failed then exited, quit) or when it restarts
//! (retry). Then the play is either recorded or skipped with a reason.

use anyhow::Result;

use crate::{
    calc,
    history::PlayRecord,
    osu::{Beatmap, Gameplay, MODE_STANDARD, OsuReader, Status},
    memory::MemSource,
};

/// Mods that disqualify a play (no real tapping, or not timed by the player).
mod excluded {
    pub const RELAX: u32 = 1 << 7;
    pub const AUTOPLAY: u32 = 1 << 11;
    pub const AUTOPILOT: u32 = 1 << 13;
    pub const CINEMA: u32 = 1 << 22;
}

/// What the tracker needs from the game. `OsuReader` implements it; tests
/// use a scripted fake.
pub trait GameSource {
    fn status(&self) -> Result<Status>;
    fn is_watching_replay(&self) -> Result<bool>;
    fn gameplay(&self) -> Result<Gameplay>;
    fn hit_errors_from(&self, from: usize) -> Result<Vec<i32>>;
    fn beatmap(&self) -> Result<Beatmap>;
    fn global_offset(&mut self) -> Result<i32>;
}

impl<M: MemSource> GameSource for OsuReader<M> {
    fn status(&self) -> Result<Status> {
        OsuReader::status(self)
    }
    fn is_watching_replay(&self) -> Result<bool> {
        OsuReader::is_watching_replay(self)
    }
    fn gameplay(&self) -> Result<Gameplay> {
        OsuReader::gameplay(self)
    }
    fn hit_errors_from(&self, from: usize) -> Result<Vec<i32>> {
        OsuReader::hit_errors_from(self, from)
    }
    fn beatmap(&self) -> Result<Beatmap> {
        OsuReader::beatmap(self)
    }
    fn global_offset(&mut self) -> Result<i32> {
        OsuReader::global_offset(self)
    }
}

/// A play that just ended.
#[derive(Clone, Debug, PartialEq)]
pub struct FinishedPlay {
    pub map_name: String,
    pub mods: u32,
    /// All raw hit errors (song time), for the hit-window graph.
    pub errors: Vec<i32>,
    /// OD after HR/EZ, for drawing the hit windows.
    pub od: f64,
    /// The 100/50 boundary used for this play.
    pub cutoff: i32,
    pub outcome: Outcome,
}

/// The play in progress, for the live view.
#[derive(Clone, Debug, PartialEq)]
pub struct LivePlay {
    pub map_name: String,
    pub mods: u32,
    /// OD after HR/EZ.
    pub od: f64,
    pub errors: Vec<i32>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Recorded(PlayRecord),
    Skipped(SkipReason),
}

#[derive(Clone, Debug, PartialEq)]
pub enum SkipReason {
    NotStandard,
    ExcludedMods,
    /// Fewer than `calc::MIN_HITS` hits after removing 50s.
    TooFewHits(usize),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Started { map_name: String, mods: u32 },
    Finished(FinishedPlay),
}

/// The play being collected.
struct CurrentPlay {
    beatmap: Beatmap,
    global_offset: i32,
    gameplay: Gameplay,
    errors: Vec<i32>,
}

#[derive(Default)]
pub struct Tracker {
    current: Option<CurrentPlay>,
}

impl Tracker {
    /// Hit errors collected so far in the current play.
    pub fn live_errors(&self) -> Option<&[i32]> {
        self.current.as_ref().map(|play| play.errors.as_slice())
    }

    /// A copy of the play in progress, for the TUI.
    pub fn live(&self) -> Option<LivePlay> {
        self.current.as_ref().map(|play| LivePlay {
            map_name: play.beatmap.name.clone(),
            mods: play.gameplay.mods,
            od: calc::effective_od(f64::from(play.beatmap.od), play.gameplay.mods),
            errors: play.errors.clone(),
        })
    }

    /// One poll. Returns at most two events (a retry ends one play and
    /// starts the next).
    pub fn poll(&mut self, game: &mut impl GameSource, count_autopilot: bool) -> Vec<Event> {
        let mut events = Vec::new();

        // A failed read is "unknown", not "stopped playing": wait for the
        // next poll instead of ending the play early.
        let (Ok(status), Ok(replay)) = (game.status(), game.is_watching_replay()) else {
            return events;
        };
        if status != Status::Playing || replay {
            if let Some(play) = self.current.take() {
                events.push(Event::Finished(finish(play, count_autopilot)));
            }
            return events;
        }
        let Ok(gameplay) = game.gameplay() else {
            return events;
        };

        // Retry: the retry counter changed, or the hit-error list got shorter.
        let restarted = self.current.as_ref().is_some_and(|current| {
            gameplay.retries != current.gameplay.retries
                || gameplay.hit_error_count < current.errors.len()
        });
        if restarted && let Some(play) = self.current.take() {
            events.push(Event::Finished(finish(play, count_autopilot)));
        }

        if self.current.is_none() {
            // A new play. If the beatmap or offset cannot be read yet (osu! is
            // still loading), try again next poll.
            let (Ok(beatmap), Ok(global_offset)) = (game.beatmap(), game.global_offset()) else {
                return events;
            };
            events.push(Event::Started {
                map_name: beatmap.name.clone(),
                mods: gameplay.mods,
            });
            self.current = Some(CurrentPlay {
                beatmap,
                global_offset,
                gameplay,
                errors: Vec::new(),
            });
        }

        let current = self.current.as_mut().expect("set above");
        // A failed read just means "nothing new this poll".
        if let Ok(new) = game.hit_errors_from(current.errors.len()) {
            current.errors.extend(new);
        }
        events
    }
}

fn finish(play: CurrentPlay, count_autopilot: bool) -> FinishedPlay {
    let mods = play.gameplay.mods;
    let od = calc::effective_od(f64::from(play.beatmap.od), mods);
    let cutoff = calc::cutoff_100(od);

    let mut excluded_mods = excluded::RELAX | excluded::AUTOPLAY | excluded::CINEMA;
    if !count_autopilot {
        excluded_mods |= excluded::AUTOPILOT;
    }

    let outcome = if play.gameplay.mode != MODE_STANDARD {
        Outcome::Skipped(SkipReason::NotStandard)
    } else if mods & excluded_mods != 0 {
        Outcome::Skipped(SkipReason::ExcludedMods)
    } else {
        match calc::analyze_play(&play.errors, f64::from(play.beatmap.od), mods) {
            Some(stats) => {
                let mut record = PlayRecord::new(
                    play.global_offset,
                    play.beatmap.local_offset,
                    mods,
                    stats,
                );
                record.map_checksum = Some(play.beatmap.checksum.clone());
                record.map_name = Some(play.beatmap.name.clone());
                Outcome::Recorded(record)
            }
            None => Outcome::Skipped(SkipReason::TooFewHits(
                calc::filter_50s(&play.errors, cutoff).len(),
            )),
        }
    };

    FinishedPlay {
        map_name: play.beatmap.name,
        mods,
        errors: play.errors,
        od,
        cutoff,
        outcome,
    }
}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;

    use super::*;

    /// A scripted game: tests change its fields between polls.
    struct FakeGame {
        status: Status,
        replay: bool,
        mode: i32,
        mods: u32,
        retries: i32,
        errors: Vec<i32>,
    }

    impl FakeGame {
        fn playing() -> Self {
            Self {
                status: Status::Playing,
                replay: false,
                mode: MODE_STANDARD,
                mods: 0,
                retries: 0,
                errors: Vec::new(),
            }
        }
    }

    impl GameSource for FakeGame {
        fn status(&self) -> Result<Status> {
            Ok(self.status)
        }
        fn is_watching_replay(&self) -> Result<bool> {
            Ok(self.replay)
        }
        fn gameplay(&self) -> Result<Gameplay> {
            if self.status != Status::Playing {
                return Err(anyhow!("no gameplay"));
            }
            Ok(Gameplay {
                mode: self.mode,
                mods: self.mods,
                retries: self.retries,
                failed: false,
                hit_error_count: self.errors.len(),
            })
        }
        fn hit_errors_from(&self, from: usize) -> Result<Vec<i32>> {
            Ok(self.errors.get(from..).unwrap_or_default().to_vec())
        }
        fn beatmap(&self) -> Result<Beatmap> {
            Ok(Beatmap {
                checksum: "abc".into(),
                name: "Map".into(),
                od: 8.0,
                local_offset: 3,
            })
        }
        fn global_offset(&mut self) -> Result<i32> {
            Ok(-10)
        }
    }

    fn finished(events: &[Event]) -> &FinishedPlay {
        events
            .iter()
            .find_map(|event| match event {
                Event::Finished(play) => Some(play),
                _ => None,
            })
            .expect("a finished play")
    }

    #[test]
    fn collects_errors_across_polls_and_records_on_exit() {
        let mut game = FakeGame::playing();
        let mut tracker = Tracker::default();

        let events = tracker.poll(&mut game, false);
        assert!(matches!(&events[..], [Event::Started { .. }]));

        game.errors = vec![5; 30];
        assert!(tracker.poll(&mut game, false).is_empty());
        game.errors.extend([5; 30]);
        tracker.poll(&mut game, false);
        assert_eq!(tracker.live_errors().unwrap().len(), 60);

        game.status = Status::Results;
        let events = tracker.poll(&mut game, false);
        let play = finished(&events);
        let Outcome::Recorded(record) = &play.outcome else {
            panic!("expected a recorded play, got {:?}", play.outcome);
        };
        assert_eq!((record.g, record.l, record.hits), (-10, 3, 60));
        assert_eq!(record.median, 5.0);
        assert_eq!(play.cutoff, 76);
        assert!(tracker.live_errors().is_none());
    }

    #[test]
    fn quitting_mid_map_still_counts() {
        let mut game = FakeGame::playing();
        let mut tracker = Tracker::default();
        tracker.poll(&mut game, false);
        game.errors = vec![2; 55];
        tracker.poll(&mut game, false);
        game.status = Status::SongSelect;
        let events = tracker.poll(&mut game, false);
        assert!(matches!(finished(&events).outcome, Outcome::Recorded(_)));
    }

    #[test]
    fn retry_ends_one_play_and_starts_the_next() {
        let mut game = FakeGame::playing();
        let mut tracker = Tracker::default();
        tracker.poll(&mut game, false);
        game.errors = vec![1; 20];
        tracker.poll(&mut game, false);

        game.retries = 1;
        game.errors = vec![9; 2];
        let events = tracker.poll(&mut game, false);
        assert_eq!(
            finished(&events).outcome,
            Outcome::Skipped(SkipReason::TooFewHits(20))
        );
        assert!(matches!(events.last(), Some(Event::Started { .. })));
        assert_eq!(tracker.live_errors().unwrap(), &[9, 9]);
    }

    #[test]
    fn replays_are_ignored() {
        let mut game = FakeGame::playing();
        game.replay = true;
        game.errors = vec![1; 100];
        let mut tracker = Tracker::default();
        assert!(tracker.poll(&mut game, false).is_empty());
        assert!(tracker.live_errors().is_none());
    }

    #[test]
    fn excluded_mods_and_autopilot_setting() {
        for (mods, count_autopilot, recorded) in [
            (excluded::RELAX, false, false),
            (excluded::AUTOPLAY, true, false),
            (excluded::CINEMA, true, false),
            (excluded::AUTOPILOT, false, false),
            (excluded::AUTOPILOT, true, true),
        ] {
            let mut game = FakeGame::playing();
            game.mods = mods;
            let mut tracker = Tracker::default();
            tracker.poll(&mut game, count_autopilot);
            game.errors = vec![0; 60];
            tracker.poll(&mut game, count_autopilot);
            game.status = Status::Menu;
            let events = tracker.poll(&mut game, count_autopilot);
            assert_eq!(
                matches!(finished(&events).outcome, Outcome::Recorded(_)),
                recorded,
                "mods {mods:#x}, autopilot setting {count_autopilot}"
            );
        }
    }

    #[test]
    fn other_modes_are_skipped() {
        let mut game = FakeGame::playing();
        game.mode = 3;
        let mut tracker = Tracker::default();
        tracker.poll(&mut game, false);
        game.errors = vec![0; 60];
        tracker.poll(&mut game, false);
        game.status = Status::Menu;
        let events = tracker.poll(&mut game, false);
        assert_eq!(
            finished(&events).outcome,
            Outcome::Skipped(SkipReason::NotStandard)
        );
    }
}
