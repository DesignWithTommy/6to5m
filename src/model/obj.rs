//! Wavefront OBJ import (port of ObjModelReader.cs): positions, UVs, polygon
//! faces triangulated by ear clipping; normals are validated and discarded.

use super::decode::{Mesh, Model, V3};
use std::collections::HashMap;

pub const MAX_CHARS: usize = 32 << 20;
const MAX_SOURCE: usize = 1_000_000;
const MAX_OUTPUT: usize = 2_000_000;
const MAX_TRIANGLES: usize = 2_000_000;
const MAX_MESHES: usize = 1024;
const MAX_POLYGON: usize = 256;

struct Part {
    name: String,
    has_uv: bool,
    map: HashMap<(usize, Option<usize>), u32>,
    positions: Vec<V3>,
    uvs: Vec<[f32; 2]>,
    indices: Vec<u32>,
}

fn number(s: &str) -> Result<f32, String> {
    match s.parse::<f32>() {
        Ok(v) if v.is_finite() && v.abs() <= 1_000_000.0 => Ok(v),
        _ => Err("expected a finite number within the supported range".into()),
    }
}

fn index(s: &str, count: usize) -> Result<usize, String> {
    let v: i64 = s.parse().map_err(|_| "face index must be a nonzero integer")?;
    let r = match v {
        0 => return Err("face index must be a nonzero integer".into()),
        v if v > 0 => v - 1,
        v => count as i64 + v,
    };
    if r < 0 || r as usize >= count {
        return Err("face index is outside the available source list".into());
    }
    Ok(r as usize)
}

fn safe_name(v: &str, fallback: &str) -> String {
    let r: String = v.chars().filter(|c| !c.is_control()).take(256).collect();
    if r.trim().is_empty() { fallback.into() } else { r }
}

pub fn parse(text: &str, name: &str) -> Result<Model, String> {
    if text.len() > MAX_CHARS {
        return Err("OBJ text exceeds the size limit".into());
    }
    let (mut vertices, mut coords, mut normals) = (Vec::<V3>::new(), Vec::<[f32; 2]>::new(), 0usize);
    let mut parts: Vec<Part> = Vec::new();
    let mut current: Option<usize> = None;
    let mut group = "default".to_owned();
    let (mut output_vertices, mut triangles, mut operations) = (0usize, 0usize, 0u64);
    for (n, raw) in text.lines().enumerate() {
        let line_no = n + 1;
        let err = |m: String| format!("OBJ line {line_no}: {m}");
        if raw.len() > 1 << 20 {
            return Err(err("line is too long".into()));
        }
        let line = raw.split('#').next().unwrap_or("").trim().trim_start_matches('\u{FEFF}');
        if line.is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        let result: Result<(), String> = (|| {
            match fields[0] {
                "v" => {
                    if ![4, 5, 7, 8].contains(&fields.len()) {
                        return Err("expected a 3D vertex".into());
                    }
                    if vertices.len() >= MAX_SOURCE {
                        return Err("vertex limit exceeded".into());
                    }
                    let mut p = [number(fields[1])?, number(fields[2])?, number(fields[3])?];
                    let homogeneous = matches!(fields.len(), 5 | 8);
                    if homogeneous {
                        let w = number(fields[4])?;
                        if w == 0.0 {
                            return Err("zero homogeneous coordinate".into());
                        }
                        p = p.map(|c| c / w);
                    }
                    for f in &fields[if homogeneous { 5 } else { 4 }..] {
                        number(f)?;
                    }
                    if !p.iter().all(|c| c.is_finite() && c.abs() <= 1_000_000.0) {
                        return Err("vertex is outside the supported finite range".into());
                    }
                    vertices.push(p);
                }
                "vt" => {
                    if !(2..=4).contains(&fields.len()) || coords.len() >= MAX_SOURCE {
                        return Err("invalid texture coordinate count".into());
                    }
                    coords.push([number(fields[1])?, if fields.len() >= 3 { number(fields[2])? } else { 0.0 }]);
                    if fields.len() == 4 {
                        number(fields[3])?;
                    }
                }
                "vn" => {
                    normals += 1;
                    if fields.len() != 4 || normals > MAX_SOURCE {
                        return Err("invalid normal count".into());
                    }
                    for f in &fields[1..] {
                        number(f)?;
                    }
                }
                "o" | "g" => {
                    group = safe_name(&fields[1..].join(" "), "default");
                    current = None;
                }
                "f" => {
                    let count = fields.len() - 1;
                    if !(3..=MAX_POLYGON).contains(&count) {
                        return Err("face must have 3 to 256 corners".into());
                    }
                    let mut corners = Vec::with_capacity(count);
                    let mut has_uv = true;
                    for f in &fields[1..] {
                        let idx: Vec<&str> = f.split('/').collect();
                        if idx.len() > 3 {
                            return Err("invalid face index".into());
                        }
                        let position = index(idx[0], vertices.len())?;
                        let mut uv = None;
                        if idx.len() >= 2 && !idx[1].is_empty() {
                            uv = Some(index(idx[1], coords.len())?);
                        } else {
                            has_uv = false;
                        }
                        if idx.len() == 3 {
                            index(idx[2], normals)?;
                        }
                        corners.push((position, uv));
                    }
                    let mut distinct: Vec<usize> = corners.iter().map(|c| c.0).collect();
                    distinct.sort_unstable();
                    distinct.dedup();
                    if distinct.len() != count {
                        return Err("face repeats a vertex".into());
                    }
                    let points: Vec<V3> = corners.iter().map(|c| vertices[c.0]).collect();
                    let local = triangulate(&points, &mut operations)?;
                    triangles += local.len() / 3;
                    if triangles > MAX_TRIANGLES {
                        return Err("triangle limit exceeded".into());
                    }
                    let part = match current {
                        Some(i) if parts[i].has_uv == has_uv => i,
                        _ => {
                            if parts.len() >= MAX_MESHES {
                                return Err("mesh limit exceeded".into());
                            }
                            parts.push(Part {
                                name: group.clone(),
                                has_uv,
                                map: HashMap::new(),
                                positions: Vec::new(),
                                uvs: Vec::new(),
                                indices: Vec::new(),
                            });
                            parts.len() - 1
                        }
                    };
                    current = Some(part);
                    let part = &mut parts[part];
                    let mut mapped = Vec::with_capacity(count);
                    for &(position, uv) in &corners {
                        let key = (position, if has_uv { uv } else { None });
                        let at = match part.map.get(&key) {
                            Some(&at) => at,
                            None => {
                                output_vertices += 1;
                                if output_vertices > MAX_OUTPUT {
                                    return Err("UV-seam vertex limit exceeded".into());
                                }
                                let at = part.positions.len() as u32;
                                part.map.insert(key, at);
                                part.positions.push(vertices[position]);
                                if let Some(uv) = key.1 {
                                    part.uvs.push(coords[uv]);
                                }
                                at
                            }
                        };
                        mapped.push(at);
                    }
                    part.indices.extend(local.iter().map(|&i| mapped[i]));
                }
                _ => {} // s, usemtl, mtllib, l, p and unknown statements are ignored
            }
            Ok(())
        })();
        result.map_err(err)?;
    }
    if parts.is_empty() || triangles == 0 {
        return Err("OBJ contains no supported polygon faces".into());
    }
    let (mut min, mut max) = ([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]);
    for p in parts.iter().flat_map(|p| &p.positions) {
        for k in 0..3 {
            min[k] = min[k].min(p[k]);
            max[k] = max[k].max(p[k]);
        }
    }
    let meshes = parts
        .into_iter()
        .map(|p| Mesh {
            name: p.name,
            positions: p.positions,
            indices: p.indices,
            uvs: p.has_uv.then_some(p.uvs),
            colors: None,
            shader: None,
            palette: None,
        })
        .collect();
    Ok(Model {
        name: safe_name(name, "Imported OBJ"),
        meshes,
        min,
        max,
        fragment: false,
        dictionary_child: false,
        bones: false,
        drawable: 0,
        declared: (min, max),
        note: Some("Imported OBJ geometry; external material libraries and texture paths are not loaded."),
        materials: None,
        embedded: None,
        palettes: Vec::new(),
    })
}

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn dot(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn triangulate(points: &[V3], operations: &mut u64) -> Result<Vec<usize>, String> {
    let n = points.len();
    let mut normal = [0f32; 3];
    let extent = points.iter().map(|&p| dot(sub(p, points[0]), sub(p, points[0])).sqrt()).fold(0f32, f32::max);
    for i in 0..n {
        let c = cross(sub(points[i], points[0]), sub(points[(i + 1) % n], points[0]));
        normal = [normal[0] + c[0], normal[1] + c[1], normal[2] + c[2]];
    }
    let len2 = dot(normal, normal);
    if extent <= 0.0 || len2 < extent.powi(4) * 1e-14 {
        return Err("degenerate polygon".into());
    }
    let normal = normal.map(|c| c / len2.sqrt());
    if points.iter().any(|&p| dot(sub(p, points[0]), normal).abs() > extent * 0.00001) {
        return Err("nonplanar polygon cannot be triangulated faithfully".into());
    }
    let [ax, ay, az] = normal.map(f32::abs);
    let axis = if ax > ay { if ax > az { 0 } else { 2 } } else if ay > az { 1 } else { 2 };
    let q: Vec<[f64; 2]> = points
        .iter()
        .map(|p| match axis {
            0 => [p[1] as f64, p[2] as f64],
            1 => [p[0] as f64, p[2] as f64],
            _ => [p[0] as f64, p[1] as f64],
        })
        .collect();
    let epsilon = (extent as f64 * extent as f64 * 1e-12).max(1e-24);
    let cr = |a: usize, b: usize, c: usize| (q[b][0] - q[a][0]) * (q[c][1] - q[a][1]) - (q[b][1] - q[a][1]) * (q[c][0] - q[a][0]);
    let area: f64 = (1..n - 1).map(|i| cr(0, i, i + 1)).sum();
    if area.abs() <= epsilon {
        return Err("degenerate projected polygon".into());
    }
    let sign = if area > 0.0 { 1.0 } else { -1.0 };
    let limit = |ops: &mut u64| {
        *ops += 1;
        if *ops > 50_000_000 { Err("polygon processing limit exceeded".to_owned()) } else { Ok(()) }
    };
    for a in 0..n {
        for b in a + 1..n {
            limit(operations)?;
            let (aa, bb) = ((a + 1) % n, (b + 1) % n);
            if a == bb || aa == b {
                continue;
            }
            let (c1, c2, c3, c4) = (cr(a, aa, b), cr(a, aa, bb), cr(b, bb, a), cr(b, bb, aa));
            let disjoint = q[a][0].max(q[aa][0]) < q[b][0].min(q[bb][0])
                || q[b][0].max(q[bb][0]) < q[a][0].min(q[aa][0])
                || q[a][1].max(q[aa][1]) < q[b][1].min(q[bb][1])
                || q[b][1].max(q[bb][1]) < q[a][1].min(q[aa][1]);
            if !disjoint && c1 * c2 <= 0.0 && c3 * c4 <= 0.0 {
                return Err("self-intersecting polygon".into());
            }
        }
    }
    let mut remaining: Vec<usize> = (0..n).collect();
    let mut out = Vec::with_capacity((n - 2) * 3);
    while remaining.len() > 3 {
        let r = remaining.len();
        let mut found = None;
        'ears: for i in 0..r {
            let (a, b, c) = (remaining[(i + r - 1) % r], remaining[i], remaining[(i + 1) % r]);
            if cr(a, b, c) * sign <= epsilon {
                continue;
            }
            for &d in &remaining {
                limit(operations)?;
                if d == a || d == b || d == c {
                    continue;
                }
                if cr(a, b, d) * sign >= -epsilon && cr(b, c, d) * sign >= -epsilon && cr(c, a, d) * sign >= -epsilon {
                    continue 'ears;
                }
            }
            found = Some((i, [a, b, c]));
            break;
        }
        let (i, tri) = found.ok_or("polygon has no supported nondegenerate triangulation")?;
        out.extend(tri);
        remaining.remove(i);
    }
    if cr(remaining[0], remaining[1], remaining[2]) * sign <= epsilon {
        return Err("degenerate final triangle".into());
    }
    out.extend(remaining);
    Ok(out)
}
