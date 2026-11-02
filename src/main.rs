mod calc;
mod history;
mod memory;
mod process;
mod settings;
mod storage;

use anyhow::Result;

use crate::{
    history::History,
    memory::{MemSource, ProcessMemory},
    settings::Settings,
};

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

    // Temporary smoke check until the worker and TUI exist.
    match process::find_osu()? {
        Some(pid) => {
            let memory = ProcessMemory::open(pid)?;
            let regions = memory.regions()?;
            let mut first = [0u8; 2];
            let readable = regions
                .iter()
                .find(|region| region.readable)
                .map(|region| memory.read(region.start, &mut first).is_ok());
            println!(
                "osu! found: pid {pid}, {} memory regions, read test: {}",
                regions.len(),
                if readable == Some(true) { "ok" } else { "FAILED" }
            );
        }
        None => println!("osu! is not running"),
    }
    Ok(())
}
