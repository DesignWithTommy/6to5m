//! Recovers texture dictionaries from an RPF9 by scanning for zlib-compressed
//! resource sections (Rpf9TextureRecovery.cs: FindCandidateOffsets +
//! RecoverAtOffsets). flate2 reports the exact packed length, so the C#
//! Adler-32 tail search is unnecessary.

use super::rage::{self, MAX_GRAPHICS_BYTES, MAX_SYSTEM_BYTES, TextureInfo};
use flate2::{Decompress, FlushDecompress, Status};

const DECODE_BUDGET: usize = 512 * 1024 * 1024;
const PAGE: usize = 4096;
pub const QUICK_SCAN_BYTES: usize = 16 * 1024 * 1024;
const QUICK_MAX_DICTIONARIES: usize = 32;

pub struct Dictionary {
    pub offset: usize,
    pub packed: usize,
    pub system: Vec<u8>,
    pub graphics: Vec<u8>,
    pub textures: Vec<TextureInfo>,
}

fn zlib_header(b: &[u8]) -> bool {
    b.len() >= 2 && b[0] & 15 == 8 && b[0] >> 4 <= 7 && (u16::from(b[0]) << 8 | u16::from(b[1])) % 31 == 0 && b[1] & 32 == 0
}

/// Inflates one stream, failing if it exceeds `maximum`. Returns (packed, data).
fn inflate(data: &[u8], at: usize, maximum: usize, budget: &mut usize) -> Option<(usize, Vec<u8>)> {
    if maximum < 1 || at + 6 > data.len() || *budget >= DECODE_BUDGET || !zlib_header(&data[at..]) {
        return None;
    }
    let mut z = Decompress::new(true);
    let mut out = Vec::new();
    loop {
        let read = z.total_in() as usize;
        out.reserve(64 * 1024);
        let before = out.len();
        let status = z.decompress_vec(&data[at + read..], &mut out, FlushDecompress::None).ok()?;
        *budget += out.len() - before;
        if out.len() > maximum || *budget > DECODE_BUDGET {
            return None;
        }
        match status {
            Status::StreamEnd => break,
            Status::Ok if z.total_in() as usize + at < data.len() || out.len() > before => {}
            _ => return None,
        }
    }
    (out.len() >= 8).then(|| (z.total_in() as usize, out))
}

fn looks_like_system(data: &[u8], at: usize) -> bool {
    let window = &data[at..data.len().min(at + 65536)];
    let mut z = Decompress::new(true);
    let mut prefix = [0u8; 64];
    let _ = z.decompress(window, &mut prefix, FlushDecompress::Sync);
    z.total_out() >= 64 && rage::looks_like_dictionary(&prefix)
}

fn zero(data: &[u8], from: usize, to: usize) -> bool {
    to >= from && to - from <= 15 && to + 6 <= data.len() && data[from..to].iter().all(|&b| b == 0)
}

pub fn candidates(data: &[u8]) -> Vec<usize> {
    // The sequential loop ran while page + 20 < len.
    let bound = data.len().saturating_sub(20);
    // Many small pieces keep every core busy; pieces are page-aligned.
    let region = (bound / (crate::par::threads() * 16)).clamp(1 << 20, 16 << 20).next_multiple_of(PAGE);
    let mut out = Vec::new();
    let is_candidate = |page: usize| {
        let at = page + 16;
        data[page..at].iter().all(|&b| b == 0) && zlib_header(&data[at..]) && looks_like_system(data, at)
    };
    crate::par::ordered(
        bound.div_ceil(region),
        crate::par::threads() * 4,
        |i| {
            let (from, to) = (i * region, ((i + 1) * region).min(bound));
            let mut found = Vec::new();
            for page in (from..to).step_by(PAGE) {
                if page % (4 << 20) == 0 && crate::progress::cancelled() {
                    break;
                }
                if is_candidate(page) {
                    found.push(page + 16);
                }
            }
            found
        },
        |i, found| {
            out.extend(found);
            !crate::progress::at(((i + 1) * region).min(bound))
        },
    );
    out
}

/// Recover every candidate in parallel and hand the dictionaries to `take`
/// in offset order, skipping candidates inside the previous one kept (as
/// the sequential loop did). `take` returns false to stop.
pub fn recover_each(data: &[u8], candidates: &[usize], mut take: impl FnMut(Dictionary) -> bool) {
    let mut end = 0;
    crate::par::ordered(
        candidates.len(),
        crate::par::threads() * 4,
        |i| recover_at(data, candidates[i]),
        |i, found| {
            if candidates[i] < end {
                return true;
            }
            match found {
                Some(d) => {
                    end = d.offset + d.packed;
                    take(d)
                }
                None => true,
            }
        },
    );
}

pub fn recover_at(data: &[u8], candidate: usize) -> Option<Dictionary> {
    if candidate < 16 || candidate + 6 > data.len() || data[candidate - 16..candidate].iter().any(|&b| b != 0) {
        return None;
    }
    let mut budget = 0;
    let (mut packed, mut system) = inflate(data, candidate, MAX_SYSTEM_BYTES, &mut budget)?;
    let mut end = candidate + packed;
    let mut blocks = 1;
    loop {
        let required = rage::minimum_system_bytes(&system)?;
        if required <= system.len() {
            break;
        }
        let next = end.next_multiple_of(16);
        if blocks >= 128 || !zero(data, end, next) {
            return None;
        }
        let (p, more) = inflate(data, next, MAX_SYSTEM_BYTES - system.len(), &mut budget)?;
        system.extend(more);
        end = next + p;
        blocks += 1;
    }
    rage::page_counts(&system)?;
    let required = rage::required_graphics_bytes(&system)?;
    let mut graphics = Vec::new();
    while graphics.len() < required {
        let next = end.next_multiple_of(16);
        let maximum = MAX_GRAPHICS_BYTES.checked_sub(system.len() + graphics.len())?;
        if maximum < 1 || blocks > 4095 || !zero(data, end, next) {
            return None;
        }
        let (p, more) = inflate(data, next, maximum, &mut budget)?;
        graphics.extend(more);
        end = next + p;
        blocks += 1;
    }
    let textures = rage::parse(&system, graphics.len()).ok()?;
    packed = end - candidate;
    Some(Dictionary { offset: candidate, packed, system, graphics, textures })
}

/// Scans the whole archive and returns every recoverable dictionary.
pub fn recover_all(data: &[u8]) -> Vec<Dictionary> {
    let mut out: Vec<Dictionary> = Vec::new();
    recover_each(data, &candidates(data), |d| {
        out.push(d);
        true
    });
    out
}

/// One system stream followed by one graphics stream at `at` (the layout
/// the quick header scan accepts; no zero-page prefix required).
pub fn recover_pair_at(data: &[u8], at: usize, budget: &mut usize) -> Option<Dictionary> {
    if !looks_like_system(data, at) {
        return None;
    }
    let (sp, system) = inflate(data, at, MAX_SYSTEM_BYTES, budget)?;
    let end = at + sp;
    let gat = end.next_multiple_of(16);
    if !zero(data, end, gat) {
        return None;
    }
    let (gp, graphics) = inflate(data, gat, MAX_GRAPHICS_BYTES, budget)?;
    if system.len() + graphics.len() > MAX_GRAPHICS_BYTES {
        return None;
    }
    let textures = rage::parse(&system, graphics.len()).ok()?;
    Some(Dictionary { offset: at, packed: gat + gp - at, system, graphics, textures })
}

/// Rpf9TextureRecovery.Recover: scans 16 MiB after the archive metadata
/// (header u32 at 4) on 16-byte steps, keeping at most 32 dictionaries.
pub fn recover_quick(data: &[u8]) -> Vec<Dictionary> {
    let mut out = Vec::new();
    let Some(meta_end) = crate::bytes::le32(data, 4).map(|v| v as usize) else { return out };
    if data.len() < 64 || meta_end < 64 || meta_end > data.len() || meta_end > QUICK_SCAN_BYTES {
        return out;
    }
    let start = meta_end.next_multiple_of(16);
    let end = data.len().min(start + QUICK_SCAN_BYTES);
    let mut budget = 0;
    let mut p = start;
    while p + 64 <= end && out.len() < QUICK_MAX_DICTIONARIES && budget < DECODE_BUDGET {
        if zlib_header(&data[p..])
            && let Some(d) = recover_pair_at(data, p, &mut budget)
        {
            p = (d.offset + d.packed).next_multiple_of(16);
            out.push(d);
            continue;
        }
        p += 16;
    }
    out
}

/// Recovers a dictionary at an offset taken from the index cache.
pub fn recover_cached(data: &[u8], at: usize) -> Option<Dictionary> {
    recover_at(data, at).or_else(|| recover_pair_at(data, at, &mut 0))
}

/// On-disk index of recovered dictionary offsets (RecoveryIndexCache.cs).
/// Stored under %LOCALAPPDATA%/rpf9/indexes (or $XDG_CACHE_HOME / ~/.cache);
/// valid only while the source path, length, mtime and header hash match.
pub mod cache {
    use crate::texture::defaults::sha256_hex;
    use std::{fs, path::{Path, PathBuf}, time::UNIX_EPOCH};

    const SCHEMA: &str = "rpf9-texture-index/1";

    fn root() -> Option<PathBuf> {
        let base = std::env::var_os("LOCALAPPDATA")
            .or_else(|| std::env::var_os("XDG_CACHE_HOME"))
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".cache")))?;
        Some(base.join("rpf9").join("indexes"))
    }

    struct Stamp {
        source: String,
        length: u64,
        mtime: u128,
        header: String,
    }

    fn stamp(source: &Path, data: &[u8]) -> Option<Stamp> {
        let full = fs::canonicalize(source).ok()?;
        let meta = fs::metadata(&full).ok()?;
        let mtime = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_nanos();
        Some(Stamp {
            source: full.to_string_lossy().into_owned(),
            length: meta.len(),
            mtime,
            header: sha256_hex(&data[..data.len().min(64)]),
        })
    }

    pub fn path(source: &Path, mode: &str) -> Option<PathBuf> {
        let full = fs::canonicalize(source).ok()?;
        let id = sha256_hex(full.to_string_lossy().to_uppercase().as_bytes());
        Some(root()?.join(format!("textures-{mode}-{}.idx", &id[..24])))
    }

    fn header(s: &Stamp, mode: &str) -> String {
        format!("{SCHEMA}\nsource {}\nlength {}\nmtime {}\nheader {}\nmode {mode}\n", s.source, s.length, s.mtime, s.header)
    }

    pub fn load(source: &Path, data: &[u8], mode: &str) -> Option<Vec<usize>> {
        let s = stamp(source, data)?;
        if s.length != data.len() as u64 {
            return None;
        }
        let text = fs::read_to_string(path(source, mode)?).ok()?;
        let body = text.strip_prefix(&header(&s, mode))?;
        let mut offsets = Vec::new();
        for line in body.lines() {
            let at = usize::from_str_radix(line.strip_prefix("0x")?, 16).ok()?;
            if at >= data.len() || offsets.last().is_some_and(|&p| at <= p) || offsets.len() >= 250_000 {
                return None;
            }
            offsets.push(at);
        }
        Some(offsets)
    }

    pub fn save(source: &Path, data: &[u8], mode: &str, offsets: &[usize]) -> Option<PathBuf> {
        let s = stamp(source, data)?;
        let target = path(source, mode)?;
        fs::create_dir_all(target.parent()?).ok()?;
        let mut text = header(&s, mode);
        for at in offsets {
            text.push_str(&format!("0x{at:X}\n"));
        }
        let temporary = target.with_extension(format!("{}.tmp", std::process::id()));
        fs::write(&temporary, text).ok()?;
        if fs::rename(&temporary, &target).is_err() {
            let _ = fs::remove_file(&temporary);
            return None;
        }
        Some(target)
    }
}