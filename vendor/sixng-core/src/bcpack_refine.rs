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
