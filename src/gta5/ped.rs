//! GTA V (legacy PC) ped writers, ported from CodeWalker (Drawable.cs,
//! Frag.cs): skinned drawables (ped.sps, skin vertex layout), the ped
//! Skeleton, .ydd drawable dictionaries and the skeleton-only .yft a
//! component ped needs (GTA V ped .yft files carry no physics).

use super::rsc::{joaat, BlockId, Builder, Seg, W};

/// One skinned vertex. `weights` sum to 255; `bones` index the skeleton.
#[derive(Clone, Copy, Debug, Default)]
pub struct SkinVert {
    pub pos: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
    pub tangent: [f32; 4],
    pub weights: [u8; 4],
    pub bones: [u8; 4],
}

#[derive(Clone, Debug, Default)]
pub struct SkinGeom {
    pub verts: Vec<SkinVert>,
    pub indices: Vec<u16>,
    pub shader: usize,
}

/// A shader: `file` without ".sps" (e.g. "ped"), parameters in file order.
#[derive(Clone, Debug)]
pub struct ShaderDef {
    pub file: String,
    pub bucket: u8,
    pub textures: Vec<(&'static str, Option<String>)>,
    pub vectors: Vec<(&'static str, [f32; 4])>,
}

impl ShaderDef {
    /// ped.sps / ped_alpha.sps with the parameter set of GTA V ped components.
    pub fn ped(diffuse: &str, normal: &str, spec: &str, alpha: bool) -> Self {
        ShaderDef {
            file: if alpha { "ped_alpha".into() } else { "ped".into() },
            bucket: if alpha { 1 } else { 0 },
            textures: vec![
                ("DiffuseSampler", Some(diffuse.into())),
                ("TextureSamplerDiffPal", None),
                ("VolumeSampler", Some("givemechecker".into())),
                ("BumpSampler", Some(normal.into())),
                ("SpecSampler", Some(spec.into())),
            ],
            vectors: vec![
                ("umGlobalParams", [0.0025, 0.0025, 7.0, 7.0]),
                ("envEffFatThickness", [25.0, 25.0, 0.0, 0.0]),
                ("specularIntensityMult", [0.3, 0.0, 0.0, 0.0]),
                ("specularFalloffMult", [100.0, 0.0, 0.0, 0.0]),
                ("specularFresnel", [0.97, 0.0, 0.0, 0.0]),
                ("bumpiness", [1.0, 0.0, 0.0, 0.0]),
                ("detailSettings", [0.1, 0.75, 40.0, 0.0]),
                ("StubbleControl", [2.0, 0.6, 0.0, 0.0]),
            ],
        }
    }
    /// default.sps (the .yft placeholder geometry).
    pub fn plain() -> Self {
        ShaderDef {
            file: "default".into(),
            bucket: 0,
            textures: vec![("DiffuseSampler", None)],
            vectors: vec![
                ("matMaterialColorScale", [1.0, 0.0, 0.0, 1.0]),
                ("HardAlphaBlend", [1.0, 0.0, 0.0, 0.0]),
                ("useTessellation", [0.0, 0.0, 0.0, 0.0]),
                ("wetnessMultiplier", [1.0, 0.0, 0.0, 0.0]),
                ("globalAnimUV1", [0.0, 1.0, 0.0, 0.0]),
                ("globalAnimUV0", [1.0, 0.0, 0.0, 0.0]),
            ],
        }
    }
}

#[derive(Clone, Debug)]
pub struct SkinDrawable {
    pub name: String,
    pub shaders: Vec<ShaderDef>,
    pub geoms: Vec<SkinGeom>,
}

/// One skeleton bone. `rot` is (x, y, z, w), `trans` local to the parent.
#[derive(Clone, Debug)]
pub struct SkelBone {
    pub name: String,
    pub tag: u16,
    pub parent: i16,
    pub sibling: i16,
    pub flags: u16,
    pub trans: [f32; 3],
    pub rot: [f32; 4],
    pub unk: [f32; 4],
}

#[derive(Clone, Debug)]
pub struct Skeleton {
    pub bones: Vec<SkelBone>,
    /// Unknown_1Ch and the three signature words (50h, 54h, 58h).
    pub sig: [u32; 4],
}

const VFT_DRAWABLE: u32 = 1079456120;
const VFT_FRAGDRAWABLE: u32 = 1080060872;
const VFT_FRAGTYPE: u32 = 1079456040;
const VFT_DICT: u32 = 1079457128;
const VFT_SHADERGROUP: u32 = 1080113136;
const VFT_SKELETON: u32 = 1080114336;
const VFT_MODEL: u32 = 1080101528;
const VFT_GEOM: u32 = 1080133528;
const VFT_VB: u32 = 1080153080;
const VFT_IB: u32 = 1080152408;
const GTAV1: u64 = 0x7755555555996996;
/// Position BlendWeights BlendIndices Normal Colour0 Colour1 TexCoord0
/// TexCoord1 Tangent (the GTA V ped component layout).
const SKIN_FLAGS: u32 = 0xFF | (1 << 14);
const SKIN_STRIDE: usize = 12 + 4 + 4 + 12 + 4 + 4 + 8 + 8 + 16;

fn pad16(o: usize) -> usize {
    (16 - o % 16) % 16
}

pub fn aabb(pts: impl Iterator<Item = [f32; 3]>) -> ([f32; 3], [f32; 3]) {
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

fn add_texture_ref(b: &mut Builder, name: &str) -> BlockId {
    let nm = b.string(name);
    let mut w = W::new();
    w.u32(0).u32(1).zeros(32);
    w.ptr(Some(nm));
    w.u16(1).u16(2).u32(0).u32(0).u32(0);
    w.u32(0).u32(0).u32(0).u32(0);
    b.add(Seg::Sys, w)
}

/// ShaderFX (48 bytes) and its ShaderParametersBlock.
fn add_shader(b: &mut Builder, s: &ShaderDef) -> BlockId {
    let nt = s.textures.len();
    let nv = s.vectors.len();
    let n = nt + nv;
    let base_size = 32 + 16 * n + 16 * nv + 4 * n;
    let data_size = base_size + pad16(base_size);
    let pb = b.reserve(Seg::Sys);
    let texs: Vec<Option<BlockId>> = s.textures.iter().map(|(_, t)| t.as_deref().map(|t| add_texture_ref(b, t))).collect();
    let mut w = W::new();
    for (i, t) in texs.iter().enumerate() {
        w.u8(0).u8((i + 2) as u8).u16(0).u32(0).ptr(*t);
    }
    let mut voff = 16 * n;
    for i in 0..nv {
        // vector params get 160.. counted from the last parameter
        w.u8(1).u8((160 + (nv - 1 - i)) as u8).u16(0).u32(0).ptr_at(Some(pb), voff as u64);
        voff += 16;
    }
    for (_, v) in &s.vectors {
        w.v4(*v);
    }
    for (name, _) in &s.textures {
        w.u32(joaat(name));
    }
    for (name, _) in &s.vectors {
        w.u32(joaat(name));
    }
    w.zeros(32 + data_size * 4);
    b.set(pb, w);

    let mut w = W::new();
    w.ptr(Some(pb));
    w.u32(joaat(&s.file)).u32(0);
    w.u8(n as u8).u8(s.bucket).u16(32768);
    w.u16((16 * n + 16 * nv) as u16).u16(data_size as u16);
    w.u32(joaat(&format!("{}.sps", s.file))).u32(0);
    w.u32((1u32 << s.bucket) | 0xFF00).u16(0).u8(0).u8(nt as u8).u64(0);
    b.add(Seg::Sys, w)
}

fn add_shader_group(b: &mut Builder, shaders: &[ShaderDef], tex_dict: Option<BlockId>) -> BlockId {
    let ids: Vec<BlockId> = shaders.iter().map(|s| add_shader(b, s)).collect();
    let mut pa = W::new();
    for &s in &ids {
        pa.ptr(Some(s));
    }
    let parr = b.add(Seg::Sys, pa);
    let mut sg = W::new();
    sg.u32(VFT_SHADERGROUP).u32(1).ptr(tex_dict).ptr(Some(parr));
    sg.u16(ids.len() as u16).u16(ids.len() as u16).u32(0).u64(0).u64(0);
    sg.u32(64 / 16).u32(0).u64(0);
    b.add(Seg::Sys, sg)
}

// ------------------------------------------------------------ math

type M4 = [f32; 16]; // column-major, column vectors: p' = M p

fn m_mul(a: &M4, b: &M4) -> M4 {
    let mut o = [0f32; 16];
    for c in 0..4 {
        for r in 0..4 {
            o[c * 4 + r] = (0..4).map(|k| a[k * 4 + r] * b[c * 4 + k]).sum();
        }
    }
    o
}

/// Rigid inverse (rotation transposed).
pub fn m_inv(m: &M4) -> M4 {
    let mut o = [0f32; 16];
    for r in 0..3 {
        for c in 0..3 {
            o[c * 4 + r] = m[r * 4 + c];
        }
    }
    for r in 0..3 {
        o[12 + r] = -(0..3).map(|k| o[k * 4 + r] * m[12 + k]).sum::<f32>();
    }
    o[15] = 1.0;
    o
}

pub fn m_trs(t: [f32; 3], q: [f32; 4]) -> M4 {
    let [x, y, z, w] = q;
    let n = (x * x + y * y + z * z + w * w).sqrt().max(1e-12);
    let (x, y, z, w) = (x / n, y / n, z / n, w / n);
    [
        1.0 - 2.0 * (y * y + z * z), 2.0 * (x * y + z * w), 2.0 * (x * z - y * w), 0.0,
        2.0 * (x * y - z * w), 1.0 - 2.0 * (x * x + z * z), 2.0 * (y * z + x * w), 0.0,
        2.0 * (x * z + y * w), 2.0 * (y * z - x * w), 1.0 - 2.0 * (x * x + y * y), 0.0,
        t[0], t[1], t[2], 1.0,
    ]
}

/// Rotation part of a column-major matrix as a quaternion (x, y, z, w),
/// columns normalised first.
pub fn m_quat(m: &M4) -> [f32; 4] {
    let col = |c: usize| {
        let v = [m[c * 4], m[c * 4 + 1], m[c * 4 + 2]];
        let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1e-12);
        [v[0] / l, v[1] / l, v[2] / l]
    };
    let (c0, c1, c2) = (col(0), col(1), col(2));
    // a(r, c) = column c, row r
    let (m00, m11, m22) = (c0[0], c1[1], c2[2]);
    let tr = m00 + m11 + m22;
    let q = if tr > 0.0 {
        let s = (tr + 1.0).sqrt() * 2.0;
        [(c1[2] - c2[1]) / s, (c2[0] - c0[2]) / s, (c0[1] - c1[0]) / s, 0.25 * s]
    } else if m00 > m11 && m00 > m22 {
        let s = (1.0 + m00 - m11 - m22).sqrt() * 2.0;
        [0.25 * s, (c1[0] + c0[1]) / s, (c2[0] + c0[2]) / s, (c1[2] - c2[1]) / s]
    } else if m11 > m22 {
        let s = (1.0 + m11 - m00 - m22).sqrt() * 2.0;
        [(c1[0] + c0[1]) / s, 0.25 * s, (c2[1] + c1[2]) / s, (c2[0] - c0[2]) / s]
    } else {
        let s = (1.0 + m22 - m00 - m11).sqrt() * 2.0;
        [(c2[0] + c0[2]) / s, (c2[1] + c1[2]) / s, 0.25 * s, (c0[1] - c1[0]) / s]
    };
    let l = q.iter().map(|v| v * v).sum::<f32>().sqrt();
    q.map(|v| v / l)
}

/// Model-space bind matrix of every bone (parents first or not).
pub fn world_mats(bones: &[SkelBone]) -> Vec<M4> {
    let mut out: Vec<Option<M4>> = vec![None; bones.len()];
    fn get(i: usize, bones: &[SkelBone], out: &mut Vec<Option<M4>>) -> M4 {
        if let Some(m) = out[i] {
            return m;
        }
        let l = m_trs(bones[i].trans, bones[i].rot);
        let p = bones[i].parent;
        let m = if p >= 0 && (p as usize) < bones.len() && p as usize != i { m_mul(&get(p as usize, bones, out), &l) } else { l };
        out[i] = Some(m);
        m
    }
    (0..bones.len()).map(|i| get(i, bones, &mut out)).collect()
}

/// A column-major matrix written as SharpDX stores it (row-vector
/// convention, i.e. the transpose), with column 4 replaced by `c4`.
fn put_matrix(w: &mut W, m: &M4, c4: [f32; 4]) {
    for r in 0..4 {
        // SharpDX row r = our column r (rows 0-2 basis, row 3 translation)
        w.f32(m[r * 4]).f32(m[r * 4 + 1]).f32(m[r * 4 + 2]).f32(c4[r]);
    }
}

// ------------------------------------------------------------ skeleton

fn hash_buckets(n: usize) -> usize {
    [11, 29, 59, 107, 191, 331, 563, 953, 1609, 2729, 4621, 7841, 13297, 22571, 38351, 65167].into_iter().find(|&b| n < b).unwrap_or(65521)
}

/// Skeleton.BuildIndices: child/parent pairs in layers of four, each
/// layer padded to 8 shorts with its last pair.
fn child_indices(bones: &[SkelBone]) -> Vec<i16> {
    let children = |p: i16| -> Vec<usize> { (0..bones.len()).filter(|&i| bones[i].parent == p).collect() };
    let all_children = |l: &[usize]| -> Vec<usize> { l.iter().flat_map(|&b| children(b as i16)).collect() };
    let roots: Vec<usize> = (0..bones.len()).filter(|&i| bones[i].parent < 0).collect();
    let mut layer = all_children(&roots);
    let mut out = Vec::new();
    while !layer.is_empty() {
        let n = layer.len().min(4);
        let ins: Vec<usize> = layer.drain(..n).collect();
        let ext = all_children(&ins);
        for (k, e) in ext.into_iter().enumerate() {
            layer.insert(k, e);
        }
        for &b in &ins {
            out.push(b as i16);
            out.push(bones[b].parent);
        }
        let last = *ins.last().unwrap();
        while out.len() % 8 != 0 {
            out.push(last as i16);
            out.push(bones[last].parent);
        }
    }
    out
}

pub fn add_skeleton(b: &mut Builder, s: &Skeleton) -> BlockId {
    let n = s.bones.len();
    let id = b.reserve(Seg::Sys);
    // bone tag hash table (ResourcePointerArray64 of chained 16-byte tags)
    let nb = hash_buckets(n);
    let mut buckets: Vec<Vec<usize>> = vec![Vec::new(); nb];
    for (i, bn) in s.bones.iter().enumerate() {
        buckets[bn.tag as usize % nb].push(i);
    }
    let mut tw = W::new();
    for bucket in &buckets {
        // CodeWalker reverses each bucket and chains it
        let mut next: Option<BlockId> = None;
        for &i in bucket.iter() {
            let mut t = W::new();
            t.u32(s.bones[i].tag as u32).u32(i as u32).ptr(next);
            next = Some(b.add(Seg::Sys, t));
        }
        tw.ptr(next);
    }
    let tags = b.add(Seg::Sys, tw);
    // bones (16-byte header, then 80 bytes each)
    let names: Vec<BlockId> = s.bones.iter().map(|bn| b.string(&bn.name)).collect();
    let bones_blk = b.reserve(Seg::Sys);
    let mut bw = W::new();
    bw.u32(n as u32).u32(0).u32(0).u32(0);
    for (i, bn) in s.bones.iter().enumerate() {
        bw.v4(bn.rot).v3(bn.trans).u32(0).v3([1.0, 1.0, 1.0]).f32(1.0);
        bw.u16(bn.sibling as u16).u16(bn.parent as u16).u32(0).ptr(Some(names[i]));
        bw.u16(bn.flags).u16(i as u16).u16(bn.tag).u16(i as u16).u64(0);
    }
    b.set(bones_blk, bw);
    // transformations
    let world = world_mats(&s.bones);
    let mut inv = W::new();
    let mut loc = W::new();
    for (i, bn) in s.bones.iter().enumerate() {
        put_matrix(&mut inv, &m_inv(&world[i]), [0.0; 4]);
        put_matrix(&mut loc, &m_trs(bn.trans, bn.rot), bn.unk);
    }
    let inv = b.add(Seg::Sys, inv);
    let loc = b.add(Seg::Sys, loc);
    let mut pw = W::new();
    for bn in &s.bones {
        pw.u16(bn.parent as u16);
    }
    let parents = b.add(Seg::Sys, pw);
    let ci = child_indices(&s.bones);
    let mut cw = W::new();
    for c in &ci {
        cw.u16(*c as u16);
    }
    let childs = (!ci.is_empty()).then(|| b.add(Seg::Sys, cw));

    let mut w = W::new();
    w.u32(VFT_SKELETON).u32(1).u64(0);
    w.ptr(Some(tags)).u16(nb as u16).u16(n.min(nb) as u16).u32(s.sig[0]);
    w.ptr_at(Some(bones_blk), 16).ptr(Some(inv)).ptr(Some(loc)).ptr(Some(parents)).ptr(childs);
    w.u64(0).u32(s.sig[1]).u32(s.sig[2]).u32(s.sig[3]);
    w.u16(1).u16(n as u16).u16(ci.len() as u16).u16(0).u32(0).u64(0);
    debug_assert_eq!(w.len(), 112);
    b.set(id, w);
    id
}

// ------------------------------------------------------------ geometry

/// DrawableGeometry (152 bytes) into `w`; buffers and bone ids as blocks.
fn write_geom(b: &mut Builder, w: &mut W, g: &SkinGeom, nbones: usize) -> Result<(), String> {
    let nv = g.verts.len();
    if nv > 65535 {
        return Err(format!("geometry has {nv} vertices (max 65535)"));
    }
    let mut vb = Vec::with_capacity(nv * SKIN_STRIDE);
    for v in &g.verts {
        for f in v.pos {
            vb.extend_from_slice(&f.to_le_bytes());
        }
        vb.extend_from_slice(&v.weights);
        vb.extend_from_slice(&v.bones);
        for f in v.normal {
            vb.extend_from_slice(&f.to_le_bytes());
        }
        vb.extend_from_slice(&[255, 255, 255, 255]); // Colour0
        vb.extend_from_slice(&[0, 0, 0, 0]); // Colour1
        for f in v.uv.iter().chain(&v.uv) {
            vb.extend_from_slice(&f.to_le_bytes());
        }
        for f in v.tangent {
            vb.extend_from_slice(&f.to_le_bytes());
        }
    }
    let vdata = b.sys_bytes(vb);
    let mut d = W::new();
    d.u32(SKIN_FLAGS).u16(SKIN_STRIDE as u16).u8(0).u8(9).u64(GTAV1);
    let decl = b.add(Seg::Sys, d);
    let mut v = W::new();
    v.u32(VFT_VB).u32(1).u16(SKIN_STRIDE as u16).u16(0).u32(0);
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

    // bone palette: identity over the skeleton (vertex bones index it)
    let mut bi = W::new();
    for i in 0..nbones {
        bi.u16(i as u16);
    }
    let bids = b.add(Seg::Sys, bi);

    let ni = g.indices.len() as u32;
    let start = w.len();
    w.u32(VFT_GEOM).u32(1).u64(0).u64(0).ptr(Some(vbuf)).u64(0).u64(0).u64(0);
    w.ptr(Some(ibuf)).u64(0).u64(0).u64(0);
    w.u32(ni).u32(ni / 3).u16(nv as u16).u16(3).u32(0);
    w.ptr(Some(bids)).u16(SKIN_STRIDE as u16).u16(nbones as u16).u32(0);
    w.ptr(Some(vdata)).u64(0).u64(0).u64(0);
    debug_assert_eq!(w.len() - start, 152);
    Ok(())
}

/// DrawableModelsBlock with one skinned high-LOD model.
fn add_models(b: &mut Builder, d: &SkinDrawable, nbones: usize) -> Result<(BlockId, usize), String> {
    let id = b.reserve(Seg::Sys);
    let n = d.geoms.len();
    let aabbs: Vec<_> = d.geoms.iter().map(|g| aabb(g.verts.iter().map(|v| v.pos))).collect();
    let mut w = W::new();
    w.ptr_at(Some(id), 16).u16(1).u16(1).u32(0);
    let model_off = 16 + 8 + pad16(16 + 8);
    w.ptr_at(Some(id), model_off as u64);
    w.align(16);
    let mut off = model_off + 48;
    let shmap = off;
    off += n * 2;
    off += if n == 1 { 6 } else { pad16(off) };
    let geoptrs = off;
    off += n * 8;
    off += pad16(off);
    let bounds = off;
    let nb = n + usize::from(n > 1);
    off += nb * 32;
    let mut gofs = Vec::with_capacity(n);
    for _ in 0..n {
        off += pad16(off);
        gofs.push(off);
        off += 152;
    }
    w.u32(VFT_MODEL).u32(1).ptr_at(Some(id), geoptrs as u64);
    w.u16(n as u16).u16(n as u16).u32(0);
    w.ptr_at(Some(id), bounds as u64).ptr_at(Some(id), shmap as u64);
    // SkeletonBinding: bone count, HasSkin 1, bone index 0; RenderMask 255, Flags 1
    w.u32((nbones.min(255) as u32) | (1 << 8)).u16(0x01FF).u16(n as u16);
    for g in &d.geoms {
        w.u16(g.shader as u16);
    }
    if n == 1 {
        w.zeros(6);
    } else {
        w.align(16);
    }
    for &g in &gofs {
        w.ptr_at(Some(id), g as u64);
    }
    w.align(16);
    let mut bl = Vec::new();
    if n > 1 {
        bl.push(aabb(aabbs.iter().flat_map(|a| [a.0, a.1])));
    }
    bl.extend_from_slice(&aabbs);
    for (lo, hi) in bl {
        w.v3(lo).f32(lo[0]).v3(hi).f32(hi[0]);
    }
    for g in &d.geoms {
        w.align(16);
        write_geom(b, &mut w, g, nbones)?;
    }
    let len = w.len();
    b.set(id, w);
    Ok((id, len))
}

/// DrawableBase (0xA8 bytes) of `d` into `w`.
fn drawable_base(b: &mut Builder, w: &mut W, vft: u32, d: &SkinDrawable, skel: Option<BlockId>, nbones: usize, lod: f32, file_root: bool) -> Result<(), String> {
    if d.geoms.is_empty() {
        return Err(format!("{}: no geometry", d.name));
    }
    let pages = if file_root {
        let mut p = W::new();
        p.u32(0).u32(0).u8(128).u8(0).u16(0).u32(0).zeros(8 * 128);
        let p = b.add(Seg::Sys, p);
        b.pages_info = Some(p);
        Some(p)
    } else {
        None
    };
    let sg = add_shader_group(b, &d.shaders, None);
    let (lo, hi) = aabb(d.geoms.iter().flat_map(|g| g.verts.iter().map(|v| v.pos)));
    let c = [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0, (lo[2] + hi[2]) / 2.0];
    let r = d.geoms.iter().flat_map(|g| g.verts.iter()).map(|v| (0..3).map(|k| (v.pos[k] - c[k]).powi(2)).sum::<f32>()).fold(0f32, f32::max).sqrt();
    let (models, mlen) = add_models(b, d, nbones)?;
    w.u32(vft).u32(1).ptr(pages);
    w.ptr(Some(sg)).ptr(skel);
    w.v3(c).f32(r);
    w.v3(lo).u32(0x7f800001).v3(hi).u32(0x7f800001);
    w.ptr(Some(models)).u64(0).u64(0).u64(0);
    w.f32(lod).f32(lod).f32(lod).f32(9998.0);
    // FlagsHigh 1, RenderMaskHigh 0xFF: as vanilla ped drawables (yft and
    // ydd, cs_amandatownley); mask 0 left the ped in no render phase
    w.u32(0xFF01).u32(0).u32(0).u32(0);
    w.u64(0).u16(0).u16(mlen.div_ceil(16) as u16).u32(0);
    w.ptr(Some(models));
    debug_assert_eq!(w.len(), 0xA8);
    Ok(())
}

/// A Drawable (208 bytes) as a dictionary member.
fn add_drawable(b: &mut Builder, d: &SkinDrawable, nbones: usize) -> Result<BlockId, String> {
    let id = b.reserve(Seg::Sys);
    let name = b.string(&d.name);
    let mut w = W::new();
    drawable_base(b, &mut w, VFT_DRAWABLE, d, None, nbones, 100.0, false)?;
    w.ptr(Some(name)).u64(0).u16(0).u16(0).u32(0).u64(0).u64(0);
    debug_assert_eq!(w.len(), 208);
    b.set(id, w);
    Ok(id)
}

/// .ydd: drawables sorted by name hash (resource version 165).
pub fn build_ydd(drawables: &[SkinDrawable], nbones: usize) -> Result<Vec<u8>, String> {
    let mut b = Builder::new();
    let root = b.reserve(Seg::Sys);
    let mut p = W::new();
    p.u32(0).u32(0).u8(128).u8(0).u16(0).u32(0).zeros(8 * 128);
    let pages = b.add(Seg::Sys, p);
    b.pages_info = Some(pages);
    let mut order: Vec<&SkinDrawable> = drawables.iter().collect();
    order.sort_by_key(|d| joaat(&d.name));
    let mut hw = W::new();
    for d in &order {
        hw.u32(joaat(&d.name));
    }
    let hashes = b.add(Seg::Sys, hw);
    let ids: Vec<BlockId> = order.iter().map(|d| add_drawable(&mut b, d, nbones)).collect::<Result<_, _>>()?;
    let mut aw = W::new();
    for &i in &ids {
        aw.ptr(Some(i));
    }
    let arr = b.add(Seg::Sys, aw);
    let n = ids.len() as u16;
    let mut w = W::new();
    w.u32(VFT_DICT).u32(1).ptr(Some(pages));
    w.u64(0).u64(1);
    w.ptr(Some(hashes)).u16(n).u16(n).u32(0);
    w.ptr(Some(arr)).u16(n).u16(n).u32(0);
    b.set(root, w);
    b.build(165)
}

/// Skeleton-only .yft (resource version 162) as GTA V ped .yft files are:
/// FragType + FragDrawable "skel" with the skeleton and a placeholder
/// skinned quad, no physics.
pub fn build_yft(name: &str, s: &Skeleton) -> Result<Vec<u8>, String> {
    let mut b = Builder::new();
    let root = b.reserve(Seg::Sys);
    let mut p = W::new();
    p.u32(0).u32(0).u8(128).u8(0).u16(0).u32(0).zeros(8 * 128);
    let pages = b.add(Seg::Sys, p);
    b.pages_info = Some(pages);
    let nbones = s.bones.len();
    let quad = SkinGeom {
        verts: [[0.5f32, -0.5], [0.5, 0.5], [-0.5, -0.5], [-0.5, 0.5]]
            .iter()
            .map(|&[x, y]| SkinVert { pos: [x, y, 0.0], normal: [0.0, 0.0, 1.0], uv: [0.0, 1.0], tangent: [1.0, 0.0, 0.0, 1.0], weights: [255, 0, 0, 0], bones: [0; 4] })
            .collect(),
        indices: vec![0, 1, 2, 3, 2, 1],
        shader: 0,
    };
    let d = SkinDrawable { name: "skel".into(), shaders: vec![ShaderDef::plain()], geoms: vec![quad] };
    let skel = add_skeleton(&mut b, s);
    let did = b.reserve(Seg::Sys);
    let dname = b.string("skel");
    let mut w = W::new();
    drawable_base(&mut b, &mut w, VFT_FRAGDRAWABLE, &d, Some(skel), nbones, 9998.0, false)?;
    w.u64(0);
    for k in 0..16 {
        w.f32(if k % 5 == 0 && k < 15 { 1.0 } else { 0.0 });
    }
    w.u64(0); // bound
    w.u64(0).u16(0).u16(0).u32(0); // frag matrix ids
    w.u64(0).u16(0).u16(1).u32(0); // frag matrices
    w.u64(0).u64(0).u64(0);
    w.ptr(Some(dname)).u64(0).u64(0).u64(0);
    debug_assert_eq!(w.len(), 336);
    b.set(did, w);
    // FragBoneTransforms: one zero 3x4 per bone
    let mut t = W::new();
    t.u64(0).u64(0).u8(nbones as u8).u8(nbones as u8).u16(0).u32(0).u64(0);
    t.zeros(48 * nbones);
    let bt = b.add(Seg::Sys, t);
    let nm = b.string(&format!("pack:/{name}"));
    let mut w = W::new();
    w.u32(VFT_FRAGTYPE).u32(1).ptr(Some(pages));
    w.u64(0).u64(0);
    w.v3([0.0; 3]).f32(17.67767);
    w.ptr(Some(did)).u64(0).u64(0).u32(0).i32(-1);
    w.u64(0).ptr(Some(nm));
    w.u64(0).u16(0).u16(0).u32(0); // cloths list
    for _ in 0..7 {
        w.u64(0);
    }
    w.ptr(Some(bt));
    w.i32(0).i32(0).i32(0).i32(0).i32(256).i32(1).i32(-1).f32(0.0);
    w.f32(1.0).f32(1.0); // gravity, buoyancy
    w.u8(0).u8(0).u16(0).u32(0);
    w.u64(0).u64(0).u64(0).u64(0).u64(0).u64(0);
    w.u64(0).u16(0).u16(0).u32(0); // light attributes
    w.u64(0).u64(0);
    debug_assert_eq!(w.len(), 304);
    b.set(root, w);
    b.build(162)
}
