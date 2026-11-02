//! The play history, stored as JSON in the data directory.
//!
//! Each record keeps the raw inputs (G, L, M, UR). S is never stored: it is
//! recomputed from them, so a formula change applies to old plays too.

use std::{
    collections::VecDeque,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::{
    calc::{self, GlobalSuggestion, PlayStats},
    storage,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlayRecord {
    /// Unix seconds when the play was recorded.
    pub timestamp: u64,
    /// Global offset during the play.
    pub g: i32,
    /// Local (beatmap) offset during the play. Recorded, not used for S.
    pub l: i32,
    /// Median hit error, song time, 50s removed.
    pub median: f64,
    /// Unstable rate, real time, 50s removed.
    pub unstable_rate: f64,
    /// Hits left after removing 50s.
    pub hits: usize,
    /// osu! stable mod bits.
    pub mods: u32,
    /// Beatmap MD5 checksum, for the per-beatmap offset later.
    #[serde(default)]
    pub map_checksum: Option<String>,
    /// "Artist - Title [Difficulty]", for display.
    #[serde(default)]
    pub map_name: Option<String>,
}

impl PlayRecord {
    pub fn new(g: i32, l: i32, mods: u32, stats: PlayStats) -> Self {
        Self {
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            g,
            l,
            median: stats.median,
            unstable_rate: stats.unstable_rate,
            hits: stats.hits,
            mods,
            map_checksum: None,
            map_name: None,
        }
    }

    /// S for this play, computed on demand.
    pub fn suggestion(&self) -> f64 {
        calc::play_suggestion(self.g, self.median, self.unstable_rate)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct History {
    /// Oldest first, newest last.
    plays: VecDeque<PlayRecord>,
}

impl History {
    /// Load the history, or start empty if the file does not exist yet.
    /// Extra plays beyond `limit` are dropped (oldest first).
    pub fn load(path: &Path, limit: usize) -> Result<Self> {
        let mut history = match storage::read_optional(path)? {
            Some(bytes) => serde_json::from_slice::<Self>(&bytes)
                .with_context(|| format!("invalid history file {}", path.display()))?,
            None => Self::default(),
        };
        history.trim(limit);
        Ok(history)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_vec_pretty(self).context("failed to serialize history")?;
        storage::write_atomic(path, &json)
    }

    /// Add the newest play and drop the oldest ones past `limit`.
    pub fn push(&mut self, play: PlayRecord, limit: usize) {
        self.plays.push_back(play);
        self.trim(limit);
    }

    /// Drop the oldest plays until at most `limit` remain.
    pub fn trim(&mut self, limit: usize) {
        while self.plays.len() > limit {
            self.plays.pop_front();
        }
    }

    pub fn plays(&self) -> &VecDeque<PlayRecord> {
        &self.plays
    }

    /// Mean of every play's S.
    pub fn suggestion(&self) -> Option<GlobalSuggestion> {
        let values: Vec<f64> = self.plays.iter().map(PlayRecord::suggestion).collect();
        calc::global_suggestion(&values)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn play(g: i32, median: f64) -> PlayRecord {
        PlayRecord::new(
            g,
            0,
            0,
            PlayStats {
                median,
                unstable_rate: 80.0,
                hits: 100,
            },
        )
    }

    #[test]
    fn keeps_only_the_newest_plays() {
        let mut history = History::default();
        for i in 0..55 {
            history.push(play(0, f64::from(i)), 50);
        }
        assert_eq!(history.plays().len(), 50);
        assert_eq!(history.plays().front().unwrap().median, 5.0);
        assert_eq!(history.plays().back().unwrap().median, 54.0);
    }

    #[test]
    fn suggestion_uses_each_play_s() {
        let mut history = History::default();
        // Same real offset (-7) seen at different global offsets.
        history.push(play(0, 7.0), 50);
        history.push(play(-5, 2.0), 50);
        history.push(play(-10, -3.0), 50);
        let s = history.suggestion().unwrap();
        assert_eq!(s.mean, -7.0);
        assert_eq!(s.apply, -7);
        assert_eq!(History::default().suggestion(), None);
    }

    #[test]
    fn save_and_load_round_trip_and_trim_on_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data/history.json");
        assert_eq!(History::load(&path, 50).unwrap(), History::default());

        let mut history = History::default();
        for i in 0..10 {
            history.push(play(0, f64::from(i)), 50);
        }
        history.save(&path).unwrap();

        assert_eq!(History::load(&path, 50).unwrap(), history);
        let smaller = History::load(&path, 3).unwrap();
        assert_eq!(smaller.plays().len(), 3);
        assert_eq!(smaller.plays().front().unwrap().median, 7.0);
    }

    #[test]
    fn s_is_not_stored_in_the_file() {
        let json = serde_json::to_string(&play(0, 7.0)).unwrap();
        assert!(!json.contains("\"s\""));
        assert!(!json.contains("suggestion"));
    }
}
