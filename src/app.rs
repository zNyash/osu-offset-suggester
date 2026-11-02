//! TUI state and input handling. Drawing lives in `ui.rs`.

use std::{
    path::PathBuf,
    sync::mpsc::{Receiver, Sender},
    time::Duration,
};

use anyhow::Result;
use ratatui::{
    DefaultTerminal,
    crossterm::event::{self, Event as TermEvent, KeyCode, KeyEventKind},
};

use crate::{
    history::History,
    map_history::MapHistory,
    osu::Beatmap,
    calc::Aggregation,
    settings::{MAX_MAP_HISTORY, Settings},
    tracker::{Event, FinishedPlay, LivePlay, Outcome},
    ui,
    worker::{Command, Connection, Update, WorkerConfig},
};

/// How long to wait for a key press before checking worker updates.
const INPUT_TICK: Duration = Duration::from_millis(50);

pub struct App {
    pub settings: Settings,
    pub history: History,
    /// Per-beatmap plays for the local suggestion (this session only).
    pub map_history: MapHistory,
    settings_path: PathBuf,
    history_path: PathBuf,

    pub connection: Connection,
    pub global_offset: Option<i32>,
    pub beatmap: Option<Beatmap>,
    pub live: Option<LivePlay>,
    pub last_play: Option<FinishedPlay>,
    /// One-line feedback (last play result, save errors, ...).
    pub message: Option<String>,
    pub menu: Option<SettingsMenu>,
    /// A reset waiting for y/n.
    pub confirm: Option<Reset>,

    commands: Sender<Command>,
    updates: Receiver<Update>,
    quit: bool,
}

/// The settings popup: edits a draft, applied on Enter.
/// What a reset key clears.
#[derive(Clone, Debug, PartialEq)]
pub enum Reset {
    /// The saved global history.
    Global,
    /// One beatmap's in-memory plays: (checksum, name).
    Map(String, String),
    /// Every beatmap's in-memory plays.
    AllMaps,
}

pub struct SettingsMenu {
    pub draft: Settings,
    pub selected: usize,
}

pub const SETTING_COUNT: usize = 5;
/// Settings toggled with Space as well as ←→.
const TOGGLES: [usize; 2] = [2, 4];

impl App {
    pub fn new(
        settings: Settings,
        history: History,
        settings_path: PathBuf,
        history_path: PathBuf,
        commands: Sender<Command>,
        updates: Receiver<Update>,
    ) -> Self {
        Self {
            settings,
            history,
            map_history: MapHistory::default(),
            settings_path,
            history_path,
            connection: Connection::WaitingForOsu,
            global_offset: None,
            beatmap: None,
            live: None,
            last_play: None,
            message: None,
            menu: None,
            confirm: None,
            commands,
            updates,
            quit: false,
        }
    }

    pub fn run(mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        while !self.quit {
            terminal.draw(|frame| ui::draw(frame, &self))?;
            if event::poll(INPUT_TICK)?
                && let TermEvent::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
            {
                self.on_key(key.code);
            }
            while let Ok(update) = self.updates.try_recv() {
                self.on_update(update);
            }
        }
        let _ = self.commands.send(Command::Quit);
        Ok(())
    }

    fn on_update(&mut self, update: Update) {
        match update {
            Update::Connection(connection) => self.connection = connection,
            Update::GlobalOffset(offset) => self.global_offset = Some(offset),
            Update::Beatmap(beatmap) => self.beatmap = beatmap,
            Update::Live(live) => self.live = live,
            Update::Play(Event::Started { .. }) => {}
            Update::Play(Event::Finished(play)) => {
                self.message = Some(match &play.outcome {
                    Outcome::Recorded(record) => {
                        self.map_history.push(record.clone(), self.settings.map_history_size);
                        self.history.push(record.clone(), self.settings.history_size);
                        match self.history.save(&self.history_path) {
                            Ok(()) => format!("Recorded: S {:+.1} ms", record.suggestion()),
                            Err(error) => format!("Recorded, but saving failed: {error:#}"),
                        }
                    }
                    Outcome::Skipped(reason) => format!("Skipped: {}", ui::skip_reason(reason)),
                });
                self.last_play = Some(play);
            }
        }
    }

    fn on_key(&mut self, key: KeyCode) {
        if let Some(reset) = self.confirm.take() {
            match key {
                KeyCode::Char('y') | KeyCode::Char('Y') => self.reset(reset),
                _ => self.message = Some("Reset cancelled".into()),
            }
            return;
        }
        match &mut self.menu {
            None => match key {
                KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
                KeyCode::Char('g') => self.confirm = Some(Reset::Global),
                KeyCode::Char('m') => match &self.beatmap {
                    Some(beatmap) => {
                        self.confirm = Some(Reset::Map(beatmap.checksum.clone(), beatmap.name.clone()));
                    }
                    None => self.message = Some("No beatmap selected to reset".into()),
                },
                KeyCode::Char('a') => self.confirm = Some(Reset::AllMaps),
                KeyCode::Char('s') => {
                    self.menu = Some(SettingsMenu {
                        draft: self.settings.clone(),
                        selected: 0,
                    });
                }
                _ => {}
            },
            Some(menu) => match key {
                KeyCode::Esc => self.menu = None,
                KeyCode::Up => menu.selected = menu.selected.saturating_sub(1),
                KeyCode::Down => menu.selected = (menu.selected + 1).min(SETTING_COUNT - 1),
                KeyCode::Left => adjust(&mut menu.draft, menu.selected, -1),
                KeyCode::Right => adjust(&mut menu.draft, menu.selected, 1),
                KeyCode::Char(' ') if TOGGLES.contains(&menu.selected) => {
                    adjust(&mut menu.draft, menu.selected, 1);
                }
                KeyCode::Enter => {
                    let draft = menu.draft.clone();
                    self.menu = None;
                    self.apply_settings(draft);
                }
                _ => {}
            },
        }
    }

    fn reset(&mut self, reset: Reset) {
        self.message = Some(match reset {
            Reset::Global => {
                let plays = self.history.plays().len();
                self.history.clear();
                match self.history.save(&self.history_path) {
                    Ok(()) => format!("Global history reset ({plays} plays removed)"),
                    Err(error) => format!("Global history cleared, but saving failed: {error:#}"),
                }
            }
            Reset::Map(checksum, name) => {
                let plays = self.map_history.clear_map(&checksum);
                format!("Reset {name} ({plays} plays removed)")
            }
            Reset::AllMaps => {
                let maps = self.map_history.clear_all();
                format!("All per-map history reset ({maps} maps)")
            }
        });
    }

    fn apply_settings(&mut self, settings: Settings) {
        let shrunk = settings.history_size < self.history.plays().len();
        self.settings = settings;
        self.history.trim(self.settings.history_size);
        self.map_history.trim(self.settings.map_history_size);
        let _ = self.commands.send(Command::Configure(worker_config(&self.settings)));

        let result = self
            .settings
            .save(&self.settings_path)
            .and_then(|()| match shrunk {
                true => self.history.save(&self.history_path),
                false => Ok(()),
            });
        self.message = Some(match result {
            Ok(()) => "Settings saved".into(),
            Err(error) => format!("Saving settings failed: {error:#}"),
        });
    }
}

pub fn worker_config(settings: &Settings) -> WorkerConfig {
    WorkerConfig {
        poll_interval: Duration::from_millis(settings.poll_interval_ms),
        count_autopilot: settings.count_autopilot,
    }
}

/// Change one setting by one step in `direction` (-1 or +1).
fn adjust(settings: &mut Settings, index: usize, direction: i64) {
    match index {
        0 => {
            settings.history_size =
                (settings.history_size as i64 + direction).clamp(1, 1_000) as usize;
        }
        1 => {
            settings.poll_interval_ms =
                (settings.poll_interval_ms as i64 + 10 * direction).clamp(10, 5_000) as u64;
        }
        2 => settings.count_autopilot = !settings.count_autopilot,
        3 => {
            settings.map_history_size = (settings.map_history_size as i64 + direction)
                .clamp(1, MAX_MAP_HISTORY as i64) as usize;
        }
        4 => {
            settings.map_aggregation = match settings.map_aggregation {
                Aggregation::Mean => Aggregation::Median,
                Aggregation::Median => Aggregation::Mean,
            };
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::mpsc;

    use crate::{calc::PlayStats, history::PlayRecord};

    fn app_with_plays() -> (App, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let (command_tx, _) = mpsc::channel();
        let (_, update_rx) = mpsc::channel();
        let mut app = App::new(
            Settings::default(),
            History::default(),
            dir.path().join("settings.toml"),
            dir.path().join("history.json"),
            command_tx,
            update_rx,
        );
        for map in ["a", "a", "b"] {
            let mut record = PlayRecord::new(0, 0, 0, PlayStats { median: 1.0, unstable_rate: 80.0, hits: 60 });
            record.map_checksum = Some(map.into());
            app.history.push(record.clone(), 50);
            app.map_history.push(record, 5);
        }
        app.beatmap = Some(Beatmap { checksum: "a".into(), name: "Map A".into(), od: 8.0, local_offset: 0 });
        (app, dir)
    }

    #[test]
    fn resets_need_confirmation() {
        let (mut app, _dir) = app_with_plays();
        app.on_key(KeyCode::Char('g'));
        assert_eq!(app.confirm, Some(Reset::Global));
        app.on_key(KeyCode::Char('n'));
        assert_eq!(app.history.plays().len(), 3);
        assert!(app.confirm.is_none());

        app.on_key(KeyCode::Char('g'));
        app.on_key(KeyCode::Char('y'));
        assert!(app.history.plays().is_empty());
        // The cleared history was saved to disk.
        assert!(History::load(&app.history_path, 50).unwrap().plays().is_empty());
    }

    #[test]
    fn resets_current_map_then_all_maps() {
        let (mut app, _dir) = app_with_plays();
        let mean = Aggregation::Mean;
        app.on_key(KeyCode::Char('m'));
        app.on_key(KeyCode::Char('y'));
        assert!(app.map_history.suggestion("a", mean, None).is_none());
        assert!(app.map_history.suggestion("b", mean, None).is_some());
        assert_eq!(app.history.plays().len(), 3, "global history untouched");

        app.on_key(KeyCode::Char('a'));
        app.on_key(KeyCode::Char('y'));
        assert!(app.map_history.suggestion("b", mean, None).is_none());
    }

    #[test]
    fn adjust_steps_and_clamps() {
        let mut settings = Settings::default();
        adjust(&mut settings, 0, 1);
        assert_eq!(settings.history_size, 51);
        adjust(&mut settings, 1, -1);
        assert_eq!(settings.poll_interval_ms, 90);
        adjust(&mut settings, 2, 1);
        assert!(settings.count_autopilot);
        adjust(&mut settings, 3, 1);
        assert_eq!(settings.map_history_size, 2);
        adjust(&mut settings, 3, -5);
        assert_eq!(settings.map_history_size, 1);
        adjust(&mut settings, 4, 1);
        assert_eq!(settings.map_aggregation, Aggregation::Median);

        settings.history_size = 1;
        adjust(&mut settings, 0, -1);
        assert_eq!(settings.history_size, 1);
        settings.poll_interval_ms = 10;
        adjust(&mut settings, 1, -1);
        assert_eq!(settings.poll_interval_ms, 10);
    }
}
