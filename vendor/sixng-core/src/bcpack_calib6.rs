//! BC6H part of the preset-history self-calibration (see `bcpack_calib`).
//! Port of Leonida's `Bc6hCalibrator` pool path (GPL-3.0).
use crate::bcpack::{self as bp, History};
use crate::bcpack_tables as t;
use std::collections::HashMap;

#[derive(Clone, Copy)]
struct WOps {
    n: u8,
    op: [(u32, i64); 3],
}

struct WObs {
    ops: WOps,
    mode: usize,
    tgt: f64,
    wt: f64,
}

pub(crate) struct Bc6Cal {
    ix: crate::bcpack_calib::IxAcc,
    w_obs: HashMap<(usize, usize), Vec<WObs>>,
    d_obs: HashMap<(usize, usize, usize), Vec<(u32, i32)>>,
    ep_seen: Vec<bool>,
}

fn sign_extend(v: i64, bits: u32) -> i64 {
    let s = 1i64 << (bits - 1);
    (v ^ s) - s
}

fn unquantize_u(v: u32, bits: u32) -> f64 {
    if bits >= 15 {
        return v as f64;
    }
    if v == 0 {
        return 0.0;
    }
    if v == (1 << bits) - 1 {
        return 65535.0;
    }
    ((((v as u64) << 16) + 0x8000) >> bits) as f64
}

impl Bc6Cal {
    pub(crate) fn new() -> Bc6Cal {
        Bc6Cal { ix: crate::bcpack_calib::IxAcc::new(), w_obs: HashMap::new(), d_obs: HashMap::new(), ep_seen: vec![false; 256] }
    }

    pub(crate) fn add_chunk(&mut self, lanes: &[&[u16]], nb: usize, quads: Option<&dyn Fn(usize) -> Option<[[f64; 4]; 4]>>) {
        let (l0, l1, l15) = (lanes[0], lanes[1], lanes[15]);
        let rows = [lanes[11], lanes[12], lanes[13], lanes[14]];
        let mut eroot = vec![-1i32; nb];
        let mut wchain: Vec<Option<[Option<WOps>; 3]>> = vec![None; nb];
        let mut iroot = vec![-1i32; nb];
        let mut imap: Vec<Option<HashMap<usize, [u8; 16]>>> = vec![None; nb];
        for i in 0..nb {
            let (a, b) = (2 * i, 2 * i + 1);
            let m_sym = l15[a] as usize;
            if m_sym > 13 {
                continue;
            }
            let mode = m_sym + 1;
            let (trans, base, deltas, reg, _) = bp::b6_info(bp::B6_CODE[mode]);
            let part = (l15[b] as usize).min(31);
            let di = bp::inter8(l1[a], l1[b]) as usize;
            let de = ((di as i32 + bp::unzig(bp::inter8(l0[a], l0[b]))) & 255) as usize;
            let ibits = if reg == 2 { 3 } else { 4 };

            if di > 0 {
                let (ki, pmap) = if di > i { ((256 + i - di) as i32, None) } else { let k = iroot[i - di]; (k, if k >= 0 { imap[i - di].clone() } else { None }) };
                if ki >= 0 {
                    iroot[i] = ki;
                    let anchors: Vec<usize> = if reg == 2 { vec![0, t::BC7_ANCHOR2[part] as usize] } else { vec![0] };
                    let sub = |p: usize| if reg == 2 { (t::BC7_PART2[part] >> p) & 1 } else { 0 };
                    let same = |p: usize| sub(p) == sub(0);
                    let mut cur = HashMap::new();
                    crate::bcpack_calib::index_constraints(&mut self.ix, ki as usize, pmap.as_ref(), &rows, a, b, &anchors, ibits, 0, &same, &mut cur);
                    imap[i] = Some(cur);
                }
            }

            if de == 0 {
                continue;
            }
            let (ke, pchain) = if de > i { ((256 + i - de) as i32, None) } else { let k = eroot[i - de]; (k, if k >= 0 { wchain[i - de] } else { None }) };
            if ke < 0 {
                continue;
            }
            let ke_u = ke as usize;
            eroot[i] = ke;
            self.ep_seen[ke_u] = true;
            let mw = (1i64 << base) - 1;
            let mut cur_w: [Option<WOps>; 3] = [None; 3];
            for c in 0..3 {
                let dc = deltas[c];
                let dz0 = bp::unzig(bp::inter(lanes[2 + c][a], lanes[2 + c][b], 8)) as i64 & mw;
                match &pchain {
                    None => {
                        cur_w[c] = Some(WOps { n: 1, op: [(base, dz0), (0, 0), (0, 0)] });
                        if trans {
                            let dz1 = bp::unzig(bp::inter(lanes[5 + c][a], lanes[5 + c][b], 5));
                            let l = self.d_obs.entry((ke_u, c, 1)).or_default();
                            if l.len() < 96 {
                                l.push((dc, dz1));
                            }
                            if reg == 2 {
                                for (q, s) in [(2, a), (3, b)] {
                                    let dz = bp::unzig(lanes[8 + c][s] as u32);
                                    let l = self.d_obs.entry((ke_u, c, q)).or_default();
                                    if l.len() < 96 {
                                        l.push((dc, dz));
                                    }
                                }
                            }
                        }
                    }
                    Some(pc) => {
                        if let Some(p) = pc[c] {
                            let (lb, lz) = p.op[p.n as usize - 1];
                            let mut o = p;
                            if lb == base {
                                o.op[o.n as usize - 1] = (base, (lz + dz0) & mw);
                                cur_w[c] = Some(o);
                            } else if p.n < 3 {
                                o.op[o.n as usize] = (base, dz0);
                                o.n += 1;
                                cur_w[c] = Some(o);
                            }
                        }
                    }
                }
            }
            wchain[i] = Some(cur_w);

            let Some(q) = quads.and_then(|f| f(i)) else { continue };
            let spread = (0..3).map(|c| q.iter().map(|p| p[c]).fold(f64::MIN, f64::max) - q.iter().map(|p| p[c]).fold(f64::MAX, f64::min)).fold(f64::MIN, f64::max);
            let wt = if spread <= 512.0 { 4.0 } else if spread <= 2048.0 { 2.0 } else { 0.75 };
            for c in 0..3 {
                if let Some(ops) = cur_w[c] {
                    let mean = q.iter().map(|p| p[c]).sum::<f64>() / 4.0;
                    let l = self.w_obs.entry((ke_u, c)).or_default();
                    if l.len() < 64 {
                        l.push(WObs { ops, mode, tgt: mean.clamp(0.0, 65535.0), wt });
                    }
                }
            }
        }
    }

    fn eval_w(ops: &WOps, mode: usize, r0: i64) -> f64 {
        let mut v = r0;
        let mut last = 16;
        for &(base, dz) in &ops.op[..ops.n as usize] {
            let sh = 16 - base;
            let m = (1i64 << base) - 1;
            v = (((v >> sh) + dz) & m) << sh;
            last = base;
        }
        unquantize_u(bp::b6_stream_w(mode, (v >> (16 - last)) as u32), last)
    }

    fn fit_w(obs: &[WObs]) -> u32 {
        let err = |h: i64| -> f64 { obs.iter().map(|o| o.wt * (Self::eval_w(&o.ops, o.mode, h) - o.tgt).abs().min(8192.0)).sum() };
        let mut c: Vec<(f64, i64)> = (0..65536).step_by(1024).map(|h| (err(h | 512), h)).collect();
        c.sort_by(|x, y| x.partial_cmp(y).unwrap());
        c.truncate(4);
        for step in [512, 256, 128, 64, 32, 16, 8, 4, 2, 1] {
            let mid = step / 2;
            let mut nxt: Vec<(f64, i64)> = Vec::new();
            for &(_, b) in &c {
                for off in [0, step] {
                    let h = (b + off) & 65535;
                    if !nxt.iter().any(|x| x.1 == h) {
                        nxt.push((err(h | mid), h));
                    }
                }
            }
            nxt.sort_by(|x, y| x.partial_cmp(y).unwrap());
            nxt.truncate(4);
            c = nxt;
        }
        c[0].1 as u32
    }

    fn fit_signed_delta(obs: Option<&Vec<(u32, i32)>>, total: u32) -> u32 {
        let Some(obs) = obs.filter(|o| !o.is_empty()) else { return 1 << (total - 1) };
        let mut best = (f64::MAX, 0);
        for r in 0..1u32 << total {
            let mut e = 0.0;
            for &(dc, dz) in obs {
                let sh = if dc < total { total - dc } else { 0 };
                let m = (1i64 << dc) - 1;
                let v = (((r >> sh) as i64) + dz as i64) & m;
                e += sign_extend(v, dc).abs() as f64 / (1u64 << (dc - 1)) as f64;
            }
            if e < best.0 {
                best = (e, r);
            }
        }
        best.1
    }

    pub(crate) fn build(&self, h: &mut History) {
        for k in 0..256 {
            if self.ep_seen[k] {
                let (mut has_w, mut has_d) = (0, 0);
                for c in 0..3 {
                    let w = match self.w_obs.get(&(k, c)).filter(|l| !l.is_empty()) {
                        Some(l) => {
                            has_w += 1;
                            Self::fit_w(l)
                        }
                        None => 0x8000,
                    };
                    let (x, y, z) = (self.d_obs.get(&(k, c, 1)), self.d_obs.get(&(k, c, 2)), self.d_obs.get(&(k, c, 3)));
                    if x.is_some() || y.is_some() || z.is_some() {
                        has_d += 1;
                    }
                    h.bc6h_ep[k][c] = [w, Self::fit_signed_delta(x, 10), Self::fit_signed_delta(y, 6), Self::fit_signed_delta(z, 6)];
                }
                h.bc6h_ecls[k] = if has_w == 3 { 1 } else if has_w > 0 || has_d > 0 { 2 } else { 0 };
            }
            if self.ix.obs[k][..16].iter().any(|&x| x > 0) {
                let mut ix = [0u8; 32];
                let mut ixb = [0u8; 32];
                self.ix.resolve(k, 0, 4, &mut ix, &mut ixb);
                for p in 0..16 {
                    h.bc6h_ix[k][p] = ix[p] as u32;
                }
                h.bc6h_icls[k] = 1;
            }
        }
    }
}
