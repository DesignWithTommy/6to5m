//! Triangle lists -> ydr geometries (obj2ydrxml.py semantics): area-weighted
//! normals over source vertices, UVs as stored (DirectX top-left), vertex colours as GTA V ambient,
//! split so no geometry exceeds 65535 vertices.

use crate::gta5::ydr::Geom;
use std::collections::HashMap;

pub type V3 = [f32; 3];

/// Append geometries for one source mesh. `uvs` are raw (OBJ-style, V up);
/// `colours` packed R in the low byte.
/// `kind`: GTA V bucket (ydr::Geom::kind). Decals are lifted 5 mm along
/// their normals so they do not z-fight with the surface below.
pub fn push_geoms(out: &mut Vec<Geom>, pos: &[V3], idx: &[u32], uvs: Option<&[[f32; 2]]>, colours: Option<&[u32]>, tex: &str, kind: u8) {
    push_geoms_n(out, pos, idx, None, uvs, colours, tex, kind)
}

/// `push_geoms` with source vertex normals (used when given for every
/// vertex; otherwise area-weighted face normals are computed).
#[allow(clippy::too_many_arguments)]
pub fn push_geoms_n(out: &mut Vec<Geom>, pos: &[V3], idx: &[u32], normals: Option<&[V3]>, uvs: Option<&[[f32; 2]]>, colours: Option<&[u32]>, tex: &str, kind: u8) {
    let n = pos.len();
    let mut nrm = vec![[0f32; 3]; n];
    for t in idx.chunks_exact(3) {
        let (a, b, c) = (t[0] as usize, t[1] as usize, t[2] as usize);
        if a >= n || b >= n || c >= n {
            continue;
        }
        let e1 = [pos[b][0] - pos[a][0], pos[b][1] - pos[a][1], pos[b][2] - pos[a][2]];
        let e2 = [pos[c][0] - pos[a][0], pos[c][1] - pos[a][1], pos[c][2] - pos[a][2]];
        let f = [e1[1] * e2[2] - e1[2] * e2[1], e1[2] * e2[0] - e1[0] * e2[2], e1[0] * e2[1] - e1[1] * e2[0]];
        for i in [a, b, c] {
            for k in 0..3 {
                nrm[i][k] += f[k];
            }
        }
    }
    let unit = |v: V3| {
        let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        if l > 0.0 { [v[0] / l, v[1] / l, v[2] / l] } else { [0.0, 0.0, 1.0] }
    };
    let uvs = uvs.filter(|u| u.len() == n);
    let colours = colours.filter(|c| c.len() == n);
    let lift = if kind == 2 { 0.005 } else { 0.0 };
    let new = || Geom { texture: tex.to_string(), kind, ..Default::default() };
    let mut g = new();
    let mut remap: HashMap<u32, u16> = HashMap::new();
    // weld: source vertices that differ only in attributes we drop
    // (tangents, extra UV sets) collapse into one output vertex
    let mut weld: HashMap<([i32; 3], [i8; 3], [i32; 2], u32), u16> = HashMap::new();
    for t in idx.chunks_exact(3) {
        if t.iter().any(|&i| i as usize >= n) || t[0] == t[1] || t[1] == t[2] || t[0] == t[2] {
            continue;
        }
        let fresh = t.iter().filter(|i| !remap.contains_key(i)).count();
        if g.positions.len() + fresh > 65535 {
            out.push(std::mem::replace(&mut g, new()));
            remap.clear();
            weld.clear();
        }
        for &i in t {
            let v = *remap.entry(i).or_insert_with(|| {
                let s = i as usize;
                let nn = normals.filter(|v| v.len() == n).map_or_else(|| unit(nrm[s]), |v| unit(v[s]));
                let p = [pos[s][0] + nn[0] * lift, pos[s][1] + nn[1] * lift, pos[s][2] + nn[2] * lift];
                // 6m COLOR0.r is not ambient occlusion: it floors at exactly
                // 0.20 and sits at ~0.2 over whole flat-wall materials
                // (ap_09_side_w: mean 0.21 over 1,786 vertices) while
                // multilayer walls swing 0.2..1.0 mid-wall. GTA V reads
                // Colour0.r/g as natural/artificial ambient scale, so writing
                // it there painted dark bands interpolated across large
                // triangles (the blurry facade smears). Ambient stays full;
                // decals keep a (decal.sps multiplies alpha by Colour0.a,
                // where 6m decals fade their edges).
                let col = colours.map_or(0xFFFF_FFFF, |c| if kind == 2 { 0x00FF_FFFF | (c[s] & 0xFF00_0000) } else { 0xFFFF_FFFF });
                // 6m UVs are DirectX top-left like GTA V: no flip (the old
                // obj2ydrxml 1-v flip assumed OBJ bottom-left input).
                let uv = uvs.map_or([0.0, 0.0], |u| u[s]);
                let key = (
                    p.map(|x| (x * 1000.0).round() as i32),
                    nn.map(|x| (x * 127.0).round() as i8),
                    uv.map(|x| (x * 4096.0).round() as i32),
                    col,
                );
                *weld.entry(key).or_insert_with(|| {
                    g.positions.push(p);
                    g.normals.push(nn);
                    g.colours.push(col);
                    g.uvs.push(uv);
                    (g.positions.len() - 1) as u16
                })
            });
            g.indices.push(v);
        }
    }
    if !g.indices.is_empty() {
        out.push(g);
    }
}

pub fn aabb<'a>(pts: impl Iterator<Item = &'a V3>) -> (V3, V3) {
    let mut lo = [f32::MAX; 3];
    let mut hi = [f32::MIN; 3];
    for p in pts {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    (lo, hi)
}
