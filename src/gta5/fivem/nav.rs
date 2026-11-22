//! AI pathing: 6m road nodes -> nodes<area>.ynd (traffic), 6m navmesh tiles
//! -> navmesh[X][Y].ynv (peds), for the GTA V grid cells overlapping --bbox.
//!
//! Sources are found once and cached in <work>:
//!   fivem_paths_src.json     ids of the node and link meta resources in levels.rpf
//!   index/navmeshes.json     carving index of navmeshes.rpf (built on first use)
//!   fivem_navmesh_index.jsonl  world bbox of every navmesh tile
//!
//! Whole grid cells are written (a ynd/ynv replaces the game's file for that
//! cell). Links and polygon edges only name cells this build writes, so
//! nothing points into a vanilla file with different indices.

use super::{sha, Ctx};
use crate::gta5::{ynd, ynv};
use rayon::prelude::*;
use serde_json::{json, Value};
use sixng_core::{
    classify::Kind,
    resource::Resource,
    scan::{ArchiveIndex, Entry},
};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    error::Error,
    fs,
    io::Write,
};

/// Bump when the ynd / ynv output changes.
pub const YND_V: &str = "ynd1";
pub const YNV_V: &str = "ynv2";

fn overlaps(ctx: &Ctx, b: [f32; 4]) -> bool {
    ctx.o.in_bbox([b[0], b[1], 0.0], [b[2], b[3], 0.0])
}

// ─── paths ──────────────────────────────────────────────────────────────────

/// Node and link resources: the largest meta resources holding each struct.
fn path_sources<'a>(ctx: &Ctx, index: &'a ArchiveIndex) -> Option<(&'a Entry, &'a Entry)> {
    let rpf = ctx.o.game.join("levels.rpf");
    let metas: Vec<&Entry> = index.entries.iter().filter(|e| e.kind == Kind::Resource && e.sub == "7a").collect();
    let cp = ctx.o.work.join("fivem_paths_src.json");
    let by_id: HashMap<&str, &Entry> = metas.iter().map(|e| (e.id.as_str(), *e)).collect();
    if let Some(v) = fs::read(&cp).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) {
        if v["entries"].as_u64() == Some(metas.len() as u64) {
            if let (Some(n), Some(l)) = (v["nodes"].as_str().and_then(|s| by_id.get(s)), v["links"].as_str().and_then(|s| by_id.get(s))) {
                return Some((n, l));
            }
        }
    }
    // the node file is ~3.8 MB and the link file ~2.9 MB: probe big metas first
    let mut cands: Vec<&Entry> = metas.iter().copied().filter(|e| e.usize >= 256 * 1024).collect();
    cands.sort_by_key(|e| std::cmp::Reverse(e.usize));
    eprintln!("paths: locating node/link resources among {} meta resources (once; cached in {})", cands.len(), cp.display());
    let found: Vec<(&Entry, usize, usize)> = cands
        .par_iter()
        .filter_map(|e| {
            let r = Resource::load_sys(&rpf, e).ok()?;
            let m = sixng_core::meta7a::Meta::parse(&r)?;
            let n = m.structs.contains_key(&sixng_core::paths::NODE).then(|| m.objects(sixng_core::paths::NODE).len()).unwrap_or(0);
            let l = m.structs.contains_key(&sixng_core::paths::LINK).then(|| m.objects(sixng_core::paths::LINK).len()).unwrap_or(0);
            (n + l > 0).then_some((*e, n, l))
        })
        .collect();
    let n = found.iter().filter(|x| x.1 > 0).max_by_key(|x| x.1)?.0;
    let l = found.iter().filter(|x| x.2 > 0).max_by_key(|x| x.2)?.0;
    let _ = fs::write(&cp, json!({"entries": metas.len(), "nodes": n.id, "links": l.id}).to_string());
    Some((n, l))
}

const PNF_NO_GPS: u64 = 1 << 0;
const PNF_WATER: u64 = 1 << 3;
const PNF_TUNNEL: u64 = 1 << 5;
const PNF_GPS_ONLY: u64 = 1 << 6;
const PNF_SLIP_LANE: u64 = 1 << 7;
const PNF_HIGHWAY_SLIP: u64 = 1 << 17;
const PNF_NO_BIG: u64 = 1 << 26;
const PNF_GIVE_WAY: u64 = 1 << 28;
const PNF_STOP: u64 = 1 << 29;
const PNF_YIELD: u64 = 1 << 30;

pub fn build_paths(ctx: &Ctx, index: &ArchiveIndex) -> Result<(), Box<dyn Error>> {
    let Some((ne, le)) = path_sources(ctx, index) else {
        ctx.skip("(paths)", "no path node/link resources found in levels.rpf", json!({}));
        return Ok(());
    };
    let rpf = ctx.o.game.join("levels.rpf");
    let nodes = sixng_core::paths::nodes(&Resource::load_sys(&rpf, ne)?);
    let recs = sixng_core::paths::link_records(&Resource::load_sys(&rpf, le)?);
    let (links, _) = sixng_core::paths::links(&nodes, &recs);
    let flags: Vec<u64> = nodes.iter().map(|n| u64::from_str_radix(&n.flags, 16).unwrap_or(0)).collect();
    let area: Vec<Option<u16>> = nodes.iter().zip(&flags).map(|(n, f)| if f & PNF_WATER != 0 { None } else { ynd::area_of(n.p[0], n.p[1]) }).collect();
    let emit: BTreeSet<u16> = area.iter().flatten().copied().filter(|&a| overlaps(ctx, ynd::area_bounds(a))).collect();
    eprintln!("paths: {} nodes, {} links -> {} ynd areas", nodes.len(), links.len(), emit.len());
    if emit.is_empty() {
        return Ok(());
    }
    // local index of each kept node inside its area
    let mut members: BTreeMap<u16, Vec<usize>> = BTreeMap::new();
    let mut local = vec![u16::MAX; nodes.len()];
    for (i, a) in area.iter().enumerate() {
        if let Some(a) = a.filter(|a| emit.contains(a)) {
            let m = members.entry(a).or_default();
            local[i] = m.len() as u16;
            m.push(i);
        }
    }
    let mut out_links: Vec<Vec<&sixng_core::paths::Link>> = vec![Vec::new(); nodes.len()];
    for l in &links {
        if local[l.from] != u16::MAX && local[l.to] != u16::MAX && l.from != l.to && !out_links[l.from].iter().any(|x| x.to == l.to) {
            out_links[l.from].push(l);
        }
    }
    let res: Vec<Result<(), String>> = members
        .par_iter()
        .map(|(&a, ids)| {
            let file = format!("nodes{a}.ynd");
            let yn: Vec<ynd::Node> = ids
                .iter()
                .map(|&i| {
                    let (n, f) = (&nodes[i], flags[i]);
                    let ls = &out_links[i][..out_links[i].len().min(31)];
                    let offroad = !ls.is_empty() && ls.iter().all(|l| l.flags & (1 << 5) != 0);
                    let special: u8 = if f & PNF_STOP != 0 { 16 } else if f & (PNF_GIVE_WAY | PNF_YIELD) != 0 { 17 } else { 0 };
                    let mut fl = [0u8; 5];
                    fl[0] = if offroad { 8 } else { 0 } | if f & PNF_NO_BIG != 0 { 32 } else { 0 };
                    fl[1] = (f & (PNF_SLIP_LANE | PNF_HIGHWAY_SLIP) != 0) as u8 | (special << 3);
                    fl[2] = (f & PNF_NO_GPS != 0) as u8 | if n.road_class == 0 { 64 } else { 0 } | if f & PNF_GPS_ONLY != 0 { 0x90 } else { 0 };
                    fl[3] = (f & PNF_TUNNEL != 0) as u8;
                    fl[4] = n.density.min(15);
                    let speed = match n.speed {
                        s if s < 7.0 => 0,
                        s if s < 14.0 => 1,
                        s if s < 21.0 => 2,
                        _ => 3,
                    };
                    let links = ls
                        .iter()
                        .map(|l| ynd::Link {
                            area: area[l.to].unwrap(),
                            node: local[l.to],
                            flags0: (l.flags & 1) as u8,
                            flags1: 0,
                            flags2: (l.lanes_to.min(7) << 5) | (l.lanes_from.min(7) << 2) | if l.flags & 4 != 0 { 2 } else { 0 },
                            length: l.length.min(255) as u8,
                        })
                        .collect();
                    ynd::Node { pos: n.p, street: u32::from_str_radix(&n.street, 16).unwrap_or(0), flags: fl, speed, links }
                })
                .collect();
            let fp = sha(&format!("{YND_V}|{a}|{yn:?}"));
            if let Some(info) = ctx.up_to_date(&file, &fp) {
                ctx.emit(&file, &fp, info, None)?;
                return Ok(());
            }
            let bytes = ynd::build_ynd(a, &yn).map_err(|e| format!("{file}: {e}"))?;
            let nl: usize = yn.iter().map(|n| n.links.len()).sum();
            let s = ctx.emit(&file, &fp, json!({"nodes": yn.len(), "links": nl}), Some(bytes))?;
            ctx.report.lock().unwrap().converted.push(json!({"kind": "paths", "ynd": s, "nodes": yn.len(), "links": nl}));
            Ok(())
        })
        .collect();
    for r in res {
        r?;
    }
    Ok(())
}

// ─── navmesh ────────────────────────────────────────────────────────────────

type Tile = (u32, Vec<[f32; 3]>); // (6m poly flags, world vertices)

fn tiles(path: &std::path::Path, e: &Entry) -> Option<sixng_core::navmesh::NavMesh> {
    sixng_core::navmesh::decode(&Resource::load_sys(path, e).ok()?)
}

/// Clip a convex polygon to an xy rectangle (Sutherland-Hodgman).
fn clip(poly: &[[f32; 3]], r: [f32; 4]) -> Vec<[f32; 3]> {
    let mut p = poly.to_vec();
    for (axis, lo, v) in [(0, true, r[0]), (0, false, r[2]), (1, true, r[1]), (1, false, r[3])] {
        if p.is_empty() {
            break;
        }
        let inside = |q: &[f32; 3]| if lo { q[axis] >= v } else { q[axis] <= v };
        let mut out = Vec::with_capacity(p.len() + 2);
        for i in 0..p.len() {
            let (a, b) = (p[i], p[(i + 1) % p.len()]);
            let (ia, ib) = (inside(&a), inside(&b));
            if ia {
                out.push(a);
            }
            if ia != ib {
                let t = (v - a[axis]) / (b[axis] - a[axis]);
                let mut c: [f32; 3] = std::array::from_fn(|k| a[k] + (b[k] - a[k]) * t);
                c[axis] = v;
                out.push(c);
            }
        }
        p = out;
    }
    // drop repeated points
    p.dedup_by(|a, b| (a[0] - b[0]).abs() < 1e-3 && (a[1] - b[1]).abs() < 1e-3);
    if p.len() > 1 && (p[0][0] - p[p.len() - 1][0]).abs() < 1e-3 && (p[0][1] - p[p.len() - 1][1]).abs() < 1e-3 {
        p.pop();
    }
    p
}

fn key(v: [f32; 3]) -> [i32; 3] {
    [(v[0] * 20.0).round() as i32, (v[1] * 20.0).round() as i32, (v[2] * 2.0).round() as i32]
}

pub fn build_navmesh(ctx: &Ctx) -> Result<(), Box<dyn Error>> {
    let rpf = ctx.o.game.join("navmeshes.rpf");
    if !rpf.is_file() {
        ctx.skip("(navmesh)", "navmeshes.rpf missing", json!({}));
        return Ok(());
    }
    let ix_dir = ctx.o.work.join("index");
    if !ix_dir.join("navmeshes.json").is_file() {
        eprintln!("navmesh: indexing navmeshes.rpf (once)");
    }
    let index = sixng_core::scan::build_index(&rpf, "navmeshes.rpf", &ix_dir, false, None).map_err(|e| format!("navmeshes index: {e:#}"))?;
    let cands: Vec<&Entry> = index.entries.iter().filter(|e| e.kind == Kind::RageObj).collect();
    let ip = ctx.o.work.join("fivem_navmesh_index.jsonl");
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
        eprintln!("navmesh: indexing {} tiles (once; cached in {})", cands.len(), ip.display());
        let found: Vec<(String, [[f32; 3]; 2])> = cands.par_iter().filter_map(|e| tiles(&rpf, e).map(|n| (e.id.clone(), n.bbox))).collect();
        let mut f = fs::File::create(&ip)?;
        writeln!(f, "{}", json!({"tool": YNV_V, "entries": cands.len()}))?;
        for (id, b) in &found {
            writeln!(f, "{}", json!({"id": id, "bbox": b}))?;
            boxes.insert(id.clone(), *b);
        }
    }
    // cells to write: grid cells overlapping the region that some tile covers
    let mut emit: BTreeSet<(u32, u32)> = BTreeSet::new();
    for b in boxes.values() {
        let (Some(lo), Some(hi)) = (ynv::cell_of(b[0][0].max(-5999.0), b[0][1].max(-5999.0)), ynv::cell_of(b[1][0].min(8999.0), b[1][1].min(8999.0))) else { continue };
        for cx in lo.0..=hi.0 {
            for cy in lo.1..=hi.1 {
                if overlaps(ctx, ynv::cell_bounds(cx, cy)) {
                    emit.insert((cx, cy));
                }
            }
        }
    }
    eprintln!("navmesh: {} ynv cells in region", emit.len());
    if emit.is_empty() {
        return Ok(());
    }
    let by_id: BTreeMap<&str, &Entry> = cands.iter().map(|e| (e.id.as_str(), *e)).collect();
    let tiles_in = |r: [f32; 4]| -> Vec<&str> { boxes.iter().filter(|(_, b)| b[1][0] >= r[0] && b[0][0] <= r[2] && b[1][1] >= r[1] && b[0][1] <= r[3]).map(|(id, _)| id.as_str()).collect() };
    // fingerprint: tiles around the cell (neighbour indices go into edges) and which neighbours are written
    let fps: BTreeMap<(u32, u32), String> = emit
        .iter()
        .map(|&(cx, cy)| {
            let b = ynv::cell_bounds(cx, cy);
            let r = [b[0] - ynv::CELL, b[1] - ynv::CELL, b[2] + ynv::CELL, b[3] + ynv::CELL];
            let nb: Vec<bool> = [(0i32, -1i32), (-1, 0), (1, 0), (0, 1)].iter().map(|(dx, dy)| emit.contains(&((cx as i32 + dx) as u32, (cy as i32 + dy) as u32))).collect();
            ((cx, cy), sha(&format!("{YNV_V}|{cx},{cy}|{nb:?}|{}", tiles_in(r).join(","))))
        })
        .collect();
    let dirty: Vec<(u32, u32)> = emit
        .iter()
        .copied()
        .filter(|c| {
            let f = ynv::file_name(c.0, c.1);
            match ctx.up_to_date(&f, &fps[c]) {
                Some(info) => ctx.emit(&f, &fps[c], info, None).map(|_| false).unwrap_or(true),
                None => true,
            }
        })
        .collect();
    if dirty.is_empty() {
        return Ok(());
    }
    // decode every tile touching a dirty cell or its neighbours
    let mut need: BTreeSet<&str> = BTreeSet::new();
    for &(cx, cy) in &dirty {
        let b = ynv::cell_bounds(cx, cy);
        need.extend(tiles_in([b[0] - ynv::CELL, b[1] - ynv::CELL, b[2] + ynv::CELL, b[3] + ynv::CELL]));
    }
    let decoded: Vec<Vec<Tile>> = need
        .par_iter()
        .filter_map(|id| tiles(&rpf, by_id.get(id)?))
        .map(|n| n.polys.iter().map(|(f, ids)| (*f, ids.iter().map(|&i| n.vertices[i as usize]).collect())).collect())
        .collect();
    // clip each polygon into the written cells it overlaps
    let mut cells: BTreeMap<(u32, u32), Vec<ynv::Poly>> = BTreeMap::new();
    for (f, vs) in decoded.iter().flatten() {
        let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
        for v in vs {
            lo = [lo[0].min(v[0]), lo[1].min(v[1])];
            hi = [hi[0].max(v[0]), hi[1].max(v[1])];
        }
        let (Some(a), Some(b)) = (ynv::cell_of(lo[0], lo[1]), ynv::cell_of(hi[0], hi[1])) else { continue };
        for cx in a.0..=b.0 {
            for cy in a.1..=b.1 {
                if !emit.contains(&(cx, cy)) {
                    continue;
                }
                let cb = ynv::cell_bounds(cx, cy);
                let p = clip(vs, cb);
                if p.len() < 3 {
                    continue;
                }
                let flags1 = if f & sixng_core::navmesh::ROAD != 0 { ynv::FLAG_ROAD } else { 0 } ;
                let n = p.len();
                cells.entry((cx, cy)).or_default().push(ynv::Poly { verts: p, edges: vec![(ynv::NONE, ynv::NONE); n], flags0: 0, flags1, flags2: 0 });
            }
        }
    }
    cells.par_iter_mut().for_each(|(c, ps)| {
        decimate(ps);
        merge(ps);
        let cb = ynv::cell_bounds(c.0, c.1);
        for p in ps.iter_mut() {
            if p.verts.iter().any(|v| v[0] <= cb[0] + 1e-3 || v[0] >= cb[2] - 1e-3 || v[1] <= cb[1] + 1e-3 || v[1] >= cb[3] - 1e-3) {
                p.flags1 |= ynv::FLAG_CELL_EDGE;
            }
        }
    });
    // adjacency: polygons sharing an edge (same endpoints, opposite direction)
    let mut edge_map: HashMap<([i32; 3], [i32; 3]), Vec<(u32, u32)>> = HashMap::new();
    for (&(cx, cy), ps) in &cells {
        for (pi, p) in ps.iter().enumerate() {
            for k in 0..p.verts.len() {
                let (a, b) = (key(p.verts[k]), key(p.verts[(k + 1) % p.verts.len()]));
                if a != b {
                    edge_map.entry((a, b)).or_default().push((cy * 100 + cx, pi as u32));
                }
            }
        }
    }
    for (&(cx, cy), ps) in cells.iter_mut() {
        let me = cy * 100 + cx;
        for (pi, p) in ps.iter_mut().enumerate() {
            for k in 0..p.verts.len() {
                let (a, b) = (key(p.verts[k]), key(p.verts[(k + 1) % p.verts.len()]));
                if let Some(o) = edge_map.get(&(b, a)) {
                    if let Some(&n) = o.iter().find(|&&(ar, q)| !(ar == me && q == pi as u32)) {
                        p.edges[k] = n;
                    }
                }
            }
        }
    }
    let res: Vec<Result<(), String>> = dirty
        .par_iter()
        .map(|c| {
            let file = ynv::file_name(c.0, c.1);
            let ps = cells.get(c).map(|v| v.as_slice()).unwrap_or(&[]);
            if ps.is_empty() {
                return Ok(());
            }
            let zr = ps.iter().flat_map(|p| p.verts.iter().map(|v| v[2])).fold([f32::MAX, f32::MIN], |r, z| [r[0].min(z), r[1].max(z)]);
            let bytes = match ynv::build_ynv(c.0, c.1, [zr[0] - 1.0, zr[1] + 1.0], ps) {
                Ok(b) => b,
                Err(e) => {
                    ctx.skip(&file, &e, json!({}));
                    return Ok(());
                }
            };
            let roads = ps.iter().filter(|p| p.flags1 & ynv::FLAG_ROAD != 0).count();
            let s = ctx.emit(&file, &fps[c], json!({"polygons": ps.len(), "road_polygons": roads}), Some(bytes))?;
            ctx.report.lock().unwrap().converted.push(json!({"kind": "navmesh", "ynv": s, "polygons": ps.len()}));
            Ok(())
        })
        .collect();
    for r in res {
        r?;
    }
    Ok(())
}

/// Greedily merge neighbouring polygons (same flags, near-coplanar, convex
/// in xy, at most MAX_V vertices): 6m tiles are 5-10x finer than a GTA V
/// cell may hold (16383 polygons). Vertices are never dropped, so shared
/// edges with other polygons and cells stay intact.
fn merge(ps: &mut Vec<ynv::Poly>) {
    const MAX_V: usize = 32;
    let mut alive = vec![true; ps.len()];
    let mut edges: HashMap<([i32; 3], [i32; 3]), usize> = HashMap::new();
    let ekeys = |p: &ynv::Poly| -> Vec<([i32; 3], [i32; 3])> { (0..p.verts.len()).map(|k| (key(p.verts[k]), key(p.verts[(k + 1) % p.verts.len()]))).collect() };
    for (i, p) in ps.iter().enumerate() {
        for e in ekeys(p) {
            edges.insert(e, i);
        }
    }
    let mut dbg = [0usize; 6];
    for i in 0..ps.len() {
        if !alive[i] {
            continue;
        }
        let mut k = 0;
        while k < ps[i].verts.len() {
            let n = ps[i].verts.len();
            let (a, b) = (key(ps[i].verts[k]), key(ps[i].verts[(k + 1) % n]));
            let Some(&j) = edges.get(&(b, a)) else {
                dbg[0] += 1;
                k += 1;
                continue;
            };
            if j == i || !alive[j] || ps[j].flags1 != ps[i].flags1 || n + ps[j].verts.len() - 2 > MAX_V {
                dbg[1] += 1;
                k += 1;
                continue;
            }
            let q = &ps[j];
            let m = q.verts.len();
            let Some(jb) = (0..m).find(|&t| key(q.verts[t]) == b && key(q.verts[(t + 1) % m]) == a) else {
                k += 1;
                continue;
            };
            // P from b around to a, then Q strictly between a and b
            let mut v: Vec<[f32; 3]> = (0..n).map(|t| ps[i].verts[(k + 1 + t) % n]).collect();
            v.extend((2..m).map(|t| q.verts[(jb + t) % m]));
            if !convex(&v) { dbg[2] += 1; }
            if !planar(&v) { dbg[3] += 1; }
            if !convex(&v) || !planar(&v) {
                k += 1;
                continue;
            }
            let old: Vec<_> = ekeys(&ps[i]).into_iter().chain(ekeys(&ps[j])).collect();
            for e in old {
                edges.remove(&e);
            }
            alive[j] = false;
            ps[i].verts = v;
            for e in ekeys(&ps[i]) {
                edges.insert(e, i);
            }
            k = 0;
        }
    }
    if std::env::var_os("NAV_DEBUG").is_some() { eprintln!("merge: {} -> {} | {:?}", ps.len(), alive.iter().filter(|a| **a).count(), dbg); }
    let mut it = alive.iter();
    ps.retain(|_| *it.next().unwrap());
    for p in ps.iter_mut() {
        p.edges = vec![(ynv::NONE, ynv::NONE); p.verts.len()];
    }
}

fn convex(v: &[[f32; 3]]) -> bool {
    let n = v.len();
    let (mut pos, mut neg) = (false, false);
    for i in 0..n {
        let (a, b, c) = (v[i], v[(i + 1) % n], v[(i + 2) % n]);
        let cr = (b[0] - a[0]) * (c[1] - b[1]) - (b[1] - a[1]) * (c[0] - b[0]);
        if cr > 1e-4 {
            pos = true;
        } else if cr < -1e-4 {
            neg = true;
        }
    }
    !(pos && neg)
}

/// All vertices within 0.25 m of the plane through the first three.
fn planar(v: &[[f32; 3]]) -> bool {
    let (a, b, c) = (v[0], v[1], v[2]);
    let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let w = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    let nrm = [u[1] * w[2] - u[2] * w[1], u[2] * w[0] - u[0] * w[2], u[0] * w[1] - u[1] * w[0]];
    let l = (nrm[0] * nrm[0] + nrm[1] * nrm[1] + nrm[2] * nrm[2]).sqrt();
    if l < 1e-6 || nrm[2].abs() / l < 0.2 {
        return false;
    }
    v.iter().all(|p| ((p[0] - a[0]) * nrm[0] + (p[1] - a[1]) * nrm[1] + (p[2] - a[2]) * nrm[2]).abs() / l < 0.5)
}

/// Cells far over the polygon budget: weld each flag class into a triangle
/// mesh and collapse edges (meshopt, open borders locked, so cell edges and
/// road / non-road seams keep their vertices), then `merge` rebuilds polygons.
fn decimate(ps: &mut Vec<ynv::Poly>) {
    const BUDGET: usize = 9000;
    if ps.len() <= BUDGET {
        return;
    }
    let target_frac = BUDGET as f32 / ps.len() as f32;
    let mut classes: BTreeMap<u32, (Vec<[f32; 3]>, Vec<u32>, HashMap<[i32; 3], u32>)> = BTreeMap::new();
    for p in ps.iter() {
        let c = classes.entry(p.flags1).or_default();
        let ids: Vec<u32> = p
            .verts
            .iter()
            .map(|&v| {
                *c.2.entry(key(v)).or_insert_with(|| {
                    c.0.push(v);
                    (c.0.len() - 1) as u32
                })
            })
            .collect();
        for t in 1..ids.len() - 1 {
            if ids[0] != ids[t] && ids[t] != ids[t + 1] && ids[0] != ids[t + 1] {
                c.1.extend([ids[0], ids[t], ids[t + 1]]);
            }
        }
    }
    let mut out = Vec::new();
    for (f, (v, idx, _)) in classes {
        if idx.is_empty() {
            continue;
        }
        let target = ((idx.len() / 3) as f32 * target_frac * 1.6) as usize * 3;
        let mut dst = vec![0u32; idx.len()];
        let k = meshopt_rs::simplify::simplify(&mut dst, &idx, &v, target.min(idx.len()), 0.01);
        let tris = if k >= 3 { &dst[..k] } else { &idx[..] };
        for t in tris.chunks_exact(3) {
            let mut vs = [v[t[0] as usize], v[t[1] as usize], v[t[2] as usize]];
            // keep counter-clockwise in xy like the source
            let cr = (vs[1][0] - vs[0][0]) * (vs[2][1] - vs[0][1]) - (vs[1][1] - vs[0][1]) * (vs[2][0] - vs[0][0]);
            if cr.abs() < 1e-5 {
                continue;
            }
            if cr < 0.0 {
                vs.swap(1, 2);
            }
            out.push(ynv::Poly { verts: vs.to_vec(), edges: vec![(ynv::NONE, ynv::NONE); 3], flags0: 0, flags1: f, flags2: 0 });
        }
    }
    *ps = out;
}
