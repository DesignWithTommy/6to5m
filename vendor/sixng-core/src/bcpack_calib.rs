//! Self-calibration of the BCPack preset ("built-in") history from the
//! user's own stores. Port of the pool self-calibration in Leonida's
//! `tools/bcpack_history_calib.py` (GPL-3.0, github.com/ItsNotRexed/Leonida).
//!
//! The hardware starts every chunk with a fixed 256-entry history that blocks
//! can predict from (`d > i`). It is not public, so it is fitted:
//! - BC6H/BC7 indices: exact anchor-MSB constraints (an anchor index must
//!   have its top bit clear) plus intra-block smoothness votes;
//! - BC7 endpoints: shared p-bit parity plus a fit against the plain
//!   (zlib/stored) sibling mip of textures that mix BCPack and plain mips;
//! - BC4 endpoints / BC6H base endpoints: the same sibling-mip fit;
//! - BC6H transformed deltas: centring.
//! The result is game data: it is cached locally, never shipped.
use crate::bcpack::{self as bp, History};
use crate::bcpack_tables as t;
use crate::ttpf::{Ttpf, TtpfSet, TtpfTex};
use std::collections::{HashMap, HashSet};

const MAX_BLOCKS: usize = 256;

/// Up to three (bits, residual) steps from a preset value to a block value.
#[derive(Clone, Copy, Debug)]
struct Ops {
    n: u8,
    op: [(u8, i32); 3],
}
impl Ops {
    fn one(bits: u8, dz: i32) -> Ops {
        Ops { n: 1, op: [(bits, dz), (0, 0), (0, 0)] }
    }
    fn last(&self) -> (u8, i32) {
        self.op[self.n as usize - 1]
    }
    /// Chain a further step; `None` past three distinct widths.
    fn then(&self, bits: u8, dz: i32, mask: i32) -> Option<Ops> {
        let mut o = *self;
        let (lb, lz) = self.last();
        if lb == bits {
            o.op[o.n as usize - 1] = (bits, (lz + dz) & mask);
        } else if self.n < 3 {
            o.op[o.n as usize] = (bits, dz);
            o.n += 1;
        } else {
            return None;
        }
        Some(o)
    }
    /// BC7: apply to an 8-bit left-aligned value.
    fn eval8(&self, h: i32) -> i32 {
        let mut v = h;
        for &(bits, dz) in &self.op[..self.n as usize] {
            let sh = 8 - bits as i32;
            let m = (1 << bits) - 1;
            v = (((v >> sh) + dz) & m) << sh;
        }
        v
    }
}

/// `(left-aligned value, exact bits)` shared by every set bit of a 16-bit mask.
fn lead_bits_4(mask: u16) -> (u8, u8) {
    if mask == 0 {
        return (0, 0);
    }
    let first = mask.trailing_zeros() as u8;
    let mut diff = 0u8;
    for h in 0..16u8 {
        if mask >> h & 1 != 0 {
            diff |= h ^ first;
        }
    }
    let bits = 4 - (8 - diff.leading_zeros() as u8);
    let val = if bits > 0 { first & (0xF << (4 - bits)) } else { 0 };
    (val, bits)
}

fn survivors(mask: u16) -> Vec<u8> {
    (0..16u8).filter(|h| mask >> h & 1 != 0).collect()
}

/// Per-block 2x2 quadrant targets from the sibling mip: RGBA (BC7), R (BC4),
/// unquantized RGB (BC6H).
type Quads = [[f64; 4]; 4];

/// Index-history accumulators shared by BC6H (16 slots) and BC7 (32).
pub(crate) struct IxAcc {
    pub(crate) valid: Vec<[u16; 32]>,
    pub(crate) obs: Vec<[u32; 32]>,
    pub(crate) votes: Vec<[[u32; 16]; 32]>,
}
impl IxAcc {
    pub(crate) fn new() -> IxAcc {
        IxAcc { valid: vec![[0xFFFF; 32]; 256], obs: vec![[0; 32]; 256], votes: vec![[[0; 16]; 32]; 256] }
    }
    /// Anchor constraint: the predicted 4-bit value must have bit 3 clear.
    fn anchor(&mut self, k: usize, p: usize, mp: &[u8; 16]) {
        let mut mask = 0u16;
        for h in 0..16 {
            if mp[h] < 8 {
                mask |= 1 << h;
            }
        }
        if self.valid[k][p] & mask != 0 {
            self.valid[k][p] &= mask;
        }
        self.obs[k][p] += 1;
    }
    /// Resolve slots `[p0, p0 + 16)` of entry `k`: `want` bits per slot.
    pub(crate) fn resolve(&self, k: usize, p0: usize, want: u8, ix: &mut [u8], ixb: &mut [u8]) -> usize {
        let mut prov = 0;
        for p in p0..p0 + 16 {
            let (v, b) = lead_bits_4(self.valid[k][p]);
            if b > 0 {
                ix[p] = v;
                ixb[p] = b;
            }
        }
        let s0 = survivors(self.valid[k][p0]);
        let v0 = if ixb[p0] == 4 || (want == 2 && ixb[p0] >= 2) { ix[p0] } else { s0.get(s0.len() / 2).copied().unwrap_or(0) };
        if self.obs[k][p0..p0 + 16].iter().any(|&x| x > 0) {
            for p in p0..p0 + 16 {
                if ixb[p] >= want {
                    continue;
                }
                let mask = self.valid[k][p];
                let votes = &self.votes[k][p];
                let best = if p == p0 {
                    v0
                } else if votes.iter().any(|&x| x > 0) {
                    let mut best = None;
                    for h in 0..16u8 {
                        if mask >> h & 1 != 0 {
                            let sc = votes[(h.wrapping_sub(v0) & 15) as usize];
                            if best.map_or(true, |(_, bs)| sc > bs) {
                                best = Some((h, sc));
                            }
                        }
                    }
                    best.map_or(v0, |b| b.0)
                } else {
                    let s = survivors(mask);
                    s.get(s.len() / 2).copied().unwrap_or(0)
                };
                ix[p] = best;
                ixb[p] = want;
                prov += 1;
            }
        }
        prov
    }
}

/// Index digit of pixel `p` (row `p / 4`) of a 16-lane block.
#[inline]
fn digit(rows: &[&[u16]; 4], p: usize, a: usize, b: usize) -> i32 {
    let (y, off) = (p / 4, p % 4);
    bp::unzig(bp::row_digits(rows[y][a], rows[y][b], y)[off])
}

/// Anchor and smoothness constraints of one block's index chain.
/// `pmap`: the anchor maps carried down the chain (`None` at the root).
#[allow(clippy::too_many_arguments)]
pub(crate) fn index_constraints(
    acc: &mut IxAcc,
    ki: usize,
    pmap: Option<&HashMap<usize, [u8; 16]>>,
    rows: &[&[u16]; 4],
    a: usize,
    b: usize,
    anchors: &[usize],
    ibits: u32,
    slot0: usize,
    same_subset: &dyn Fn(usize) -> bool,
    cur: &mut HashMap<usize, [u8; 16]>,
) {
    let sh = 4 - ibits;
    let m = (1i32 << ibits) - 1;
    for &p in anchors {
        let key = slot0 + p;
        let prev = match pmap {
            Some(pm) => match pm.get(&key) {
                Some(v) => Some(*v),
                None => continue,
            },
            None => None,
        };
        let dv = digit(rows, p, a, b);
        let mut mp = [0u8; 16];
        for h in 0..16 {
            let base = prev.map_or(h as i32, |v| v[h] as i32);
            mp[h] = ((((base >> sh) + dv) & m) << sh) as u8;
        }
        cur.insert(key, mp);
        acc.anchor(ki, key, &mp);
    }
    if pmap.is_none() {
        let dv0 = digit(rows, 0, a, b);
        for p in 1..16 {
            acc.obs[ki][slot0 + p] += 1;
            if same_subset(p) {
                let diff_top = ((dv0 - digit(rows, p, a, b)) & m) << sh;
                for lo in 0..1 << sh {
                    acc.votes[ki][slot0 + p][((diff_top | lo) & 15) as usize] += 1;
                }
            }
        }
    }
}

// BC7 -------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Ep7Obs {
    o0: Ops,
    o1: Ops,
    tgt: f64,
    wt: f64,
}

struct Bc7Cal {
    ix: IxAcc,
    pbit: HashMap<(usize, usize), [u32; 2]>,
    ep: HashMap<(usize, usize, usize), Vec<Ep7Obs>>,
}

impl Bc7Cal {
    fn new() -> Bc7Cal {
        Bc7Cal { ix: IxAcc::new(), pbit: HashMap::new(), ep: HashMap::new() }
    }

    fn add_chunk(&mut self, lanes: &[&[u16]], nb: usize, quads: Option<&dyn Fn(usize) -> Option<Quads>>) {
        let (l0, l1, l15) = (lanes[0], lanes[1], lanes[15]);
        let rows1 = [lanes[11], lanes[12], lanes[13], lanes[14]];
        let rows2 = [lanes[6], lanes[7], lanes[8], lanes[10]];
        let mut eroot = vec![-1i32; nb];
        let mut echain: Vec<Option<[Option<(u8, Ops)>; 24]>> = vec![None; nb];
        let mut iroot = vec![-1i32; nb];
        let mut imap: Vec<Option<HashMap<usize, [u8; 16]>>> = vec![None; nb];
        for i in 0..nb {
            let (a, b) = (2 * i, 2 * i + 1);
            let (sa, sb) = (l15[a] as usize, l15[b] as usize);
            let rank = if sa < 32 && sb < 16 { t::BC7_RANK_OF[sa * 16 + sb] } else { -1 };
            if rank < 0 || t::BC7_INFERRED.contains(&(rank as u16)) {
                continue;
            }
            let [mode, part, rot, _] = t::BC7_CONFIG[rank as usize].map(|x| x as usize);
            if mode > 7 {
                continue;
            }
            let di = bp::inter8(l1[a], l1[b]) as usize;
            let de = ((di as i32 + bp::unzig(bp::inter8(l0[a], l0[b]))) & 255) as usize;
            let ns = bp::B7_NS[mode];

            if di > 0 {
                let (ki, pmap) = if di > i { ((256 + i - di) as i32, None) } else { let k = iroot[i - di]; (k, if k >= 0 { imap[i - di].as_ref() } else { None }) };
                if ki >= 0 {
                    let ki_u = ki as usize;
                    iroot[i] = ki;
                    let anchors: Vec<usize> = if mode == 4 || mode == 5 {
                        vec![0]
                    } else {
                        match ns {
                            1 => vec![0],
                            2 => vec![0, t::BC7_ANCHOR2[part] as usize],
                            _ => vec![0, t::BC7_ANCHOR3A[part] as usize, t::BC7_ANCHOR3B[part] as usize],
                        }
                    };
                    let sub = |p: usize| -> usize {
                        match ns {
                            2 => ((t::BC7_PART2[part] >> p) & 1) as usize,
                            3 => ((t::BC7_PART3[part] >> (2 * p)) & 3) as usize,
                            _ => 0,
                        }
                    };
                    let same = |p: usize| sub(p) == sub(0);
                    let pm_owned = pmap.cloned();
                    let mut cur = HashMap::new();
                    index_constraints(&mut self.ix, ki_u, pm_owned.as_ref(), &rows1, a, b, &anchors, bp::B7_IB[mode], 0, &same, &mut cur);
                    if mode == 4 || mode == 5 {
                        index_constraints(&mut self.ix, ki_u, pm_owned.as_ref(), &rows2, a, b, &[0], 2, 16, &|_| true, &mut cur);
                    }
                    imap[i] = Some(cur);
                }
            }

            if de == 0 {
                continue;
            }
            let (ke, pchain) = if de > i { ((256 + i - de) as i32, None) } else { let k = eroot[i - de]; (k, if k >= 0 { echain[i - de] } else { None }) };
            if ke < 0 {
                continue;
            }
            let ke_u = ke as usize;
            eroot[i] = ke;
            let (cb, abits, pk) = (bp::B7_CB[mode], bp::B7_AB[mode], bp::B7_PK[mode]);
            let mut w = [cb, cb, cb, abits];
            if rot != 0 {
                w[rot - 1] = abits;
                w[3] = cb;
            }
            let mut cur_e: [Option<(u8, Ops)>; 24] = [None; 24];
            for sub in 0..ns {
                let lrow = bp::B7_LANE[sub];
                for c in 0..4 {
                    let li = lrow[c];
                    if w[c] == 0 || li < 0 {
                        continue;
                    }
                    let bits = (w[c] + (pk != 0 && c == 0) as u32) as u8;
                    let mb = (1i32 << bits) - 1;
                    let ln = lanes[li as usize];
                    for (e, s) in [(0, a), (1, b)] {
                        let dz = bp::unzig(ln[s] as u32) & mb;
                        let slot = sub * 8 + e * 4 + c;
                        cur_e[slot] = match &pchain {
                            None => Some((slot as u8, Ops::one(bits, dz))),
                            Some(pc) => pc[slot].and_then(|(ss, ops)| ops.then(bits, dz, mb).map(|o| (ss, o))),
                        };
                    }
                }
                if pk == 2 && pchain.is_none() {
                    let l = lanes[lrow[0] as usize];
                    let par = ((bp::unzig(l[a] as u32) ^ bp::unzig(l[b] as u32)) & 1) as usize;
                    self.pbit.entry((ke_u, sub)).or_default()[par] += 1;
                }
            }
            for sub in ns..3 {
                for j in 0..8 {
                    cur_e[sub * 8 + j] = cur_e[j];
                }
            }
            echain[i] = Some(cur_e);

            let Some(q) = quads.and_then(|f| f(i)) else { continue };
            let spread = (0..4).map(|c| q.iter().map(|p| p[c]).fold(f64::MIN, f64::max) - q.iter().map(|p| p[c]).fold(f64::MAX, f64::min)).fold(f64::MIN, f64::max);
            let wt = if spread <= 6.0 { 4.0 } else if spread <= 18.0 { 2.0 } else { 0.5 };
            let sub_of = |px: usize| -> usize {
                match ns {
                    2 => ((t::BC7_PART2[part] >> px) & 1) as usize,
                    3 => ((t::BC7_PART3[part] >> (2 * px)) & 3) as usize,
                    _ => 0,
                }
            };
            for sub in 0..ns {
                let rgba = if ns == 1 {
                    let mut s = [0.0; 4];
                    for c in 0..4 {
                        s[c] = q.iter().map(|p| p[c]).sum::<f64>() / 4.0;
                    }
                    s
                } else {
                    let mut s = [0.0; 4];
                    let mut n = 0;
                    for px in 0..16 {
                        if sub_of(px) == sub {
                            let qi = (px / 4 / 2) * 2 + (px % 4) / 2;
                            for c in 0..4 {
                                s[c] += q[qi][c];
                            }
                            n += 1;
                        }
                    }
                    if n == 0 {
                        continue;
                    }
                    s.map(|v| v / n as f64)
                };
                for c in 0..4 {
                    let (Some((s0, o0)), Some((s1, o1))) = (cur_e[sub * 8 + c], cur_e[sub * 8 + 4 + c]) else { continue };
                    let (s0, s1) = (s0 as usize, s1 as usize);
                    if s0 / 8 != s1 / 8 || s0 % 8 != c || s1 % 8 != 4 + c {
                        continue;
                    }
                    let lst = self.ep.entry((ke_u, s0 / 8, c)).or_default();
                    if lst.len() < 60 {
                        lst.push(Ep7Obs { o0, o1, tgt: rgba[c], wt });
                    }
                }
            }
        }
    }

    fn fit_pair(obs: &[Ep7Obs]) -> (i32, i32) {
        let err = |h0: i32, h1: i32| -> f64 {
            let mut e = 0.0;
            for o in obs {
                let (e0, e1) = (o.o0.eval8(h0) as f64, o.o1.eval8(h1) as f64);
                let d = (0.5 * (e0 + e1) - o.tgt).abs();
                e += o.wt * (d.min(40.0) + 0.01 * (e0 - e1).abs());
            }
            e
        };
        coarse_to_fine(|h0, h1, mid| err(h0 | mid, h1 | mid))
    }

    fn build(&self, h: &mut History) {
        for k in 0..256 {
            let (ix, ixb) = (&mut h.bc7_ix[k], &mut h.bc7_ix_bits[k]);
            self.ix.resolve(k, 0, 4, ix, ixb);
            self.ix.resolve(k, 16, 2, ix, ixb);
            for sub in 0..3 {
                for c in 0..4 {
                    let Some(obs) = self.ep.get(&(k, sub, c)) else { continue };
                    if obs.is_empty() {
                        continue;
                    }
                    let (b0, mut b1) = Self::fit_pair(obs);
                    if c == 0 {
                        if let Some(&[c0, c1]) = self.pbit.get(&(k, sub)) {
                            if (c0 > 0) != (c1 > 0) {
                                let req = if c0 > 0 { 0 } else { 1 };
                                if ((b0 >> 1) ^ (b1 >> 1)) & 1 != req {
                                    b1 ^= 2;
                                }
                            }
                        }
                    }
                    let depth = if obs.len() >= 4 { 8 } else { 5 };
                    for (s, v) in [(sub * 8 + c, b0), (sub * 8 + 4 + c, b1)] {
                        h.bc7_ep[k][s] = v as u8;
                        h.bc7_ep_bits[k][s] = depth;
                    }
                }
            }
            // mid-range fill below the known bits
            for s in 0..24 {
                let d = h.bc7_ep_bits[k][s] as u32;
                if d > 0 && d < 8 {
                    let v = h.bc7_ep[k][s] as u32;
                    h.bc7_ep[k][s] = ((v >> (8 - d) << (8 - d)) | (1 << (7 - d))) as u8;
                }
            }
        }
    }
}

/// Search two 8-bit values: 16-step grid, then refine the best 4 by halving.
/// `err(h0, h1, mid)` scores a cell of size `2 * mid` at (h0, h1).
fn coarse_to_fine(err: impl Fn(i32, i32, i32) -> f64) -> (i32, i32) {
    let mut c: Vec<(f64, i32, i32)> = Vec::with_capacity(256);
    for h0 in (0..256).step_by(16) {
        for h1 in (0..256).step_by(16) {
            c.push((err(h0, h1, 8), h0, h1));
        }
    }
    let sort = |v: &mut Vec<(f64, i32, i32)>| v.sort_by(|x, y| x.partial_cmp(y).unwrap());
    sort(&mut c);
    c.truncate(4);
    for step in [8, 4, 2, 1] {
        let mid = step / 2;
        let mut seen = HashSet::new();
        let mut nxt = Vec::new();
        for &(_, b0, b1) in &c {
            for d0 in [0, step] {
                for d1 in [0, step] {
                    let (h0, h1) = ((b0 + d0) & 255, (b1 + d1) & 255);
                    if seen.insert((h0, h1)) {
                        nxt.push((err(h0, h1, mid), h0, h1));
                    }
                }
            }
        }
        sort(&mut nxt);
        nxt.truncate(4);
        c = nxt;
    }
    (c[0].1, c[0].2)
}

// BC4 -------------------------------------------------------------------------

struct Bc4Obs {
    d: [i32; 2],
    tgt: [f64; 4],
    qw: [f64; 4],
    wt: f64,
}

#[derive(Default)]
struct Bc4Cal {
    obs: HashMap<usize, Vec<Bc4Obs>>,
}

impl Bc4Cal {
    fn add_chunk(&mut self, lanes: &[&[u16]], nb: usize, quads: &dyn Fn(usize) -> Option<Quads>) {
        const ALUT: [f64; 8] = [0.0, 1.0, 1.0 / 7.0, 2.0 / 7.0, 3.0 / 7.0, 4.0 / 7.0, 5.0 / 7.0, 6.0 / 7.0];
        let mut roots = vec![-1i32; nb];
        let mut deltas = vec![[0i32; 2]; nb];
        for i in 0..nb {
            let d = bp::dist8(lanes[0][i]);
            let dz = [bp::unzig(lanes[2][i] as u32), bp::unzig(lanes[3][i] as u32)];
            if d == 0 {
                continue;
            }
            let (root, acc) = if d > i {
                (256 + i - d, dz.map(|x| x.rem_euclid(256)))
            } else {
                if roots[i - d] < 0 {
                    continue;
                }
                let p = deltas[i - d];
                (roots[i - d] as usize, [(p[0] + dz[0]).rem_euclid(256), (p[1] + dz[1]).rem_euclid(256)])
            };
            roots[i] = root as i32;
            deltas[i] = acc;
            let Some(q) = quads(i) else { continue };
            let mut qw = [0.0; 4];
            for r in 0..4 {
                let l = t::BC4_PAIR[lanes[4 + 2 * r][i] as usize];
                let rr = t::BC4_PAIR[lanes[5 + 2 * r][i] as usize];
                let qi = (r / 2) * 2;
                qw[qi] += ALUT[l.0 as usize] + ALUT[l.1 as usize];
                qw[qi + 1] += ALUT[rr.0 as usize] + ALUT[rr.1 as usize];
            }
            let tgt = [q[0][0], q[1][0], q[2][0], q[3][0]];
            let spread = tgt.iter().cloned().fold(f64::MIN, f64::max) - tgt.iter().cloned().fold(f64::MAX, f64::min);
            let wt = if spread <= 8.0 { 4.0 } else if spread <= 24.0 { 2.0 } else { 0.75 };
            let lst = self.obs.entry(root).or_default();
            if lst.len() < 64 {
                lst.push(Bc4Obs { d: acc, tgt, qw: qw.map(|v| v / 4.0), wt });
            }
        }
    }

    fn build(&self, h: &mut History) {
        for (&k, obs) in &self.obs {
            let err = |h0: i32, h1: i32, mid: i32| -> f64 {
                let mut e = 0.0;
                for o in obs {
                    let e0 = ((h0 + mid + o.d[0]) & 255) as f64;
                    let e1 = ((h1 + mid + o.d[1]) & 255) as f64;
                    for q in 0..4 {
                        let d = (e0 + o.qw[q] * (e1 - e0) - o.tgt[q]).abs();
                        e += o.wt * d.min(64.0);
                    }
                }
                e
            };
            let (a0, a1) = coarse_to_fine(|h0, h1, mid| err(h0, h1, mid));
            h.bc4[k] = Some([a0 as u8, a1 as u8]);
        }
    }
}

// Pool scan -------------------------------------------------------------------

fn codec_of(dxgi: u32) -> Option<u8> {
    match dxgi {
        79..=81 => Some(bp::CODEC_BC4),
        94..=96 => Some(bp::CODEC_BC6H),
        97..=99 => Some(bp::CODEC_BC7),
        _ => None,
    }
}

/// Chunks (index) overlapping virtual [a, b).
fn chunks_in(s: &Ttpf, a: u64, b: u64) -> Vec<usize> {
    let mut v = Vec::new();
    for (i, w) in s.chunks.windows(2).enumerate() {
        if w[1].1 > a && w[0].1 < b {
            v.push(i);
        }
    }
    v
}

fn is_bcpack(fl: u8) -> bool {
    fl & 0x1C != 0
}

struct Sibling {
    rgba: Vec<f64>,
    w: i64,
    h: i64,
    child: bool,
}
impl Sibling {
    fn px(&self, x: i64, y: i64) -> [f64; 4] {
        let (x, y) = (x.clamp(0, self.w - 1), y.clamp(0, self.h - 1));
        let o = ((y * self.w + x) * 4) as usize;
        [self.rgba[o], self.rgba[o + 1], self.rgba[o + 2], self.rgba[o + 3]]
    }
    fn quads(&self, bx: i64, by: i64) -> Quads {
        let mut out = [[0.0; 4]; 4];
        for qy in 0..2 {
            for qx in 0..2 {
                let qi = (qy * 2 + qx) as usize;
                if self.child {
                    out[qi] = self.px(2 * bx + qx, 2 * by + qy);
                } else {
                    let (x0, y0) = (8 * bx + 4 * qx, 8 * by + 4 * qy);
                    let (x1, y1) = ((x0 + 4).min(self.w), (y0 + 4).min(self.h));
                    let mut s = [0.0; 4];
                    let mut n = 0.0;
                    for y in y0..y1 {
                        for x in x0..x1 {
                            let p = self.px(x, y);
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
        }
        out
    }
}

/// Decoded mip `mz` of `t` as f64 RGBA (BC6H: unquantized half bits in RGB).
fn sibling(t: &TtpfTex, data: &[u8], mz: usize, child: bool) -> Option<Sibling> {
    let (w, h) = ((t.width >> mz).max(1), (t.height >> mz).max(1));
    let rgba: Vec<f64> = if codec_of(t.dxgi_format) == Some(bp::CODEC_BC6H) {
        let l = t.layout()?;
        let lin = l.untile(data, mz);
        crate::bcpack::bc6h_unq_image(&lin, w, h, t.dxgi_format == 96)
    } else {
        crate::ttpf::decode_rgba(t, data, mz as u32).ok()?.0.into_iter().map(|v| v as f64).collect()
    };
    Some(Sibling { rgba, w: w as i64, h: h as i64, child })
}

/// Fit a preset history from every store in `set` (plain 2D textures).
pub fn calibrate(set: &TtpfSet) -> History {
    let mut c7 = Bc7Cal::new();
    let mut c4 = Bc4Cal::default();
    let mut c6 = crate::bcpack_calib6::Bc6Cal::new();
    let mut recs6: Vec<crate::bcpack_refine6::Rec> = Vec::new();
    for s in &set.stores {
        let mut seen: HashSet<usize> = HashSet::new();
        for t in &s.textures {
            let Some(cid) = codec_of(t.dxgi_format) else { continue };
            if t.slices() != 1 || (9..=12).contains(&t.tile_mode()) || t.split_2b {
                continue;
            }
            let Some(l) = t.layout() else { continue };
            let nm = l.mips.len();
            let spans: Vec<(u64, u64)> = (0..nm).map(|m| l.mip_span(m)).collect();
            let cks: Vec<Vec<usize>> = spans.iter().map(|&(a, b)| chunks_in(s, t.virt + a, t.virt + b)).collect();
            let has = |m: usize| cks[m].iter().any(|&i| s.chunks[i].2 & 0x3F == cid);
            let plain = |m: usize| cks[m].iter().all(|&i| !is_bcpack(s.chunks[i].2));
            let mut pairs = Vec::new();
            for m in 0..nm {
                if !has(m) {
                    continue;
                }
                if m + 1 < nm && plain(m + 1) {
                    pairs.push((m, m + 1, true));
                } else if m > 0 && plain(m - 1) && spans[m - 1].1 - spans[m - 1].0 <= 1 << 20 {
                    pairs.push((m, m - 1, false));
                }
            }
            let eb = if cid == bp::CODEC_BC4 { 8 } else { 16 };
            let mut used = false;
            let mut data: Option<Vec<u8>> = None;
            for (m, mz, child) in pairs {
                let mip_virt = t.virt + spans[m].0;
                let bc: Vec<usize> = cks[m].iter().copied().filter(|&i| s.chunks[i].2 & 0x3F == cid).collect();
                if bc.is_empty() {
                    continue;
                }
                if data.is_none() {
                    data = s.read_plain(t.virt, t.size.min(64 << 20)).ok();
                }
                let Some(d) = data.as_deref() else { continue };
                let Some(sib) = sibling(t, d, mz, child) else { continue };
                // block byte offset inside mip m -> block (x, y)
                let mp = &l.mips[m];
                let mut inv: HashMap<u64, (i64, i64)> = HashMap::with_capacity((mp.w * mp.h) as usize);
                for y in 0..mp.h {
                    for x in 0..mp.w {
                        inv.insert(l.addr(m, 0, x, y) - spans[m].0, (x as i64, y as i64));
                    }
                }
                for ci in bc {
                    let (v0, v1) = (s.chunks[ci].1, s.chunks[ci + 1].1);
                    let vlen = (v1 - v0) as usize;
                    let nb = (vlen / eb).min(MAX_BLOCKS);
                    let Ok(comp) = s.chunk_bytes(ci) else { continue };
                    let Ok(lo) = bp::decode_lanes(cid, &comp, vlen) else { continue };
                    let lanes: Vec<&[u16]> = lo.iter().map(|v| v.as_slice()).collect();
                    let rel = v0 as i64 - mip_virt as i64;
                    let q = |i: usize| -> Option<Quads> {
                        let off = rel + (i * eb) as i64;
                        if off < 0 {
                            return None;
                        }
                        inv.get(&(off as u64)).map(|&(bx, by)| sib.quads(bx, by))
                    };
                    if cid == bp::CODEC_BC6H {
                        let full = vlen / 16;
                        let tgt = (0..full).map(|i| q(i).map(|qq| std::array::from_fn(|c| qq.iter().map(|p| p[c]).sum::<f64>() / 4.0))).collect();
                        recs6.push(crate::bcpack_refine6::Rec { lanes: lo.clone(), nb: full, tgt });
                    }
                    match cid {
                        bp::CODEC_BC7 => c7.add_chunk(&lanes, nb, Some(&q)),
                        bp::CODEC_BC4 => c4.add_chunk(&lanes, nb, &q),
                        _ => c6.add_chunk(&lanes, nb, Some(&q)),
                    }
                    seen.insert(ci);
                    used = true;
                }
            }
            if !used && cid != bp::CODEC_BC4 {
                // pure-BCPack textures still give the exact anchor / p-bit constraints
                if let Some(m) = (0..nm).find(|&m| has(m)) {
                    if let Some(&ci) = cks[m].iter().find(|&&i| s.chunks[i].2 & 0x3F == cid) {
                        if seen.insert(ci) {
                            let vlen = (s.chunks[ci + 1].1 - s.chunks[ci].1) as usize;
                            if let Ok(lo) = s.chunk_bytes(ci).map_err(|e| e.to_string()).and_then(|c| bp::decode_lanes(cid, &c, vlen).map_err(|e| e.to_string())) {
                                let lanes: Vec<&[u16]> = lo.iter().map(|v| v.as_slice()).collect();
                                let nb = (vlen / 16).min(MAX_BLOCKS);
                                if cid == bp::CODEC_BC7 {
                                    c7.add_chunk(&lanes, nb, None);
                                } else {
                                    c6.add_chunk(&lanes, nb, None);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    let mut h = History::empty();
    c7.build(&mut h);
    c4.build(&mut h);
    c6.build(&mut h);
    crate::bcpack_refine::refine_indices(set, &mut h);
    crate::bcpack_refine6::refine_endpoints(&recs6, &mut h);
    h
}
