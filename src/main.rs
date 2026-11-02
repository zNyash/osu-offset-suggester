mod calc;
mod history;
mod memory;
mod osu;
mod process;
mod settings;
mod storage;

use anyhow::Result;

use crate::{
    history::History,
    memory::ProcessMemory,
    osu::OsuReader,
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
            let mut osu = OsuReader::attach(ProcessMemory::open(pid)?)?;
            println!("osu! found: pid {pid}");
            println!("  status:        {:?}", osu.status());
            println!("  replay:        {:?}", osu.is_watching_replay());
            println!("  global offset: {:?}", osu.global_offset());
            println!("  beatmap:       {:?}", osu.beatmap());
            match osu.gameplay() {
                Ok(play) => {
                    println!("  gameplay:      {play:?}");
                    let errors = osu.hit_errors_from(0)?;
                    println!("  hit errors:    {} (first: {:?})", errors.len(), &errors[..errors.len().min(10)]);
                }
                Err(error) => println!("  gameplay:      none ({error:#})"),
            }
        }
        None => println!("osu! is not running"),
    }
    Ok(())
}
