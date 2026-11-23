//! Night lighting: exterior 6m lights as GTA V LODLights and
//! DistantLODLights (`t6m_lodlights_X_Y.ymap` with parent
//! `t6m_distlights_X_Y.ymap`, 512 m cells), packed as CodeWalker's
//! GenerateLODLightsPanel does. Sources are the light sets of each world
//! entity in the region: its own (in its placement file) and its
//! archetype's (in whichever meta file defines that archetype, found via a
//! index the interior-ents prep pass writes, <work>/index/light_index_v1.json).
//! T6M_NO_LIGHTS=1 skips lights.

use super::{emit_meta, Ctx};
use crate::gta5::meta::{self, arr, MB};
use crate::gta5::rsc::W;
use serde_json::Value;
use sixng_core::{lights::Light, resource::Resource, scan::{ArchiveIndex, Entry}};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::Path,
};

type V3 = [f32; 3];

/// FloatXYZ (MetaVECTOR3 items) and plain array element types
const T_XYZ: u32 = 0xe2cbcfd4;
const T_UINT: u32 = 0x15;
const T_FLOAT: u32 = 0x21;
const T_UBYTE: u32 = 0x11;

/// Archetype hash -> meta file offset (hex) of the file defining that
/// archetype's light extension: written by the interior-ents prep pass
/// (one parallel read of every meta, shared), cached in <work>/index.
pub fn light_index(ctx: &Ctx) -> HashMap<u32, String> {
    let p = ctx.o.work.join("index").join(sixng_core::interiorents::LIGHT_INDEX);
    let m: HashMap<String, String> = sixng_core::vfs::read(&p).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    m.into_iter().filter_map(|(k, v)| Some((u32::from_str_radix(&k, 16).ok()?, v))).collect()
}

fn rotate(q: [f32; 4], v: V3) -> V3 {
    sixng_core::lights::rotate(q, v)
}

fn f3(v: &Value) -> V3 {
    std::array::from_fn(|k| v[k].as_f64().unwrap_or(0.0) as f32)
}

struct LodLight {
    pos: V3,
    dir: V3,
    rgbi: u32,
    falloff: f32,
    exp: f32,
    flags: u32,
    hash: u32,
    inner: u8,
    outer: u8,
    corona: u8,
}

fn pack(l: &Light, pos: V3, dir: V3, hash: u32) -> Option<LodLight> {
    let ty = l.light_type;
    if !matches!(ty, 1 | 2 | 4) {
        return None;
    }
    let c = u32::from_str_radix(&l.colour, 16).unwrap_or(0xffffff) & 0xff_ffff;
    let i = (l.intensity * 5.3125).round().clamp(0.0, 255.0) as u32;
    let time = u32::from_str_radix(&l.time_flags, 16).unwrap_or(0xff_ffff) & 0xff_ffff;
    let b = |x: f32| x.round().clamp(0.0, 255.0) as u8;
    Some(LodLight {
        pos,
        dir,
        rgbi: (i << 24) | c,
        falloff: l.range,
        exp: l.falloff_exponent,
        flags: time | (ty << 26),
        hash,
        inner: b(l.cone_inner * 1.4117647),
        outer: if ty == 4 { b(l.extents[0] * 1.82) } else { b(l.cone_outer * 1.4117647) },
        corona: if l.corona_size != 0.0 { b(l.corona_intensity * 6.0) } else { 0 },
    })
}

/// Writes the LODLights / DistantLODLights ymaps for the world entities of
/// the placement files in `dir` (index.jsonl) inside the region.
pub fn emit(ctx: &Ctx, dir: &Path, index: &ArchiveIndex) -> Result<(), String> {
    if let Ok(rd) = fs::read_dir(&ctx.stream) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if (n.starts_with("t6m_lodlights_") || n.starts_with("t6m_distlights_")) && n.ends_with(".ymap") {
                let _ = fs::remove_file(e.path());
            }
        }
    }
    if std::env::var_os("T6M_NO_LIGHTS").is_some() {
        return Ok(());
    }
    let Ok(idx) = sixng_core::vfs::read_to_string(dir.join("index.jsonl")) else { return Ok(()) };
    // world entities in the region: (file offset, entity index, archetype, pos, world rotation, scale)
    let mut ents: Vec<(String, usize, u32, V3, [f32; 4], V3)> = Vec::new();
    for l in idx.lines() {
        let Ok(v) = serde_json::from_str::<Value>(l) else { continue };
        if v["space"] != "world" {
            continue;
        }
        let (lo, hi) = (f3(&v["bbox"][0]), f3(&v["bbox"][1]));
        if !ctx.o.in_bbox(lo, hi) {
            continue;
        }
        let (Some(id), Some(file)) = (v["id"].as_str().and_then(|s| s.strip_prefix("levels.rpf@")), v["file"].as_str()) else { continue };
        let name = Path::new(file).file_name().map(|f| f.to_owned()).unwrap_or_default();
        let Some(doc) = sixng_core::vfs::read(dir.join(name)).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) else { continue };
        for (i, e) in doc["entities"].as_array().into_iter().flatten().enumerate() {
            let p = f3(&e["position"]);
            if !ctx.o.in_bbox(p, p) {
                continue;
            }
            let q = e["rotation_world"].as_array().filter(|a| a.len() == 4).map(|a| std::array::from_fn(|k| a[k].as_f64().unwrap_or(0.0) as f32)).unwrap_or([0.0, 0.0, 0.0, 1.0]);
            let (sxy, sz) = (e["scale_xy"].as_f64().unwrap_or(1.0) as f32, e["scale_z"].as_f64().unwrap_or(1.0) as f32);
            ents.push((id.to_string(), i, e["archetype"].as_u64().unwrap_or(0) as u32, p, q, [sxy, sxy, sz]));
        }
    }
    if ents.is_empty() {
        return Ok(());
    }
    let lix = light_index(ctx);
    let mut files: std::collections::BTreeSet<&str> = ents.iter().map(|e| e.0.as_str()).collect();
    files.extend(ents.iter().filter_map(|e| lix.get(&e.2).map(String::as_str)));
    let levels = ctx.o.game.join("levels.rpf");
    let by_off: HashMap<u64, &Entry> = index.entries.iter().map(|e| (e.off, e)).collect();
    let mut by_arch: HashMap<u32, Vec<(V3, Light)>> = HashMap::new();
    let mut by_ent: HashMap<(&str, usize), Vec<(V3, Light)>> = HashMap::new();
    let by_off_files = files.len();
    for f in files {
        let Some(e) = u64::from_str_radix(f, 16).ok().and_then(|o| by_off.get(&o)) else { continue };
        let Ok(r) = Resource::load_sys(&levels, e) else { continue };
        for s in sixng_core::lights::light_sets(&r) {
            let ls: Vec<(V3, Light)> = s.lights.into_iter().map(|l| (s.ext_offset, l)).collect();
            match (s.owner, s.entity_index) {
                ("archetype", _) if lix.get(&s.archetype).is_none_or(|x| x == f) => {
                    by_arch.entry(s.archetype).or_insert(ls);
                }
                ("entity", Some(i)) => by_ent.entry((f, i)).or_default().extend(ls),
                _ => {}
            }
        }
    }
    let off = meta::world_offset();
    let mut cells: BTreeMap<(i64, i64), Vec<LodLight>> = BTreeMap::new();
    for (f, i, arch, p, q, sc) in &ents {
        let sets = by_arch.get(arch).into_iter().chain(by_ent.get(&(f.as_str(), *i))).flatten();
        for (k, (eo, l)) in sets.enumerate() {
            let lp = [(l.position[0] + eo[0]) * sc[0], (l.position[1] + eo[1]) * sc[1], (l.position[2] + eo[2]) * sc[2]];
            let v = rotate(*q, lp);
            let wp = [p[0] + v[0], p[1] + v[1], p[2] + v[2]];
            let hash = crate::gta5::rsc::joaat(&format!("{f}:{i}:{k}"));
            let Some(ll) = pack(l, std::array::from_fn(|j| wp[j] + off[j]), rotate(*q, l.direction), hash) else { continue };
            cells.entry(((wp[0] / 512.0).floor() as i64, (wp[1] / 512.0).floor() as i64)).or_default().push(ll);
        }
    }
    let (mut total, ncells, nfiles) = (0, cells.len(), by_off_files);
    for ((x, y), mut ls) in cells {
        ls.sort_by_key(|l| l.hash);
        total += ls.len();
        let dist = format!("t6m_distlights_{x}_{y}");
        let lod = format!("t6m_lodlights_{x}_{y}");
        let mut bb = [[f32::MAX; 3], [f32::MIN; 3]];
        for l in &ls {
            for k in 0..3 {
                bb[0][k] = bb[0][k].min(l.pos[k]);
                bb[1][k] = bb[1][k].max(l.pos[k]);
            }
        }
        let grow = |d: f32| [bb[0].map(|v| v - d), bb[1].map(|v| v + d)];
        // distant: positions + RGBI
        let mut mb = MB::default();
        mb.ensure(meta::T_CMAPDATA);
        let pp = mb.add_item(T_XYZ, ls.iter().flat_map(|l| l.pos).flat_map(f32::to_le_bytes).collect());
        let cp = mb.add_item(T_UINT, ls.iter().flat_map(|l| l.rgbi.to_le_bytes()).collect());
        let n = ls.len() as u16;
        let mut soa = W::new();
        soa.zeros(8);
        arr(&mut soa, pp, n);
        arr(&mut soa, cp, n);
        soa.u16(0).u16(1).zeros(4); // numStreetLights, category (medium)
        emit_meta(ctx, &format!("{dist}.ymap"), mapdata(mb, &dist, "", 2, 256, grow(3000.0), grow(20.0), None, Some(soa.b))?)?;
        // lod: the rest of the light, parent = distant
        let mut mb = MB::default();
        mb.ensure(meta::T_CMAPDATA);
        let dp = mb.add_item(T_XYZ, ls.iter().flat_map(|l| l.dir).flat_map(f32::to_le_bytes).collect());
        let fp = mb.add_item(T_FLOAT, ls.iter().flat_map(|l| l.falloff.to_le_bytes()).collect());
        let ep = mb.add_item(T_FLOAT, ls.iter().flat_map(|l| l.exp.to_le_bytes()).collect());
        let tp = mb.add_item(T_UINT, ls.iter().flat_map(|l| l.flags.to_le_bytes()).collect());
        let hp = mb.add_item(T_UINT, ls.iter().flat_map(|l| l.hash.to_le_bytes()).collect());
        let ip = mb.add_item(T_UBYTE, ls.iter().map(|l| l.inner).collect());
        let op = mb.add_item(T_UBYTE, ls.iter().map(|l| l.outer).collect());
        let rp = mb.add_item(T_UBYTE, ls.iter().map(|l| l.corona).collect());
        let mut soa = W::new();
        soa.zeros(8);
        for p in [dp, fp, ep, tp, hp, ip, op, rp] {
            arr(&mut soa, p, n);
        }
        emit_meta(ctx, &format!("{lod}.ymap"), mapdata(mb, &lod, &dist, 0, 128, grow(950.0), grow(20.0), Some(soa.b), None)?)?;
    }
    if total > 0 {
        eprintln!("[t6m:interiors] lodlights: {total} exterior lights in {ncells} cells, {} entities, {nfiles} meta files read", ents.len());
    }
    Ok(())
}

/// A CMapData holding only LODLights or DistantLODLights SOA data.
#[allow(clippy::too_many_arguments)]
fn mapdata(mut mb: MB, name: &str, parent: &str, flags: u32, content: u32, se: [V3; 2], ee: [V3; 2], lod: Option<Vec<u8>>, dist: Option<Vec<u8>>) -> Result<Vec<u8>, String> {
    let (np, nn) = mb.add_string(name);
    let (xp, xn) = mb.add_string("6ixpack fivem");
    mb.sinfo(&meta::INSTANCED);
    mb.sinfo(&meta::LODLIGHT);
    mb.sinfo(&meta::DISTLODLIGHT);
    // the direction/position arrays are FloatXYZ blocks (CodeWalker
    // YmapFile.BuildCMapData adds this whenever LOD lights are present);
    // without it the loader crashed on a null structure (+1361943)
    mb.sinfo(&meta::FLOATXYZ);
    mb.sinfo(&meta::BLOCKDESC);
    mb.sinfo(&meta::CMAPDATA);
    let mut w = W::new();
    w.u32(0).u32(0).u32(meta::hash(name)?).u32(if parent.is_empty() { 0 } else { meta::hash(parent)? }).u32(flags).u32(content).u32(0).u32(0);
    w.v3(se[0]).f32(0.0).v3(se[1]).f32(0.0).v3(ee[0]).f32(0.0).v3(ee[1]).f32(0.0);
    for _ in 0..5 {
        arr(&mut w, 0, 0); // entities, containerLods, boxOccluders, occludeModels, physicsDictionaries
    }
    w.zeros(48); // instancedData
    arr(&mut w, 0, 0); // timeCycleModifiers
    arr(&mut w, 0, 0); // carGenerators
    match lod {
        Some(b) => w.bytes(&b),
        None => w.zeros(136),
    };
    match dist {
        Some(b) => w.bytes(&b),
        None => w.zeros(48),
    };
    w.u32(0).u32(0);
    arr(&mut w, np, nn);
    arr(&mut w, xp, xn);
    arr(&mut w, 0, 0);
    arr(&mut w, 0, 0);
    debug_assert_eq!(w.len(), 512);
    mb.blocks[0].1.extend_from_slice(&w.b);
    mb.build()
}
