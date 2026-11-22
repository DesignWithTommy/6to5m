//! GTA V Meta (RSC7 v2) writers for .ymap (CMapData) and .ytyp (CMapTypes),
//! legacy PC layout. Port of CodeWalker's MetaBuilder / XmlMeta path: data
//! blocks, MetaPOINTER/Array_* encodings, structure/enum infos and block
//! order follow CodeWalker so output matches its xml2meta byte-for-byte.

use super::rsc::{self, BlockId, Builder, Seg, W};
use serde::Deserialize;

// ---------------------------------------------------------------- JSON model

#[derive(Deserialize, Clone, Debug)]
#[serde(untagged)]
pub enum EnumVal {
    N(i32),
    S(String),
}

fn d_one() -> f32 {
    1.0
}
fn d_one_u() -> u32 {
    1
}
fn d_m1() -> i32 {
    -1
}
fn d_255() -> i32 {
    255
}
fn d_32() -> u32 {
    32
}
fn d_rot() -> [f32; 4] {
    [0.0, 0.0, 0.0, 1.0]
}
fn d_orphan() -> EnumVal {
    EnumVal::S("LODTYPES_DEPTH_ORPHANHD".into())
}
fn d_prireq() -> EnumVal {
    EnumVal::S("PRI_REQUIRED".into())
}
fn d_drawable() -> EnumVal {
    EnumVal::S("ASSET_TYPE_DRAWABLE".into())
}
fn d_500() -> f32 {
    500.0
}
fn d_exp() -> String {
    "6ixpack".into()
}

#[derive(Deserialize, Clone, Debug)]
pub struct MapData {
    pub name: String,
    #[serde(default)]
    pub parent: String,
    #[serde(default)]
    pub flags: u32,
    #[serde(default = "d_one_u")]
    pub content_flags: u32,
    /// [min, max]; computed from entities + archetype bounds when absent.
    #[serde(default)]
    pub streaming_extents: Option<[[f32; 3]; 2]>,
    #[serde(default)]
    pub entities_extents: Option<[[f32; 3]; 2]>,
    #[serde(default)]
    pub entities: Vec<Entity>,
    /// CBlockDesc strings (block.name = map name).
    #[serde(default = "d_exp")]
    pub exported_by: String,
    #[serde(default)]
    pub owner: String,
    #[serde(default)]
    pub time: String,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Entity {
    pub archetype_name: String,
    #[serde(default = "d_32")]
    pub flags: u32,
    #[serde(default)]
    pub guid: u32,
    #[serde(default)]
    pub position: [f32; 3],
    /// Quaternion x,y,z,w as stored in the ymap.
    #[serde(default = "d_rot")]
    pub rotation: [f32; 4],
    #[serde(default = "d_one")]
    pub scale_xy: f32,
    #[serde(default = "d_one")]
    pub scale_z: f32,
    #[serde(default = "d_m1")]
    pub parent_index: i32,
    pub lod_dist: f32,
    #[serde(default)]
    pub child_lod_dist: f32,
    #[serde(default = "d_orphan")]
    pub lod_level: EnumVal,
    #[serde(default)]
    pub num_children: u32,
    #[serde(default = "d_prireq")]
    pub priority_level: EnumVal,
    #[serde(default = "d_255")]
    pub ambient_occlusion_multiplier: i32,
    #[serde(default = "d_255")]
    pub artificial_ambient_occlusion: i32,
    #[serde(default)]
    pub tint_value: u32,
}

#[derive(Deserialize, Clone, Debug)]
pub struct MapTypes {
    pub name: String,
    #[serde(default)]
    pub archetypes: Vec<Archetype>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Archetype {
    pub name: String,
    pub lod_dist: f32,
    #[serde(default)]
    pub flags: u32,
    #[serde(default)]
    pub special_attribute: u32,
    pub bb_min: [f32; 3],
    pub bb_max: [f32; 3],
    /// Default: box centre / half diagonal.
    #[serde(default)]
    pub bs_centre: Option<[f32; 3]>,
    #[serde(default)]
    pub bs_radius: Option<f32>,
    #[serde(default = "d_500")]
    pub hd_texture_dist: f32,
    #[serde(default)]
    pub texture_dictionary: String,
    #[serde(default)]
    pub clip_dictionary: String,
    #[serde(default)]
    pub drawable_dictionary: String,
    #[serde(default)]
    pub physics_dictionary: String,
    #[serde(default = "d_drawable")]
    pub asset_type: EnumVal,
    /// Default: same as `name`.
    #[serde(default)]
    pub asset_name: Option<String>,
}

/// Name -> hash. "" = 0; "hash:0x1234ABCD", "hash_1234ABCD" or "0x1234ABCD"
/// are raw hashes; anything else is joaat'd.
pub fn hash(s: &str) -> Result<u32, String> {
    let t = s.trim();
    if t.is_empty() {
        return Ok(0);
    }
    let hex = t
        .strip_prefix("hash:")
        .map(|r| r.trim_start_matches("0x").trim_start_matches("0X"))
        .or_else(|| t.strip_prefix("hash_"))
        .or_else(|| t.strip_prefix("0x"));
    if let Some(h) = hex {
        return u32::from_str_radix(h, 16).map_err(|_| format!("bad hash '{s}'"));
    }
    Ok(rsc::joaat(t))
}

fn enum_val(v: &EnumVal, names: &[&str]) -> Result<i32, String> {
    match v {
        EnumVal::N(n) => Ok(*n),
        EnumVal::S(s) => names
            .iter()
            .position(|n| n.eq_ignore_ascii_case(s))
            .map(|i| i as i32)
            .ok_or_else(|| format!("unknown enum value '{s}' (expected one of {names:?})")),
    }
}
const LOD_TYPES: &[&str] = &[
    "LODTYPES_DEPTH_HD",
    "LODTYPES_DEPTH_LOD",
    "LODTYPES_DEPTH_SLOD1",
    "LODTYPES_DEPTH_SLOD2",
    "LODTYPES_DEPTH_SLOD3",
    "LODTYPES_DEPTH_ORPHANHD",
    "LODTYPES_DEPTH_SLOD4",
];
const PRI_LEVELS: &[&str] = &["PRI_REQUIRED", "PRI_OPTIONAL_HIGH", "PRI_OPTIONAL_MEDIUM", "PRI_OPTIONAL_LOW"];
const ASSET_TYPES: &[&str] = &[
    "ASSET_TYPE_UNINITIALIZED",
    "ASSET_TYPE_FRAGMENT",
    "ASSET_TYPE_DRAWABLE",
    "ASSET_TYPE_DRAWABLEDICTIONARY",
    "ASSET_TYPE_ASSETLESS",
];

// ---------------------------------------------------------------- extents

/// SharpDX Quaternion.Multiply(Vector3): rotate v by q (x,y,z,w).
fn qmul(q: [f32; 4], v: [f32; 3]) -> [f32; 3] {
    let [x, y, z, w] = q;
    let (x2, y2, z2) = (x + x, y + y, z + z);
    let (wx, wy, wz) = (w * x2, w * y2, w * z2);
    let (xx, xy, xz) = (x * x2, x * y2, x * z2);
    let (yy, yz, zz) = (y * y2, y * z2, z * z2);
    [
        v[0] * (1.0 - yy - zz) + v[1] * (xy - wz) + v[2] * (xz + wy),
        v[0] * (xy + wz) + v[1] * (1.0 - xx - zz) + v[2] * (yz - wx),
        v[0] * (xz - wy) + v[1] * (yz + wx) + v[2] * (1.0 - xx - yy),
    ]
}

/// CodeWalker YmapFile.CalcExtents for entities. Returns
/// (entities_extents, streaming_extents), each [min, max]. Orientation is the
/// inverse of the stored quaternion (non-MLO entities); the box is the scaled
/// archetype box, the streaming box that box grown by lodDist (the
/// archetype's when the entity's is <= 0). Entities whose archetype is not in
/// `archetypes` count as a point (grown by lodDist for streaming).
pub fn compute_extents(entities: &[Entity], archetypes: &[Archetype]) -> Result<([[f32; 3]; 2], [[f32; 3]; 2]), String> {
    if entities.is_empty() {
        return Ok(([[0.0; 3]; 2], [[0.0; 3]; 2]));
    }
    let mut ah = Vec::with_capacity(archetypes.len());
    for a in archetypes {
        ah.push(hash(&a.name)?);
    }
    let mut e = [[f32::MAX; 3], [f32::MIN; 3]];
    let mut s = e;
    for ent in entities {
        let h = hash(&ent.archetype_name)?;
        let arch = ah.iter().position(|&x| x == h).map(|i| &archetypes[i]);
        let p = ent.position;
        let mut lod = ent.lod_dist;
        let r = ent.rotation;
        let ori = if r == [0.0, 0.0, 0.0, 1.0] {
            r
        } else {
            let n: f32 = r.iter().map(|v| v * v).sum();
            [-r[0] / n, -r[1] / n, -r[2] / n, r[3] / n]
        };
        let (bmin, bmax, smin, smax);
        if let Some(a) = arch {
            if lod <= 0.0 {
                lod = a.lod_dist;
            }
            let sc = [ent.scale_xy, ent.scale_xy, ent.scale_z];
            let amin: [f32; 3] = std::array::from_fn(|k| a.bb_min[k] * sc[k]);
            let amax: [f32; 3] = std::array::from_fn(|k| a.bb_max[k] * sc[k]);
            let corners = |lo: [f32; 3], hi: [f32; 3]| {
                let mut mn = [f32::MAX; 3];
                let mut mx = [f32::MIN; 3];
                for j in 0..8 {
                    let c = [
                        if j & 4 != 0 { hi[0] } else { lo[0] },
                        if j & 2 != 0 { hi[1] } else { lo[1] },
                        if j & 1 != 0 { hi[2] } else { lo[2] },
                    ];
                    let w = qmul(ori, c);
                    for k in 0..3 {
                        mn[k] = mn[k].min(w[k] + p[k]);
                        mx[k] = mx[k].max(w[k] + p[k]);
                    }
                }
                (mn, mx)
            };
            (bmin, bmax) = corners(amin, amax);
            (smin, smax) = corners(amin.map(|v| v - lod), amax.map(|v| v + lod));
        } else {
            bmin = p;
            bmax = p;
            smin = p.map(|v| v - lod);
            smax = p.map(|v| v + lod);
        }
        for k in 0..3 {
            e[0][k] = e[0][k].min(bmin[k]);
            e[1][k] = e[1][k].max(bmax[k]);
            s[0][k] = s[0][k].min(smin[k]);
            s[1][k] = s[1][k].max(smax[k]);
        }
    }
    Ok((e, s))
}

// ---------------------------------------------------------------- infos

struct SInfo {
    name: u32,
    key: u32,
    unk8: u32,
    size: i32,
    /// (entry name hash, data offset, data type, unk9, ref type index, ref key)
    entries: &'static [(u32, i32, u8, u8, i16, u32)],
}
struct EInfo {
    name: u32,
    key: u32,
    entries: &'static [(u32, i32)],
}

// Verbatim from CodeWalker MetaTypes.GetStructureInfo / GetEnumInfo (as
// written into CodeWalker-built ymap/ytyp files).
const CENTITYDEF: SInfo = SInfo { name: 0xce501483, key: 0x6cd37d5a, unk8: 0x400, size: 128, entries: CENTITYDEF_E };
const CENTITYDEF_E: &[(u32, i32, u8, u8, i16, u32)] = &[
    (0xa023a02c, 8, 0x4a, 0, 0, 0x00000000),
    (0x67d26872, 12, 0x15, 0, 0, 0x00000000),
    (0x9a7b6e6d, 16, 0x15, 0, 0, 0x00000000),
    (0x01166164, 32, 0x33, 0, 0, 0x00000000),
    (0x77dd4010, 48, 0x34, 0, 0, 0x00000000),
    (0x9ca32637, 64, 0x21, 0, 0, 0x00000000),
    (0x10fb7c42, 68, 0x21, 0, 0, 0x00000000),
    (0xd89231bd, 72, 0x14, 0, 0, 0x00000000),
    (0xbf74dfa7, 76, 0x21, 0, 0, 0x00000000),
    (0xca974bcd, 80, 0x21, 0, 0, 0x00000000),
    (0x6c8f1715, 84, 0x62, 0, 0, 0x4b5acc2f),
    (0xa687ac89, 88, 0x15, 0, 0, 0x00000000),
    (0x2691f019, 92, 0x62, 0, 0, 0x26a60207),
    (0x00000100, 0, 0x07, 0, 0, 0x00000000),
    (0xf17e7f28, 96, 0x52, 0, 13, 0x00000000),
    (0x18c1d587, 112, 0x14, 0, 0, 0x00000000),
    (0x23c0e543, 116, 0x14, 0, 0, 0x00000000),
    (0x3c852527, 120, 0x15, 0, 0, 0x00000000),
];
const INSTANCED: SInfo = SInfo { name: 0xf14a15be, key: 0x6d7b0a56, unk8: 0x300, size: 48, entries: INSTANCED_E };
const INSTANCED_E: &[(u32, i32, u8, u8, i16, u32)] = &[
    (0x7fae45e2, 8, 0x4a, 0, 0, 0x00000000),
    (0x00000100, 0, 0x05, 0, 0, 0xba049770),
    (0xd3af3360, 16, 0x52, 0, 1, 0x00000000),
    (0x00000100, 0, 0x05, 0, 0, 0x7c475b5d),
    (0x0f3773dd, 32, 0x52, 0, 3, 0x00000000),
];
const LODLIGHT: SInfo = SInfo { name: 0x87ac4538, key: 0x8a97926c, unk8: 0x300, size: 136, entries: LODLIGHT_E };
const LODLIGHT_E: &[(u32, i32, u8, u8, i16, u32)] = &[
    (0x00000100, 0, 0x05, 0, 0, 0xe2cbcfd4),
    (0x34df9828, 8, 0x52, 0, 0, 0x00000000),
    (0x00000100, 0, 0x21, 0, 0, 0x00000000),
    (0xcc6806e8, 24, 0x52, 0, 2, 0x00000000),
    (0x00000100, 0, 0x21, 0, 0, 0x00000000),
    (0x2bb342ba, 40, 0x52, 0, 4, 0x00000000),
    (0x00000100, 0, 0x15, 0, 0, 0x00000000),
    (0xb983bbe6, 56, 0x52, 0, 6, 0x00000000),
    (0x00000100, 0, 0x15, 0, 0, 0x00000000),
    (0x3e818018, 72, 0x52, 0, 8, 0x00000000),
    (0x00000100, 0, 0x11, 0, 0, 0x00000000),
    (0x455c3938, 88, 0x52, 0, 10, 0x00000000),
    (0x00000100, 0, 0x11, 0, 0, 0x00000000),
    (0xbc76acc0, 104, 0x52, 0, 12, 0x00000000),
    (0x00000100, 0, 0x11, 0, 0, 0x00000000),
    (0x88a2b1fb, 120, 0x52, 0, 14, 0x00000000),
];
const DISTLODLIGHT: SInfo = SInfo { name: 0x793441f4, key: 0xa823a583, unk8: 0x300, size: 48, entries: DISTLODLIGHT_E };
const DISTLODLIGHT_E: &[(u32, i32, u8, u8, i16, u32)] = &[
    (0x00000100, 0, 0x05, 0, 0, 0xe2cbcfd4),
    (0x01166164, 8, 0x52, 0, 0, 0x00000000),
    (0x00000100, 0, 0x15, 0, 0, 0x00000000),
    (0x0b987d32, 24, 0x52, 0, 2, 0x00000000),
    (0xdd11304b, 40, 0x13, 0, 0, 0x00000000),
    (0x7a5c560d, 42, 0x13, 0, 0, 0x00000000),
];
const BLOCKDESC: SInfo = SInfo { name: 0xb7206e4a, key: 0x782698f9, unk8: 0x300, size: 72, entries: BLOCKDESC_E };
const BLOCKDESC_E: &[(u32, i32, u8, u8, i16, u32)] = &[
    (0x68c27e33, 0, 0x15, 0, 0, 0x00000000),
    (0x67d26872, 4, 0x15, 0, 0, 0x00000000),
    (0xe6e17f14, 8, 0x44, 0, 0, 0x00000000),
    (0x76350055, 24, 0x44, 0, 0, 0x00000000),
    (0x8c060170, 40, 0x44, 0, 0, 0x00000000),
    (0x0f678e23, 56, 0x44, 0, 0, 0x00000000),
];
const CMAPDATA: SInfo = SInfo { name: 0xd3593fa6, key: 0xcd85db27, unk8: 0x400, size: 512, entries: CMAPDATA_E };
const CMAPDATA_E: &[(u32, i32, u8, u8, i16, u32)] = &[
    (0xe6e17f14, 8, 0x4a, 0, 0, 0x00000000),
    (0xa99c4651, 12, 0x4a, 0, 0, 0x00000000),
    (0x67d26872, 16, 0x15, 0, 0, 0x00000000),
    (0x6a675035, 20, 0x15, 0, 0, 0x00000000),
    (0xdd22821f, 32, 0x33, 0, 0, 0x00000000),
    (0xa22ea335, 48, 0x33, 0, 0, 0x00000000),
    (0x1c75bcf1, 64, 0x33, 0, 0, 0x00000000),
    (0x6d074437, 80, 0x33, 0, 0, 0x00000000),
    (0x00000100, 0, 0x07, 0, 0, 0x00000000),
    (0xccab9307, 96, 0x52, 0, 8, 0x00000000),
    (0x00000100, 0, 0x05, 0, 0, 0x163022a5),
    (0xaeff8d15, 112, 0x52, 0, 10, 0x00000000),
    (0x00000100, 0, 0x05, 0, 0, 0x3a282e1d),
    (0xed70c614, 128, 0x52, 4, 12, 0x00000000),
    (0x00000100, 0, 0x05, 0, 0, 0xa36c4ead),
    (0x7f1998dd, 144, 0x52, 4, 14, 0x00000000),
    (0x00000100, 0, 0x4a, 0, 0, 0x00000000),
    (0x38999564, 160, 0x52, 0, 16, 0x00000000),
    (0x9920dc29, 176, 0x05, 0, 0, 0xf14a15be),
    (0x00000100, 0, 0x05, 0, 0, 0x674f9350),
    (0xaf9c3bd9, 224, 0x52, 0, 19, 0x00000000),
    (0x00000100, 0, 0x05, 0, 0, 0x6ee83bdf),
    (0xc200ab4c, 240, 0x52, 0, 21, 0x00000000),
    (0x69c2c0fa, 256, 0x05, 0, 0, 0x87ac4538),
    (0xb0199551, 392, 0x05, 0, 0, 0x793441f4),
    (0xe433d77d, 440, 0x05, 0, 0, 0xb7206e4a),
];
const E_LODTYPE: EInfo = EInfo { name: 0x4b5acc2f, key: 0x6ea51086, entries: &[(0xc2485bbc, 0), (0x8e22c856, 1), (0x983c176c, 2), (0x12cd0c94, 3), (0x81046901, 4), (0x7d934f41, 5), (0x6f5d45b3, 6)] };
const E_PRIORITY: EInfo = EInfo { name: 0x26a60207, key: 0x8326cb4f, entries: &[(0x73d556cb, 0), (0xee09c197, 1), (0x1ebb6975, 2), (0x13a5b7d4, 3)] };
const CBASEARCH: SInfo = SInfo { name: 0x82d6fc83, key: 0x8fbadaa4, unk8: 0x400, size: 144, entries: CBASEARCH_E };
const CBASEARCH_E: &[(u32, i32, u8, u8, i16, u32)] = &[
    (0xbf74dfa7, 8, 0x21, 0, 0, 0x00000000),
    (0x67d26872, 12, 0x15, 0, 0, 0x00000000),
    (0x6c1523e4, 16, 0x15, 0, 0, 0x00000000),
    (0xd9ef8236, 32, 0x33, 0, 0, 0x00000000),
    (0xe78aa618, 48, 0x33, 0, 0, 0x00000000),
    (0xd61872ff, 64, 0x33, 0, 0, 0x00000000),
    (0x82e8c5de, 80, 0x21, 0, 0, 0x00000000),
    (0xad5d5b4c, 84, 0x21, 0, 0, 0x00000000),
    (0xe6e17f14, 88, 0x4a, 0, 0, 0x00000000),
    (0x75d215a1, 92, 0x4a, 0, 0, 0x00000000),
    (0x19471791, 96, 0x4a, 0, 0, 0x00000000),
    (0xfc1fb6fb, 100, 0x4a, 0, 0, 0x00000000),
    (0xd3c717fc, 104, 0x4a, 0, 0, 0x00000000),
    (0x069004b2, 108, 0x62, 0, 0, 0x76baf7c7),
    (0x9a77c7c5, 112, 0x4a, 0, 0, 0x00000000),
    (0x00000100, 0, 0x07, 0, 0, 0x00000000),
    (0xf17e7f28, 120, 0x52, 0, 15, 0x00000000),
];
const CMAPTYPES: SInfo = SInfo { name: 0xd98bb561, key: 0x9b8046d4, unk8: 0x300, size: 80, entries: CMAPTYPES_E };
const CMAPTYPES_E: &[(u32, i32, u8, u8, i16, u32)] = &[
    (0x00000100, 0, 0x07, 0, 0, 0x00000000),
    (0xf17e7f28, 8, 0x52, 0, 0, 0x00000000),
    (0x00000100, 0, 0x07, 0, 0, 0x00000000),
    (0x018a3b1b, 24, 0x52, 0, 2, 0x00000000),
    (0xe6e17f14, 40, 0x4a, 0, 0, 0x00000000),
    (0x00000100, 0, 0x4a, 0, 0, 0x00000000),
    (0x3c6f8844, 48, 0x52, 0, 5, 0x00000000),
    (0x00000100, 0, 0x05, 0, 0, 0x46ad6dff),
    (0x65e9d4eb, 64, 0x52, 0, 7, 0x00000000),
];
const E_ASSETTYPE: EInfo = EInfo { name: 0x76baf7c7, key: 0x6f39632c, entries: &[(0x0b4f1fed, 0), (0x22097fe7, 1), (0x07c0cb71, 2), (0x5e2f6a14, 3), (0xf8051e71, 4)] };

// ---------------------------------------------------------------- meta builder

const T_CMAPDATA: u32 = 0xd3593fa6;
const T_CMAPTYPES: u32 = 0xd98bb561;
const T_POINTER: u32 = 7;
const T_STRING: u32 = 0x10;
/// MetaBuilder.MaxBlockLength: a data block takes new items while smaller.
const MAX_BLOCK: usize = 0x4000;

#[derive(Default)]
struct MB {
    blocks: Vec<(u32, Vec<u8>)>,
    sinfos: Vec<&'static SInfo>,
    einfos: Vec<&'static EInfo>,
}

/// MetaPOINTER / Array_* pointer value: 1-based block id + offset << 12.
fn mptr(id: usize, off: usize) -> u64 {
    ((id as u64) & 0xFFF) | (((off as u64) & 0xFFFFF) << 12)
}

impl MB {
    fn ensure(&mut self, t: u32) -> usize {
        if let Some(i) = self.blocks.iter().position(|b| b.0 == t && b.1.len() < MAX_BLOCK) {
            return i;
        }
        self.blocks.push((t, Vec::new()));
        self.blocks.len() - 1
    }
    /// AddItem / AddItemArray (data padded to 16).
    fn add_item(&mut self, t: u32, mut d: Vec<u8>) -> u64 {
        let i = self.ensure(t);
        d.resize(d.len().div_ceil(16) * 16, 0);
        let off = self.blocks[i].1.len();
        self.blocks[i].1.extend_from_slice(&d);
        mptr(i + 1, off)
    }
    fn add_string(&mut self, s: &str) -> (u64, u16) {
        let i = self.ensure(T_STRING);
        let off = self.blocks[i].1.len();
        self.blocks[i].1.extend_from_slice(s.as_bytes());
        self.blocks[i].1.push(0);
        (mptr(i + 1, off), s.len() as u16)
    }
    fn sinfo(&mut self, s: &'static SInfo) {
        if !self.sinfos.iter().any(|x| x.name == s.name) {
            self.sinfos.push(s);
        }
    }
    fn einfo(&mut self, e: &'static EInfo) {
        if !self.einfos.iter().any(|x| x.name == e.name) {
            self.einfos.push(e);
        }
    }
    /// Array_StructurePointer: each item its own struct item, then a POINTER array.
    fn ptr_array(&mut self, t: u32, items: Vec<Vec<u8>>) -> (u64, u16) {
        if items.is_empty() {
            return (0, 0);
        }
        let n = items.len() as u16;
        let mut ptrs = Vec::with_capacity(items.len() * 8);
        for it in items {
            ptrs.extend_from_slice(&self.add_item(t, it).to_le_bytes());
        }
        (self.add_item(T_POINTER, ptrs), n)
    }

    /// Emit the RSC7 resource (block order = CodeWalker GetBlocks order).
    fn build(self) -> Result<Vec<u8>, String> {
        let make = |pages: u8| -> Builder {
            let mut b = Builder::new();
            b.meta_packing = true;
            let root = b.reserve(Seg::Sys);
            let mut pi = W::new();
            pi.u32(0).u32(0).u8(pages).u8(0).u16(0).u32(0).zeros(8 * 128);
            let pinfo = b.add(Seg::Sys, pi);
            let si = (!self.sinfos.is_empty()).then(|| b.reserve(Seg::Sys));
            let mut siw = W::new();
            for s in &self.sinfos {
                let mut ew = W::new();
                for &(h, off, t, u9, ri, rk) in s.entries {
                    ew.u32(h).i32(off).u8(t).u8(u9).u16(ri as u16).u32(rk);
                }
                let eb = b.add(Seg::Sys, ew);
                siw.u32(s.name).u32(s.key).u32(s.unk8).u32(0).ptr(Some(eb)).i32(s.size).u16(0).u16(s.entries.len() as u16);
            }
            if let Some(si) = si {
                b.set(si, siw);
            }
            let ei = (!self.einfos.is_empty()).then(|| b.reserve(Seg::Sys));
            let mut eiw = W::new();
            for e in &self.einfos {
                let mut ew = W::new();
                for &(h, v) in e.entries {
                    ew.u32(h).i32(v);
                }
                let eb = b.add(Seg::Sys, ew);
                eiw.u32(e.name).u32(e.key).ptr(Some(eb)).i32(e.entries.len() as i32).i32(0);
            }
            if let Some(ei) = ei {
                b.set(ei, eiw);
            }
            let db = b.reserve(Seg::Sys);
            let mut dbw = W::new();
            for (t, d) in &self.blocks {
                let id: BlockId = b.sys_bytes(d.clone());
                dbw.u32(*t).i32(d.len() as i32).ptr(Some(id));
            }
            b.set(db, dbw);
            let mut m = W::new();
            m.u32(0x405b_c808).u32(1).ptr(Some(pinfo));
            m.u32(0x5052_4430).u16(0x79).u8(0).u8(0).u32(0).i32(1);
            m.ptr(si).ptr(ei).ptr(Some(db)).ptr(None).ptr(None);
            m.u16(self.sinfos.len() as u16).u16(self.einfos.len() as u16).u16(self.blocks.len() as u16).u16(0);
            m.zeros(32);
            b.set(root, m);
            b
        };
        // ResourcePagesInfo.SystemPagesCount = page-flags count, known only
        // after packing (positions don't depend on it): build twice.
        let first = make(0).build(2)?;
        let f = u32::from_le_bytes(first[8..12].try_into().unwrap()) & 0x0FFF_FFFF;
        let count = ((f >> 4) & 1) + ((f >> 5) & 3) + ((f >> 7) & 0xF) + ((f >> 11) & 0x3F) + ((f >> 17) & 0x7F) + ((f >> 24) & 1) + ((f >> 25) & 1) + ((f >> 26) & 1) + ((f >> 27) & 1);
        make(count as u8).build(2)
    }
}

/// Array_Structure / Array_StructurePointer / CharPointer (16 bytes).
fn arr(w: &mut W, p: u64, n: u16) {
    w.u64(p).u16(n).u16(n).u32(0);
}

// ---------------------------------------------------------------- ymap

pub fn build_ymap(m: &MapData) -> Result<Vec<u8>, String> {
    build_ymap_with(m, &[])
}

/// Like `build_ymap`; `archetypes` supply bounds for extents left out of `m`.
pub fn build_ymap_with(m: &MapData, archetypes: &[Archetype]) -> Result<Vec<u8>, String> {
    if m.entities.len() > 0xFFFF {
        return Err("too many entities".into());
    }
    let (ee, se) = match (m.entities_extents, m.streaming_extents) {
        (Some(e), Some(s)) => (e, s),
        (e, s) => {
            let (ce, cs) = compute_extents(&m.entities, archetypes)?;
            (e.unwrap_or(ce), s.unwrap_or(cs))
        }
    };
    let mut mb = MB::default();
    mb.ensure(T_CMAPDATA); // root = block 1
    let mut items = Vec::with_capacity(m.entities.len());
    for e in &m.entities {
        let mut w = W::new();
        w.u32(0).u32(0).u32(hash(&e.archetype_name)?).u32(e.flags).u32(e.guid).zeros(12);
        w.v3(e.position).f32(0.0).v4(e.rotation);
        w.f32(e.scale_xy).f32(e.scale_z).i32(e.parent_index).f32(e.lod_dist).f32(e.child_lod_dist);
        w.i32(enum_val(&e.lod_level, LOD_TYPES)?).u32(e.num_children).i32(enum_val(&e.priority_level, PRI_LEVELS)?);
        arr(&mut w, 0, 0); // extensions
        w.i32(e.ambient_occlusion_multiplier).i32(e.artificial_ambient_occlusion).u32(e.tint_value).u32(0);
        items.push(w.b);
    }
    if !items.is_empty() {
        mb.einfo(&E_LODTYPE);
        mb.einfo(&E_PRIORITY);
        mb.sinfo(&CENTITYDEF);
    }
    let (ep, en) = mb.ptr_array(CENTITYDEF.name, items);
    mb.sinfo(&INSTANCED);
    mb.sinfo(&LODLIGHT);
    mb.sinfo(&DISTLODLIGHT);
    let mut sp = [(0u64, 0u16); 4];
    for (i, s) in [m.name.as_str(), &m.exported_by, &m.owner, &m.time].into_iter().enumerate() {
        if !s.is_empty() {
            sp[i] = mb.add_string(s);
        }
    }
    mb.sinfo(&BLOCKDESC);
    mb.sinfo(&CMAPDATA);

    let mut w = W::new();
    w.u32(0).u32(0).u32(hash(&m.name)?).u32(hash(&m.parent)?).u32(m.flags).u32(m.content_flags).u32(0).u32(0);
    w.v3(se[0]).f32(0.0).v3(se[1]).f32(0.0).v3(ee[0]).f32(0.0).v3(ee[1]).f32(0.0);
    arr(&mut w, ep, en); // entities
    for _ in 0..4 {
        arr(&mut w, 0, 0); // containerLods, boxOccluders, occludeModels, physicsDictionaries
    }
    w.zeros(48); // instancedData
    arr(&mut w, 0, 0); // timeCycleModifiers
    arr(&mut w, 0, 0); // carGenerators
    w.zeros(136); // LODLightsSOA
    w.zeros(48); // DistantLODLightsSOA
    w.u32(0).u32(0); // block.version, block.flags
    for (p, n) in sp {
        arr(&mut w, p, n); // block.name, exportedBy, owner, time
    }
    debug_assert_eq!(w.len(), 512);
    mb.blocks[0].1.extend_from_slice(&w.b);
    mb.build()
}

// ---------------------------------------------------------------- ytyp

pub fn build_ytyp(t: &MapTypes) -> Result<Vec<u8>, String> {
    if t.archetypes.len() > 0xFFFF {
        return Err("too many archetypes".into());
    }
    let mut mb = MB::default();
    mb.ensure(T_CMAPTYPES);
    let mut items = Vec::with_capacity(t.archetypes.len());
    for a in &t.archetypes {
        let c = a.bs_centre.unwrap_or(std::array::from_fn(|k| (a.bb_min[k] + a.bb_max[k]) * 0.5));
        let r = a.bs_radius.unwrap_or_else(|| (0..3).map(|k| (a.bb_max[k] - a.bb_min[k]).powi(2)).sum::<f32>().sqrt() * 0.5);
        let mut w = W::new();
        w.u32(0).u32(0).f32(a.lod_dist).u32(a.flags).u32(a.special_attribute).zeros(12);
        w.v3(a.bb_min).f32(0.0).v3(a.bb_max).f32(0.0).v3(c).f32(0.0);
        w.f32(r).f32(a.hd_texture_dist).u32(hash(&a.name)?).u32(hash(&a.texture_dictionary)?);
        w.u32(hash(&a.clip_dictionary)?).u32(hash(&a.drawable_dictionary)?).u32(hash(&a.physics_dictionary)?);
        w.i32(enum_val(&a.asset_type, ASSET_TYPES)?).u32(hash(a.asset_name.as_deref().unwrap_or(&a.name))?).u32(0);
        arr(&mut w, 0, 0); // extensions
        w.u32(0).u32(0);
        items.push(w.b);
    }
    if !items.is_empty() {
        mb.einfo(&E_ASSETTYPE);
        mb.sinfo(&CBASEARCH);
    }
    let (ap, an) = mb.ptr_array(CBASEARCH.name, items);
    mb.sinfo(&CMAPTYPES);
    let mut w = W::new();
    w.u32(0).u32(0);
    arr(&mut w, 0, 0); // extensions
    arr(&mut w, ap, an); // archetypes
    w.u32(hash(&t.name)?).u32(0);
    arr(&mut w, 0, 0); // dependencies
    arr(&mut w, 0, 0); // compositeEntityTypes
    mb.blocks[0].1.extend_from_slice(&w.b);
    mb.build()
}

// ---------------------------------------------------------------- cli

/// `ymap <in.json> <out.ymap> [types.json]` | `ytyp <in.json> <out.ytyp>`.
/// types.json (a MapTypes) supplies archetype bounds when the ymap JSON
/// omits entities_extents / streaming_extents.
pub fn cli(args: &[String]) -> Result<(), String> {
    let usage = "usage: 6ixpack ymap <in.json> <out.ymap> [types.json] | ytyp <in.json> <out.ytyp>";
    if args.len() < 3 {
        return Err(usage.into());
    }
    let read = |p: &str| std::fs::read_to_string(p).map_err(|e| format!("{p}: {e}"));
    let data = match args[0].as_str() {
        "ymap" => {
            let m: MapData = serde_json::from_str(&read(&args[1])?).map_err(|e| format!("{}: {e}", args[1]))?;
            let types: Vec<Archetype> = match args.get(3) {
                Some(p) => serde_json::from_str::<MapTypes>(&read(p)?).map_err(|e| format!("{p}: {e}"))?.archetypes,
                None => Vec::new(),
            };
            build_ymap_with(&m, &types)?
        }
        "ytyp" => {
            let t: MapTypes = serde_json::from_str(&read(&args[1])?).map_err(|e| format!("{}: {e}", args[1]))?;
            build_ytyp(&t)?
        }
        _ => return Err(usage.into()),
    };
    std::fs::write(&args[2], &data).map_err(|e| format!("{}: {e}", args[2]))?;
    println!("meta ok: {} ({} bytes)", args[2], data.len());
    Ok(())
}
