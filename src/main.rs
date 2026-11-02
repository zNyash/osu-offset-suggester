//! Entry point: load settings and history, start the worker, run the TUI.

mod app;
mod calc;
mod history;
mod map_history;
mod memory;
mod osu;
mod process;
mod settings;
mod storage;
mod tracker;
mod ui;
mod worker;

use std::sync::mpsc;

use anyhow::Result;

use crate::{app::App, history::History, settings::Settings};

fn main() -> Result<()> {
    let settings_path = storage::settings_path()?;
    let history_path = storage::history_path()?;
    let settings = Settings::load_or_create(&settings_path)?;
    let history = History::load(&history_path, settings.history_size)?;

    let (command_tx, command_rx) = mpsc::channel();
    let (update_tx, update_rx) = mpsc::channel();
    worker::spawn(app::worker_config(&settings), command_rx, update_tx);

    let app = App::new(
        settings,
        history,
        settings_path,
        history_path,
        command_tx,
        update_rx,
    );
    ratatui::run(|terminal| app.run(terminal))
}
