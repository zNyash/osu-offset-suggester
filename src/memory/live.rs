//! The real process, via `read-process-memory` (bytes) and `proc-maps`
//! (regions). Both crates pick the right OS API internally:
//! Linux `process_vm_readv` + `/proc/<pid>/maps`,
//! Windows `ReadProcessMemory` + `VirtualQueryEx`.

use std::io;

use read_process_memory::{CopyAddress, ProcessHandle};

use super::{MemSource, Region};

pub struct ProcessMemory {
    pid: proc_maps::Pid,
    handle: ProcessHandle,
}

impl ProcessMemory {
    /// Open a process for reading. On Windows this opens a process handle;
    /// on Linux it only stores the pid.
    pub fn open(pid: u32) -> io::Result<Self> {
        let invalid = || io::Error::new(io::ErrorKind::InvalidInput, "pid out of range");
        let maps_pid = proc_maps::Pid::try_from(pid).map_err(|_| invalid())?;
        let read_pid = read_process_memory::Pid::try_from(pid).map_err(|_| invalid())?;
        Ok(Self {
            pid: maps_pid,
            handle: ProcessHandle::try_from(read_pid)?,
        })
    }
}

impl MemSource for ProcessMemory {
    fn read(&self, addr: u64, buf: &mut [u8]) -> io::Result<()> {
        if buf.is_empty() {
            return Ok(());
        }
        let addr = usize::try_from(addr)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "address out of range"))?;
        self.handle.copy_address(addr, buf)
    }

    fn regions(&self) -> io::Result<Vec<Region>> {
        Ok(proc_maps::get_process_maps(self.pid)?
            .into_iter()
            .map(|range| Region {
                start: range.start() as u64,
                end: (range.start() + range.size()) as u64,
                readable: range.is_read(),
                executable: range.is_exec(),
                name: range.filename().map(|path| path.to_path_buf()),
            })
            .collect())
    }
}
