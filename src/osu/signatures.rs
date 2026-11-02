//! Base addresses found by signature scan, once per osu! launch.
//! Patterns and offsets come from tosu (`packages/tosu/src/memory/stable.ts`).

use anyhow::{Context, Result};

use crate::memory::{MemSource, Pattern, find_pattern, find_patterns};

#[derive(Clone, Debug, PartialEq)]
pub struct Signatures {
    /// Holds the current beatmap pointer (at -0xc) and play stats (at -0x33).
    pub base: Vec<u64>,
    /// Pointer to the game status.
    pub status: u64,
    /// Root of the gameplay objects (score, hit errors, HP).
    pub rulesets: u64,
    /// Pointer to the "watching a replay" flag (at +0x46).
    pub can_run_slowly: u64,
    /// Pointer to the config manager (global offset lives in its dictionary).
    pub config: u64,
}

const BASE: &str = "F8 01 74 04 83 65";
const STATUS: &str = "48 83 F8 04 73 1E";
const RULESETS: &str = "7D 15 A1 ?? ?? ?? ?? 85 C0";
const CAN_RUN_SLOWLY: &str = "55 8B EC 80 3D ?? ?? ?? ?? 00 75 26 80 3D";
const CONFIG: &str =
    "8D 45 EC 50 8B 0D ?? ?? ?? ?? 8B D7 39 09 E8 ?? ?? ?? ?? 85 C0 74 ?? 8B 4D EC";

/// The base pattern can match more than once; the reader tries each match
/// until one leads to a valid beatmap (same as the old offset-suggester).
const MAX_BASE_MATCHES: usize = 24;

impl Signatures {
    pub fn scan(source: &impl MemSource) -> Result<Self> {
        let base = find_patterns(source, &Pattern::parse(BASE)?, MAX_BASE_MATCHES)
            .context("base signature scan failed")?;
        anyhow::ensure!(!base.is_empty(), "base signature not found");
        Ok(Self {
            base,
            status: find(source, "status", STATUS)? - 0x4,
            rulesets: find(source, "rulesets", RULESETS)?,
            can_run_slowly: find(source, "replay", CAN_RUN_SLOWLY)?,
            config: find(source, "config", CONFIG)? + 0x6,
        })
    }
}

fn find(source: &impl MemSource, name: &str, pattern: &str) -> Result<u64> {
    find_pattern(source, &Pattern::parse(pattern)?)
        .with_context(|| format!("{name} signature not found (is osu! fully loaded?)"))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::memory::{Region, fake::FakeMem};

    /// Write each pattern into fake code memory, wildcards as 0x11.
    fn place(bytes: &mut [u8], offset: usize, pattern: &str) {
        for (i, token) in pattern.split_whitespace().enumerate() {
            bytes[offset + i] = u8::from_str_radix(token, 16).unwrap_or(0x11);
        }
    }

    #[test]
    fn resolves_all_signatures() {
        let start = 0x1000;
        let mut bytes = vec![0x90; 512];
        place(&mut bytes, 8, BASE);
        place(&mut bytes, 32, STATUS);
        place(&mut bytes, 64, RULESETS);
        place(&mut bytes, 128, CAN_RUN_SLOWLY);
        place(&mut bytes, 256, CONFIG);
        let mut memory = FakeMem::default();
        memory.regions.push(Region {
            start,
            end: start + bytes.len() as u64,
            readable: true,
            executable: true,
            name: Some(PathBuf::from("osu!.exe")),
        });
        memory.write(start, &bytes);

        let signatures = Signatures::scan(&memory).unwrap();
        assert_eq!(signatures.base, vec![start + 8]);
        assert_eq!(signatures.status, start + 32 - 0x4);
        assert_eq!(signatures.rulesets, start + 64);
        assert_eq!(signatures.can_run_slowly, start + 128);
        assert_eq!(signatures.config, start + 256 + 0x6);
    }
}
