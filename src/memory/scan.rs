//! Signature (pattern) scanning: find a known byte sequence in the target's
//! executable memory. osu! moves its objects every launch, but the machine
//! code that references them looks the same, so we search for that code and
//! read the addresses embedded in it.
//!
//! Patterns are written like `"7D 15 A1 ?? ?? ?? ?? 85 C0"`; `??` matches any byte.

use std::io;

use super::{MemSource, Region};

/// Read regions in chunks so a huge region never needs one huge buffer.
const CHUNK_SIZE: usize = 64 * 1024;

#[derive(Clone, Debug)]
pub struct Pattern {
    /// `None` = wildcard.
    bytes: Vec<Option<u8>>,
    /// Index of the first fixed byte, used as a quick pre-check.
    anchor: usize,
}

impl Pattern {
    pub fn parse(text: &str) -> io::Result<Self> {
        let invalid = |message: String| io::Error::new(io::ErrorKind::InvalidInput, message);
        let bytes = text
            .split_whitespace()
            .map(|token| match token {
                "??" => Ok(None),
                _ if token.len() == 2 => u8::from_str_radix(token, 16)
                    .map(Some)
                    .map_err(|_| invalid(format!("invalid pattern byte: {token}"))),
                _ => Err(invalid(format!("invalid pattern byte: {token}"))),
            })
            .collect::<io::Result<Vec<_>>>()?;
        let anchor = bytes
            .iter()
            .position(Option::is_some)
            .ok_or_else(|| invalid("pattern must contain a fixed byte".into()))?;
        Ok(Self { bytes, anchor })
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    fn matches(&self, candidate: &[u8]) -> bool {
        candidate.len() >= self.len()
            && self
                .bytes
                .iter()
                .zip(candidate)
                .all(|(expected, actual)| expected.is_none_or(|byte| byte == *actual))
    }
}

/// First match of `pattern`, or `NotFound`.
pub fn find_pattern(source: &impl MemSource, pattern: &Pattern) -> io::Result<u64> {
    find_patterns(source, pattern, 1)?
        .into_iter()
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "signature not found"))
}

/// Up to `limit` matches, searching osu!'s own regions first.
pub fn find_patterns(
    source: &impl MemSource,
    pattern: &Pattern,
    limit: usize,
) -> io::Result<Vec<u64>> {
    let mut regions: Vec<Region> = source
        .regions()?
        .into_iter()
        .filter(|region| region.readable && region.executable)
        .collect();
    regions.sort_by_key(region_priority);

    let mut matches = Vec::new();
    let mut first_error = None;
    for region in &regions {
        if matches.len() >= limit {
            break;
        }
        match scan_region(source, region, pattern, limit - matches.len()) {
            Ok(found) => matches.extend(found),
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }
    }
    match (matches.is_empty(), first_error) {
        // Nothing found and something failed: report why (e.g. permissions).
        (true, Some(error)) => Err(error),
        _ => Ok(matches),
    }
}

/// .NET JIT code lives in anonymous regions, so those come right after
/// regions named after osu!; shared libraries are searched last.
fn region_priority(region: &Region) -> u8 {
    let name = region
        .name
        .as_ref()
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if name.contains("osu!") {
        0
    } else if name.is_empty() || name.starts_with("[anon") || name.starts_with("[heap") {
        1
    } else {
        2
    }
}

fn scan_region(
    source: &impl MemSource,
    region: &Region,
    pattern: &Pattern,
    limit: usize,
) -> io::Result<Vec<u64>> {
    let anchor_byte = pattern.bytes[pattern.anchor].expect("anchor is a fixed byte");
    let mut matches = Vec::new();
    let mut last_error = None;
    // The tail of the previous chunk, so matches across chunk edges are found.
    let mut carry: Vec<u8> = Vec::new();
    let mut cursor = region.start;

    while cursor < region.end && matches.len() < limit {
        let len = (region.end - cursor).min(CHUNK_SIZE as u64) as usize;
        let mut chunk = vec![0; len];
        if let Err(error) = source.read(cursor, &mut chunk) {
            last_error = Some(error);
            carry.clear();
            cursor += len as u64;
            continue;
        }
        let window_start = cursor - carry.len() as u64;
        carry.extend_from_slice(&chunk);

        // memchr jumps straight to each occurrence of the anchor byte (SIMD),
        // instead of checking every position one by one.
        let last_start = carry.len().saturating_sub(pattern.len() - 1);
        for hit in memchr::memchr_iter(anchor_byte, &carry[pattern.anchor..]) {
            let start = hit;
            if start >= last_start {
                break;
            }
            if pattern.matches(&carry[start..]) {
                let address = window_start + start as u64;
                if !matches.contains(&address) {
                    matches.push(address);
                    if matches.len() >= limit {
                        break;
                    }
                }
            }
        }

        let keep = (pattern.len() - 1).min(carry.len());
        carry.drain(..carry.len() - keep);
        cursor += len as u64;
    }

    match (matches.is_empty(), last_error) {
        (true, Some(error)) => Err(error),
        _ => Ok(matches),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::memory::fake::FakeMem;

    fn code_region(start: u64, end: u64) -> Region {
        Region {
            start,
            end,
            readable: true,
            executable: true,
            name: Some(PathBuf::from("/games/osu!.exe")),
        }
    }

    #[test]
    fn finds_wildcard_pattern_across_a_chunk_boundary() {
        let base = 0x1000;
        let mut memory = FakeMem::default();
        memory.regions.push(code_region(base, base + 70_000));
        let mut bytes = vec![0x90; 70_000];
        // Starts 2 bytes before the 64 KiB chunk edge.
        bytes[65_534..65_539].copy_from_slice(&[0xAA, 0x11, 0xBB, 0xCC, 0xDD]);
        memory.write(base, &bytes);

        let pattern = Pattern::parse("AA ?? BB CC DD").unwrap();
        assert_eq!(find_pattern(&memory, &pattern).unwrap(), base + 65_534);
    }

    #[test]
    fn finds_multiple_matches_up_to_limit() {
        let base = 0x1000;
        let mut memory = FakeMem::default();
        memory.regions.push(code_region(base, base + 32));
        let mut bytes = vec![0; 32];
        for offset in [2, 10, 20] {
            bytes[offset] = 0xF8;
            bytes[offset + 1] = 0x01;
        }
        memory.write(base, &bytes);

        let pattern = Pattern::parse("F8 01").unwrap();
        assert_eq!(
            find_patterns(&memory, &pattern, 2).unwrap(),
            vec![base + 2, base + 10]
        );
    }

    #[test]
    fn skips_non_executable_regions() {
        let mut memory = FakeMem::default();
        memory.regions.push(Region {
            executable: false,
            ..code_region(0x1000, 0x1010)
        });
        memory.write(0x1000, &[0xAB; 16]);
        let pattern = Pattern::parse("AB AB").unwrap();
        assert_eq!(
            find_pattern(&memory, &pattern).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn rejects_bad_patterns() {
        assert!(Pattern::parse("?? ??").is_err());
        assert!(Pattern::parse("ZZ").is_err());
        assert!(Pattern::parse("ABC").is_err());
        assert!(Pattern::parse("").is_err());
    }
}
