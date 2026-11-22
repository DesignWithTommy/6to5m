//! Interiors: sixng interior instances (<placements>/interiors.jsonl plus
//! the interior-space entity files) -> GTA V MLOs. One CMloArchetypeDef per
//! 6m interior archetype in t6m_int.ytyp (decoded rooms, entity sets, the
//! interior entities) and CMloInstanceDef placements in 512 m
//! t6m_int_X_Y.ymap cells. Entity archetypes come from t6m_bld.ytyp
//! (converted by buildings.rs).
//!
//! Portals are not decoded from 6m yet, and converted interior collision
//! carries no room ids, so the game never puts the player "inside" a room:
//! every entity is attached to limbo (room 0, drawn from outside). The
//! decoded rooms are still written (name, bounds from their entities) so
//! the structure is there once portals decode. Entity-set entities go in
//! CMloEntitySets, off by default (ActivateInteriorEntitySet turns them on).

use super::super::Ctx;
use super::emit_meta;
use crate::gta5::meta::{self, arr, Archetype, EInfo, SInfo, MB};
use crate::gta5::rsc::W;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::Path,
    sync::Arc,
};

type V3 = [f32; 3];
type Q = [f32; 4];

pub struct Child {
    pub name: String,
    pub off: usize,
    pos: V3,
    /// as stored in CEntityDef (inverse of the interior-space orientation)
    rot: Q,
    sxy: f32,
    sz: f32,
    lod: f32,
    room: u32,
    set: Option<u32>,
}

pub struct Interior {
    hash: u32,
    bbox: [V3; 2],
    /// (name hash, name) in file order; limbo first
    rooms: Vec<(u32, Option<String>)>,
    pub children: Vec<Child>,
    /// (position, rotation as stored: world orientation, not inverted)
    instances: Vec<(V3, Q)>,
}

fn f3(v: &Value) -> V3 {
    std::array::from_fn(|k| v[k].as_f64().unwrap_or(0.0) as f32)
}
fn q4(v: &Value) -> Option<Q> {
    let a = v.as_array().filter(|a| a.len() == 4)?;
    Some(std::array::from_fn(|k| a[k].as_f64().unwrap_or(0.0) as f32))
}
fn hx(v: &Value) -> Option<u32> {
    v.as_str().and_then(|s| u32::from_str_radix(s, 16).ok())
}
fn qrot(q: Q, v: V3) -> V3 {
    let [x, y, z, w] = q;
    let t = [2.0 * (y * v[2] - z * v[1]), 2.0 * (z * v[0] - x * v[2]), 2.0 * (x * v[1] - y * v[0])];
    [v[0] + w * t[0] + (y * t[2] - z * t[1]), v[1] + w * t[1] + (z * t[0] - x * t[2]), v[2] + w * t[2] + (x * t[1] - y * t[0])]
}

/// Interiors whose instances fall in the region, one per archetype.
pub fn load(ctx: &Ctx, dir: &Path) -> Vec<Interior> {
    let Ok(text) = fs::read_to_string(dir.join("interiors.jsonl")) else { return Vec::new() };
    let mut files: HashMap<String, Option<Arc<Value>>> = HashMap::new();
    let mut by_hash: BTreeMap<u32, Interior> = BTreeMap::new();
    let mut missing = 0;
    for l in text.lines() {
        let Ok(inst) = serde_json::from_str::<Value>(l) else { continue };
        let ip = f3(&inst["position"]);
        if !ctx.o.in_bbox(ip, ip) {
            continue;
        }
        let Some(h) = hx(&inst["archetype"]) else { continue };
        let ri = q4(&inst["rotation_world"]).unwrap_or([0.0, 0.0, 0.0, 1.0]);
        if let Some(i) = by_hash.get_mut(&h) {
            i.instances.push((ip, ri));
            continue;
        }
        let Some(file) = inst["interior_file"].as_str().and_then(|f| f.strip_prefix("levels.rpf@")) else {
            missing += 1;
            continue;
        };
        let src = files.entry(file.to_string()).or_insert_with(|| fs::read(dir.join(format!("{file}.json"))).ok().and_then(|b| serde_json::from_slice(&b).ok()).map(Arc::new)).clone();
        let Some(src) = src else {
            missing += 1;
            continue;
        };
        let ents = src["entities"].as_array().map(Vec::as_slice).unwrap_or(&[]);
        let info = src["interiors"].as_array().into_iter().flatten().find(|v| hx(&v["interior"]) == Some(h));
        let mut rooms: Vec<(u32, Option<String>)> =
            info.and_then(|v| v["rooms"].as_array()).into_iter().flatten().filter_map(|r| Some((hx(&r["room"])?, r["name"].as_str().map(str::to_string)))).collect();
        if rooms.is_empty() {
            rooms.push((crate::gta5::rsc::joaat("limbo"), Some("limbo".into())));
        }
        let mut children = Vec::new();
        for ix in inst["entities"].as_array().into_iter().flatten().chain(inst["entity_set_entities"].as_array().into_iter().flatten()) {
            let Some(e) = ix.as_u64().and_then(|i| ents.get(i as usize)) else { continue };
            let (Some(name), Some(off)) = (e["name"].as_str(), e["model_id"].as_str().and_then(|m| m.strip_prefix("levels.rpf@")).and_then(|h| usize::from_str_radix(h, 16).ok())) else {
                continue;
            };
            let rot = q4(&e["rotation_world"]).map(|q| [-q[0], -q[1], -q[2], q[3]]).or_else(|| q4(&e["rotation"])).unwrap_or([0.0, 0.0, 0.0, 1.0]);
            children.push(Child {
                name: name.to_string(),
                off,
                pos: f3(&e["position"]),
                rot,
                sxy: e["scale_xy"].as_f64().unwrap_or(1.0) as f32,
                sz: e["scale_z"].as_f64().unwrap_or(1.0) as f32,
                lod: e["lod"]["lod_dist"].as_f64().unwrap_or(-1.0) as f32,
                room: hx(&e["interior"]["room"]).unwrap_or(0),
                set: hx(&e["interior"]["entity_set"]),
            });
        }
        let bbox = serde_json::from_value(inst["bbox"].clone()).unwrap_or([[-1.0; 3], [1.0; 3]]);
        by_hash.insert(h, Interior { hash: h, bbox, rooms, children, instances: vec![(ip, ri)] });
    }
    if missing > 0 {
        ctx.skip(&format!("({missing} interior instances)"), "interior archetype file missing", json!({"count": missing}));
    }
    let out: Vec<Interior> = by_hash.into_values().collect();
    if !out.is_empty() {
        let n: usize = out.iter().map(|i| i.instances.len()).sum();
        eprintln!("interiors: {} MLO archetypes, {n} instances, {} entities", out.len(), out.iter().map(|i| i.children.len()).sum::<usize>());
    }
    out
}

// CodeWalker MetaTypes.GetStructureInfo, verbatim.
const MLOINST: SInfo = SInfo { name: 0x09cc28be, key: 0x803e74b0, unk8: 0x400, size: 160, entries: MLOINST_E };
const MLOINST_E: &[(u32, i32, u8, u8, i16, u32)] = &[
    (0xa023a02c, 8, 0x4a, 0, 0, 0),
    (0x67d26872, 12, 0x15, 0, 0, 0),
    (0x9a7b6e6d, 16, 0x15, 0, 0, 0),
    (0x01166164, 32, 0x33, 0, 0, 0),
    (0x77dd4010, 48, 0x34, 0, 0, 0),
    (0x9ca32637, 64, 0x21, 0, 0, 0),
    (0x10fb7c42, 68, 0x21, 0, 0, 0),
    (0xd89231bd, 72, 0x14, 0, 0, 0),
    (0xbf74dfa7, 76, 0x21, 0, 0, 0),
    (0xca974bcd, 80, 0x21, 0, 0, 0),
    (0x6c8f1715, 84, 0x62, 0, 0, 0x4b5acc2f),
    (0xa687ac89, 88, 0x15, 0, 0, 0),
    (0x2691f019, 92, 0x62, 0, 0, 0x26a60207),
    (0x00000100, 0, 0x07, 0, 0, 0),
    (0xf17e7f28, 96, 0x52, 0, 13, 0),
    (0x18c1d587, 112, 0x14, 0, 0, 0),
    (0x23c0e543, 116, 0x14, 0, 0, 0),
    (0x3c852527, 120, 0x15, 0, 0, 0),
    (0x951bdd14, 128, 0x15, 0, 0, 0),
    (0x8264e631, 132, 0x15, 0, 0, 0),
    (0x00000100, 0, 0x4a, 0, 0, 0),
    (0x53df8649, 136, 0x52, 0, 20, 0),
    (0x1f837fb7, 152, 0x15, 0, 0, 0),
    (0xe03b0caa, 156, 0x15, 0, 0, 0),
];
const MLOARCH: SInfo = SInfo { name: 0x10506455, key: 0x37e3a0f2, unk8: 0x400, size: 240, entries: MLOARCH_E };
const MLOARCH_E: &[(u32, i32, u8, u8, i16, u32)] = &[
    (0xbf74dfa7, 8, 0x21, 0, 0, 0),
    (0x67d26872, 12, 0x15, 0, 0, 0),
    (0x6c1523e4, 16, 0x15, 0, 0, 0),
    (0xd9ef8236, 32, 0x33, 0, 0, 0),
    (0xe78aa618, 48, 0x33, 0, 0, 0),
    (0xd61872ff, 64, 0x33, 0, 0, 0),
    (0x82e8c5de, 80, 0x21, 0, 0, 0),
    (0xad5d5b4c, 84, 0x21, 0, 0, 0),
    (0xe6e17f14, 88, 0x4a, 0, 0, 0),
    (0x75d215a1, 92, 0x4a, 0, 0, 0),
    (0x19471791, 96, 0x4a, 0, 0, 0),
    (0xfc1fb6fb, 100, 0x4a, 0, 0, 0),
    (0xd3c717fc, 104, 0x4a, 0, 0, 0),
    (0x069004b2, 108, 0x62, 0, 0, 0x76baf7c7),
    (0x9a77c7c5, 112, 0x4a, 0, 0, 0),
    (0x00000100, 0, 0x07, 0, 0, 0),
    (0xf17e7f28, 120, 0x52, 0, 15, 0),
    (0xd607de68, 144, 0x15, 0, 0, 0),
    (0x00000100, 0, 0x07, 0, 0, 0),
    (0xccab9307, 152, 0x52, 0, 18, 0),
    (0x00000100, 0, 0x05, 0, 0, 0x0b1811f1),
    (0xcd20d7a8, 168, 0x52, 0, 20, 0),
    (0x00000100, 0, 0x05, 0, 0, 0x995072ca),
    (0x89f7e992, 184, 0x52, 0, 22, 0),
    (0x00000100, 0, 0x05, 0, 0, 0xd6a799f9),
    (0x45bcb930, 200, 0x52, 0, 24, 0),
    (0x00000100, 0, 0x05, 0, 0, 0x301d99a8),
    (0xaf9c3bd9, 216, 0x52, 0, 26, 0),
];
const ROOM: SInfo = SInfo { name: 0x0b1811f1, key: 0xe796ee15, unk8: 0x400, size: 112, entries: ROOM_E };
const ROOM_E: &[(u32, i32, u8, u8, i16, u32)] = &[
    (0xe6e17f14, 8, 0x44, 0, 0, 0),
    (0xd9ef8236, 32, 0x33, 0, 0, 0),
    (0xe78aa618, 48, 0x33, 0, 0, 0),
    (0xea73a60b, 64, 0x21, 0, 0, 0),
    (0xa261e0a9, 68, 0x4a, 0, 0, 0),
    (0xc208509c, 72, 0x4a, 0, 0, 0),
    (0x67d26872, 76, 0x15, 0, 0, 0),
    (0x41e225b3, 80, 0x15, 0, 0, 0),
    (0x8264e631, 84, 0x14, 0, 0, 0),
    (0x20f3d23e, 88, 0x14, 0, 0, 0),
    (0x00000100, 0, 0x15, 0, 0, 0),
    (0x8e05312c, 96, 0x52, 0, 10, 0),
];
const PORTAL: SInfo = SInfo { name: 0x995072ca, key: 0x422ca2c9, unk8: 0x300, size: 64, entries: PORTAL_E };
const PORTAL_E: &[(u32, i32, u8, u8, i16, u32)] = &[
    (0xf470d2fd, 8, 0x15, 0, 0, 0),
    (0x9b649621, 12, 0x15, 0, 0, 0),
    (0x67d26872, 16, 0x15, 0, 0, 0),
    (0x46a92719, 20, 0x15, 0, 0, 0),
    (0xd9394f28, 24, 0x15, 0, 0, 0),
    (0x4131e934, 28, 0x15, 0, 0, 0),
    (0x00000100, 0, 0x33, 0, 0, 0),
    (0x4f42aeb3, 32, 0x52, 0, 6, 0),
    (0x00000100, 0, 0x15, 0, 0, 0),
    (0x8e05312c, 48, 0x52, 0, 8, 0),
];
const ESET: SInfo = SInfo { name: 0xd6a799f9, key: 0xf928f783, unk8: 0x300, size: 48, entries: ESET_E };
const ESET_E: &[(u32, i32, u8, u8, i16, u32)] = &[
    (0xe6e17f14, 8, 0x4a, 0, 0, 0),
    (0x00000100, 0, 0x15, 0, 0, 0),
    (0xddfc2211, 16, 0x52, 0, 1, 0),
    (0x00000100, 0, 0x07, 0, 0, 0),
    (0xccab9307, 32, 0x52, 0, 3, 0),
];
const TCM: SInfo = SInfo { name: 0x301d99a8, key: 0x32003632, unk8: 0x400, size: 48, entries: TCM_E };
const TCM_E: &[(u32, i32, u8, u8, i16, u32)] = &[
    (0xe6e17f14, 8, 0x4a, 0, 0, 0),
    (0x38da06a2, 16, 0x34, 0, 0, 0),
    (0xafb02915, 32, 0x21, 0, 0, 0),
    (0x13d14fa4, 36, 0x21, 0, 0, 0),
    (0x2543dc07, 40, 0x15, 0, 0, 0),
    (0x16af8fb2, 44, 0x15, 0, 0, 0),
];
const T_UINT: u32 = 0x15;

fn infos(mb: &mut MB, s: &[&'static SInfo], e: &[&'static EInfo]) {
    for x in e {
        mb.einfo(x);
    }
    for x in s {
        mb.sinfo(x);
    }
}

/// CEntityDef / CMloInstanceDef head (128 bytes).
fn entity(w: &mut W, arch: u32, guid: u32, pos: V3, rot: Q, sxy: f32, sz: f32, lod: f32) {
    w.u32(0).u32(0).u32(arch).u32(32).u32(guid).zeros(12);
    w.v3(pos).f32(0.0).v4(rot);
    w.f32(sxy).f32(sz).i32(-1).f32(lod).f32(0.0);
    w.i32(5).u32(0).i32(0); // ORPHANHD, numChildren, PRI_REQUIRED
    arr(w, 0, 0);
    w.i32(255).i32(255).u32(0).u32(0);
}

fn uints(mb: &mut MB, t: u32, v: &[u32]) -> (u64, u16) {
    if v.is_empty() {
        return (0, 0);
    }
    let b: Vec<u8> = v.iter().flat_map(|x| x.to_le_bytes()).collect();
    (mb.add_item(t, b), v.len() as u16)
}

fn grow(b: &mut [V3; 2], p: V3) {
    for k in 0..3 {
        b[0][k] = b[0][k].min(p[k]);
        b[1][k] = b[1][k].max(p[k]);
    }
}

/// Box of `a` (lo, hi) placed at pos with orientation `ori`.
fn placed(lo: V3, hi: V3, pos: V3, ori: Q, sc: V3, out: &mut [V3; 2]) {
    for j in 0..8 {
        let c = [if j & 4 != 0 { hi[0] } else { lo[0] } * sc[0], if j & 2 != 0 { hi[1] } else { lo[1] } * sc[1], if j & 1 != 0 { hi[2] } else { lo[2] } * sc[2]];
        let w = qrot(ori, c);
        grow(out, [w[0] + pos[0], w[1] + pos[1], w[2] + pos[2]]);
    }
}

const EMPTY: [V3; 2] = [[f32::MAX; 3], [f32::MIN; 3]];

/// Writes t6m_int.ytyp and the t6m_int_X_Y.ymap cells. `an` maps a 6m
/// archetype name to its converted name; `archetypes` are t6m_bld.ytyp's.
pub fn emit(ctx: &Ctx, ints: &[Interior], an: &HashMap<String, String>, archetypes: &[Archetype]) -> Result<(), String> {
    let stale = |prefix: &str| {
        if let Ok(rd) = fs::read_dir(&ctx.stream) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().to_string();
                if n.starts_with(prefix) && n.ends_with(".ymap") {
                    let _ = fs::remove_file(e.path());
                }
            }
        }
    };
    let by_an: HashMap<&str, &Archetype> = archetypes.iter().map(|a| (a.name.as_str(), a)).collect();
    let name = |h: u32| format!("t6m_int_{h:08x}");
    let mut mb = MB::default();
    mb.ensure(meta::T_CMAPTYPES);
    let (mut ptrs, mut dropped, mut kept) = (Vec::new(), 0usize, Vec::new());
    let mut sets_report = Vec::new();
    for it in ints {
        let mut main: Vec<Vec<u8>> = Vec::new();
        let mut sets: BTreeMap<u32, Vec<Vec<u8>>> = BTreeMap::new();
        let mut rbox: HashMap<u32, [V3; 2]> = HashMap::new();
        let mut guid = 1u32;
        for c in &it.children {
            let Some(a) = an.get(&c.name).and_then(|n| by_an.get(n.as_str())) else {
                dropped += 1;
                continue;
            };
            let lod = if c.lod > 0.0 { c.lod } else { a.lod_dist };
            let mut w = W::new();
            entity(&mut w, meta::hash(&a.name)?, guid, c.pos, c.rot, c.sxy, c.sz, lod);
            guid += 1;
            let ori = [-c.rot[0], -c.rot[1], -c.rot[2], c.rot[3]];
            placed(a.bb_min, a.bb_max, c.pos, ori, [c.sxy, c.sxy, c.sz], rbox.entry(c.room).or_insert(EMPTY));
            match c.set {
                Some(s) => sets.entry(s).or_default().push(w.b),
                None => main.push(w.b),
            }
        }
        if main.is_empty() && sets.is_empty() {
            continue;
        }
        kept.push(it);
        let n_main = main.len() as u32;
        let (ep, en) = mb.ptr_array(meta::CENTITYDEF.name, main);
        // rooms: limbo (index 0) holds every entity, see module doc
        let mut all = EMPTY;
        for b in rbox.values() {
            grow(&mut all, b[0]);
            grow(&mut all, b[1]);
        }
        let mut rooms = Vec::new();
        for (i, (h, nm)) in it.rooms.iter().enumerate() {
            let label = nm.clone().unwrap_or_else(|| if i == 0 { "limbo".into() } else { format!("room_{h:08x}") });
            let bb = if i == 0 { all } else { rbox.get(h).copied().filter(|b| b[0][0] <= b[1][0]).unwrap_or(it.bbox) };
            let (sp, sn) = mb.add_string(&label);
            let att: Vec<u32> = if i == 0 { (0..n_main).collect() } else { Vec::new() };
            let (ap, an_) = uints(&mut mb, T_UINT, &att);
            let mut w = W::new();
            w.u32(0).u32(0);
            arr(&mut w, sp, sn);
            w.zeros(8).v3(bb[0]).f32(0.0).v3(bb[1]).f32(0.0);
            w.f32(1.0).u32(0).u32(0).u32(if i == 0 { 0 } else { 96 }).u32(0).i32(0).i32(-1).u32(0);
            arr(&mut w, ap, an_);
            rooms.push(w.b);
        }
        let nrooms = rooms.len() as u16;
        let rp = mb.add_item(ROOM.name, rooms.concat());
        let mut set_items = Vec::new();
        for (h, ents) in sets {
            sets_report.push(json!({"interior": name(it.hash), "set": format!("{h:08x}"), "entities": ents.len()}));
            let (lp, ln) = uints(&mut mb, T_UINT, &vec![0u32; ents.len()]);
            let (pp, pn) = mb.ptr_array(meta::CENTITYDEF.name, ents);
            let mut w = W::new();
            w.u32(0).u32(0).u32(h).u32(0);
            arr(&mut w, lp, ln);
            arr(&mut w, pp, pn);
            set_items.push(w.b);
        }
        let nsets = set_items.len() as u16;
        let sp = if set_items.is_empty() { 0 } else { mb.add_item(ESET.name, set_items.concat()) };
        let [lo, hi] = it.bbox;
        let c: V3 = std::array::from_fn(|k| (lo[k] + hi[k]) * 0.5);
        let r = (0..3).map(|k| (hi[k] - lo[k]).powi(2)).sum::<f32>().sqrt() * 0.5;
        let mut w = W::new();
        w.u32(0).u32(0).f32(lod_of(r)).u32(0).u32(0).zeros(12);
        w.v3(lo).f32(0.0).v3(hi).f32(0.0).v3(c).f32(0.0);
        w.f32(r.max(1.0)).f32(100.0).u32(meta::hash(&name(it.hash))?).u32(0).u32(0).u32(0).u32(0);
        w.i32(4).u32(0).u32(0); // ASSET_TYPE_ASSETLESS, assetName
        arr(&mut w, 0, 0); // extensions
        w.zeros(8).u32(0).u32(0); // mloFlags
        arr(&mut w, ep, en);
        arr(&mut w, rp, nrooms);
        arr(&mut w, 0, 0); // portals
        arr(&mut w, sp, nsets);
        arr(&mut w, 0, 0); // timeCycleModifiers
        w.zeros(8);
        debug_assert_eq!(w.len(), 240);
        ptrs.push(mb.add_item(MLOARCH.name, w.b));
    }
    if dropped > 0 {
        ctx.skip(&format!("({dropped} interior entities)"), "interior entity archetype not converted", json!({"count": dropped}));
    }
    if !sets_report.is_empty() {
        ctx.skip(&format!("({} entity sets)", sets_report.len()), "interior entity sets written but off by default (ActivateInteriorEntitySet)", json!({"sets": sets_report}));
    }
    if kept.is_empty() {
        let _ = fs::remove_file(ctx.stream.join("t6m_int.ytyp"));
        stale("t6m_int_");
        return Ok(());
    }
    let n = ptrs.len() as u16;
    let pb: Vec<u8> = ptrs.iter().flat_map(|p| p.to_le_bytes()).collect();
    let ap = mb.add_item(meta::T_POINTER, pb);
    infos(&mut mb, &[&meta::CENTITYDEF, &ROOM, &PORTAL, &ESET, &TCM, &MLOARCH, &meta::CMAPTYPES], &[&meta::E_LODTYPE, &meta::E_PRIORITY, &meta::E_ASSETTYPE]);
    let mut w = W::new();
    w.u32(0).u32(0);
    arr(&mut w, 0, 0);
    arr(&mut w, ap, n);
    w.u32(meta::hash("t6m_int")?).u32(0);
    arr(&mut w, 0, 0);
    arr(&mut w, 0, 0);
    mb.blocks[0].1.extend_from_slice(&w.b);
    emit_meta(ctx, "t6m_int.ytyp", mb.build()?)?;

    // instances in 512 m cells
    let mut cells: BTreeMap<(i64, i64), Vec<(&Interior, V3, Q)>> = BTreeMap::new();
    for it in &kept {
        for &(p, q) in &it.instances {
            cells.entry(((p[0] / 512.0).floor() as i64, (p[1] / 512.0).floor() as i64)).or_default().push((it, p, q));
        }
    }
    stale("t6m_int_");
    for ((x, y), list) in cells {
        let file = format!("t6m_int_{x}_{y}");
        let mut mb = MB::default();
        mb.ensure(meta::T_CMAPDATA);
        let (mut ee, mut se) = (EMPTY, EMPTY);
        let mut ps = Vec::new();
        for (k, (it, p, q)) in list.iter().enumerate() {
            let [lo, hi] = it.bbox;
            let r = (0..3).map(|k| (hi[k] - lo[k]).powi(2)).sum::<f32>().sqrt() * 0.5;
            let lod = lod_of(r);
            let mut w = W::new();
            entity(&mut w, meta::hash(&name(it.hash))?, k as u32 + 1, *p, *q, 1.0, 1.0, lod);
            w.u32(0).u32(0); // groupId, floorId
            arr(&mut w, 0, 0); // defaultEntitySets
            w.u32(0).u32(0); // numExitPortals, MLOInstflags
            ps.push(mb.add_item(MLOINST.name, w.b));
            placed(lo, hi, *p, *q, [1.0; 3], &mut ee);
            placed(lo.map(|v| v - lod), hi.map(|v| v + lod), *p, *q, [1.0; 3], &mut se);
        }
        let n = ps.len() as u16;
        let ep = mb.add_item(meta::T_POINTER, ps.iter().flat_map(|p| p.to_le_bytes()).collect());
        let (np, nn) = mb.add_string(&file);
        let (xp, xn) = mb.add_string("6ixpack fivem");
        infos(&mut mb, &[&MLOINST, &meta::INSTANCED, &meta::LODLIGHT, &meta::DISTLODLIGHT, &meta::BLOCKDESC, &meta::CMAPDATA], &[&meta::E_LODTYPE, &meta::E_PRIORITY]);
        let mut w = W::new();
        w.u32(0).u32(0).u32(meta::hash(&file)?).u32(0).u32(0).u32(9).u32(0).u32(0);
        w.v3(se[0]).f32(0.0).v3(se[1]).f32(0.0).v3(ee[0]).f32(0.0).v3(ee[1]).f32(0.0);
        arr(&mut w, ep, n);
        for _ in 0..4 {
            arr(&mut w, 0, 0);
        }
        w.zeros(48);
        arr(&mut w, 0, 0);
        arr(&mut w, 0, 0);
        w.zeros(136);
        w.zeros(48);
        w.u32(0).u32(0);
        arr(&mut w, np, nn);
        arr(&mut w, xp, xn);
        arr(&mut w, 0, 0);
        arr(&mut w, 0, 0);
        debug_assert_eq!(w.len(), 512);
        mb.blocks[0].1.extend_from_slice(&w.b);
        emit_meta(ctx, &format!("{file}.ymap"), mb.build()?)?;
    }
    Ok(())
}

/// MLO lodDist from the interior's bounding radius (100..500 m).
fn lod_of(r: f32) -> f32 {
    (r * 10.0).clamp(100.0, 500.0)
}
