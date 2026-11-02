//! Reading another process's memory.
//!
//! Everything above this module only sees the [`MemSource`] trait: "read N
//! bytes at address X" and "list the memory regions". The real osu! process
//! ([`ProcessMemory`]) and the test fake ([`fake::FakeMem`]) both implement it,
//! so the osu!-specific code can be tested without the game running.

mod live;
mod scan;

pub use live::ProcessMemory;
pub use scan::{Pattern, find_pattern, find_patterns};

use std::{io, path::PathBuf};

/// One mapped range of the target's address space.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Region {
    pub start: u64,
    pub end: u64,
    pub readable: bool,
    pub executable: bool,
    pub name: Option<PathBuf>,
}

pub trait MemSource {
    /// Fill `buf` with the bytes starting at `addr`.
    fn read(&self, addr: u64, buf: &mut [u8]) -> io::Result<()>;
    /// All mapped regions, used by the signature scan.
    fn regions(&self) -> io::Result<Vec<Region>>;
}

// Typed reads. osu! stable is a 32-bit little-endian .NET program, so
// pointers are 4 bytes even though this program is 64-bit.

pub fn read_u32(source: &impl MemSource, addr: u64) -> io::Result<u32> {
    let mut bytes = [0; 4];
    source.read(addr, &mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

pub fn read_i32(source: &impl MemSource, addr: u64) -> io::Result<i32> {
    let mut bytes = [0; 4];
    source.read(addr, &mut bytes)?;
    Ok(i32::from_le_bytes(bytes))
}

pub fn read_f32(source: &impl MemSource, addr: u64) -> io::Result<f32> {
    let mut bytes = [0; 4];
    source.read(addr, &mut bytes)?;
    Ok(f32::from_le_bytes(bytes))
}

pub fn read_f64(source: &impl MemSource, addr: u64) -> io::Result<f64> {
    let mut bytes = [0; 8];
    source.read(addr, &mut bytes)?;
    Ok(f64::from_le_bytes(bytes))
}

/// Read a 32-bit pointer stored at `addr`.
pub fn read_ptr(source: &impl MemSource, addr: u64) -> io::Result<u64> {
    Ok(u64::from(read_u32(source, addr)?))
}

#[cfg(test)]
pub mod fake {
    //! In-memory stand-in for a process, for tests.

    use std::{collections::BTreeMap, io};

    use super::{MemSource, Region};

    #[derive(Default)]
    pub struct FakeMem {
        bytes: BTreeMap<u64, u8>,
        pub regions: Vec<Region>,
    }

    impl FakeMem {
        pub fn write(&mut self, addr: u64, bytes: &[u8]) {
            for (offset, byte) in bytes.iter().enumerate() {
                self.bytes.insert(addr + offset as u64, *byte);
            }
        }

        pub fn put_u32(&mut self, addr: u64, value: u32) {
            self.write(addr, &value.to_le_bytes());
        }

        pub fn put_i32(&mut self, addr: u64, value: i32) {
            self.write(addr, &value.to_le_bytes());
        }
    }

    impl MemSource for FakeMem {
        fn read(&self, addr: u64, buf: &mut [u8]) -> io::Result<()> {
            for (offset, byte) in buf.iter_mut().enumerate() {
                *byte = *self.bytes.get(&(addr + offset as u64)).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::UnexpectedEof, "unmapped fake memory")
                })?;
            }
            Ok(())
        }

        fn regions(&self) -> io::Result<Vec<Region>> {
            Ok(self.regions.clone())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{fake::FakeMem, *};

    #[test]
    fn typed_reads_are_little_endian() {
        let mut memory = FakeMem::default();
        memory.put_u32(0x10, 0x2000);
        memory.put_i32(0x20, -15);
        memory.write(0x30, &8.5_f32.to_le_bytes());
        memory.write(0x40, &(-7.4_f64).to_le_bytes());
        assert_eq!(read_ptr(&memory, 0x10).unwrap(), 0x2000);
        assert_eq!(read_i32(&memory, 0x20).unwrap(), -15);
        assert_eq!(read_f32(&memory, 0x30).unwrap(), 8.5);
        assert_eq!(read_f64(&memory, 0x40).unwrap(), -7.4);
        assert!(read_i32(&memory, 0x99).is_err());
    }
}
