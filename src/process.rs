//! Finding the osu! process. Works the same on Linux (Wine) and Windows:
//! in both cases the process is the Windows binary `osu!.exe`.

use anyhow::{Result, anyhow};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

const OSU_EXE: &str = "osu!.exe";

/// The pid of the running osu! stable, if any.
///
/// Under Wine a few helper processes can also mention `osu!.exe` (e.g. the
/// launcher), so the one using the most memory is the game itself.
pub fn find_osu() -> Result<Option<u32>> {
    let mut system = new_system()?;
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_memory(),
    );
    Ok(system
        .processes()
        .values()
        // On Linux sysinfo also lists threads, and osu!'s threads share its
        // name; keep only real processes.
        .filter(|process| process.thread_kind().is_none())
        .filter(|process| process.name().eq_ignore_ascii_case(OSU_EXE))
        .max_by_key(|process| process.memory())
        .map(|process| process.pid().as_u32()))
}

/// Whether the process with this pid still exists.
pub fn is_running(pid: u32) -> Result<bool> {
    let mut system = new_system()?;
    let pid = Pid::from_u32(pid);
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing(),
    );
    Ok(system.process(pid).is_some())
}

fn new_system() -> Result<System> {
    System::new().map_err(|error| anyhow!("failed to query processes: {error:?}"))
}
