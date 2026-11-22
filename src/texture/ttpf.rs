//! RDS5 / embedded ETPF634 streaming texture pools (TtpfReader.cs).

use super::format::{self, R, dds_header, element_bytes, element_log2, is_bc};
use crate::bytes::{le32, le64};
use std::collections::{HashMap, HashSet};

const MAX_OUTPUT: usize = 256 * 1024 * 1024;
const MAX_INDEX_BYTES: usize = 64 * 1024 * 1024;
const MAX_TEXTURES: usize = 250_000;

const PATTERNS: [&[i32]; 5] = [
    &[1, 2, 3, -2, -1, -3, 4, -4, -5, 5, -6, 6, -7, 7, -8, 8],
    &[0, 1, 2, 3, -1, -2, -3, 4, -4, 5, -5, 6, -6, 7, -7, 8],
    &[0, 0, 1, 2, -1, -2, -3, 3, -4, 4, -5, 5, -6, 6, -7, 7],
    &[0, 0, 0, 1, -1, 2, 3, -2, -3, 4, -4, 5, -5, 6, -6, 7],
    &[0, 0, 0, 0, 1, -1, 2, -2, -3, 3, -4, 4, -5, 5, -6, 6],
];
const FOURKB_PATTERNS: [&[i32]; 5] = [
    &[1, 2, 3, 4, -1, -2, -3, -4, -5, 5, -6, 6],
    &[0, 1, 2, 3, -1, -2, -3, 4, -4, 5, -5, 6],
    &[0, 0, 1, 2, -1, -2, -3, 3, -4, 4, -5, 5],
    &[0, 0, 0, 1, -1, -2, 2, 3, -3, 4, -4, 5],
    &[0, 0, 0, 0, -1, -2, 1, 2, -3, 3, -4, 4],
];

/// Chunk codec 1 (flags 0x04): Xbox BCPack holding BC1 blocks.
const CODEC_BC1: u32 = 1;

/// Decode a BCPack BC1 chunk to plain BC1 blocks (`blocks` x 8 bytes).
fn decode_bc1(comp: &[u8], blocks: usize) -> Option<Vec<u8>> {
    use bcpack::bc1::{QuadTable, Window};
    static TABLES: std::sync::OnceLock<(Window, QuadTable)> = std::sync::OnceLock::new();
    let (w, q) = TABLES.get_or_init(|| (Window::default_table(), QuadTable::default_table()));
    bcpack::bc1::decode(comp, blocks, w, q).ok().map(|o| o.blocks)
}

#[derive(Clone, Copy)]
pub struct Chunk {
    source: usize,
    packed: usize,
    virt: usize,
    size: usize,
    codec: u32,
    padding: usize,
}

#[derive(Clone)]
struct Mip {
    width: usize,
    height: usize,
    padded_width: usize,
    offset: usize,
    region: usize,
    tail_x: usize,
    tail_y: usize,
}

#[derive(Clone)]
struct Layout {
    size: usize,
    bw: usize,
    bh: usize,
    first_tail: usize,
    eb: usize,
    mips: Vec<Mip>,
    tile_bytes: usize,
    slice_bytes: usize,
    data_offset: usize,
    bd: usize,
    z_stride: usize,
    mip_major: bool,
    volume: bool,
}

pub struct Texture {
    pub index: usize,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub format: u32,
    pub mips: u32,
    pub source_mips: u32,
    pub base_mip: u32,
    pub depth: u32,
    pub dimension: u32,
    pub stored: usize,
    pub dds_size: usize,
    pub asset_key: u64,
    pub virt: usize,
    pub limitation: Option<String>,
    layout: Option<Layout>,
}

impl Texture {
    pub fn export_width(&self) -> u32 {
        (self.width >> self.base_mip).max(1)
    }
    pub fn export_height(&self) -> u32 {
        (self.height >> self.base_mip).max(1)
    }
    pub fn export_depth(&self) -> u32 {
        if self.dimension == 3 { (self.depth >> self.base_mip).max(1) } else { self.depth }
    }
}

pub struct Pool<'a> {
    pub data: &'a [u8],
    pub virtual_size: usize,
    chunks: Vec<Chunk>,
    pub textures: Vec<Texture>,
}

fn u32at(d: &[u8], at: usize) -> R<u32> {
    le32(d, at).ok_or_else(|| "truncated".into())
}
fn u64at(d: &[u8], at: usize) -> R<u64> {
    le64(d, at).ok_or_else(|| "truncated".into())
}
fn align(v: usize, b: usize) -> usize {
    v.div_ceil(b) * b
}
fn src_off(v: u64) -> usize {
    (v & 0x3FF_FFFF) as usize * 256
}
fn virt_off(v: u64) -> usize {
    ((v >> 30) & 0x3F_FFFF) as usize * 4096
}
fn elements(pixels: u32, mip: usize, px: u32) -> usize {
    (pixels >> mip).max(1).div_ceil(px) as usize
}

pub fn is_pool(data: &[u8]) -> bool {
    data.starts_with(b"RDS5")
}

fn chunks_for(chunks: &[Chunk], start: usize, count: usize) -> R<Vec<Chunk>> {
    let end = start + count;
    let mut covered = start;
    let lo = chunks.partition_point(|c| c.virt + c.size <= start);
    let mut out = Vec::new();
    for c in chunks[lo..].iter().take_while(|c| c.virt < end) {
        if c.virt > covered {
            return Err("texture crosses an unmapped virtual range".into());
        }
        out.push(*c);
        covered = end.min(c.virt + c.size);
    }
    if covered != end {
        return Err("texture range is not completely mapped".into());
    }
    Ok(out)
}

fn surface_layout(w: u32, h: u32, format: u32, mips: usize, tile_bytes: usize) -> R<Layout> {
    let mut levels = 1;
    let mut v = w.max(h);
    while v > 1 {
        v >>= 1;
        levels += 1;
    }
    if mips < 1 || mips > levels {
        return Err("invalid source mip count".into());
    }
    let eb = element_bytes(format)?;
    let log = element_log2(eb)?;
    let px = if is_bc(format) { 4 } else { 1 };
    let (half_bits, max_tail) = if tile_bytes == 4096 { (6, 8) } else { (8, 12) };
    let bw = 1usize << (half_bits - log / 2);
    let bh = 1usize << (half_bits - log.div_ceil(2));
    let (mw, mh) = [(16, 16), (16, 8), (8, 8), (8, 4), (4, 4)][log];
    let (ew, eh) = (w.div_ceil(px) as usize, h.div_ceil(px) as usize);
    let mut tail = mips;
    let (mut sizes, mut ms) = (Vec::new(), Vec::new());
    for i in 0..mips {
        let wc = ((ew + (1 << i) - 1) >> i).max(1);
        let hc = ((eh + (1 << i) - 1) >> i).max(1);
        if mips > 1 && wc <= bw / 2 && hc <= bh && mips - i <= max_tail {
            tail = i;
            break;
        }
        let size = align(wc, bw) * align(hc, bh) * eb;
        sizes.push(size);
        ms.push(Mip {
            width: elements(w, i, px),
            height: elements(h, i, px),
            padded_width: align(wc, bw),
            offset: 0,
            region: size,
            tail_x: 0,
            tail_y: 0,
        });
    }
    let mut off = if tail < mips { tile_bytes } else { 0 };
    for i in (0..tail).rev() {
        ms[i].offset = off;
        off += sizes[i];
    }
    for i in tail..mips {
        let entry = max_tail - 1 - (i - tail);
        let moff = if entry > 6 { 16 << entry } else { entry << 8 };
        let (mut x, mut y) = (0, 0);
        for bit in 0..6 {
            x |= ((moff >> (9 + 2 * bit)) & 1) << bit;
            y |= ((moff >> (8 + 2 * bit)) & 1) << bit;
        }
        ms.push(Mip {
            width: elements(w, i, px),
            height: elements(h, i, px),
            padded_width: bw,
            offset: 0,
            region: tile_bytes,
            tail_x: x * mw,
            tail_y: y * mh,
        });
    }
    if off < 1 || off > MAX_OUTPUT {
        return Err("texture exceeds the 256MiB resource limit".into());
    }
    Ok(Layout {
        size: off,
        bw,
        bh,
        first_tail: tail,
        eb,
        mips: ms,
        tile_bytes,
        slice_bytes: 0,
        data_offset: 0,
        bd: 1,
        z_stride: 0,
        mip_major: false,
        volume: false,
    })
}

fn array_layout(w: u32, h: u32, depth: usize, format: u32, mips: usize) -> R<Layout> {
    let mut l = surface_layout(w, h, format, mips, 65536)?;
    let mut off = if l.first_tail < mips { depth * 65536 } else { 0 };
    for i in (0..l.first_tail).rev() {
        l.mips[i].offset = off;
        off += depth * l.mips[i].region;
    }
    if off != l.size * depth || off > MAX_OUTPUT {
        return Err("texture exceeds the 256MiB resource limit".into());
    }
    l.size = off;
    l.mip_major = true;
    Ok(l)
}

fn volume_layout(w: u32, h: u32, depth: usize, format: u32) -> R<Layout> {
    let eb = element_bytes(format)?;
    let log = element_log2(eb)?;
    let px = if is_bc(format) { 4 } else { 1 };
    let (bw, bh, bd) = [(16, 16, 16), (8, 16, 16), (8, 16, 8), (8, 8, 8), (4, 8, 8)][log];
    let (ew, eh) = (w.div_ceil(px) as usize, h.div_ceil(px) as usize);
    let (pw, ph) = (align(ew, bw), align(eh, bh));
    let z_stride = pw * ph * eb * bd;
    let size = z_stride * (align(depth, bd) / bd);
    if size < 1 || size > MAX_OUTPUT {
        return Err("texture exceeds the 256MiB resource limit".into());
    }
    let mip = Mip { width: ew, height: eh, padded_width: pw, offset: 0, region: z_stride, tail_x: 0, tail_y: 0 };
    Ok(Layout {
        size,
        bw,
        bh,
        first_tail: 1,
        eb,
        mips: vec![mip],
        tile_bytes: 4096,
        slice_bytes: 0,
        data_offset: 0,
        bd,
        z_stride,
        mip_major: false,
        volume: true,
    })
}

fn mip_slice_offset(l: &Layout, base: usize, m: &Mip, slice: usize) -> usize {
    if l.volume {
        base + (slice / l.bd) * l.z_stride + m.offset
    } else if l.mip_major {
        base + m.offset + slice * m.region
    } else {
        base + l.data_offset + slice * l.slice_bytes + m.offset
    }
}

fn tiled_offset(l: &Layout, m: &Mip, x: usize, y: usize, slice: usize) -> R<usize> {
    let mut offset = ((y / l.bh) * (m.padded_width / l.bw) + x / l.bw) * l.tile_bytes;
    let log = element_log2(l.eb)?;
    let pattern: &[i32] = if l.volume {
        &super::rage::VOLUME_PATTERNS[log]
    } else if l.tile_bytes == 4096 {
        FOURKB_PATTERNS[log]
    } else {
        PATTERNS[log]
    };
    for (bit, &s) in pattern.iter().enumerate() {
        if s >= 101 {
            offset |= ((slice >> (s - 101)) & 1) << bit;
        } else if s > 0 {
            offset |= (((x + m.tail_x) >> (s - 1)) & 1) << bit;
        } else if s < 0 {
            offset |= (((y + m.tail_y) >> (-s - 1)) & 1) << bit;
        }
    }
    Ok(offset)
}

fn chunk_code_pages(code: u64) -> usize {
    match code {
        1..=8 => code as usize,
        10 => 12,
        11 => 16,
        12 => 24,
        13 => 32,
        14 => 48,
        15 => 64,
        16 => 128,
        21 => 18,
        22 => 80,
        23 => 96,
        24 => 144,
        26 => 272,
        27 => 384,
        28 => 528,
        _ => 0,
    }
}

fn decode_chunk_pages(meta: u32, flags: u32) -> Option<Vec<usize>> {
    if (flags >> 9) & 0x3FF != 0 {
        return None;
    }
    let packed = (u64::from(flags) << 11) | u64::from(meta);
    let (mut ended, mut out) = (false, Vec::new());
    for k in 0..4 {
        let code = (packed >> (5 * k)) & 0x1F;
        if code == 0 {
            ended = true;
            continue;
        }
        let pages = chunk_code_pages(code);
        if ended || pages == 0 {
            return None;
        }
        out.push(pages);
    }
    (!out.is_empty()).then_some(out)
}

fn validate_chunk_sizes(chunks: &[Chunk], base: usize, alloc: usize, pages: &[usize]) -> Option<Vec<Chunk>> {
    let mapped = chunks_for(chunks, base, alloc).ok()?;
    if mapped.len() != pages.len() || mapped[0].virt != base {
        return None;
    }
    let mut total = 0;
    for (c, &p) in mapped.iter().zip(pages) {
        if c.size != p * 4096 {
            return None;
        }
        total += p * 4096;
    }
    (total == alloc).then_some(mapped)
}

fn validate_compact(chunks: &[Chunk], base: usize, alloc: usize, mode: u32, non_tail: u32, meta: u32, flags: u32) -> bool {
    let Some(p) = decode_chunk_pages(meta, flags) else { return false };
    let (group, pad) = ((flags >> 19) & 3, (flags >> 21) as usize);
    let ok = match non_tail {
        1 => matches!(mode, 4 | 5 | 36) && group == 0 && pad == 0,
        3 => match group {
            1 => matches!(mode, 2 | 4) && p.len() == 3 && p[0] + pad == 16,
            2 => matches!(mode, 4 | 5 | 36) && p.len() >= 2 && p[0] + p[1] + pad == 16,
            3 => matches!(mode, 6 | 7 | 38) && p.len() >= 2 && p[0] + p[1] + pad == 8,
            _ => false,
        },
        _ => false,
    };
    ok && validate_chunk_sizes(chunks, base, alloc, &p).is_some()
}

#[allow(clippy::too_many_arguments)]
fn validate_cubemap(
    chunks: &[Chunk],
    base: usize,
    alloc: usize,
    non_tail: u32,
    meta: u32,
    flags: u32,
    faces: usize,
) -> Option<usize> {
    if flags >> 9 != 0 {
        return None;
    }
    let p = decode_chunk_pages(meta, flags)?;
    if p.len() != non_tail as usize + 1 {
        return None;
    }
    let mapped = validate_chunk_sizes(chunks, base, alloc, &p)?;
    let last_expected = if non_tail > 0 && faces >= 36 * 4096 { align(faces, 65536) } else { faces };
    let last = mapped.last()?;
    (last.size == last_expected).then(|| last.virt - base)
}

#[allow(clippy::too_many_arguments)]
fn legacy_top_only(
    w: u32,
    h: u32,
    format: u32,
    old_mips: usize,
    b: u32,
    c: u32,
    d: u32,
    metadata: usize,
    virtual_size: usize,
    chunks: &[Chunk],
) -> bool {
    if b >> 23 != 0 || (b >> 10) & 0x7FF != 0 || (b >> 21) & 3 != 1 || c >> 22 != 0 || d != 0 || old_mips <= 1 {
        return false;
    }
    let offset = (c & 0x3F_FFFF) as usize * 4096;
    if offset < metadata || offset >= virtual_size {
        return false;
    }
    let Ok(l) = surface_layout(w, h, format, old_mips, 65536) else { return false };
    if (b & 7) as usize != l.first_tail || l.size > virtual_size - offset {
        return false;
    }
    let plain = |cs: R<Vec<Chunk>>| cs.is_ok_and(|v| v.iter().all(|x| x.codec == 0 || x.codec == 8));
    if plain(chunks_for(chunks, offset, l.size)) {
        return false;
    }
    plain(chunks_for(chunks, offset + l.mips[0].offset, l.mips[0].region))
}

impl<'a> Pool<'a> {
    pub fn open(data: &'a [u8]) -> R<Self> {
        let size = data.len();
        if size < 256 || !data.starts_with(b"RDS5") {
            return Err("not an RDS5 container".into());
        }
        let count = u32at(data, 4)? as usize;
        let word = u64at(data, 8)?;
        let virtual_size = (word & 0x00FF_FFFF_FFFF_FFFF) as usize;
        let bitmap = u32at(data, 16)? as usize;
        if !(3..=1_000_002).contains(&count)
            || word >> 56 != 22
            || !(4096..=1 << 34).contains(&virtual_size)
            || bitmap < count.div_ceil(32)
            || bitmap > count.div_ceil(32) + 1
        {
            return Err("unsupported RDS5 index layout".into());
        }
        let table_end = 24 + (count - 1) * 8;
        let etpf = align(table_end, 256);
        if table_end > size - 16 || (count - 1) * 8 > MAX_INDEX_BYTES {
            return Err("RDS5 index exceeds its source or limit".into());
        }
        let table = &data[24..table_end];
        let entry = |i: usize| u64::from_le_bytes(table[i * 8..i * 8 + 8].try_into().unwrap());
        let last = entry(count - 2);
        if src_off(last) != size || virt_off(last) != virtual_size {
            return Err("RDS5 sentinel does not match its source/virtual length".into());
        }
        let mut chunks = Vec::with_capacity(count - 2);
        for i in 0..count - 2 {
            let (value, next) = (entry(i), entry(i + 1));
            let (start, end, v, ve) = (src_off(value), src_off(next), virt_off(value), virt_off(next));
            let pad = (value >> 52) as usize;
            let codec = ((value >> 26) & 15) as u32;
            let packed = end as i64 - start as i64 - pad as i64;
            let sz = ve as i64 - v as i64;
            if start < etpf + 16
                || end > size
                || packed < 1
                || packed > MAX_OUTPUT as i64
                || sz < 1
                || sz > MAX_OUTPUT as i64
                || ve > virtual_size
                || (codec == 0 && packed != sz)
            {
                return Err("RDS5 chunk range is invalid".into());
            }
            chunks.push(Chunk { source: start, packed: packed as usize, virt: v, size: sz as usize, codec, padding: pad });
        }
        let eh = data.get(etpf..etpf + 16).ok_or("truncated ETPF header")?;
        if &eh[..4] != b"ETPF" || u32at(eh, 4)? != 634 {
            return Err("unsupported embedded ETPF version".into());
        }
        let n = u32at(eh, 8)? as usize;
        let area = u32at(eh, 12)? as usize;
        let meta_bytes = 16 + area + n * 32;
        let padded_meta = align(meta_bytes, 4096);
        let align_word = u32at(data, 20)?;
        let src_pad = (align_word >> 28) as usize * 256;
        if n < 1
            || n > MAX_TEXTURES
            || area < n * 16
            || meta_bytes > MAX_INDEX_BYTES
            || align_word & 0x0FFF_FFFF != 0
            || chunks.is_empty()
            || padded_meta != chunks[0].virt
            || etpf + padded_meta + src_pad != chunks[0].source
        {
            return Err("ETPF tables do not match the source/virtual data boundary".into());
        }
        let records = data.get(etpf + 16..etpf + 16 + area).ok_or("truncated ETPF records")?;
        let ids = data.get(etpf + 16 + area..etpf + 16 + area + n * 32).ok_or("truncated ETPF ids")?;
        if n * 24 > records.len() + ids.len() {
            return Err("ETPF asset-key array exceeds its metadata".into());
        }
        let mut textures = Vec::with_capacity(n);
        for i in 0..n {
            let p = i * 16;
            let (a, b, c, d) = (u32at(records, p)?, u32at(records, p + 4)?, u32at(records, p + 8)?, u32at(records, p + 12)?);
            let non_tail = b & 7;
            let packed_mips = a >> 28;
            let mode = b >> 23;
            let width = (a & 0x3FFF) + 1;
            let height = ((a >> 14) & 0x3FFF) + 1;
            let mips = (packed_mips + non_tail) as usize;
            let format = (b >> 3) & 0x7F;
            let depth = ((b >> 10) & 0x7FF) + 1;
            let dimension = (b >> 21) & 3;
            let base = (c & 0x1F_FFFF) as usize * 4096;
            let id: String = ids[i * 32..i * 32 + 8].iter().map(|x| format!("{x:02X}")).collect();
            let key_at = n * 16 + i * 8;
            let asset_key = if key_at + 8 <= records.len() {
                u64at(records, key_at)?
            } else if key_at >= records.len() {
                u64at(ids, key_at - records.len())?
            } else {
                return Err("ETPF asset key straddles a metadata boundary".into());
            };
            let top_only = legacy_top_only(width, height, format, packed_mips as usize + 1, b, c, d, padded_meta, virtual_size, &chunks);
            let aux = (c >> 21) != 0 || d != 0;

            let inspect = || -> R<(Layout, u32, u32, usize, usize)> {
                if base == 0 || c & 0x1F_FFFF == 0x1F_FFFF {
                    return Err("no mapped allocation in this pool shard".into());
                }
                if base < padded_meta || base >= virtual_size {
                    return Err("texture virtual base exceeds the pool data".into());
                }
                let mut next_alloc = virtual_size;
                for f in i + 1..n {
                    let cand = (u32at(records, f * 16 + 8)? & 0x1F_FFFF) as usize * 4096;
                    if cand < base {
                        return Err("texture allocations are not ordered".into());
                    }
                    if cand > base {
                        next_alloc = cand;
                        break;
                    }
                }
                let span = next_alloc - base;
                if span < 1 || span > MAX_OUTPUT {
                    return Err("texture exceeds the 256MiB resource limit".into());
                }
                let layout = if dimension == 1 && depth == 1 {
                    let ok = if aux { matches!(mode, 2 | 4 | 5 | 6 | 7 | 36 | 38) } else { matches!(mode, 0 | 1 | 2 | 32 | 34) };
                    if !ok {
                        return Err(format!("compact mip mapping mode {mode} not validated"));
                    }
                    let l = surface_layout(width, height, format, mips, if mode & 6 == 0 { 65536 } else { 4096 })?;
                    if l.size != span {
                        return Err("footprint does not match its native allocation span".into());
                    }
                    if aux && !validate_compact(&chunks, base, l.size, mode, non_tail, c >> 21, d) {
                        return Err("auxiliary compact mip mapping is not decoded".into());
                    }
                    l
                } else if dimension == 1 && depth > 1 {
                    if mode != 1 || aux {
                        return Err("array layout not validated".into());
                    }
                    let l = array_layout(width, height, depth as usize, format, mips)?;
                    if l.size != span {
                        return Err("array footprint does not match its allocation span".into());
                    }
                    l
                } else if dimension == 2 {
                    if depth != 6 || width != height || non_tail > 3 || mode != 18 + 2 * non_tail {
                        return Err("cubemap layout not validated".into());
                    }
                    let face = surface_layout(width, height, format, mips, 4096)?;
                    let faces = face.size * depth as usize;
                    let off = if aux { validate_cubemap(&chunks, base, span, non_tail, c >> 21, d, faces) } else { None };
                    let off = off.ok_or("cubemap streaming layout not validated")?;
                    Layout { size: span, slice_bytes: face.size, data_offset: off, ..face }
                } else if dimension == 3 {
                    if mode != 2 || aux || depth < 2 || mips != 1 {
                        return Err("volume layout not validated".into());
                    }
                    let l = volume_layout(width, height, depth as usize, format)?;
                    if l.size != span {
                        return Err("volume footprint does not match its allocation span".into());
                    }
                    l
                } else {
                    return Err("array/volume layout not validated".into());
                };
                let slices_at = |mip: usize| if layout.volume { (depth as usize >> mip).max(1) } else { depth as usize };
                let step = if layout.volume { layout.bd } else { 1 };
                let mut available = vec![false; mips];
                for (mip, slot) in available.iter_mut().enumerate() {
                    let level = &layout.mips[mip];
                    let mut ok = true;
                    let mut s = 0;
                    while s < slices_at(mip) && ok {
                        let cs = chunks_for(&chunks, mip_slice_offset(&layout, base, level, s), level.region)?;
                        ok = cs.iter().all(|x| x.codec == 0 || x.codec == 8 || x.codec == CODEC_BC1);
                        s += step;
                    }
                    *slot = ok;
                }
                let base_mip = available.iter().position(|&x| x).ok_or("all source mips require Xbox BCPack")?;
                let mut exported = 0;
                if base_mip == 0 && top_only {
                    exported = 1;
                } else {
                    while base_mip + exported < mips && available[base_mip + exported] {
                        exported += 1;
                    }
                }
                let mut used = HashSet::new();
                let mut dds = 148;
                for mip in base_mip..base_mip + exported {
                    let level = &layout.mips[mip];
                    dds += level.width * level.height * layout.eb * slices_at(mip);
                    let mut s = 0;
                    while s < slices_at(mip) {
                        for ch in chunks_for(&chunks, mip_slice_offset(&layout, base, level, s), level.region)? {
                            used.insert((ch.source, ch.packed));
                        }
                        s += step;
                    }
                }
                if dds > MAX_OUTPUT {
                    return Err("DDS exceeds the 256MiB output limit".into());
                }
                let stored = used.iter().map(|x| x.1).sum();
                Ok((layout, base_mip as u32, exported as u32, stored, dds))
            };
            let suffix = if top_only { "_top_mip" } else { "" };
            let mut t = Texture {
                index: i,
                name: format!("texture_{i:06}_{id}{suffix}"),
                width,
                height,
                format,
                mips: 0,
                source_mips: mips as u32,
                base_mip: 0,
                depth,
                dimension,
                stored: 0,
                dds_size: 0,
                asset_key,
                virt: base,
                limitation: None,
                layout: None,
            };
            match inspect() {
                Ok((layout, base_mip, exported, stored, dds)) => {
                    (t.base_mip, t.mips, t.stored, t.dds_size, t.layout) = (base_mip, exported, stored, dds, Some(layout));
                }
                Err(e) => t.limitation = Some(e),
            }
            textures.push(t);
        }
        Ok(Self { data, virtual_size, chunks, textures })
    }

    pub(super) fn decode_chunk(&self, c: &Chunk) -> R<Vec<u8>> {
        let packed = self.data.get(c.source..c.source + c.packed).ok_or("chunk exceeds source")?;
        match c.codec {
            0 => Ok(packed.to_vec()),
            8 => {
                let mut z = flate2::Decompress::new(true);
                let mut out = Vec::with_capacity(c.size + 1);
                let status = z.decompress_vec(packed, &mut out, flate2::FlushDecompress::Finish).map_err(|e| e.to_string())?;
                if status != flate2::Status::StreamEnd || out.len() != c.size {
                    return Err("zlib output length disagrees with its chunk".into());
                }
                Ok(out)
            }
            CODEC_BC1 => decode_bc1(packed, c.size / 8).filter(|b| b.len() == c.size).ok_or_else(|| "BCPack BC1 chunk does not decode".into()),
            _ => Err("BCPack chunk in a format other than BC1 (not decoded yet)".into()),
        }
    }

    fn read_mip(&self, t: &Texture, mip: usize, slice: usize, cache: &mut HashMap<usize, Vec<u8>>) -> R<Vec<u8>> {
        let l = t.layout.as_ref().ok_or("layout unavailable")?;
        let m = &l.mips[mip];
        let start = mip_slice_offset(l, t.virt, m, slice);
        let mut tiled = vec![0u8; m.region];
        for c in chunks_for(&self.chunks, start, m.region)? {
            if !cache.contains_key(&c.source) {
                cache.insert(c.source, self.decode_chunk(&c)?);
            }
            let decoded = &cache[&c.source];
            let (from, to) = (start.max(c.virt), (start + m.region).min(c.virt + c.size));
            tiled[from - start..to - start].copy_from_slice(&decoded[from - c.virt..to - c.virt]);
        }
        let eb = l.eb;
        let mut linear = vec![0u8; m.width * m.height * eb];
        for y in 0..m.height {
            for x in 0..m.width {
                let o = tiled_offset(l, m, x, y, slice)?;
                if o + eb > tiled.len() {
                    return Err("tiled address exceeds its source range".into());
                }
                let d = (y * m.width + x) * eb;
                linear[d..d + eb].copy_from_slice(&tiled[o..o + eb]);
            }
        }
        Ok(linear)
    }

    pub fn create_dds(&self, t: &Texture) -> R<Vec<u8>> {
        if let Some(why) = &t.limitation {
            return Err(why.clone());
        }
        let l = t.layout.as_ref().ok_or("layout unavailable")?;
        let mut cache = HashMap::new();
        let mut out = Vec::with_capacity(t.dds_size);
        let bm = t.base_mip as usize;
        let top = &l.mips[bm];
        let pitch = if is_bc(t.format) { top.width * top.height * l.eb } else { t.export_width() as usize * l.eb };
        dds_header(&mut out, t.export_width(), t.export_height(), t.export_depth(), t.dimension, t.mips, t.format, pitch as u32);
        let range = bm..bm + t.mips as usize;
        if t.dimension == 3 {
            for mip in range {
                for z in 0..(t.depth as usize >> mip).max(1) {
                    out.extend(self.read_mip(t, mip, z, &mut cache)?);
                }
            }
        } else {
            for slice in 0..t.depth as usize {
                for mip in range.clone() {
                    out.extend(self.read_mip(t, mip, slice, &mut cache)?);
                }
            }
        }
        if out.len() != t.dds_size {
            return Err("generated DDS length disagrees with the inspected texture".into());
        }
        Ok(out)
    }

    pub fn decode_preview(&self, t: &Texture) -> R<(u32, u32, Vec<u8>)> {
        if let Some(why) = &t.limitation {
            return Err(why.clone());
        }
        let level = t.base_mip + format::choose_preview_mip(t.export_width(), t.export_height(), t.mips);
        let (w, h) = ((t.width >> level).max(1), (t.height >> level).max(1));
        let linear = self.read_mip(t, level as usize, 0, &mut HashMap::new())?;
        Ok((w, h, format::decode_rgba(&linear, w, h, t.format)?))
    }
}

const MAX_PHYSICAL: usize = 0x3FF_FFFF * 256;
const SOURCE_MASK: u64 = 0x3FF_FFFF;
const PADDING_MASK: u64 = 0xFFF << 52;

pub struct Replacement {
    pub changed_chunks: usize,
    pub changed_bytes: usize,
    pub source_bytes: usize,
    pub output_bytes: usize,
    pub samples_verified: usize,
}

impl Pool<'_> {
    /// Retiles a full-chain 2D DDS into the texture's native allocation
    /// (TtpfReader.RetileReplacement). Returns the tiled allocation.
    fn retile_replacement(&self, t: &Texture, dds_bytes: &[u8]) -> R<Vec<u8>> {
        let l = t.layout.as_ref().ok_or("texture layout is unavailable")?;
        let read_only = t.dimension != 1 || t.depth != 1;
        if t.limitation.is_some() || read_only || t.base_mip != 0 || t.mips != t.source_mips {
            return Err("pool replacement requires a complete supported 2D mip chain".into());
        }
        let dds = super::dds::inspect(dds_bytes)?;
        if dds.width != t.width
            || dds.height != t.height
            || dds.format != t.format
            || dds.mips != t.source_mips
            || dds.depth != 1
            || dds.dimension != 1
            || dds.resource_dimension != 3
            || dds.array_size != 1
            || dds.slices != 1
        {
            return Err("DDS dimensions, format, complete mip count and 2D shape must match the pool texture".into());
        }
        if dds.alpha_mode > 1 {
            return Err("replacement DDS requires unknown or straight alpha".into());
        }
        let mut tiled = vec![0u8; l.size];
        for c in chunks_for(&self.chunks, t.virt, l.size)? {
            let decoded = self.decode_chunk(&c)?;
            let (lo, hi) = (t.virt.max(c.virt), (t.virt + l.size).min(c.virt + c.size));
            tiled[lo - t.virt..hi - t.virt].copy_from_slice(&decoded[lo - c.virt..hi - c.virt]);
        }
        for mip in 0..t.source_mips {
            let m = &l.mips[mip as usize];
            let linear = dds.plane(dds_bytes, mip, 0)?;
            for y in 0..m.height {
                for x in 0..m.width {
                    let at = m.offset + tiled_offset(l, m, x, y, 0)?;
                    if at + l.eb > tiled.len() {
                        return Err("retiled address exceeds the native allocation".into());
                    }
                    let src = (y * m.width + x) * l.eb;
                    tiled[at..at + l.eb].copy_from_slice(&linear[src..src + l.eb]);
                }
            }
        }
        Ok(tiled)
    }

    /// Writes a copy of this pool to `dest` (a new file) with texture `index`
    /// replaced by `dds_bytes` (Rds5TextureWriter.ExportReplacementCopy).
    /// ETPF metadata and asset keys are kept; only affected chunks are
    /// recompressed and the RDS5 chunk table is rewritten. The copy is
    /// verified and only then moved into place.
    pub fn write_replacement(&self, index: usize, dds_bytes: &[u8], dest: &std::path::Path) -> R<Replacement> {
        use flate2::{Compression, write::ZlibEncoder};
        use std::io::Write;
        if dest.exists() {
            return Err("the replacement destination must be a new file".into());
        }
        let t = self.textures.get(index).ok_or("texture index out of range")?;
        let l = t.layout.as_ref().ok_or("texture layout is unavailable")?;
        let tiled = self.retile_replacement(t, dds_bytes)?;
        let dds = super::dds::inspect(dds_bytes)?;
        let data = self.data;
        let mut table = data[24..24 + (self.chunks.len() + 1) * 8].to_vec();
        let first = self.chunks[0].source;
        let mut out = data[..first].to_vec();
        let (mut changed_chunks, mut changed_bytes) = (0, 0);
        let mut affected = HashMap::new();
        for (i, c) in self.chunks.iter().enumerate() {
            let start = out.len();
            let mut padding = c.padding;
            let (lo, hi) = (t.virt.max(c.virt), (t.virt + tiled.len()).min(c.virt + c.size));
            let mut decoded = None;
            if lo < hi {
                let mut d = self.decode_chunk(c)?;
                let (src, dst) = (&tiled[lo - t.virt..hi - t.virt], &mut d[lo - c.virt..hi - c.virt]);
                let diff = src.iter().zip(dst.iter()).filter(|(a, b)| a != b).count();
                if diff > 0 {
                    changed_bytes += diff;
                    dst.copy_from_slice(src);
                    decoded = Some(d);
                }
            }
            match decoded {
                None => out.extend_from_slice(&data[c.source..c.source + c.packed + c.padding]),
                Some(d) => {
                    changed_chunks += 1;
                    match c.codec {
                        0 => out.extend_from_slice(&d),
                        8 => {
                            let mut z = ZlibEncoder::new(Vec::new(), Compression::best());
                            z.write_all(&d).map_err(|e| e.to_string())?;
                            out.extend(z.finish().map_err(|e| e.to_string())?);
                        }
                        _ => return Err("BCPack-dependent chunks cannot be replaced".into()),
                    }
                    let packed = out.len() - start;
                    let extra = (256 - ((packed + c.padding) & 255)) & 255;
                    padding = c.padding + extra;
                    if padding > 4095 {
                        return Err("recompressed chunk padding exceeds the 12-bit field".into());
                    }
                    out.extend_from_slice(&data[c.source + c.packed..c.source + c.packed + c.padding]);
                    out.resize(out.len() + extra, 0);
                    affected.insert(i, d);
                }
            }
            if start & 255 != 0 || out.len() & 255 != 0 || out.len() > MAX_PHYSICAL {
                return Err("rewritten chunk exceeds the 26-bit physical offset range or alignment".into());
            }
            let slot = &mut table[i * 8..i * 8 + 8];
            let mut value = u64::from_le_bytes(slot.try_into().unwrap());
            value = (value & !(SOURCE_MASK | PADDING_MASK)) | (start / 256) as u64 | ((padding as u64) << 52);
            slot.copy_from_slice(&value.to_le_bytes());
        }
        let last = table.len() - 8;
        let mut sentinel = u64::from_le_bytes(table[last..].try_into().unwrap());
        sentinel = (sentinel & !SOURCE_MASK) | (out.len() / 256) as u64;
        table[last..].copy_from_slice(&sentinel.to_le_bytes());
        out[24..24 + table.len()].copy_from_slice(&table);

        // Verify the copy before publishing it.
        let copy = Pool::open(&out).map_err(|e| format!("rewritten pool does not reopen: {e}"))?;
        if copy.virtual_size != self.virtual_size
            || copy.chunks.len() != self.chunks.len()
            || copy.textures.len() != self.textures.len()
        {
            return Err("rewritten pool changed the native virtual index shape".into());
        }
        if out[..24] != data[..24] || out[24 + table.len()..first] != data[24 + table.len()..first] {
            return Err("original header/ETPF metadata was not preserved".into());
        }
        for (i, (a, b)) in self.chunks.iter().zip(&copy.chunks).enumerate() {
            if a.virt != b.virt || a.size != b.size || a.codec != b.codec {
                return Err("rewritten chunk changed virtual range or codec".into());
            }
            if let Some(expected) = affected.get(&i)
                && copy.decode_chunk(b)? != *expected
            {
                return Err("affected chunk does not decode to its expected contents".into());
            }
        }
        if copy.create_dds(&copy.textures[index])?[148..] != dds_bytes[dds.data_offset..] {
            return Err("selected DDS payload does not reproduce the replacement exactly".into());
        }
        let (start, end) = (t.virt, t.virt + l.size);
        let mut samples: Vec<&Texture> = self
            .textures
            .iter()
            .filter(|n| n.index != index && n.limitation.is_none())
            .filter(|n| n.layout.as_ref().is_some_and(|nl| n.virt >= end || n.virt + nl.size <= start))
            .collect();
        samples.sort_by_key(|n| n.dds_size);
        let mut samples_verified = 0;
        for n in samples.into_iter().take(2) {
            if self.create_dds(n)? != copy.create_dds(&copy.textures[n.index])? {
                return Err("an unaffected texture sample changed in the pool copy".into());
            }
            samples_verified += 1;
        }
        let name = dest.file_name().ok_or("bad destination")?.to_string_lossy();
        let temporary = dest.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
        std::fs::write(&temporary, &out).map_err(|e| format!("{}: {e}", temporary.display()))?;
        if std::fs::read(&temporary).map_err(|e| e.to_string())? != out {
            let _ = std::fs::remove_file(&temporary);
            return Err("written pool copy does not match the verified bytes".into());
        }
        std::fs::rename(&temporary, dest).map_err(|e| {
            let _ = std::fs::remove_file(&temporary);
            e.to_string()
        })?;
        Ok(Replacement {
            changed_chunks,
            changed_bytes,
            source_bytes: data.len(),
            output_bytes: out.len(),
            samples_verified,
        })
    }
}