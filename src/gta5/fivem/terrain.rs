//! Terrain (terrain_port.py): every maptile_*_terrain_std model becomes
//! t6m_terrain_NNN.ydr (embedded baked ground texture, planar UVs) +
//! .ybn (static collision); one t6m_terrain.ytyp and t6m_terrain.ymap with
//! all selected tiles at the origin (tile geometry is in world space).
//! NNN numbers tiles over the whole archive in (offset, child) order, so
//! names match the Python output whatever --bbox selects.

use super::geom::{aabb, push_geoms, V3};
use super::{Ctx, TOOL};
use crate::gta5::{
    meta::{self, Archetype, Entity, MapData, MapTypes},
    ybn,
    ydr::{self, Mesh},
    ytd::{self, Tex, TexFormat},
};
use rayon::prelude::*;
use serde_json::{json, Value};
use sixng_core::scan::ArchiveIndex;
use std::error::Error;

const TEX: usize = 512;
const PAL: [[f32; 3]; 4] = [[194.0, 178.0, 128.0], [86.0, 112.0, 58.0], [120.0, 98.0, 70.0], [118.0, 114.0, 108.0]];

struct Tile {
    off: usize,
    child: usize,
    verts: Vec<V3>,
    tris: Vec<[u32; 3]>,
}

struct Rng(u64);
impl Rng {
    fn f(&mut self) -> f32 {
        // splitmix64
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        ((z ^ (z >> 31)) >> 40) as f32 / (1u64 << 24) as f32
    }
}

/// Height + slope raster blended sand/grass/dirt/rock (terrain_port.bake),
/// row 0 = north; A8R8G8B8 with a full box-filtered mip chain.
fn bake(name: &str, v: &[V3], tris: &[[u32; 3]], x0: f32, y0: f32) -> Tex {
    let n = TEX;
    let mut h = vec![f32::NAN; n * n];
    let mut s = vec![0f32; n * n];
    let px: Vec<f32> = v.iter().map(|p| (p[0] - x0) / 1024.0 * n as f32).collect();
    let py: Vec<f32> = v.iter().map(|p| (1.0 - (p[1] - y0) / 1024.0) * n as f32).collect();
    for t in tris {
        let [a, b, c] = t.map(|i| i as usize);
        let e1 = [v[b][0] - v[a][0], v[b][1] - v[a][1], v[b][2] - v[a][2]];
        let e2 = [v[c][0] - v[a][0], v[c][1] - v[a][1], v[c][2] - v[a][2]];
        let nn = [e1[1] * e2[2] - e1[2] * e2[1], e1[2] * e2[0] - e1[0] * e2[2], e1[0] * e2[1] - e1[1] * e2[0]];
        let ln = (nn[0] * nn[0] + nn[1] * nn[1] + nn[2] * nn[2]).sqrt();
        if ln == 0.0 {
            continue;
        }
        let slope = (nn[2].abs() / ln).min(1.0).acos().to_degrees();
        let (xs, ys) = ([px[a], px[b], px[c]], [py[a], py[b], py[c]]);
        let c0 = xs.iter().cloned().fold(f32::MAX, f32::min).max(0.0) as i64;
        let c1 = (xs.iter().cloned().fold(f32::MIN, f32::max).ceil() as i64).min(n as i64 - 1);
        let r0 = ys.iter().cloned().fold(f32::MAX, f32::min).max(0.0) as i64;
        let r1 = (ys.iter().cloned().fold(f32::MIN, f32::max).ceil() as i64).min(n as i64 - 1);
        if c0 > c1 || r0 > r1 {
            continue;
        }
        let d = (ys[1] - ys[2]) * (xs[0] - xs[2]) + (xs[2] - xs[1]) * (ys[0] - ys[2]);
        if d.abs() < 1e-9 {
            continue;
        }
        for r in r0..=r1 {
            for cc in c0..=c1 {
                let (gx, gy) = (cc as f32 + 0.5, r as f32 + 0.5);
                let l0 = ((ys[1] - ys[2]) * (gx - xs[2]) + (xs[2] - xs[1]) * (gy - ys[2])) / d;
                let l1 = ((ys[2] - ys[0]) * (gx - xs[2]) + (xs[0] - xs[2]) * (gy - ys[2])) / d;
                let l2 = 1.0 - l0 - l1;
                if l0 >= -0.01 && l1 >= -0.01 && l2 >= -0.01 {
                    let i = r as usize * n + cc as usize;
                    h[i] = l0 * v[a][2] + l1 * v[b][2] + l2 * v[c][2];
                    s[i] = slope;
                }
            }
        }
    }
    let mut rng = Rng(((x0 * 7.0 + y0) as i64).unsigned_abs());
    let gn = n / 32 + 1;
    let g: Vec<f32> = (0..gn * gn).map(|_| rng.f()).collect();
    let tpos = |i: usize| i as f32 * (gn - 1) as f32 / (n - 1) as f32;
    let mut px_out = Vec::with_capacity(n * n * 4);
    for r in 0..n {
        for c in 0..n {
            let (ty, tx) = (tpos(r), tpos(c));
            let (iy, ix) = ((ty as usize).min(gn - 2), (tx as usize).min(gn - 2));
            let (fy, fx) = (ty - iy as f32, tx - ix as f32);
            let at = |y: usize, x: usize| g[y * gn + x];
            let lowf = (at(iy, ix) * (1.0 - fx) + at(iy, ix + 1) * fx) * (1.0 - fy) + (at(iy + 1, ix) * (1.0 - fx) + at(iy + 1, ix + 1) * fx) * fy;
            let i = r * n + c;
            let hh = if h[i].is_nan() { 0.0 } else { h[i] };
            let ss = s[i];
            let ws = ((2.5 - hh) / 2.0).clamp(0.0, 1.0);
            let wr = (((ss - 30.0) / 10.0).clamp(0.0, 1.0) + ((hh - 250.0) / 80.0).clamp(0.0, 0.6)).clamp(0.0, 1.0) * (1.0 - ws);
            let wd = ((ss - 14.0) / 10.0 + (lowf - 0.6)).clamp(0.0, 1.0) * (1.0 - ws - wr);
            let wg = (1.0 - ws - wr - wd).clamp(0.0, 1.0);
            let k = (0.85 + 0.3 * rng.f()) * (0.92 + 0.16 * lowf);
            let ch = |j: usize| ((ws * PAL[0][j] + wg * PAL[1][j] + wd * PAL[2][j] + wr * PAL[3][j]) * k).clamp(0.0, 255.0) as u8;
            px_out.extend_from_slice(&[ch(2), ch(1), ch(0), 255]);
        }
    }
    // mip chain
    let mut data = px_out.clone();
    let (mut cur, mut w, mut levels) = (px_out, n, 1u8);
    while w > 1 {
        let nw = w / 2;
        let mut next = vec![0u8; nw * nw * 4];
        for y in 0..nw {
            for x in 0..nw {
                for k in 0..4 {
                    let p = |yy: usize, xx: usize| cur[(yy * w + xx) * 4 + k] as u32;
                    next[(y * nw + x) * 4 + k] = ((p(2 * y, 2 * x) + p(2 * y, 2 * x + 1) + p(2 * y + 1, 2 * x) + p(2 * y + 1, 2 * x + 1) + 2) / 4) as u8;
                }
            }
        }
        data.extend_from_slice(&next);
        cur = next;
        w = nw;
        levels += 1;
    }
    Tex { name: name.into(), width: n as u16, height: n as u16, levels, format: TexFormat::A8R8G8B8, data, unk32: 0, usage_data: 0, extra_flags: 0 }
}

pub fn build(ctx: &Ctx, index: &ArchiveIndex) -> Result<(), Box<dyn Error>> {
    let mut offs: Vec<usize> = index
        .entries
        .iter()
        .filter(|e| e.name.as_deref().is_some_and(|n| n.starts_with("maptile_") && n.ends_with("_terrain_std")))
        .map(|e| e.off as usize)
        .collect();
    offs.sort();
    offs.dedup();
    eprintln!("terrain: decoding {} maptile resources", offs.len());
    let mut tiles: Vec<Tile> = offs
        .par_iter()
        .flat_map_iter(|&off| {
            crate::model::load_models(&ctx.levels, off).into_iter().map(move |m| {
                let (mut verts, mut tris) = (Vec::new(), Vec::new());
                for mesh in &m.meshes {
                    let base = verts.len() as u32;
                    verts.extend_from_slice(&mesh.positions);
                    tris.extend(mesh.indices.chunks_exact(3).map(|t| [t[0] + base, t[1] + base, t[2] + base]));
                }
                Tile { off, child: m.drawable, verts, tris }
            })
        })
        .filter(|t| !t.tris.is_empty())
        .collect();
    tiles.sort_by_key(|t| (t.off, t.child));

    let sel: Vec<(usize, &Tile)> = tiles
        .iter()
        .enumerate()
        .filter(|(_, t)| {
            let (lo, hi) = aabb(t.verts.iter());
            ctx.o.in_bbox(lo, hi)
        })
        .collect();
    eprintln!("terrain: {} tiles, {} in region", tiles.len(), sel.len());
    // sink terrain under flat ground tiles recorded by the buildings stage
    let ground: Vec<[f32; 5]> = std::fs::read_to_string(ctx.o.work.join("fivem_ground.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    let sunk: Vec<(usize, Vec<V3>, u64)> = sel.iter().map(|&(i, t)| { let (v, n) = sink(&t.verts, &ground); (i, v, n) }).collect();
    let sunk_n: u64 = sunk.iter().map(|x| x.2).sum();
    if sunk_n > 0 {
        eprintln!("terrain: {sunk_n} vertices sunk below {} ground tiles", ground.len());
    }
    let sunk: std::collections::HashMap<usize, (Vec<V3>, u64)> = sunk.into_iter().map(|(i, v, n)| (i, (v, n))).collect();
    if sel.is_empty() {
        return Ok(());
    }
    let infos: Vec<Result<(String, Value), String>> = sel
        .par_iter()
        .map(|&(i, t)| {
            let name = format!("t6m_terrain_{i:03}");
            let (verts, n_sunk) = &sunk[&i];
            let t = &Tile { off: t.off, child: t.child, verts: verts.clone(), tris: t.tris.clone() };
            let fp = format!("{TOOL}|terrain|{:x}|{:x}|{}|{}|{:x}", t.off, t.child, t.verts.len(), n_sunk, t.verts.iter().fold(0u64, |h, v| h.rotate_left(5) ^ (v[2].to_bits() as u64)));
            let (ydr_f, ybn_f) = (format!("{name}.ydr"), format!("{name}.ybn"));
            let (lo, hi) = aabb(t.verts.iter());
            let c: V3 = std::array::from_fn(|k| (lo[k] + hi[k]) / 2.0);
            let r = t.verts.iter().map(|v| (0..3).map(|k| (v[k] - c[k]).powi(2)).sum::<f32>()).fold(0.0, f32::max).sqrt();
            let info = json!({"lo": lo, "hi": hi, "c": c, "r": r});
            let fresh = ctx.up_to_date(&ydr_f, &fp).is_some() && ctx.up_to_date(&ybn_f, &fp).is_some();
            let (yd, yb) = if fresh {
                (None, None)
            } else {
                let x0 = ((lo[0] / 1024.0 + 0.01).floor()) * 1024.0;
                let y0 = ((lo[1] / 1024.0 + 0.01).floor()) * 1024.0;
                let tex_name = format!("t6m_g{i:03}");
                let tex = bake(&tex_name, &t.verts, &t.tris, x0, y0);
                let uvs: Vec<[f32; 2]> = t.verts.iter().map(|v| [(v[0] - x0) / 1024.0, (v[1] - y0) / 1024.0]).collect();
                let idx: Vec<u32> = t.tris.iter().flatten().copied().collect();
                let mut mesh = Mesh { name: name.clone(), geoms: Vec::new() };
                push_geoms(&mut mesh.geoms, &t.verts, &idx, Some(&uvs), None, &tex_name, 0);
                let texs = [tex];
                let embed = |b: &mut crate::gta5::rsc::Builder| ytd::add_dict(b, &texs);
                let yd = ydr::build_ydr(&mesh, 9998.0, Some(&embed)).map_err(|e| format!("{ydr_f}: {e}"))?;
                let yb = ybn::build_ybn(&t.verts, &t.tris, 0).map_err(|e| format!("{ybn_f}: {e}"))?;
                (Some(yd), Some(yb))
            };
            let s1 = ctx.emit(&ydr_f, &fp, info.clone(), yd)?;
            let s2 = ctx.emit(&ybn_f, &fp, info.clone(), yb)?;
            ctx.report.lock().unwrap().converted.push(json!({"kind": "terrain", "name": name, "source": format!("levels.rpf@{:x} child {:x}", t.off, t.child), "ydr": s1, "ybn": s2}));
            Ok((name, info))
        })
        .collect();
    let mut arch = Vec::new();
    for r in infos {
        arch.push(r?);
    }
    arch.sort_by(|a, b| a.0.cmp(&b.0));

    let f3 = |v: &Value| -> [f32; 3] { std::array::from_fn(|k| v[k].as_f64().unwrap_or(0.0) as f32) };
    let archetypes: Vec<Archetype> = arch
        .iter()
        .map(|(n, i)| {
            serde_json::from_value(json!({"name": n, "lod_dist": 3000.0, "bb_min": i["lo"], "bb_max": i["hi"], "bs_centre": i["c"], "bs_radius": i["r"], "hd_texture_dist": 500.0}))
                .unwrap()
        })
        .collect();
    let types = MapTypes { name: "t6m_terrain".into(), archetypes };
    let entities: Vec<Entity> = arch
        .iter()
        .enumerate()
        .map(|(j, (n, _))| serde_json::from_value(json!({"archetype_name": n, "flags": 32, "guid": 1000 + j, "position": [0.0, 0.0, 0.0], "lod_dist": 3000.0})).unwrap())
        .collect();
    let mut lo = [f32::MAX; 3];
    let mut hi = [f32::MIN; 3];
    for (_, i) in &arch {
        let (l, h) = (f3(&i["lo"]), f3(&i["hi"]));
        for k in 0..3 {
            lo[k] = lo[k].min(l[k]);
            hi[k] = hi[k].max(h[k]);
        }
    }
    let pad = 3000.0;
    let map = MapData {
        streaming_extents: Some([lo.map(|v| v - pad), hi.map(|v| v + pad)]),
        entities_extents: Some([lo, hi]),
        entities,
        ..serde_json::from_value(json!({"name": "t6m_terrain", "exported_by": "6ixpack fivem"})).unwrap()
    };
    let yt = meta::build_ytyp(&types)?;
    let ym = meta::build_ymap(&map)?;
    super::buildings::emit_meta(ctx, "t6m_terrain.ytyp", yt)?;
    super::buildings::emit_meta(ctx, "t6m_terrain.ymap", ym)?;
    Ok(())
}

/// Terrain vertices inside a flat ground tile's footprint drop to 0.3 m
/// below the tile's underside: the terrain mesh is far coarser (one vertex
/// per ~35 m) than 6m's road and apron tiles and otherwise covers them.
/// Returns the new vertices and how many moved.
/// One terrain vertex spacing: every terrain triangle over a tile gets
/// all three corners sunk.
const MARGIN: f32 = 40.0;

fn sink(verts: &[V3], ground: &[[f32; 5]]) -> (Vec<V3>, u64) {
    let mut n = 0;
    let out = verts
        .iter()
        .map(|v| {
            let mut z = v[2];
            for g in ground {
                if v[0] >= g[0] - MARGIN && v[0] <= g[2] + MARGIN && v[1] >= g[1] - MARGIN && v[1] <= g[3] + MARGIN {
                    z = z.min(g[4] - 0.3);
                }
            }
            if z < v[2] {
                n += 1;
            }
            [v[0], v[1], z]
        })
        .collect();
    (out, n)
}
