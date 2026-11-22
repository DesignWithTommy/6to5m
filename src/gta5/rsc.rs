//! GTA V RSC7 resource writer (port of CodeWalker's ResourceBuilder,
//! legacy/non-Gen9 layout).
//!
//! A resource is a graph of blocks. Each block lives in the system (virtual,
//! 0x50000000) or graphics (physical, 0x60000000) segment, has fixed bytes,
//! and a list of pointer slots that point at (block, offset). `build` packs
//! the blocks into pages, patches the pointers and writes the RSC7 file.

use flate2::{write::DeflateEncoder, Compression};
use std::io::Write;

pub const SYS_BASE: u64 = 0x5000_0000;
pub const GFX_BASE: u64 = 0x6000_0000;
const ALIGN: u64 = 16;
const SKIP: u64 = 16;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct BlockId(pub usize);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Seg {
    Sys,
    Gfx,
}

pub struct Block {
    pub seg: Seg,
    pub data: Vec<u8>,
    /// (byte offset in this block, target block, byte offset in target)
    pub ptrs: Vec<(usize, BlockId, u64)>,
}

#[derive(Default)]
pub struct Builder {
    pub blocks: Vec<Block>,
    /// Use CodeWalker's naive packing (required for Meta resources: ymap/ytyp).
    pub meta_packing: bool,
    /// ResourcePagesInfo block: page counts are patched into bytes 8/9 after packing.
    pub pages_info: Option<BlockId>,
}

/// Little-endian byte writer used to fill a block's bytes.
#[derive(Default, Clone)]
pub struct W {
    pub b: Vec<u8>,
    pub ptrs: Vec<(usize, BlockId, u64)>,
}

impl W {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn len(&self) -> usize {
        self.b.len()
    }
    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.b.push(v);
        self
    }
    pub fn u16(&mut self, v: u16) -> &mut Self {
        self.b.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.b.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn i32(&mut self, v: i32) -> &mut Self {
        self.b.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn u64(&mut self, v: u64) -> &mut Self {
        self.b.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn f32(&mut self, v: f32) -> &mut Self {
        self.b.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn v3(&mut self, v: [f32; 3]) -> &mut Self {
        self.f32(v[0]).f32(v[1]).f32(v[2])
    }
    pub fn v4(&mut self, v: [f32; 4]) -> &mut Self {
        self.f32(v[0]).f32(v[1]).f32(v[2]).f32(v[3])
    }
    pub fn bytes(&mut self, v: &[u8]) -> &mut Self {
        self.b.extend_from_slice(v);
        self
    }
    pub fn zeros(&mut self, n: usize) -> &mut Self {
        self.b.resize(self.b.len() + n, 0);
        self
    }
    /// Pad with zeros to a multiple of `a`.
    pub fn align(&mut self, a: usize) -> &mut Self {
        let n = (a - self.b.len() % a) % a;
        self.zeros(n)
    }
    /// An 8-byte pointer to (target, offset); `None` writes 0.
    pub fn ptr(&mut self, t: Option<BlockId>) -> &mut Self {
        self.ptr_at(t, 0)
    }
    pub fn ptr_at(&mut self, t: Option<BlockId>, off: u64) -> &mut Self {
        if let Some(t) = t {
            self.ptrs.push((self.b.len(), t, off));
        }
        self.u64(0)
    }
    /// Patch a u32 already written at `at`.
    pub fn put_u32(&mut self, at: usize, v: u32) {
        self.b[at..at + 4].copy_from_slice(&v.to_le_bytes());
    }
    pub fn put_u16(&mut self, at: usize, v: u16) {
        self.b[at..at + 2].copy_from_slice(&v.to_le_bytes());
    }
}

impl Builder {
    pub fn new() -> Self {
        Self::default()
    }
    /// Reserve a block id now and fill it later with `set` (lets blocks point
    /// at each other in any order). The root (first system block) must be
    /// reserved first.
    pub fn reserve(&mut self, seg: Seg) -> BlockId {
        self.blocks.push(Block { seg, data: Vec::new(), ptrs: Vec::new() });
        BlockId(self.blocks.len() - 1)
    }
    pub fn set(&mut self, id: BlockId, w: W) {
        let b = &mut self.blocks[id.0];
        b.data = w.b;
        b.ptrs = w.ptrs;
    }
    pub fn add(&mut self, seg: Seg, w: W) -> BlockId {
        let id = self.reserve(seg);
        self.set(id, w);
        id
    }
    /// Convenience: a system block holding raw bytes (arrays, strings).
    pub fn sys_bytes(&mut self, bytes: Vec<u8>) -> BlockId {
        self.add(Seg::Sys, W { b: bytes, ptrs: Vec::new() })
    }
    /// A NUL-terminated string block.
    pub fn string(&mut self, s: &str) -> BlockId {
        let mut b = s.as_bytes().to_vec();
        b.push(0);
        self.sys_bytes(b)
    }

    /// Pack, patch and write the RSC7 file. `version` is the resource type
    /// version (ytd 13, ydr 165, ybn 43, ymap/ytyp 2).
    pub fn build(self, version: u32) -> Result<Vec<u8>, String> {
        let sys: Vec<usize> = (0..self.blocks.len()).filter(|&i| self.blocks[i].seg == Seg::Sys).collect();
        let gfx: Vec<usize> = (0..self.blocks.len()).filter(|&i| self.blocks[i].seg == Seg::Gfx).collect();
        if sys.first() != Some(&0) {
            return Err("the root block must be the first system block".into());
        }
        let sizes: Vec<u64> = self.blocks.iter().map(|b| b.data.len() as u64).collect();
        let mut pos = vec![0u64; self.blocks.len()];
        let (sf, sys_size, sys_pages) = if self.meta_packing {
            assign_meta(&sys, &sizes, SYS_BASE, &mut pos)?
        } else {
            assign(&sys, &sizes, SYS_BASE, true, 128, &mut pos)?
        };
        let (gf, gfx_size, gfx_pages) = if self.meta_packing {
            assign_meta(&gfx, &sizes, GFX_BASE, &mut pos)?
        } else {
            assign(&gfx, &sizes, GFX_BASE, false, 128 - sys_pages, &mut pos)?
        };

        let mut blocks = self.blocks;
        if let Some(pi) = self.pages_info {
            let d = &mut blocks[pi.0].data;
            if d.len() >= 10 {
                d[8] = sys_pages as u8;
                d[9] = gfx_pages as u8;
            }
        }
        let mut sdata = vec![0u8; sys_size as usize];
        let mut gdata = vec![0u8; gfx_size as usize];
        for (i, b) in blocks.iter().enumerate() {
            let (buf, base) = match b.seg {
                Seg::Sys => (&mut sdata, SYS_BASE),
                Seg::Gfx => (&mut gdata, GFX_BASE),
            };
            let at = (pos[i] - base) as usize;
            buf[at..at + b.data.len()].copy_from_slice(&b.data);
            for &(off, t, toff) in &b.ptrs {
                let v = pos[t.0] + toff;
                buf[at + off..at + off + 8].copy_from_slice(&v.to_le_bytes());
            }
        }

        let sv = (version >> 4) & 0xF;
        let gv = version & 0xF;
        let mut raw = sdata;
        raw.extend_from_slice(&gdata);
        let mut enc = DeflateEncoder::new(Vec::new(), Compression::best());
        enc.write_all(&raw).map_err(|e| e.to_string())?;
        let comp = enc.finish().map_err(|e| e.to_string())?;
        let mut out = Vec::with_capacity(16 + comp.len());
        out.extend_from_slice(&0x3743_5352u32.to_le_bytes());
        out.extend_from_slice(&version.to_le_bytes());
        out.extend_from_slice(&(sf + (sv << 28)).to_le_bytes());
        out.extend_from_slice(&(gf + (gv << 28)).to_le_bytes());
        out.extend_from_slice(&comp);
        Ok(out)
    }
}

/// Page flags value -> total byte size of its pages.
pub fn flags_size(v: u32) -> u64 {
    let base = 0x200u64 << (v & 0xF);
    let counts = [
        (v >> 4) & 1,
        (v >> 5) & 3,
        (v >> 7) & 0xF,
        (v >> 11) & 0x3F,
        (v >> 17) & 0x7F,
        (v >> 24) & 1,
        (v >> 25) & 1,
        (v >> 26) & 1,
        (v >> 27) & 1,
    ];
    let shifts = [8, 7, 6, 5, 4, 3, 2, 1, 0];
    counts.iter().zip(shifts).map(|(&c, s)| c as u64 * (base << s)).sum()
}

fn flags_count(v: u32) -> u32 {
    ((v >> 4) & 1) + ((v >> 5) & 3) + ((v >> 7) & 0xF) + ((v >> 11) & 0x3F) + ((v >> 17) & 0x7F) + ((v >> 24) & 1) + ((v >> 25) & 1) + ((v >> 26) & 1) + ((v >> 27) & 1)
}

/// CodeWalker AssignPositions2. Returns (flags, total size, page count).
fn assign(ids: &[usize], sizes: &[u64], base: u64, sys: bool, max_pages: u32, pos: &mut [u64]) -> Result<(u32, u64, u32), String> {
    if ids.is_empty() {
        return Ok((0, 0, 0));
    }
    const MULT: u64 = 16;
    let max_block = ids.iter().map(|&i| sizes[i]).max().unwrap_or(0);
    let min_block = ids.iter().map(|&i| sizes[i]).min().unwrap_or(0);
    let mut shift = 0u32;
    let mut bsize = 0x2000u64;
    while (bsize < min_block || bsize * MULT < max_block) && shift < 0xF {
        shift += 1;
        bsize = 0x2000 << shift;
    }
    if bsize * MULT < max_block {
        return Err("unable to fit largest block".into());
    }
    let mut sorted: Vec<usize> = ids.iter().copied().filter(|&i| !(sys && i == ids[0])).collect();
    sorted.sort_by(|&a, &b| sizes[b].cmp(&sizes[a])); // stable, like List.Sort on equal keys is not, close enough
    if sys {
        sorted.insert(0, ids[0]);
    }
    let mut placed: Vec<(usize, usize, usize, u64)>;
    let mut counts: [u32; 5];
    loop {
        let mut pages: [Vec<u64>; 5] = Default::default();
        placed = Vec::with_capacity(sorted.len());
        let mut largest_i = 0usize;
        let mut largest = bsize;
        while largest < max_block {
            largest_i += 1;
            largest *= 2;
        }
        for (n, &b) in sorted.iter().enumerate() {
            let size = sizes[b];
            if n == 0 {
                pages[largest_i].push(size);
                placed.push((b, largest_i, 0, 0));
                continue;
            }
            let mut psi = 0usize;
            let mut ps = bsize;
            while size > ps && psi < largest_i {
                psi += 1;
                ps *= 2;
            }
            let mut found = None;
            let (mut ti, mut ts) = (psi, ps);
            while found.is_none() && ti <= largest_i {
                for p in 0..pages[ti].len() {
                    let mut s = pages[ti][p];
                    s += (ALIGN - s % ALIGN) % ALIGN;
                    let o = s;
                    s += size;
                    if s <= ts {
                        pages[ti][p] = s;
                        found = Some((ti, p, o));
                        break;
                    }
                }
                ti += 1;
                ts *= 2;
            }
            let (ti, p, o) = found.unwrap_or_else(|| {
                pages[psi].push(size);
                (psi, pages[psi].len() - 1, 0)
            });
            placed.push((b, ti, p, o));
        }
        counts = [0; 5];
        for i in 0..5 {
            counts[i] = pages[i].len() as u32;
        }
        let total: u32 = counts.iter().sum();
        let ok = total <= max_pages && counts[0] <= 0x7F && counts[1] <= 0x3F && counts[2] <= 0xF && counts[3] <= 3 && counts[4] <= 1;
        if ok {
            break;
        }
        if shift >= 0xF {
            return Err("unable to pack blocks with largest possible base".into());
        }
        shift += 1;
        bsize = 0x2000 << shift;
    }
    let mut offs = [0u64; 5];
    let mut o = 0u64;
    for i in (0..5).rev() {
        offs[i] = o;
        o += (bsize << i) * counts[i] as u64;
    }
    for (b, psi, p, off) in placed {
        pos[b] = base + offs[psi] + (bsize << psi) * p as u64 + off;
    }
    let mut v = shift & 0xF;
    v += (counts[4] & 1) << 4;
    v += (counts[3] & 3) << 5;
    v += (counts[2] & 0xF) << 7;
    v += (counts[1] & 0x3F) << 11;
    v += (counts[0] & 0x7F) << 17;
    Ok((v, flags_size(v), flags_count(v)))
}

/// CodeWalker AssignPositionsForMeta (naive sequential packing).
fn assign_meta(ids: &[usize], sizes: &[u64], base: u64, pos: &mut [u64]) -> Result<(u32, u64, u32), String> {
    if ids.is_empty() {
        return Ok((0, 0, 0));
    }
    let largest = ids.iter().map(|&i| sizes[i]).max().unwrap_or(0);
    let mut psize = 0x2000u64;
    while psize < largest {
        psize *= 2;
    }
    let mut count;
    loop {
        count = 0u64;
        let mut cur = 0u64;
        for &b in ids {
            let space = (count * psize) as i64 - cur as i64;
            if space < (sizes[b] + SKIP) as i64 {
                count += 1;
                cur = psize * (count - 1);
            }
            pos[b] = base + cur;
            cur += sizes[b];
            if cur % ALIGN != 0 {
                cur += ALIGN - cur % ALIGN;
            }
        }
        if count < 128 {
            break;
        }
        psize *= 2;
    }
    let v = flags_from_blocks(count as u32, psize as u32);
    Ok((v, flags_size(v), flags_count(v)))
}

/// CodeWalker RpfResourceFileEntry.GetFlagsFromBlocks (version bits left 0).
fn flags_from_blocks(count: u32, size: u32) -> u32 {
    let mut ss = 0u32;
    let mut bst = size;
    if count > 0 {
        while bst > 0x200 {
            ss += 1;
            bst >>= 1;
        }
    }
    let mut f = 0u32;
    f |= (count & 1) << 27;
    f |= ((count >> 1) & 1) << 26;
    f |= ((count >> 2) & 1) << 25;
    f |= ((count >> 3) & 1) << 24;
    f |= ((count >> 4) & 0x7F) << 17;
    f |= ss & 0xF;
    f
}

/// GTA V joaat hash (lowercased, as the game hashes names).
pub fn joaat(s: &str) -> u32 {
    let mut h: u32 = 0;
    for c in s.bytes() {
        h = h.wrapping_add(c.to_ascii_lowercase() as u32);
        h = h.wrapping_add(h << 10);
        h ^= h >> 6;
    }
    h = h.wrapping_add(h << 3);
    h ^= h >> 11;
    h.wrapping_add(h << 15)
}
