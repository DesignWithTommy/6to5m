//! Light emitters in the self-describing meta resources (`levels.rpf`).
//!
//! Like GTA V's `CExtensionDefLightEffect` / `CLightAttrDef`, lights hang off
//! archetypes (and some entities) as extensions:
//!
//! ```text
//! archetype 82d6fc83 (184 B): +0x74 u32 name (joaat), +0x90 array of
//!     extension pointers (pool references)
//! entity    ce501483 (144 B): +0x28 array of extension pointers
//! light extension 27922c43 (80 B): +0x08 u32 name, +0x10 vec3 offset,
//!     +0x20 array of lights (f54b227b), +0x30 second light array
//! light f54b227b (672 B): +0x120 vec3 position (local to the archetype),
//!     +0x1A0 / +0x1AC direction / tangent, +0x288 u8[3] RGB colour
//!     (+0x282, +0x285 two more colours), other fields by hash
//! ```
//! Pool reference: u64, 1-based block index in bits 0..12, byte offset in
//! bits 20+ (see `Meta::ptr`).
use crate::meta7a::{Meta, ENTITY};
use crate::resource::Resource;
use serde::Serialize;
use std::collections::BTreeMap;

pub const ARCHETYPE: u32 = 0x82d6_fc83;
pub const LIGHT_EXT: u32 = 0x2792_2c43;
pub const LIGHT: u32 = 0xf54b_227b;

#[derive(Debug, Clone, Serialize)]
pub struct Light {
    /// Local position (archetype / entity space, Z-up, metres).
    pub position: [f32; 3],
    /// u8[3] at +0x288 as RRGGBB: the light colour (warm orange `ff8026`
    /// on street lamps, cool `d8faff`, warm white `fff5c3`, ...).
    pub colour: String,
    /// u8[3] at +0x282 and +0x285: two more colours (mostly `ffffff`;
    /// volume / corona tint?).
    pub colour2: String,
    pub colour3: String,
    /// Unit vectors at +0x1A0 / +0x1AC (direction and tangent, as in GTA V
    /// `CLightAttrDef`).
    pub direction: [f32; 3],
    pub tangent: [f32; 3],
    /// vec4 at +0x00 (unknown).
    pub v0: [f32; 4],
    /// Field names are the case-sensitive joaat of the GTA V `CLightAttr`
    /// names; the hashed originals stay in `fields`.
    /// `lightType` (`1cbbbe30`): 1 point, 2 spot, 4 capsule, 32 (rare, other).
    pub light_type: u32,
    /// `intensity` (`efcd993d`, median 50).
    pub intensity: f32,
    /// `falloff` (`cc6806e8`): the range in metres (spot median 6).
    pub range: f32,
    /// `falloffExponent` (`2bb342ba`; 8 = GTA V default).
    pub falloff_exponent: f32,
    /// `coronaSize`, `coronaIntensity`, `volIntensity`, `flashiness`.
    pub corona_size: f32,
    pub corona_intensity: f32,
    pub vol_intensity: f32,
    pub flashiness: u8,
    /// `lightFadeDistance` / `shadowFadeDistance` (u8 at +0x28D / +0x28E).
    pub light_fade_distance: u8,
    pub shadow_fade_distance: u8,
    /// `455c3938` / `f8d9e344`, degrees (inner <= outer for 98 % of spots).
    pub cone_inner: f32,
    pub cone_outer: f32,
    /// `8609d52c`: 24-bit hour mask, bit h = on at hour h (`ffffff` always).
    pub time_flags: String,
    /// On between 07:00 and 19:59 at no hour (night-only lamp).
    pub night_only: bool,
    /// Every scalar field by hash.
    pub fields: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LightSet {
    /// Owner: "archetype" (name = joaat of the model name) or "entity".
    pub owner: &'static str,
    pub archetype: u32,
    /// For entity-owned sets: index of the entity in its map file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity_index: Option<usize>,
    pub ext_name: u32,
    pub ext_offset: [f32; 3],
    pub lights: Vec<Light>,
}

fn num(f: &BTreeMap<String, serde_json::Value>, k: &str) -> f64 {
    f.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0)
}

fn rgb(r: &Resource, o: usize) -> String {
    r.sys.get(o..o + 3).map(|b| format!("{:02x}{:02x}{:02x}", b[0], b[1], b[2])).unwrap_or_default()
}

fn lights_of_ext(m: &Meta, eo: usize) -> Vec<Light> {
    let r = m.r;
    let mut out = Vec::new();
    for arr in [0x20usize, 0x30] {
        let n = r.u16(eo + arr + 8) as usize;
        let Some((h, base)) = m.ptr(r.u64(eo + arr)) else { continue };
        if h != LIGHT || n == 0 || n > 10_000 {
            continue;
        }
        for k in 0..n {
            let o = base + k * 672;
            if o + 672 > r.sys.len() {
                break;
            }
            let f = m.scalars(LIGHT, o, &[0, 0x120]);
            out.push(Light {
                position: [r.f32(o + 0x120), r.f32(o + 0x124), r.f32(o + 0x128)],
                colour: rgb(r, o + 0x288),
                colour2: rgb(r, o + 0x282),
                colour3: rgb(r, o + 0x285),
                direction: [r.f32(o + 0x1A0), r.f32(o + 0x1A4), r.f32(o + 0x1A8)],
                tangent: [r.f32(o + 0x1AC), r.f32(o + 0x1B0), r.f32(o + 0x1B4)],
                v0: [r.f32(o), r.f32(o + 4), r.f32(o + 8), r.f32(o + 12)],
                light_type: num(&f, "1cbbbe30") as u32,
                intensity: num(&f, "efcd993d") as f32,
                range: num(&f, "cc6806e8") as f32,
                falloff_exponent: num(&f, "2bb342ba") as f32,
                corona_size: r.f32(o + 0x168),
                corona_intensity: r.f32(o + 0x174),
                vol_intensity: r.f32(o + 0x154),
                flashiness: r.sys[o + 0x28B],
                light_fade_distance: r.sys[o + 0x28D],
                shadow_fade_distance: r.sys[o + 0x28E],
                cone_inner: num(&f, "455c3938") as f32,
                cone_outer: num(&f, "f8d9e344") as f32,
                time_flags: format!("{:06x}", r.u32(o + 0x144) & 0xFF_FFFF),
                night_only: { let t = r.u32(o + 0x144) & 0xFF_FFFF; t != 0 && t & 0x0F_FF80 == 0 },
                fields: f,
            });
        }
    }
    out
}

/// Light extensions reachable from an extension-pointer array at `at`.
fn ext_sets(m: &Meta, at: usize) -> Vec<(u32, [f32; 3], Vec<Light>)> {
    let r = m.r;
    let n = r.u16(at + 8) as usize;
    let Some((_, base)) = m.ptr(r.u64(at)) else { return vec![] };
    (0..n.min(256))
        .filter_map(|i| {
            let (h, eo) = m.ptr(r.u64(base + 8 * i))?;
            if h != LIGHT_EXT {
                return None;
            }
            let l = lights_of_ext(m, eo);
            (!l.is_empty()).then(|| (r.u32(eo + 8), [r.f32(eo + 0x10), r.f32(eo + 0x14), r.f32(eo + 0x18)], l))
        })
        .collect()
}

/// All light sets in one meta resource.
pub fn light_sets(r: &Resource) -> Vec<LightSet> {
    let Some(m) = Meta::parse(r) else { return vec![] };
    if !m.structs.contains_key(&LIGHT_EXT) {
        return vec![];
    }
    let mut v = Vec::new();
    for o in m.objects(ARCHETYPE) {
        for (en, eo, lights) in ext_sets(&m, o + 0x90) {
            v.push(LightSet { owner: "archetype", archetype: r.u32(o + 0x74), entity_index: None, ext_name: en, ext_offset: eo, lights });
        }
    }
    for (i, o) in m.objects(ENTITY).into_iter().enumerate() {
        for (en, eo, lights) in ext_sets(&m, o + 0x28) {
            v.push(LightSet { owner: "entity", archetype: r.u32(o + 0x44), entity_index: Some(i), ext_name: en, ext_offset: eo, lights });
        }
    }
    v
}

/// Rotate `v` by quaternion `q` (x, y, z, w).
pub fn rotate(q: [f32; 4], v: [f32; 3]) -> [f32; 3] {
    let [x, y, z, w] = q;
    let t = [2.0 * (y * v[2] - z * v[1]), 2.0 * (z * v[0] - x * v[2]), 2.0 * (x * v[1] - y * v[0])];
    [
        v[0] + w * t[0] + (y * t[2] - z * t[1]),
        v[1] + w * t[1] + (z * t[0] - x * t[2]),
        v[2] + w * t[2] + (x * t[1] - y * t[0]),
    ]
}
