//! GTA V navmesh cells (`navmesh[X][Y].ynv`, resource version 2): convex
//! polygons over quantised vertices with per-edge neighbours and a sector
//! quadtree, one file per 150 m cell of the 100 x 100 grid from (-6000, -6000).
//!
//! Layout from CodeWalker.Core `Resources/Nav.cs` / `FileTypes/YnvFile.cs`;
//! the field placement follows rage-formats `ynv.rs` (Unlicense), laid out
//! here with our RSC block builder (no block straddles a page).

use super::rsc::{BlockId, Builder, Seg, W};
use std::collections::HashMap;

pub const CELL: f32 = 150.0;
pub const ORIGIN: f32 = -6000.0;
/// "No neighbour" area / polygon id.
pub const NONE: u32 = 0x3FFF;
pub const FLAG_ROAD: u32 = 1024;
pub const FLAG_CELL_EDGE: u32 = 2048;

const VFT: u32 = 0x4061_E7E8;
const VFT_VERTS: u32 = 1080158456;
const VFT_INDICES: u32 = 1080158424;
const VFT_EDGES: u32 = 1080158440;
const VFT_POLYS: u32 = 1080158408;
const POLY_SIZE: usize = 48;

/// Cell (x, y) of a position; None off the grid.
pub fn cell_of(x: f32, y: f32) -> Option<(u32, u32)> {
    let cx = ((x - ORIGIN) / CELL).floor();
    let cy = ((y - ORIGIN) / CELL).floor();
    ((0.0..100.0).contains(&cx) && (0.0..100.0).contains(&cy)).then_some((cx as u32, cy as u32))
}

pub fn cell_bounds(cx: u32, cy: u32) -> [f32; 4] {
    let (x, y) = (ORIGIN + cx as f32 * CELL, ORIGIN + cy as f32 * CELL);
    [x, y, x + CELL, y + CELL]
}

pub fn file_name(cx: u32, cy: u32) -> String {
    format!("navmesh[{}][{}].ynv", cx * 3, cy * 3)
}

#[derive(Clone, Debug)]
pub struct Poly {
    pub verts: Vec<[f32; 3]>,
    /// Neighbour across the edge leaving vertex i: (area id, poly index) or (NONE, NONE).
    pub edges: Vec<(u32, u32)>,
    pub flags0: u16,
    pub flags1: u32,
    pub flags2: u32,
}

/// One grid cell. `zr` is the z range the vertices are quantised into.
pub fn build_ynv(cx: u32, cy: u32, zr: [f32; 2], polys: &[Poly]) -> Result<Vec<u8>, String> {
    let area = cy * 100 + cx;
    if polys.len() >= NONE as usize {
        return Err(format!("cell {cx},{cy}: {} polygons", polys.len()));
    }
    let cb = cell_bounds(cx, cy);
    let mn = [cb[0], cb[1], zr[0]];
    let mx = [cb[2], cb[3], zr[1].max(zr[0] + 1.0)];
    let size = [mx[0] - mn[0], mx[1] - mn[1], mx[2] - mn[2]];
    let quant = |v: [f32; 3]| -> [u16; 3] { std::array::from_fn(|k| (((v[k] - mn[k]) / size[k]).clamp(0.0, 1.0) * 65535.0).round() as u16) };

    let mut areas: Vec<u32> = Vec::new();
    for id in [area, NONE, area.wrapping_sub(100), area.wrapping_sub(1), area + 1, area + 100] {
        if !areas.contains(&id) {
            areas.push(id);
        }
    }
    let mut vix: HashMap<[u16; 3], u16> = HashMap::new();
    let mut verts: Vec<[u16; 3]> = Vec::new();
    let mut idx: Vec<u16> = Vec::new();
    let mut edges = W::new();
    let mut prec = W::new();
    let mut cells: Vec<[i16; 6]> = Vec::with_capacity(polys.len());
    let per_part = 16384 / POLY_SIZE;
    for (pi, p) in polys.iter().enumerate() {
        let n = p.verts.len();
        if !(3..=0x7FF).contains(&n) {
            return Err(format!("cell {cx},{cy}: polygon {pi} has {n} vertices"));
        }
        let start = idx.len();
        if start + n > u16::MAX as usize {
            return Err(format!("cell {cx},{cy}: more than 65535 polygon vertices"));
        }
        for (k, &v) in p.verts.iter().enumerate() {
            let qv = quant(v);
            let i = match vix.get(&qv) {
                Some(&i) => i,
                None => {
                    if verts.len() > u16::MAX as usize {
                        return Err(format!("cell {cx},{cy}: more than 65536 vertices"));
                    }
                    verts.push(qv);
                    vix.insert(qv, (verts.len() - 1) as u16);
                    (verts.len() - 1) as u16
                }
            };
            idx.push(i);
            let (ea, ep) = p.edges.get(k).copied().unwrap_or((NONE, NONE));
            let ai = match areas.iter().position(|&a| a == ea) {
                Some(i) => i,
                None if areas.len() < 32 => {
                    areas.push(ea);
                    areas.len() - 1
                }
                None => return Err(format!("cell {cx},{cy}: more than 32 adjacent areas")),
            } as u32;
            let w = (ai & 0x1F) | ((ep & 0x3FFF) << 5);
            edges.u32(w).u32(w);
        }
        let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
        for v in &p.verts {
            for k in 0..3 {
                lo[k] = lo[k].min(v[k]);
                hi[k] = hi[k].max(v[k]);
            }
        }
        let c = aabb_cell(lo, hi);
        cells.push(c);
        // centroid in the cell, 0-255 (CodeWalker UnkX / UnkY)
        let cen: [f32; 2] = std::array::from_fn(|k| p.verts.iter().map(|v| v[k]).sum::<f32>() / n as f32);
        let ux = (((cen[0] - mn[0]) / size[0]).clamp(0.0, 1.0) * 255.0) as u32;
        let uy = (((cen[1] - mn[1]) / size[1]).clamp(0.0, 1.0) * 255.0) as u32;
        prec.u16(p.flags0).u16((n as u16) << 5).u16(start as u16).u16(area as u16).zeros(16);
        for v in c {
            prec.u16(v as u16);
        }
        prec.u32(p.flags1).u32((p.flags2 & !0xFFFF) | ux | (uy << 8)).u32((((pi / per_part) as u32) & 0xFF) << 4);
    }

    let mut b = Builder::new();
    let root = b.reserve(Seg::Sys);
    let vb: Vec<u8> = verts.iter().flat_map(|q| q.iter().flat_map(|c| c.to_le_bytes())).collect();
    let ib: Vec<u8> = idx.iter().flat_map(|i| i.to_le_bytes()).collect();
    let (eb, pb) = (edges.b, prec.b);
    let lv = list(&mut b, VFT_VERTS, &vb, 6);
    let li = list(&mut b, VFT_INDICES, &ib, 2);
    let le = list(&mut b, VFT_EDGES, &eb, 8);
    let lp = list(&mut b, VFT_POLYS, &pb, POLY_SIZE);
    let mut total = (vb.len() + ib.len() + eb.len() + pb.len()) as u32;
    let sector = sector(&mut b, mn, mx, 2, &cells, &mut total);

    let mut h = W::new();
    h.u32(VFT).u32(1).u64(0).u32(1).u32(0x0001_0011).u64(0);
    for row in 0..4 {
        for col in 0..4 {
            h.f32(if col == 3 { f32::NAN } else if row == col { 1.0 } else { 0.0 });
        }
    }
    h.v3(size).u32(0x7F80_0001);
    h.ptr(Some(lv)).u64(0).ptr(Some(li)).ptr(Some(le));
    h.u32(idx.len() as u32).u32(areas.len() as u32);
    for i in 0..32 {
        h.u32(areas.get(i).copied().unwrap_or(0));
    }
    h.ptr(Some(lp)).ptr(Some(sector)).u64(0).u64(0);
    h.u32(verts.len() as u32).u32(polys.len() as u32).u32(area).u32(total);
    h.u32(0).u32(0).u32(0).zeros(12); // points, portals, portal links
    h.u32(0x85CB_3561).zeros(12);
    debug_assert_eq!(h.b.len(), 368);
    b.set(root, h);
    b.build(2)
}

fn aabb_cell(lo: [f32; 3], hi: [f32; 3]) -> [i16; 6] {
    let f = |v: f32| (v * 4.0).floor().clamp(-32768.0, 32767.0) as i16;
    let c = |v: f32| (v * 4.0).ceil().clamp(-32768.0, 32767.0) as i16;
    [f(lo[0]), c(hi[0]), f(lo[1]), c(hi[1]), f(lo[2]), c(hi[2])]
}

/// `NavMeshList<T>`: 48-byte header, part table, parts of at most 16 KiB, offsets.
fn list(b: &mut Builder, vft: u32, items: &[u8], size: usize) -> BlockId {
    let per = 16384 / size;
    let parts: Vec<&[u8]> = items.chunks(per * size).collect();
    let mut table = W::new();
    let mut offs = W::new();
    let mut first = 0u32;
    for p in &parts {
        let id = b.sys_bytes(p.to_vec());
        table.ptr(Some(id)).u32((p.len() / size) as u32).u32(0);
        offs.u32(first);
        first += (p.len() / size) as u32;
    }
    let t = (!parts.is_empty()).then(|| b.add(Seg::Sys, table));
    let o = (!parts.is_empty()).then(|| b.add(Seg::Sys, offs));
    let mut h = W::new();
    h.u32(vft).u32(1).u32((items.len() / size) as u32).u32(0).ptr(t).ptr(o).u32(parts.len() as u32).zeros(12);
    b.add(Seg::Sys, h)
}

/// Sector quadtree (`NavMeshSector` 96 bytes, leaves hold `NavMeshSectorData`).
fn sector(b: &mut Builder, mn: [f32; 3], mx: [f32; 3], depth: u32, cells: &[[i16; 6]], total: &mut u32) -> BlockId {
    let id = b.reserve(Seg::Sys);
    let c = aabb_cell(mn, mx);
    let mut s = W::new();
    s.v3(mn).f32(f32::NAN).v3(mx).f32(f32::NAN);
    for v in c {
        s.u16(v as u16);
    }
    *total += 96;
    if depth == 0 {
        let ids: Vec<u8> = cells
            .iter()
            .enumerate()
            .filter(|(_, p)| p[1] >= c[0] && p[0] <= c[1] && p[3] >= c[2] && p[2] <= c[3])
            .flat_map(|(i, _)| (i as u16).to_le_bytes())
            .collect();
        let n = ids.len() / 2;
        *total += 32 + ids.len() as u32;
        let ib = (n > 0).then(|| b.sys_bytes(ids));
        let mut d = W::new();
        d.u32(0).u32(0).ptr(ib).u64(0).u16(n as u16).u16(0).u32(0);
        let db = b.add(Seg::Sys, d);
        s.ptr(Some(db)).zeros(44);
    } else {
        let cen: [f32; 3] = std::array::from_fn(|k| (mn[k] + mx[k]) * 0.5);
        let quads = [
            ([cen[0], cen[1], cen[2]], [mx[0], mx[1], mx[2]]),
            ([cen[0], mn[1], 0.0], [mx[0], cen[1], 0.0]),
            ([mn[0], mn[1], mn[2]], [cen[0], cen[1], cen[2]]),
            ([mn[0], cen[1], 0.0], [cen[0], mx[1], 0.0]),
        ];
        s.u64(0);
        for (qa, qb) in quads {
            // child boxes use the full z range for polygon lookup
            let sub = sector(b, [qa[0], qa[1], mn[2]], [qb[0], qb[1], mx[2]], depth - 1, cells, total);
            s.ptr(Some(sub));
        }
        s.zeros(12);
    }
    debug_assert_eq!(s.b.len(), 96);
    b.set(id, s);
    id
}
