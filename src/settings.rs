//! User settings, stored as TOML in the config directory.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::{calc::Aggregation, storage};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
// Any field missing from the file falls back to its default, so adding new
// settings later never breaks an existing settings.toml.
#[serde(default)]
pub struct Settings {
    /// How many plays the global suggestion averages over.
    pub history_size: usize,
    /// How often the game state and hit errors are polled.
    pub poll_interval_ms: u64,
    /// Count plays with Autopilot (off by default).
    pub count_autopilot: bool,
    /// Plays kept per beatmap for the local suggestion (in memory only).
    pub map_history_size: usize,
    /// How a beatmap's plays are combined: "mean" or "median".
    pub map_aggregation: Aggregation,
}

pub const MAX_MAP_HISTORY: usize = 50;

impl Default for Settings {
    fn default() -> Self {
        Self {
            history_size: 50,
            poll_interval_ms: 100,
            count_autopilot: false,
            map_history_size: 1,
            map_aggregation: Aggregation::Mean,
        }
    }
}

impl Settings {
    /// Load settings; if the file does not exist yet, write the defaults so
    /// the user has a file to look at.
    pub fn load_or_create(path: &Path) -> Result<Self> {
        let settings = match storage::read_optional(path)? {
            Some(bytes) => {
                let text = String::from_utf8(bytes)
                    .with_context(|| format!("{} is not valid UTF-8", path.display()))?;
                toml::from_str::<Self>(&text)
                    .with_context(|| format!("invalid settings file {}", path.display()))?
            }
            None => {
                let defaults = Self::default();
                defaults.save(path)?;
                defaults
            }
        };
        Ok(settings.sanitized())
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let text = toml::to_string_pretty(self).context("failed to serialize settings")?;
        storage::write_atomic(path, text.as_bytes())
    }

    /// Clamp values that would make the app misbehave.
    fn sanitized(mut self) -> Self {
        self.history_size = self.history_size.max(1);
        self.poll_interval_ms = self.poll_interval_ms.clamp(10, 5_000);
        self.map_history_size = self.map_history_size.clamp(1, MAX_MAP_HISTORY);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_creates_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config/settings.toml");
        assert_eq!(
            Settings::load_or_create(&path).unwrap(),
            Settings::default()
        );
        assert!(path.exists());
    }

    #[test]
    fn missing_fields_use_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        std::fs::write(&path, "history_size = 20\n").unwrap();
        let settings = Settings::load_or_create(&path).unwrap();
        assert_eq!(settings.history_size, 20);
        assert_eq!(settings.poll_interval_ms, 100);
        assert!(!settings.count_autopilot);
    }

    #[test]
    fn round_trips_and_sanitizes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        let custom = Settings {
            history_size: 0,
            poll_interval_ms: 1,
            count_autopilot: true,
            map_history_size: 500,
            map_aggregation: Aggregation::Median,
        };
        custom.save(&path).unwrap();
        let loaded = Settings::load_or_create(&path).unwrap();
        assert_eq!(loaded.history_size, 1);
        assert_eq!(loaded.poll_interval_ms, 10);
        assert!(loaded.count_autopilot);
        assert_eq!(loaded.map_history_size, MAX_MAP_HISTORY);
        assert_eq!(loaded.map_aggregation, Aggregation::Median);
    }

    #[test]
    fn aggregation_is_written_in_lowercase() {
        let text = toml::to_string(&Settings::default()).unwrap();
        assert!(text.contains("map_aggregation = \"mean\""));
    }
}
