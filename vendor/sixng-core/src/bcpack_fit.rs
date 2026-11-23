//! Preset-table fit passes behind `6ixpack bcpack-calibrate`, after
//! GTA6-Asset-Info 644a32b and f815727 (formats/06-bcpack.md):
//!
//! - **BC7 refinement**: every preset slot, endpoint and index, refined field
//!   by field, scored on the blocks whose chain roots in that slot, in chunks
//!   that have a plain (non-BCPack) neighbour mip.
//! - **BC6H refit of unreferenced slots**: endpoint and index slots with no
//!   plain reference are refitted index-aware (full pixel error of the decoded
//!   block, indices included) against the decoded neighbour mips in both
//!   directions; slots that have a plain reference stay frozen.
//!
//! Every score is a full decode of the chunk prefix with the candidate table
//! (the real decoder, so chains and indices follow exactly), compared per 2x2
//! quadrant with the neighbour mip (child pixel, or parent 4x4 mean).
use crate::bcpack_presets::Presets;
use crate::ttpf::{Ttpf, TtpfSet, TtpfTex};
use rayon::prelude::*;
use std::collections::HashMap;

const NONE: u16 = u16::MAX;
/// Targeted blocks scored per slot and field kind (keeps a pass bounded).
const SLOT_BLOCKS: usize = 4000;
/// A slot with fewer targeted blocks than this has no usable reference.
const MIN_REF: usize = 8;
/// Coordinate-descent rounds per slot.
const ROUNDS: usize = 3;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Codec {
    Bc7,
    Bc6,
}

#[derive(Clone, Copy, PartialEq)]
enum Ref {
    /// The neighbour mip is stored plain (ground truth).
    Plain,
    /// The neighbour mip is BCPack too, decoded with the current tables.
    Decoded,
}

/// One chunk prefix with targets for some of its blocks.
struct Samp {
    lanes: Vec<Vec<u16>>,
    nb: usize,
    roots: Vec<(u16, u16)>,
    tgt: Vec<Option<[[f32; 4]; 4]>>,
}

fn cid(c: Codec) -> u8 {
    match c {
        Codec::Bc7 => crate::bcpack::CODEC_BC7,
        Codec::Bc6 => crate::bcpack::CODEC_BC6H,
    }
}

fn of_dxgi(d: u32) -> Option<Codec> {
    match d {
        94..=96 => Some(Codec::Bc6),
        97..=99 => Some(Codec::Bc7),
        _ => None,
    }
}

fn chunks_in(s: &Ttpf, a: u64, b: u64) -> Vec<usize> {
    s.chunks.windows(2).enumerate().filter(|(_, w)| w[1].1 > a && w[0].1 < b).map(|(i, _)| i).collect()
}

fn is_bcpack(fl: u8) -> bool {
    fl & 0x1C != 0
}

/// Decoded neighbour mip as f32 RGBA (BC6H: linear RGB, alpha 0).
struct Img {
    px: Vec<f32>,
    w: i64,
    h: i64,
    child: bool,
}

impl Img {
    fn at(&self, x: i64, y: i64) -> [f32; 4] {
        let (x, y) = (x.clamp(0, self.w - 1), y.clamp(0, self.h - 1));
        let o = ((y * self.w + x) * 4) as usize;
        [self.px[o], self.px[o + 1], self.px[o + 2], self.px[o + 3]]
    }
    /// Target per quadrant of block (bx, by) of the BCPack mip.
    fn quads(&self, bx: i64, by: i64) -> [[f32; 4]; 4] {
        let mut out = [[0.0; 4]; 4];
        for qy in 0..2 {
            for qx in 0..2 {
                let qi = (qy * 2 + qx) as usize;
                if self.child {
                    out[qi] = self.at(2 * bx + qx, 2 * by + qy);
                    continue;
                }
                let (x0, y0) = (8 * bx + 4 * qx, 8 * by + 4 * qy);
                let (mut s, mut n) = ([0.0f32; 4], 0.0f32);
                for y in y0..(y0 + 4).min(self.h) {
                    for x in x0..(x0 + 4).min(self.w) {
                        let p = self.at(x, y);
                        for c in 0..4 {
                            s[c] += p[c];
                        }
                        n += 1.0;
                    }
                }
                if n > 0.0 {
                    out[qi] = s.map(|v| v / n);
                }
            }
        }
        out
    }
}

fn image(t: &TtpfTex, data: &[u8], mz: usize, codec: Codec, child: bool) -> Option<Img> {
    let (w, h) = ((t.width >> mz).max(1), (t.height >> mz).max(1));
    let px: Vec<f32> = match codec {
        Codec::Bc7 => crate::ttpf::decode_rgba(t, data, mz as u32).ok()?.0.into_iter().map(|v| v as f32).collect(),
        Codec::Bc6 => {
            let lin = t.layout()?.untile(data, mz);
            let (bw, bh) = (w.div_ceil(4) as usize, h.div_ceil(4) as usize);
            if lin.len() < bw * bh * 16 {
                return None;
            }
            let mut px = vec![0f32; (w * h * 4) as usize];
            for by in 0..bh {
                for bx in 0..bw {
                    let o = (by * bw + bx) * 16;
                    let blk: [u8; 16] = lin[o..o + 16].try_into().ok()?;
                    let Some(f) = crate::bc6pack::unpack(&blk) else { continue };
                    let p = crate::bc6pack::decode_pixels(&f);
                    for k in 0..16 {
                        let (x, y) = (bx * 4 + k % 4, by * 4 + k / 4);
                        if x < w as usize && y < h as usize {
                            let d = (y * w as usize + x) * 4;
                            px[d..d + 3].copy_from_slice(&p[k]);
                        }
                    }
                }
            }
            px
        }
    };
    Some(Img { px, w: w as i64, h: h as i64, child })
}

/// Chunk prefixes of `codec` with neighbour-mip targets of kind `r`.
fn collect(set: &TtpfSet, codec: Codec, r: Ref) -> Vec<Samp> {
    let c = cid(codec);
    let bits: &[u8] = match codec {
        Codec::Bc7 => &crate::bc7pack::BC7_ALPHABET_BITS,
        Codec::Bc6 => &crate::bc6pack::BC6_ALPHABET_BITS,
    };
    let jobs: Vec<(&Ttpf, &TtpfTex)> = set.stores.iter().flat_map(|s| s.textures.iter().map(move |t| (s, t))).filter(|(_, t)| of_dxgi(t.dxgi_format) == Some(codec)).collect();
    jobs.par_iter()
        .flat_map_iter(|&(s, t)| {
            let mut out = Vec::new();
            if t.slices() != 1 || (9..=12).contains(&t.tile_mode()) || t.split_2b {
                return out;
            }
            let Some(l) = t.layout() else { return out };
            let nm = l.mips.len();
            let spans: Vec<(u64, u64)> = (0..nm).map(|m| l.mip_span(m)).collect();
            let cks: Vec<Vec<usize>> = spans.iter().map(|&(a, b)| chunks_in(s, t.virt + a, t.virt + b)).collect();
            let has = |m: usize| cks[m].iter().any(|&i| s.chunks[i].2 & 0x3F == c);
            let plain = |m: usize| cks[m].iter().all(|&i| !is_bcpack(s.chunks[i].2));
            let small = |m: usize| spans[m].1 - spans[m].0 <= 1 << 20;
            let mut pairs = Vec::new();
            for m in 0..nm {
                if !has(m) {
                    continue;
                }
                match r {
                    Ref::Plain => {
                        if m + 1 < nm && plain(m + 1) {
                            pairs.push((m, m + 1, true));
                        } else if m > 0 && plain(m - 1) && small(m - 1) {
                            pairs.push((m, m - 1, false));
                        }
                    }
                    Ref::Decoded => {
                        if m + 1 < nm && !plain(m + 1) {
                            pairs.push((m, m + 1, true));
                        }
                        if m > 0 && !plain(m - 1) && small(m - 1) {
                            pairs.push((m, m - 1, false));
                        }
                    }
                }
            }
            if pairs.is_empty() {
                return out;
            }
            let len = t.size.min(64 << 20);
            let data = match r {
                Ref::Plain => s.read_plain(t.virt, len).ok(),
                Ref::Decoded => s.read(t.virt, len).ok(),
            };
            let Some(data) = data else { return out };
            for (m, mz, child) in pairs {
                let Some(img) = image(t, &data, mz, codec, child) else { continue };
                let mp = &l.mips[m];
                let mut inv: HashMap<u64, (i64, i64)> = HashMap::with_capacity((mp.w * mp.h) as usize);
                for y in 0..mp.h {
                    for x in 0..mp.w {
                        inv.insert(l.addr(m, 0, x, y) - spans[m].0, (x as i64, y as i64));
                    }
                }
                let mip_virt = t.virt + spans[m].0;
                for &ci in cks[m].iter().filter(|&&i| s.chunks[i].2 & 0x3F == c) {
                    let (v0, v1) = (s.chunks[ci].1, s.chunks[ci + 1].1);
                    let nb = ((v1 - v0) / 16) as usize;
                    let Ok(comp) = s.chunk_bytes(ci) else { continue };
                    let Ok(lanes) = ::bcpack::rans::decode_slice(&comp, bits, 2 * nb) else { continue };
                    let lane: Vec<&[u16]> = (0..16).map(|k| lanes.lane(k)).collect();
                    let roots = match codec {
                        Codec::Bc7 => crate::bc7pack::preset_roots(&lane, nb),
                        Codec::Bc6 => crate::bc6pack::preset_roots(&lane, nb),
                    };
                    let rel = v0 as i64 - mip_virt as i64;
                    let tgt: Vec<Option<[[f32; 4]; 4]>> = (0..nb)
                        .map(|i| {
                            let off = rel + (i * 16) as i64;
                            if off < 0 || (roots[i].0 == NONE && roots[i].1 == NONE) {
                                return None;
                            }
                            inv.get(&(off as u64)).map(|&(bx, by)| img.quads(bx, by))
                        })
                        .collect();
                    // keep the prefix up to the last scored block
                    let Some(last) = tgt.iter().rposition(|x| x.is_some()) else { continue };
                    let n = last + 1;
                    out.push(Samp { lanes: lane.iter().map(|l| l[..2 * n].to_vec()).collect(), nb: n, roots: roots[..n].to_vec(), tgt: tgt[..n].to_vec() });
                }
            }
            out
        })
        .collect()
}

fn quad_means<const C: usize>(px: &[[f32; C]; 16]) -> [[f32; C]; 4] {
    let mut q = [[0f32; C]; 4];
    for (k, p) in px.iter().enumerate() {
        let qi = (k / 8) * 2 + (k % 4) / 2;
        for c in 0..C {
            q[qi][c] += p[c] / 4.0;
        }
    }
    q
}

/// BC7 block error: per quadrant and channel, squared 8-bit error capped at 32 levels.
fn err7(blk: &[u8], t: &[[f32; 4]; 4]) -> f64 {
    let mut px = [0u32; 16];
    if texture2ddecoder::decode_bc7(blk, 4, 4, &mut px).is_err() {
        return 16.0 * 1024.0;
    }
    let rgba: [[f32; 4]; 16] = px.map(|v| [((v >> 16) & 255) as f32, ((v >> 8) & 255) as f32, (v & 255) as f32, (v >> 24) as f32]);
    let q = quad_means(&rgba);
    let mut e = 0.0;
    for k in 0..4 {
        for c in 0..4 {
            let d = (q[k][c] - t[k][c]) as f64;
            e += (d * d).min(1024.0);
        }
    }
    e
}

/// BC6H block error: per quadrant and channel, log2 error capped at 4 stops.
fn err6(blk: &[u8], t: &[[f32; 4]; 4]) -> f64 {
    let Some(f) = blk.try_into().ok().and_then(|b: [u8; 16]| crate::bc6pack::unpack(&b)) else { return 48.0 };
    let q = quad_means(&crate::bc6pack::decode_pixels(&f));
    let lg = |v: f32| ((v.max(0.0) + 1.0) as f64).log2();
    let mut e = 0.0;
    for k in 0..4 {
        for c in 0..3 {
            e += (lg(q[k][c]) - lg(t[k][c])).abs().min(4.0);
        }
    }
    e
}

/// Which table half a slot list is about.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Ep,
    Ix,
}

/// Per slot: (sample, block list) of targeted blocks whose `kind` chain roots there.
fn uses(samps: &[Samp], kind: Kind) -> Vec<Vec<(usize, Vec<usize>)>> {
    let mut u: Vec<Vec<(usize, Vec<usize>)>> = vec![Vec::new(); 256];
    let mut n = [0usize; 256];
    for (si, s) in samps.iter().enumerate() {
        let mut per: HashMap<u16, Vec<usize>> = HashMap::new();
        for i in 0..s.nb {
            if s.tgt[i].is_none() {
                continue;
            }
            let k = if kind == Kind::Ep { s.roots[i].0 } else { s.roots[i].1 };
            if k != NONE && n[k as usize] < SLOT_BLOCKS {
                n[k as usize] += 1;
                per.entry(k).or_default().push(i);
            }
        }
        for (k, b) in per {
            u[k as usize].push((si, b));
        }
    }
    u
}

fn count(u: &[(usize, Vec<usize>)]) -> usize {
    u.iter().map(|x| x.1.len()).sum()
}

/// Slot parameters as plain integers (field order fixed per codec and kind).
fn get(p: &Presets, c: Codec, kind: Kind, k: usize) -> Option<Vec<u32>> {
    Some(match (c, kind) {
        (Codec::Bc7, Kind::Ep) => p.bc7_ep[k]?.iter().flat_map(|s| s.iter().flat_map(|e| e.iter().map(|&v| v as u32))).collect(),
        (Codec::Bc7, Kind::Ix) => p.bc7_ix[k]?.iter().flat_map(|s| s.iter().map(|&v| v as u32)).collect(),
        (Codec::Bc6, Kind::Ep) => p.bc6_ep[k]?.iter().flat_map(|f| f.iter().map(|&v| v as u32)).collect(),
        (Codec::Bc6, Kind::Ix) => p.bc6_ix[k]?.iter().map(|&v| v as u32).collect(),
    })
}

fn set(p: &mut Presets, c: Codec, kind: Kind, k: usize, v: Option<&[u32]>) {
    match (c, kind) {
        (Codec::Bc7, Kind::Ep) => p.bc7_ep[k] = v.map(|v| std::array::from_fn(|s| std::array::from_fn(|e| std::array::from_fn(|ch| v[s * 8 + e * 4 + ch] as u8)))),
        (Codec::Bc7, Kind::Ix) => p.bc7_ix[k] = v.map(|v| std::array::from_fn(|s| std::array::from_fn(|i| v[s * 16 + i] as u8))),
        (Codec::Bc6, Kind::Ep) => p.bc6_ep[k] = v.map(|v| std::array::from_fn(|f| std::array::from_fn(|ch| v[f * 3 + ch] as u16))),
        (Codec::Bc6, Kind::Ix) => p.bc6_ix[k] = v.map(|v| std::array::from_fn(|i| v[i] as u8)),
    }
}

/// (field count, largest value, start value for an empty slot).
fn shape(c: Codec, kind: Kind) -> (usize, u32, u32) {
    match (c, kind) {
        (Codec::Bc7, Kind::Ep) => (24, 255, 0x80),
        (Codec::Bc7, Kind::Ix) => (32, 15, 8),
        (Codec::Bc6, Kind::Ep) => (12, 0xFFFF, 0x8000),
        (Codec::Bc6, Kind::Ix) => (16, 15, 8),
    }
}

/// Error of the given blocks with table `p` (full decode of each prefix).
fn score(samps: &[Samp], u: &[(usize, Vec<usize>)], p: &Presets, c: Codec) -> f64 {
    let mut e = 0.0;
    for (si, blocks) in u {
        let s = &samps[*si];
        let n = blocks.last().map_or(0, |&b| b + 1);
        let lane: Vec<&[u16]> = s.lanes.iter().map(|l| l.as_slice()).collect();
        let out = match c {
            Codec::Bc7 => {
                let mut t = crate::bc7pack::shipped_tables();
                t.hist_ep = p.bc7_ep;
                t.hist_idx = p.bc7_ix;
                crate::bc7pack::decode_lanes_states(&lane, n, &t).0.blocks
            }
            Codec::Bc6 => crate::bc6pack::decode_lanes(&lane, n, &crate::bc6pack::Tables { hist_ep: p.bc6_ep, hist_idx: p.bc6_ix }).0.blocks,
        };
        for &b in blocks {
            let blk = &out[b * 16..b * 16 + 16];
            let t = s.tgt[b].as_ref().unwrap();
            e += if c == Codec::Bc7 { err7(blk, t) } else { err6(blk, t) };
        }
    }
    e
}

/// Field-by-field coordinate descent of one slot. Returns the new values
/// when they lower the error.
fn fit_slot(samps: &[Samp], u: &[(usize, Vec<usize>)], base: &Presets, c: Codec, kind: Kind, k: usize) -> Option<(Vec<u32>, f64, f64)> {
    let (nf, maxv, mid) = shape(c, kind);
    let mut p = base.clone();
    let before = score(samps, u, &p, c);
    let mut v = get(base, c, kind, k).unwrap_or_else(|| vec![mid; nf]);
    set(&mut p, c, kind, k, Some(&v));
    let mut e = score(samps, u, &p, c);
    for _ in 0..ROUNDS {
        let e0 = e;
        for f in 0..nf {
            let try_v = |x: u32, p: &mut Presets, v: &mut Vec<u32>, e: &mut f64| {
                if x == v[f] {
                    return;
                }
                let old = v[f];
                v[f] = x;
                set(p, c, kind, k, Some(v));
                let te = score(samps, u, p, c);
                if te < *e {
                    *e = te;
                } else {
                    v[f] = old;
                    set(p, c, kind, k, Some(v));
                }
            };
            if maxv <= 15 {
                for x in 0..=maxv {
                    try_v(x, &mut p, &mut v, &mut e);
                }
                continue;
            }
            // coarse scan over 16 bins, then halve the step around the best
            let step = (maxv + 1) / 16;
            for b in 0..16 {
                try_v(b * step + step / 2, &mut p, &mut v, &mut e);
            }
            let mut st = step / 2;
            while st >= 1 {
                let cur = v[f];
                try_v(cur.saturating_sub(st), &mut p, &mut v, &mut e);
                try_v((cur + st).min(maxv), &mut p, &mut v, &mut e);
                st /= 2;
            }
        }
        if e >= e0 - 1e-9 {
            break;
        }
    }
    (e < before - 1e-9).then_some((v, before, e))
}

/// One refinement pass over the slots of `c`/`kind` not in `frozen`.
fn pass(samps: &[Samp], base: &Presets, c: Codec, kind: Kind, frozen: &[bool], log: &(dyn Fn(String) + Sync)) -> (Presets, usize, f64, f64) {
    let u = uses(samps, kind);
    let fitted: Vec<(usize, Vec<u32>, f64, f64)> = (0..256usize)
        .into_par_iter()
        .filter(|&k| !frozen[k] && count(&u[k]) >= MIN_REF)
        .filter_map(|k| fit_slot(samps, &u[k], base, c, kind, k).map(|(v, b, a)| (k, v, b, a)))
        .collect();
    let mut p = base.clone();
    let (mut b, mut a) = (0.0, 0.0);
    for (k, v, eb, ea) in &fitted {
        set(&mut p, c, kind, *k, Some(v));
        b += eb;
        a += ea;
    }
    log(format!("{c:?} {} slots: {} changed (scored error {b:.0} -> {a:.0})", if kind == Kind::Ep { "endpoint" } else { "index" }, fitted.len()));
    (p, fitted.len(), b, a)
}

/// Runs both fits from the tables in use now and returns the new tables.
pub fn calibrate(set: &TtpfSet, log: &(dyn Fn(String) + Sync)) -> Presets {
    let mut p = Presets::current();
    let none = [false; 256];

    log("BC7: collecting chunks with a plain neighbour mip...".into());
    let s7 = collect(set, Codec::Bc7, Ref::Plain);
    log(format!("BC7: {} chunk prefixes", s7.len()));
    p = pass(&s7, &p, Codec::Bc7, Kind::Ep, &none, log).0;
    p = pass(&s7, &p, Codec::Bc7, Kind::Ix, &none, log).0;
    drop(s7);

    log("BC6H: finding slots with a plain reference...".into());
    let plain6 = collect(set, Codec::Bc6, Ref::Plain);
    let froz = |kind: Kind| -> Vec<bool> { uses(&plain6, kind).iter().map(|u| count(u) >= MIN_REF).collect() };
    let (fe, fi) = (froz(Kind::Ep), froz(Kind::Ix));
    drop(plain6);
    log(format!("BC6H: {} endpoint and {} index slots frozen (plain reference)", fe.iter().filter(|&&f| f).count(), fi.iter().filter(|&&f| f).count()));
    let d6 = collect(set, Codec::Bc6, Ref::Decoded);
    log(format!("BC6H: {} chunk prefixes against decoded neighbour mips", d6.len()));
    p = pass(&d6, &p, Codec::Bc6, Kind::Ep, &fe, log).0;
    p = pass(&d6, &p, Codec::Bc6, Kind::Ix, &fi, log).0;
    p
}
