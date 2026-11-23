//! Static collision: 6m physics bounds (sixng bounds decoder, already in
//! world space) merged into one t6m_cc_XXX_YYY.ybn per 128 m cell
//! (XXX = cell + 200). Triangles are welded on a 5 cm grid; boxes, spheres,
//! capsules and cylinders stay native ybn primitives (6m bounds use the GTA V
//! polygon types; tessellating them was two thirds of all triangles).
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
    let c = sixng_core::bounds::decode_native(&r, !full)?;
    (!c.by_mat.values().all(|x| x.1.is_empty()) || !c.native.is_empty()).then_some(c)
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
    // live-test log: [t6m:collision] lines + <out>/t6m_report/collision.json
    let log = std::sync::Mutex::new(ColLog::default());
    let res: Vec<Result<(), String>> = cells
        .par_iter()
        .map(|((cx, cy), es)| {
            let file = format!("t6m_cc_{:03}_{:03}.ybn", cx + 200, cy + 200);
            let fp = sha(&format!("{YBN_V}|col|{mats_fp}|{}|{}{}", ctx.o.weapon_collision, es.iter().map(|e| e.id.as_str()).collect::<Vec<_>>().join(","), ctx.o.offset_fp()));
            if let Some(info) = ctx.up_to_date(&file, &fp) {
                log.lock().unwrap().cells_cached += 1;
                ctx.emit(&file, &fp, info, None)?;
                return Ok(());
            }
            let (mut verts, mut tris, mut tm): (Vec<[f32; 3]>, Vec<[u32; 3]>, Vec<u8>) = (Vec::new(), Vec::new(), Vec::new());
            let mut prims: Vec<sixng_core::bounds::NativePrim> = Vec::new();
            for e in es {
                let Some(c) = decode(&rpf, e, ctx.o.weapon_collision) else { continue };
                {
                    let mut l = log.lock().unwrap();
                    for (id, (_, idx)) in &c.by_mat {
                        l.surface(*id, mats.get(id).copied(), idx.len() / 3, &e.id);
                    }
                    for p in &c.native {
                        use sixng_core::bounds::NativePrim as P;
                        let (k, mat) = match p {
                            P::Box { mat, .. } => (0, *mat),
                            P::Sphere { mat, .. } => (1, *mat),
                            P::Capsule { mat, .. } => (2, *mat),
                            P::Cylinder { mat, .. } => (3, *mat),
                        };
                        l.prims[k] += 1;
                        l.surface(mat, mats.get(&mat).copied(), 1, &e.id);
                    }
                }
                for (id, (v, idx)) in &c.by_mat {
                    let base = verts.len() as u32;
                    verts.extend_from_slice(v);
                    tris.extend(idx.chunks_exact(3).map(|t| [base + t[0], base + t[1], base + t[2]]));
                    tm.resize(tris.len(), mats.get(id).copied().unwrap_or(0));
                }
                prims.extend(c.native);
            }
            if tris.is_empty() && prims.is_empty() {
                return Ok(());
            }
            let raw = tris.len();
            let (mut verts, tris, tm) = weld(&verts, &tris, &tm);
            let mut polys: Vec<ybn::Poly> = tris.iter().map(|&t| ybn::Poly::Tri(t)).collect();
            let mut words: Vec<u32> = tm.iter().map(|&m| m as u32).collect();
            let n_tris = polys.len();
            for p in &prims {
                use sixng_core::bounds::NativePrim as P;
                let mut at = |v: [f32; 3]| {
                    verts.push(v);
                    (verts.len() - 1) as u32
                };
                let (poly, mat) = match *p {
                    P::Box { mat, corners } => (ybn::Poly::Box(corners.map(&mut at)), mat),
                    P::Sphere { mat, centre, radius } => (ybn::Poly::Sphere(at(centre), radius), mat),
                    P::Capsule { mat, a, b, radius } => (ybn::Poly::Capsule(at(a), at(b), radius), mat),
                    P::Cylinder { mat, a, b, radius } => (ybn::Poly::Cylinder(at(a), at(b), radius), mat),
                };
                polys.push(poly);
                words.push(mats.get(&mat).copied().unwrap_or(0) as u32);
            }
            let verts: Vec<[f32; 3]> = verts.iter().map(|v| ctx.o.shift(*v)).collect();
            let bytes = ybn::build_ybn_polys(&verts, &polys, &words).map_err(|e| format!("{file}: {e}"))?;
            let info = json!({"bounds": es.len(), "triangles": n_tris, "primitives": prims.len(), "raw_triangles": raw});
            {
                let mut l = log.lock().unwrap();
                l.cells_built += 1;
                l.bounds += es.len();
                l.tris_raw += raw;
                l.tris += n_tris;
                l.bytes += bytes.len();
                if bytes.len() > l.largest.1 {
                    l.largest = (file.clone(), bytes.len());
                }
            }
            let s = ctx.emit(&file, &fp, info, Some(bytes))?;
            ctx.report.lock().unwrap().converted.push(json!({"kind": "collision", "ybn": s, "bounds": es.len(), "triangles": n_tris, "primitives": prims.len()}));
            Ok(())
        })
        .collect();
    for r in res {
        r?;
    }
    log.into_inner().unwrap().write(ctx, mats.is_empty());
    Ok(())
}

/// Counters for the live-test log (cells rebuilt this run only; cached
/// cells are counted but not decoded).
#[derive(Default)]
struct ColLog {
    cells_built: usize,
    cells_cached: usize,
    bounds: usize,
    tris_raw: usize,
    tris: usize,
    bytes: usize,
    largest: (String, usize),
    /// box, sphere, capsule, cylinder
    prims: [usize; 4],
    /// surface id -> (GTA V material if mapped, polygons, example bounds id)
    surfaces: BTreeMap<u16, (Option<u8>, usize, String)>,
}

impl ColLog {
    fn surface(&mut self, id: u16, mat: Option<u8>, n: usize, example: &str) {
        let e = self.surfaces.entry(id).or_insert_with(|| (mat, 0, example.to_string()));
        e.1 += n;
    }

    fn write(self, ctx: &Ctx, no_map: bool) {
        let (hit, miss): (Vec<_>, Vec<_>) = self.surfaces.iter().partition(|(_, v)| v.0.is_some());
        let polys = |v: &[(&u16, &(Option<u8>, usize, String))]| v.iter().map(|x| x.1 .1).sum::<usize>();
        let mut miss_top = miss.clone();
        miss_top.sort_by(|a, b| b.1 .1.cmp(&a.1 .1));
        let t = "[t6m:collision]";
        println!("{t} cells built {} cached {}, bounds {}, triangles {} (raw {}), {:.1} MB, largest {} {:.2} MB", self.cells_built, self.cells_cached, self.bounds, self.tris, self.tris_raw, self.bytes as f64 / 1e6, self.largest.0, self.largest.1 as f64 / 1e6);
        println!("{t} primitives: box {} sphere {} capsule {} cylinder {}", self.prims[0], self.prims[1], self.prims[2], self.prims[3]);
        println!("{t} surfaces: {} ids, mapped {} ({} polys), unmapped {} ({} polys){}", self.surfaces.len(), hit.len(), polys(&hit), miss.len(), polys(&miss), if no_map { " - no material map, all DEFAULT" } else { "" });
        for (id, (_, n, ex)) in miss_top.iter().take(8) {
            println!("{t}   unmapped {id:04x}: {n} polys, e.g. {ex}");
        }
        let doc = json!({
            "cells_built": self.cells_built, "cells_cached": self.cells_cached, "bounds": self.bounds,
            "triangles": self.tris, "triangles_raw": self.tris_raw, "bytes": self.bytes,
            "largest": {"file": self.largest.0, "bytes": self.largest.1},
            "primitives": {"box": self.prims[0], "sphere": self.prims[1], "capsule": self.prims[2], "cylinder": self.prims[3]},
            "material_map_loaded": !no_map,
            "surfaces_mapped": hit.iter().map(|(id, v)| json!({"id": format!("{id:04x}"), "material": v.0, "polys": v.1, "example": v.2})).collect::<Vec<_>>(),
            "surfaces_unmapped": miss_top.iter().map(|(id, v)| json!({"id": format!("{id:04x}"), "polys": v.1, "example": v.2})).collect::<Vec<_>>(),
        });
        let dir = ctx.o.out.join("t6m_report");
        let _ = fs::create_dir_all(&dir).and_then(|_| fs::write(dir.join("collision.json"), serde_json::to_vec_pretty(&doc).unwrap_or_default()));
    }
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
