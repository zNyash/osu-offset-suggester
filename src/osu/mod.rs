//! Reading osu! stable's state through pointer chains.
//!
//! Each method follows a chain from a base address found by
//! [`Signatures::scan`] and returns typed values. Reads are cheap and
//! independent, so the tracker only reads what it needs on each poll.
//! Offsets come from tosu and the old offset-suggester.

mod signatures;

pub use signatures::Signatures;

use anyhow::{Context, Result, anyhow, ensure};

use crate::memory::{MemSource, read_f32, read_f64, read_i32, read_ptr};

/// Game status values (tosu `GameState`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Menu,
    Playing,
    SongSelect,
    Results,
    Other(i32),
}

impl From<i32> for Status {
    fn from(value: i32) -> Self {
        match value {
            0 => Self::Menu,
            2 => Self::Playing,
            5 => Self::SongSelect,
            7 => Self::Results,
            other => Self::Other(other),
        }
    }
}

/// osu!standard; the hit-window formulas only apply to this mode.
pub const MODE_STANDARD: i32 = 0;

#[derive(Clone, Debug, PartialEq)]
pub struct Beatmap {
    pub checksum: String,
    /// "Artist - Title [Difficulty]".
    pub name: String,
    /// Overall difficulty, before mods.
    pub od: f32,
    /// Local (per-beatmap) offset.
    pub local_offset: i32,
}

/// The current play, readable while `Status::Playing`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Gameplay {
    pub mode: i32,
    pub mods: u32,
    /// Increases on every retry of the same map.
    pub retries: i32,
    pub failed: bool,
    /// How many hit errors the game has recorded so far.
    pub hit_error_count: usize,
}

/// .NET object layout (32-bit): an array's elements start after the
/// method-table pointer and the length.
const ARRAY_DATA: u64 = 0x8;
/// Hit errors are plain ms values; anything wilder is a torn read.
const MAX_HIT_ERROR_MS: i32 = 10_000;
const MAX_HIT_ERRORS: i32 = 65_535;
const MAX_CONFIG_ENTRIES: i32 = 512;

pub struct OsuReader<M: MemSource> {
    memory: M,
    signatures: Signatures,
    /// Index of the "Offset" entry in the config dictionary, once found.
    offset_index: Option<i32>,
}

impl<M: MemSource> OsuReader<M> {
    /// Scan for signatures (slow: once per osu! launch).
    pub fn attach(memory: M) -> Result<Self> {
        let signatures = Signatures::scan(&memory)?;
        Ok(Self::with_signatures(memory, signatures))
    }

    pub fn with_signatures(memory: M, signatures: Signatures) -> Self {
        Self {
            memory,
            signatures,
            offset_index: None,
        }
    }

    pub fn status(&self) -> Result<Status> {
        let status = self.ptr(self.signatures.status)?;
        Ok(Status::from(read_i32(&self.memory, status)?))
    }

    pub fn is_watching_replay(&self) -> Result<bool> {
        let flag = self.ptr(self.signatures.can_run_slowly + 0x46)?;
        if flag == 0 {
            return Ok(false);
        }
        let mut byte = [0];
        self.memory.read(flag, &mut byte)?;
        Ok(byte[0] == 1)
    }

    pub fn beatmap(&self) -> Result<Beatmap> {
        let mut last_error = anyhow!("no base signature");
        for &base in &self.signatures.base {
            match self.beatmap_at(base) {
                Ok(beatmap) => return Ok(beatmap),
                Err(error) => last_error = error,
            }
        }
        Err(last_error)
    }

    fn beatmap_at(&self, base: u64) -> Result<Beatmap> {
        let beatmap = self.non_null_ptr(self.non_null_ptr(base - 0xc)?)?;
        let checksum = self.string(beatmap + 0x6c)?;
        ensure!(!checksum.is_empty(), "beatmap has no checksum");
        let artist = self.string(beatmap + 0x18).unwrap_or_default();
        let title = self.string(beatmap + 0x24).unwrap_or_default();
        let difficulty = self.string(beatmap + 0xac).unwrap_or_default();
        let od = read_f32(&self.memory, beatmap + 0x38)?;
        ensure!((0.0..=11.0).contains(&od), "implausible OD {od}");
        Ok(Beatmap {
            checksum,
            name: format!("{artist} - {title} [{difficulty}]"),
            od,
            local_offset: read_i32(&self.memory, beatmap + 0x108)?,
        })
    }

    /// The play in progress. Errors when there is none.
    pub fn gameplay(&self) -> Result<Gameplay> {
        let gameplay = self.gameplay_base()?;
        let score = self.non_null_ptr(gameplay + 0x38)?;

        // Mods are stored XOR-obfuscated: two ints that XOR to the value.
        let mods_object = self.non_null_ptr(score + 0x1c)?;
        let mods =
            read_i32(&self.memory, mods_object + 0xc)? ^ read_i32(&self.memory, mods_object + 0x8)?;

        let hp_bar = self.non_null_ptr(gameplay + 0x40)?;
        let hp = read_f64(&self.memory, hp_bar + 0x1c)?;

        let stats = self.non_null_ptr(self.signatures.base[0] - 0x33)?;

        Ok(Gameplay {
            mode: read_i32(&self.memory, score + 0x64)?,
            mods: mods as u32,
            retries: read_i32(&self.memory, stats + 0x8)?,
            failed: hp <= 0.0,
            hit_error_count: self.hit_error_list()?.1,
        })
    }

    /// Hit errors recorded since index `from`, so each poll reads only the
    /// new ones.
    pub fn hit_errors_from(&self, from: usize) -> Result<Vec<i32>> {
        let (items, size) = self.hit_error_list()?;
        if from >= size {
            return Ok(Vec::new());
        }
        let mut bytes = vec![0; (size - from) * 4];
        self.memory
            .read(items + ARRAY_DATA + (from as u64) * 4, &mut bytes)?;
        let errors: Vec<i32> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|chunk| i32::from_le_bytes(*chunk))
            .collect();
        ensure!(
            errors.iter().all(|e| e.abs() <= MAX_HIT_ERROR_MS),
            "implausible hit error values (list changed mid-read?)"
        );
        Ok(errors)
    }

    /// Global offset from osu!'s config dictionary (stored as a double).
    pub fn global_offset(&mut self) -> Result<i32> {
        let manager = self.non_null_ptr(self.non_null_ptr(self.signatures.config)?)?;
        let table = self.ptr(manager + 0x8)?;
        let count = read_i32(&self.memory, manager + 0x1c)?;
        ensure!(
            (0..=MAX_CONFIG_ENTRIES).contains(&count),
            "implausible config size {count}"
        );

        if let Some(index) = self.offset_index.filter(|&i| i < count)
            && self.config_key(table, index).as_deref().ok() == Some("Offset")
        {
            return self.config_value(table, index);
        }
        for index in 0..count {
            if self.config_key(table, index).as_deref().ok() == Some("Offset") {
                self.offset_index = Some(index);
                return self.config_value(table, index);
            }
        }
        Err(anyhow!("Offset not found in osu! config"))
    }

    fn gameplay_base(&self) -> Result<u64> {
        let root = self.non_null_ptr(self.signatures.rulesets - 0xb)?;
        let ruleset = self.non_null_ptr(root + 0x4)?;
        self.non_null_ptr(ruleset + 0x64)
            .context("no gameplay in progress")
    }

    /// (array address, element count) of the score's hit-error `List<int>`.
    fn hit_error_list(&self) -> Result<(u64, usize)> {
        let score = self.non_null_ptr(self.gameplay_base()? + 0x38)?;
        let list = self.non_null_ptr(score + 0x38)?;
        let items = self.non_null_ptr(list + 0x4)?;
        let size = read_i32(&self.memory, list + 0xc)?;
        let capacity = read_i32(&self.memory, items + 0x4)?;
        ensure!(
            (0..=MAX_HIT_ERRORS).contains(&size) && size <= capacity,
            "implausible hit error list {size}/{capacity}"
        );
        Ok((items, size as usize))
    }

    // Config dictionary entries are 16 bytes each, starting at table + 0x8:
    // key string pointer, then the bindable holding the value.
    fn config_key(&self, table: u64, index: i32) -> Result<String> {
        self.string(table + 0x8 + index as u64 * 0x10)
    }

    fn config_value(&self, table: u64, index: i32) -> Result<i32> {
        let bindable = self.non_null_ptr(table + 0x8 + index as u64 * 0x10 + 0x4)?;
        let value = read_f64(&self.memory, bindable + 0x4)?;
        ensure!(
            value.is_finite() && value.abs() < 10_000.0,
            "implausible offset {value}"
        );
        Ok(value.round() as i32)
    }

    fn ptr(&self, addr: u64) -> Result<u64> {
        Ok(read_ptr(&self.memory, addr)?)
    }

    fn non_null_ptr(&self, addr: u64) -> Result<u64> {
        let value = self.ptr(addr)?;
        ensure!(value != 0, "null pointer at {addr:#x}");
        Ok(value)
    }

    /// A .NET string: pointer to [method table][length][UTF-16 chars].
    fn string(&self, field: u64) -> Result<String> {
        let string = self.non_null_ptr(field)?;
        let length = read_i32(&self.memory, string + 0x4)?;
        ensure!((0..=4096).contains(&length), "implausible string length");
        let mut bytes = vec![0; length as usize * 2];
        self.memory.read(string + 0x8, &mut bytes)?;
        let units: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect();
        String::from_utf16(&units).context("invalid UTF-16 string")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::fake::FakeMem;

    /// Fake memory with simple, made-up addresses for each object.
    struct World {
        memory: FakeMem,
        next_string: u64,
    }

    const BASE: u64 = 0x1_0000;
    const STATUS_PTR: u64 = 0x2_0000;
    const RULESETS: u64 = 0x3_0000;
    const REPLAY: u64 = 0x4_0000;
    const CONFIG: u64 = 0x5_0000;

    impl World {
        fn new() -> Self {
            Self {
                memory: FakeMem::default(),
                next_string: 0x90_0000,
            }
        }

        fn string_at(&mut self, field: u64, value: &str) {
            let address = self.next_string;
            self.next_string += 0x200;
            let units: Vec<u16> = value.encode_utf16().collect();
            self.memory.put_u32(field, address as u32);
            self.memory.put_i32(address + 4, units.len() as i32);
            let bytes: Vec<u8> = units.iter().flat_map(|u| u.to_le_bytes()).collect();
            self.memory.write(address + 8, &bytes);
        }

        fn reader(self) -> OsuReader<FakeMem> {
            OsuReader::with_signatures(
                self.memory,
                Signatures {
                    base: vec![BASE],
                    status: STATUS_PTR,
                    rulesets: RULESETS,
                    can_run_slowly: REPLAY,
                    config: CONFIG,
                },
            )
        }

        /// rulesets -> root -> ruleset -> gameplay -> score -> hit list.
        fn gameplay(&mut self, errors: &[i32], mods_value: u32, hp: f64) {
            let (root, ruleset, gameplay, score) = (0x10_0000, 0x11_0000, 0x12_0000, 0x13_0000);
            let (list, items, mods, hp_bar, stats) =
                (0x14_0000, 0x15_0000, 0x16_0000, 0x17_0000, 0x18_0000);
            let m = &mut self.memory;
            m.put_u32(RULESETS - 0xb, root);
            m.put_u32(root as u64 + 0x4, ruleset);
            m.put_u32(ruleset as u64 + 0x64, gameplay);
            m.put_u32(gameplay as u64 + 0x38, score);
            m.put_u32(gameplay as u64 + 0x40, hp_bar);
            m.write(hp_bar as u64 + 0x1c, &hp.to_le_bytes());
            m.put_i32(score as u64 + 0x64, MODE_STANDARD);
            // mods = 0xA5A5 ^ (0xA5A5 ^ value)
            m.put_u32(score as u64 + 0x1c, mods);
            m.put_u32(mods as u64 + 0x8, 0xA5A5);
            m.put_u32(mods as u64 + 0xc, 0xA5A5 ^ mods_value);
            m.put_u32(BASE - 0x33, stats);
            m.put_i32(stats as u64 + 0x8, 3);
            m.put_u32(score as u64 + 0x38, list);
            m.put_u32(list as u64 + 0x4, items);
            m.put_i32(list as u64 + 0xc, errors.len() as i32);
            m.put_i32(items as u64 + 0x4, 64);
            for (i, e) in errors.iter().enumerate() {
                m.put_i32(items as u64 + ARRAY_DATA + i as u64 * 4, *e);
            }
        }
    }

    #[test]
    fn reads_status_and_replay_flag() {
        let mut world = World::new();
        world.memory.put_u32(STATUS_PTR, 0x20_0000);
        world.memory.put_i32(0x20_0000, 2);
        world.memory.put_u32(REPLAY + 0x46, 0x21_0000);
        world.memory.write(0x21_0000, &[1]);
        let reader = world.reader();
        assert_eq!(reader.status().unwrap(), Status::Playing);
        assert!(reader.is_watching_replay().unwrap());
    }

    #[test]
    fn reads_beatmap() {
        let mut world = World::new();
        let (intermediate, beatmap) = (0x30_0000, 0x31_0000);
        world.memory.put_u32(BASE - 0xc, intermediate);
        world.memory.put_u32(intermediate as u64, beatmap);
        let beatmap = beatmap as u64;
        world.string_at(beatmap + 0x6c, "abc123");
        world.string_at(beatmap + 0x18, "Artist");
        world.string_at(beatmap + 0x24, "Song");
        world.string_at(beatmap + 0xac, "Insane");
        world.memory.write(beatmap + 0x38, &8.5_f32.to_le_bytes());
        world.memory.put_i32(beatmap + 0x108, -15);

        assert_eq!(
            world.reader().beatmap().unwrap(),
            Beatmap {
                checksum: "abc123".into(),
                name: "Artist - Song [Insane]".into(),
                od: 8.5,
                local_offset: -15,
            }
        );
    }

    #[test]
    fn reads_gameplay_with_deobfuscated_mods() {
        let mut world = World::new();
        world.gameplay(&[-5, 0, 8], 64 | 16, 0.0);
        let gameplay = world.reader().gameplay().unwrap();
        assert_eq!(
            gameplay,
            Gameplay {
                mode: MODE_STANDARD,
                mods: 64 | 16,
                retries: 3,
                failed: true,
                hit_error_count: 3,
            }
        );
    }

    #[test]
    fn reads_only_new_hit_errors() {
        let mut world = World::new();
        world.gameplay(&[-5, 0, 8, 12], 0, 1.0);
        let reader = world.reader();
        assert_eq!(reader.hit_errors_from(0).unwrap(), vec![-5, 0, 8, 12]);
        assert_eq!(reader.hit_errors_from(3).unwrap(), vec![12]);
        assert!(reader.hit_errors_from(4).unwrap().is_empty());
    }

    #[test]
    fn rejects_torn_hit_error_reads() {
        let mut world = World::new();
        world.gameplay(&[1, 50_000], 0, 1.0);
        assert!(world.reader().hit_errors_from(0).is_err());
    }

    #[test]
    fn reads_and_caches_global_offset() {
        let mut world = World::new();
        let (manager_ptr, manager, table, binding) = (0x40_0000, 0x41_0000, 0x42_0000, 0x43_0000);
        world.memory.put_u32(CONFIG, manager_ptr);
        world.memory.put_u32(manager_ptr as u64, manager);
        world.memory.put_u32(manager as u64 + 0x8, table);
        world.memory.put_i32(manager as u64 + 0x1c, 2);
        world.string_at(table as u64 + 0x8, "Volume");
        world.string_at(table as u64 + 0x18, "Offset");
        world.memory.put_u32(table as u64 + 0x18 + 0x4, binding);
        world
            .memory
            .write(binding as u64 + 0x4, &(-7.4_f64).to_le_bytes());

        let mut reader = world.reader();
        assert_eq!(reader.global_offset().unwrap(), -7);
        assert_eq!(reader.offset_index, Some(1));
        assert_eq!(reader.global_offset().unwrap(), -7);
    }
}
