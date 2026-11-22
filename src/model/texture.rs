//! Texture dictionary metadata needed by the model path (port of the sizing and
//! naming parts of RageTextureDecoder.cs): graphics-allocation size of every
//! texture (AMD tiled mip layout) and the name/hash table.

use crate::bytes::{le16, le32, le64};

const SYS: u64 = 0x5000_0000;
const GFX: u64 = 0x6000_0000;
const MAX_GRAPHICS: usize = 224 << 20;

pub struct TextureInfo {
    pub graphics: usize,
    pub bytes: usize,
}

fn element_bytes(format: u8) -> Option<usize> {
    Some(match format {
        61 | 65 => 1,
        49 | 54 | 56 => 2,
        24 | 28 | 29 | 34 | 41 | 87 | 88 | 91 | 93 => 4,
        10 | 11 | 71 | 72 | 80 | 81 => 8,
        74 | 75 | 77 | 78 | 83 | 84 | 95 | 96 | 98 | 99 => 16,
        _ => return None,
    })
}

fn is_bc(format: u8) -> bool {
    (70..=84).contains(&format) || (94..=99).contains(&format)
}

/// Total bytes of one tiled allocation (ComputeLayout's size).
fn layout_size(w: usize, h: usize, eb: usize, mips: usize, ppe: usize, depth: usize, volume: bool) -> Option<usize> {
    let mut max_mips = 1;
    let mut size = w.max(h).max(if volume { depth } else { 1 });
    while size > 1 {
        max_mips += 1;
        size >>= 1;
    }
    if mips < 1 || mips > max_mips {
        return None;
    }
    let log = match eb {
        1 => 0,
        2 => 1,
        4 => 2,
        8 => 3,
        16 => 4,
        _ => return None,
    };
    let (bw, bh, bd) = if volume {
        [(16, 16, 16), (8, 16, 16), (8, 16, 8), (8, 8, 8), (4, 8, 8)][log]
    } else {
        [(64, 64, 1), (64, 32, 1), (32, 32, 1), (32, 16, 1), (16, 16, 1)][log]
    };
    let (ew, eh) = (w.div_ceil(ppe), h.div_ceil(ppe));
    let (tail_w, tail_h, max_tail) = if volume { (bw, bh / 2, 5) } else { (bw / 2, bh, 8) };
    let mut tail = mips;
    let mut sizes = Vec::new();
    for m in 0..mips {
        let wc = ((ew + (1 << m) - 1) >> m).max(1);
        let hc = ((eh + (1 << m) - 1) >> m).max(1);
        if mips > 1 && wc <= tail_w && hc <= tail_h && mips - m <= max_tail {
            tail = m;
            break;
        }
        sizes.push(wc.next_multiple_of(bw).checked_mul(hc.next_multiple_of(bh))?.checked_mul(eb)?);
    }
    let mut off: usize = if tail < mips { 4096 } else { 0 };
    for m in (0..tail).rev() {
        off = off.checked_add(sizes[m].checked_mul(bd)?)?;
    }
    let bytes = if volume { (off / bd).checked_mul(depth.next_multiple_of(bd))? } else { off };
    (bytes <= MAX_GRAPHICS).then_some(bytes)
}

fn pointer(sys: &[u8], at: usize, size: usize) -> Option<usize> {
    let p = le64(sys, at)?;
    if !(SYS..SYS + 0x0100_0000).contains(&p) {
        return None;
    }
    let at = (p - SYS) as usize;
    (at.checked_add(size)? <= sys.len()).then_some(at)
}

pub fn looks_like_dictionary(sys: &[u8], at: usize) -> bool {
    let Some(d) = sys.get(at..at + 64) else { return false };
    let count = le16(d, 0x38).unwrap_or(0);
    let ptr = |o| le64(d, o).is_some_and(|p| (SYS..SYS + 0x0100_0000).contains(&p));
    (1..=4096).contains(&count) && le16(d, 0x3A).unwrap_or(0) >= count && le16(d, 0x28) == Some(count) && ptr(0x20) && ptr(0x30)
}

/// Strict validation and allocation layout of a texture dictionary at `dict` (RageTextureDecoder.Parse).
pub fn parse(sys: &[u8], graphics_len: usize, dict: usize) -> Option<Vec<TextureInfo>> {
    if !looks_like_dictionary(sys, dict) || graphics_len > MAX_GRAPHICS {
        return None;
    }
    let count = le16(sys, dict + 0x38)? as usize;
    let hashes = pointer(sys, dict + 0x20, count * 4)?;
    let pointers = pointer(sys, dict + 0x30, count * 8)?;
    let mut result: Vec<TextureInfo> = Vec::with_capacity(count);
    for i in 0..count {
        let t = pointer(sys, pointers + i * 8, 0x70)?;
        let descriptor = pointer(sys, t + 0x30, 8)?;
        if le64(sys, descriptor)? == 0 {
            let start = descriptor + 8;
            let text = sys.get(start..start + 1024.min(sys.len().saturating_sub(start)))?;
            let end = text.iter().position(|&b| b == 0).filter(|&e| e > 0)?;
            let name = std::str::from_utf8(&text[..end]).ok()?;
            if name.chars().any(char::is_control) {
                return None;
            }
        } else {
            let (marker, key) = (le64(sys, descriptor)?, le64(sys, descriptor + 8)?);
            if descriptor < 16
                || marker != SYS + descriptor as u64 - 11
                || key == 0
                || le32(sys, descriptor - 16)? != 6
                || le32(sys, descriptor - 12)? != 0
                || le32(sys, descriptor - 4)? != 0
            {
                return None;
            }
        }
        let (width, height) = (le16(sys, t + 0x38)? as usize, le16(sys, t + 0x3A)? as usize);
        let format = *sys.get(t + 0x3F)?;
        let eb = element_bytes(format)?;
        let mips = *sys.get(t + 0x42)? as usize;
        let depth = le16(sys, t + 0x3C)? as usize;
        let dimension = *sys.get(t + 0x3E)?;
        if !(1..=16384).contains(&width)
            || !(1..=16384).contains(&height)
            || !(1..=2048).contains(&depth)
            || !(1..=3).contains(&dimension)
            || (dimension == 2 && depth % 6 != 0)
            || *sys.get(t + 0x40)? != 5
            || le16(sys, t + 0x0C)? as usize != eb
            || le64(sys, t + 0x28)? != SYS + t as u64 + 0x50
        {
            return None;
        }
        let g = le64(sys, t + 0x18)?;
        if g < GFX || g - GFX > i32::MAX as u64 {
            return None;
        }
        let graphics = (g - GFX) as usize;
        let bytes = (le32(sys, t + 8)? as usize).checked_mul(eb)?;
        if graphics.checked_add(bytes)? > graphics_len {
            return None;
        }
        let volume = dimension == 3;
        let size = layout_size(width, height, eb, mips, if is_bc(format) { 4 } else { 1 }, depth, volume)?;
        if size.checked_mul(if volume { 1 } else { depth })? != bytes {
            return None;
        }
        if result.iter().any(|r| graphics < r.graphics + r.bytes && r.graphics < graphics + bytes) {
            return None;
        }
        le32(sys, hashes + i * 4)?;
        result.push(TextureInfo { graphics, bytes });
    }
    Some(result)
}

/// Graphics bytes the dictionary's pixel data occupies (TryGetRequiredGraphicsBytes).
pub fn required_graphics(sys: &[u8], dict: usize) -> Option<usize> {
    let textures = parse(sys, MAX_GRAPHICS, dict)?;
    textures.iter().map(|t| t.graphics + t.bytes).max().filter(|&r| r > 0)
}

/// Result of trying to read names from a possibly incomplete system segment.
pub enum Names {
    Done(Vec<(u32, String)>),
    Need,
    Bad,
}

/// Names and hashes of a standalone texture dictionary whose system segment may
/// still be partial (only the name strings are needed, not the pixel data).
pub fn names(sys: &[u8]) -> Names {
    if !looks_like_dictionary(sys, 0) {
        return Names::Bad;
    }
    let count = le16(sys, 0x38).unwrap_or(0) as usize;
    let off = |at| le64(sys, at).map(|p| p.wrapping_sub(SYS) as usize);
    let (Some(hashes), Some(pointers)) = (off(0x20), off(0x30)) else { return Names::Need };
    if hashes + count * 4 > sys.len() || pointers + count * 8 > sys.len() {
        return if hashes.max(pointers) < 32 << 20 { Names::Need } else { Names::Bad };
    }
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let Some(t) = off(pointers + i * 8).filter(|&t| t < 32 << 20) else { return Names::Bad };
        if t + 0x70 > sys.len() {
            return Names::Need;
        }
        let format = sys[t + 0x3F];
        if !(1..=3).contains(&sys[t + 0x3E])
            || sys[t + 0x40] != 5
            || element_bytes(format).map(|e| e as u16) != le16(sys, t + 0x0C)
            || le64(sys, t + 0x28) != Some(SYS + t as u64 + 0x50)
        {
            return Names::Bad;
        }
        let Some(descriptor) = off(t + 0x30).filter(|&d| d < 32 << 20) else { return Names::Bad };
        if descriptor + 16 > sys.len() {
            return Names::Need;
        }
        if le64(sys, descriptor) != Some(0) {
            continue; // keyed descriptor: no stored name
        }
        let text = &sys[descriptor + 8..];
        let Some(end) = text.iter().take(1024).position(|&b| b == 0) else {
            return if text.len() < 1024 { Names::Need } else { Names::Bad };
        };
        match std::str::from_utf8(&text[..end]) {
            Ok(name) if end > 0 && !name.chars().any(char::is_control) => {
                out.push((le32(sys, hashes + i * 4).unwrap_or(0), name.to_owned()));
            }
            _ => return Names::Bad,
        }
    }
    Names::Done(out)
}

/// Jenkins one-at-a-time hash of the lowercased name (RAGE joaat).
pub fn joaat(text: &str) -> u32 {
    let mut h: u32 = 0;
    for b in text.bytes() {
        h = h.wrapping_add(b.to_ascii_lowercase() as u32);
        h = h.wrapping_add(h << 10);
        h ^= h >> 6;
    }
    h = h.wrapping_add(h << 3);
    h ^= h >> 11;
    h.wrapping_add(h << 15)
}
