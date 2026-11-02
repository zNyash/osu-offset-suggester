//! Background thread that owns everything osu!-related: finding the
//! process, scanning signatures, and polling the tracker. It talks to the
//! TUI only through channels, so a slow scan never freezes the interface.

use std::{
    sync::mpsc::{Receiver, RecvTimeoutError, Sender},
    thread,
    time::{Duration, Instant},
};

use crate::{
    memory::ProcessMemory,
    osu::{Beatmap, OsuReader},
    process,
    tracker::{Event, LivePlay, Tracker},
};

/// Messages from the TUI to the worker.
pub enum Command {
    Configure(WorkerConfig),
    Quit,
}

#[derive(Clone, Copy, Debug)]
pub struct WorkerConfig {
    pub poll_interval: Duration,
    pub count_autopilot: bool,
}

/// Messages from the worker to the TUI.
pub enum Update {
    Connection(Connection),
    /// Current global offset in osu!.
    GlobalOffset(i32),
    /// The beatmap currently selected or playing in osu!.
    Beatmap(Option<Beatmap>),
    /// The play in progress (`None` when not playing).
    Live(Option<LivePlay>),
    Play(Event),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Connection {
    WaitingForOsu,
    Scanning(u32),
    Connected(u32),
    /// Attach failed; retrying.
    Failed(String),
}

/// How often to look for osu! when it is not running.
const SEARCH_INTERVAL: Duration = Duration::from_secs(2);
/// How often to check that osu! is still alive and re-read the global offset.
const SLOW_CHECK_INTERVAL: Duration = Duration::from_secs(1);

pub fn spawn(config: WorkerConfig, commands: Receiver<Command>, updates: Sender<Update>) {
    thread::spawn(move || {
        Worker {
            config,
            commands,
            updates,
        }
        .run()
    });
}

struct Worker {
    config: WorkerConfig,
    commands: Receiver<Command>,
    updates: Sender<Update>,
}

impl Worker {
    fn run(mut self) {
        loop {
            self.send(Update::Connection(Connection::WaitingForOsu));
            let pid = loop {
                if let Ok(Some(pid)) = process::find_osu() {
                    break pid;
                }
                if !self.wait(SEARCH_INTERVAL) {
                    return;
                }
            };

            self.send(Update::Connection(Connection::Scanning(pid)));
            let reader = ProcessMemory::open(pid)
                .map_err(anyhow::Error::from)
                .and_then(OsuReader::attach);
            match reader {
                Ok(reader) => {
                    self.send(Update::Connection(Connection::Connected(pid)));
                    if !self.watch(pid, reader) {
                        return;
                    }
                }
                Err(error) => {
                    // Usually osu! is still starting up; try again shortly.
                    self.send(Update::Connection(Connection::Failed(format!("{error:#}"))));
                    if !self.wait(SEARCH_INTERVAL) {
                        return;
                    }
                }
            }
        }
    }

    /// Poll one osu! session. Returns `false` when the app is quitting.
    fn watch(&mut self, pid: u32, mut reader: OsuReader<ProcessMemory>) -> bool {
        let mut tracker = Tracker::default();
        let mut last_slow_check: Option<Instant> = None;
        let mut last_offset = None;
        let mut last_beatmap: Option<Beatmap> = None;
        let mut last_live_len = None;

        loop {
            if last_slow_check.is_none_or(|at| at.elapsed() >= SLOW_CHECK_INTERVAL) {
                last_slow_check = Some(Instant::now());
                if !matches!(process::is_running(pid), Ok(true)) {
                    self.send(Update::Live(None));
                    self.send(Update::Beatmap(None));
                    return true; // osu! closed: go back to searching
                }
                if let Ok(offset) = reader.global_offset()
                    && last_offset != Some(offset)
                {
                    last_offset = Some(offset);
                    self.send(Update::GlobalOffset(offset));
                }
                // Also catches local offset changes made in osu!.
                let beatmap = reader.beatmap().ok();
                if beatmap != last_beatmap {
                    last_beatmap = beatmap.clone();
                    self.send(Update::Beatmap(beatmap));
                }
            }

            for event in tracker.poll(&mut reader, self.config.count_autopilot) {
                self.send(Update::Play(event));
            }

            // Only send the live play when it changed.
            let live_len = tracker.live_errors().map(<[i32]>::len);
            if live_len != last_live_len {
                last_live_len = live_len;
                self.send(Update::Live(tracker.live()));
            }

            if !self.wait(self.config.poll_interval) {
                return false;
            }
        }
    }

    /// Sleep for `duration`, waking early for commands. Returns `false` on quit.
    fn wait(&mut self, duration: Duration) -> bool {
        let deadline = Instant::now() + duration;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.commands.recv_timeout(remaining) {
                Ok(Command::Configure(config)) => self.config = config,
                Ok(Command::Quit) | Err(RecvTimeoutError::Disconnected) => return false,
                Err(RecvTimeoutError::Timeout) => return true,
            }
        }
    }

    fn send(&self, update: Update) {
        // The TUI only goes away when quitting; nothing to do if it's gone.
        let _ = self.updates.send(update);
    }
}
