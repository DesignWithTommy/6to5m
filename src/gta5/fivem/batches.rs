//! Instanced batches of the 6m map data (`f14a15be`, GTA V
//! `fwInstancedMapData`): prop batches and grass batches.
//!
//! - Prop batches (`ba049770`: archetype +0x44, LOD distance +0x60,
//!   instances `ea6b3a96` at +0x90, box +0xA0) are streamed and culled as a
//!   unit, so each becomes one merged drawable `b6m_<hash>` (every instance
//!   baked in, 3x4 matrix applied) placed once at the box centre with the
//!   batch LOD distance. The per-instance tint (u32 RGBA at +0x34, rounded
//!   to steps of 32 as palette tints are) multiplies the base colour
//!   texture: tinted instances use tinted texture copies.
//! - Grass batches (`7c475b5d` / `ed86f522`) become GTA V
//!   `GrassInstanceList`s in `t6m_grass_<x>_<y>.ymap` (one per 512 m cell).
//!   Their 16-byte instances have the GTA V layout, re-quantised to the
//!   batch box. Grass archetypes are converted with the buildings models.
//!
//! The batch tables come from sixng `export_batches`, cached per 512 m cell
//! in `<work>/fivem_batches/<input key>/c_<x>_<y>/`; a build exports only
//! the cells its bbox touches. Finding which map files hold batches is one
//! pass over the map files per input (`maps_<key>.json`).

use super::Ctx;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::PathBuf;

/// Bump when the batch tables or their use change.
pub const BATCH_V: &str = "batch4";
const CELL: f32 = 512.0;

type Bbox = [[f32; 3]; 2];

fn key(ctx: &Ctx) -> String {
    super::sha(&format!("{BATCH_V}|{}", ctx.input_stamp))[..16].to_string()
}

/// Cache tag of one archive: "" for levels.rpf (the layout every earlier
/// build wrote), else "<index stem>_<size>" so a changed pack re-exports.
fn tag(a: &sixng_core::game::ArchiveInfo) -> String {
    let st = sixng_core::game::index_stem(&a.id);
    if st == "levels" { String::new() } else { format!("{st}_{}", a.size) }
}

/// Map files holding batches, with the union box of their batches.
fn map_index(ctx: &Ctx, a: &sixng_core::game::ArchiveInfo, index: &sixng_core::scan::ArchiveIndex) -> Result<Vec<(String, Bbox)>, String> {
    use rayon::prelude::*;
    let t = tag(a);
    let p = ctx.o.work.join("fivem_batches").join(if t.is_empty() { format!("maps_{}.json", key(ctx)) } else { format!("maps_{}_{t}.json", key(ctx)) });
    if let Some(v) = sixng_core::vfs::read(&p).ok().and_then(|b| serde_json::from_slice::<Vec<(String, Bbox)>>(&b).ok()) {
        eprintln!("[t6m:batches] map-file index: cached ({} map files with batches)", v.len());
        return Ok(v);
    }
    eprintln!("[t6m:batches] indexing map files with instanced batches (once per input; system pages from one archive map)");
    let t = std::time::Instant::now();
    let file = fs::File::open(&a.path).map_err(|e| e.to_string())?;
    // SAFETY: read-only map of the game archive.
    let map = unsafe { memmap2::Mmap::map(&file) }.map_err(|e| e.to_string())?;
    let v: Vec<(String, Bbox)> = index
        .entries
        .par_iter()
        .filter(|e| e.sub == "7a")
        .filter_map(|e| {
            let r = super::buildings::sys_from_map(&map, e)?;
            let m = sixng_core::meta7a::Meta::parse(&r)?;
            // batch boxes sit in the system page (prop +0xA0, grass +0x80);
            // only the instance arrays need the graphics page
            let v3 = |o: usize| [r.f32(o), r.f32(o + 4), r.f32(o + 8)];
            let bbs: Vec<Bbox> = m.objects(sixng_core::batches::PROP_BATCH).into_iter().map(|o| [v3(o + 0xA0), v3(o + 0xB0)])
                .chain(m.objects(sixng_core::batches::GRASS_BATCH).into_iter().map(|o| [v3(o + 0x80), v3(o + 0x90)])).collect();
            let lo = std::array::from_fn(|k| bbs.iter().map(|b| b[0][k]).fold(f32::MAX, f32::min));
            let hi = std::array::from_fn(|k| bbs.iter().map(|b| b[1][k]).fold(f32::MIN, f32::max));
            (!bbs.is_empty()).then(|| (e.id.clone(), [lo, hi]))
        })
        .collect();
    eprintln!("[t6m:batches] indexed {} map files with batches in {:.1}s", v.len(), t.elapsed().as_secs_f32());
    let _ = sixng_core::vfs::create_dir_all(p.parent().unwrap());
    sixng_core::vfs::write(&p, serde_json::to_vec(&v).unwrap_or_default()).map_err(|e| e.to_string())?;
    Ok(v)
}

/// Batch table dirs (batches.jsonl / batch_props.bin / batch_grass.bin) of
/// the cells touching the region; each cell is exported once per input from
/// the map files centred in it.
fn tables(ctx: &Ctx, index: &sixng_core::scan::ArchiveIndex) -> Result<Vec<PathBuf>, String> {
    // opt-in (--only ...,batches) until the map-file index is bbox-scoped
    if !ctx.o.only.iter().any(|p| p == "batches") {
        return Ok(Vec::new());
    }
    let ws = sixng_core::export::Workspace::new(&ctx.o.game, &ctx.o.work);
    let a = ws.resolve("levels.rpf").map_err(|e| e.to_string())?;
    let mut out = tables_of(ctx, &ws, &a, index)?;
    // story / minigame packs: same tables from their own map files
    for pk in &ctx.packs {
        let a = ws.resolve(&pk.id).map_err(|e| e.to_string())?;
        out.extend(tables_of(ctx, &ws, &a, &pk.index)?);
    }
    Ok(out)
}

/// Batch table dirs of one archive's map files (see `tables`).
fn tables_of(ctx: &Ctx, ws: &sixng_core::export::Workspace, a: &sixng_core::game::ArchiveInfo, index: &sixng_core::scan::ArchiveIndex) -> Result<Vec<PathBuf>, String> {
    let a = a.clone();
    let maps = map_index(ctx, &a, index)?;
    let mut cells: BTreeMap<(i32, i32), Vec<&str>> = BTreeMap::new();
    for (id, b) in &maps {
        cells.entry((((b[0][0] + b[1][0]) * 0.5 / CELL).floor() as i32, ((b[0][1] + b[1][1]) * 0.5 / CELL).floor() as i32)).or_default().push(id);
    }
    let root = ctx.o.work.join("fivem_batches").join(key(ctx));
    let root = match tag(&a) { t if t.is_empty() => root, t => root.join(t) };
    let mut out = Vec::new();
    let mut todo: Vec<((i32, i32), PathBuf)> = Vec::new();
    let mut cell_of: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for ((x, y), ids) in cells {
        // a cell's map files reach at most about one cell beyond it
        let lo = [(x - 1) as f32 * CELL, (y - 1) as f32 * CELL, -1e4];
        let hi = [(x + 2) as f32 * CELL, (y + 2) as f32 * CELL, 1e4];
        if !ctx.o.in_bbox(lo, hi) {
            continue;
        }
        let dir = root.join(format!("c_{x}_{y}")).join("placements").join("levels");
        if !sixng_core::vfs::exists(&dir.join("done")) {
            for id in ids {
                cell_of.insert(id, todo.len());
            }
            todo.push(((x, y), dir.clone()));
        }
        out.push(dir);
    }
    eprintln!("[t6m:batches] cells in region: {} ({} cached, {} to export)", out.len(), out.len() - todo.len(), todo.len());
    if !todo.is_empty() {
        // One export for every missing cell: export_batches loads the model
        // name table of all archives on each call, so a call per cell spent
        // most of the cold build there. Rows are then split per cell.
        let t = std::time::Instant::now();
        let tmp = root.join("_export");
        let sel: Vec<&sixng_core::scan::Entry> = index.entries.iter().filter(|e| cell_of.contains_key(e.id.as_str())).collect();
        sixng_core::export::export_batches(ws, &a, &sel, &tmp, sixng_core::export::no_progress()).map_err(|e| e.to_string())?;
        let src = tmp.join("placements").join(a.id.trim_end_matches(".rpf"));
        let pbin = sixng_core::vfs::read(src.join("batch_props.bin")).unwrap_or_default();
        let gbin = sixng_core::vfs::read(src.join("batch_grass.bin")).unwrap_or_default();
        let mut parts: Vec<(String, Vec<u8>, Vec<u8>, usize, usize)> = vec![(String::new(), Vec::new(), Vec::new(), 0, 0); todo.len()];
        for l in sixng_core::vfs::read_to_string(src.join("batches.jsonl")).unwrap_or_default().lines() {
            let Ok(mut v) = serde_json::from_str::<Value>(l) else { continue };
            let Some(&ci) = v["map"].as_str().and_then(|m| cell_of.get(m)) else { continue };
            let (first, count) = (v["first"].as_u64().unwrap_or(0) as usize, v["count"].as_u64().unwrap_or(0) as usize);
            let c = &mut parts[ci];
            if v["kind"] == "prop" {
                c.1.extend_from_slice(pbin.get(first * 44..(first + count) * 44).unwrap_or_default());
                v["first"] = c.3.into();
                c.3 += count;
            } else {
                c.2.extend_from_slice(gbin.get(first * 20..(first + count) * 20).unwrap_or_default());
                v["first"] = c.4.into();
                c.4 += count;
            }
            c.0.push_str(&v.to_string());
            c.0.push('\n');
        }
        for ((_, dir), (j, pb, gb, _, _)) in todo.iter().zip(parts) {
            sixng_core::vfs::create_dir_all(dir).map_err(|e| e.to_string())?;
            sixng_core::vfs::write(dir.join("batches.jsonl"), j).map_err(|e| e.to_string())?;
            sixng_core::vfs::write(dir.join("batch_props.bin"), pb).map_err(|e| e.to_string())?;
            sixng_core::vfs::write(dir.join("batch_grass.bin"), gb).map_err(|e| e.to_string())?;
            sixng_core::vfs::write(dir.join("done"), b"").map_err(|e| e.to_string())?;
        }
        let _ = sixng_core::vfs::remove_dir_all(&tmp);
        eprintln!("[t6m:batches] exported {} cells ({} map files) in {:.1}s", todo.len(), sel.len(), t.elapsed().as_secs_f32());
    }
    Ok(out)
}

fn f3(v: &Value) -> [f32; 3] {
    std::array::from_fn(|i| v[i].as_f64().unwrap_or(0.0) as f32)
}

fn rows(dir: &std::path::Path, kind: &str) -> Vec<Value> {
    sixng_core::vfs::read_to_string(dir.join("batches.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v["kind"] == kind)
        .collect()
}

fn map_bin(dir: &std::path::Path, f: &str) -> Option<std::sync::Arc<Vec<u8>>> {
    sixng_core::vfs::read_arc(dir.join(f)).ok()
}

/// One prop batch baked into a single drawable (`b6m_<hash>`): GTA VI
/// streams and culls a batch as a unit (one LOD distance at +0x60, one box
/// at +0xA0), so it becomes one GTA V entity at the batch centre.
pub struct BatchDef {
    pub model: String,
    /// per instance: position relative to the centre, world rotation, scale, RGB tint
    pub inst: Vec<([f32; 3], [f32; 4], [f32; 3], [u8; 3])>,
}

type Defs = std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<BatchDef>>>;
static DEFS: std::sync::OnceLock<Defs> = std::sync::OnceLock::new();

pub fn def(name: &str) -> Option<std::sync::Arc<BatchDef>> {
    DEFS.get()?.lock().unwrap().get(name).cloned()
}

/// Prop batches in the region, as (placement JSON, centre, stored rotation).
pub fn props(ctx: &Ctx, index: &sixng_core::scan::ArchiveIndex) -> Vec<(Value, [f32; 3], [f32; 4])> {
    let dirs = match tables(ctx, index) {
        Ok(d) => d,
        Err(e) => {
            ctx.skip("instanced batches", "batch scan failed", json!({"error": e}));
            return Vec::new();
        }
    };
    let defs = DEFS.get_or_init(Default::default);
    let mut out = Vec::new();
    for dir in &dirs {
        let Some(bin) = map_bin(dir, "batch_props.bin") else { continue };
        for b in rows(dir, "prop") {
            let (lo, hi) = (f3(&b["bbox"][0]), f3(&b["bbox"][1]));
            if !ctx.o.in_bbox(lo, hi) {
                continue;
            }
            let off = b["model_id"].as_str().and_then(|m| ctx.voff_of(m));
            let (Some(model), Some(_)) = (b["name"].as_str(), off) else {
                // these batches are LOD parents of HD props (lod_parents kind "batch")
                let why = if b["name"].is_null() { "batch archetype has no name" } else { "batch model drawable not found" };
                super::regionlog::bump("batch_skip", why, b["name"].as_str().or(b["archetype"].as_str()).unwrap_or(""));
                continue;
            };
            let c: [f32; 3] = std::array::from_fn(|k| (lo[k] + hi[k]) * 0.5);
            let (first, count) = (b["first"].as_u64().unwrap_or(0) as usize, b["count"].as_u64().unwrap_or(0) as usize);
            let mut inst = Vec::with_capacity(count);
            for i in first..first + count {
                let Some(r) = bin.get(i * 44..i * 44 + 44) else { break };
                let f = |k: usize| f32::from_le_bytes([r[4 * k], r[4 * k + 1], r[4 * k + 2], r[4 * k + 3]]);
                let t = [r[40], r[41], r[42]].map(|v| ((v as u32 + 16) / 32 * 32).min(255) as u8);
                inst.push(([f(0) - c[0], f(1) - c[1], f(2) - c[2]], [f(3), f(4), f(5), f(6)], [f(7), f(8), f(9)], t));
            }
            if inst.is_empty() {
                super::regionlog::bump("batch_skip", "no decodable instances", model);
                continue;
            }
            let name = format!("b6m_{}", &super::sha(&format!("{}|{}|{:?}", b["map"], model, b["bbox"]))[..12]);
            defs.lock().unwrap().insert(name.clone(), std::sync::Arc::new(BatchDef { model: model.to_string(), inst }));
            let e = json!({"name": name, "model_id": b["model_id"], "lod": {"level": "HD", "lod_dist": b["lod_distance"]}, "scale_xy": 1.0, "scale_z": 1.0, "batch_map": b["map"], "batch_model": model});
            out.push((e, c, [0.0, 0.0, 0.0, 1.0]));
        }
    }
    eprintln!("[t6m:batches] {} prop batches in region (one entity each)", out.len());
    if let Some(d) = DEFS.get() {
        let d = d.lock().unwrap();
        let mut per: Vec<(&String, usize, &str)> = out.iter().filter_map(|(e, _, _)| e["name"].as_str()).filter_map(|n| d.get_key_value(n)).map(|(n, b)| (n, b.inst.len(), b.model.as_str())).collect();
        let total: usize = per.iter().map(|x| x.1).sum();
        per.sort_by(|a, b| b.1.cmp(&a.1));
        let ex: Vec<String> = per.iter().take(5).map(|(n, c, m)| format!("{n} ({m}) x{c}")).collect();
        eprintln!("[t6m:batches] {total} instances placed in {} batches (avg {:.1}); largest: {}", per.len(), total as f32 / per.len().max(1) as f32, ex.join(", "));
    }
    out
}

/// Merge `mesh` (the batch's model) once per instance; colour textures of
/// tinted instances become tinted copies (`tint(tex, rgb)`).
pub fn bake(d: &BatchDef, mesh: &crate::gta5::ydr::Mesh, tint: &dyn Fn(&str, [u8; 3]) -> String) -> (crate::gta5::ydr::Mesh, Vec<String>) {
    let cr = |a: [f32; 3], b: [f32; 3]| [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
    let rot = |q: [f32; 4], v: [f32; 3]| {
        let u = [q[0], q[1], q[2]];
        let t = cr(u, v).map(|x| 2.0 * x);
        let ut = cr(u, t);
        [v[0] + q[3] * t[0] + ut[0], v[1] + q[3] * t[1] + ut[1], v[2] + q[3] * t[2] + ut[2]]
    };
    let mut geoms: Vec<crate::gta5::ydr::Geom> = Vec::new();
    for (p, q, s, t) in &d.inst {
        for g in &mesh.geoms {
            let tex = if *t == [255; 3] { g.texture.clone() } else { tint(&g.texture, *t) };
            let n0 = g.positions.len();
            let slot = geoms.iter().rposition(|x| x.texture == tex && x.kind == g.kind && x.normal == g.normal && x.layers == g.layers && x.positions.len() + n0 <= 0xFFFF);
            let gi = match slot {
                Some(i) => i,
                None => {
                    let mut e = g.clone();
                    e.texture = tex;
                    e.positions.clear();
                    e.normals.clear();
                    e.colours.clear();
                    e.uvs.clear();
                    e.indices.clear();
                    geoms.push(e);
                    geoms.len() - 1
                }
            };
            let o = &mut geoms[gi];
            let base = o.positions.len() as u16;
            o.positions.extend(g.positions.iter().map(|v| {
                let w = rot(*q, [v[0] * s[0], v[1] * s[1], v[2] * s[2]]);
                [w[0] + p[0], w[1] + p[1], w[2] + p[2]]
            }));
            o.normals.extend(g.normals.iter().map(|n| {
                let w = rot(*q, [n[0] / s[0].max(1e-6), n[1] / s[1].max(1e-6), n[2] / s[2].max(1e-6)]);
                let l = (w[0] * w[0] + w[1] * w[1] + w[2] * w[2]).sqrt().max(1e-9);
                w.map(|x| x / l)
            }));
            o.colours.extend_from_slice(&g.colours);
            o.uvs.extend_from_slice(&g.uvs);
            o.indices.extend(g.indices.iter().map(|i| i + base));
        }
    }
    let mut texs: Vec<String> = geoms.iter().map(|g| g.texture.clone()).collect();
    texs.sort();
    texs.dedup();
    (crate::gta5::ydr::Mesh { name: mesh.name.clone(), geoms }, texs)
}

/// Grass batch archetypes in the region: (name, levels.rpf model offset).
pub fn grass_archetypes(ctx: &Ctx, index: &sixng_core::scan::ArchiveIndex) -> Vec<(String, usize)> {
    let Ok(dirs) = tables(ctx, index) else { return Vec::new() };
    let mut seen = BTreeMap::new();
    for b in dirs.iter().flat_map(|d| rows(d, "grass")) {
        if !ctx.o.in_bbox(f3(&b["bbox"][0]), f3(&b["bbox"][1])) {
            continue;
        }
        let off = b["model_id"].as_str().and_then(|m| ctx.voff_of(m));
        if let (Some(n), Some(off)) = (b["name"].as_str(), off) {
            seen.entry(n.to_string()).or_insert(off);
        }
    }
    seen.into_iter().collect()
}

/// Write the grass ymaps for the region; `converted` = converted archetype
/// (safe) names. Returns the ymap file names.
pub fn grass(ctx: &Ctx, index: &sixng_core::scan::ArchiveIndex, converted: &HashSet<String>) -> Result<Vec<String>, String> {
    let mut cells: BTreeMap<(i32, i32), Vec<crate::gta5::meta::GrassList>> = BTreeMap::new();
    let mut missing = 0usize;
    for dir in tables(ctx, index)? {
        let Some(bin) = map_bin(&dir, "batch_grass.bin") else { continue };
        for b in rows(&dir, "grass") {
            let (lo, hi) = (f3(&b["bbox"][0]), f3(&b["bbox"][1]));
            if !ctx.o.in_bbox(lo, hi) {
                continue;
            }
            let Some(name) = b["name"].as_str().map(super::safe_name).filter(|n| converted.contains(n)) else {
                missing += 1;
                continue;
            };
            let (first, count) = (b["first"].as_u64().unwrap_or(0) as usize, b["count"].as_u64().unwrap_or(0) as usize);
            let span: [f32; 3] = std::array::from_fn(|k| (hi[k] - lo[k]).max(1e-6));
            let mut inst = Vec::with_capacity(count);
            for i in first..first + count {
                let Some(r) = bin.get(i * 20..i * 20 + 20) else { break };
                let f = |k: usize| f32::from_le_bytes([r[4 * k], r[4 * k + 1], r[4 * k + 2], r[4 * k + 3]]);
                let mut d = [0u8; 16];
                for k in 0..3 {
                    let q = (((f(k) - lo[k]) / span[k]).clamp(0.0, 1.0) * 65535.0).round() as u16;
                    d[2 * k..2 * k + 2].copy_from_slice(&q.to_le_bytes());
                }
                // normal x / y, colour rgb, scale, ao (same order as GTA V)
                d[6..13].copy_from_slice(&r[12..19]);
                inst.push(d);
            }
            if inst.is_empty() {
                continue;
            }
            let lod = b["lod_distance"].as_f64().unwrap_or(120.0).clamp(20.0, 400.0) as f32;
            let c = [(lo[0] + hi[0]) * 0.5, (lo[1] + hi[1]) * 0.5];
            cells.entry(((c[0] / CELL).floor() as i32, (c[1] / CELL).floor() as i32)).or_default().push(crate::gta5::meta::GrassList {
                aabb: [lo, hi],
                scale_range: f3(&b["scale_range"]),
                archetype: name,
                lod_dist: lod as u32,
                fade_start: lod * 0.75,
                fade_range: lod * 0.15,
                orient: 1.0,
                instances: inst,
            });
        }
    }
    if missing > 0 {
        ctx.skip(&format!("({missing} grass batches)"), "grass archetype not converted", json!({"count": missing}));
    }
    let mut files = Vec::new();
    for ((x, y), lists) in cells {
        let name = format!("t6m_grass_{x}_{y}");
        let b = crate::gta5::meta::build_grass_ymap(&name, &lists)?;
        files.push(super::buildings::emit_meta(ctx, &format!("{name}.ymap"), b)?);
    }
    eprintln!("[t6m:batches] {} grass ymaps", files.len());
    Ok(files)
}
