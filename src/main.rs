mod calc;
mod history;
mod memory;
mod osu;
mod process;
mod settings;
mod storage;
mod tracker;

use std::{thread, time::Duration};

use anyhow::Result;

use crate::{
    history::History,
    memory::ProcessMemory,
    osu::OsuReader,
    settings::Settings,
    tracker::{Event, Outcome, Tracker},
};

fn main() -> Result<()> {
    let settings_path = storage::settings_path()?;
    let history_path = storage::history_path()?;

    let settings = Settings::load_or_create(&settings_path)?;
    let mut history = History::load(&history_path, settings.history_size)?;

    println!("settings: {}", settings_path.display());
    println!("history:  {} ({} plays)", history_path.display(), history.plays().len());
    match history.suggestion() {
        Some(s) => println!("suggested global offset: {} ms (mean {:.2})", s.apply, s.mean),
        None => println!("no plays recorded yet"),
    }

    // Temporary console loop until the worker and TUI exist (step 6).
    let Some(pid) = process::find_osu()? else {
        println!("osu! is not running");
        return Ok(());
    };
    let mut osu = OsuReader::attach(ProcessMemory::open(pid)?)?;
    println!("watching osu! (pid {pid}); Ctrl+C to stop");
    let mut tracker = Tracker::default();
    let interval = Duration::from_millis(settings.poll_interval_ms);
    while process::is_running(pid)? {
        for event in tracker.poll(&mut osu, settings.count_autopilot) {
            match event {
                Event::Started { map_name, mods } => println!("started: {map_name} (mods {mods})"),
                Event::Finished(play) => {
                    print!("ended: {} - {} hits, cutoff {} -> ", play.map_name, play.errors.len(), play.cutoff);
                    match play.outcome {
                        Outcome::Recorded(record) => {
                            println!(
                                "recorded M {:.1} UR {:.1} S {:.1}",
                                record.median,
                                record.unstable_rate,
                                record.suggestion()
                            );
                            history.push(record, settings.history_size);
                            history.save(&history_path)?;
                            if let Some(s) = history.suggestion() {
                                println!("  suggestion now: {} ms (mean {:.2}, {} plays)", s.apply, s.mean, history.plays().len());
                            }
                        }
                        Outcome::Skipped(reason) => println!("skipped ({reason:?})"),
                    }
                }
            }
        }
        thread::sleep(interval);
    }
    println!("osu! closed");
    Ok(())
}
