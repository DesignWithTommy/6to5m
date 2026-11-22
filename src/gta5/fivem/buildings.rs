//! Buildings/props (buildings_port.py): sixng world placements -> one ydr
//! per archetype (shared texture dictionaries), t6m_bld.ytyp (bounds from
//! the mesh, textureDictionary set) and 512 m t6m_bld_XX_YY.ymap cells.

use super::geom::{aabb, push_geoms, V3};
use super::{safe_name, sha, write_atomic, Ctx, TOOL};
use crate::gta5::{
    meta::{self, Archetype, Entity, MapData, MapTypes},
    ydr::{self, Mesh},
    ytd::{self, Tex, TexFormat},
};
use memmap2::Mmap;
use rayon::prelude::*;
use serde_json::{json, Value};
use sixng_core::scan::{ArchiveIndex, Entry};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    error::Error,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

const TXD_CAP: u64 = 48 << 20;
const MISSING: &str = "r6m_missing";

/// 6m render bucket bits -> GTA V shader kind (ydr::Geom::kind).
/// DXT1 (opaque) or DXT5 (with alpha) DDS with a box-filtered mip chain,
/// compressed in pure Rust (texpresso).
fn rgba_dds(rgba: &[u8], w: u32, h: u32) -> Vec<u8> {
    let mut levels = vec![(rgba.to_vec(), w, h)];
    while levels.len() < 12 {
        let (src, sw, sh) = levels.last().unwrap();
        let (sw, sh) = (*sw, *sh);
        if sw < 8 || sh < 8 {
            break;
        }
        let (nw, nh) = (sw / 2, sh / 2);
        let mut d = vec![0u8; (nw * nh * 4) as usize];
        for y in 0..nh {
            for x in 0..nw {
                for c in 0..4 {
                    let at = |xx: u32, yy: u32| src[((yy * sw + xx) * 4 + c) as usize] as u32;
                    d[((y * nw + x) * 4 + c) as usize] = ((at(2 * x, 2 * y) + at(2 * x + 1, 2 * y) + at(2 * x, 2 * y + 1) + at(2 * x + 1, 2 * y + 1) + 2) / 4) as u8;
                }
            }
        }
        levels.push((d, nw, nh));
    }
    let opaque = rgba.chunks_exact(4).all(|p| p[3] == 255);
    let (fmt, cc) = if opaque { (texpresso::Format::Bc1, *b"DXT1") } else { (texpresso::Format::Bc3, *b"DXT5") };
    let u = |v: u32, b: &mut Vec<u8>| b.extend_from_slice(&v.to_le_bytes());
    let mut b = Vec::new();
    b.extend_from_slice(b"DDS ");
    u(124, &mut b);
    u(0x1 | 0x2 | 0x4 | 0x1000 | 0x20000 | 0x80000, &mut b);
    u(h, &mut b);
    u(w, &mut b);
    u(fmt.compressed_size(w as usize, h as usize) as u32, &mut b);
    u(0, &mut b);
    u(levels.len() as u32, &mut b);
    for _ in 0..11 {
        u(0, &mut b);
    }
    u(32, &mut b);
    u(0x4, &mut b);
    b.extend_from_slice(&cc);
    for _ in 0..5 {
        u(0, &mut b);
    }
    u(0x1000 | 0x400000 | 0x8, &mut b);
    for _ in 0..4 {
        u(0, &mut b);
    }
    for (px, lw, lh) in &levels {
        let mut out = vec![0u8; fmt.compressed_size(*lw as usize, *lh as usize)];
        fmt.compress(px, *lw as usize, *lh as usize, texpresso::Params::default(), &mut out);
        b.extend_from_slice(&out);
    }
    b
}

/// Rejects decodes that cannot be a colour map: flat images (BCPack data
/// sixng cannot decode yet comes out as one solid colour, seen in game as
/// solid blue/magenta props) and tangent-space normal maps.
fn bad_colour(rgba: &[u8]) -> Option<&'static str> {
    let n = (rgba.len() / 4).max(1) as f64;
    let (mut s, mut q) = ([0f64; 3], [0f64; 3]);
    for px in rgba.chunks_exact(4) {
        for k in 0..3 {
            let v = px[k] as f64;
            s[k] += v;
            q[k] += v * v;
        }
    }
    let m = s.map(|x| x / n);
    let var: f64 = (0..3).map(|k| q[k] / n - m[k] * m[k]).sum();
    if var < 12.0 {
        return Some("flat decode (undecoded BCPack data)");
    }
    if m[2] > 200.0 && (100.0..160.0).contains(&m[0]) && (100.0..160.0).contains(&m[1]) {
        return Some("normal map, not a colour map");
    }
    // Packed masks (blend weights, noise) have independent channels; colour
    // maps are luminance-dominated, with R/G/B strongly correlated.
    let corr = |a: usize, b: usize| {
        let (mut sab, mut saa, mut sbb) = (0f64, 0f64, 0f64);
        for px in rgba.chunks_exact(4) {
            let (x, y) = (px[a] as f64 - m[a], px[b] as f64 - m[b]);
            sab += x * y;
            saa += x * x;
            sbb += y * y;
        }
        if saa < 1.0 || sbb < 1.0 { 1.0 } else { sab / (saa * sbb).sqrt() }
    };
    if corr(0, 1).min(corr(1, 2)).min(corr(0, 2)) < 0.2 {
        return Some("mask/packed data, not a colour map");
    }
    None
}

/// 6m paints ground tiles as stacked decal layers blended by alpha masks.
/// GTA needs an opaque surface underneath: the decal texture covering the
/// most area becomes opaque (when it covers at least half the footprint and
/// the mesh has no opaque ground already); every other layer stays an
/// alpha-blended decal.
fn promote_base_layer(mesh: &mut Mesh) {
    let area = |g: &ydr::Geom| -> f64 {
        g.indices
            .chunks_exact(3)
            .map(|t| {
                let (a, b, c) = (g.positions[t[0] as usize], g.positions[t[1] as usize], g.positions[t[2] as usize]);
                let (u, v) = ([b[0] - a[0], b[1] - a[1]], [c[0] - a[0], c[1] - a[1]]);
                ((u[0] * v[1] - u[1] * v[0]) as f64).abs() / 2.0
            })
            .sum()
    };
    let mut by_tex: HashMap<String, f64> = HashMap::new();
    let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
    for g in &mesh.geoms {
        for p in &g.positions {
            for k in 0..2 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        if g.kind == 2 && g.texture != MISSING {
            *by_tex.entry(g.texture.clone()).or_default() += area(g);
        }
    }
    let foot = ((hi[0] - lo[0]) * (hi[1] - lo[1])).max(0.0) as f64;
    let opaque: f64 = mesh.geoms.iter().filter(|g| g.kind == 0).map(area).sum();
    if let Some((t, a)) = by_tex.into_iter().max_by(|a, b| a.1.total_cmp(&b.1)) {
        if a >= foot * 0.5 && opaque < foot * 0.5 {
            for g in mesh.geoms.iter_mut().filter(|g| g.kind == 2 && g.texture == t) {
                g.kind = 0;
            }
        }
    }
}

/// Rejects (and deletes) a cached DDS that is not a usable colour map.
fn check_dds(p: &std::path::Path) -> Result<(), String> {
    let bytes = fs::read(p).map_err(|e| e.to_string())?;
    let (_, _, rgba) = crate::texture::dds_preview(&bytes).map_err(|e| e.to_string())?;
    match bad_colour(&rgba) {
        None => Ok(()),
        Some(why) => {
            let _ = fs::remove_file(p);
            Err(why.into())
        }
    }
}

fn kind_of(bucket: u8) -> u8 {
    match bucket {
        x if x & 0x04 != 0 => 2,
        x if x & 0x42 != 0 => 1,
        x if x & 0x08 != 0 => 3,
        _ => 0,
    }
}

/// Diffuse/colour parameter hashes in priority order (model::COLOR_SLOTS).
const COLOR_SLOTS: [u32; 16] = [
    3370697346, 756347250, 0x346394EA, 0x7D4F12E2, 3464205966, 3632507550, 851042057, 1024521143, 3853173992, 2464948076, 3389132183, 3547013225,
    234123336, 3831897388, 3489704073, 1380592926,
];

struct Place {
    name: String,
    off: usize,
    pos: V3,
    rot: [f32; 4],
    sxy: f32,
    sz: f32,
    /// (map id, entity index) in the source placement file (world files only).
    key: Option<(String, usize)>,
    /// 0 = HD/orphan HD, 1 = LOD, 2 = SLOD1, 3 = SLOD2, ...
    level: u8,
    elod: f32,
    child_lod: f32,
}

fn level_of(s: &str) -> u8 {
    match s {
        "LOD" => 1,
        x if x.starts_with("SLOD") => 1 + x[4..].parse::<u8>().unwrap_or(1),
        _ => 0,
    }
}

const LEVEL_NAMES: [&str; 6] = ["LODTYPES_DEPTH_HD", "LODTYPES_DEPTH_LOD", "LODTYPES_DEPTH_SLOD1", "LODTYPES_DEPTH_SLOD2", "LODTYPES_DEPTH_SLOD3", "LODTYPES_DEPTH_SLOD4"];

fn qmul(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [
        a[3] * b[0] + a[0] * b[3] + a[1] * b[2] - a[2] * b[1],
        a[3] * b[1] - a[0] * b[2] + a[1] * b[3] + a[2] * b[0],
        a[3] * b[2] + a[0] * b[1] - a[1] * b[0] + a[2] * b[3],
        a[3] * b[3] - a[0] * b[0] - a[1] * b[1] - a[2] * b[2],
    ]
}
fn qrot(q: [f32; 4], v: V3) -> V3 {
    let r = qmul(qmul(q, [v[0], v[1], v[2], 0.0]), [-q[0], -q[1], -q[2], q[3]]);
    [r[0], r[1], r[2]]
}
fn q4(v: &Value) -> Option<[f32; 4]> {
    let a = v.as_array()?;
    (a.len() == 4).then(|| std::array::from_fn(|k| a[k].as_f64().unwrap_or(0.0) as f32))
}

/// Keyed textures from the ttdb streaming pools, cached as DDS under
/// <work>/fivem_tex_cache/<hash>.dds.
struct Pools {
    game: PathBuf,
    catalog: Option<crate::texture::Catalog>,
    maps: Mutex<HashMap<PathBuf, Arc<Mmap>>>,
    dir: PathBuf,
}

impl Pools {
    fn path(&self, hash: u32) -> PathBuf {
        self.dir.join(format!("{hash:08x}.dds"))
    }
    /// Cached DDS path for a texture, fetching it from the pools if needed.
    /// Cached DDS for a texture: ttdb pools by asset key, then the TTPF
    /// stores (sixng) by 64-bit hash.
    fn get(&self, hash: u32, key: u64, h64: u64) -> Result<PathBuf, String> {
        // Pool textures can be BCPack-flat or a normal map too: check the
        // decoded image and fall back to the TTPF stores, then give up.
        let e1 = match self.from_pools(hash, key).and_then(|p| check_dds(&p).map(|_| p)) {
            Ok(p) => return Ok(p),
            Err(e) => e,
        };
        self.from_ttpf(hash, h64).and_then(|p| check_dds(&p).map(|_| p)).map_err(|e2| format!("{e1}; ttpf: {e2}"))
    }

    fn from_ttpf(&self, hash: u32, h64: u64) -> Result<PathBuf, String> {
        if h64 == 0 {
            return Err("no 64-bit hash".into());
        }
        let set = sixng_core::ttpf::global(&self.game);
        let (store, t) = set.get(h64).ok_or("not in any ttpf store")?;
        if !t.decodable() {
            return Err(format!("format {} / tile mode {} not decodable", t.dxgi_format, t.tile_mode()));
        }
        let (data, skipped) = store.data_ex(t).map_err(|e| e.to_string())?;
        let mip = if skipped == 0 { 0 } else { store.best_mip(t).ok_or("only BCPack mips")? };
        let (rgba, w, h) = sixng_core::ttpf::decode_rgba(t, &data, mip).map_err(|e| e.to_string())?;
        if let Some(why) = bad_colour(&rgba) {
            return Err(why.into());
        }
        let dds = rgba_dds(&rgba, w, h);
        ytd::from_dds("check", &dds)?;
        let p = self.path(hash);
        write_atomic(&p, &dds).or_else(|e| if p.exists() { Ok(()) } else { Err(e.to_string()) })?;
        Ok(p)
    }

    fn from_pools(&self, hash: u32, key: u64) -> Result<PathBuf, String> {
        let p = self.path(hash);
        if p.exists() {
            return Ok(p);
        }
        if key == 0 {
            return Err("no asset key".into());
        }
        let cat = self.catalog.as_ref().ok_or("no pool catalog")?;
        let loc = cat.find(key).iter().find(|l| l.extractable).ok_or("not in pools")?;
        let map = {
            let mut m = self.maps.lock().unwrap();
            match m.get(&loc.path) {
                Some(x) => x.clone(),
                None => {
                    let f = fs::File::open(&loc.path).map_err(|e| e.to_string())?;
                    // SAFETY: read-only map of a game file.
                    let x = Arc::new(unsafe { Mmap::map(&f) }.map_err(|e| e.to_string())?);
                    m.insert(loc.path.clone(), x.clone());
                    x
                }
            }
        };
        let pool = crate::texture::Pool::open(&map).map_err(|e| e.to_string())?;
        let t = pool.textures.get(loc.index).ok_or("pool index out of range")?;
        let dds = pool.create_dds(t).map_err(|e| e.to_string())?;
        ytd::from_dds("check", &dds)?;
        write_atomic(&p, &dds).or_else(|e| if p.exists() { Ok(()) } else { Err(e.to_string()) })?;
        Ok(p)
    }
}

fn placeholder() -> Tex {
    let mut data = Vec::new();
    let (mut w, mut levels) = (64usize, 0u8);
    while w >= 1 {
        for y in 0..w {
            for x in 0..w {
                let on = ((x * 8 / w.max(8)) + (y * 8 / w.max(8))) % 2 == 0;
                data.extend_from_slice(if on { &[200, 0, 200, 255] } else { &[90, 90, 90, 255] });
            }
        }
        levels += 1;
        w /= 2;
    }
    Tex { name: MISSING.into(), width: 64, height: 64, levels, format: TexFormat::A8R8G8B8, data, unk32: 0, usage_data: 0, extra_flags: 0 }
}

fn f3(v: &Value) -> V3 {
    std::array::from_fn(|k| v[k].as_f64().unwrap_or(0.0) as f32)
}

/// Placement JSON dir: given, else <work>/placements/levels, generated with
/// the sixng library when it has no index.jsonl yet.
fn placements_dir(ctx: &Ctx, index: &ArchiveIndex) -> Result<PathBuf, Box<dyn Error>> {
    let dir = ctx.o.placements.clone().unwrap_or_else(|| ctx.o.work.join("placements").join("levels"));
    if dir.join("index.jsonl").exists() {
        return Ok(dir);
    }
    if ctx.o.placements.is_some() {
        return Err(format!("{}: no index.jsonl", dir.display()).into());
    }
    eprintln!("buildings: no placements in {}, running sixng placements", dir.display());
    let ws = sixng_core::export::Workspace::new(&ctx.o.game, &ctx.o.work);
    let a = ws.resolve("levels.rpf")?;
    if let Some(b) = ctx.o.bbox {
        sixng_core::export::set_region(b);
    }
    let sel: Vec<&sixng_core::scan::Entry> = index.entries.iter().filter(|e| e.sub == "7a").collect();
    sixng_core::export::export_placements(&ws, &a, &sel, &ctx.o.work, sixng_core::export::no_progress())?;
    Ok(dir)
}

fn load_places(ctx: &Ctx, dir: &Path) -> Result<Vec<Place>, Box<dyn Error>> {
    let idx = fs::read_to_string(dir.join("index.jsonl"))?;
    let files: Vec<PathBuf> = idx
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| {
            if v["space"] != "world" {
                return false;
            }
            ctx.o.in_bbox(f3(&v["bbox"][0]), f3(&v["bbox"][1]))
        })
        .filter_map(|v| Some(dir.join(Path::new(v["file"].as_str()?).file_name()?)))
        .collect();
    eprintln!("buildings: {} world placement files in region", files.len());
    let load = |f: &Path| fs::read(f).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    let per: Vec<(String, Vec<Value>)> = files
        .par_iter()
        .filter_map(|f| {
            let v = load(f)?;
            Some((v["id"].as_str().unwrap_or("").to_string(), v["entities"].as_array().cloned().unwrap_or_default()))
        })
        .collect();
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut push = |e: &Value, pos: V3, stored: [f32; 4], key: Option<(String, usize)>, out: &mut Vec<Place>| {
        if !ctx.o.in_bbox(pos, pos) || (pos[0].abs() < 50.0 && pos[1].abs() < 50.0) {
            return;
        }
        let name = e["name"].as_str().map(str::to_string);
        if let (Some(f), Some(n)) = (&ctx.o.name, &name) {
            if !n.to_lowercase().contains(f.as_str()) {
                return;
            }
        } else if ctx.o.name.is_some() {
            return;
        }
        let Some(name) = name else {
            ctx.skip(&format!("hash_{:08x}", e["archetype"].as_u64().unwrap_or(0)), "unnamed hash", json!({"position": pos}));
            return;
        };
        let Some(off) = e["model_id"].as_str().and_then(|m| m.strip_prefix("levels.rpf@")).and_then(|h| usize::from_str_radix(h, 16).ok()) else {
            ctx.skip(&name, "model not in levels.rpf", json!({"model_id": e["model_id"]}));
            return;
        };
        let k = (name.clone(), (pos[0] * 100.0).round() as i64, (pos[1] * 100.0).round() as i64, (pos[2] * 100.0).round() as i64);
        if !seen.insert(k) {
            return;
        }
        let rot = if ctx.o.conjugate { [-stored[0], -stored[1], -stored[2], stored[3]] } else { stored };
        let l = &e["lod"];
        out.push(Place {
            name,
            off,
            pos,
            rot,
            sxy: e["scale_xy"].as_f64().unwrap_or(1.0) as f32,
            sz: e["scale_z"].as_f64().unwrap_or(1.0) as f32,
            key,
            level: level_of(l["level"].as_str().unwrap_or("HD")),
            elod: l["lod_dist"].as_f64().unwrap_or(-1.0) as f32,
            child_lod: l["child_lod_dist"].as_f64().unwrap_or(0.0) as f32,
        });
    };
    for (map, ents) in &per {
        for (i, e) in ents.iter().enumerate() {
            let stored = q4(&e["rotation"]).unwrap_or([0.0, 0.0, 0.0, 1.0]);
            push(e, f3(&e["position"]), stored, Some((map.clone(), i)), &mut out);
        }
    }
    // Interiors (MLO instances): interior-space entities moved into world space.
    let (mut n_int, mut n_ent) = (0, 0);
    let int_text = fs::read_to_string(dir.join("interiors.jsonl")).ok();
    if !ctx.o.interiors {
        let n = int_text.iter().flat_map(|t| t.lines()).filter_map(|l| serde_json::from_str::<Value>(l).ok()).filter(|v| {
            let p = f3(&v["position"]);
            ctx.o.in_bbox(p, p)
        }).count();
        if n > 0 {
            ctx.skip(&format!("({n} interiors)"), "interior contents skipped (MLO shell not converted; use --interiors)", json!({"count": n}));
        }
    }
    if let Some(text) = int_text.filter(|_| ctx.o.interiors) {
        let mut cache: HashMap<String, Option<Arc<Value>>> = HashMap::new();
        for l in text.lines() {
            let Ok(inst) = serde_json::from_str::<Value>(l) else { continue };
            let ip = f3(&inst["position"]);
            if !ctx.o.in_bbox(ip, ip) {
                continue;
            }
            let Some(file) = inst["interior_file"].as_str().and_then(|f| f.strip_prefix("levels.rpf@")) else { continue };
            let Some(src) = cache.entry(file.to_string()).or_insert_with(|| load(&dir.join(format!("{file}.json"))).map(Arc::new)).clone() else {
                ctx.skip(&format!("interior {}", inst["archetype"].as_str().unwrap_or("?")), "interior entity file missing", json!({"file": file}));
                continue;
            };
            let empty = Vec::new();
            let ents = src["entities"].as_array().unwrap_or(&empty);
            let ri = q4(&inst["rotation_world"]).unwrap_or([0.0, 0.0, 0.0, 1.0]);
            n_int += 1;
            for ix in inst["entities"].as_array().into_iter().flatten().chain(inst["entity_set_entities"].as_array().into_iter().flatten()) {
                let Some(e) = ix.as_u64().and_then(|i| ents.get(i as usize)) else { continue };
                let lp = f3(&e["position"]);
                let d = qrot(ri, lp);
                let pos = [ip[0] + d[0], ip[1] + d[1], ip[2] + d[2]];
                let lw = q4(&e["rotation_world"])
                    .or_else(|| q4(&e["rotation"]).map(|q| [-q[0], -q[1], -q[2], q[3]]))
                    .unwrap_or([0.0, 0.0, 0.0, 1.0]);
                let w = qmul(ri, lw);
                n_ent += 1;
                push(e, pos, [-w[0], -w[1], -w[2], w[3]], None, &mut out);
            }
        }
    }
    if n_int > 0 {
        eprintln!("buildings: {n_int} interiors, {n_ent} interior entities placed in world space");
    }
    Ok(out)
}

/// child (map, index) -> parent (map, index), from sixng lod-links
/// (<work>/index/lod_parents.jsonl), for the given maps only.
fn lod_parents(ctx: &Ctx, maps: &HashSet<&str>) -> HashMap<(String, usize), (String, usize)> {
    let mut out = HashMap::new();
    let Ok(text) = fs::read_to_string(ctx.o.work.join("index").join("lod_parents.jsonl")) else {
        ctx.skip("(lod chains)", "lod_parents.jsonl missing (run sixng lod-links); entities left as orphans", json!({}));
        return out;
    };
    for l in text.lines() {
        let Some(m) = l.strip_prefix("{\"map\":\"").and_then(|r| r.split('"').next()) else { continue };
        if !maps.contains(m) {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(l) else { continue };
        for p in v["parents"].as_array().into_iter().flatten() {
            if p["kind"] != "entity" {
                continue;
            }
            if let (Some(i), Some(pm), Some(pi)) = (p["index"].as_u64(), p["map"].as_str(), p["parent_index"].as_u64()) {
                out.insert((m.to_string(), i as usize), (pm.to_string(), pi as usize));
            }
        }
    }
    out
}

/// archetype name -> lodDist from sixng lod-links (<work>/index/archetype_lod.jsonl).
fn archetype_lods(ctx: &Ctx, names: &[&String]) -> HashMap<String, f32> {
    let want: HashMap<String, &String> = names.iter().map(|n| (format!("{:08x}", crate::gta5::rsc::joaat(n)), *n)).collect();
    let mut out = HashMap::new();
    let Ok(text) = fs::read_to_string(ctx.o.work.join("index").join("archetype_lod.jsonl")) else { return out };
    for l in text.lines() {
        let Some(h) = l.strip_prefix("{\"archetype\":\"").and_then(|r| r.get(..8)) else { continue };
        let Some(n) = want.get(h) else { continue };
        if let Ok(v) = serde_json::from_str::<Value>(l) {
            if let Some(d) = v["lod_dist"].as_f64().filter(|d| *d > 0.0) {
                out.insert((*n).clone(), d as f32);
            }
        }
    }
    out
}

struct Arch {
    an: String,
    lo: V3,
    hi: V3,
    texs: Vec<String>,
}

type Built = (Mesh, Vec<String>, Vec<String>, bool);

/// rpf9 decoder path; None when the model does not decode (or is skinned).
fn rpf9_mesh(ctx: &Ctx, pools: &Pools, name: &str, an: &str, off: usize) -> Result<Option<Built>, String> {
    let an = an.to_string();
    let models = crate::model::load_models(&ctx.levels, off);
    let base = |n: &str| n.split(".#").next().unwrap_or(n).to_lowercase();
    let want = name.to_lowercase();
    let pick = models.iter().position(|m| base(&m.name) == want).or_else(|| (0..models.len()).max_by_key(|&i| models[i].meshes.iter().map(|m| m.positions.len()).sum::<usize>()));
    let Some(m) = pick.map(|i| &models[i]) else {
        return Ok(None);
    };
    if m.bones && !m.fragment {
        return Ok(None);
    }
    let frag_out = m.fragment && {
        let (dl, dh) = m.declared;
        let pad = 0.05 * (0..3).map(|k| dh[k] - dl[k]).fold(1.0f32, f32::max);
        (0..3).any(|k| m.min[k] < dl[k] - pad || m.max[k] > dh[k] + pad)
    };
    let mut mesh = Mesh { name: an.clone(), geoms: Vec::new() };
    let (mut texs, mut missing) = (Vec::new(), Vec::new());
    for sm in &m.meshes {
        // Colour slots in priority order, then every other texture: the first
        // one that decodes to a usable colour map wins (a colour slot can hold
        // a normal map or a 4x4 default).
        let tex = sm.shader.and_then(|s| m.materials.as_ref()?.get(s)).map_or_else(
            || MISSING.to_string(),
            |mat| {
                let mut cands: Vec<_> = COLOR_SLOTS.iter().filter_map(|slot| mat.textures.iter().find(|t| t.param == *slot)).collect();
                cands.extend(mat.textures.iter().filter(|t| !COLOR_SLOTS.contains(&t.param)));
                let mut errs = Vec::new();
                for t in cands {
                    let key = m.embedded.as_ref().and_then(|e| e.by_hash(t.hash)).and_then(|x| x.asset_key).unwrap_or(t.key);
                    match pools.get(t.hash, key, t.h64) {
                        Ok(_) => return format!("r6m_{:08x}", t.hash),
                        Err(e) => errs.push(format!("{:08x}: {e}", t.hash)),
                    }
                }
                missing.extend(errs.into_iter().take(1));
                MISSING.to_string()
            },
        );
        if tex != MISSING && !texs.contains(&tex) {
            texs.push(tex.clone());
        }
        let kind = sm.shader.and_then(|s| m.materials.as_ref()?.get(s)).map_or(0, |mat| kind_of(mat.bucket));
        push_geoms(&mut mesh.geoms, &sm.positions, &sm.indices, sm.uvs.as_deref(), sm.colors.as_deref(), &tex, kind);
    }
    Ok(Some((mesh, texs, missing, frag_out)))
}

/// Fragment path through sixng-core's decoder: LOD0 models of the main
/// drawable (fragment children when it has none), raw bind pose, no physics.
fn sixng_mesh(ctx: &Ctx, pools: &Pools, an: &str, e: &Entry) -> Result<Built, String> {
    use sixng_core::{drawable, resource::Resource};
    let r = Resource::load(&ctx.o.game.join("levels.rpf"), e).map_err(|x| x.to_string())?;
    let info = drawable::describe(&r);
    let slice = |i: usize| {
        let b = &info.buffers[i];
        let end = (b.gfx_off + b.bytes).min(r.gfx.len() as u64) as usize;
        &r.gfx[(b.gfx_off as usize).min(end)..end]
    };
    let main: Vec<_> = info.models.iter().filter(|m| !m.part && m.lod == 0).collect();
    let models = if main.iter().any(|m| !m.geometries.is_empty()) { main } else { info.models.iter().filter(|m| m.part).collect() };
    let key_of = |sh: &drawable::Shader, param: usize| -> u64 {
        (|| {
            let ta = r.sys_off(r.u64(sh.record_off as usize + 0x10))?;
            let t = r.sys_off(r.u64(ta + 8 * param))?;
            let tr = r.sys_off(r.u64(t + 0x30))?;
            Some(r.u64(tr + 0x28))
        })()
        .unwrap_or(0)
    };
    let drawable = r.sys_off(r.u64(0x18)).unwrap_or(0);
    let emb = [drawable, 0].into_iter().find_map(|d| crate::texture::embedded_dictionary(&r.sys, &r.gfx, d));
    let mut mesh = Mesh { name: an.to_string(), geoms: Vec::new() };
    let (mut texs, mut missing) = (Vec::new(), Vec::new());
    for md in models {
        for (k, &gi) in md.geometries.iter().enumerate() {
            let g = &info.geometries[gi];
            let Some(h) = info.buffers[g.vertex_buffer].mesh_blob.as_ref() else { continue };
            let m = drawable::decode_mesh(slice(g.vertex_buffer), h, slice(g.index_buffer));
            if m.indices.is_empty() {
                continue;
            }
            let sh = md.shader_indices.get(k).and_then(|&s| info.shaders.get(s as usize));
            let tex = sh.map_or_else(
                || MISSING.to_string(),
                |sh| {
                    let ph = |t: &&sixng_core::drawable::TexRef| u32::from_str_radix(&t.param_hash, 16).ok();
                    let mut cands: Vec<_> = COLOR_SLOTS.iter().filter_map(|slot| sh.textures.iter().find(|t| ph(t) == Some(*slot))).collect();
                    cands.extend(sh.textures.iter().filter(|t| !ph(t).is_some_and(|h| COLOR_SLOTS.contains(&h))));
                    let mut errs = Vec::new();
                    for t in cands {
                        let hash = u32::from_str_radix(&t.hash32, 16).unwrap_or(0);
                        let key = emb.as_ref().and_then(|e| e.by_hash(hash)).and_then(|x| x.asset_key).unwrap_or_else(|| key_of(sh, t.param));
                        match pools.get(hash, key, u64::from_str_radix(&t.hash64, 16).unwrap_or(0)) {
                            Ok(_) => return format!("r6m_{hash:08x}"),
                            Err(e) => errs.push(format!("{hash:08x}: {e}")),
                        }
                    }
                    missing.extend(errs.into_iter().take(1));
                    MISSING.to_string()
                },
            );
            if tex != MISSING && !texs.contains(&tex) {
                texs.push(tex.clone());
            }
            let uv = m.uvs.iter().find(|u| u.0 == 3).map(|u| u.1.as_slice());
            let col: Option<Vec<u32>> = m.colors.as_ref().map(|c| c.iter().map(|x| u32::from_le_bytes(*x)).collect());
            let kind = sh.map_or(0, |sh| kind_of(sh.flags.get(4).copied().unwrap_or(0)));
            push_geoms(&mut mesh.geoms, &m.positions, &m.indices, uv, col.as_deref(), &tex, kind);
        }
    }
    let declared = drawable::drawable_bounds(&r);
    let (lo, hi) = aabb(mesh.geoms.iter().flat_map(|g| g.positions.iter()));
    let out = declared.is_some_and(|[dl, dh]| {
        let pad = 0.05 * (0..3).map(|k| dh[k] - dl[k]).fold(1.0f32, f32::max);
        (0..3).any(|k| lo[k] < dl[k] - pad || hi[k] > dh[k] + pad)
    });
    Ok((mesh, texs, missing, out))
}

fn convert(ctx: &Ctx, pools: &Pools, name: &str, off: usize, entry: Option<&Entry>) -> Result<Option<Arch>, String> {
    let an = safe_name(name);
    let file = format!("{an}.ydr");
    let fp = format!("{TOOL}|bld|{off:x}|{name}");
    let from_info = |i: &Value| Arch {
        an: an.clone(),
        lo: f3(&i["lo"]),
        hi: f3(&i["hi"]),
        texs: i["texs"].as_array().map(|a| a.iter().filter_map(|t| t.as_str().map(String::from)).collect()).unwrap_or_default(),
    };
    if let Some(info) = ctx.up_to_date(&file, &fp) {
        let a = from_info(&info);
        if a.texs.iter().all(|t| pools.dir.join(format!("{}.dds", &t[4..])).exists()) {
            let s = ctx.emit(&file, &fp, info, None)?;
            ctx.report.lock().unwrap().converted.push(json!({"kind": "building", "name": an, "source": format!("levels.rpf@{off:x}"), "ydr": s}));
            return Ok(Some(a));
        }
    }
    let src = format!("levels.rpf@{off:x}");
    let rpf = rpf9_mesh(ctx, pools, name, &an, off)?;
    let (mesh, texs, missing, frag_out) = match (rpf, entry) {
        (Some(x), _) => x,
        (None, Some(e)) => match sixng_mesh(ctx, pools, &an, e) {
            Ok(x) => x,
            Err(err) => {
                ctx.skip(name, "model does not decode", json!({"source": src, "error": err}));
                return Ok(None);
            }
        },
        (None, None) => {
            ctx.skip(name, "model does not decode", json!({"source": src}));
            return Ok(None);
        }
    };
    let mut mesh = mesh;
    promote_base_layer(&mut mesh);
    let is_frag = entry.is_some_and(|e| e.rtype.as_deref() == Some("ft"));
    if frag_out {
        ctx.skip(name, "fragment geometry outside declared bounds (converted anyway, check placement)", json!({"source": src}));
    }
    if mesh.geoms.is_empty() {
        ctx.skip(name, "no triangles", json!({"source": format!("levels.rpf@{off:x}")}));
        return Ok(None);
    }
    if !missing.is_empty() {
        ctx.skip(name, "missing textures (placeholder used)", json!({"textures": missing}));
    }
    let (lo, hi) = aabb(mesh.geoms.iter().flat_map(|g| g.positions.iter()));
    let bytes = ydr::build_ydr(&mesh, 9998.0, None).map_err(|e| format!("{file}: {e}"))?;
    let info = json!({"lo": lo, "hi": hi, "texs": texs});
    let s = ctx.emit(&file, &fp, info.clone(), Some(bytes))?;
    ctx.report.lock().unwrap().converted.push(json!({"kind": "building", "name": an, "source": format!("levels.rpf@{off:x}"), "ydr": s, "textures": texs.len(), "fragment": is_frag}));
    Ok(Some(from_info(&info)))
}

pub fn build(ctx: &Ctx, index: &ArchiveIndex) -> Result<(), Box<dyn Error>> {
    let dir = placements_dir(ctx, index)?;
    let places = load_places(ctx, &dir)?;
    // archetype name -> model offset (first wins), most used first
    let mut use_count: HashMap<&str, (usize, usize)> = HashMap::new();
    for p in &places {
        use_count.entry(&p.name).or_insert((p.off, 0)).1 += 1;
    }
    let mut arch: Vec<(&str, usize)> = use_count.iter().map(|(n, (o, _))| (*n, *o)).collect();
    arch.sort_by(|a, b| use_count[b.0].1.cmp(&use_count[a.0].1).then(a.0.cmp(b.0)));
    eprintln!("buildings: {} placements, {} archetypes", places.len(), arch.len());
    if arch.is_empty() {
        return Ok(());
    }
    let cache = ctx.o.work.join("fivem_tex_cache");
    fs::create_dir_all(&cache)?;
    let ttdb = ctx.o.game.join("data").join("ttdb");
    let pools = Pools { game: ctx.o.game.clone(), catalog: crate::texture::Catalog::build(&ttdb).map_err(|e| eprintln!("{}: {e}", ttdb.display())).ok(), maps: Mutex::new(HashMap::new()), dir: cache.clone() };
    let by_off: HashMap<usize, &Entry> = index.entries.iter().map(|e| (e.off as usize, e)).collect();
    let res: Vec<(String, Result<Option<Arch>, String>)> =
        arch.par_iter().map(|&(n, off)| (n.to_string(), convert(ctx, &pools, n, off, by_off.get(&off).copied()))).collect();
    let mut done: BTreeMap<String, Arch> = BTreeMap::new();
    let mut names: HashMap<String, String> = HashMap::new();
    for (n, r) in res {
        if let Some(a) = r? {
            if let Some(prev) = names.insert(a.an.clone(), n.clone()) {
                return Err(format!("archetype name clash: {prev} / {n} -> {}", a.an).into());
            }
            done.insert(n, a);
        }
    }

    // Texture dictionaries. A texture used by one name family (first two
    // tokens) goes in that family's t6m_txdNNNN (<= 48 MB each); a texture
    // used by several families goes once into the shared chain
    // t6m_shr0000 <- t6m_shr0001 <- ... (gtxd.meta parents), and every family
    // txd's parent is the last shared txd, so nothing is stored twice.
    // Texture size cap by the largest model using it (bounding radius):
    // small props never show a texture at 1-2k on screen.
    let mut tcap: HashMap<String, u16> = HashMap::new();
    for a in done.values() {
        let rad = (0..3).map(|k| (a.hi[k] - a.lo[k]).powi(2)).sum::<f32>().sqrt() / 2.0;
        let cap: u16 = if rad < 1.5 { 128 } else if rad < 4.0 { 256 } else if rad < 12.0 { 512 } else { 4096 };
        for t in &a.texs {
            let e = tcap.entry(t.clone()).or_insert(0);
            *e = (*e).max(cap);
        }
    }
    let cap_of = |t: &str| -> u16 {
        let c = tcap.get(t).copied().unwrap_or(4096);
        ctx.o.max_tex.map_or(c, |m| c.min(m))
    };
    let tsize = |t: &str| fs::metadata(cache.join(format!("{}.dds", &t[4..]))).map(|m| m.len()).unwrap_or(0);
    let mut fam: BTreeMap<String, Vec<&Arch>> = BTreeMap::new();
    for a in done.values() {
        fam.entry(a.an[4..].split('_').take(2).collect::<Vec<_>>().join("_")).or_default().push(a);
    }
    let mut users: HashMap<&String, HashSet<&String>> = HashMap::new();
    for (f, v) in &fam {
        for a in v {
            for t in &a.texs {
                users.entry(t).or_default().insert(f);
            }
        }
    }
    let mut shared: Vec<String> = users.iter().filter(|(_, f)| f.len() > 1).map(|(t, _)| (*t).clone()).collect();
    shared.sort();
    let is_shared: HashSet<&String> = shared.iter().collect();
    // (file stem, textures, parent)
    let mut dicts: Vec<(String, Vec<String>, Option<String>)> = Vec::new();
    {
        let (mut cur, mut size): (Vec<String>, u64) = (Vec::new(), 0);
        for t in &shared {
            let s = tsize(t);
            if !cur.is_empty() && size + s > TXD_CAP {
                let n = dicts.len();
                dicts.push((format!("t6m_shr{n:04}"), std::mem::take(&mut cur), n.checked_sub(1).map(|p| format!("t6m_shr{p:04}"))));
                size = 0;
            }
            cur.push(t.clone());
            size += s;
        }
        if !cur.is_empty() {
            let n = dicts.len();
            dicts.push((format!("t6m_shr{n:04}"), cur, n.checked_sub(1).map(|p| format!("t6m_shr{p:04}"))));
        }
    }
    let top_shared = dicts.last().map(|d| d.0.clone());
    let mut txd_of: HashMap<String, String> = HashMap::new();
    let mut fam_n = 0usize;
    for (_, mut v) in fam {
        v.sort_by(|a, b| a.an.cmp(&b.an));
        let (mut cur, mut size): (Vec<String>, u64) = (Vec::new(), 0);
        let mut members: Vec<String> = Vec::new();
        for a in v {
            let mut new: Vec<&String> = a.texs.iter().filter(|t| !is_shared.contains(t) && !cur.contains(t)).collect();
            let add: u64 = new.iter().map(|t| tsize(t)).sum();
            if !cur.is_empty() && size + add > TXD_CAP {
                let name = format!("t6m_txd{fam_n:04}");
                fam_n += 1;
                for m in members.drain(..) {
                    txd_of.insert(m, name.clone());
                }
                dicts.push((name, std::mem::take(&mut cur), top_shared.clone()));
                size = 0;
                new = a.texs.iter().filter(|t| !is_shared.contains(t)).collect();
            }
            size += new.iter().map(|t| tsize(t)).sum::<u64>();
            cur.extend(new.into_iter().cloned());
            members.push(a.an.clone());
        }
        let name = format!("t6m_txd{fam_n:04}");
        fam_n += 1;
        for m in members {
            txd_of.insert(m, name.clone());
        }
        dicts.push((name, cur, top_shared.clone()));
    }
    let total: usize = dicts.iter().map(|d| d.1.len()).sum();
    eprintln!("buildings: {} textures ({} shared) in {} dictionaries", total, shared.len(), dicts.len());
    let results: Vec<Result<String, String>> = dicts
        .par_iter()
        .map(|(stem, list, _)| {
            let file = format!("{stem}.ytd");
            let mut sorted = list.clone();
            sorted.sort();
            let fp = sha(&format!("{TOOL}|{:?}|{}", ctx.o.max_tex, sorted.iter().map(|t| format!("{t}:{}:{}", tsize(t), cap_of(t))).collect::<Vec<_>>().join(",")));
            if ctx.up_to_date(&file, &fp).is_some() {
                return ctx.emit(&file, &fp, json!({"textures": sorted.len()}), None);
            }
            let mut texs = vec![placeholder()];
            for t in &sorted {
                let p = cache.join(format!("{}.dds", &t[4..]));
                let b = fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))?;
                let mut tex = ytd::from_dds(t, &b)?;
                tex.limit_size(cap_of(t));
                texs.push(tex);
            }
            let bytes = ytd::build_ytd(&texs).map_err(|e| format!("{file}: {e}"))?;
            ctx.emit(&file, &fp, json!({"textures": sorted.len()}), Some(bytes))
        })
        .collect();
    for r in results {
        r?;
    }
    // gtxd.meta (CMapParentTxds), referenced from fxmanifest.lua
    let gp = ctx.o.out.join("gtxd.meta");
    let rel: Vec<(&String, &String)> = dicts.iter().filter_map(|(c, _, p)| p.as_ref().map(|p| (p, c))).collect();
    if rel.is_empty() {
        let _ = fs::remove_file(&gp);
    } else {
        let mut x = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<CMapParentTxds>\n <txdRelationships>\n");
        for (p, c) in rel {
            x += &format!("  <Item>\n   <parent>{p}</parent>\n   <child>{c}</child>\n  </Item>\n");
        }
        x += " </txdRelationships>\n</CMapParentTxds>\n";
        fs::write(&gp, x)?;
    }


    // ytyp
    let names: Vec<&String> = done.keys().collect();
    let alod = archetype_lods(ctx, &names);
    // Size floor: 6m's lodDists assume its own LOD chain; in GTA a big
    // building must not stop drawing before the small props on it, so
    // lodDist >= 10x bounding radius (150..1500 m), >= 300 m when large.
    let radius = |a: &Arch| (0..3).map(|k| (a.hi[k] - a.lo[k]).powi(2)).sum::<f32>().sqrt() / 2.0;
    let floor = |a: &Arch| {
        let r = radius(a);
        let f = (r.max(1.0) * 10.0).clamp(150.0, 1500.0);
        if r >= 15.0 { f.max(300.0) } else { f }
    };
    // Clutter (bounding radius < 3 m: cones, crates, tools, signs) streams
    // only near the player: lodDist 40..100 m whatever 6m says.
    let small = |a: &Arch| radius(a) < 3.0;
    let clamp_small = |a: &Arch, d: f32| if small(a) { d.clamp(40.0, 100.0) } else { d };
    let lod = |n: &str, a: &Arch| {
        if small(a) {
            clamp_small(a, alod.get(n).copied().unwrap_or(radius(a) * 30.0))
        } else {
            alod.get(n).copied().unwrap_or(0.0).max(floor(a))
        }
    };
    let archetypes: Vec<Archetype> = done
        .iter()
        .map(|(n, a)| {
            let r = ((0..3).map(|k| (a.hi[k] - a.lo[k]).powi(2)).sum::<f32>().sqrt() / 2.0).max(1.0);
            serde_json::from_value(json!({"name": a.an, "lod_dist": lod(n, a), "bb_min": a.lo, "bb_max": a.hi, "bs_radius": r, "hd_texture_dist": 100.0,
                "texture_dictionary": txd_of[&a.an]}))
            .unwrap()
        })
        .collect();
    let yt = meta::build_ytyp(&MapTypes { name: "t6m_bld".into(), archetypes: archetypes.clone() })?;
    emit_meta(ctx, "t6m_bld.ytyp", yt)?;

    // LOD chains: level 0 in 512 m cells, each LOD level in one ymap
    // (t6m_lod1 = LOD, t6m_lod2 = SLOD1, ...), each the parent of the one below.
    let kept: Vec<&Place> = places.iter().filter(|p| done.contains_key(&p.name)).collect();
    let maps: HashSet<&str> = kept.iter().filter_map(|p| p.key.as_ref().map(|k| k.0.as_str())).collect();
    let links = lod_parents(ctx, &maps);
    let by_key: HashMap<&(String, usize), usize> = kept.iter().enumerate().filter_map(|(i, p)| p.key.as_ref().map(|k| (k, i))).collect();
    let mut parent: Vec<Option<usize>> = kept
        .iter()
        .map(|p| {
            let q = *by_key.get(links.get(p.key.as_ref()?)?)?;
            (!ctx.o.no_lod && kept[q].level == p.level + 1).then_some(q)
        })
        .collect();
    // drop LOD-level entities whose children are all gone (they would draw over HD), bottom up
    let top = kept.iter().map(|p| p.level).max().unwrap_or(0);
    // --no-lod: HD only, every entity ORPHANHD
    let mut alive: Vec<bool> = kept.iter().map(|p| !ctx.o.no_lod || p.level == 0).collect();
    for lvl in 1..=top {
        let mut has = vec![false; kept.len()];
        for (i, pa) in parent.iter().enumerate() {
            if let (true, Some(q)) = (alive[i], pa) {
                has[*q] = true;
            }
        }
        for i in 0..kept.len() {
            if kept[i].level == lvl && !has[i] {
                alive[i] = false;
            }
        }
    }
    let mut dropped = 0;
    for i in 0..kept.len() {
        if !alive[i] {
            dropped += 1;
        }
        if parent[i].is_some_and(|q| !alive[q]) {
            parent[i] = None;
        }
    }
    if dropped > 0 {
        ctx.skip(&format!("({dropped} placements)"), "LOD/SLOD entity without converted children", json!({"count": dropped}));
    }
    let elod = |i: usize| {
        let p = kept[i];
        let a = &done[&p.name];
        if p.elod > 0.0 {
            if small(a) { clamp_small(a, p.elod) } else { p.elod.max(floor(a)) }
        } else {
            lod(&p.name, a)
        }
    };
    let mut kids = vec![0u32; kept.len()];
    let mut kid_lod = vec![0f32; kept.len()];
    for (i, pa) in parent.iter().enumerate() {
        if let (true, Some(q)) = (alive[i], *pa) {
            kids[q] += 1;
            kid_lod[q] = kid_lod[q].max(elod(i));
        }
    }
    let ent = |i: usize, guid: usize, pidx: i32| -> Entity {
        let p = kept[i];
        let a = &done[&p.name];
        // A parent must cover its children: childLodDist >= every child's
        // lodDist and its own lodDist beyond that, or the HD never streams.
        let child = if kids[i] > 0 { p.child_lod.max(kid_lod[i]) } else { 0.0 };
        let d = elod(i).max(child * 1.25);
        let level = if p.level == 0 && parent[i].is_none() { "LODTYPES_DEPTH_ORPHANHD" } else { LEVEL_NAMES[(p.level as usize).min(5)] };
        serde_json::from_value(json!({"archetype_name": a.an, "flags": if p.level > 0 { 1572864 + 32 } else { 32 }, "guid": guid, "position": p.pos, "rotation": p.rot,
            "scale_xy": p.sxy, "scale_z": p.sz, "lod_dist": d, "parent_index": pidx, "lod_level": level, "num_children": kids[i], "child_lod_dist": child}))
        .unwrap()
    };
    // index of each entity within its level's ymap, top level first
    let mut slot = vec![0usize; kept.len()];
    let lvl_name = |l: u8| format!("t6m_lod{l}");
    for lvl in (1..=top).rev() {
        let ids: Vec<usize> = (0..kept.len()).filter(|&i| alive[i] && kept[i].level == lvl).collect();
        if ids.is_empty() {
            let _ = fs::remove_file(ctx.o.out.join(format!("{}.ymap", lvl_name(lvl))));
            continue;
        }
        for (k, &i) in ids.iter().enumerate() {
            slot[i] = k;
        }
        let has_parent = ids.iter().any(|&i| parent[i].is_some());
        let entities: Vec<Entity> = ids.iter().enumerate().map(|(k, &i)| ent(i, k + 1, parent[i].map_or(-1, |q| slot[q] as i32))).collect();
        let name = lvl_name(lvl);
        let pname = if has_parent { lvl_name(lvl + 1) } else { String::new() };
        let map = MapData { entities, ..serde_json::from_value(json!({"name": name, "parent": pname, "flags": 2, "content_flags": 2, "exported_by": "6ixpack fivem"})).unwrap() };
        emit_meta(ctx, &format!("{name}.ymap"), meta::build_ymap_with(&map, &archetypes)?)?;
    }
    let mut cells: BTreeMap<(i64, i64), Vec<usize>> = BTreeMap::new();
    for i in 0..kept.len() {
        if alive[i] && kept[i].level == 0 {
            let p = kept[i];
            cells.entry(((p.pos[0] / 512.0).floor() as i64, (p.pos[1] / 512.0).floor() as i64)).or_default().push(i);
        }
    }
    // Wide ground-anchored models (asphalt tiles, aprons, pads, buildings):
    // footprint and underside z,
    // so the coarse terrain can be sunk below them (terrain::sink).
    {
        let mut ground: Vec<[f32; 5]> = Vec::new();
        for (i, p) in kept.iter().enumerate() {
            let Some(a) = done.get(&p.name) else { continue };
            if !alive[i] || p.level != 0 {
                continue;
            }
            let s = p.sxy.max(0.01);
            let (dx, dy, dz) = ((a.hi[0] - a.lo[0]) * s, (a.hi[1] - a.lo[1]) * s, (a.hi[2] - a.lo[2]) * p.sz);
            // ground-anchored (model starts at its origin's height) and wide;
            // tall ones are fine too: terrain under a building is hidden anyway
            let _ = dz;
            if dx.min(dy) >= 16.0 && (-1.5..=1.0).contains(&(a.lo[2] * p.sz)) {
                // axis-aligned box of the rotated footprint
                let r = (dx * dx + dy * dy).sqrt() / 2.0;
                let c = [p.pos[0] + (a.lo[0] + a.hi[0]) / 2.0 * s, p.pos[1] + (a.lo[1] + a.hi[1]) / 2.0 * s];
                let flat = p.rot[0].abs() < 0.01 && p.rot[1].abs() < 0.01 && (p.rot[2].abs() < 0.01 || p.rot[3].abs() < 0.01);
                let (hx, hy) = if flat { (dx / 2.0, dy / 2.0) } else { (r, r) };
                ground.push([c[0] - hx, c[1] - hy, c[0] + hx, c[1] + hy, p.pos[2] + a.lo[2] * p.sz]);
            }
        }
        let gp = ctx.o.work.join("fivem_ground.json");
        fs::write(&gp, serde_json::to_string(&ground)?)?;
        eprintln!("buildings: {} flat ground tiles recorded for terrain", ground.len());
    }
    let links_n = (0..kept.len()).filter(|&i| alive[i] && parent[i].is_some()).count();
    eprintln!("buildings: {} entities, {links_n} LOD links, {top} LOD levels", alive.iter().filter(|x| **x).count());
    for ((cx, cy), ids) in cells {
        let name = format!("t6m_bld_{:02}_{:02}", cx + 50, cy + 50);
        let has_parent = ids.iter().any(|&i| parent[i].is_some());
        let entities: Vec<Entity> = ids.iter().enumerate().map(|(j, &i)| ent(i, j + 1, parent[i].map_or(-1, |q| slot[q] as i32))).collect();
        let pname = if has_parent { lvl_name(1) } else { String::new() };
        let map = MapData { entities, ..serde_json::from_value(json!({"name": name, "parent": pname, "exported_by": "6ixpack fivem"})).unwrap() };
        let ym = meta::build_ymap_with(&map, &archetypes)?;
        emit_meta(ctx, &format!("{name}.ymap"), ym)?;
    }
    Ok(())
}

/// Meta files are cheap to rebuild: rewrite only when the bytes differ.
pub fn emit_meta(ctx: &Ctx, file: &str, b: Vec<u8>) -> Result<String, String> {
    let same = fs::read(ctx.stream.join(file)).is_ok_and(|x| x == b);
    ctx.emit(file, "meta", Value::Null, (!same).then_some(b))
}
