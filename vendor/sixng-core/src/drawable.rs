//! Drawable / fragment resources (".#dr", ".#ft", ...).
//!
//! Geometry lives in "mesh blobs" (graphics buffers starting with u32 0x4042):
//! a 0x80-byte header holding a vertex-stream table (slot k at +4k:
//! `offset_in_2_byte_units << 8 | format`), a position base (f32 x3 at +0x60)
//! and a quantisation step (f32 x3 at +0x70). Triangles live in a separate
//! "meshlet" buffer: descriptors, 7-bit local indices packed in u32s and a u16
//! vertex remap.
use crate::resource::{find_resource_name, Resource};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GfxBuffer {
    pub struct_off: u64,
    pub gfx_off: u64,
    pub bytes: u64,
    pub flags: u32,
    pub kind_hint: u16,
    /// Present when the buffer is a 0x4042 mesh blob.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mesh_blob: Option<MeshBlobHeader>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshBlobHeader {
    /// (slot, byte offset, format code) for every non-empty vertex stream.
    /// Slot 0 is the position stream (always at 0x80, format 0x42).
    pub sections: Vec<(u32, u32, u32)>,
    pub position_base: [f32; 3],
    pub position_step: [f32; 3],
    pub w: f32,
    pub unk_5c: u32,
    /// Vertex count: positions are 8 bytes each from 0x80 up to the next
    /// stream (12 bytes from 0x60 for float positions).
    pub vertex_count_guess: Option<u32>,
    /// Slot 0 is format 0x63 (3 x f32, unquantised) at 0x60 instead of
    /// packed u16 at 0x80. Seen on cable drawables.
    #[serde(default)]
    pub float_positions: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DrawableInfo {
    pub name: Option<String>,
    pub tag: Option<String>,
    /// Floats at +0x2C..+0x4C of the root object (radius?, bbox min, bbox max).
    /// Meaningless on fragments: use `bounds`.
    pub bounds_raw: Vec<f32>,
    /// Model-space bbox [min, max] (RAGE, Z-up): the root drawable's, or for
    /// a fragment the drawable that the root's +0x18 pointer reaches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounds: Option<[[f32; 3]; 2]>,
    pub buffers: Vec<GfxBuffer>,
    pub embedded_textures: Vec<crate::texture::TexInfo>,
    /// Mesh blob + index buffer pairs.
    #[serde(default)]
    pub geometries: Vec<Geometry>,
    /// Geometry groups with per-geometry shader indices.
    #[serde(default)]
    pub models: Vec<Model>,
    /// Shader instances (materials); names are hashed.
    #[serde(default)]
    pub shaders: Vec<Shader>,
    /// Skeleton / node hierarchy (vehicles: chassis, wheels, hubs, doors,
    /// steering wheel...). Hierarchy verified; local transforms unverified.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub skeleton: Vec<Bone>,
    /// Identifier-like strings found in the system page (names of bones,
    /// attach points, etc.).
    pub strings: Vec<String>,
}

/// One bone record (0x50 bytes): +0x00 rotation quaternion (x,y,z,w),
/// +0x10 translation (+ u32 hash), +0x20 scale, +0x30 name ptr, +0x40 8×u16
/// (flags, ?, next sibling, parent, index, name hash, index, ?).
/// The skeleton header (found by its pointer to the bone array) is followed
/// by pointers to the inverse bind matrices (+0x08, 4x4 f32, row-vector
/// convention: p_bone = p_model · M), local bind matrices (+0x10) and u16
/// parent indices (+0x18).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bone {
    pub index: u16,
    pub name: String,
    /// Parent bone index, None for the root.
    pub parent: Option<u16>,
    pub next_sibling: Option<u16>,
    pub flags: u16,
    pub hash16: u16,
    /// Local rotation (x, y, z, w) as stored in the bone record.
    pub rotation: [f32; 4],
    /// Local translation as stored in the bone record, metres.
    pub translation: [f32; 3],
    pub scale: [f32; 3],
    /// Bind pose in model space (bone -> model), 4x4 column-major, RAGE
    /// Z-up, from the inverse bind matrices. Verified: wheels, doors and
    /// lights come out symmetric and in place.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bind_matrix: Option<[f32; 16]>,
    /// Translation part of `bind_matrix` (the pivot, model space).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pivot: Option<[f32; 3]>,
}

/// Invert an affine 4x4 column-major matrix (last row 0,0,0,1).
pub fn affine_inverse(m: &[f32; 16]) -> [f32; 16] {
    let a = |r: usize, c: usize| m[c * 4 + r];
    let det = a(0, 0) * (a(1, 1) * a(2, 2) - a(1, 2) * a(2, 1)) - a(0, 1) * (a(1, 0) * a(2, 2) - a(1, 2) * a(2, 0))
        + a(0, 2) * (a(1, 0) * a(2, 1) - a(1, 1) * a(2, 0));
    let id = if det.abs() < 1e-12 { 0.0 } else { 1.0 / det };
    let mut inv = [0f32; 9];
    inv[0] = (a(1, 1) * a(2, 2) - a(1, 2) * a(2, 1)) * id;
    inv[1] = (a(0, 2) * a(2, 1) - a(0, 1) * a(2, 2)) * id;
    inv[2] = (a(0, 1) * a(1, 2) - a(0, 2) * a(1, 1)) * id;
    inv[3] = (a(1, 2) * a(2, 0) - a(1, 0) * a(2, 2)) * id;
    inv[4] = (a(0, 0) * a(2, 2) - a(0, 2) * a(2, 0)) * id;
    inv[5] = (a(0, 2) * a(1, 0) - a(0, 0) * a(1, 2)) * id;
    inv[6] = (a(1, 0) * a(2, 1) - a(1, 1) * a(2, 0)) * id;
    inv[7] = (a(0, 1) * a(2, 0) - a(0, 0) * a(2, 1)) * id;
    inv[8] = (a(0, 0) * a(1, 1) - a(0, 1) * a(1, 0)) * id;
    let t = [a(0, 3), a(1, 3), a(2, 3)];
    let mut o = [0f32; 16];
    for r in 0..3 {
        for c in 0..3 {
            o[c * 4 + r] = inv[r * 3 + c];
        }
        o[12 + r] = -(inv[r * 3] * t[0] + inv[r * 3 + 1] * t[1] + inv[r * 3 + 2] * t[2]);
    }
    o[15] = 1.0;
    o
}

/// Find the bone array: consecutive 0x50-byte records whose index field
/// counts 0, 1, 2... and whose name pointer resolves to a name.
pub fn find_skeleton(r: &Resource) -> Vec<Bone> {
    let ok_name = |s: &str| !s.is_empty() && s.len() < 96 && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_');
    // q = record start; name ptr at +0x30, u16 fields at +0x40.
    let rec = |q: usize, k: u16| -> Option<Bone> {
        let np = r.sys_off(r.u64(q + 0x30))?;
        let name = r.cstr(np)?;
        if !ok_name(&name) || r.u16(q + 0x48) != k {
            return None;
        }
        let opt = |v: u16| if v == 0xffff { None } else { Some(v) };
        Some(Bone {
            index: k,
            name,
            parent: opt(r.u16(q + 0x46)),
            next_sibling: opt(r.u16(q + 0x44)),
            flags: r.u16(q + 0x40),
            hash16: r.u16(q + 0x4a),
            rotation: [r.f32(q), r.f32(q + 4), r.f32(q + 8), r.f32(q + 12)],
            translation: [r.f32(q + 0x10), r.f32(q + 0x14), r.f32(q + 0x18)],
            scale: [r.f32(q + 0x20), r.f32(q + 0x24), r.f32(q + 0x28)],
            bind_matrix: None,
            pivot: None,
        })
    };
    let mut p = 0usize;
    while p + 0x50 <= r.sys.len() {
        if let Some(b0) = rec(p, 0) {
            if b0.parent.is_none() {
                let mut v = vec![b0];
                while let Some(b) = rec(p + v.len() * 0x50, v.len() as u16) {
                    v.push(b);
                }
                if v.len() >= 2 {
                    attach_binds(r, p, &mut v);
                    return v;
                }
            }
        }
        p += 16;
    }
    Vec::new()
}

/// Read the inverse bind matrices through the skeleton header and store
/// each bone's model-space bind matrix.
fn attach_binds(r: &Resource, start: usize, bones: &mut [Bone]) {
    let target = 0x5000_0000u64 | start as u64;
    let n = bones.len();
    let Some(hdr) = (0..r.sys.len().saturating_sub(0x20)).step_by(8).find(|&o| r.u64(o) == target) else { return };
    let Some(ibm) = r.sys_off(r.u64(hdr + 8)) else { return };
    if ibm + 64 * n > r.sys.len() {
        return;
    }
    if let Some(par) = r.sys_off(r.u64(hdr + 0x18)) {
        // sanity: the parent table must agree with the bone records
        let ok = bones.iter().all(|b| {
            let v = r.u16(par + 2 * b.index as usize);
            b.parent.map(|x| x == v).unwrap_or(v == 0xffff)
        });
        if !ok {
            return;
        }
    }
    for b in bones.iter_mut() {
        let o = ibm + 64 * b.index as usize;
        // Stored for row vectors (p · M); read column-major = column-vector form.
        let mut m = [0f32; 16];
        for (k, x) in m.iter_mut().enumerate() {
            *x = r.f32(o + 4 * k);
        }
        m[3] = 0.0;
        m[7] = 0.0;
        m[11] = 0.0;
        m[15] = 1.0;
        let w = affine_inverse(&m);
        b.pivot = Some([w[12], w[13], w[14]]);
        b.bind_matrix = Some(w);
    }
}

fn f32le(b: &[u8], o: usize) -> f32 {
    f32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

pub fn parse_mesh_blob(b: &[u8]) -> Option<MeshBlobHeader> {
    if b.len() < 0x80 {
        return None;
    }
    let float_positions = u32le(b, 0) == 0x3063;
    if u32le(b, 0) != 0x4042 && !float_positions {
        return None;
    }
    let (p0, stride) = if float_positions { (0x60u32, 12u32) } else { (0x80, 8) };
    let mut sections = Vec::new();
    for slot in 0..23u32 {
        let v = u32le(b, (slot * 4) as usize);
        if v != 0 && slot * 4 < 0x5c {
            sections.push((slot, (v >> 8) * 2, v & 0xff));
        }
    }
    let vertex_count_guess = sections
        .iter()
        .map(|s| s.1)
        .filter(|&o| o > p0 && (o as usize) <= b.len())
        .min()
        .map(|o| (o - p0) / stride);
    Some(MeshBlobHeader {
        sections,
        position_base: if float_positions { [0.0; 3] } else { [f32le(b, 0x60), f32le(b, 0x64), f32le(b, 0x68)] },
        position_step: if float_positions { [0.0; 3] } else { [f32le(b, 0x70), f32le(b, 0x74), f32le(b, 0x78)] },
        w: if float_positions { 0.0 } else { f32le(b, 0x7c) },
        unk_5c: u32le(b, 0x5c),
        vertex_count_guess,
        float_positions,
    })
}

pub fn find_buffers(r: &Resource) -> Vec<GfxBuffer> {
    let mut v = Vec::new();
    let mut p = 0usize;
    while p + 0x20 <= r.sys.len() {
        let ptr = r.u64(p + 0x18);
        let k = r.u16(p + 0x0E);
        if ptr >> 28 == 6 && r.u16(p + 0x0C) == 4 && (k == 8 || k == 9) {
            let dw = r.u32(p + 8) as u64;
            if let Some(go) = r.gfx_off(ptr) {
                let bytes = dw * 4;
                let mesh_blob = r.gfx.get(go..(go + bytes as usize).min(r.gfx.len())).and_then(parse_mesh_blob);
                v.push(GfxBuffer {
                    struct_off: p as u64,
                    gfx_off: go as u64,
                    bytes,
                    flags: r.u32(p + 0x10),
                    kind_hint: k,
                    mesh_blob,
                });
                p += 0x20;
                continue;
            }
        }
        p += 8;
    }
    v
}

pub fn strings(sys: &[u8], min: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = Vec::new();
    for &c in sys {
        if (0x20..0x7f).contains(&c) {
            cur.push(c);
        } else {
            if cur.len() >= min && c == 0 {
                let s = String::from_utf8_lossy(&cur).to_string();
                if s.chars().any(|c| c.is_ascii_alphabetic()) && !out.contains(&s) {
                    out.push(s);
                }
            }
            cur.clear();
        }
    }
    out
}

/// Model-space bbox of a drawable or fragment resource (system page only).
/// Drawables keep it at +0x30 (min) / +0x40 (max) of the root. A fragment
/// root has other data there; its drawable is at the pointer in +0x18.
pub fn drawable_bounds(r: &Resource) -> Option<[[f32; 3]; 2]> {
    let at = |o: usize| -> Option<[[f32; 3]; 2]> {
        if r.sys.len() < o + 0x4c {
            return None;
        }
        let b = [[r.f32(o + 0x30), r.f32(o + 0x34), r.f32(o + 0x38)], [r.f32(o + 0x40), r.f32(o + 0x44), r.f32(o + 0x48)]];
        let sane = |v: f32| v.is_finite() && v.abs() < 1e5 && (v == 0.0 || v.abs() > 1e-20);
        let ok = b.iter().flatten().all(|&v| sane(v)) && (0..3).all(|k| b[1][k] >= b[0][k]) && (0..3).any(|k| b[1][k] - b[0][k] > 1e-3);
        ok.then_some(b)
    };
    if find_resource_name(&r.sys).map_or(false, |t| t.1 == "ft") {
        return r.sys_off(r.u64(0x18)).and_then(at).or_else(|| at(0));
    }
    at(0)
}

pub fn describe(r: &Resource) -> DrawableInfo {
    let nt = find_resource_name(&r.sys);
    let bounds_raw = if r.sys.len() >= 0x4c { (0..8).map(|i| r.f32(0x2c + i * 4)).collect() } else { vec![] };
    let buffers = find_buffers(r);
    let geometries = find_geometries(r, &buffers);
    DrawableInfo {
        name: nt.as_ref().map(|x| x.0.clone()),
        tag: nt.map(|x| x.1),
        bounds_raw,
        bounds: drawable_bounds(r),
        models: find_models(r, &geometries),
        shaders: find_shaders(r),
        geometries,
        buffers,
        embedded_textures: crate::texture::find_textures(r),
        skeleton: find_skeleton(r),
        strings: strings(&r.sys, 4)
            .into_iter()
            .filter(|s| s.len() >= 4 && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'.' || c == b'#') && s.bytes().any(|c| c.is_ascii_lowercase()))
            .take(4000)
            .collect(),
    }
}

/// Decode the packed positions of a mesh blob. Each vertex is one u64 at
/// 0x80 + 8*i. x and y are 24-bit: high 16 bits at 16..31 (x) and 32..47
/// (y), low bytes at 0..7 (x) and 8..15 (y). z is 16-bit at 48..63.
pub fn decode_positions(b: &[u8], h: &MeshBlobHeader) -> Vec<[f32; 3]> {
    let n = h.vertex_count_guess.unwrap_or(0) as usize;
    let mut v = Vec::with_capacity(n);
    if h.float_positions {
        for i in 0..n {
            let o = 0x60 + 12 * i;
            if o + 12 > b.len() { break }
            v.push([f32le(b, o), f32le(b, o + 4), f32le(b, o + 8)]);
        }
        return v;
    }
    for i in 0..n {
        let o = 0x80 + 8 * i;
        let Some(q) = b.get(o..o + 8).map(|x| u64::from_le_bytes(x.try_into().unwrap())) else { break };
        let (bx, sx) = (h.position_base, h.position_step);
        v.push([
            ((((q >> 16) & 0xffff) << 8) | (q & 0xff)) as f32 * sx[0] + bx[0],
            ((((q >> 32) & 0xffff) << 8) | ((q >> 8) & 0xff)) as f32 * sx[1] + bx[1],
            (q >> 48) as f32 * sx[2] + bx[2],
        ]);
    }
    v
}

/// A decoded mesh (one geometry), RAGE space (Z-up, metres).
#[derive(Debug, Clone, Default)]
pub struct Mesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Option<Vec<[f32; 3]>>,
    /// (vertex-stream slot, UVs). Slots 3, 4, 14, 15 = UV0..UV3.
    pub uvs: Vec<(u32, Vec<[f32; 2]>)>,
    /// Slot 1 RGBA8 (vertex colour).
    pub colors: Option<Vec<[u8; 4]>>,
    pub indices: Vec<u32>,
    /// Triangles whose winding was flipped to agree with stored normals.
    pub flipped: usize,
    /// Triangles dropped (index out of range or degenerate).
    pub dropped: usize,
    /// Vertices whose stored normal word held no normal (bit 31 clear);
    /// their normals were rebuilt from the faces around them.
    pub rebuilt_normals: usize,
    /// Model axis held by each quantised position slot (x, y, z slot).
    /// [0, 1, 2] = identity. The encoder puts the flattest axis in the
    /// 16-bit z slot and rotates the others cyclically.
    pub axis_order: [usize; 3],
    /// Skin joints per vertex (skeleton bone indices). Stream 8: format
    /// 0x25 = 4 x u8 blend indices; format 0x00 is sized per vertex:
    /// 1 byte = one u8 bone, 2 = one u16 bone (rigid), 4 = 4 x u8,
    /// 8 = 4 x u16 blend indices.
    pub joints: Option<Vec<[u16; 4]>>,
    /// Skin weights per vertex (u8, sum 255). Stream 6 (format 0x26) when
    /// stream 8 has 4 indices; otherwise (255, 0, 0, 0).
    pub weights: Option<Vec<[u8; 4]>>,
    /// Second influence set (influences 5-8): stream 9 (format 0x25, 4 x u8
    /// joint indices) and stream 7 (format 0x26, weights). Seen on ped
    /// meshes; the 8 weights together sum to 255 and are sorted descending.
    pub joints1: Option<Vec<[u16; 4]>>,
    pub weights1: Option<Vec<[u8; 4]>>,
    /// Tangents (x, y, z, w), model space like the normals: stream 11,
    /// format 0x27 = R10G10B10A2 (xyz = v / 1023 * 2 - 1, A2 = 3 when the
    /// bitangent is flipped, so w = -1). Orthonormalised against the normal.
    pub tangents: Option<Vec<[f32; 4]>>,
}

/// Half-width (m) given to cable ribbons, which the game widens in its
/// cable shader. Power and phone lines are a few centimetres thick.
pub const CABLE_RADIUS: f32 = 0.02;

/// Streams that hold texture coordinates, in UV-set order.
pub const UV_SLOTS: [u32; 4] = [3, 4, 14, 15];

/// Stream 11 (format 0x27): R10G10B10A2 tangent. Checked against tangents
/// computed from UV0 (mean |dot| 1.000) and handedness (A2 vs UV winding,
/// correlation 0.999) on a map piece.
fn tangent_decode(w: u32, n: Option<[f32; 3]>) -> [f32; 4] {
    let c = |s: u32| ((w >> s) & 1023) as f32 / 1023.0 * 2.0 - 1.0;
    let mut t = [c(0), c(10), c(20)];
    if let Some(n) = n {
        let d = t[0] * n[0] + t[1] * n[1] + t[2] * n[2];
        for k in 0..3 {
            t[k] -= d * n[k];
        }
        if t.iter().map(|x| x * x).sum::<f32>() < 1e-8 {
            // Degenerate: any unit vector perpendicular to the normal.
            t = if n[0].abs() < 0.9 { [1.0 - n[0] * n[0], -n[0] * n[1], -n[0] * n[2]] } else { [-n[1] * n[0], 1.0 - n[1] * n[1], -n[1] * n[2]] };
        }
    }
    let l = t.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
    let sign = if (w >> 31) & 1 == 1 { -1.0 } else { 1.0 };
    [t[0] / l, t[1] / l, t[2] / l, sign]
}

/// Normal word (stream 2, format 0x21): octahedral x in bits 0-9, y in
/// bits 10-19. Bit 31 is set on words that hold a normal. Words with it
/// clear (about a third of the vertices of ped heads, a few per cent of
/// other skinned meshes, almost none on static ones) decode to directions
/// that disagree with the surface in ~75 % of cases (checked against the
/// face normals of `cs_ali` / `crr_iguana` heads and bodies, a helicopter
/// and buildings), and no other field layout fits them; they are treated as
/// missing and rebuilt from the faces.
fn octa_decode_checked(w: u32) -> Option<[f32; 3]> {
    if w >> 31 == 0 {
        return None;
    }
    Some(octa_decode(w))
}

fn octa_decode(w: u32) -> [f32; 3] {
    let x = (w & 1023) as f32 / 1023.0 * 2.0 - 1.0;
    let y = ((w >> 10) & 1023) as f32 / 1023.0 * 2.0 - 1.0;
    let z = 1.0 - x.abs() - y.abs();
    let t = (-z).max(0.0);
    let x = x + if x >= 0.0 { -t } else { t };
    let y = y + if y >= 0.0 { -t } else { t };
    let l = (x * x + y * y + z * z).sqrt().max(1e-12);
    [x / l, y / l, z / l]
}

/// Decode the meshlet index buffer into a triangle list (vertex indices).
/// Layout: a 12-byte header (u16 meshlet count, u16 ?, u16 triangle count,
/// u16 0, u16 remap count, u16 0); then 16-byte meshlet descriptors from
/// 0x0C (u32 ?, u16 remap start, u16 triangle start, ...); then one u32 per
/// triangle (7 bits per local index, meshlet number from bit 21); then the
/// u16 remap list (local -> vertex index).
pub fn decode_meshlets(ib: &[u8], vertex_count: usize) -> (Vec<u32>, usize) {
    let u16at = |o: usize| ib.get(o..o + 2).map(|x| u16::from_le_bytes([x[0], x[1]]) as usize);
    let (Some(n), Some(ntri), Some(nremap)) = (u16at(0), u16at(4), u16at(8)) else { return (vec![], 0) };
    let tri0 = 0x0c + 16 * n;
    let rem0 = tri0 + 4 * ntri;
    let starts: Vec<usize> = (0..n).map(|k| u16at(0x10 + 16 * k).unwrap_or(0)).collect();
    let nremap = nremap.min(ib.len().saturating_sub(rem0) / 2);
    let mut idx = Vec::with_capacity(ntri * 3);
    let mut dropped = 0;
    for t in 0..ntri {
        let Some(w) = ib.get(tri0 + 4 * t..tri0 + 4 * t + 4).map(|x| u32::from_le_bytes(x.try_into().unwrap())) else { break };
        let ml = (w >> 21) as usize;
        let Some(&base) = starts.get(ml) else {
            dropped += 1;
            continue;
        };
        let mut tri = [0u32; 3];
        let mut ok = true;
        for (k, sh) in [0u32, 7, 14].iter().enumerate() {
            let r = base + ((w >> sh) & 0x7f) as usize;
            match if r < nremap { u16at(rem0 + 2 * r) } else { None } {
                Some(v) if v < vertex_count => tri[k] = v as u32,
                _ => ok = false,
            }
        }
        if ok && tri[0] != tri[1] && tri[1] != tri[2] && tri[0] != tri[2] {
            idx.extend_from_slice(&tri);
        } else {
            dropped += 1;
        }
    }
    (idx, dropped)
}

/// Decode a full mesh from a mesh blob and its meshlet index buffer.
pub fn decode_mesh(blob: &[u8], h: &MeshBlobHeader, ib: &[u8]) -> Mesh {
    let mut positions = decode_positions(blob, h);
    let v = positions.len();
    let stream = |slot: u32| h.sections.iter().find(|s| s.0 == slot).map(|s| (s.1 as usize, s.2));
    // Vertices whose normal word has no usable normal get a zero normal here
    // (left out of the axis and winding checks) and are rebuilt from the
    // faces at the end.
    let mut invalid: Vec<usize> = Vec::new();
    let mut normals = stream(2).filter(|s| s.1 == 0x21).and_then(|s| blob.get(s.0..s.0 + 4 * v)).map(|b| {
        b.chunks_exact(4)
            .enumerate()
            .map(|(i, c)| match octa_decode_checked(u32::from_le_bytes(c.try_into().unwrap())) {
                Some(n) => n,
                None => {
                    invalid.push(i);
                    [0.0; 3]
                }
            })
            .collect::<Vec<_>>()
    });
    let mut uvs = Vec::new();
    for slot in UV_SLOTS {
        let Some((off, fmt)) = stream(slot) else { continue };
        let uv: Option<Vec<[f32; 2]>> = match fmt {
            // 2 x s16, 1.0 = 8192
            0x28 => blob.get(off..off + 4 * v).map(|b| {
                b.chunks_exact(4)
                    .map(|c| [i16::from_le_bytes([c[0], c[1]]) as f32 / 8192.0, i16::from_le_bytes([c[2], c[3]]) as f32 / 8192.0])
                    .collect()
            }),
            // 2 x f32
            0x42 => blob.get(off..off + 8 * v).map(|b| {
                b.chunks_exact(8)
                    .map(|c| [f32::from_le_bytes(c[0..4].try_into().unwrap()), f32::from_le_bytes(c[4..8].try_into().unwrap())])
                    .collect()
            }),
            _ => None,
        };
        if let Some(uv) = uv {
            uvs.push((slot, uv));
        }
    }
    let colors = stream(1)
        .filter(|s| s.1 == 0x26)
        .and_then(|s| blob.get(s.0..s.0 + 4 * v))
        .map(|b| b.chunks_exact(4).map(|c| [c[0], c[1], c[2], c[3]]).collect());
    let (mut indices, dropped) = decode_meshlets(ib, v);
    // Stream 8 is sized per vertex: the next section's offset (or the blob
    // end) minus this one, over the vertex count, gives its width.
    let width = |slot: u32| -> usize {
        let Some((o, _)) = stream(slot) else { return 0 };
        let next = h.sections.iter().map(|s| s.1 as usize).filter(|&x| x > o).min().unwrap_or(blob.len());
        if v == 0 { 0 } else { (next.min(blob.len()) - o) / v }
    };
    let u16le = |c: &[u8]| u16::from_le_bytes([c[0], c[1]]);
    let jw = width(8);
    let joints: Option<Vec<[u16; 4]>> = match stream(8) {
        Some((o, 0x25)) => blob.get(o..o + 4 * v).map(|b| b.chunks_exact(4).map(|c| [c[0] as u16, c[1] as u16, c[2] as u16, c[3] as u16]).collect()),
        // one u16 bone per vertex (rigid, skeletons over 255 bones)
        Some((o, 0x00)) if (2..4).contains(&jw) => blob.get(o..o + 2 * v).map(|b| b.chunks_exact(2).map(|c| [u16le(c), 0, 0, 0]).collect()),
        // 4 x u16 blend indices
        Some((o, 0x00)) if (8..16).contains(&jw) => blob.get(o..o + 8 * v).map(|b| b.chunks_exact(8).map(|c| [u16le(&c[0..]), u16le(&c[2..]), u16le(&c[4..]), u16le(&c[6..])]).collect()),
        // 4 x u8 blend indices in a format-0 stream
        Some((o, 0x00)) if (4..8).contains(&jw) => blob.get(o..o + 4 * v).map(|b| b.chunks_exact(4).map(|c| [c[0] as u16, c[1] as u16, c[2] as u16, c[3] as u16]).collect()),
        Some((o, 0x00)) if jw < 2 => blob.get(o..o + v).map(|b| b.iter().map(|&j| [j as u16, 0, 0, 0]).collect()),
        _ => None,
    };
    let blend = joints.is_some() && (matches!(stream(8).map(|s| s.1), Some(0x25)) || (4..16).contains(&jw));
    let weights: Option<Vec<[u8; 4]>> = joints.as_ref().map(|j| {
        if blend {
            stream(6)
                .filter(|s| s.1 == 0x26)
                .and_then(|s| blob.get(s.0..s.0 + 4 * v))
                .map(|b| b.chunks_exact(4).map(|c| [c[0], c[1], c[2], c[3]]).collect())
                .unwrap_or_else(|| vec![[255, 0, 0, 0]; j.len()])
        } else {
            vec![[255, 0, 0, 0]; j.len()]
        }
    });
    // Influences 5-8 (peds): only next to a 4-index blend set.
    let quad = |slot: u32, fmt: u32| stream(slot).filter(|s| s.1 == fmt).and_then(|s| blob.get(s.0..s.0 + 4 * v)).map(|b| b.chunks_exact(4).map(|c| [c[0], c[1], c[2], c[3]]).collect::<Vec<[u8; 4]>>());
    let (joints1, weights1) = match (blend && matches!(stream(8).map(|s| s.1), Some(0x25)), quad(9, 0x25), quad(7, 0x26)) {
        (true, Some(j), Some(w)) if w.iter().any(|x| x.iter().any(|&y| y > 0)) => (Some(j.iter().map(|c| c.map(|x| x as u16)).collect::<Vec<_>>()), Some(w)),
        _ => (None, None),
    };
    if std::env::var_os("SIXNG_DEBUG_SKIN").is_some() {
        eprintln!("skin: v={v} stream8 fmt={:?} width={jw} max={:?}", stream(8).map(|s| s.1), joints.as_ref().map(|j| j.iter().flatten().max().copied()));
    }
    // Undo the axis rotation of the position quantiser. The header's u32 at
    // +0x6C (2 = identity, 1 / 0 = rotated) predicts it in most cases but not
    // all, so pick the cyclic order whose faces best agree with the stored
    // (model-space) normals, preferring the header's choice on near-ties.
    // Cables: float positions, slot 2 = unit direction along the wire, UV0.x
    // = +-1 = which side the shader pushes the vertex to. Each vertex pair
    // shares a position, so the stored ribbons have zero width. Widen them
    // here into two crossed ribbons (horizontal and vertical) so the wire is
    // visible without the cable shader.
    if h.float_positions {
        let dir: Option<Vec<[f32; 3]>> = stream(2).filter(|s| s.1 == 0x63).and_then(|s| blob.get(s.0..s.0 + 12 * v)).map(|b| {
            b.chunks_exact(12).map(|c| [f32le(c, 0), f32le(c, 4), f32le(c, 8)]).collect()
        });
        let side: Option<Vec<f32>> = uvs.iter().find(|u| u.0 == 3).map(|u| u.1.iter().map(|x| x[0]).collect());
        if let (Some(dir), Some(side)) = (dir, side) {
            // Every position appears exactly twice (one vertex per side).
            let mut seen: std::collections::HashMap<[u32; 3], u32> = std::collections::HashMap::new();
            for p in &positions {
                *seen.entry(p.map(f32::to_bits)).or_default() += 1;
            }
            let paired = v >= 4 && seen.values().all(|&c| c == 2);
            if paired && side.iter().all(|s| (s.abs() - 1.0).abs() < 1e-3) {
                let norm = |a: [f32; 3]| { let l = (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt(); if l < 1e-6 { None } else { Some([a[0] / l, a[1] / l, a[2] / l]) } };
                let cross = |a: [f32; 3], b: [f32; 3]| [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
                let mut pos2 = Vec::with_capacity(2 * v);
                let mut nrm2 = Vec::with_capacity(2 * v);
                let mut pos_v = Vec::with_capacity(v);
                let mut nrm_v = Vec::with_capacity(v);
                for i in 0..v {
                    let t = norm(dir[i]).unwrap_or([1.0, 0.0, 0.0]);
                    let hz = norm(cross(t, [0.0, 0.0, 1.0])).unwrap_or([1.0, 0.0, 0.0]);
                    let vt = norm(cross(hz, t)).unwrap_or([0.0, 0.0, 1.0]);
                    let (p, s) = (positions[i], side[i] * CABLE_RADIUS);
                    pos2.push([p[0] + s * hz[0], p[1] + s * hz[1], p[2] + s * hz[2]]);
                    nrm2.push(vt);
                    pos_v.push([p[0] + s * vt[0], p[1] + s * vt[1], p[2] + s * vt[2]]);
                    nrm_v.push(hz);
                }
                pos2.extend(pos_v);
                nrm2.extend(nrm_v);
                let n0 = indices.len();
                for k in 0..n0 { let x = indices[k] + v as u32; indices.push(x); }
                for u in uvs.iter_mut() { let c = u.1.clone(); u.1.extend(c); }
                let colors: Option<Vec<[u8; 4]>> = colors.map(|c: Vec<[u8; 4]>| { let mut d = c.clone(); d.extend(c); d });
                return Mesh { positions: pos2, normals: Some(nrm2), uvs, colors, indices, flipped: 0, dropped, axis_order: [0, 1, 2], joints: None, weights: None, joints1: None, weights1: None, tangents: None, rebuilt_normals: 0 };
            }
        }
    }
    // Float-position meshes (water planes) keep 3 x f32 normals in slot 2.
    if normals.is_none() && h.float_positions {
        normals = stream(2).filter(|s| s.1 == 0x63).and_then(|s| blob.get(s.0..s.0 + 12 * v)).map(|b| {
            b.chunks_exact(12).map(|c| { let n = [f32le(c, 0), f32le(c, 4), f32le(c, 8)]; let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt().max(1e-12); [n[0] / l, n[1] / l, n[2] / l] }).collect()
        });
    }
    let mode = if h.float_positions { 2 } else { blob.get(0x6c..0x70).map(|x| u32::from_le_bytes(x.try_into().unwrap())).unwrap_or(2) };
    // The three cyclic orders first (the header hints between them), then
    // the three swaps, which some building drawables use (wall panels
    // came out lying flat with only the cyclic orders).
    let orders: [[usize; 3]; 6] = [[0, 1, 2], [2, 0, 1], [1, 2, 0], [0, 2, 1], [2, 1, 0], [1, 0, 2]];
    // +0x6C is the model axis held in the flat 16-bit slot: 0 = x (slots
    // hold z, y, x: a swap), 1 = y (z, x, y), 2 = z (identity). Checked on
    // dn_14_structure_03_b: the hinted order scores ~1.0 against the normals
    // on every geometry.
    let hinted = match mode { 1 => 1, 0 => 4, _ => 0 };
    let mut axis_order = orders[hinted];
    if let Some(n) = &normals {
        let score = |o: &[usize; 3]| -> f32 {
            let mut sum = 0.0;
            let mut cnt = 0usize;
            for t in indices.chunks_exact(3).step_by(1 + indices.len() / 3 / 4000) {
                let m = |i: u32| { let p = positions[i as usize]; let mut q = [0f32; 3]; for k in 0..3 { q[o[k]] = p[k]; } q };
                let (a, b, c) = (m(t[0]), m(t[1]), m(t[2]));
                let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
                let w = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
                let f = [u[1] * w[2] - u[2] * w[1], u[2] * w[0] - u[0] * w[2], u[0] * w[1] - u[1] * w[0]];
                let nn: Vec<f32> = (0..3).map(|k| n[t[0] as usize][k] + n[t[1] as usize][k] + n[t[2] as usize][k]).collect();
                let lf = (f[0] * f[0] + f[1] * f[1] + f[2] * f[2]).sqrt();
                let ln = (nn[0] * nn[0] + nn[1] * nn[1] + nn[2] * nn[2]).sqrt();
                if lf > 0.0 && ln > 0.0 {
                    sum += ((f[0] * nn[0] + f[1] * nn[1] + f[2] * nn[2]) / (lf * ln)).abs();
                    cnt += 1;
                }
            }
            if cnt == 0 { 0.0 } else { sum / cnt as f32 }
        };
        let sh = score(&orders[hinted]);
        if std::env::var_os("SIXNG_DEBUG_AXIS").is_some() {
            eprintln!("axis: v={v} mode={mode} scores={:?}", orders.iter().map(|o| (score(o) * 1000.0).round() / 1000.0).collect::<Vec<_>>());
        }
        let (bi, bs) = orders.iter().enumerate().map(|(i, o)| (i, score(o))).fold((hinted, sh), |a, b| if b.1 > a.1 + 0.05 { b } else { a });
        let _ = bs;
        axis_order = orders[bi];
    }
    if axis_order != [0, 1, 2] {
        for p in positions.iter_mut() {
            let d = *p;
            for k in 0..3 { p[axis_order[k]] = d[k]; }
        }
    }
    // Orient each triangle so its geometric normal agrees with the stored
    // vertex normals (the stored winding is not consistent with them).
    let mut flipped = 0;
    if let Some(n) = &normals {
        for t in indices.chunks_exact_mut(3) {
            let (a, b, c) = (positions[t[0] as usize], positions[t[1] as usize], positions[t[2] as usize]);
            let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
            let w = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
            let f = [u[1] * w[2] - u[2] * w[1], u[2] * w[0] - u[0] * w[2], u[0] * w[1] - u[1] * w[0]];
            let s: f32 = (0..3).map(|k| f[k] * (n[t[0] as usize][k] + n[t[1] as usize][k] + n[t[2] as usize][k])).sum();
            if s < 0.0 {
                t.swap(1, 2);
                flipped += 1;
            }
        }
    }
    // Rebuild the normals of vertices whose stored word has no usable normal
    // (bit 31 clear, see `octa_decode_checked`) from the oriented faces
    // around them, welding vertices at the same position so UV seams stay
    // smooth.
    let mut rebuilt_normals = 0;
    if let (Some(n), false) = (normals.as_mut(), invalid.is_empty()) {
        let key = |p: [f32; 3]| [(p[0] * 1e4).round() as i64, (p[1] * 1e4).round() as i64, (p[2] * 1e4).round() as i64];
        let mut acc: std::collections::HashMap<[i64; 3], [f32; 3]> = std::collections::HashMap::new();
        for t in indices.chunks_exact(3) {
            let (a, b, c) = (positions[t[0] as usize], positions[t[1] as usize], positions[t[2] as usize]);
            let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
            let w = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
            let f = [u[1] * w[2] - u[2] * w[1], u[2] * w[0] - u[0] * w[2], u[0] * w[1] - u[1] * w[0]];
            for &i in t {
                if invalid.contains(&(i as usize)) {
                    let e = acc.entry(key(positions[i as usize])).or_insert([0.0; 3]);
                    for k in 0..3 {
                        e[k] += f[k];
                    }
                }
            }
        }
        for &i in &invalid {
            if let Some(g) = acc.get(&key(positions[i])) {
                let l = (g[0] * g[0] + g[1] * g[1] + g[2] * g[2]).sqrt();
                if l > 0.0 {
                    n[i] = [g[0] / l, g[1] / l, g[2] / l];
                    rebuilt_normals += 1;
                }
            }
        }
    }
    let tangents = stream(11).filter(|s| s.1 == 0x27).and_then(|s| blob.get(s.0..s.0 + 4 * v)).map(|b| {
        b.chunks_exact(4)
            .enumerate()
            .map(|(i, c)| tangent_decode(u32::from_le_bytes(c.try_into().unwrap()), normals.as_ref().map(|n| n[i])))
            .collect::<Vec<_>>()
    });
    Mesh { positions, normals, uvs, colors, indices, flipped, dropped, axis_order, joints, weights, joints1, weights1, tangents, rebuilt_normals }
}

/// A geometry: a mesh blob paired with its meshlet index buffer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Geometry {
    /// Index into `DrawableInfo::buffers` of the mesh blob.
    pub vertex_buffer: usize,
    /// Index into `DrawableInfo::buffers` of the meshlet index buffer.
    pub index_buffer: usize,
    /// System-page offset of the geometry record (blob pointer at +0x00,
    /// pointer to the index-buffer holder at +0x38).
    pub record_off: u64,
}

/// Pair mesh blobs with their index buffers via the geometry records.
pub fn find_geometries(r: &Resource, bufs: &[GfxBuffer]) -> Vec<Geometry> {
    use std::collections::HashMap;
    let by_struct: HashMap<u64, usize> = bufs.iter().enumerate().map(|(i, b)| (b.struct_off, i)).collect();
    let target = |ptr: u64| -> Option<usize> { r.sys_off(ptr).and_then(|o| by_struct.get(&(o as u64)).copied()) };
    let mut out = Vec::new();
    let mut o = 0;
    while o + 0x40 <= r.sys.len() {
        if let Some(vb) = target(r.u64(o)).filter(|&i| bufs[i].mesh_blob.is_some()) {
            if let Some(holder) = r.sys_off(r.u64(o + 0x38)) {
                if let Some(ib) = target(r.u64(holder)).filter(|&i| bufs[i].mesh_blob.is_none()) {
                    out.push(Geometry { vertex_buffer: vb, index_buffer: ib, record_off: o as u64 });
                }
            }
        }
        o += 8;
    }
    out.sort_by_key(|g| g.vertex_buffer);
    out.dedup_by_key(|g| g.vertex_buffer);
    out
}

/// A model: a group of geometries drawn together, each with a shader index.
/// Struct: +0x00 ptr to an array of geometry-record ptrs, +0x08 u16 count,
/// +0x0A u16 capacity, +0x0C u32 (bone count?), +0x18 ptr to u16 shader
/// indices (one per geometry).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Model {
    pub struct_off: u64,
    /// Indices into `DrawableInfo::geometries`.
    pub geometries: Vec<usize>,
    /// Shader index (into `DrawableInfo::shaders`) for each geometry.
    pub shader_indices: Vec<u16>,
    /// Level of detail (0 = highest), from the drawable's LOD table:
    /// 16-byte entries {ptr to an array of model ptrs, u16 count, u16 cap}.
    /// Ranked by table position; empty levels are not counted.
    #[serde(default)]
    pub lod: usize,
    /// Not in the main drawable's LOD group: a model of an embedded drawable
    /// (fragment child, nested sub-drawable). `lod` is then 0.
    #[serde(default)]
    pub part: bool,
    /// Skinning palette (peds): the vertex joint indices of this model's
    /// geometries index this list of bone name hashes (joaat, sorted
    /// ascending), not the drawable's own bone order. The bones are the
    /// drawable's own (`GM_U01_*` girth helpers) and bones of the shared ped
    /// skeleton, which is not in the file. Model record: +0x0C u16 count,
    /// +0x20 pointer. Vehicle models use these fields for something else
    /// (the list is not ascending), so only an ascending list is taken.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bone_palette: Option<Vec<u32>>,
}

/// Bone palette of a model record (see `Model::bone_palette`).
fn model_palette(r: &Resource, o: usize) -> Option<Vec<u32>> {
    let n = r.u16(o + 12) as usize;
    if !(2..=1024).contains(&n) {
        return None;
    }
    let p = r.sys_off(r.u64(o + 0x20))?;
    if p + 4 * n > r.sys.len() {
        return None;
    }
    let v: Vec<u32> = (0..n).map(|k| r.u32(p + 4 * k)).collect();
    v.windows(2).all(|w| w[0] < w[1]).then_some(v)
}

/// One shader instance (64-byte record tagged "atem"): +0x00 u32 shader
/// name hash, +0x04 "atem", +0x10 ptr to per-parameter texture slots,
/// +0x20 ptr to the parameter table (u8 ?, u8 count, ..., then count x
/// (u32 parameter name hash, u32 data offset)).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Shader {
    pub index: usize,
    pub record_off: u64,
    /// Shader name hash (unresolved: does not match joaat of known names).
    pub name_hash: String,
    pub param_hashes: Vec<String>,
    pub textures: Vec<TexRef>,
    /// Record bytes +0x38..+0x40 (render bucket flags, see material.rs).
    #[serde(default)]
    pub flags: Vec<u8>,
}

/// A texture slot of a shader: the parameter it binds to and the texture
/// reference hashes (64-bit and 32-bit; unresolved).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TexRef {
    pub param: usize,
    pub param_hash: String,
    pub hash64: String,
    pub hash32: String,
}

/// Find the shader array: the longest run of pointers to "atem" records.
pub fn find_shaders(r: &Resource) -> Vec<Shader> {
    let is_shader = |p: u64| r.sys_off(p).filter(|&o| r.sys.get(o + 4..o + 8) == Some(b"atem")).is_some();
    let mut best = (0usize, 0usize);
    let mut o = 0;
    while o + 8 <= r.sys.len() {
        let mut n = 0;
        while is_shader(r.u64(o + 8 * n)) {
            n += 1;
        }
        if n > best.1 {
            best = (o, n);
        }
        o += 8 * n.max(1);
    }
    let (arr, n) = best;
    (0..n)
        .filter_map(|k| {
            let s = r.sys_off(r.u64(arr + 8 * k))?;
            let mut sh = Shader { index: k, record_off: s as u64, name_hash: format!("{:08x}", r.u32(s)), param_hashes: vec![], textures: vec![], flags: r.sys.get(s + 0x38..s + 0x40).map(|b| b.to_vec()).unwrap_or_default() };
            if let Some(pt) = r.sys_off(r.u64(s + 0x20)) {
                let np = r.sys.get(pt + 1).copied().unwrap_or(0) as usize;
                sh.param_hashes = (0..np).map(|i| format!("{:08x}", r.u32(pt + 8 + 8 * i))).collect();
                if let Some(ta) = r.sys_off(r.u64(s + 0x10)) {
                    for i in 0..np {
                        let Some(t) = r.sys_off(r.u64(ta + 8 * i)) else { continue };
                        let Some(tr) = r.sys_off(r.u64(t + 0x30)) else { continue };
                        if r.u32(tr + 0x10) != 6 {
                            continue;
                        }
                        sh.textures.push(TexRef {
                            param: i,
                            param_hash: sh.param_hashes[i].clone(),
                            hash64: format!("{:016x}", r.u64(tr + 8)),
                            hash32: format!("{:08x}", r.u32(tr + 0x18)),
                        });
                    }
                }
            }
            Some(sh)
        })
        .collect()
}

/// Find the models (geometry groups) and their shader index maps.
pub fn find_models(r: &Resource, geoms: &[Geometry]) -> Vec<Model> {
    use std::collections::HashMap;
    // Geometry records start 8 bytes before the blob pointer.
    let by_rec: HashMap<usize, usize> = geoms.iter().enumerate().map(|(i, g)| (g.record_off.saturating_sub(8) as usize, i)).collect();
    let mut out = Vec::new();
    let mut o = 0;
    while o + 0x30 <= r.sys.len() {
        let n = r.u16(o + 8) as usize;
        if n > 0 && n == r.u16(o + 10) as usize {
            if let (Some(a), Some(m)) = (r.sys_off(r.u64(o)), r.sys_off(r.u64(o + 0x18))) {
                let g: Option<Vec<usize>> = (0..n).map(|k| r.sys_off(r.u64(a + 8 * k)).and_then(|x| by_rec.get(&x).copied())).collect();
                if let Some(g) = g {
                    out.push(Model { struct_off: o as u64, geometries: g, shader_indices: (0..n).map(|k| r.u16(m + 2 * k)).collect(), lod: 0, part: false, bone_palette: model_palette(r, o) });
                }
            }
        }
        o += 8;
    }
    // LOD table: model structs are referenced (at struct - 8) from arrays of
    // pointers; each array is referenced by a LOD entry.
    let mut arrays: Vec<(usize, Vec<usize>)> = Vec::new();
    let model_at: HashMap<u64, usize> = out.iter().enumerate().map(|(i, m)| (0x5000_0000 | (m.struct_off - 8), i)).collect();
    let mut o = 0;
    while o + 0x10 <= r.sys.len() {
        let n = r.u16(o + 8) as usize;
        if n > 0 && n == r.u16(o + 10) as usize {
            if let Some(a) = r.sys_off(r.u64(o)) {
                let ms: Option<Vec<usize>> = (0..n).map(|k| model_at.get(&r.u64(a + 8 * k)).copied()).collect();
                if let Some(ms) = ms {
                    arrays.push((o, ms));
                }
            }
        }
        o += 8;
    }
    // The main drawable's LOD group holds the model lists at +0x50 + 8*k
    // (k = 0..3: high, med, low, vlow). The main drawable is the root for
    // drawables and the struct at root+0x18 for fragments. Other model lists
    // belong to embedded drawables (fragment children at root+0x10, nested
    // sub-drawables): those models are parts, not LODs.
    let base = if find_resource_name(&r.sys).map_or(false, |t| t.1 == "ft") { r.sys_off(r.u64(0x18)) } else { Some(0) };
    let mut main_hit = false;
    if let Some(d) = base {
        let slot_of: HashMap<u64, usize> = (0..4).filter(|k| d + 0x58 + 8 * k <= r.sys.len()).map(|k| (r.u64(d + 0x50 + 8 * k), k)).filter(|x| x.0 != 0).collect();
        for (o, ms) in &arrays {
            let k = slot_of.get(&(0x5000_0000 | *o as u64)).copied();
            main_hit |= k.is_some();
            for &m in ms {
                match k {
                    Some(k) => out[m].lod = k,
                    None => out[m].part = true,
                }
            }
        }
    }
    if !main_hit {
        // layout not recognised (e.g. drawable dictionaries): rank by position
        arrays.sort_by_key(|a| a.0);
        for (lod, (_, ms)) in arrays.iter().enumerate() {
            for &m in ms {
                out[m].lod = lod;
                out[m].part = false;
            }
        }
    }
    out
}
