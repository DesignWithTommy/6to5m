//! Strand-hair grooms (unnamed `packs.rpf` resources, type 3) as GTA V
//! drawable hair cards.
//!
//! The decoder is ported from SixNG `groom.rs` (GPL-3.0, upstream de366fb),
//! after GTA6-Asset-Info formats/19 §19.2. The system page has the type byte
//! 0x1A = 3, the tag u32 0x1C = 0x7d8f3db3, the root box at 0x30/0x40, the
//! buffer descriptor ptr at 0x70 and the bone hashes at 0x98/0xA0. The
//! graphics buffer has section offsets B0..B7 at 0x30, S at 0x58, K at 0x5C,
//! B5 = S x K x 4 f16 points (model space, Z-up metres).
//!
//! No resource links a ped to its grooms, so they are chosen by id
//! (`--hair` / `--beard`); `--grooms` lists the candidates.

use sixng_core::resource::Resource;
use sixng_core::scan::{ArchiveIndex, Entry};
use std::collections::HashMap;
use std::path::Path;

const TAG: u32 = 0x7d8f_3db3;

pub struct Groom {
    pub strands: usize,
    pub k: usize,
    pub points: Vec<[f32; 3]>,
}

fn is_groom(sys: &[u8]) -> bool {
    sys.len() >= 0xE0 && sys[0x1A] == 3 && sys.get(0x1C..0x20) == Some(&TAG.to_le_bytes()[..])
}

fn f16(h: u16) -> f32 {
    let s = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let e = ((h >> 10) & 0x1F) as i32;
    let m = (h & 0x3FF) as f32;
    s * match e {
        0 => m / 1024.0 * 2f32.powi(-14),
        31 => f32::INFINITY,
        _ => (1.0 + m / 1024.0) * 2f32.powi(e - 15),
    }
}

fn bones_of(r: &Resource) -> Vec<u32> {
    let nb = r.u32(0x98) as usize;
    match r.sys_off(r.u64(0xA0)) {
        Some(o) if nb <= 4096 => (0..nb).map(|i| r.u32(o + 4 * i)).collect(),
        _ => Vec::new(),
    }
}

pub fn parse(r: &Resource) -> Result<Groom, String> {
    if !is_groom(&r.sys) {
        return Err("not a groom resource".into());
    }
    let desc = r.sys_off(r.u64(0x70)).ok_or("no buffer descriptor")?;
    let base = r.gfx_off(r.u64(desc + 0x18)).ok_or("no graphics buffer")?;
    let g = r.gfx.get(base..).ok_or("graphics buffer outside the pages")?;
    let gu16 = |o: usize| g.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]])).unwrap_or(0);
    let gu32 = |o: usize| g.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).unwrap_or(0);
    let (s, k) = (gu16(0x58) as usize, gu32(0x5C) as usize);
    if s == 0 || k < 2 || k > 1024 {
        return Err(format!("unexpected groom header (S {s}, K {k})"));
    }
    let b5 = gu32(0x30 + 4 * 5) as usize;
    let pts = g.get(b5..b5 + s * k * 8).ok_or("point section outside the buffer")?;
    let points = pts.chunks_exact(8).map(|p| [0, 1, 2].map(|i| f16(u16::from_le_bytes([p[2 * i], p[2 * i + 1]])))).collect();
    Ok(Groom { strands: s, k, points })
}

/// Load a groom by id (`packs.rpf@<hex>`) or hex offset.
pub fn load(game: &Path, ix: &ArchiveIndex, id: &str) -> Result<Groom, String> {
    let off = u64::from_str_radix(id.rsplit('@').next().unwrap_or(id).trim_start_matches("0x"), 16).map_err(|_| format!("groom id {id}: expected packs.rpf@<hex>"))?;
    let e = ix.entries.iter().find(|e| e.off == off).ok_or_else(|| format!("groom {id}: no packs.rpf entry at {off:x}"))?;
    parse(&Resource::load(&game.join("packs.rpf"), e).map_err(|x| x.to_string())?).map_err(|x| format!("groom {id}: {x}"))
}

impl Groom {
    /// Crossed ribbons (two cards per strand), half-width `hw` at the root
    /// tapering to 40 % at the tip (a display choice: guide strands are
    /// sparse, so the cards are much wider than a hair). Returns positions,
    /// normals and UVs (u across the card, v along it) and triangles.
    pub fn ribbons(&self, hw: f32, strands: std::ops::Range<usize>) -> (Vec<[f32; 3]>, Vec<[f32; 3]>, Vec<[f32; 2]>, Vec<u32>) {
        let k = self.k;
        let (mut pos, mut nrm, mut uv, mut idx) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let sub = |a: [f32; 3], b: [f32; 3]| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
        let cross = |a: [f32; 3], b: [f32; 3]| [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
        let norm = |a: [f32; 3]| {
            let l = (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt();
            if l > 1e-9 { [a[0] / l, a[1] / l, a[2] / l] } else { [0.0, 0.0, 1.0] }
        };
        for s in strands {
            let p0 = &self.points[s * k..s * k + k];
            if p0.iter().any(|p| !p.iter().all(|c| c.is_finite() && c.abs() < 10.0)) {
                continue;
            }
            let base = pos.len() as u32;
            for j in 0..k {
                let t = norm(sub(p0[(j + 1).min(k - 1)], p0[j.saturating_sub(1)]));
                let up = if t[2].abs() < 0.9 { [0.0, 0.0, 1.0] } else { [1.0, 0.0, 0.0] };
                let a = norm(cross(t, up));
                let b = norm(cross(t, a));
                let r = hw * (1.0 - 0.6 * j as f32 / (k - 1) as f32);
                let v = j as f32 / (k - 1) as f32;
                for (side, nv) in [(a, b), (b, a)] {
                    for (sg, u) in [(-1.0f32, 0.0f32), (1.0, 1.0)] {
                        pos.push([p0[j][0] + side[0] * r * sg, p0[j][1] + side[1] * r * sg, p0[j][2] + side[2] * r * sg]);
                        nrm.push(nv);
                        uv.push([u, v]);
                    }
                }
            }
            for j in 0..(k - 1) as u32 {
                for q in 0..2u32 {
                    let v0 = base + j * 4 + q * 2;
                    let v1 = v0 + 4;
                    // both faces (cards are seen from both sides)
                    idx.extend_from_slice(&[v0, v0 + 1, v1 + 1, v0, v1 + 1, v1, v0, v1 + 1, v0 + 1, v0, v1, v1 + 1]);
                }
            }
        }
        (pos, nrm, uv, idx)
    }
}

/// One groom in the catalog (system page only).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct GroomInfo {
    pub id: String,
    pub off: u64,
    pub strands: u32,
    pub bones: Vec<u32>,
    pub root_box: [[f32; 3]; 2],
    /// 0 = densest of its LOD chain (same bones, fewer strands each time).
    pub lod: u32,
}

/// Every groom in packs.rpf, cached in `<work>/index/fivem_grooms_packs.json`.
pub fn catalog(game: &Path, work: &Path, ix: &ArchiveIndex) -> Result<Vec<GroomInfo>, String> {
    use rayon::prelude::*;
    let p = work.join("index").join("fivem_grooms_packs.json");
    if let Ok(v) = std::fs::read(&p).map_err(|e| e.to_string()).and_then(|b| serde_json::from_slice::<Vec<GroomInfo>>(&b).map_err(|e| e.to_string())) {
        return Ok(v);
    }
    let arc = game.join("packs.rpf");
    let cands: Vec<&Entry> = ix.entries.iter().filter(|e| e.name.is_none() && e.kind == sixng_core::classify::Kind::RageObj).collect();
    let v3 = |r: &Resource, o: usize| [r.f32(o), r.f32(o + 4), r.f32(o + 8)];
    let mut v: Vec<GroomInfo> = cands
        .par_iter()
        .filter_map(|e| {
            let r = Resource::load_sys(&arc, e).ok()?;
            is_groom(&r.sys).then(|| GroomInfo { id: e.id.clone(), off: e.off, strands: r.u16(0xDE) as u32, bones: bones_of(&r), root_box: [v3(&r, 0x30), v3(&r, 0x40)], lod: 0 })
        })
        .collect();
    v.sort_by_key(|g| g.off);
    for i in 1..v.len() {
        if v[i].bones == v[i - 1].bones && v[i].strands < v[i - 1].strands {
            v[i].lod = v[i - 1].lod + 1;
        }
    }
    let _ = std::fs::write(&p, serde_json::to_vec(&v).unwrap_or_default());
    Ok(v)
}

/// scalp / face / brows / body / animal / other, from bone names (SixNG
/// `category`): head-only grooms whose roots sit low on the front are face.
pub fn category(bones: &[u32], root_box: &[[f32; 3]; 2], names: &HashMap<u32, String>) -> &'static str {
    let n: Vec<String> = bones.iter().filter_map(|h| names.get(h).cloned().or_else(|| sixng_core::names::lookup(*h, sixng_core::names::Ctx::Field))).collect();
    let has = |f: &dyn Fn(&str) -> bool| n.iter().any(|b| f(b));
    let c = if has(&|b| b.starts_with("SKEL_Tail") || b.contains("_Fin") || b.contains("Paw") || b.contains("Wing")) {
        "animal"
    } else if has(&|b| b.contains("Brow") || b.contains("Lid") || b.contains("Lash")) {
        "brows"
    } else if has(&|b| b.contains("Ear") || b.contains("Cheek") || b.contains("Throat") || b.contains("Chin") || b.contains("Jaw") || b.contains("Lip")) {
        "face"
    } else if has(&|b| b.eq_ignore_ascii_case("BM_Head") || b.eq_ignore_ascii_case("SKEL_Head") || b.contains("Occipitalis")) {
        "scalp"
    } else if has(&|b| b.contains("Spine") || b.contains("Pelvis") || b.contains("Thigh") || b.contains("Arm")) {
        "body"
    } else {
        "other"
    };
    if c == "scalp" && root_box[1][2] < 0.74 && root_box[1][1] < 0.0 { "face" } else { c }
}
