//! Static collision: 6m physics bounds (sixng bounds decoder, already in
//! world space) merged into one t6m_cc_XXX_YYY.ybn per 128 m cell
//! (XXX = cell + 200), welded on a 5 cm grid and simplified per material:
//! collision only needs the walkable shape, not render detail.
//!
//! The bounds' world bboxes are cached in <work>/fivem_collision_index.jsonl
//! (one decode pass over every unnamed rageobj resource of levels.rpf).
//!
//! Materials: the 6m surface id is a hash of an unknown name, so ids map to
//! GTA V materials (materials.dat order, as in Sollumz collision_materials.py)
//! through the categories `sixng surfaces` infers
//! (<work>/collision/levels/surface_categories.json). Without that file, or
//! for an id it doesn't cover, the material is 0 (DEFAULT).

use super::{sha, Ctx, YBN_V};
use crate::gta5::ybn;
use rayon::prelude::*;
use serde_json::{json, Value};
use sixng_core::{
    resource::Resource,
    classify::Kind,
    scan::{ArchiveIndex, Entry},
};
use std::{collections::BTreeMap, error::Error, fs, io::Write};

const CELL: f32 = 128.0;
/// meshopt error bound, relative to a cell's extent (~0.4 m at 128 m).
const SIMPLIFY_ERR: f32 = 0.003;
/// Triangles kept at most, as a fraction of the welded input.
const SIMPLIFY_KEEP: f32 = 0.25;

/// GTA V material index for an inferred 6m surface category.
pub fn gta_material(category: &str) -> u8 {
    let c = category.to_ascii_lowercase();
    let has = |k: &str| c.contains(k);
    if has("generic") {
        0
    } else if has("ballast variant") {
        31 // GRAVEL_SMALL
    } else if has("ballast") || has("gravel") {
        34 // GRAVEL_TRAIN_TRACK
    } else if has("dirt track") {
        35 // DIRT_TRACK
    } else if has("road") {
        4 // TARMAC
    } else if has("seabed") || has("underwater") {
        22 // SAND_UNDERWATER
    } else if has("shore") {
        20 // SAND_WET
    } else if has("sand") {
        18 // SAND_LOOSE
    } else if has("wetland") || has("swamp") {
        41 // MARSH
    } else if has("forest") {
        43 // SOIL
    } else if has("grass") || has("vegetated") {
        47 // GRASS
    } else if has("rock") {
        9 // ROCK
    } else if has("paved") || has("hard") || has("wall") || has("structure") {
        1 // CONCRETE
    } else {
        0
    }
}

/// 6m surface id -> GTA V material, from `sixng surfaces` output.
fn material_map(ctx: &Ctx) -> BTreeMap<u16, u8> {
    let p = ctx.o.work.join("collision").join("levels").join("surface_categories.json");
    let Some(doc) = fs::read_to_string(&p).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) else {
        ctx.skip("(collision materials)", "surface_categories.json missing (run `6ixpack sixng surfaces`); all collision is DEFAULT", json!({}));
        return BTreeMap::new();
    };
    doc["surfaces"]
        .as_object()
        .map(|m| m.iter().filter_map(|(k, v)| Some((u16::from_str_radix(k, 16).ok()?, gta_material(v["category"].as_str()?)))).collect())
        .unwrap_or_default()
}

fn decode(path: &std::path::Path, e: &Entry, full: bool) -> Option<sixng_core::bounds::Collision> {
    let r = Resource::load_sys(path, e).ok()?;
    let c = if full { sixng_core::bounds::decode(&r)? } else { sixng_core::bounds::decode_movement(&r)? };
    (!c.by_mat.values().all(|x| x.1.is_empty())).then_some(c)
}

pub fn build(ctx: &Ctx, index: &ArchiveIndex) -> Result<(), Box<dyn Error>> {
    let rpf = ctx.o.game.join("levels.rpf");
    let cands: Vec<&Entry> = index.entries.iter().filter(|e| e.kind == Kind::RageObj && e.name.is_none()).collect();
    // id -> world bbox of entries that hold collision
    let ip = ctx.o.work.join("fivem_collision_index.jsonl");
    let mut boxes: BTreeMap<String, [[f32; 3]; 2]> = BTreeMap::new();
    let cached = fs::read_to_string(&ip).ok().filter(|t| t.lines().next().is_some_and(|l| l.contains(&format!("\"entries\":{}", cands.len()))));
    if let Some(t) = cached {
        for l in t.lines().skip(1) {
            if let Ok(v) = serde_json::from_str::<Value>(l) {
                if let (Some(id), Ok(b)) = (v["id"].as_str(), serde_json::from_value::<[[f32; 3]; 2]>(v["bbox"].clone())) {
                    boxes.insert(id.to_string(), b);
                }
            }
        }
    } else {
        eprintln!("collision: indexing {} resources (once; cached in {})", cands.len(), ip.display());
        let found: Vec<(String, [[f32; 3]; 2])> = cands.par_iter().filter_map(|e| decode(&rpf, e, true).map(|c| (e.id.clone(), c.bbox))).collect();
        let mut f = fs::File::create(&ip)?;
        writeln!(f, "{}", json!({"tool": YBN_V, "entries": cands.len()}))?;
        for (id, b) in &found {
            writeln!(f, "{}", json!({"id": id, "bbox": b}))?;
            boxes.insert(id.clone(), *b);
        }
    }
    let mats = material_map(ctx);
    let mats_fp = sha(&format!("{mats:?}"));
    let by_id: BTreeMap<&str, &Entry> = cands.iter().map(|e| (e.id.as_str(), *e)).collect();
    let sel: Vec<&Entry> = boxes.iter().filter(|(_, b)| ctx.o.in_bbox(b[0], b[1])).filter_map(|(id, _)| by_id.get(id.as_str()).copied()).collect();
    eprintln!("collision: {} bounds in region", sel.len());
    // cells by bbox centre
    let mut cells: BTreeMap<(i64, i64), Vec<&Entry>> = BTreeMap::new();
    for e in &sel {
        let b = boxes[&e.id];
        let c = [(b[0][0] + b[1][0]) / 2.0, (b[0][1] + b[1][1]) / 2.0];
        cells.entry(((c[0] / CELL).floor() as i64, (c[1] / CELL).floor() as i64)).or_default().push(e);
    }
    let res: Vec<Result<(), String>> = cells
        .par_iter()
        .map(|((cx, cy), es)| {
            let file = format!("t6m_cc_{:03}_{:03}.ybn", cx + 200, cy + 200);
            let fp = sha(&format!("{YBN_V}|col|{mats_fp}|{}|{}{}", ctx.o.weapon_collision, es.iter().map(|e| e.id.as_str()).collect::<Vec<_>>().join(","), ctx.o.offset_fp()));
            if let Some(info) = ctx.up_to_date(&file, &fp) {
                ctx.emit(&file, &fp, info, None)?;
                return Ok(());
            }
            let (mut verts, mut tris, mut tm): (Vec<[f32; 3]>, Vec<[u32; 3]>, Vec<u8>) = (Vec::new(), Vec::new(), Vec::new());
            for e in es {
                let Some(c) = decode(&rpf, e, ctx.o.weapon_collision) else { continue };
                for (id, (v, idx)) in &c.by_mat {
                    let base = verts.len() as u32;
                    verts.extend_from_slice(v);
                    tris.extend(idx.chunks_exact(3).map(|t| [base + t[0], base + t[1], base + t[2]]));
                    tm.resize(tris.len(), mats.get(id).copied().unwrap_or(0));
                }
            }
            if tris.is_empty() {
                return Ok(());
            }
            let raw = tris.len();
            let (verts, tris, tm) = weld(&verts, &tris, &tm);
            let (verts, tris, tm) = simplify(verts, &tris, &tm);
            let verts: Vec<[f32; 3]> = verts.iter().map(|v| ctx.o.shift(*v)).collect();
            let bytes = ybn::build_ybn_mats(&verts, &tris, &tm).map_err(|e| format!("{file}: {e}"))?;
            let info = json!({"bounds": es.len(), "triangles": tris.len(), "raw_triangles": raw});
            let s = ctx.emit(&file, &fp, info, Some(bytes))?;
            ctx.report.lock().unwrap().converted.push(json!({"kind": "collision", "ybn": s, "bounds": es.len(), "triangles": tris.len()}));
            Ok(())
        })
        .collect();
    for r in res {
        r?;
    }
    Ok(())
}

/// Per-material meshopt simplification (materials stay separate so their
/// borders are kept), then unused vertices are dropped.
fn simplify(verts: Vec<[f32; 3]>, tris: &[[u32; 3]], mats: &[u8]) -> (Vec<[f32; 3]>, Vec<[u32; 3]>, Vec<u8>) {
    let mut by: BTreeMap<u8, Vec<u32>> = BTreeMap::new();
    for (t, m) in tris.iter().zip(mats) {
        by.entry(*m).or_default().extend_from_slice(t);
    }
    let (mut out_t, mut out_m) = (Vec::new(), Vec::new());
    for (m, idx) in by {
        let mut dst = vec![0u32; idx.len()];
        let target = ((idx.len() / 3) as f32 * SIMPLIFY_KEEP) as usize * 3;
        let k = if idx.len() >= 300 { meshopt_rs::simplify::simplify(&mut dst, &idx, &verts, target, SIMPLIFY_ERR) } else { 0 };
        let kept = if k >= 3 { &dst[..k] } else { &idx[..] };
        out_t.extend(kept.chunks_exact(3).map(|t| [t[0], t[1], t[2]]));
        out_m.resize(out_t.len(), m);
    }
    let mut remap = vec![u32::MAX; verts.len()];
    let mut out_v = Vec::new();
    for t in out_t.iter_mut() {
        for i in t.iter_mut() {
            if remap[*i as usize] == u32::MAX {
                remap[*i as usize] = out_v.len() as u32;
                out_v.push(verts[*i as usize]);
            }
            *i = remap[*i as usize];
        }
    }
    (out_v, out_t, out_m)
}

/// Weld vertices on a 5 cm grid and drop degenerate and duplicate triangles
/// (overlapping 6m bounds repeat the same surfaces many times).
fn weld(verts: &[[f32; 3]], tris: &[[u32; 3]], mats: &[u8]) -> (Vec<[f32; 3]>, Vec<[u32; 3]>, Vec<u8>) {
    use std::collections::{HashMap, HashSet};
    let mut ids: HashMap<[i32; 3], u32> = HashMap::new();
    let mut out_v = Vec::new();
    let remap: Vec<u32> = verts
        .iter()
        .map(|v| {
            let k = [(v[0] * 20.0).round() as i32, (v[1] * 20.0).round() as i32, (v[2] * 20.0).round() as i32];
            *ids.entry(k).or_insert_with(|| {
                out_v.push(*v);
                (out_v.len() - 1) as u32
            })
        })
        .collect();
    let mut seen: HashSet<[u32; 3]> = HashSet::new();
    let mut out_t = Vec::new();
    let mut out_m = Vec::new();
    for (t, m) in tris.iter().zip(mats) {
        let t = [remap[t[0] as usize], remap[t[1] as usize], remap[t[2] as usize]];
        if t[0] == t[1] || t[1] == t[2] || t[0] == t[2] {
            continue;
        }
        let mut k = t;
        k.sort_unstable();
        if seen.insert(k) {
            out_t.push(t);
            out_m.push(*m);
        }
    }
    (out_v, out_t, out_m)
}
