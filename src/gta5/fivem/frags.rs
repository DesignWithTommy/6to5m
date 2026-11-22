//! Dynamic world props: a 6m fragment (`ft`) prop as a GTA V .yft instead of
//! a static .ydr, so it can be pushed, knocked over and broken.
//!
//! The prop becomes one rigid fragment: a root bone, the built mesh as the
//! high LOD, and one box physics child over its bounds. Mass is the sum of
//! the 6m collider masses (GTA6-Asset-Info 13.4: root +0x28 colliders, stride
//! 0x50, mass f32 +0x2C, count u16 +0x36), else a density estimate from the
//! box. Multi-group breakables (fences, glass) stay one piece for now.
//! Hooked from `buildings.rs` (fragment path) once its open edits land.

use crate::gta5::{
    ydr::Mesh,
    yft::{self, Bone, Child, Frag, Group, Model, V3},
};
use sixng_core::{resource::Resource, scan::Entry};
use std::path::Path;

/// Summed 6m collider mass of a fragment resource; None when it has none.
pub(super) fn collider_mass(rpf: &Path, e: &Entry) -> Option<f32> {
    let r = Resource::load(rpf, e).ok()?;
    let u16at = |o: usize| r.sys.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let cp = r.sys_off(r.u64(0x28))?;
    let nc = u16at(0x36)? as usize;
    if nc == 0 || cp + nc * 0x50 > r.sys.len() {
        return None;
    }
    let m: f32 = (0..nc).filter_map(|c| r.sys.get(cp + c * 0x50 + 0x2c..cp + c * 0x50 + 0x30)).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).filter(|m| m.is_finite() && *m > 0.0).sum();
    (m > 0.0).then_some(m)
}

/// Box mass at a light prop density (~150 kg/m^3: hollow crates, bins,
/// furniture), clamped to what GTA V props usually weigh.
fn estimate_mass(lo: V3, hi: V3) -> f32 {
    let v = (hi[0] - lo[0]).max(0.05) * (hi[1] - lo[1]).max(0.05) * (hi[2] - lo[2]).max(0.05);
    (v * 150.0).clamp(1.0, 2000.0)
}

/// One-piece .yft for a prop mesh (geometry in prop space).
pub(super) fn build_prop_yft(mesh: &Mesh, mass: Option<f32>, lod_dist: f32) -> Result<Vec<u8>, String> {
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
    let mass = mass.unwrap_or_else(|| estimate_mass(lo, hi));
    let mut f = Frag {
        name: mesh.name.clone(),
        bones: vec![Bone { name: mesh.name.clone(), parent: None, t: [0.0; 3], r: [0.0, 0.0, 0.0, 1.0] }],
        lod_dist: [lod_dist, lod_dist, lod_dist, lod_dist],
        groups: vec![Group { name: mesh.name.clone(), parent: None }],
        children: vec![Child { bone: 0, group: 0, min: lo, max: hi, mass, drawable: Vec::new() }],
        mass,
        ..Default::default()
    };
    f.lods[0] = vec![Model { bone: 0, geoms: mesh.geoms.clone() }];
    yft::build_yft(&f)
}
