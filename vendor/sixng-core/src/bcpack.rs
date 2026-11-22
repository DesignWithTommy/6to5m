//! BCPack block rebuilders for BC3, BC4, BC5, BC6H and BC7 (Xbox Series
//! hardware texture codec), on top of the rANS lane decoder of `bcpack-rs`.
//! BC1 stays with `bcpack-rs` (`bcpack::bc1`).
//!
//! Port of Leonida's clean-room `tools/bcpack.py` (GPL-3.0,
//! github.com/ItsNotRexed/Leonida); tables in `bcpack_tables.rs` are generated
//! from its `bcpack_tables*.py`.
//!
//! Lanes: 12 (BC1, BC3, BC4, BC5) or 16 (BC6H, BC7); every lane emits one
//! symbol per 8 output bytes, so 16-byte formats have two symbols per block
//! (`a = 2i`, `b = 2i + 1`), often bit-interleaved into one 8-bit value.
//! Endpoints and (BC6H/BC7) indices are zigzag residuals against a block
//! `d` back in the chunk, or, when `d > i`, against a fixed 256-entry
//! "history" preset the hardware starts with. BC3/BC4/BC5 presets are fitted
//! (from Leonida); the BC6H/BC7 presets are not public, so blocks whose
//! reference chain starts in the preset are counted in `Decoded::unknown`.
use crate::bcpack_tables as t;
use anyhow::{anyhow, bail, Result};

pub const CODEC_BC1: u8 = 0x04;
pub const CODEC_BC3: u8 = 0x0C;
pub const CODEC_BC4: u8 = 0x10;
pub const CODEC_BC5: u8 = 0x14;
pub const CODEC_BC6H: u8 = 0x18;
pub const CODEC_BC7: u8 = 0x1C;

/// Lane symbol widths per codec.
fn lane_widths(codec: u8) -> Option<&'static [u8]> {
    Some(match codec {
        CODEC_BC1 => &[8, 8, 5, 6, 5, 5, 6, 5, 8, 8, 8, 8],
        CODEC_BC3 => &[4, 4, 8, 6, 6, 6, 6, 5, 6, 5, 8, 8],
        CODEC_BC4 => &[8, 8, 8, 8, 6, 6, 6, 6, 6, 6, 6, 6],
        CODEC_BC5 => &[4, 4, 8, 8, 6, 6, 6, 6, 6, 6, 6, 6],
        CODEC_BC6H => &[4, 4, 8, 8, 8, 5, 5, 5, 6, 6, 6, 8, 8, 8, 8, 5],
        CODEC_BC7 => &[4, 4, 8, 8, 8, 8, 8, 7, 7, 5, 6, 8, 8, 8, 8, 5],
        _ => return None,
    })
}

fn block_bytes(codec: u8) -> usize {
    if codec == CODEC_BC1 || codec == CODEC_BC4 { 8 } else { 16 }
}

/// Codecs with a block rebuilder here.
pub fn supported(codec: u8) -> bool {
    matches!(codec, CODEC_BC3 | CODEC_BC4 | CODEC_BC5 | CODEC_BC6H | CODEC_BC7)
}

pub struct Decoded {
    /// Plain BCn blocks (Gfx10 tiled order, as stored).
    pub data: Vec<u8>,
    /// Blocks whose prediction chain starts in a preset history entry that is
    /// not known (decoded against zeros: colours may be off).
    pub unknown: usize,
    /// Per block: true for the `unknown` blocks.
    pub lost: Vec<bool>,
    /// Blocks failing a BC6H/BC7 consistency check (anchor index MSB set,
    /// shared p-bit mismatch): a sign the chain is wrong.
    pub inconsistent: usize,
    /// BC6H, per block (diagnostics): mode | endpoint distance << 8 |
    /// chain-from-preset << 16 | referenced block's mode << 20. Empty otherwise.
    pub info: Vec<u32>,
}

/// Decode one BCPack chunk (`comp` = stored bytes, zero pad allowed) into
/// `out_len` bytes of plain blocks.
pub fn decode(codec: u8, comp: &[u8], out_len: usize) -> Result<Decoded> {
    let widths = lane_widths(codec).ok_or_else(|| anyhow!("BCPack codec {codec:#x} unknown"))?;
    if !supported(codec) {
        bail!("BCPack codec {codec:#x} has no rebuilder here");
    }
    let bb = block_bytes(codec);
    if out_len == 0 || out_len % bb != 0 {
        bail!("BCPack: {out_len} bytes is not a whole number of blocks");
    }
    let _ = widths;
    let lo = decode_lanes(codec, comp, out_len)?;
    let lanes: Vec<&[u16]> = lo.iter().map(|v| v.as_slice()).collect();
    let n = out_len / bb;
    Ok(match codec {
        CODEC_BC3 => bc3(&lanes, n),
        CODEC_BC4 => bc4(&lanes, n),
        CODEC_BC5 => bc5(&lanes, n),
        CODEC_BC6H => bc6h(&lanes, n),
        _ => bc7(&lanes, n),
    })
}

/// The rANS lanes of a chunk (one symbol per 8 output bytes each).
pub(crate) fn decode_lanes(codec: u8, comp: &[u8], out_len: usize) -> Result<Vec<Vec<u16>>> {
    let widths = lane_widths(codec).ok_or_else(|| anyhow!("BCPack codec {codec:#x} unknown"))?;
    let ab: Vec<u8> = widths.iter().map(|w| w - 1).collect();
    let lo = bcpack::rans::decode_slice(comp, &ab, out_len / 8).map_err(|e| anyhow!("BCPack lanes: {e}"))?;
    Ok((0..widths.len()).map(|i| lo.lane(i).to_vec()).collect())
}

// Preset history ----------------------------------------------------------------

/// Fitted preset ("built-in") history; see `bcpack_calib`.
#[derive(Clone)]
pub struct History {
    pub bc4: Vec<Option<[u8; 2]>>,
    pub bc7_ep: Vec<[u8; 24]>,
    pub bc7_ep_bits: Vec<[u8; 24]>,
    /// 4-bit values (slots 0-15 primary, 16-31 secondary index set).
    pub bc7_ix: Vec<[u8; 32]>,
    pub bc7_ix_bits: Vec<[u8; 32]>,
    pub bc6h_ep: Vec<[[u32; 4]; 3]>,
    pub bc6h_ix: Vec<[u32; 16]>,
    /// 0 unknown, 1 fitted, 2 rough.
    pub bc6h_ecls: Vec<u8>,
    pub bc6h_icls: Vec<u8>,
}

const HIST_MAGIC: &[u8; 8] = b"BCPHIST1";
const HIST_REC: usize = 3 + 24 + 24 + 32 + 32 + 48 + 16 + 2;

impl History {
    pub fn empty() -> History {
        History {
            bc4: vec![None; 256],
            bc7_ep: vec![[0; 24]; 256],
            bc7_ep_bits: vec![[0; 24]; 256],
            bc7_ix: vec![[0; 32]; 256],
            bc7_ix_bits: vec![[0; 32]; 256],
            bc6h_ep: vec![[[0; 4]; 3]; 256],
            bc6h_ix: vec![[0; 16]; 256],
            bc6h_ecls: vec![0; 256],
            bc6h_icls: vec![0; 256],
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut o = HIST_MAGIC.to_vec();
        for k in 0..256 {
            let b = self.bc4[k];
            o.push(b.is_some() as u8);
            o.extend_from_slice(&b.unwrap_or([0; 2]));
            o.extend_from_slice(&self.bc7_ep[k]);
            o.extend_from_slice(&self.bc7_ep_bits[k]);
            o.extend_from_slice(&self.bc7_ix[k]);
            o.extend_from_slice(&self.bc7_ix_bits[k]);
            for c in 0..3 {
                for q in 0..4 {
                    o.extend_from_slice(&self.bc6h_ep[k][c][q].to_le_bytes());
                }
            }
            for p in 0..16 {
                o.push(self.bc6h_ix[k][p] as u8);
            }
            o.push(self.bc6h_ecls[k]);
            o.push(self.bc6h_icls[k]);
        }
        o
    }

    pub fn from_bytes(b: &[u8]) -> Option<History> {
        if b.len() != 8 + 256 * HIST_REC || &b[..8] != HIST_MAGIC {
            return None;
        }
        let mut h = History::empty();
        for k in 0..256 {
            let r = &b[8 + k * HIST_REC..8 + (k + 1) * HIST_REC];
            h.bc4[k] = (r[0] != 0).then(|| [r[1], r[2]]);
            h.bc7_ep[k].copy_from_slice(&r[3..27]);
            h.bc7_ep_bits[k].copy_from_slice(&r[27..51]);
            h.bc7_ix[k].copy_from_slice(&r[51..83]);
            h.bc7_ix_bits[k].copy_from_slice(&r[83..115]);
            for c in 0..3 {
                for q in 0..4 {
                    let o = 115 + (c * 4 + q) * 4;
                    h.bc6h_ep[k][c][q] = u32::from_le_bytes(r[o..o + 4].try_into().unwrap());
                }
            }
            for p in 0..16 {
                h.bc6h_ix[k][p] = r[163 + p] as u32;
            }
            h.bc6h_ecls[k] = r[179];
            h.bc6h_icls[k] = r[180];
        }
        Some(h)
    }
}

static HISTORY: std::sync::OnceLock<Option<History>> = std::sync::OnceLock::new();
type Loader = Box<dyn Fn() -> Option<History> + Send + Sync>;
static LOADER: std::sync::OnceLock<Loader> = std::sync::OnceLock::new();

/// Install the preset history (first call wins). Without one, blocks that
/// predict from it decode against zeros and are counted in `Decoded::unknown`.
pub fn set_history(h: Option<History>) {
    let _ = HISTORY.set(h);
}

/// Where to get the history on first use (e.g. calibrate the stores, cached).
pub fn set_history_loader(f: Loader) {
    let _ = LOADER.set(f);
}

/// The installed history. On first use: the file named by
/// `SIXNG_BCPACK_HISTORY`, else the registered loader.
pub fn history() -> Option<&'static History> {
    HISTORY
        .get_or_init(|| {
            if let Some(p) = std::env::var_os("SIXNG_BCPACK_HISTORY") {
                return std::fs::read(p).ok().and_then(|b| History::from_bytes(&b));
            }
            LOADER.get().and_then(|f| f())
        })
        .as_ref()
}

/// BC6H blocks (row-major, 16 bytes each) to unquantized 16-bit RGB per
/// pixel (alpha 0), as f64 RGBA of a `w` x `h` image. Unsigned only
/// (`signed` textures give zeros).
pub fn bc6h_unq_image(lin: &[u8], w: u32, h: u32, signed: bool) -> Vec<f64> {
    const W3: [u32; 8] = [0, 9, 18, 27, 37, 46, 55, 64];
    const W4: [u32; 16] = [0, 4, 9, 13, 17, 21, 26, 30, 34, 38, 43, 47, 51, 55, 60, 64];
    let (bw, bh) = (w.div_ceil(4) as usize, h.div_ceil(4) as usize);
    let mut out = vec![0.0f64; (w * h) as usize * 4];
    if signed {
        return out;
    }
    let mut mode_of = [0usize; 32];
    for (m, &c) in B6_CODE.iter().enumerate().skip(1) {
        mode_of[c as usize] = m;
    }
    let plans: Vec<(Vec<(i8, u8, u8, u8)>, u32)> = B6_CODE.iter().map(|&c| b6_plan(c)).collect();
    let unq = |v: i64, bits: u32| -> f64 {
        let v = v as u32;
        if bits >= 15 {
            v as f64
        } else if v == 0 {
            0.0
        } else if v == (1 << bits) - 1 {
            65535.0
        } else {
            ((((v as u64) << 16) + 0x8000) >> bits) as f64
        }
    };
    let sx = |v: u32, bits: u32| -> i64 {
        let s = 1i64 << (bits - 1);
        (v as i64 ^ s) - s
    };
    for by in 0..bh {
        for bx in 0..bw {
            let o = (by * bw + bx) * 16;
            let Some(blk) = lin.get(o..o + 16) else { continue };
            let acc = u128::from_le_bytes(blk.try_into().unwrap());
            let code = if acc & 3 < 2 { (acc & 3) as u32 } else { (acc & 0x1F) as u32 };
            let mode = mode_of[code as usize];
            if mode == 0 || B6_CODE[mode] != code {
                continue;
            }
            let (trans, base, deltas, reg, _) = b6_info(code);
            let (plan, header) = &plans[mode];
            let mut f = [[0u32; 4]; 3];
            let mut part = 0u32;
            for &(c, q, bit, pos) in plan {
                if acc >> pos & 1 != 0 {
                    if c < 0 {
                        part |= 1 << bit;
                    } else {
                        f[c as usize][q as usize] |= 1 << bit;
                    }
                }
            }
            // un-reverse the stream order of the base value (involution)
            for c in 0..3 {
                f[c][0] = b6_stream_w(mode, f[c][0]);
            }
            let mw = (1i64 << base) - 1;
            let mut ep = [[0.0f64; 4]; 3];
            for c in 0..3 {
                let [wv, x, y, z] = f[c];
                let wv = b6_stream_w(mode, wv);
                let dc = deltas[c];
                if trans {
                    ep[c][0] = unq(wv as i64, base);
                    ep[c][1] = unq((wv as i64 + sx(x, dc)) & mw, base);
                    if reg == 2 {
                        ep[c][2] = unq((wv as i64 + sx(y, dc)) & mw, base);
                        ep[c][3] = unq((wv as i64 + sx(z, dc)) & mw, base);
                    }
                } else {
                    ep[c][0] = unq(wv as i64, base);
                    ep[c][1] = unq(x as i64, dc);
                    if reg == 2 {
                        ep[c][2] = unq(y as i64, dc);
                        ep[c][3] = unq(z as i64, dc);
                    }
                }
            }
            let ibits = if reg == 2 { 3 } else { 4 };
            let second = if reg == 2 { t::BC7_ANCHOR2[part as usize] as usize } else { 99 };
            let mut at = *header;
            for p in 0..16 {
                let nb = if p == 0 || p == second { ibits - 1 } else { ibits };
                let ix = ((acc >> at) as u32) & ((1 << nb) - 1);
                at += nb;
                let wt = if reg == 2 { W3[ix as usize] } else { W4[ix as usize] } as f64;
                let s2 = if reg == 2 { (((t::BC7_PART2[part as usize] >> p) & 1) * 2) as usize } else { 0 };
                let (px, py) = (bx * 4 + p % 4, by * 4 + p / 4);
                if px >= w as usize || py >= h as usize {
                    continue;
                }
                let d = (py * w as usize + px) * 4;
                for c in 0..3 {
                    out[d + c] = ((64.0 - wt) * ep[c][s2] + wt * ep[c][s2 + 1]) / 64.0;
                }
            }
        }
    }
    out
}

#[inline]
pub(crate) fn unzig(v: u32) -> i32 {
    if v & 1 != 0 { -(((v + 1) >> 1) as i32) } else { (v >> 1) as i32 }
}

/// Bit-interleave the low `n` bits of `a` (even bits) and `b` (odd bits).
#[inline]
pub(crate) fn inter(a: u16, b: u16, n: u32) -> u32 {
    let mut d = 0u32;
    for k in 0..n {
        d |= ((a as u32 >> k) & 1) << (2 * k) | ((b as u32 >> k) & 1) << (2 * k + 1);
    }
    d
}
#[inline]
pub(crate) fn inter8(a: u16, b: u16) -> u32 {
    inter(a, b, 4)
}

/// Even (`0`) / odd (`1`) bits of an 8-bit symbol, packed into 4 bits.
#[inline]
fn deint(s: u16, odd: u32) -> u32 {
    let s = (s as u32) >> odd;
    (s & 1) | ((s >> 2) & 1) << 1 | ((s >> 4) & 1) << 2 | ((s >> 6) & 1) << 3
}

/// Back-reference distance of a 12-lane (8-byte block) codec, lane 0.
#[inline]
pub(crate) fn dist8(v: u16) -> usize {
    let v = v as usize;
    if v & 1 != 0 { 256 - ((v + 1) >> 1) } else { v >> 1 }
}

fn find<const N: usize>(table: &[(u16, [u8; N])], entry: usize) -> Option<[u8; N]> {
    table.binary_search_by_key(&(entry as u16), |e| e.0).ok().map(|i| table[i].1)
}

/// 3-bit index rows of BC3 alpha / BC4 / BC5: four rows of 12 bits.
fn alpha_bits(pairs: &[(u8, u8)], rows: impl Fn(usize) -> (u16, u16)) -> u64 {
    let mut bits = 0u64;
    for r in 0..4 {
        let (sl, sr) = rows(r);
        let (l, rr) = (pairs[sl as usize], pairs[sr as usize]);
        bits |= ((l.0 as u64) | (l.1 as u64) << 3 | (rr.0 as u64) << 6 | (rr.1 as u64) << 9) << (12 * r);
    }
    bits
}

fn bc1_quads(out: &mut [u8], q: [u8; 4]) {
    let [a, b, c, d] = q;
    out[0] = (a & 15) | (b & 15) << 4;
    out[1] = (a >> 4) | (b >> 4) << 4;
    out[2] = (c & 15) | (d & 15) << 4;
    out[3] = (c >> 4) | (d >> 4) << 4;
}

fn bc3(lanes: &[&[u16]], n: usize) -> Decoded {
    const MODS: [i32; 8] = [256, 256, 32, 64, 32, 32, 64, 32];
    const NONE: [u8; 8] = [128, 128, 0, 0, 0, 0, 0, 0];
    let mut out = vec![0u8; n * 16];
    let mut hist: Vec<[u8; 8]> = Vec::with_capacity(n);
    let mut lost = vec![false; n];
    let (l0, alpha) = (lanes[0], lanes[2]);
    for i in 0..n {
        let (a, b) = (2 * i, 2 * i + 1);
        let d = (unzig(inter8(l0[a], l0[b])) & 255) as usize;
        let base = if d == 0 {
            NONE
        } else if d <= i {
            lost[i] = lost[i - d];
            hist[i - d]
        } else {
            find(&t::BC3_HISTORY, 256 + i - d).unwrap_or_else(|| {
                lost[i] = true;
                NONE
            })
        };
        let dz = [alpha[a], alpha[b], lanes[7][a], lanes[8][a], lanes[9][a], lanes[7][b], lanes[8][b], lanes[9][b]];
        let mut e = [0u8; 8];
        for k in 0..8 {
            e[k] = (base[k] as i32 + unzig(dz[k] as u32)).rem_euclid(MODS[k]) as u8;
        }
        hist.push(e);
        let o = &mut out[i * 16..i * 16 + 16];
        o[0] = e[0];
        o[1] = e[1];
        let bits = alpha_bits(&t::BC3_ALPHA_PAIR, |r| (lanes[3 + r][a], lanes[3 + r][b]));
        o[2..8].copy_from_slice(&bits.to_le_bytes()[..6]);
        let c0 = (e[2] as u16) << 11 | (e[3] as u16) << 5 | e[4] as u16;
        let c1 = (e[5] as u16) << 11 | (e[6] as u16) << 5 | e[7] as u16;
        o[8..10].copy_from_slice(&c0.to_le_bytes());
        o[10..12].copy_from_slice(&c1.to_le_bytes());
        let q = &t::BC3_COLOUR_QUADRANT;
        let (top, bot) = (lanes[10], lanes[11]);
        bc1_quads(&mut o[12..16], [q[top[a] as usize], q[top[b] as usize], q[bot[a] as usize], q[bot[b] as usize]]);
    }
    Decoded { data: out, unknown: lost.iter().filter(|&&x| x).count(), lost, inconsistent: 0, info: Vec::new() }
}

fn bc4(lanes: &[&[u16]], n: usize) -> Decoded {
    let mut out = vec![0u8; n * 8];
    let mut hist: Vec<[u8; 2]> = Vec::with_capacity(n);
    let mut lost = vec![false; n];
    for i in 0..n {
        let d = dist8(lanes[0][i]);
        let base = if d == 0 {
            [128, 128]
        } else if d <= i {
            lost[i] = lost[i - d];
            hist[i - d]
        } else {
            history().and_then(|h| h.bc4[256 + i - d]).or_else(|| find(&t::BC4_HISTORY, 256 + i - d)).unwrap_or_else(|| {
                lost[i] = true;
                [128, 128]
            })
        };
        let a0 = (base[0] as i32 + unzig(lanes[2][i] as u32)) as u8;
        let a1 = (base[1] as i32 + unzig(lanes[3][i] as u32)) as u8;
        hist.push([a0, a1]);
        let o = &mut out[i * 8..i * 8 + 8];
        o[0] = a0;
        o[1] = a1;
        let bits = alpha_bits(&t::BC4_PAIR, |r| (lanes[4 + 2 * r][i], lanes[5 + 2 * r][i]));
        o[2..8].copy_from_slice(&bits.to_le_bytes()[..6]);
    }
    Decoded { data: out, unknown: lost.iter().filter(|&&x| x).count(), lost, inconsistent: 0, info: Vec::new() }
}

fn bc5(lanes: &[&[u16]], n: usize) -> Decoded {
    let mut out = vec![0u8; n * 16];
    let mut hist: Vec<[u8; 4]> = Vec::with_capacity(n);
    let mut lost = vec![false; n];
    let (l0, red, green) = (lanes[0], lanes[2], lanes[3]);
    for i in 0..n {
        let (a, b) = (2 * i, 2 * i + 1);
        let d = (unzig(inter8(l0[a], l0[b])) & 255) as usize;
        let base = if d == 0 {
            [128; 4]
        } else if d <= i {
            lost[i] = lost[i - d];
            hist[i - d]
        } else {
            find(&t::BC5_HISTORY, 256 + i - d).unwrap_or_else(|| {
                lost[i] = true;
                [128; 4]
            })
        };
        let r = [red[a], red[b], green[a], green[b]];
        let mut e = [0u8; 4];
        for k in 0..4 {
            e[k] = (base[k] as i32 + unzig(r[k] as u32)) as u8;
        }
        hist.push(e);
        for ch in 0..2 {
            let o = &mut out[i * 16 + 8 * ch..i * 16 + 8 * ch + 8];
            o[0] = e[2 * ch];
            o[1] = e[2 * ch + 1];
            let bits = alpha_bits(&t::BC5_PAIR, |r| (lanes[4 + 4 * ch + r][a], lanes[4 + 4 * ch + r][b]));
            o[2..8].copy_from_slice(&bits.to_le_bytes()[..6]);
        }
    }
    Decoded { data: out, unknown: lost.iter().filter(|&&x| x).count(), lost, inconsistent: 0, info: Vec::new() }
}

/// The four 4-bit index residuals of one row (BC6H/BC7 lanes 11-14 style):
/// rows 0-2 take odd bits first, row 3 even bits first.
#[inline]
pub(crate) fn row_digits(sa: u16, sb: u16, y: usize) -> [u32; 4] {
    if y < 3 {
        [deint(sa, 1), deint(sa, 0), deint(sb, 1), deint(sb, 0)]
    } else {
        [deint(sa, 0), deint(sa, 1), deint(sb, 0), deint(sb, 1)]
    }
}

/// Endpoint / index reference distances of a 16-lane block.
#[inline]
fn refs16(lanes: &[&[u16]], a: usize, b: usize) -> (usize, usize) {
    let di = inter8(lanes[1][a], lanes[1][b]);
    let de = ((di as i32 + unzig(inter8(lanes[0][a], lanes[0][b]))) & 255) as usize;
    (de, di as usize)
}

// BC7 -------------------------------------------------------------------------

pub(crate) const B7_NS: [usize; 8] = [3, 2, 3, 2, 1, 1, 1, 2];
pub(crate) const B7_CB: [u32; 8] = [4, 6, 5, 7, 5, 7, 7, 5];
pub(crate) const B7_AB: [u32; 8] = [0, 0, 0, 0, 6, 8, 7, 5];
pub(crate) const B7_PK: [u8; 8] = [1, 2, 0, 1, 0, 0, 1, 1];
pub(crate) const B7_IB: [u32; 8] = [3, 3, 2, 2, 3, 2, 4, 2];
const B7_PB: [u32; 8] = [4, 6, 6, 6, 0, 0, 0, 6];
pub(crate) const B7_LANE: [[i8; 4]; 3] = [[2, 3, 4, 5], [6, 7, 8, 9], [10, 5, 9, -1]];

struct Bits {
    acc: u128,
    pos: u32,
}
impl Bits {
    #[inline]
    fn put(&mut self, v: u32, n: u32) {
        if n > 0 {
            self.acc |= ((v & ((1u32 << n) - 1)) as u128) << self.pos;
            self.pos += n;
        }
    }
}

fn bc7(lanes: &[&[u16]], n: usize) -> Decoded {
    let pre = history();
    let mut out = vec![0u8; n * 16];
    let mut eps: Vec<[u8; 24]> = Vec::with_capacity(n);
    let mut ixs: Vec<[u8; 32]> = Vec::with_capacity(n);
    let (mut eroot, mut iroot) = (vec![false; n], vec![false; n]);
    let mut inconsistent = 0;
    let rows1 = [lanes[11], lanes[12], lanes[13], lanes[14]];
    let rows2 = [lanes[6], lanes[7], lanes[8], lanes[10]];
    let l15 = lanes[15];
    for i in 0..n {
        let (a, b) = (2 * i, 2 * i + 1);
        let (s15a, s15b) = (l15[a] as usize, l15[b] as usize);
        let rank = if s15a < 32 && s15b < 16 { t::BC7_RANK_OF[s15a * 16 + s15b] } else { -1 };
        let cfg = if rank >= 0 { t::BC7_CONFIG[rank as usize] } else { [255, 0, 0, 0] };
        if cfg[0] > 7 {
            inconsistent += 1;
            eps.push([0; 24]);
            ixs.push([0; 32]);
            continue;
        }
        let (mode, part, rot, isel) = (cfg[0] as usize, cfg[1] as usize, cfg[2] as usize, cfg[3] as u32);
        let mut bad = false;
        let (de, di) = refs16(lanes, a, b);
        // Preset history is unknown: zeros, and the block is flagged.
        let rep: [u8; 24] = if de == 0 {
            [0; 24]
        } else if de <= i {
            eroot[i] = eroot[i - de];
            eps[i - de]
        } else if let Some(p) = pre {
            let k = 256 + i - de;
            eroot[i] = p.bc7_ep_bits[k].iter().all(|&b| b == 0);
            p.bc7_ep[k]
        } else {
            eroot[i] = true;
            [0; 24]
        };
        let rix: [u8; 32] = if di == 0 {
            [0; 32]
        } else if di <= i {
            iroot[i] = iroot[i - di];
            ixs[i - di]
        } else if let Some(p) = pre {
            let k = 256 + i - di;
            iroot[i] = p.bc7_ix_bits[k][..16].iter().all(|&b| b == 0);
            p.bc7_ix[k]
        } else {
            iroot[i] = true;
            [0; 32]
        };

        let ns = B7_NS[mode];
        let (cb, abits, pk) = (B7_CB[mode], B7_AB[mode], B7_PK[mode]);
        let mut w = [cb, cb, cb, abits];
        if rot != 0 {
            w[rot - 1] = abits;
            w[3] = cb;
        }
        let mut val = [[[0u32; 4]; 2]; 3];
        let mut sep = [0u8; 24];
        let mut pb = [[0u32; 2]; 3];
        for sub in 0..ns {
            for c in 0..4 {
                let li = B7_LANE[sub][c];
                let wc = w[c];
                if wc == 0 || li < 0 {
                    continue;
                }
                let bits = wc + (pk != 0 && c == 0) as u32;
                let shift = 8 - bits;
                let mask = (1i32 << bits) - 1;
                let ln = lanes[li as usize];
                let v0 = (((rep[sub * 8 + c] >> shift) as i32 + unzig(ln[a] as u32)) & mask) as u32;
                let v1 = (((rep[sub * 8 + 4 + c] >> shift) as i32 + unzig(ln[b] as u32)) & mask) as u32;
                val[sub][0][c] = v0;
                val[sub][1][c] = v1;
                sep[sub * 8 + c] = (v0 << shift) as u8;
                sep[sub * 8 + 4 + c] = (v1 << shift) as u8;
            }
            match pk {
                1 => pb[sub] = [val[sub][0][0] & 1, val[sub][1][0] & 1],
                2 => {
                    let p0 = val[sub][0][0] & 1;
                    pb[sub] = [p0, p0];
                    if val[sub][1][0] & 1 != p0 {
                        bad = true;
                    }
                }
                _ => {}
            }
        }
        for sub in ns..3 {
            let (lo, hi) = sep.split_at_mut(sub * 8);
            hi[..8].copy_from_slice(&lo[..8]);
        }
        eps.push(sep);

        let b1 = B7_IB[mode];
        let (b1_shift, b1_mask) = (4 - b1, (1i32 << b1) - 1);
        let two = mode == 4 || mode == 5;
        let mut s1 = [0u32; 16];
        let mut s2 = [0u32; 16];
        let mut six = [0u8; 32];
        for y in 0..4 {
            let d = row_digits(rows1[y][a], rows1[y][b], y);
            for off in 0..4 {
                let p = y * 4 + off;
                let iv = (((rix[p] as u32 >> b1_shift) as i32 + unzig(d[off])) & b1_mask) as u32;
                s1[p] = iv;
                six[p] = (iv << b1_shift) as u8;
            }
            if two {
                let d = row_digits(rows2[y][a], rows2[y][b], y);
                for off in 0..4 {
                    let p = y * 4 + off;
                    let iv = (((rix[16 + p] >> 2) as i32 + unzig(d[off])) & 3) as u32;
                    s2[p] = iv;
                    six[16 + p] = (iv << 2) as u8;
                }
            }
        }
        ixs.push(six);

        if mode == 0 && part == 0 && sep.iter().all(|&x| x == 0) && s1.iter().all(|&x| x == 0) {
            // all-zero block: left as zeros (BC7 reserved encoding)
            if bad {
                inconsistent += 1;
            }
            continue;
        }

        let subset = |p: usize| -> usize {
            match ns {
                2 => ((t::BC7_PART2[part] >> p) & 1) as usize,
                3 => ((t::BC7_PART3[part] >> (2 * p)) & 3) as usize,
                _ => 0,
            }
        };
        let anchors: [usize; 3] = match ns {
            1 => [0, 99, 99],
            2 => [0, t::BC7_ANCHOR2[part] as usize, 99],
            _ => [0, t::BC7_ANCHOR3A[part] as usize, t::BC7_ANCHOR3B[part] as usize],
        };
        let (pri, ib, sec, ib2): (&[u32; 16], u32, Option<&[u32; 16]>, u32) = match mode {
            4 => (&s2, 2, Some(&s1), 3),
            5 => (&s1, 2, Some(&s2), 2),
            _ => (&s1, b1, None, 0),
        };

        let mut o = Bits { acc: 1u128 << mode, pos: mode as u32 + 1 };
        o.put(part as u32, B7_PB[mode]);
        if two {
            o.put(rot as u32, 2);
        }
        if mode == 4 {
            o.put(isel, 1);
        }
        for c in 0..3 {
            let tc = if rot != 0 && c == rot - 1 { 3 } else { c };
            let drop = pk != 0 && tc == 0;
            for sub in 0..ns {
                for e in 0..2 {
                    let v = val[sub][e][tc];
                    o.put(if drop { v >> 1 } else { v }, cb);
                }
            }
        }
        if abits != 0 {
            let tc = if rot != 0 { rot - 1 } else { 3 };
            let drop = pk != 0 && tc == 0;
            for sub in 0..ns {
                for e in 0..2 {
                    let v = val[sub][e][tc];
                    o.put(if drop { v >> 1 } else { v }, abits);
                }
            }
        }
        match pk {
            1 => {
                for sub in 0..ns {
                    o.put(pb[sub][0], 1);
                    o.put(pb[sub][1], 1);
                }
            }
            2 => {
                for sub in 0..ns {
                    o.put(pb[sub][0], 1);
                }
            }
            _ => {}
        }
        for p in 0..16 {
            let pv = pri[p];
            if p == anchors[subset(p)] {
                if pv >> (ib - 1) != 0 {
                    bad = true;
                }
                o.put(pv, ib - 1);
            } else {
                o.put(pv, ib);
            }
        }
        if let Some(sec) = sec {
            for p in 0..16 {
                let sv = sec[p];
                if p == 0 {
                    if sv >> (ib2 - 1) != 0 {
                        bad = true;
                    }
                    o.put(sv, ib2 - 1);
                } else {
                    o.put(sv, ib2);
                }
            }
        }
        if o.pos != 128 {
            bad = true;
        }
        if bad {
            inconsistent += 1;
        }
        out[16 * i..16 * i + 16].copy_from_slice(&o.acc.to_le_bytes());
    }
    let lost: Vec<bool> = (0..n).map(|i| eroot[i] || iroot[i]).collect();
    Decoded { data: out, unknown: lost.iter().filter(|&&x| x).count(), lost, inconsistent, info: Vec::new() }
}

// BC6H ------------------------------------------------------------------------

/// Mode index (1..14) -> D3D11 mode bits.
pub(crate) const B6_CODE: [u32; 15] = [0, 0x00, 0x01, 0x02, 0x06, 0x0A, 0x0E, 0x12, 0x16, 0x1A, 0x1E, 0x03, 0x07, 0x0B, 0x0F];

/// (transformed, base bits, deltas, regions, header layout)
pub(crate) fn b6_info(code: u32) -> (bool, u32, [u32; 3], usize, &'static str) {
    match code {
        0x00 => (true, 10, [5, 5, 5], 2, "gy4 by4 bz4 rw10 gw10 bw10 rx5 gz4 gy4_ gx5 bz0 gz4_ bx5 bz1 by4_ ry5 bz2 rz5 bz3 d"),
        0x01 => (true, 7, [6, 6, 6], 2, "gy5 gz4 gz5 rw7 bz0 bz1 by4 gw7 by5 bz2 gy4 bw7 bz3 bz5 bz4 rx6 gy4_ gx6 gz4_ bx6 by4_ ry6 rz6 d"),
        0x02 => (true, 11, [5, 4, 4], 2, "rw10 gw10 bw10 rx5 rw:10 gy4_ gx4 gw:10 bz0 gz4_ bx4 bw:10 bz1 by4_ ry5 bz2 rz5 bz3 d"),
        0x06 => (true, 11, [4, 5, 4], 2, "rw10 gw10 bw10 rx4 rw:10 gz4 gy4_ gx5 gw:10 gz4_ bx4 bw:10 bz1 by4_ ry4 bz0 bz2 rz4 gy4 bz3 d"),
        0x0A => (true, 11, [4, 4, 5], 2, "rw10 gw10 bw10 rx4 rw:10 by4 gy4_ gx4 gw:10 bz0 gz4_ bx5 bw:10 by4_ ry4 bz1 bz2 rz4 bz4 bz3 d"),
        0x0E => (true, 9, [5, 5, 5], 2, "rw9 by4 gw9 gy4 bw9 bz4 rx5 gz4 gy4_ gx5 bz0 gz4_ bx5 bz1 by4_ ry5 bz2 rz5 bz3 d"),
        0x12 => (true, 8, [6, 5, 5], 2, "rw8 gz4 by4 gw8 bz2 gy4 bw8 bz3 bz4 rx6 gy4_ gx5 bz0 gz4_ bx5 bz1 by4_ ry6 rz6 d"),
        0x16 => (true, 8, [5, 6, 5], 2, "rw8 bz0 by4 gw8 gy5 gy4 bw8 gz5 bz4 rx5 gz4 gy4_ gx6 gz4_ bx5 bz1 by4_ ry5 bz2 rz5 bz3 d"),
        0x1A => (true, 8, [5, 5, 6], 2, "rw8 bz1 by4 gw8 by5 gy4 bw8 bz5 bz4 rx5 gz4 gy4_ gx5 bz0 gz4_ bx6 by4_ ry5 bz2 rz5 bz3 d"),
        0x1E => (false, 6, [6, 6, 6], 2, "rw6 gz4 bz0 bz1 by4 gw6 gy5 by5 bz2 gy4 bw6 gz5 bz3 bz5 bz4 rx6 gy4_ gx6 gz4_ bx6 by4_ ry6 rz6 d"),
        0x03 => (false, 10, [10, 10, 10], 1, "rw10 gw10 bw10 rx10 gx10 bx10"),
        0x07 => (true, 11, [9, 9, 9], 1, "rw10 gw10 bw10 rx9 rw:10 gx9 gw:10 bx9 bw:10"),
        0x0B => (true, 12, [8, 8, 8], 1, "rw10 gw10 bw10 rx8 rw:11-10 gx8 gw:11-10 bx8 bw:11-10"),
        _ => (true, 16, [4, 4, 4], 1, "rw10 gw10 bw10 rx4 rw:15-10 gx4 gw:15-10 bx4 bw:15-10"),
    }
}

/// Header plan: (channel or -1 for the partition, endpoint slot, bit, position), header bits.
pub(crate) fn b6_plan(code: u32) -> (Vec<(i8, u8, u8, u8)>, u32) {
    let mut pos: u32 = if code < 2 { 2 } else { 5 };
    let mut plan = Vec::new();
    let field = |name: &str| -> (i8, u8) {
        let c = match &name[..1] { "r" => 0, "g" => 1, _ => 2 };
        let q = match &name[1..2] { "w" => 0, "x" => 1, "y" => 2, _ => 3 };
        (c, q)
    };
    for tok in b6_info(code).4.split_whitespace() {
        if tok == "d" {
            for bb in 0..5 {
                plan.push((-1, 0, bb, (pos + bb as u32) as u8));
            }
            pos += 5;
        } else if let Some((name, span)) = tok.split_once(':') {
            let (c, q) = field(name);
            let (hi, lo): (u32, u32) = match span.split_once('-') {
                Some((h, l)) => (h.parse().unwrap(), l.parse().unwrap()),
                None => (span.parse().unwrap(), span.parse().unwrap()),
            };
            let mut bb = hi as i32;
            while bb >= lo as i32 {
                plan.push((c, q, bb as u8, pos as u8));
                pos += 1;
                bb -= 1;
            }
        } else if let Some(stem) = tok.strip_suffix('_') {
            let (c, q) = field(&stem[..2]);
            let n: u32 = stem[2..].parse().unwrap();
            for bb in 0..n {
                plan.push((c, q, bb as u8, (pos + bb) as u8));
            }
            pos += n;
        } else {
            let name = &tok[..2];
            let (c, q) = field(name);
            let n: u32 = tok[2..].parse().unwrap();
            if matches!(&name[1..], "w" | "x") || name == "ry" || name == "rz" {
                for bb in 0..n {
                    plan.push((c, q, bb as u8, (pos + bb) as u8));
                }
                pos += n;
            } else {
                plan.push((c, q, n as u8, pos as u8));
                pos += 1;
            }
        }
    }
    (plan, pos)
}

pub(crate) fn b6_stream_w(mode: usize, w: u32) -> u32 {
    match mode {
        13 => (w & 0x3FF) | ((w >> 10) & 1) << 11 | ((w >> 11) & 1) << 10,
        14 => {
            let mut o = w & 0x3FF;
            for bb in 10..16 {
                o |= ((w >> bb) & 1) << (25 - bb);
            }
            o
        }
        _ => w,
    }
}

fn bc6h(lanes: &[&[u16]], n: usize) -> Decoded {
    const NONE_E: [[u32; 4]; 3] = [[0x8000, 0x200, 0x20, 0x20]; 3];
    let plans: Vec<(Vec<(i8, u8, u8, u8)>, u32)> = B6_CODE.iter().map(|&c| b6_plan(c)).collect();
    let mut out = vec![0u8; n * 16];
    let mut st_e: Vec<[[u32; 4]; 3]> = Vec::with_capacity(n);
    let mut st_i: Vec<[u32; 16]> = Vec::with_capacity(n);
    let (mut eroot, mut iroot) = (vec![false; n], vec![false; n]);
    let mut inconsistent = 0;
    let mut info = vec![0u32; n];
    let mut modes = vec![0u32; n];
    let l15 = lanes[15];
    for i in 0..n {
        let (a, b) = (2 * i, 2 * i + 1);
        let m_sym = l15[a] as usize;
        if m_sym > 13 {
            inconsistent += 1;
            st_e.push(NONE_E);
            st_i.push([8; 16]);
            continue;
        }
        let mode = m_sym + 1;
        let code = B6_CODE[mode];
        let (trans, base, deltas, reg, _) = b6_info(code);
        let part = (l15[b] as u32).min(31);
        let (de, di) = refs16(lanes, a, b);
        let eref = if de == 0 {
            NONE_E
        } else if de <= i {
            eroot[i] = eroot[i - de];
            st_e[i - de]
        } else {
            let k = 256 + i - de;
            match history().filter(|h| h.bc6h_ecls[k] != 0) {
                Some(h) => h.bc6h_ep[k],
                None => {
                    eroot[i] = true;
                    [[0; 4]; 3]
                }
            }
        };
        let iref = if di == 0 {
            [8u32; 16]
        } else if di <= i {
            iroot[i] = iroot[i - di];
            st_i[i - di]
        } else {
            let k = 256 + i - di;
            match history().filter(|h| h.bc6h_icls[k] != 0) {
                Some(h) => h.bc6h_ix[k],
                None => {
                    iroot[i] = true;
                    [0; 16]
                }
            }
        };
        let mw = (1i64 << base) - 1;
        let w_shift = 16 - base;
        let mut f = [[0u32; 4]; 3];
        let mut stb = [[0u32; 4]; 3];
        for c in 0..3 {
            let dc = deltas[c];
            let md = (1i64 << dc) - 1;
            let sh = if dc < 6 { 6 - dc } else { 0 };
            let dz0 = unzig(inter(lanes[2 + c][a], lanes[2 + c][b], 8)) as i64;
            let dz1 = unzig(inter(lanes[5 + c][a], lanes[5 + c][b], 5)) as i64;
            let dz2 = unzig(lanes[8 + c][a] as u32) as i64;
            let dz3 = unzig(lanes[8 + c][b] as u32) as i64;
            let [r0, r1, r2, r3] = eref[c];
            let w = (((r0 >> w_shift) as i64 + dz0) & mw) as u32;
            let x = (((r1 >> (10 - dc)) as i64 + dz1) & md) as u32;
            let y = (((r2 >> sh) as i64 + dz2) & md) as u32;
            let z = (((r3 >> sh) as i64 + dz3) & md) as u32;
            let x10 = x << (10 - dc);
            if reg == 2 {
                stb[c] = [w << w_shift, x10, y << sh, z << sh];
                f[c] = [b6_stream_w(mode, w), x, y, z];
            } else {
                let v = (w & md as u32) ^ if trans { 1 << (dc - 1) } else { 0 };
                let low_y = if dc >= 6 { v >> (dc - 6) } else { v << sh };
                stb[c] = [w << w_shift, x10, low_y, x10 >> 4];
                f[c] = [b6_stream_w(mode, w), x, 0, 0];
            }
        }
        st_e.push(stb);
        modes[i] = mode as u32;
        info[i] = mode as u32 | (de as u32) << 8 | (eroot[i] as u32) << 16 | if de > 0 && de <= i { modes[i - de] << 20 } else { 0 };
        let ibits = if reg == 2 { 3 } else { 4 };
        let imask = (1i32 << ibits) - 1;
        let ishift = 4 - ibits;
        let mut ix = [0u32; 16];
        let mut si = [0u32; 16];
        for y in 0..4 {
            let d = row_digits(lanes[11 + y][a], lanes[11 + y][b], y);
            for off in 0..4 {
                let p = y * 4 + off;
                let v = (((iref[p] >> ishift) as i32 + unzig(d[off])) & imask) as u32;
                ix[p] = v;
                si[p] = v << ishift;
            }
        }
        st_i.push(si);
        let second = if reg == 2 { t::BC7_ANCHOR2[part as usize] as usize } else { 99 };
        let mut bad = ix[0] & if reg == 2 { 4 } else { 8 } != 0;
        if reg == 2 && ix[second] & 4 != 0 {
            bad = true;
        }
        let (plan, header) = &plans[mode];
        let mut acc: u128 = code as u128;
        for &(c, q, bit, pos) in plan {
            let v = if c < 0 { part >> bit } else { f[c as usize][q as usize] >> bit };
            if v & 1 != 0 {
                acc |= 1u128 << pos;
            }
        }
        let mut at = *header;
        for p in 0..16 {
            let nb = if p == 0 || p == second { ibits - 1 } else { ibits };
            acc |= ((ix[p] & ((1 << nb) - 1)) as u128) << at;
            at += nb;
        }
        if bad {
            inconsistent += 1;
        }
        out[16 * i..16 * i + 16].copy_from_slice(&acc.to_le_bytes());
    }
    let lost: Vec<bool> = (0..n).map(|i| eroot[i] || iroot[i]).collect();
    Decoded { data: out, unknown: lost.iter().filter(|&&x| x).count(), lost, inconsistent, info }
}
