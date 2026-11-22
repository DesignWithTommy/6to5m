//! Dynamic world props: a 6m fragment (`ft`) prop as a GTA V .yft instead of
//! a static .ydr, so it can be pushed, knocked over and broken.
//!
//! The prop becomes one rigid fragment: a root bone, the built mesh as the
//! high LOD, and one physics child per 6m collider (GTA6-Asset-Info
//! 13-collision.md, "Fragment physics"): root +0x20 groups (stride 0x70,
//! u16 skeleton index +0x10), +0x28 colliders (stride 0x50: quaternion +0x00,
//! bone-local translation +0x10, mass +0x2C, bound pointer +0x30, owning
//! group u16 +0x48), counts u16 +0x34 / +0x36; bound type u8 +0x10, max/min
//! +0x20 / +0x30. Placement R_b * (R_c * p + t_c) + t_b with the group's bone
//! (same section, "Collider bind transforms"). Type 3 (box) is exact; the
//! other 6m types (1, 2, 9, 14: sphere/capsule/hull-like, unconfirmed) become
//! their bound box. Without colliders: one box over the mesh at a density
//! estimate. Multi-group breakables stay one piece (one group) for now.
//! Hooked from `buildings.rs` (fragment path).

use crate::gta5::{
    ydr::Mesh,
    yft::{self, Bone, Child, Frag, Group, Model, V3},
};
use sixng_core::{drawable, resource::Resource, scan::Entry};
use std::path::Path;

/// One 6m collider as a prop-space box: min, max, mass.
#[derive(Clone, Debug)]
pub(super) struct Collider {
    pub min: V3,
    pub max: V3,
    pub mass: f32,
}

/// The 6m colliders of a fragment in prop (model) space.
pub(super) fn colliders(rpf: &Path, e: &Entry) -> Vec<Collider> {
    let Ok(r) = Resource::load(rpf, e) else { return Vec::new() };
    let u16at = |o: usize| r.sys.get(o..o + 2).map_or(0xFFFF, |b| u16::from_le_bytes([b[0], b[1]]));
    let f = |o: usize| r.sys.get(o..o + 4).map_or(f32::NAN, |b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let v3 = |o: usize| [f(o), f(o + 4), f(o + 8)];
    let (Some(gp), Some(cp)) = (r.sys_off(r.u64(0x20)), r.sys_off(r.u64(0x28))) else { return Vec::new() };
    let (ng, nc) = (u16at(0x34) as usize, u16at(0x36) as usize);
    if nc == 0 || gp + ng * 0x70 > r.sys.len() || cp + nc * 0x50 > r.sys.len() {
        return Vec::new();
    }
    let sk = drawable::find_skeleton(&r);
    let bones: Vec<Bone> = sk.iter().map(|b| Bone { name: b.name.clone(), parent: b.parent.map(|p| p as usize), t: b.translation, r: b.rotation }).collect();
    let g = yft::globals(&bones);
    let mut out = Vec::new();
    for c in 0..nc {
        let o = cp + c * 0x50;
        let Some(b) = r.sys_off(r.u64(o + 0x30)) else { continue };
        let (bmax, bmin) = (v3(b + 0x20), v3(b + 0x30));
        let mass = f(o + 0x2c);
        if !(0..3).all(|k| bmin[k].is_finite() && bmax[k].is_finite() && bmin[k] <= bmax[k]) || !mass.is_finite() {
            continue;
        }
        let qc = [f(o), f(o + 4), f(o + 8), f(o + 12)];
        let tc = v3(o + 0x10);
        let grp = u16at(o + 0x48) as usize;
        let bi = if grp < ng { u16at(gp + grp * 0x70 + 0x10) as usize } else { 0xFFFF };
        let (tb, qb) = g.get(bi).copied().unwrap_or(([0.0; 3], [0.0, 0.0, 0.0, 1.0]));
        let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
        for k in 0..8 {
            let p = [if k & 1 == 0 { bmin[0] } else { bmax[0] }, if k & 2 == 0 { bmin[1] } else { bmax[1] }, if k & 4 == 0 { bmin[2] } else { bmax[2] }];
            let pc = yft::qrot(qc, p);
            let pb = yft::qrot(qb, [pc[0] + tc[0], pc[1] + tc[1], pc[2] + tc[2]]);
            let w = [pb[0] + tb[0], pb[1] + tb[1], pb[2] + tb[2]];
            for a in 0..3 {
                lo[a] = lo[a].min(w[a]);
                hi[a] = hi[a].max(w[a]);
            }
        }
        out.push(Collider { min: lo, max: hi, mass: mass.max(0.01) });
    }
    out
}

/// Box mass at a light prop density (~150 kg/m^3: hollow crates, bins,
/// furniture), clamped to what GTA V props usually weigh.
fn estimate_mass(lo: V3, hi: V3) -> f32 {
    let v = (hi[0] - lo[0]).max(0.05) * (hi[1] - lo[1]).max(0.05) * (hi[2] - lo[2]).max(0.05);
    (v * 150.0).clamp(1.0, 2000.0)
}

/// One-piece .yft for a prop mesh (geometry in prop space).
pub(super) fn build_prop_yft(mesh: &Mesh, cols: &[Collider], lod_dist: f32) -> Result<Vec<u8>, String> {
    let pts = mesh.geoms.iter().flat_map(|g| g.positions.iter());
    let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
    for p in pts {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    if lo[0] > hi[0] {
        return Err("no geometry".into());
    }
    let cols: Vec<Collider> = if cols.is_empty() { vec![Collider { min: lo, max: hi, mass: estimate_mass(lo, hi) }] } else { cols.to_vec() };
    let mass: f32 = cols.iter().map(|c| c.mass).sum();
    let mut f = Frag {
        name: mesh.name.clone(),
        bones: vec![Bone { name: mesh.name.clone(), parent: None, t: [0.0; 3], r: [0.0, 0.0, 0.0, 1.0] }],
        lod_dist: [lod_dist, lod_dist, lod_dist, lod_dist],
        groups: vec![Group { name: mesh.name.clone(), parent: None }],
        children: cols.iter().take(255).map(|c| Child { bone: 0, group: 0, min: c.min, max: c.max, mass: c.mass, drawable: Vec::new() }).collect(),
        mass,
        ..Default::default()
    };
    f.lods[0] = vec![Model { bone: 0, geoms: mesh.geoms.clone() }];
    yft::build_yft(&f)
}
