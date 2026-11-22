//! BC6H endpoint-history refinement against plain sibling mips.
//!
//! The pool calibrator fits each preset entry's base value from the first
//! 256 blocks of a chunk and centres the delta fields, which leaves chains
//! rooted in the preset visibly wrong (mode 13/14 chains: ~10-75% of blocks
//! off against the plain sibling mip, vs ~2% for chains rooted in-chunk).
//! This re-fits all four endpoint fields of every entry and channel by
//! coordinate descent, simulating every chain rooted at the entry through
//! whole chunks and scoring the block mean (in unquantized space, against
//! the sibling 2x2 mean, log error) wherever a sibling exists. Channels are
//! independent: a channel's endpoints depend only on its own chain, and the
//! index weights do not depend on endpoints at all.
use crate::bcpack::{self as bp, History};
use crate::bcpack_tables as t;
use rayon::prelude::*;

const BUDGET: usize = 40000;

/// One BC6H chunk with sibling targets for some of its blocks.
pub(crate) struct Rec {
    pub lanes: Vec<Vec<u16>>,
    pub nb: usize,
    /// per 2x2 quadrant (qy * 2 + qx), the sibling pixel mean
    pub tgt: Vec<Option<[[f64; 3]; 4]>>,
}

const W3: [u32; 8] = [0, 9, 18, 27, 37, 46, 55, 64];
const W4: [u32; 16] = [0, 4, 9, 13, 17, 21, 26, 30, 34, 38, 43, 47, 51, 55, 60, 64];

/// Per block: everything a channel simulation needs.
#[derive(Clone, Copy)]
struct Blk {
    mode: u8,
    /// endpoint parent block (usize::MAX = chain root in the preset, root entry in `k`)
    parent: usize,
    k: i16,
    dz: [[i64; 4]; 3],
    /// per quadrant region weights: (A0, B0, A1, B1), quadrant mean = A0 e0 + B0 e1 + A1 e2 + B1 e3
    wt: [[f64; 4]; 4],
    tgt: Option<[[f64; 3]; 4]>,
}

fn unq(v: i64, bits: u32) -> f64 {
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
}

fn sx(v: u32, bits: u32) -> i64 {
    let s = 1i64 << (bits - 1);
    (v as i64 ^ s) - s
}

/// Channel step: state in, (state out, quadrant means).
fn step(b: &Blk, c: usize, r: [u32; 4]) -> ([u32; 4], [f64; 4]) {
    let mode = b.mode as usize;
    let (trans, base, deltas, reg, _) = bp::b6_info(bp::B6_CODE[mode]);
    let dc = deltas[c];
    let md = (1i64 << dc) - 1;
    let mw = (1i64 << base) - 1;
    let ws = 16 - base;
    let sh = if dc < 6 { 6 - dc } else { 0 };
    let [d0, d1, d2, d3] = b.dz[c];
    let w = (((r[0] >> ws) as i64 + d0) & mw) as u32;
    let x = (((r[1] >> (10 - dc)) as i64 + d1) & md) as u32;
    let y = (((r[2] >> sh) as i64 + d2) & md) as u32;
    let z = (((r[3] >> sh) as i64 + d3) & md) as u32;
    let x10 = x << (10 - dc);
    let st = if reg == 2 {
        [w << ws, x10, y << sh, z << sh]
    } else {
        let low_y = if trans {
            let v = (w & md as u32) ^ (1 << (dc - 1));
            if dc >= 6 { v >> (dc - 6) } else { v << sh }
        } else {
            (w << ws) >> 10
        };
        [w << ws, x10, low_y, x10 >> 4]
    };
    let wv = bp::b6_stream_w(mode, w) as i64;
    let e = if trans {
        [unq(wv, base), unq((wv + sx(x, dc)) & mw, base), unq((wv + sx(y, dc)) & mw, base), unq((wv + sx(z, dc)) & mw, base)]
    } else {
        [unq(wv, base), unq(x as i64, dc), unq(y as i64, dc), unq(z as i64, dc)]
    };
    let mean = std::array::from_fn(|k| {
        let w = &b.wt[k];
        if reg == 2 { w[0] * e[0] + w[1] * e[1] + w[2] * e[2] + w[3] * e[3] } else { w[0] * e[0] + w[1] * e[1] }
    });
    (st, mean)
}

fn blocks(rec: &Rec, h: &History) -> Vec<Blk> {
    let l = &rec.lanes;
    let (l15,) = (&l[15],);
    let mut out: Vec<Blk> = Vec::with_capacity(rec.nb);
    let mut iroot_ok = vec![false; rec.nb];
    let mut st_i: Vec<[u32; 16]> = Vec::with_capacity(rec.nb);
    for i in 0..rec.nb {
        let (a, b) = (2 * i, 2 * i + 1);
        let ms = l15[a] as usize;
        let di = bp::inter8(l[1][a], l[1][b]) as usize;
        let de = ((di as i32 + bp::unzig(bp::inter8(l[0][a], l[0][b]))) & 255) as usize;
        let mut blk = Blk { mode: 0, parent: usize::MAX, k: -1, dz: [[0; 4]; 3], wt: [[0.0; 4]; 4], tgt: rec.tgt.get(i).copied().flatten() };
        if ms > 13 {
            out.push(blk);
            st_i.push([8; 16]);
            continue;
        }
        let mode = ms + 1;
        let (_, _, _, reg, _) = bp::b6_info(bp::B6_CODE[mode]);
        let part = (l15[b] as usize).min(31);
        // indices (same chain logic as the decoder)
        let (iref, iok) = if di == 0 {
            ([8u32; 16], true)
        } else if di <= i {
            (st_i[i - di], iroot_ok[i - di])
        } else {
            let k = 256 + i - di;
            (h.bc6h_ix[k], h.bc6h_icls[k] != 0)
        };
        iroot_ok[i] = iok;
        let ibits = if reg == 2 { 3 } else { 4 };
        let imask = (1i32 << ibits) - 1;
        let ish = 4 - ibits;
        let mut si = [0u32; 16];
        let mut wt = [[0.0f64; 4]; 4];
        for y in 0..4 {
            let d = bp::row_digits(l[11 + y][a], l[11 + y][b], y);
            for off in 0..4 {
                let p = y * 4 + off;
                let v = (((iref[p] >> ish) as i32 + bp::unzig(d[off])) & imask) as u32;
                si[p] = v << ish;
                let w = if reg == 2 { W3[v as usize] } else { W4[v as usize] } as f64;
                let s2 = if reg == 2 { (((t::BC7_PART2[part] >> p) & 1) * 2) as usize } else { 0 };
                let qd = (y / 2) * 2 + off / 2;
                wt[qd][s2] += (64.0 - w) / 64.0 / 4.0;
                wt[qd][s2 + 1] += w / 64.0 / 4.0;
            }
        }
        st_i.push(si);
        blk.mode = mode as u8;
        blk.wt = wt;
        if !iok {
            blk.tgt = None;
        }
        for c in 0..3 {
            blk.dz[c] = [
                bp::unzig(bp::inter(l[2 + c][a], l[2 + c][b], 8)) as i64,
                bp::unzig(bp::inter(l[5 + c][a], l[5 + c][b], 5)) as i64,
                bp::unzig(l[8 + c][a] as u32) as i64,
                bp::unzig(l[8 + c][b] as u32) as i64,
            ];
        }
        if de == 0 {
            // own chain, not rooted in the preset
        } else if de <= i {
            blk.parent = i - de;
            blk.k = out[i - de].k;
        } else {
            blk.k = (256 + i - de) as i16;
        }
        out.push(blk);
    }
    out
}

/// Chains of one entry in one chunk: block list in order (parents precede).
struct Chain<'a> {
    blks: &'a [Blk],
    ids: Vec<usize>,
    /// parent position inside `ids` (usize::MAX = preset root)
    par: Vec<usize>,
}

fn chain_err(ch: &Chain, c: usize, r: [u32; 4], st: &mut Vec<[u32; 4]>) -> f64 {
    st.clear();
    let mut e = 0.0;
    for (j, &i) in ch.ids.iter().enumerate() {
        let b = &ch.blks[i];
        let src = if ch.par[j] == usize::MAX { r } else { st[ch.par[j]] };
        let (s, mean) = step(b, c, src);
        st.push(s);
        if let Some(tg) = b.tgt {
            // per-quadrant error with the decoded indices: a block-mean score
            // lets the second region's endpoints drift (GTA6-Asset-Info d62cc61)
            for k in 0..4 {
                let d = ((mean[k].max(0.0) + 1.0).log2() - (tg[k][c].max(0.0) + 1.0).log2()).abs();
                e += d.min(4.0) / 4.0;
            }
        }
    }
    e
}

/// Refine `h.bc6h_ep` from chunks with sibling targets.
pub(crate) fn refine_endpoints(recs: &[Rec], h: &mut History) {
    let all: Vec<Vec<Blk>> = recs.par_iter().map(|r| blocks(r, h)).collect();
    let fitted: Vec<(usize, [[u32; 4]; 3], f64, f64)> = (0..256usize)
        .into_par_iter()
        .filter_map(|k| {
            let chains: Vec<Chain> = all
                .iter()
                .map(|bl| {
                    let mut ids: Vec<usize> = (0..bl.len()).filter(|&i| bl[i].k == k as i16 && bl[i].mode != 0).collect();
                    // drop the tail after the last target
                    let last = ids.iter().rposition(|&i| bl[i].tgt.is_some()).map_or(0, |p| p + 1);
                    ids.truncate(last);
                    let pos: std::collections::HashMap<usize, usize> = ids.iter().enumerate().map(|(j, &i)| (i, j)).collect();
                    let par = ids.iter().map(|&i| if bl[i].parent == usize::MAX { usize::MAX } else { pos.get(&bl[i].parent).copied().unwrap_or(usize::MAX) }).collect();
                    Chain { blks: bl, ids, par }
                })
                .filter(|c| !c.ids.is_empty())
                .collect();
            // budget: at most BUDGET simulated blocks per entry
            let mut chains = chains;
            let mut used = 0;
            chains.retain(|c| {
                used += c.ids.len();
                used <= BUDGET || used == c.ids.len()
            });
            if chains.iter().map(|c| c.ids.iter().filter(|&&i| c.blks[i].tgt.is_some()).count()).sum::<usize>() < 8 {
                return None;
            }
            let mut st = Vec::new();
            let mut best = h.bc6h_ep[k];
            let (mut before, mut after) = (0.0, 0.0);
            for c in 0..3 {
                let mut err = |r: [u32; 4]| chains.iter().map(|ch| chain_err(ch, c, r, &mut st)).sum::<f64>();
                let mut r = if h.bc6h_ecls[k] != 0 { h.bc6h_ep[k][c] } else { [0x8000, 0x200, 0x20, 0x20] };
                let mut e = err(r);
                before += e;
                for _round in 0..6 {
                    let e0 = e;
                    for (f, bits) in [(0usize, 16u32), (1, 10), (2, 6), (3, 6)] {
                        // coarse scan then local refinement
                        let n = 1u32 << bits;
                        let coarse = if bits > 6 { 1u32 << (bits - 5) } else { 1 };
                        let mut cand = r;
                        let mut ce = e;
                        let mut v = 0;
                        while v < n {
                            let mut t = r;
                            t[f] = v | if coarse > 1 { coarse / 2 } else { 0 };
                            let te = err(t);
                            if te < ce {
                                ce = te;
                                cand = t;
                            }
                            v += coarse;
                        }
                        if coarse > 1 {
                            let mut stepv = coarse / 2;
                            while stepv >= 1 {
                                for s in [-(stepv as i64), stepv as i64] {
                                    let mut t = cand;
                                    t[f] = (cand[f] as i64 + s).clamp(0, n as i64 - 1) as u32;
                                    let te = err(t);
                                    if te < ce {
                                        ce = te;
                                        cand = t;
                                    }
                                }
                                stepv /= 2;
                            }
                        }
                        // single-bit flips: the 12/16-bit modes keep w's high bits
                        // reversed, so neighbouring values are not neighbours in error
                        for bit in 0..bits {
                            let mut t = cand;
                            t[f] ^= 1 << bit;
                            let te = err(t);
                            if te < ce {
                                ce = te;
                                cand = t;
                            }
                        }
                        if ce < e {
                            e = ce;
                            r = cand;
                        }
                    }
                    if e >= e0 - 1e-9 {
                        break;
                    }
                }
                after += e;
                best[c] = r;
            }
            Some((k, best, before, after))
        })
        .collect();
    for (k, ep, before, after) in fitted {
        if after < before || h.bc6h_ecls[k] == 0 {
            h.bc6h_ep[k] = ep;
            h.bc6h_ecls[k] = 1;
        }
    }
}
