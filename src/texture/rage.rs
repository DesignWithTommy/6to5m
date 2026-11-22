//! RAGE texture dictionary (system + graphics sections) parsing, AMD detiling
//! and DDS creation. Port of RageTextureDecoder.cs.

use super::format::{self, R, dds_header, element_bytes, element_log2, is_bc};
use crate::bytes::{le16, le32, le64};

pub const MAX_SYSTEM_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_GRAPHICS_BYTES: usize = 256 * 1024 * 1024;
pub const MAX_EMBEDDED_SYSTEM_BYTES: usize = 32 * 1024 * 1024;
const SYS: u64 = 0x5000_0000;
const GFX: u64 = 0x6000_0000;

const PATTERN8: [i32; 12] = [0, 0, 0, 1, -1, -2, 2, 3, -3, 4, -4, 5];
const PATTERN16: [i32; 12] = [0, 0, 0, 0, -1, -2, 1, 2, -3, 3, -4, 4];
const PATTERN1: [i32; 12] = [1, 2, 3, 4, -1, -2, -3, -4, -5, 5, -6, 6];
const PATTERN2: [i32; 12] = [0, 1, 2, 3, -1, -2, -3, 4, -4, 5, -5, 6];
const PATTERN4: [i32; 12] = [0, 0, 1, 2, -1, -2, -3, 3, -4, 4, -5, 5];
pub const VOLUME_PATTERNS: [[i32; 12]; 5] = [
    [1, 2, 101, -1, 102, -2, 3, 103, -3, 4, 104, -4],
    [0, 1, 101, -1, 102, -2, 2, 103, -3, 3, 104, -4],
    [0, 0, 1, -1, 101, -2, 2, 102, -3, 3, 103, -4],
    [0, 0, 0, 1, 101, -1, 2, 102, -2, 3, 103, -3],
    [0, 0, 0, 0, 101, -1, 1, 102, -2, 2, 103, -3],
];

#[derive(Clone, Debug)]
pub struct TextureInfo {
    pub name: String,
    pub name_hash: u32,
    pub asset_key: Option<u64>,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub dimension: u32,
    pub format: u32,
    pub mips: u32,
    pub gfx_offset: usize,
    pub alloc_bytes: usize,
}

#[derive(Clone)]
struct Mip {
    width: usize,
    height: usize,
    padded_width: usize,
    offset: usize,
    tail_x: usize,
    tail_y: usize,
}

struct Layout {
    size: usize,
    bw: usize,
    bh: usize,
    bd: usize,
    z_stride: usize,
    volume: bool,
    mips: Vec<Mip>,
}

fn check(offset: usize, count: usize, len: usize) -> R<()> {
    if offset > len || count > len - offset { Err("resource range exceeds its section".into()) } else { Ok(()) }
}
fn u16at(d: &[u8], at: usize) -> R<usize> {
    le16(d, at).map(usize::from).ok_or_else(|| "truncated".into())
}
fn u32at(d: &[u8], at: usize) -> R<u32> {
    le32(d, at).ok_or_else(|| "truncated".into())
}
fn u64at(d: &[u8], at: usize) -> R<u64> {
    le64(d, at).ok_or_else(|| "truncated".into())
}
fn is_sys(v: u64) -> bool {
    (SYS..SYS + 0x0100_0000).contains(&v)
}
fn sys_offset(v: u64) -> R<usize> {
    if is_sys(v) { Ok((v - SYS) as usize) } else { Err("invalid system pointer".into()) }
}
fn pointer(d: &[u8], at: usize) -> R<usize> {
    let o = sys_offset(u64at(d, at)?)?;
    check(o, 1, d.len())?;
    Ok(o)
}

pub fn looks_like_dictionary(s: &[u8]) -> bool {
    if s.len() < 64 {
        return false;
    }
    let count = le16(s, 0x38).unwrap_or(0);
    count > 0
        && count <= 4096
        && le16(s, 0x3A).unwrap_or(0) >= count
        && le16(s, 0x28) == Some(count)
        && is_sys(le64(s, 0x20).unwrap_or(0))
        && is_sys(le64(s, 0x30).unwrap_or(0))
}

/// How many system bytes must be present before the whole dictionary can be
/// parsed (None if the prefix is not a plausible dictionary).
pub fn minimum_system_bytes(p: &[u8]) -> Option<usize> {
    let inner = || -> R<usize> {
        if !looks_like_dictionary(p) {
            return Err(String::new());
        }
        let count = u16at(p, 0x38)?;
        let page = sys_offset(u64at(p, 8)?)?;
        let hashes = sys_offset(u64at(p, 0x20)?)?;
        let ptrs = sys_offset(u64at(p, 0x30)?)?;
        let mut req = (page + 16).max(hashes + count * 4).max(ptrs + count * 8);
        if req > MAX_SYSTEM_BYTES {
            return Err(String::new());
        }
        if req > p.len() {
            return Ok(req);
        }
        for i in 0..count {
            let t = sys_offset(u64at(p, ptrs + i * 8)?)?;
            req = req.max(t + 0x70);
            if req > MAX_SYSTEM_BYTES {
                return Err(String::new());
            }
            if t + 0x70 > p.len() {
                continue;
            }
            let fmt = u32::from(p[t + 0x3F]);
            if !(1..=3).contains(&p[t + 0x3E])
                || p[t + 0x40] != 5
                || u16at(p, t + 0x0C)? != element_bytes(fmt)?
                || u64at(p, t + 0x28)? != SYS + t as u64 + 0x50
            {
                return Err(String::new());
            }
            let name = sys_offset(u64at(p, t + 0x30)?)? + 8;
            req = req.max(name + 1);
            if name < p.len() {
                let text = &p[name..p.len().min(name + 1024)];
                if !text.contains(&0) {
                    req = req.max(name + 1024);
                }
            }
        }
        Ok(req)
    };
    inner().ok().filter(|&r| r <= MAX_SYSTEM_BYTES)
}

pub fn page_counts(s: &[u8]) -> Option<(u8, u8)> {
    if !looks_like_dictionary(s) {
        return None;
    }
    let info = pointer(s, 8).ok()?;
    check(info, 16, s.len()).ok()?;
    let (sp, gp) = (s[info + 8], s[info + 9]);
    ((1..=32).contains(&sp) && (1..=32).contains(&gp)).then_some((sp, gp))
}

pub fn required_graphics_bytes(s: &[u8]) -> Option<usize> {
    let textures = parse(s, MAX_GRAPHICS_BYTES).ok()?;
    textures.iter().map(|t| t.gfx_offset + t.alloc_bytes).max().filter(|&r| r > 0)
}

pub fn parse(s: &[u8], gfx_len: usize) -> R<Vec<TextureInfo>> {
    parse_at(s, gfx_len, 0)
}

/// Parses a dictionary at `dict` inside a system section (0 for a standalone
/// dictionary, otherwise one embedded in a drawable resource).
pub fn parse_at(s: &[u8], gfx_len: usize, dict: usize) -> R<Vec<TextureInfo>> {
    let limit = if dict == 0 { MAX_SYSTEM_BYTES } else { MAX_EMBEDDED_SYSTEM_BYTES };
    if s.len() > limit
        || gfx_len > MAX_GRAPHICS_BYTES
        || s.len() < 64
        || dict > s.len() - 64
        || !looks_like_dictionary(&s[dict..])
    {
        return Err("not a supported bounded texture dictionary".into());
    }
    let count = u16at(s, dict + 0x38)?;
    let hashes = pointer(s, dict + 0x20)?;
    let ptrs = pointer(s, dict + 0x30)?;
    check(hashes, count * 4, s.len())?;
    check(ptrs, count * 8, s.len())?;
    let mut out = Vec::with_capacity(count);
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for i in 0..count {
        let t = pointer(s, ptrs + i * 8)?;
        check(t, 0x70, s.len())?;
        let desc = pointer(s, t + 0x30)?;
        check(desc, 8, s.len())?;
        let (name, asset_key);
        if u64at(s, desc)? == 0 {
            let start = desc + 8;
            check(start, 1, s.len())?;
            let span = &s[start..s.len().min(start + 1024)];
            let end = span.iter().position(|&b| b == 0).filter(|&e| e > 0).ok_or("invalid texture name")?;
            let text = std::str::from_utf8(&span[..end]).map_err(|_| "invalid texture name")?;
            if text.chars().any(char::is_control) {
                return Err("invalid texture name characters".into());
            }
            name = text.to_string();
            asset_key = None;
        } else {
            check(desc, 16, s.len())?;
            let (marker, key) = (u64at(s, desc)?, u64at(s, desc + 8)?);
            if desc < 16
                || marker != SYS + desc as u64 - 11
                || key == 0
                || u32at(s, desc - 16)? != 6
                || u32at(s, desc - 12)? != 0
                || u32at(s, desc - 4)? != 0
            {
                return Err("unsupported keyed texture descriptor".into());
            }
            name = format!("texture_{key:016X}");
            asset_key = Some(key);
        }
        let width = u16at(s, t + 0x38)? as u32;
        let height = u16at(s, t + 0x3A)? as u32;
        let fmt = u32::from(s[t + 0x3F]);
        let eb = element_bytes(fmt)?;
        let mips = u32::from(s[t + 0x42]);
        let depth = u16at(s, t + 0x3C)? as u32;
        let dimension = u32::from(s[t + 0x3E]);
        if !(1..=16384).contains(&width)
            || !(1..=16384).contains(&height)
            || !(1..=2048).contains(&depth)
            || !(1..=3).contains(&dimension)
            || (dimension == 2 && depth % 6 != 0)
            || s[t + 0x40] != 5
            || u16at(s, t + 0x0C)? != eb
            || u64at(s, t + 0x28)? != SYS + t as u64 + 0x50
        {
            return Err("unsupported texture descriptor".into());
        }
        let gp = u64at(s, t + 0x18)?;
        if gp < GFX || gp - GFX > i32::MAX as u64 {
            return Err("invalid graphics pointer".into());
        }
        let gfx = (gp - GFX) as usize;
        let len = u32at(s, t + 8)? as usize * eb;
        check(gfx, len, gfx_len)?;
        let info = TextureInfo {
            name,
            name_hash: u32at(s, hashes + i * 4)?,
            asset_key,
            width,
            height,
            depth,
            dimension,
            format: fmt,
            mips,
            gfx_offset: gfx,
            alloc_bytes: len,
        };
        let layout = layout(&info)?;
        if layout.size * if dimension == 3 { 1 } else { depth as usize } != len {
            return Err("descriptor allocation disagrees with AMD mip layout".into());
        }
        if ranges.iter().any(|&(a, b)| gfx < b && a < gfx + len) {
            return Err("overlapping texture allocations".into());
        }
        ranges.push((gfx, gfx + len));
        out.push(info);
    }
    Ok(out)
}

fn align(v: usize, b: usize) -> usize {
    v.div_ceil(b) * b
}
fn elements(pixels: u32, mip: u32, px: u32) -> usize {
    (pixels >> mip).max(1).div_ceil(px) as usize
}

fn layout(t: &TextureInfo) -> R<Layout> {
    let (w, h, mips, depth, volume) = (t.width, t.height, t.mips as usize, t.depth as usize, t.dimension == 3);
    let eb = element_bytes(t.format)?;
    let px = if is_bc(t.format) { 4 } else { 1 };
    let mut max_mips = 1;
    let mut size = w.max(h).max(if volume { t.depth } else { 1 });
    while size > 1 {
        size >>= 1;
        max_mips += 1;
    }
    if mips < 1 || mips > max_mips {
        return Err("invalid mip count".into());
    }
    let log = element_log2(eb)?;
    let (bw, bh, bd) = if volume {
        [(16, 16, 16), (8, 16, 16), (8, 16, 8), (8, 8, 8), (4, 8, 8)][log]
    } else {
        [(64, 64, 1), (64, 32, 1), (32, 32, 1), (32, 16, 1), (16, 16, 1)][log]
    };
    let (mw, mh) = if volume {
        [(8, 4), (4, 4), (4, 4), (4, 2), (2, 2)][log]
    } else {
        [(16, 16), (16, 8), (8, 8), (8, 4), (4, 4)][log]
    };
    let (ew, eh) = (w.div_ceil(px) as usize, h.div_ceil(px) as usize);
    let (tail_w, tail_h, max_tail) = if volume { (bw, bh / 2, 5) } else { (bw / 2, bh, 8) };
    let mut tail = mips;
    let (mut ls, mut sizes) = (Vec::new(), Vec::new());
    for m in 0..mips {
        let wc = ((ew + (1 << m) - 1) >> m).max(1);
        let hc = ((eh + (1 << m) - 1) >> m).max(1);
        if mips > 1 && wc <= tail_w && hc <= tail_h && mips - m <= max_tail {
            tail = m;
            break;
        }
        let (pw, ph) = (align(wc, bw), align(hc, bh));
        sizes.push(pw * ph * eb);
        ls.push(Mip {
            width: elements(w, m as u32, px),
            height: elements(h, m as u32, px),
            padded_width: pw,
            offset: 0,
            tail_x: 0,
            tail_y: 0,
        });
    }
    let mut off = if tail < mips { 4096 } else { 0 };
    for m in (0..tail).rev() {
        ls[m].offset = off;
        off += sizes[m] * bd;
    }
    for m in tail..mips {
        let entry = max_tail - 1 - (m - tail);
        let moff = if entry > 6 { 16 << entry } else { entry << 8 };
        let (mut mx, mut my) = (0, 0);
        for bit in 0..6 {
            mx |= ((moff >> (9 + 2 * bit)) & 1) << bit;
            my |= ((moff >> (8 + 2 * bit)) & 1) << bit;
        }
        ls.push(Mip {
            width: elements(w, m as u32, px),
            height: elements(h, m as u32, px),
            padded_width: bw,
            offset: 0,
            tail_x: mx * mw,
            tail_y: my * mh,
        });
    }
    let size = if volume { off / bd * align(depth, bd) } else { off };
    if size > MAX_GRAPHICS_BYTES {
        return Err("texture layout exceeds the output limit".into());
    }
    Ok(Layout { size, bw, bh, bd, z_stride: if volume { off } else { 0 }, volume, mips: ls })
}

fn tiled_offset(l: &Layout, m: &Mip, eb: usize, x: usize, y: usize, slice: usize) -> R<usize> {
    let pattern = if l.volume {
        &VOLUME_PATTERNS[element_log2(eb)?]
    } else {
        match eb {
            1 => &PATTERN1,
            2 => &PATTERN2,
            4 => &PATTERN4,
            8 => &PATTERN8,
            _ => &PATTERN16,
        }
    };
    let block = (y / l.bh) * (m.padded_width / l.bw) + x / l.bw;
    let (tx, ty) = (x + m.tail_x, y + m.tail_y);
    let mut address = 0;
    for (bit, &s) in pattern.iter().enumerate() {
        if s >= 101 {
            address |= ((slice >> (s - 101)) & 1) << bit;
        } else if s > 0 {
            address |= ((tx >> (s - 1)) & 1) << bit;
        } else if s < 0 {
            address |= ((ty >> (-s - 1)) & 1) << bit;
        }
    }
    Ok((slice / l.bd) * l.z_stride + m.offset + block * 4096 + address)
}

fn detile(alloc: &[u8], l: &Layout, eb: usize, level: usize, slice: usize) -> R<Vec<u8>> {
    let m = &l.mips[level];
    let mut linear = vec![0u8; m.width * m.height * eb];
    for y in 0..m.height {
        for x in 0..m.width {
            let o = tiled_offset(l, m, eb, x, y, slice)?;
            check(o, eb, alloc.len())?;
            let d = (y * m.width + x) * eb;
            linear[d..d + eb].copy_from_slice(&alloc[o..o + eb]);
        }
    }
    Ok(linear)
}

pub fn create_dds(t: &TextureInfo, gfx: &[u8]) -> R<Vec<u8>> {
    let eb = element_bytes(t.format)?;
    let l = layout(t)?;
    check(t.gfx_offset, t.alloc_bytes, gfx.len())?;
    let alloc = &gfx[t.gfx_offset..t.gfx_offset + t.alloc_bytes];
    let pitch = if is_bc(t.format) { l.mips[0].width * l.mips[0].height * eb } else { t.width as usize * eb };
    let mut out = Vec::new();
    dds_header(&mut out, t.width, t.height, t.depth, t.dimension, t.mips, t.format, pitch as u32);
    if t.dimension == 3 {
        for mip in 0..t.mips as usize {
            for z in 0..(t.depth as usize >> mip).max(1) {
                out.extend(detile(alloc, &l, eb, mip, z)?);
            }
        }
    } else {
        for slice in 0..t.depth as usize {
            let a = &alloc[slice * l.size..(slice + 1) * l.size];
            for mip in 0..t.mips as usize {
                out.extend(detile(a, &l, eb, mip, 0)?);
            }
        }
    }
    Ok(out)
}

/// Decodes slice 0 at the preview mip to RGBA. Returns (width, height, rgba).
pub fn decode_preview(t: &TextureInfo, gfx: &[u8]) -> R<(u32, u32, Vec<u8>)> {
    let level = format::choose_preview_mip(t.width, t.height, t.mips);
    let (w, h) = ((t.width >> level).max(1), (t.height >> level).max(1));
    if u64::from(w) * u64::from(h) > format::MAX_PREVIEW_PIXELS {
        return Err("no mip within the 16-megapixel preview limit".into());
    }
    let eb = element_bytes(t.format)?;
    let l = layout(t)?;
    check(t.gfx_offset, t.alloc_bytes, gfx.len())?;
    let mut alloc = &gfx[t.gfx_offset..t.gfx_offset + t.alloc_bytes];
    if t.dimension != 3 {
        alloc = &alloc[..l.size];
    }
    let linear = detile(alloc, &l, eb, level as usize, 0)?;
    Ok((w, h, format::decode_rgba(&linear, w, h, t.format)?))
}

/// Writes a replacement DDS into a copy of the graphics section, retiled into
/// the native AMD layout, and verifies the round trip
/// (RageTextureDecoder.RetileReplacement). Returns the staged graphics.
pub fn retile_replacement(system: &[u8], gfx: &[u8], index: usize, dds_bytes: &[u8]) -> R<Vec<u8>> {
    let textures = parse(system, gfx.len())?;
    let t = textures.get(index).ok_or("texture index out of range")?;
    let dds = super::dds::inspect(dds_bytes)?;
    if dds.width != t.width
        || dds.height != t.height
        || dds.depth != t.depth
        || dds.dimension != t.dimension
        || dds.mips != t.mips
        || dds.format != t.format
        || dds.resource_dimension != if t.dimension == 3 { 4 } else { 3 }
    {
        return Err("replacement DDS must match the native dimensions, format, mip count and shape exactly".into());
    }
    if dds.alpha_mode > 1 {
        return Err("replacement alpha mode must be unknown or straight".into());
    }
    let l = layout(t)?;
    let eb = element_bytes(t.format)?;
    let mut staged = gfx.to_vec();
    for mip in 0..t.mips {
        let slices = if t.dimension == 3 { (t.depth >> mip).max(1) } else { t.depth };
        let m = &l.mips[mip as usize];
        for slice in 0..slices {
            let linear = dds.plane(dds_bytes, mip, slice)?;
            let base = t.gfx_offset + if t.dimension == 3 { 0 } else { slice as usize * l.size };
            let limit = if t.dimension == 3 { t.alloc_bytes } else { l.size };
            for y in 0..m.height {
                for x in 0..m.width {
                    let at = tiled_offset(&l, m, eb, x, y, if t.dimension == 3 { slice as usize } else { 0 })?;
                    check(at, eb, limit)?;
                    let src = (y * m.width + x) * eb;
                    staged[base + at..base + at + eb].copy_from_slice(&linear[src..src + eb]);
                }
            }
        }
    }
    if create_dds(t, &staged)?[148..] != dds_bytes[dds.data_offset..] {
        return Err("native retile round trip did not reproduce the imported DDS payload".into());
    }
    Ok(staged)
}

/// A texture dictionary embedded in a drawable resource.
// Public API for the model decoder; not every item is used by the CLI.
#[allow(dead_code)]
pub struct Embedded {
    /// System offset of the dictionary.
    pub offset: usize,
    pub textures: Vec<TextureInfo>,
}

#[allow(dead_code)]
impl Embedded {
    /// Looks a texture up by its name hash (as referenced by shader params).
    pub fn by_hash(&self, hash: u32) -> Option<&TextureInfo> {
        self.textures.iter().find(|t| t.name_hash == hash)
    }
    pub fn by_name(&self, name: &str) -> Option<&TextureInfo> {
        self.textures.iter().find(|t| t.name.eq_ignore_ascii_case(name))
    }
}

fn sys_ptr(s: &[u8], at: usize, size: usize) -> R<usize> {
    let p = u64at(s, at)?;
    if p < SYS || p - SYS > MAX_EMBEDDED_SYSTEM_BYTES as u64 {
        return Err("invalid system pointer".into());
    }
    let o = (p - SYS) as usize;
    check(o, size, s.len())?;
    Ok(o)
}

/// Embedded dictionary of the drawable at system offset `drawable`
/// (drawable+16 -> shader group, group+8 -> dictionary), as in
/// RageModelDecoder.ReadEmbeddedTextures. For use by the model decoder.
pub fn embedded_dictionary(system: &[u8], graphics: &[u8], drawable: usize) -> Option<Embedded> {
    let group = sys_ptr(system, drawable + 16, 64).ok()?;
    if u64at(system, group + 8).ok()? == 0 {
        return None;
    }
    let offset = sys_ptr(system, group + 8, 64).ok()?;
    let textures = parse_at(system, graphics.len(), offset).ok()?;
    Some(Embedded { offset, textures })
}

/// Embedded dictionary when the dictionary's system offset is already known.
#[allow(dead_code)]
pub fn embedded_at(system: &[u8], graphics: &[u8], offset: usize) -> Option<Embedded> {
    parse_at(system, graphics.len(), offset).ok().map(|textures| Embedded { offset, textures })
}