//! GTA V (legacy PC) .ybn collision writer: a port of CodeWalker's
//! BoundGeometry / BoundBVH / BoundComposite writing path (Bounds.cs),
//! including its BVHBuilder, edge (neighbour) indices and quantization.

use super::rsc::{BlockId, Builder, Seg, W};

type V3 = [f32; 3];

const VFT_BVH: u32 = 1080228536;
const VFT_COMPOSITE: u32 = 1080212136;
const TYPE_BVH: u8 = 8;
const TYPE_COMPOSITE: u8 = 10;
const MARGIN: f32 = 0.04;
/// Per-geometry limits: vertex indices are 15 bits; BVH item ids and node
/// indices are i16; edge indices u16 with 0xFFFF as "none".
const MAX_VERTS: usize = 32000;
const MAX_POLYS: usize = 32000;
/// Composite child flags (static map collision: collides with everything).
const COMP_FLAGS1: u32 = 0x3E; // MAP_WEAPON|MAP_DYNAMIC|MAP_ANIMAL|MAP_COVER|MAP_VEHICLE
const COMP_FLAGS2: u32 = (1 << 6) | (1 << 7) | (1 << 9) | (1 << 10) | (1 << 11) | (1 << 12) | (1 << 13) | (1 << 15) | (1 << 16) | (1 << 17)
    | (1 << 19) | (1 << 20) | (1 << 21) | (1 << 22) | (1 << 23) | (1 << 24) | (1 << 25) | (1 << 26);

fn vmin(a: V3, b: V3) -> V3 {
    [a[0].min(b[0]), a[1].min(b[1]), a[2].min(b[2])]
}
fn vmax(a: V3, b: V3) -> V3 {
    [a[0].max(b[0]), a[1].max(b[1]), a[2].max(b[2])]
}
fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn scale(a: V3, s: f32) -> V3 {
    [a[0] * s, a[1] * s, a[2] * s]
}
fn len(a: V3) -> f32 {
    (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt()
}
/// C# (short)float cast after clamping (truncates toward zero).
fn q16(v: f32) -> i16 {
    if v.is_nan() {
        0
    } else {
        v.clamp(-32767.0, 32767.0) as i16
    }
}

// ---------------------------------------------------------------- BVH builder

#[derive(Clone, Copy)]
struct Item {
    min: V3,
    max: V3,
    index: usize,
}

struct Node {
    children: Vec<Node>,
    items: Option<Vec<Item>>,
    min: V3,
    max: V3,
    index: usize,
}

impl Node {
    fn total_nodes(&self) -> usize {
        1 + self.children.iter().map(|c| c.total_nodes()).sum::<usize>()
    }
    fn total_items(&self) -> usize {
        self.items.as_ref().map_or(0, |i| i.len()) + self.children.iter().map(|c| c.total_items()).sum::<usize>()
    }
    fn update_min_max(&mut self) {
        let mut mn = [f32::MAX; 3];
        let mut mx = [f32::MIN; 3];
        if let Some(items) = &self.items {
            for it in items {
                mn = vmin(mn, it.min);
                mx = vmax(mx, it.max);
            }
        }
        for c in &mut self.children {
            c.update_min_max();
            mn = vmin(mn, c.min);
            mx = vmax(mx, c.max);
        }
        self.min = mn;
        self.max = mx;
    }
    fn build(&mut self, threshold: usize) {
        self.update_min_max();
        let items = match &self.items {
            Some(i) if i.len() > threshold => i.clone(),
            _ => return,
        };
        let mut avgsum = [0f32; 3];
        for it in &items {
            avgsum = add(avgsum, it.min);
            avgsum = add(avgsum, it.max);
        }
        let avg = scale(avgsum, 0.5 / items.len() as f32);
        let mut cnt = [0usize; 3];
        for it in &items {
            let c = scale(add(it.min, it.max), 0.5);
            for k in 0..3 {
                if c[k] < avg[k] {
                    cnt[k] += 1;
                }
            }
        }
        let target = items.len() as f32 / 2.0;
        let d: Vec<f32> = cnt.iter().map(|&c| (target - c as f32).abs()).collect();
        let axis = if d[0] <= d[1] && d[0] <= d[2] { 0 } else if d[1] <= d[2] { 1 } else { 2 };
        let (mut l1, mut l2) = (Vec::new(), Vec::new());
        for it in &items {
            let c = scale(add(it.min, it.max), 0.5);
            if c[axis] > avg[axis] {
                l1.push(*it)
            } else {
                l2.push(*it)
            }
        }
        if l1.is_empty() || l2.is_empty() {
            let mut l3: Vec<Item> = l1.into_iter().chain(l2).collect();
            let key = |a: &Item| [a.min[0], a.min[1], a.min[2], a.max[0], a.max[1], a.max[2]];
            l3.sort_by(|a, b| key(a).partial_cmp(&key(b)).unwrap_or(std::cmp::Ordering::Equal));
            let h = l3.len() / 2;
            l2 = l3.split_off(h);
            l1 = l3;
        }
        self.items = None;
        let mut n1 = Node { children: vec![], items: Some(l1), min: [0.0; 3], max: [0.0; 3], index: 0 };
        n1.build(threshold);
        let mut n2 = Node { children: vec![], items: Some(l2), min: [0.0; 3], max: [0.0; 3], index: 0 };
        n2.build(threshold);
        if n2.total_items() > n1.total_items() {
            self.children = vec![n2, n1];
        } else {
            self.children = vec![n1, n2];
        }
    }
    fn gather_nodes<'a>(&'a mut self, out: &mut Vec<*const Node>) {
        self.index = out.len();
        out.push(self as *const Node);
        for c in &mut self.children {
            c.gather_nodes(out);
        }
    }
    fn gather_trees(&self, out: &mut Vec<(V3, V3, usize, usize)>) {
        let tn = self.total_nodes();
        if tn > 127 && !self.children.is_empty() {
            for c in &self.children {
                c.gather_trees(out);
            }
        } else {
            out.push((self.min, self.max, self.index, tn));
        }
    }
}

struct Bvh {
    min: V3,
    max: V3,
    cen: V3,
    quantum: V3,
    qinv: V3,
    nodes: Vec<[i16; 8]>,
    node_count: usize,
    trees: Vec<[i16; 8]>,
}

/// BVHBuilder.Build. Returns the BVH and (for threshold > 1) the new item
/// order (original indices), grouped per leaf.
fn build_bvh(items: &[Item], threshold: usize) -> (Bvh, Vec<usize>) {
    let mut mn = [f32::MAX; 3];
    let mut mx = [f32::MIN; 3];
    for it in items {
        mn = vmin(mn, it.min);
        mx = vmax(mx, it.max);
    }
    let cen = scale(add(mn, mx), 0.5);
    let a = sub(mn, cen);
    let b = sub(mx, cen);
    let quantum = [a[0].abs().max(b[0].abs()) / 32767.0, a[1].abs().max(b[1].abs()) / 32767.0, a[2].abs().max(b[2].abs()) / 32767.0];
    let qinv = [1.0 / quantum[0], 1.0 / quantum[1], 1.0 / quantum[2]];

    let mut root = Node { children: vec![], items: Some(items.to_vec()), min: [0.0; 3], max: [0.0; 3], index: 0 };
    root.build(threshold);
    let mut nodes: Vec<*const Node> = Vec::new();
    root.gather_nodes(&mut nodes);
    let mut trees = Vec::new();
    root.gather_trees(&mut trees);
    // SAFETY: the tree is not mutated while these pointers are used.
    let nodes: Vec<&Node> = nodes.iter().map(|&p| unsafe { &*p }).collect();

    let mut order = Vec::new();
    let mut leaf_first = vec![0usize; nodes.len()];
    if threshold > 1 {
        for (ni, n) in nodes.iter().enumerate() {
            if let Some(its) = &n.items {
                leaf_first[ni] = order.len();
                order.extend(its.iter().map(|i| i.index));
            }
        }
    } else {
        order = items.iter().map(|i| i.index).collect();
        for (ni, n) in nodes.iter().enumerate() {
            if let Some(its) = &n.items {
                leaf_first[ni] = its.first().map_or(0, |i| i.index);
            }
        }
    }

    let qv = |v: V3| -> [i16; 3] {
        let d = sub(v, cen);
        [q16(d[0] * qinv[0]), q16(d[1] * qinv[1]), q16(d[2] * qinv[2])]
    };
    let mut bn = Vec::with_capacity(nodes.len());
    for (ni, n) in nodes.iter().enumerate() {
        let tn = n.total_nodes();
        let has_items = n.items.as_ref().map_or(false, |i| !i.is_empty());
        let id = if has_items { leaf_first[ni] } else { 0 };
        let a = qv(n.min);
        let b = qv(n.max);
        let count = if tn <= 1 { n.total_items() } else { 0 };
        let item_id = if tn <= 1 { id } else { tn };
        bn.push([a[0], a[1], a[2], b[0], b[1], b[2], item_id as i16, count as i16]);
    }
    let mut bt = Vec::new();
    for &(tmin, tmax, idx, tn) in &trees {
        let a = qv(tmin);
        let b = qv(tmax);
        bt.push([a[0], a[1], a[2], b[0], b[1], b[2], idx as i16, (idx + tn) as i16]);
    }
    let node_count = bn.len();
    if threshold <= 1 {
        let cap = items.len() * 2 + 1;
        while bn.len() < cap {
            bn.push([0, 0, 0, 0, 0, 0, 1, 0]);
        }
    }
    (Bvh { min: mn, max: mx, cen, quantum, qinv, nodes: bn, node_count, trees: bt }, order)
}

fn write_bvh(b: &mut Builder, bvh: &Bvh) -> BlockId {
    let mut nw = W::new();
    for n in &bvh.nodes {
        for &v in n {
            nw.u16(v as u16);
        }
    }
    let nodes = b.add(Seg::Sys, nw);
    let mut tw = W::new();
    for t in &bvh.trees {
        for &v in t {
            tw.u16(v as u16);
        }
    }
    let trees = b.add(Seg::Sys, tw);
    let nan = f32::NAN;
    let mut w = W::new();
    w.ptr(Some(nodes)).u32(bvh.node_count as u32).u32(bvh.nodes.len() as u32);
    w.zeros(16);
    w.v3(bvh.min).f32(nan).v3(bvh.max).f32(nan).v3(bvh.cen).f32(nan).v3(bvh.qinv).f32(nan).v3(bvh.quantum).f32(nan);
    w.ptr(Some(trees)).u16(bvh.trees.len() as u16).u16(bvh.trees.len() as u16).u32(0);
    b.add(Seg::Sys, w)
}

// ---------------------------------------------------------------- geometry

struct BoundsHdr {
    vft: u32,
    ty: u8,
    min: V3,
    max: V3,
    margin: f32,
}

fn write_bounds_hdr(w: &mut W, h: &BoundsHdr) {
    let cen = scale(add(h.min, h.max), 0.5);
    let rad = len(sub(h.max, cen));
    w.u32(h.vft).u32(1).u64(0);
    w.u8(h.ty).u8(0).u16(0).f32(rad).u32(0).u32(0);
    w.v3(h.max).f32(h.margin).v3(h.min).u32(1); // UnkType 1 (vanilla map ybns; 0 lets peds walk through)
    w.v3(cen).u8(0).u8(0).u8(0).u8(0);
    w.v3(cen).u8(0).u8(0).u16(0);
    w.v3([1.0, 1.0, 1.0]).f32(1.0); // inertia, volume
}

/// One BoundGeometryBVH from local verts/tris (all indices valid, < MAX_VERTS).
/// Returns (block id, world box min, box max).
fn write_geometry(b: &mut Builder, id: BlockId, verts: &[V3], tris: &[[u32; 3]], tri_mats: &[u32]) -> (V3, V3) {
    // Centre of the vertex bounding box (GeometryCenter in the XML pipeline).
    let mut vmn = [f32::MAX; 3];
    let mut vmx = [f32::MIN; 3];
    for v in verts {
        vmn = vmin(vmn, *v);
        vmx = vmax(vmx, *v);
    }
    let center = scale(add(vmn, vmx), 0.5);
    let local: Vec<V3> = verts.iter().map(|v| sub(*v, center)).collect();
    let world = |i: u32| add(local[i as usize], center);

    // BuildBVH: items are polygon AABBs (world space), threshold 4.
    let items: Vec<Item> = tris
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let (a, bb, c) = (world(t[0]), world(t[1]), world(t[2]));
            Item { min: vmin(vmin(a, bb), c), max: vmax(vmax(a, bb), c), index: i }
        })
        .collect();
    let (bvh, order) = build_bvh(&items, 4);
    let polys: Vec<[u32; 3]> = order.iter().map(|&i| tris[i]).collect();
    // material list (first-use order) and per-polygon index into it
    let mut mat_list: Vec<u32> = Vec::new();
    let poly_mi: Vec<u8> = order
        .iter()
        .map(|&i| {
            let m = tri_mats[i];
            (match mat_list.iter().position(|&x| x == m) {
                Some(k) => k,
                None => {
                    mat_list.push(m);
                    mat_list.len() - 1
                }
            }) as u8
        })
        .collect();
    let (bmin, bmax) = (bvh.min, bvh.max);

    // CalculateQuantum (from the BVH-updated box).
    let mut quantum = scale(sub(bmax, bmin), 0.5 / 32767.0);
    for q in &mut quantum {
        if !(*q > 0.0) {
            *q = 1e-6;
        }
    }

    // UpdateEdgeIndices on the final polygon order.
    let mut edges: Vec<[u16; 3]> = vec![[0xFFFF; 3]; polys.len()];
    {
        use std::collections::HashMap;
        // edge -> (tri1, edge1, Option<(tri2, edge2)>)
        let mut dict: HashMap<(u32, u32), (usize, usize, Option<(usize, usize)>)> = HashMap::with_capacity(polys.len() * 2);
        let pack = |i: usize| if i > 0xFFFF { 0xFFFF } else { i as u16 };
        for (pi, t) in polys.iter().enumerate() {
            for e in 0..3 {
                let (a, c) = (t[e], t[(e + 1) % 3]);
                let key = (a.min(c), a.max(c));
                match dict.get_mut(&key) {
                    Some(ent) => {
                        if ent.2.is_some() {
                            edges[pi][e] = pack(ent.0);
                        } else {
                            ent.2 = Some((pi, e));
                        }
                    }
                    None => {
                        dict.insert(key, (pi, e, None));
                    }
                }
            }
        }
        for (_, (t1, e1, o)) in dict {
            match o {
                None => edges[t1][e1] = 0xFFFF,
                Some((t2, e2)) => {
                    edges[t1][e1] = pack(t2);
                    edges[t2][e2] = pack(t1);
                }
            }
        }
    }

    // Polygons block (triangle type 0 lives in the low 3 bits of the area).
    let mut pw = W::new();
    for (pi, t) in polys.iter().enumerate() {
        let (a, bb, c) = (world(t[0]), world(t[1]), world(t[2]));
        let u = sub(bb, a);
        let v = sub(c, a);
        let cr = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
        let area = 0.5 * len(cr);
        pw.u32(area.to_bits() & !7);
        pw.u16((t[0] & 0x7FFF) as u16).u16((t[1] & 0x7FFF) as u16).u16((t[2] & 0x7FFF) as u16);
        pw.u16(edges[pi][0]).u16(edges[pi][1]).u16(edges[pi][2]);
    }
    let polys_b = b.add(Seg::Sys, pw);

    let mut vw = W::new();
    for v in &local {
        vw.u16(q16(v[0] / quantum[0]) as u16).u16(q16(v[1] / quantum[1]) as u16).u16(q16(v[2] / quantum[2]) as u16);
    }
    let verts_b = b.add(Seg::Sys, vw);

    let mut mw = W::new();
    for m in &mat_list {
        mw.u32(*m).u32(0);
    }
    mw.zeros(8 * 4usize.saturating_sub(mat_list.len())); // padded to 4 materials
    let mats_b = b.add(Seg::Sys, mw);
    let pmi_b = b.sys_bytes(poly_mi);
    let bvh_b = write_bvh(b, &bvh);

    let mut w = W::new();
    write_bounds_hdr(&mut w, &BoundsHdr { vft: VFT_BVH, ty: TYPE_BVH, min: bmin, max: bmax, margin: MARGIN });
    let nv = local.len() as u32;
    w.u32(0).u32(0).ptr(None).u16(0).u16(0).u32(nv);
    w.ptr(Some(polys_b)).v3(quantum).f32(0.0).v3(center).f32(0.0);
    w.ptr(Some(verts_b)).ptr(None).ptr(None).ptr(None);
    w.u32(nv).u32(polys.len() as u32).zeros(24);
    w.ptr(Some(mats_b)).ptr(None).zeros(24);
    w.ptr(Some(pmi_b)).u8(mat_list.len() as u8).u8(0).u16(0).u32(0).u32(0).u32(0);
    w.ptr(Some(bvh_b)).u32(0).u32(0).u16(0xFFFF).u16(0).u32(0).u32(0).u32(0);
    debug_assert_eq!(w.len(), 336);
    b.set(id, w);
    (bmin, bmax)
}

// ---------------------------------------------------------------- splitting

/// Split triangle ids spatially (median on the longest axis of centroids)
/// until each chunk fits one geometry.
fn split(verts: &[V3], tris: &[[u32; 3]], ids: Vec<usize>, out: &mut Vec<Vec<usize>>) {
    let nverts = {
        let mut s: Vec<u32> = ids.iter().flat_map(|&i| tris[i]).collect();
        s.sort_unstable();
        s.dedup();
        s.len()
    };
    if (ids.len() <= MAX_POLYS && nverts <= MAX_VERTS) || ids.len() < 2 {
        out.push(ids);
        return;
    }
    let cen = |i: usize| {
        let t = tris[i];
        scale(add(add(verts[t[0] as usize], verts[t[1] as usize]), verts[t[2] as usize]), 1.0 / 3.0)
    };
    let mut mn = [f32::MAX; 3];
    let mut mx = [f32::MIN; 3];
    for &i in &ids {
        let c = cen(i);
        mn = vmin(mn, c);
        mx = vmax(mx, c);
    }
    let e = sub(mx, mn);
    let ax = if e[0] >= e[1] && e[0] >= e[2] { 0 } else if e[1] >= e[2] { 1 } else { 2 };
    let mut ids = ids;
    ids.sort_by(|&a, &b| cen(a)[ax].partial_cmp(&cen(b)[ax]).unwrap_or(std::cmp::Ordering::Equal));
    let right = ids.split_off(ids.len() / 2);
    split(verts, tris, ids, out);
    split(verts, tris, right, out);
}

/// Build a .ybn: one BoundGeometryBVH, or a BoundComposite of several when
/// the mesh exceeds one geometry's limits. `material` is the material type.
pub fn build_ybn(verts: &[[f32; 3]], tris: &[[u32; 3]], material: u8) -> Result<Vec<u8>, String> {
    build_ybn_mats(verts, tris, &vec![material; tris.len()])
}

/// As [`build_ybn`] with one GTA V material index per triangle
/// (materials.dat order; at most 255 distinct per geometry).
pub fn build_ybn_mats(verts: &[[f32; 3]], tris: &[[u32; 3]], tri_mats: &[u8]) -> Result<Vec<u8>, String> {
    build_ybn_words(verts, tris, &tri_mats.iter().map(|&m| m as u32).collect::<Vec<_>>())
}

/// BoundMaterial data word (low u32): material type in bits 0-7, room id
/// (MLO room index, 0 = limbo) in bits 16-20.
pub fn material_word(material: u8, room: u8) -> u32 {
    material as u32 | ((room as u32 & 31) << 16)
}

/// As [`build_ybn_mats`] with full BoundMaterial words per triangle
/// (see [`material_word`]).
pub fn build_ybn_words(verts: &[[f32; 3]], tris: &[[u32; 3]], tri_mats: &[u32]) -> Result<Vec<u8>, String> {
    if tri_mats.len() != tris.len() {
        return Err("material count != triangle count".into());
    }
    let n = verts.len() as u32;
    let (tris, mats): (Vec<[u32; 3]>, Vec<u32>) = tris
        .iter()
        .copied()
        .zip(tri_mats.iter().copied())
        .filter(|(t, _)| t[0] != t[1] && t[1] != t[2] && t[0] != t[2])
        .unzip();
    if let Some(t) = tris.iter().find(|t| t.iter().any(|&i| i >= n)) {
        return Err(format!("triangle index out of range: {:?} (verts {})", t, n));
    }
    if tris.is_empty() {
        return Err("no triangles".into());
    }
    if verts.iter().any(|v| v.iter().any(|c| !c.is_finite())) {
        return Err("non-finite vertex".into());
    }
    let mut chunks = Vec::new();
    split(verts, &tris, (0..tris.len()).collect(), &mut chunks);

    // Remap each chunk to local vertex arrays (first-use order).
    let local: Vec<(Vec<V3>, Vec<[u32; 3]>, Vec<u32>)> = chunks
        .iter()
        .map(|ids| {
            let mut map = std::collections::HashMap::new();
            let mut lv = Vec::new();
            let lt = ids
                .iter()
                .map(|&i| {
                    let t = tris[i];
                    let mut o = [0u32; 3];
                    for k in 0..3 {
                        o[k] = *map.entry(t[k]).or_insert_with(|| {
                            lv.push(verts[t[k] as usize]);
                            (lv.len() - 1) as u32
                        });
                    }
                    o
                })
                .collect();
            (lv, lt, ids.iter().map(|&i| mats[i]).collect())
        })
        .collect();

    // Always a BoundComposite root, even for one geometry: loose static
    // ybns need the composite child flags or peds pass through.
    let local = if local.len() == 1 { vec![(verts.to_vec(), tris.clone(), mats.clone())] } else { local };
    let mut b = Builder::new();

    let root = b.reserve(Seg::Sys);
    let kids: Vec<BlockId> = local.iter().map(|_| b.reserve(Seg::Sys)).collect();
    let mut boxes = Vec::new();
    for (k, (lv, lt, lm)) in kids.iter().zip(&local) {
        boxes.push(write_geometry(&mut b, *k, lv, lt, lm));
    }
    let cn = kids.len();
    let mut cw = W::new();
    for k in &kids {
        cw.ptr(Some(*k));
    }
    let children_b = b.add(Seg::Sys, cw);
    let mut tw = W::new();
    for _ in 0..cn {
        tw.v3([1.0, 0.0, 0.0]).u32(0).v3([0.0, 1.0, 0.0]).u32(1).v3([0.0, 0.0, 1.0]).u32(1).v3([0.0; 3]).u32(0);
    }
    let tr_b = b.add(Seg::Sys, tw);
    let mut bw = W::new();
    for (mn, mx) in &boxes {
        bw.v3(*mn).u32(1).v3(*mx).f32(MARGIN); // min.w = float.Epsilon
    }
    let bb_b = b.add(Seg::Sys, bw);
    let flags = |b: &mut Builder| {
        let mut fw = W::new();
        for _ in 0..cn {
            fw.u32(COMP_FLAGS1).u32(COMP_FLAGS2);
        }
        b.add(Seg::Sys, fw)
    };
    let f1 = flags(&mut b);
    let f2 = flags(&mut b);
    let bvh_b = if cn > 5 {
        let items: Vec<Item> = boxes.iter().enumerate().map(|(i, (mn, mx))| Item { min: *mn, max: *mx, index: i }).collect();
        let (bvh, _) = build_bvh(&items, 1);
        Some(write_bvh(&mut b, &bvh))
    } else {
        None
    };
    let mut mn = [f32::MAX; 3];
    let mut mx = [f32::MIN; 3];
    for (a, c) in &boxes {
        mn = vmin(mn, *a);
        mx = vmax(mx, *c);
    }
    let mut w = W::new();
    write_bounds_hdr(&mut w, &BoundsHdr { vft: VFT_COMPOSITE, ty: TYPE_COMPOSITE, min: mn, max: mx, margin: MARGIN });
    w.ptr(Some(children_b)).ptr(Some(tr_b)).ptr(Some(tr_b)).ptr(Some(bb_b)).ptr(Some(f1)).ptr(Some(f2));
    w.u16(cn as u16).u16(cn as u16).u32(0).ptr(bvh_b);
    debug_assert_eq!(w.len(), 176);
    b.set(root, w);
    b.build(43)
}

// ---------------------------------------------------------------- OBJ / CLI

pub fn read_obj(text: &str) -> Result<(Vec<V3>, Vec<[u32; 3]>), String> {
    let mut v = Vec::new();
    let mut f = Vec::new();
    for (ln, line) in text.lines().enumerate() {
        let mut it = line.split_whitespace();
        match it.next() {
            Some("v") => {
                let c: Vec<f32> = it.take(3).map(|s| s.parse::<f32>()).collect::<Result<_, _>>().map_err(|e| format!("line {}: {}", ln + 1, e))?;
                if c.len() < 3 {
                    return Err(format!("line {}: bad vertex", ln + 1));
                }
                v.push([c[0], c[1], c[2]]);
            }
            Some("f") => {
                let mut idx = Vec::new();
                for s in it {
                    let i: i64 = s.split('/').next().unwrap_or("").parse().map_err(|e| format!("line {}: {}", ln + 1, e))?;
                    let i = if i < 0 { v.len() as i64 + i } else { i - 1 };
                    if i < 0 {
                        return Err(format!("line {}: bad index", ln + 1));
                    }
                    idx.push(i as u32);
                }
                for k in 1..idx.len().saturating_sub(1) {
                    f.push([idx[0], idx[k], idx[k + 1]]);
                }
            }
            _ => {}
        }
    }
    Ok((v, f))
}

pub fn cli(args: &[String]) -> Result<(), String> {
    if args.len() != 2 {
        return Err("usage: ybn <in.obj> <out.ybn>".into());
    }
    let text = std::fs::read_to_string(&args[0]).map_err(|e| format!("{}: {}", args[0], e))?;
    let (v, f) = read_obj(&text)?;
    let data = build_ybn(&v, &f, 0)?;
    std::fs::write(&args[1], &data).map_err(|e| format!("{}: {}", args[1], e))?;
    println!("ybn: {} verts, {} tris -> {} ({} bytes)", v.len(), f.len(), args[1], data.len());
    Ok(())
}

/// BVH for a BoundComposite with more than 5 children (one item per child
/// AABB, in composite space), as the ybn composite above builds it. Used by
/// the yft writer.
pub(crate) fn composite_bvh(b: &mut Builder, boxes: &[(V3, V3)]) -> BlockId {
    let items: Vec<Item> = boxes.iter().enumerate().map(|(i, (mn, mx))| Item { min: *mn, max: *mx, index: i }).collect();
    let (bvh, _) = build_bvh(&items, 1);
    write_bvh(b, &bvh)
}
