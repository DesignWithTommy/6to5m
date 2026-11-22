//! Index-history refinement (BC6H, BC7): for every preset entry and pixel,
//! pick the 4-bit start value that minimises anchor-index violations over
//! every block whose index chain starts there, in every chunk.
//!
//! A pixel's index only depends on its own chain (`v = ((prev >> s) + dz) &
//! m`, left-aligned to 4 bits), so each (entry, pixel) is an independent
//! 16-way choice and the count is exact. An anchor index must have its top
//! bit clear in BC6H/BC7, so a set top bit marks a wrong start value.
//! Leonida's greedy constraint intersection (first 256 blocks of a sample of
//! chunks) leaves BC6H at ~3% violating blocks; this counts all of them.
use crate::bcpack::{self as bp, History};
use crate::bcpack_tables as t;
use crate::ttpf::TtpfSet;
use rayon::prelude::*;

/// Blocks per BC7 chunk scanned (chains root early; BC7 chunks are many).
const BC7_BLOCKS: usize = 4096;

/// viol[k][slot][h], obs[k][slot]
struct Counts {
    viol: Vec<[[u32; 16]; 32]>,
    obs: Vec<[u32; 32]>,
}
impl Counts {
    fn new() -> Counts {
        Counts { viol: vec![[[0; 16]; 32]; 256], obs: vec![[0; 32]; 256] }
    }
    fn add(mut self, o: Counts) -> Counts {
        for k in 0..256 {
            for s in 0..32 {
                self.obs[k][s] += o.obs[k][s];
                for h in 0..16 {
                    self.viol[k][s][h] += o.viol[k][s][h];
                }
            }
        }
        self
    }
}

/// Per-slot chain state: root entry and the map start value -> current value.
type Slot = Option<(u8, [u8; 16])>;

fn step(prev: Slot, root_if_new: Option<u8>, dz: i32, bits: u32) -> Slot {
    let sh = 4 - bits;
    let m = (1i32 << bits) - 1;
    let (k, base) = match (prev, root_if_new) {
        (_, Some(k)) => (k, std::array::from_fn(|h| h as u8)),
        (Some(p), None) => p,
        (None, None) => return None,
    };
    let mut out = [0u8; 16];
    for h in 0..16 {
        out[h] = ((((base[h] as i32 >> sh) + dz) & m) << sh) as u8;
    }
    Some((k, out))
}

fn count_chunk(codec: u8, lanes: &[&[u16]], nb: usize, c: &mut Counts) {
    let (l1, l15) = (lanes[1], lanes[15]);
    let rows1 = [lanes[11], lanes[12], lanes[13], lanes[14]];
    let rows2 = [lanes[6], lanes[7], lanes[8], lanes[10]];
    let mut st: Vec<[Slot; 32]> = Vec::with_capacity(nb);
    for i in 0..nb {
        let (a, b) = (2 * i, 2 * i + 1);
        let di = bp::inter8(l1[a], l1[b]) as usize;
        let mut cur: [Slot; 32] = [None; 32];
        // (index bits primary, secondary set?, anchors of the primary set)
        let spec: Option<(u32, bool, [usize; 3], usize)> = if codec == bp::CODEC_BC6H {
            let ms = l15[a] as usize;
            (ms <= 13).then(|| {
                let reg = bp::b6_info(bp::B6_CODE[ms + 1]).3;
                let part = (l15[b] as usize).min(31);
                if reg == 2 { (3, false, [0, t::BC7_ANCHOR2[part] as usize, 0], 2) } else { (4, false, [0, 0, 0], 1) }
            })
        } else {
            let (sa, sb) = (l15[a] as usize, l15[b] as usize);
            let rank = if sa < 32 && sb < 16 { t::BC7_RANK_OF[sa * 16 + sb] } else { -1 };
            if rank < 0 || t::BC7_CONFIG[rank as usize][0] > 7 {
                None
            } else {
                let [mode, part, _, _] = t::BC7_CONFIG[rank as usize].map(|x| x as usize);
                let two = mode == 4 || mode == 5;
                let ns = bp::B7_NS[mode];
                let anc = if two || ns == 1 {
                    ([0, 0, 0], 1)
                } else if ns == 2 {
                    ([0, t::BC7_ANCHOR2[part] as usize, 0], 2)
                } else {
                    ([0, t::BC7_ANCHOR3A[part] as usize, t::BC7_ANCHOR3B[part] as usize], 3)
                };
                Some((bp::B7_IB[mode], two, anc.0, anc.1))
            }
        };
        let Some((ib, two, anchors, na)) = spec else {
            st.push(cur);
            continue;
        };
        let root = (di > i).then(|| (256 + i - di) as u8);
        let prev: Option<&[Slot; 32]> = (di != 0 && di <= i).then(|| &st[i - di]);
        let nsets = if two { 2 } else { 1 };
        for set in 0..nsets {
            let (rows, bits) = if set == 0 { (&rows1, ib) } else { (&rows2, 2) };
            for y in 0..4 {
                let d = bp::row_digits(rows[y][a], rows[y][b], y);
                for off in 0..4 {
                    let p = y * 4 + off;
                    let s = set * 16 + p;
                    let pv = prev.and_then(|ps| ps[s]);
                    cur[s] = if di == 0 { None } else { step(pv, root, bp::unzig(d[off]), bits) };
                }
            }
            let anc: &[usize] = if set == 0 { &anchors[..na] } else { &[0] };
            for &p in anc {
                let s = set * 16 + p;
                if let Some((k, map)) = cur[s] {
                    let k = k as usize;
                    c.obs[k][s] += 1;
                    for h in 0..16 {
                        if map[h] & 8 != 0 {
                            c.viol[k][s][h] += 1;
                        }
                    }
                }
            }
        }
        st.push(cur);
    }
}

/// Refine the BC6H and BC7 index presets of `h` over every chunk of `set`.
pub fn refine_indices(set: &TtpfSet, h: &mut History) {
    for codec in [bp::CODEC_BC6H, bp::CODEC_BC7] {
        let jobs: Vec<(usize, usize)> = set
            .stores
            .iter()
            .enumerate()
            .flat_map(|(si, s)| s.chunks.windows(2).enumerate().filter(|(_, w)| w[0].2 & 0x3F == codec && w[0].2 & 0x1C != 0).map(move |(ci, _)| (si, ci)))
            .collect();
        let counts = jobs
            .par_iter()
            .fold(Counts::new, |mut c, &(si, ci)| {
                let s = &set.stores[si];
                let vlen = (s.chunks[ci + 1].1 - s.chunks[ci].1) as usize;
                if let Ok(lo) = s.chunk_bytes(ci).map_err(|e| e.to_string()).and_then(|b| bp::decode_lanes(codec, &b, vlen).map_err(|e| e.to_string())) {
                    let lanes: Vec<&[u16]> = lo.iter().map(|v| v.as_slice()).collect();
                    let mut nb = vlen / 16;
                    if codec == bp::CODEC_BC7 {
                        nb = nb.min(BC7_BLOCKS);
                    }
                    count_chunk(codec, &lanes, nb, &mut c);
                }
                c
            })
            .reduce(Counts::new, Counts::add);
        for k in 0..256 {
            let nslots = if codec == bp::CODEC_BC6H { 16 } else { 32 };
            for s in 0..nslots {
                if counts.obs[k][s] == 0 {
                    continue;
                }
                let v = &counts.viol[k][s];
                let cur = if codec == bp::CODEC_BC6H { h.bc6h_ix[k][s] as usize } else { h.bc7_ix[k][s] as usize } & 15;
                let mut best = cur;
                for c in 0..16 {
                    if v[c] < v[best] {
                        best = c;
                    }
                }
                if codec == bp::CODEC_BC6H {
                    h.bc6h_ix[k][s] = best as u32;
                    h.bc6h_icls[k] = h.bc6h_icls[k].max(1);
                } else {
                    h.bc7_ix[k][s] = best as u8;
                    h.bc7_ix_bits[k][s] = h.bc7_ix_bits[k][s].max(if s < 16 { 4 } else { 2 });
                }
            }
        }
    }
}

/// R-channel slot state: (root entry, root slot, start value -> current value).
type RSlot = Option<(u8, u8, [u8; 256])>;

/// Mode-1 BC7 blocks share one p-bit per subset, stored as the low bit of
/// both endpoints' R value; chains rooted in the preset whose two R start
/// values have the wrong relative parity flag every such block. For every
/// (entry, R slot) count, per candidate start value with the partner slot
/// at its current value, how many shared-p-bit mismatches it gives.
fn count_pbits(lanes: &[&[u16]], nb: usize, cur: &History, viol: &mut [[[u32; 256]; 24]], obs: &mut [[u32; 24]]) {
    let l15 = lanes[15];
    let mut st: Vec<[RSlot; 24]> = vec![[None; 24]; 256];
    for i in 0..nb {
        let (a, b) = (2 * i, 2 * i + 1);
        let (sa, sb) = (l15[a] as usize, l15[b] as usize);
        let rank = if sa < 32 && sb < 16 { t::BC7_RANK_OF[sa * 16 + sb] } else { -1 };
        let mut out: [RSlot; 24] = [None; 24];
        if rank < 0 || t::BC7_CONFIG[rank as usize][0] > 7 {
            st[i & 255] = out;
            continue;
        }
        let [mode, _, rot, _] = t::BC7_CONFIG[rank as usize].map(|x| x as usize);
        let l1 = lanes[1];
        let di = bp::inter8(l1[a], l1[b]) as i32;
        let de = ((di + bp::unzig(bp::inter8(lanes[0][a], lanes[0][b]))) & 255) as usize;
        let src = |s: usize| -> RSlot {
            if de == 0 {
                None
            } else if de <= i {
                st[(i - de) & 255][s]
            } else {
                Some(((256 + i - de) as u8, s as u8, std::array::from_fn(|h| h as u8)))
            }
        };
        let ns = bp::B7_NS[mode];
        let (cb, abits, pk) = (bp::B7_CB[mode], bp::B7_AB[mode], bp::B7_PK[mode]);
        let wc = if rot == 1 { abits } else { cb };
        for sub in 0..ns {
            let li = bp::B7_LANE[sub][0];
            if wc == 0 || li < 0 {
                continue;
            }
            let bits = wc + (pk != 0) as u32;
            let shift = 8 - bits;
            let mask = (1i32 << bits) - 1;
            for e in 0..2 {
                let s = sub * 8 + e * 4;
                let dz = bp::unzig(lanes[li as usize][if e == 0 { a } else { b }] as u32);
                out[s] = src(s).map(|(k, rs, m)| (k, rs, m.map(|v| (((((v >> shift) as i32) + dz) & mask) << shift) as u8)));
            }
            if pk == 2 {
                let (s0, s1) = (sub * 8, sub * 8 + 4);
                let bitv = |v: u8| (v >> shift) & 1;
                if let (Some((k0, r0, m0)), Some((k1, r1, m1))) = (out[s0], out[s1]) {
                    let c0 = bitv(m0[cur.bc7_ep[k0 as usize][r0 as usize] as usize]);
                    let c1 = bitv(m1[cur.bc7_ep[k1 as usize][r1 as usize] as usize]);
                    for h in 0..256 {
                        if bitv(m1[h]) != c0 {
                            viol[k1 as usize][r1 as usize][h] += 1;
                        }
                        if bitv(m0[h]) != c1 {
                            viol[k0 as usize][r0 as usize][h] += 1;
                        }
                    }
                    obs[k1 as usize][r1 as usize] += 1;
                    obs[k0 as usize][r0 as usize] += 1;
                }
            }
        }
        for sub in ns..3 {
            for e in 0..2 {
                out[sub * 8 + e * 4] = out[e * 4];
            }
        }
        st[i & 255] = out;
    }
}

/// Refit BC7 preset R values so mode-1 shared p-bits agree.
pub fn refine_bc7_pbits(set: &TtpfSet, h: &mut History) {
    let jobs: Vec<(usize, usize)> = set
        .stores
        .iter()
        .enumerate()
        .flat_map(|(si, s)| s.chunks.windows(2).enumerate().filter(|(_, w)| w[0].2 & 0x3F == bp::CODEC_BC7 && w[0].2 & 0x1C != 0).map(move |(ci, _)| (si, ci)))
        .collect();
    for _pass in 0..2 {
        let cur: &History = h;
        let (viol, obs) = jobs
            .par_iter()
            .fold(
                || (vec![[[0u32; 256]; 24]; 256], vec![[0u32; 24]; 256]),
                |(mut v, mut o), &(si, ci)| {
                    let s = &set.stores[si];
                    let vlen = (s.chunks[ci + 1].1 - s.chunks[ci].1) as usize;
                    if let Ok(lo) = s.chunk_bytes(ci).map_err(|e| e.to_string()).and_then(|b| bp::decode_lanes(bp::CODEC_BC7, &b, vlen).map_err(|e| e.to_string())) {
                        let lanes: Vec<&[u16]> = lo.iter().map(|x| x.as_slice()).collect();
                        count_pbits(&lanes, vlen / 16, cur, &mut v, &mut o);
                    }
                    (v, o)
                },
            )
            .reduce(
                || (vec![[[0u32; 256]; 24]; 256], vec![[0u32; 24]; 256]),
                |(mut v, mut o), (v2, o2)| {
                    for k in 0..256 {
                        for s in 0..24 {
                            o[k][s] += o2[k][s];
                            for x in 0..256 {
                                v[k][s][x] += v2[k][s][x];
                            }
                        }
                    }
                    (v, o)
                },
            );
        let mut changed = 0;
        for k in 0..256 {
            for s in 0..24 {
                if obs[k][s] == 0 {
                    continue;
                }
                let v = &viol[k][s];
                let c = h.bc7_ep[k][s] as usize;
                // nearest value with the fewest mismatches (keeps the colour close)
                let best = (0..256).min_by_key(|&x| (v[x], (x as i32 - c as i32).abs())).unwrap();
                if v[best] < v[c] {
                    h.bc7_ep[k][s] = best as u8;
                    changed += 1;
                }
            }
        }
        if changed == 0 {
            break;
        }
    }
}
