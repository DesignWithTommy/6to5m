//! Calibrated BCPack preset tables (BC6H, BC7) written by
//! `6ixpack bcpack-calibrate` (`bcpack_fit`). When the file is present the
//! decoders use it; otherwise the shipped v1 tables in `bc6pack_tables.rs` /
//! `bc7pack_tables.rs` (the verified retail fit).
//!
//! File: `BCPPRE2\0`, then 256 BC7 slots (ep flag, 24 bytes, ix flag, 32
//! bytes), then 256 BC6H slots (ep flag, 12 x u16 LE, ix flag, 16 bytes).
use std::path::PathBuf;
use std::sync::OnceLock;

const MAGIC: &[u8; 8] = b"BCPPRE2\0";

#[derive(Clone)]
pub struct Presets {
    pub bc7_ep: [Option<crate::bc7pack::EpState>; 256],
    pub bc7_ix: [Option<crate::bc7pack::IdxState>; 256],
    pub bc6_ep: [Option<crate::bc6pack::EpState>; 256],
    pub bc6_ix: [Option<crate::bc6pack::IdxState>; 256],
}

impl Presets {
    /// The tables the decoders use right now (calibrated file or shipped v1).
    pub fn current() -> Presets {
        let (a, b) = (crate::bc7pack::tables(), crate::bc6pack::tables());
        Presets { bc7_ep: a.hist_ep, bc7_ix: a.hist_idx, bc6_ep: b.hist_ep, bc6_ix: b.hist_idx }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut o = MAGIC.to_vec();
        for k in 0..256 {
            o.push(self.bc7_ep[k].is_some() as u8);
            let e = self.bc7_ep[k].unwrap_or_default();
            for s in e.iter() {
                for p in s.iter() {
                    o.extend_from_slice(p);
                }
            }
            o.push(self.bc7_ix[k].is_some() as u8);
            let x = self.bc7_ix[k].unwrap_or_default();
            o.extend_from_slice(&x[0]);
            o.extend_from_slice(&x[1]);
        }
        for k in 0..256 {
            o.push(self.bc6_ep[k].is_some() as u8);
            for f in self.bc6_ep[k].unwrap_or_default().iter() {
                for c in f.iter() {
                    o.extend_from_slice(&c.to_le_bytes());
                }
            }
            o.push(self.bc6_ix[k].is_some() as u8);
            o.extend_from_slice(&self.bc6_ix[k].unwrap_or_default());
        }
        o
    }

    pub fn from_bytes(b: &[u8]) -> Option<Presets> {
        const R7: usize = 1 + 24 + 1 + 32;
        const R6: usize = 1 + 24 + 1 + 16;
        if b.len() != 8 + 256 * (R7 + R6) || &b[..8] != MAGIC {
            return None;
        }
        let mut p = Presets { bc7_ep: [None; 256], bc7_ix: [None; 256], bc6_ep: [None; 256], bc6_ix: [None; 256] };
        for k in 0..256 {
            let r = &b[8 + k * R7..8 + (k + 1) * R7];
            if r[0] != 0 {
                let mut e: crate::bc7pack::EpState = Default::default();
                for (i, v) in r[1..25].iter().enumerate() {
                    e[i / 8][(i / 4) % 2][i % 4] = *v;
                }
                p.bc7_ep[k] = Some(e);
            }
            if r[25] != 0 {
                let mut x: crate::bc7pack::IdxState = Default::default();
                x[0].copy_from_slice(&r[26..42]);
                x[1].copy_from_slice(&r[42..58]);
                p.bc7_ix[k] = Some(x);
            }
        }
        let b6 = &b[8 + 256 * R7..];
        for k in 0..256 {
            let r = &b6[k * R6..(k + 1) * R6];
            if r[0] != 0 {
                let mut e: crate::bc6pack::EpState = Default::default();
                for i in 0..12 {
                    e[i / 3][i % 3] = u16::from_le_bytes([r[1 + 2 * i], r[2 + 2 * i]]);
                }
                p.bc6_ep[k] = Some(e);
            }
            if r[25] != 0 {
                let mut x: crate::bc6pack::IdxState = Default::default();
                x.copy_from_slice(&r[26..42]);
                p.bc6_ix[k] = Some(x);
            }
        }
        Some(p)
    }
}

/// Where the calibrated tables live: `SIXNG_BCPACK_PRESETS`, else
/// `%LOCALAPPDATA%/sixng/bcpack-presets-v2.bin`.
pub fn path() -> PathBuf {
    if let Some(p) = std::env::var_os("SIXNG_BCPACK_PRESETS") {
        return PathBuf::from(p);
    }
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("sixng").join("bcpack-presets-v2.bin")
}

struct Loaded {
    p: Option<Presets>,
    tag: String,
}

fn loaded() -> &'static Loaded {
    static L: OnceLock<Loaded> = OnceLock::new();
    L.get_or_init(|| {
        let file = path();
        let Ok(b) = std::fs::read(&file) else { return Loaded { p: None, tag: String::new() } };
        match Presets::from_bytes(&b) {
            Some(p) => {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                b.hash(&mut h);
                eprintln!("BCPack: calibrated preset tables from {}", file.display());
                Loaded { p: Some(p), tag: format!("p{:08x}", h.finish() as u32) }
            }
            None => {
                eprintln!("BCPack: {} is not a preset table file; using the shipped v1 tables", file.display());
                Loaded { p: None, tag: String::new() }
            }
        }
    })
}

/// The calibrated tables, if the file is present and valid.
pub fn get() -> Option<&'static Presets> {
    loaded().p.as_ref()
}

/// Cache-key suffix for BC6H/BC7 decodes: empty with the shipped tables,
/// `p<hash>` with a calibrated file (so a new calibration re-decodes them).
pub fn tag() -> &'static str {
    &loaded().tag
}
