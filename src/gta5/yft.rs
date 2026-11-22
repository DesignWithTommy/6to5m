//! GTA V (legacy PC) .yft fragment writer for vehicles, ported from
//! CodeWalker (Frag.cs: FragType, FragDrawable, FragBoneTransforms,
//! FragPhysicsLODGroup, FragPhysicsLOD, FragPhysArchetype,
//! FragPhysTypeChild, FragPhysTypeGroup, FragPhysTransforms; Drawable.cs:
//! Skeleton, Bone, DrawableModelsBlock; Bounds.cs: BoundComposite, BoundBox),
//! with field defaults from Sollumz' sollumz_cube.yft.xml.
//!
//! Scope: one skinned-free drawable whose models are rigidly bound to bones
//! (model SkeletonBinding, vertices in bone space, at most 255 bones), up to
//! four LOD model lists, a box-per-child BoundComposite (at most 5 children:
//! no composite BVH), one physics LOD, child drawables that share the main
//! shader group (vehicle wheels). No glass windows, damage or articulation.

use super::rsc::{joaat, BlockId, Builder, Seg, W};
use super::ydr::Geom;

pub type V3 = [f32; 3];
/// Quaternion x, y, z, w.
pub type Q = [f32; 4];

const VFT_FRAG: u32 = 1079456040;
const VFT_FRAGDRAWABLE: u32 = 1080060872;
const VFT_SKELETON: u32 = 1080114336;
const VFT_SHADERGROUP: u32 = 1080113136;
const VFT_MODEL: u32 = 1080101528;
const VFT_GEOM: u32 = 1080133528;
const VFT_VB: u32 = 1080153080;
const VFT_IB: u32 = 1080152408;
const VFT_LODGROUP: u32 = 1080055472;
const VFT_LOD: u32 = 1080055512;
const VFT_ARCHETYPE: u32 = 1080215944;
const VFT_CHILD: u32 = 1080061712;
const VFT_TRANSFORMS: u32 = 1080043536;
const VFT_GROUPNAMES: u32 = 1095046985;
const VFT_COMPOSITE: u32 = 1080212136;
const VFT_BOX: u32 = 1080221016;
const GTAV1: u64 = 0x7755555555996996;
const LAYOUT_FLAGS: u32 = 1 | (1 << 3) | (1 << 4) | (1 << 6);
const STRIDE: usize = 36;
const MARGIN: f32 = 0.04;
const NAN_BITS: u32 = 0x7f800001;
const QNAN_BITS: u32 = 0x7fc00001;

/// Composite child flags: vehicle body (type) colliding with the world,
/// peds, objects and other vehicles (include).
const VEH_TYPE: u32 = 1 << 6; // VEHICLE_NOT_BVH
const VEH_INCLUDE: u32 = (1 << 1) | (1 << 2) | (1 << 3) | (1 << 4) | (1 << 5) | (1 << 6) | (1 << 7) | (1 << 9) | (1 << 10) | (1 << 11) | (1 << 12)
    | (1 << 13) | (1 << 15) | (1 << 16) | (1 << 17) | (1 << 20) | (1 << 21) | (1 << 22) | (1 << 23) | (1 << 24) | (1 << 27);

const VECTORS: [(&str, [f32; 4]); 6] = [
    ("matMaterialColorScale", [1.0, 0.0, 0.0, 1.0]),
    ("HardAlphaBlend", [1.0, 0.0, 0.0, 0.0]),
    ("useTessellation", [0.0, 0.0, 0.0, 0.0]),
    ("wetnessMultiplier", [1.0, 0.0, 0.0, 0.0]),
    ("globalAnimUV1", [0.0, 1.0, 0.0, 0.0]),
    ("globalAnimUV0", [1.0, 0.0, 0.0, 0.0]),
];

#[derive(Clone, Debug)]
pub struct Bone {
    pub name: String,
    pub parent: Option<usize>,
    /// Local (parent space) transform.
    pub t: V3,
    pub r: Q,
}

/// Geometries rigidly bound to one bone, in that bone's space.
#[derive(Clone, Debug, Default)]
pub struct Model {
    pub bone: usize,
    pub geoms: Vec<Geom>,
}

/// One physics child: a box in bone space, its group and optional drawable
/// (models in bone space, bound to bone 0 of the child drawable).
#[derive(Clone, Debug)]
pub struct Child {
    pub bone: usize,
    pub group: usize,
    pub min: V3,
    pub max: V3,
    pub mass: f32,
    pub drawable: Vec<Geom>,
}

#[derive(Clone, Debug)]
pub struct Group {
    pub name: String,
    pub parent: Option<usize>,
}

#[derive(Clone, Debug, Default)]
pub struct Frag {
    pub name: String,
    pub bones: Vec<Bone>,
    /// High, medium, low, very low model lists (empty = absent).
    pub lods: [Vec<Model>; 4],
    pub lod_dist: [f32; 4],
    pub groups: Vec<Group>,
    pub children: Vec<Child>,
    pub mass: f32,
}

// ------------------------------------------------------------------ maths

fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn len(a: V3) -> f32 {
    (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt()
}
fn qmul(a: Q, b: Q) -> Q {
    [
        a[3] * b[0] + a[0] * b[3] + a[1] * b[2] - a[2] * b[1],
        a[3] * b[1] - a[0] * b[2] + a[1] * b[3] + a[2] * b[0],
        a[3] * b[2] + a[0] * b[1] - a[1] * b[0] + a[2] * b[3],
        a[3] * b[3] - a[0] * b[0] - a[1] * b[1] - a[2] * b[2],
    ]
}
pub fn qrot(q: Q, v: V3) -> V3 {
    let r = qmul(qmul(q, [v[0], v[1], v[2], 0.0]), [-q[0], -q[1], -q[2], q[3]]);
    [r[0], r[1], r[2]]
}
fn qconj(q: Q) -> Q {
    [-q[0], -q[1], -q[2], q[3]]
}
fn qnorm(q: Q) -> Q {
    let l = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
    if l > 1e-6 { q.map(|x| x / l) } else { [0.0, 0.0, 0.0, 1.0] }
}
/// Rotation axes (images of X, Y, Z).
fn axes(q: Q) -> [V3; 3] {
    [qrot(q, [1.0, 0.0, 0.0]), qrot(q, [0.0, 1.0, 0.0]), qrot(q, [0.0, 0.0, 1.0])]
}

/// Model-space (global) transform of every bone.
pub fn globals(bones: &[Bone]) -> Vec<(V3, Q)> {
    let mut g: Vec<(V3, Q)> = Vec::with_capacity(bones.len());
    for b in bones {
        let r = qnorm(b.r);
        g.push(match b.parent {
            Some(p) if p < g.len() => {
                let (pt, pr) = g[p];
                (add(pt, qrot(pr, b.t)), qnorm(qmul(pr, r)))
            }
            _ => (b.t, r),
        });
    }
    g
}

/// Model space -> bone space.
pub fn to_local(g: (V3, Q), p: V3) -> V3 {
    qrot(qconj(g.1), sub(p, g.0))
}

/// CodeWalker's CalculateBoneHash (ElfHash of the upper-cased name).
pub fn bone_tag(name: &str) -> u16 {
    let mut h: u32 = 0;
    for c in name.bytes() {
        let c = c.to_ascii_uppercase() as u32;
        h = (h << 4).wrapping_add(c);
        let x = h & 0xF000_0000;
        if x != 0 {
            h ^= x >> 24;
        }
        h &= !x;
    }
    (h % 0xFE8F + 0x170) as u16
}

fn tags(bones: &[Bone]) -> Vec<u16> {
    bones.iter().enumerate().map(|(i, b)| if i == 0 { 0 } else { bone_tag(&b.name) }).collect()
}

fn pad16(o: usize) -> usize {
    (16 - o % 16) % 16
}

fn aabb(pts: impl Iterator<Item = V3>) -> (V3, V3) {
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

/// Matrix4F_s / SharpDX Matrix memory order: X, Y, Z axes then translation,
/// each followed by a w word.
fn mat(w: &mut W, g: (V3, Q), ws: [u32; 4]) {
    let a = axes(g.1);
    for k in 0..3 {
        w.v3(a[k]).u32(ws[k]);
    }
    w.v3(g.0).u32(ws[3]);
}

// ------------------------------------------------------------- drawables

fn add_texture_ref(b: &mut Builder, name: &str) -> BlockId {
    let nm = b.string(name);
    let mut w = W::new();
    w.u32(0).u32(1).zeros(32);
    w.ptr(Some(nm));
    w.u16(1).u16(2).u32(0).u32(0).u32(0);
    w.u32(0).u32(0).u32(0).u32(0);
    b.add(Seg::Sys, w)
}

fn add_shader(b: &mut Builder, texture: &str, kind: u8) -> BlockId {
    let (sname, bucket) = match kind {
        1 => ("alpha", 1u8),
        2 => ("decal", 2),
        3 => ("cutout", 3),
        _ => ("default", 0),
    };
    let tex = add_texture_ref(b, texture);
    let n = 1 + VECTORS.len();
    let base_size = 32 + 16 * n + 16 * VECTORS.len() + 4 * n;
    let data_size = base_size + pad16(base_size);
    let pb = b.reserve(Seg::Sys);
    let mut w = W::new();
    w.u8(0).u8(2).u16(0).u32(0).ptr(Some(tex));
    let mut voff = 16 * n;
    for (i, _) in VECTORS.iter().enumerate() {
        let unk = 160 + (VECTORS.len() - 1 - i);
        w.u8(1).u8(unk as u8).u16(0).u32(0).ptr_at(Some(pb), voff as u64);
        voff += 16;
    }
    for (_, v) in VECTORS {
        w.v4(v);
    }
    w.u32(joaat("diffusesampler"));
    for (name, _) in VECTORS {
        w.u32(joaat(&name.to_ascii_lowercase()));
    }
    w.zeros(32 + data_size * 4);
    b.set(pb, w);
    let mut w = W::new();
    w.ptr(Some(pb));
    w.u32(joaat(sname)).u32(0);
    w.u8(n as u8).u8(bucket).u16(32768);
    w.u16((16 * n + 16 * VECTORS.len()) as u16).u16(data_size as u16);
    w.u32(joaat(&format!("{sname}.sps"))).u32(0);
    w.u32((1u32 << bucket) | 0xFF00).u16(0).u8(0).u8(1).u64(0);
    b.add(Seg::Sys, w)
}

fn write_geom(b: &mut Builder, w: &mut W, g: &Geom) -> Result<(), String> {
    let nv = g.positions.len();
    if nv > 65535 {
        return Err(format!("geometry has {nv} vertices (max 65535)"));
    }
    if g.normals.len() != nv || g.colours.len() != nv || g.uvs.len() != nv {
        return Err("geometry attribute arrays differ in length".into());
    }
    let mut vb = Vec::with_capacity(nv * STRIDE);
    for i in 0..nv {
        for f in g.positions[i].iter().chain(&g.normals[i]) {
            vb.extend_from_slice(&f.to_le_bytes());
        }
        vb.extend_from_slice(&g.colours[i].to_le_bytes());
        for f in &g.uvs[i] {
            vb.extend_from_slice(&f.to_le_bytes());
        }
    }
    let vdata = b.sys_bytes(vb);
    let mut d = W::new();
    d.u32(LAYOUT_FLAGS).u16(STRIDE as u16).u8(0).u8(4).u64(GTAV1);
    let decl = b.add(Seg::Sys, d);
    let mut v = W::new();
    v.u32(VFT_VB).u32(1).u16(STRIDE as u16).u16(0).u32(0);
    v.ptr(Some(vdata)).u32(nv as u32).u32(0).ptr(Some(vdata)).u64(0).ptr(Some(decl));
    v.zeros(128 - v.len());
    let vbuf = b.add(Seg::Sys, v);
    let mut ib = Vec::with_capacity(g.indices.len() * 2);
    for i in &g.indices {
        ib.extend_from_slice(&i.to_le_bytes());
    }
    let idata = b.sys_bytes(ib);
    let mut x = W::new();
    x.u32(VFT_IB).u32(1).u32(g.indices.len() as u32).u32(0).ptr(Some(idata));
    x.zeros(96 - x.len());
    let ibuf = b.add(Seg::Sys, x);
    let ni = g.indices.len() as u32;
    w.u32(VFT_GEOM).u32(1).u64(0).u64(0).ptr(Some(vbuf)).u64(0).u64(0).u64(0);
    w.ptr(Some(ibuf)).u64(0).u64(0).u64(0);
    w.u32(ni).u32(ni / 3).u16(nv as u16).u16(3).u32(0);
    w.u64(0).u16(STRIDE as u16).u16(0).u32(0);
    w.ptr(Some(vdata)).u64(0).u64(0).u64(0);
    Ok(())
}

/// A model to write: bone binding and (geometry, shader index) pairs.
struct M<'a> {
    bone: u8,
    geoms: Vec<(&'a Geom, u16)>,
}

/// Bytes of one DrawableModel (header, shader map, pointers, bounds, geoms).
fn model_len(n: usize) -> usize {
    let mut off = 48 + n * 2;
    off += if n == 1 { 6 } else { pad16(off) };
    off += n * 8;
    off += pad16(off);
    off += (n + usize::from(n > 1)) * 32;
    for _ in 0..n {
        off += pad16(off);
        off += 152;
    }
    off
}

fn write_model(b: &mut Builder, w: &mut W, id: BlockId, m: &M) -> Result<(), String> {
    let base = w.len();
    let n = m.geoms.len();
    let mut off = base + 48;
    let shmap = off;
    off += n * 2;
    off += if n == 1 { 6 } else { pad16(off - base) };
    let geoptrs = off;
    off += n * 8;
    off += pad16(off - base);
    let bounds = off;
    off += (n + usize::from(n > 1)) * 32;
    let mut gofs = Vec::with_capacity(n);
    for _ in 0..n {
        off += pad16(off - base);
        gofs.push(off);
        off += 152;
    }
    w.u32(VFT_MODEL).u32(1).ptr_at(Some(id), geoptrs as u64);
    w.u16(n as u16).u16(n as u16).u32(0);
    w.ptr_at(Some(id), bounds as u64).ptr_at(Some(id), shmap as u64);
    w.u32((m.bone as u32) << 24).u16(0x00FF).u16(n as u16);
    for (_, s) in &m.geoms {
        w.u16(*s);
    }
    if n == 1 {
        w.zeros(6);
    } else {
        w.zeros(pad16(w.len() - base));
    }
    for &g in &gofs {
        w.ptr_at(Some(id), g as u64);
    }
    w.zeros(pad16(w.len() - base));
    let boxes: Vec<(V3, V3)> = m.geoms.iter().map(|(g, _)| aabb(g.positions.iter().copied())).collect();
    let mut bl = Vec::new();
    if n > 1 {
        bl.push(aabb(boxes.iter().flat_map(|a| [a.0, a.1])));
    }
    bl.extend_from_slice(&boxes);
    for (lo, hi) in bl {
        w.v3(lo).f32(0.0).v3(hi).f32(0.0);
    }
    for (g, _) in &m.geoms {
        w.zeros(pad16(w.len() - base));
        write_geom(b, w, g)?;
    }
    debug_assert_eq!(w.len() - base, model_len(n));
    Ok(())
}

/// DrawableModelsBlock: one pointer list per LOD (CodeWalker layout).
/// Returns the block, each LOD's header offset and the block length.
fn add_models_block(b: &mut Builder, lods: &[Vec<M>; 4]) -> Result<(BlockId, [Option<u64>; 4], usize), String> {
    let id = b.reserve(Seg::Sys);
    let mut w = W::new();
    let mut heads = [None; 4];
    for (li, models) in lods.iter().enumerate() {
        if models.is_empty() {
            continue;
        }
        w.zeros(pad16(w.len()));
        let h = w.len();
        heads[li] = Some(h as u64);
        let n = models.len();
        // header + pointers, then each model 16-aligned
        let mut p = h + 16 + n * 8;
        let mut ptrs = Vec::with_capacity(n);
        for m in models {
            p += pad16(p);
            ptrs.push(p);
            p += model_len(m.geoms.len());
        }
        w.ptr_at(Some(id), (h + 16) as u64).u16(n as u16).u16(n as u16).u32(0);
        for &q in &ptrs {
            w.ptr_at(Some(id), q as u64);
        }
        for (m, &q) in models.iter().zip(&ptrs) {
            w.zeros(pad16(w.len()));
            debug_assert_eq!(w.len(), q);
            write_model(b, &mut w, id, m)?;
        }
    }
    let len = w.len();
    b.set(id, w);
    Ok((id, heads, len))
}

struct DrawableSpec<'a> {
    name: &'a str,
    shader_group: Option<BlockId>,
    skeleton: Option<BlockId>,
    lods: [Vec<M<'a>>; 4],
    lod_dist: [f32; 4],
    bound: Option<BlockId>,
    /// Pages info pointer (only the FragType root carries one).
    flags_high: u32,
}

/// FragDrawable: DrawableBase (168) + FragDrawable fields (336 total).
fn add_frag_drawable(b: &mut Builder, s: &DrawableSpec) -> Result<BlockId, String> {
    let id = b.reserve(Seg::Sys);
    let name = b.string(s.name);
    let pts: Vec<V3> = s.lods.iter().flatten().flat_map(|m| m.geoms.iter().flat_map(|(g, _)| g.positions.iter().copied())).collect();
    let (lo, hi, c, r) = if pts.is_empty() {
        ([0.0; 3], [0.0; 3], [0.0; 3], 0.0)
    } else {
        let (lo, hi) = aabb(pts.iter().copied());
        let c = [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0, (lo[2] + hi[2]) / 2.0];
        let r = pts.iter().map(|p| len(sub(*p, c))).fold(0.0f32, f32::max);
        (lo, hi, c, r)
    };
    let has_models = s.lods.iter().any(|l| !l.is_empty());
    let (models, heads, mlen) = if has_models {
        let (m, h, l) = add_models_block(b, &s.lods)?;
        (Some(m), h, l)
    } else {
        (None, [None; 4], 0)
    };
    let lp = |i: usize| heads[i].map(|o| (models, o));
    let mut w = W::new();
    w.u32(VFT_FRAGDRAWABLE).u32(1).u64(0);
    w.ptr(s.shader_group).ptr(s.skeleton);
    w.v3(c).f32(r);
    w.v3(lo).u32(NAN_BITS).v3(hi).u32(NAN_BITS);
    for i in 0..4 {
        match lp(i) {
            Some((m, o)) => w.ptr_at(m, o),
            None => w.u64(0),
        };
    }
    for d in s.lod_dist {
        w.f32(d);
    }
    let mask = |i: usize| if heads[i].is_some() { 0xFF00 | if i == 0 { s.flags_high } else { 1 } } else { 0 };
    w.u32(mask(0)).u32(mask(1)).u32(mask(2)).u32(mask(3));
    w.u64(0).u16(0).u16(mlen.div_ceil(16) as u16).u32(0);
    w.ptr(models);
    debug_assert_eq!(w.len(), 0xA8);
    // FragDrawable
    w.u64(0);
    mat(&mut w, ([0.0; 3], [0.0, 0.0, 0.0, 1.0]), [0, 0, 0, 0]);
    w.ptr(s.bound).u64(0).u16(0).u16(0).u32(0);
    w.u64(0).u16(0).u16(1).u32(0);
    w.u64(0).u64(0).u64(0);
    w.ptr(Some(name)).u64(0).u64(0).u64(0);
    debug_assert_eq!(w.len(), 336);
    b.set(id, w);
    Ok(id)
}

// --------------------------------------------------------------- skeleton

fn add_skeleton(b: &mut Builder, bones: &[Bone]) -> BlockId {
    let n = bones.len();
    let tg = tags(bones);
    let g = globals(bones);
    let has_kids: Vec<bool> = (0..n).map(|i| bones.iter().any(|x| x.parent == Some(i))).collect();
    let next_sib: Vec<i16> = (0..n)
        .map(|i| ((i + 1)..n).find(|&j| bones[j].parent == bones[i].parent).map_or(-1, |j| j as i16))
        .collect();

    // bones block: 16-byte header + 80 bytes per bone
    let names: Vec<BlockId> = bones.iter().map(|x| b.string(&x.name)).collect();
    let mut w = W::new();
    w.u32(n as u32).u32(0).u32(0).u32(0);
    for (i, x) in bones.iter().enumerate() {
        let flags: u16 = 0x77 | if has_kids[i] { 0x1000 } else { 0 };
        w.v4(qnorm(x.r)).v3(x.t).u32(0).v3([1.0; 3]).f32(1.0);
        w.u16(next_sib[i] as u16).u16(x.parent.map_or(-1i16, |p| p as i16) as u16).u32(0);
        w.ptr(Some(names[i]));
        w.u16(flags).u16(i as u16).u16(tg[i]).u16(i as u16).u64(0);
    }
    let bones_b = b.add(Seg::Sys, w);

    // inverse model-space transforms (column 4 zero), local transforms
    // (column 4 = Sollumz' TransformUnk 0,4,-3,0)
    let mut wi = W::new();
    let mut wl = W::new();
    for (i, x) in bones.iter().enumerate() {
        let (t, q) = g[i];
        let qi = qconj(q);
        let ti = qrot(qi, [-t[0], -t[1], -t[2]]);
        let a = axes(qi);
        for k in 0..3 {
            wi.v3(a[k]).f32(0.0);
        }
        wi.v3(ti).f32(0.0);
        let a = axes(qnorm(x.r));
        let unk = [0.0f32, 4.0, -3.0, 0.0];
        for k in 0..3 {
            wl.v3(a[k]).f32(unk[k]);
        }
        wl.v3(x.t).f32(unk[3]);
    }
    let inv_b = b.add(Seg::Sys, wi);
    let loc_b = b.add(Seg::Sys, wl);

    let mut wp = W::new();
    for x in bones {
        wp.u16(x.parent.map_or(-1i16, |p| p as i16) as u16);
    }
    let par_b = b.add(Seg::Sys, wp);
    // child -> parent pairs, breadth first
    let mut order: Vec<usize> = Vec::new();
    let mut q: Vec<usize> = (0..n).filter(|&i| bones[i].parent.is_none()).collect();
    while !q.is_empty() {
        let mut nq = Vec::new();
        for &p in &q {
            for c in (0..n).filter(|&c| bones[c].parent == Some(p)) {
                order.push(c);
                nq.push(c);
            }
        }
        q = nq;
    }
    let mut wc = W::new();
    for &c in &order {
        wc.u16(c as u16).u16(bones[c].parent.unwrap() as u16);
    }
    let nchild = order.len() * 2;
    let child_b = if nchild > 0 { Some(b.add(Seg::Sys, wc)) } else { None };

    // bone tag hash table
    let nb = [11usize, 29, 59, 107, 191, 331, 563, 953].into_iter().find(|&k| n < k).unwrap_or(1609);
    let mut buckets: Vec<Vec<usize>> = vec![Vec::new(); nb];
    for i in 0..n {
        buckets[tg[i] as usize % nb].push(i);
    }
    let mut heads = Vec::with_capacity(nb);
    for bk in &buckets {
        // chain in reverse insertion order (CodeWalker reverses each bucket)
        let mut next: Option<BlockId> = None;
        for &i in bk.iter() {
            let mut t = W::new();
            t.u32(tg[i] as u32).u32(i as u32).ptr(next);
            next = Some(b.add(Seg::Sys, t));
        }
        heads.push(next);
    }
    let mut wt = W::new();
    for h in &heads {
        wt.ptr(*h);
    }
    let tags_b = b.add(Seg::Sys, wt);

    let mut w = W::new();
    w.u32(VFT_SKELETON).u32(1).u64(0);
    w.ptr(Some(tags_b)).u16(nb as u16).u16(n.min(nb) as u16).u32(0x0100_0000);
    w.ptr_at(Some(bones_b), 16).ptr(Some(inv_b)).ptr(Some(loc_b)).ptr(Some(par_b)).ptr(child_b);
    w.u64(0);
    let sig = |s: &str| joaat(&format!("{s}{n}"));
    w.u32(sig("bones")).u32(sig("tags")).u32(sig("parents"));
    w.u16(1).u16(n as u16).u16(nchild as u16).u16(0).u32(0).u64(0);
    debug_assert_eq!(w.len(), 112);
    b.add(Seg::Sys, w)
}

// ----------------------------------------------------------------- bounds

/// Bounds header (112 bytes, the whole BoundBox).
fn bounds_hdr(w: &mut W, vft: u32, ty: u8, min: V3, max: V3, inertia: V3, volume: f32) {
    let cen = [(min[0] + max[0]) / 2.0, (min[1] + max[1]) / 2.0, (min[2] + max[2]) / 2.0];
    let rad = len(sub(max, cen));
    w.u32(vft).u32(1).u64(0);
    w.u8(ty).u8(0).u16(0).f32(rad).u32(0).u32(0);
    w.v3(max).f32(MARGIN).v3(min).u32(2);
    w.v3(cen).u8(0).u8(0).u8(0).u8(0);
    w.v3(cen).u8(0).u8(0).u16(0);
    w.v3(inertia).f32(volume);
}

fn box_inertia(min: V3, max: V3) -> (V3, f32) {
    let d = sub(max, min);
    let (x, y, z) = (d[0] * d[0], d[1] * d[1], d[2] * d[2]);
    ([(y + z) / 12.0, (x + z) / 12.0, (x + y) / 12.0], d[0] * d[1] * d[2])
}

/// Composite of one box per child; child boxes in bone space, placed by
/// the bones' model-space transforms.
fn add_composite(b: &mut Builder, f: &Frag, g: &[(V3, Q)]) -> Result<(BlockId, Vec<BlockId>), String> {
    let n = f.children.len();
    if n == 0 || n > 5 {
        return Err(format!("{n} physics children (1..5 supported: no composite BVH)"));
    }
    let mut kids = Vec::new();
    let mut world = Vec::new();
    for c in &f.children {
        let (i, v) = box_inertia(c.min, c.max);
        let mut w = W::new();
        bounds_hdr(&mut w, VFT_BOX, 3, c.min, c.max, i, v);
        kids.push(b.add(Seg::Sys, w));
        let gt = g[c.bone];
        let corners = (0..8).map(|k| {
            let p = [if k & 1 == 0 { c.min[0] } else { c.max[0] }, if k & 2 == 0 { c.min[1] } else { c.max[1] }, if k & 4 == 0 { c.min[2] } else { c.max[2] }];
            add(gt.0, qrot(gt.1, p))
        });
        world.push(aabb(corners));
    }
    let mut cw = W::new();
    for k in &kids {
        cw.ptr(Some(*k));
    }
    let children_b = b.add(Seg::Sys, cw);
    let mut tw = W::new();
    for c in &f.children {
        mat(&mut tw, g[c.bone], [0, 0, 0, 0]);
    }
    let tr_b = b.add(Seg::Sys, tw);
    let mut bw = W::new();
    for (mn, mx) in &world {
        bw.v3(*mn).u32(1).v3(*mx).f32(MARGIN);
    }
    let bb_b = b.add(Seg::Sys, bw);
    let mut fw = W::new();
    for _ in 0..n {
        fw.u32(VEH_TYPE).u32(VEH_INCLUDE);
    }
    let f1 = b.add(Seg::Sys, fw.clone());
    let f2 = b.add(Seg::Sys, fw);
    let (mn, mx) = aabb(world.iter().flat_map(|a| [a.0, a.1]));
    let (i, v) = box_inertia(mn, mx);
    let mut w = W::new();
    bounds_hdr(&mut w, VFT_COMPOSITE, 10, mn, mx, i, v);
    w.ptr(Some(children_b)).ptr(Some(tr_b)).ptr(Some(tr_b)).ptr(Some(bb_b)).ptr(Some(f1)).ptr(Some(f2));
    w.u16(n as u16).u16(n as u16).u32(0).u64(0);
    debug_assert_eq!(w.len(), 176);
    Ok((b.add(Seg::Sys, w), kids))
}

// ---------------------------------------------------------------- physics

fn add_group(w: &mut W, gr: &Group, idx: usize, f: &Frag) {
    let child_groups: Vec<usize> = (0..f.groups.len()).filter(|&j| f.groups[j].parent == Some(idx)).collect();
    let kids: Vec<usize> = (0..f.children.len()).filter(|&j| f.children[j].group == idx).collect();
    let mass: f32 = kids.iter().map(|&k| f.children[k].mass).sum();
    w.f32(0.0).f32(0.0).f32(0.0).f32(0.0);
    w.f32(100.0).f32(0.25).f32(0.25).f32(0.0); // strength, force up/down, stiffness
    w.f32(-1.0).f32(1.0).f32(1.0).f32(1.0); // soft angles
    w.f32(0.0).f32(0.0).f32(0.0).f32(0.0).f32(0.0); // rotation, restoring, latch
    w.f32(mass).f32(0.0);
    w.u8(child_groups.first().map_or(255, |&x| x as u8));
    w.u8(gr.parent.map_or(255, |p| p as u8));
    w.u8(kids.first().map_or(0, |&x| x as u8));
    w.u8(kids.len() as u8);
    w.u8(child_groups.len() as u8).u8(255).u8(0).u8(0);
    w.f32(100.0).f32(1000.0); // min damage force, damage health
    w.f32(0.0);
    for _ in 0..7 {
        w.f32(1.0);
    }
    w.f32(0.0);
    let mut nm = [0u8; 40];
    for (k, c) in gr.name.bytes().take(39).enumerate() {
        nm[k] = c;
    }
    w.bytes(&nm);
    w.f32(1.0).f32(0.0);
}

/// Build a .yft (resource version 162).
pub fn build_yft(f: &Frag) -> Result<Vec<u8>, String> {
    let nb = f.bones.len();
    if nb == 0 || nb > 255 {
        return Err(format!("{nb} bones (1..255 supported)"));
    }
    if f.groups.is_empty() || f.groups.len() > 255 {
        return Err("bad group count".into());
    }
    let g = globals(&f.bones);
    let tg = tags(&f.bones);
    let mut b = Builder::new();
    let root = b.reserve(Seg::Sys);
    let mut p = W::new();
    p.u32(0).u32(0).u8(128).u8(0).u16(0).u32(0).zeros(8 * 128);
    let pages = b.add(Seg::Sys, p);
    b.pages_info = Some(pages);

    // shaders: unique (texture, kind) over every geometry
    let mut shaders: Vec<(String, u8)> = Vec::new();
    let sidx = |gm: &Geom, shaders: &mut Vec<(String, u8)>| -> u16 {
        let k = (gm.texture.clone(), gm.kind);
        (shaders.iter().position(|s| *s == k).unwrap_or_else(|| {
            shaders.push(k);
            shaders.len() - 1
        })) as u16
    };
    let mut lods: [Vec<M>; 4] = Default::default();
    for (li, l) in f.lods.iter().enumerate() {
        for m in l {
            if m.geoms.is_empty() {
                continue;
            }
            if m.bone >= nb {
                return Err(format!("model bound to bone {} of {nb}", m.bone));
            }
            let geoms = m.geoms.iter().map(|gm| (gm, sidx(gm, &mut shaders))).collect();
            lods[li].push(M { bone: m.bone as u8, geoms });
        }
    }
    if lods[0].is_empty() {
        return Err("no high LOD models".into());
    }
    let child_models: Vec<Vec<(&Geom, u16)>> = f.children.iter().map(|c| c.drawable.iter().map(|gm| (gm, sidx(gm, &mut shaders))).collect()).collect();

    let sh: Vec<BlockId> = shaders.iter().map(|(t, k)| add_shader(&mut b, t, *k)).collect();
    let mut pa = W::new();
    for &s in &sh {
        pa.ptr(Some(s));
    }
    let parr = b.add(Seg::Sys, pa);
    let mut sg = W::new();
    sg.u32(VFT_SHADERGROUP).u32(1).u64(0).ptr(Some(parr));
    sg.u16(sh.len() as u16).u16(sh.len() as u16).u32(0).u64(0).u64(0);
    sg.u32(64 / 16).u32(0).u64(0);
    let sgid = b.add(Seg::Sys, sg);

    let skel = add_skeleton(&mut b, &f.bones);
    let (comp, kid_bounds) = add_composite(&mut b, f, &g)?;
    let main = add_frag_drawable(&mut b, &DrawableSpec { name: "skel", shader_group: Some(sgid), skeleton: Some(skel), lods, lod_dist: f.lod_dist, bound: Some(comp), flags_high: 1 })?;

    // children
    let mut child_ids = Vec::new();
    for (ci, c) in f.children.iter().enumerate() {
        let cm = child_models[ci].clone();
        let mut l: [Vec<M>; 4] = Default::default();
        if !cm.is_empty() {
            l[0].push(M { bone: 0, geoms: cm });
        }
        let d = add_frag_drawable(&mut b, &DrawableSpec { name: &f.bones[c.bone].name, shader_group: None, skeleton: Some(skel), lods: l, lod_dist: [9998.0; 4], bound: Some(kid_bounds[ci]), flags_high: 1 })?;
        let mut w = W::new();
        w.u32(VFT_CHILD).u32(1).f32(c.mass).f32(c.mass);
        w.u16(c.group as u16).u16(tg[c.bone]).u32(0);
        w.zeros(0xA0 - w.len());
        w.ptr(Some(d)).u64(0).u64(0);
        w.zeros(256 - w.len());
        child_ids.push(b.add(Seg::Sys, w));
    }
    let n = f.children.len();
    let mut w = W::new();
    for &c in &child_ids {
        w.ptr(Some(c));
    }
    let children_b = b.add(Seg::Sys, w);

    // groups (pointer array) and their name table (pointers to group +0x80)
    let mut gids = Vec::new();
    for (gi, gr) in f.groups.iter().enumerate() {
        let mut w = W::new();
        add_group(&mut w, gr, gi, f);
        debug_assert_eq!(w.len(), 176);
        gids.push(b.add(Seg::Sys, w));
    }
    let mut w = W::new();
    for &gid in &gids {
        w.ptr(Some(gid));
    }
    let groups_b = b.add(Seg::Sys, w);
    let mut w = W::new();
    for &gid in &gids {
        w.ptr_at(Some(gid), 128);
    }
    w.u32(VFT_GROUPNAMES).u32(1);
    let names_b = b.add(Seg::Sys, w);

    // per-child arrays
    let mut wm = W::new();
    let mut wv = W::new();
    let mut wi = W::new();
    let mut wt = W::new();
    wt.u32(VFT_TRANSFORMS).u32(1).u64(0).u32(n as u32).u32(0).u64(0);
    for c in &f.children {
        let (i, v) = box_inertia(c.min, c.max);
        wm.f32(c.mass);
        wv.v4([0.0; 4]);
        wi.v3(i.map(|x| x * c.mass)).f32(c.mass * v);
        // SharpDX Matrix: rows = axes, translation row, M44 = 1
        let gt = g[c.bone];
        let a = axes(gt.1);
        for k in 0..3 {
            wt.v3(a[k]).f32(0.0);
        }
        wt.v3(gt.0).f32(1.0);
    }
    let masses_b = b.add(Seg::Sys, wm);
    let vecs_b = b.add(Seg::Sys, wv);
    let inert_b = b.add(Seg::Sys, wi);
    let trans_b = b.add(Seg::Sys, wt);

    // archetype
    let (mn, mx) = {
        let pts: Vec<V3> = f.lods[0].iter().flat_map(|m| m.geoms.iter().flat_map(|gm| gm.positions.iter().map(|p| add(g[m.bone].0, qrot(g[m.bone].1, *p))))).collect();
        aabb(pts.into_iter())
    };
    let (ai, _) = box_inertia(mn, mx);
    let ai = ai.map(|x| x * f.mass);
    let aname = b.string(&f.name);
    let mut w = W::new();
    w.u32(VFT_ARCHETYPE).u32(1).u64(0).u64(2).ptr(Some(aname)).ptr(Some(comp));
    w.u32(1).u32(0xFFFF_FFFF).u32(0x0001_0000).u32(0).u64(0);
    w.f32(f.mass).f32(1.0 / f.mass).f32(1.0).f32(150.0).f32(std::f32::consts::TAU).f32(1.0).u64(0);
    w.v3(ai).u32(QNAN_BITS).v3(ai.map(|x| 1.0 / x.max(1e-6))).u32(QNAN_BITS);
    for _ in 0..6 {
        w.v3([0.0; 3]).u32(NAN_BITS);
    }
    debug_assert_eq!(w.len(), 224);
    let arch = b.add(Seg::Sys, w);

    // physics LOD
    let roots = f.groups.iter().filter(|x| x.parent.is_none()).count();
    let mut w = W::new();
    w.u32(VFT_LOD).u32(1).u64(0).u32(0).f32(0.55).f32(550.0).f32(-1.0);
    w.u64(0).ptr(Some(masses_b));
    w.v3([0.0; 3]).u32(QNAN_BITS).v3([0.0; 3]).u32(QNAN_BITS).v3([0.0; 3]).u32(NAN_BITS);
    for d in [0.02f32, 0.02, 0.01, 0.02, 0.02, 0.01] {
        w.v3([d; 3]).u32(NAN_BITS);
    }
    w.ptr(Some(names_b)).ptr(Some(groups_b)).ptr(Some(children_b)).ptr(Some(arch)).ptr(Some(arch)).ptr(Some(comp));
    w.ptr(Some(inert_b)).ptr(Some(vecs_b)).ptr(Some(trans_b)).u64(0).u64(0);
    w.u8(0).u8(0).u8(f.groups.len() as u8).u8(roots as u8).u8(1).u8(n as u8).u8(n as u8).u8(0);
    w.u64(0).u64(0);
    debug_assert_eq!(w.len(), 304);
    let lod = b.add(Seg::Sys, w);
    let mut w = W::new();
    w.u32(VFT_LODGROUP).u32(1).u64(0).ptr(Some(lod)).u64(0).u64(0).u64(0);
    let lodg = b.add(Seg::Sys, w);

    // bone transforms (Matrix3_s: rows of the 3x4 model-space transform)
    let mut w = W::new();
    w.u64(0).u64(0).u8(nb as u8).u8(nb as u8).u16(1).u32(0).u64(0);
    for &(t, q) in &g {
        let a = axes(q);
        for r in 0..3 {
            w.f32(a[0][r]).f32(a[1][r]).f32(a[2][r]).f32(t[r]);
        }
    }
    let bt = b.add(Seg::Sys, w);

    // FragType root
    let name = b.string(&f.name);
    let pts: Vec<V3> = vec![mn, mx];
    let c = [(pts[0][0] + pts[1][0]) / 2.0, (pts[0][1] + pts[1][1]) / 2.0, (pts[0][2] + pts[1][2]) / 2.0];
    let r = len(sub(mx, c));
    let mut w = W::new();
    w.u32(VFT_FRAG).u32(1).ptr(Some(pages));
    w.u64(0).u64(0);
    w.v3(c).f32(r);
    w.ptr(Some(main)).u64(0).u64(0).u32(0).i32(-1);
    w.u64(0).ptr(Some(name));
    w.u64(0).u16(0).u16(0).u32(0); // cloths list
    w.zeros(7 * 8);
    w.ptr(Some(bt));
    w.i32(0).i32(0).i32(0).i32(0).i32(65280).i32(1).i32(-1).f32(0.0);
    w.f32(1.0).f32(1.0); // gravity, buoyancy
    w.u8(0).u8(0).u16(0).u32(0).u64(0).u64(0);
    w.ptr(Some(lodg)).u64(0).u64(0).u64(0);
    w.u64(0).u16(0).u16(0).u32(0); // light attributes
    w.u64(0).u64(0);
    debug_assert_eq!(w.len(), 304);
    b.set(root, w);
    b.build(162)
}

/// Unit cylinder tyre along X, radius `r`, width `wd`, centred at the origin.
pub fn tyre(r: f32, wd: f32, texture: &str, seg: usize) -> Geom {
    let mut g = Geom { texture: texture.into(), ..Default::default() };
    let hx = wd / 2.0;
    for side in [-1.0f32, 1.0] {
        for k in 0..=seg {
            let a = k as f32 / seg as f32 * std::f32::consts::TAU;
            let (s, c) = a.sin_cos();
            g.positions.push([hx * side, c * r, s * r]);
            g.normals.push([0.0, c, s]);
            g.colours.push(0xFF20_2020);
            g.uvs.push([k as f32 / seg as f32, (side + 1.0) / 2.0]);
        }
    }
    let row = (seg + 1) as u16;
    for k in 0..seg as u16 {
        let (a, b2, c, d) = (k, k + 1, row + k, row + k + 1);
        g.indices.extend_from_slice(&[a, c, b2, b2, c, d]);
    }
    // caps
    for side in [-1.0f32, 1.0] {
        let centre = g.positions.len() as u16;
        g.positions.push([hx * side, 0.0, 0.0]);
        g.normals.push([side, 0.0, 0.0]);
        g.colours.push(0xFF30_3030);
        g.uvs.push([0.5, 0.5]);
        for k in 0..=seg {
            let a = k as f32 / seg as f32 * std::f32::consts::TAU;
            let (s, c) = a.sin_cos();
            g.positions.push([hx * side, c * r * 0.7, s * r * 0.7]);
            g.normals.push([side, 0.0, 0.0]);
            g.colours.push(0xFF90_9090);
            g.uvs.push([0.5 + c * 0.5, 0.5 + s * 0.5]);
        }
        let ring = centre + 1;
        for k in 0..seg as u16 {
            if side > 0.0 {
                g.indices.extend_from_slice(&[centre, ring + k, ring + k + 1]);
            } else {
                g.indices.extend_from_slice(&[centre, ring + k + 1, ring + k]);
            }
        }
    }
    g
}
