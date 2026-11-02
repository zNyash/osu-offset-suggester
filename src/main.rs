mod calc;
mod history;
mod settings;
mod storage;

use anyhow::Result;

use crate::{history::History, settings::Settings};

fn main() -> Result<()> {
    let settings_path = storage::settings_path()?;
    let history_path = storage::history_path()?;

    let settings = Settings::load_or_create(&settings_path)?;
    let history = History::load(&history_path, settings.history_size)?;

    println!("settings: {}", settings_path.display());
    println!("history:  {} ({} plays)", history_path.display(), history.plays().len());
    match history.suggestion() {
        Some(s) => println!("suggested global offset: {} ms (mean {:.2})", s.apply, s.mean),
        None => println!("no plays recorded yet"),
    }
    Ok(())
}
