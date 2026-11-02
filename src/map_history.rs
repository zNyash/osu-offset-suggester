//! Per-beatmap plays for the local offset suggestion.
//!
//! In memory only, by design: it starts empty each time the app starts and
//! is never written to disk.

use std::collections::{HashMap, VecDeque};

use crate::{
    calc::{self, Aggregation},
    history::PlayRecord,
};

#[derive(Default)]
pub struct MapHistory {
    /// Beatmap checksum -> that map's plays, oldest first.
    maps: HashMap<String, VecDeque<PlayRecord>>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LocalSuggestion {
    /// Combined (mean or median), unrounded.
    pub value: f64,
    /// Rounded to a whole ms, the value to set in osu! stable.
    pub apply: i32,
    /// How many plays it is based on.
    pub plays: usize,
    /// The same suggestion shifted by how much the global offset changed
    /// since those plays. `None` when the global offset is unchanged.
    pub adjusted_for_global: Option<i32>,
}

impl MapHistory {
    /// Add a play (ignored without a checksum), keeping the newest `limit`.
    pub fn push(&mut self, play: PlayRecord, limit: usize) {
        let Some(checksum) = play.map_checksum.clone() else {
            return;
        };
        let plays = self.maps.entry(checksum).or_default();
        plays.push_back(play);
        while plays.len() > limit {
            plays.pop_front();
        }
    }

    /// Apply a new limit to every map.
    pub fn trim(&mut self, limit: usize) {
        for plays in self.maps.values_mut() {
            while plays.len() > limit {
                plays.pop_front();
            }
        }
    }

    /// Forget one beatmap's plays. Returns how many were removed.
    pub fn clear_map(&mut self, checksum: &str) -> usize {
        self.maps.remove(checksum).map_or(0, |plays| plays.len())
    }

    /// Forget every beatmap's plays. Returns how many maps were cleared.
    pub fn clear_all(&mut self) -> usize {
        let maps = self.maps.len();
        self.maps.clear();
        maps
    }

    pub fn suggestion(
        &self,
        checksum: &str,
        how: Aggregation,
        current_global: Option<i32>,
    ) -> Option<LocalSuggestion> {
        let plays = self.maps.get(checksum)?;
        let values: Vec<f64> = plays
            .iter()
            .map(|p| calc::local_play_suggestion(p.l, p.median, p.unstable_rate))
            .collect();
        let value = calc::combine(&values, how)?;

        let adjusted_for_global = current_global
            .filter(|&g| plays.iter().any(|p| p.g != g))
            .and_then(|g| {
                let shifted: Vec<f64> = plays
                    .iter()
                    .zip(&values)
                    .map(|(p, local)| local + f64::from(g - p.g))
                    .collect();
                calc::combine(&shifted, how).map(|v| v.round() as i32)
            });

        Some(LocalSuggestion {
            value,
            apply: value.round() as i32,
            plays: plays.len(),
            adjusted_for_global,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calc::PlayStats;

    fn play(map: &str, g: i32, l: i32, median: f64) -> PlayRecord {
        let mut record = PlayRecord::new(
            g,
            l,
            0,
            PlayStats {
                median,
                unstable_rate: 80.0,
                hits: 100,
            },
        );
        record.map_checksum = Some(map.into());
        record
    }

    #[test]
    fn late_play_suggests_positive_local_offset() {
        let mut maps = MapHistory::default();
        maps.push(play("a", 0, 0, 5.0), 1);
        let s = maps.suggestion("a", Aggregation::Mean, Some(0)).unwrap();
        assert_eq!((s.apply, s.plays, s.adjusted_for_global), (5, 1, None));
        assert!(maps.suggestion("b", Aggregation::Mean, Some(0)).is_none());
    }

    #[test]
    fn keeps_only_the_newest_plays_per_map() {
        let mut maps = MapHistory::default();
        maps.push(play("a", 0, 0, 1.0), 1);
        maps.push(play("a", 0, 0, 9.0), 1);
        maps.push(play("b", 0, 0, 3.0), 1);
        assert_eq!(maps.suggestion("a", Aggregation::Mean, None).unwrap().value, 9.0);
        assert_eq!(maps.suggestion("b", Aggregation::Mean, None).unwrap().value, 3.0);
    }

    #[test]
    fn mean_or_median_over_several_plays() {
        let mut maps = MapHistory::default();
        for median in [1.0, 2.0, 9.0] {
            maps.push(play("a", 0, 0, median), 3);
        }
        assert_eq!(maps.suggestion("a", Aggregation::Mean, None).unwrap().value, 4.0);
        assert_eq!(maps.suggestion("a", Aggregation::Median, None).unwrap().value, 2.0);

        maps.trim(1);
        assert_eq!(maps.suggestion("a", Aggregation::Mean, None).unwrap().plays, 1);
    }

    #[test]
    fn global_change_only_affects_the_adjusted_value() {
        let mut maps = MapHistory::default();
        maps.push(play("a", 0, 0, 5.0), 1);
        // Global moved from 0 to -5 after the play.
        let s = maps.suggestion("a", Aggregation::Mean, Some(-5)).unwrap();
        assert_eq!(s.apply, 5);
        assert_eq!(s.adjusted_for_global, Some(0));
    }

    #[test]
    fn clearing_one_map_or_all() {
        let mut maps = MapHistory::default();
        maps.push(play("a", 0, 0, 1.0), 5);
        maps.push(play("a", 0, 0, 2.0), 5);
        maps.push(play("b", 0, 0, 3.0), 5);
        assert_eq!(maps.clear_map("a"), 2);
        assert!(maps.suggestion("a", Aggregation::Mean, None).is_none());
        assert!(maps.suggestion("b", Aggregation::Mean, None).is_some());
        assert_eq!(maps.clear_all(), 1);
        assert!(maps.suggestion("b", Aggregation::Mean, None).is_none());
    }

    #[test]
    fn plays_without_a_checksum_are_ignored() {
        let mut maps = MapHistory::default();
        let mut record = play("a", 0, 0, 5.0);
        record.map_checksum = None;
        maps.push(record, 1);
        assert!(maps.maps.is_empty());
    }
}
