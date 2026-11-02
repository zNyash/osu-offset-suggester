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
    settings::Settings,
    tracker::{Event, FinishedPlay, LivePlay, Outcome},
    ui,
    worker::{Command, Connection, Update, WorkerConfig},
};

/// How long to wait for a key press before checking worker updates.
const INPUT_TICK: Duration = Duration::from_millis(50);

pub struct App {
    pub settings: Settings,
    pub history: History,
    settings_path: PathBuf,
    history_path: PathBuf,

    pub connection: Connection,
    pub global_offset: Option<i32>,
    pub live: Option<LivePlay>,
    pub last_play: Option<FinishedPlay>,
    /// One-line feedback (last play result, save errors, ...).
    pub message: Option<String>,
    pub menu: Option<SettingsMenu>,

    commands: Sender<Command>,
    updates: Receiver<Update>,
    quit: bool,
}

/// The settings popup: edits a draft, applied on Enter.
pub struct SettingsMenu {
    pub draft: Settings,
    pub selected: usize,
}

pub const SETTING_COUNT: usize = 3;

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
            settings_path,
            history_path,
            connection: Connection::WaitingForOsu,
            global_offset: None,
            live: None,
            last_play: None,
            message: None,
            menu: None,
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
            Update::Live(live) => self.live = live,
            Update::Play(Event::Started { .. }) => {}
            Update::Play(Event::Finished(play)) => {
                self.message = Some(match &play.outcome {
                    Outcome::Recorded(record) => {
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
        match &mut self.menu {
            None => match key {
                KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
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
                KeyCode::Char(' ') if menu.selected == 2 => adjust(&mut menu.draft, 2, 1),
                KeyCode::Enter => {
                    let draft = menu.draft.clone();
                    self.menu = None;
                    self.apply_settings(draft);
                }
                _ => {}
            },
        }
    }

    fn apply_settings(&mut self, settings: Settings) {
        let shrunk = settings.history_size < self.history.plays().len();
        self.settings = settings;
        self.history.trim(self.settings.history_size);
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
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adjust_steps_and_clamps() {
        let mut settings = Settings::default();
        adjust(&mut settings, 0, 1);
        assert_eq!(settings.history_size, 51);
        adjust(&mut settings, 1, -1);
        assert_eq!(settings.poll_interval_ms, 90);
        adjust(&mut settings, 2, 1);
        assert!(settings.count_autopilot);

        settings.history_size = 1;
        adjust(&mut settings, 0, -1);
        assert_eq!(settings.history_size, 1);
        settings.poll_interval_ms = 10;
        adjust(&mut settings, 1, -1);
        assert_eq!(settings.poll_interval_ms, 10);
    }
}
