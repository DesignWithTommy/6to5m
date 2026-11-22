//! Material classification without shader names.
//!
//! Shader and parameter names are hashed with an unknown function, so
//! classes come from two structural sources measured over
//! 254 K drawables (`sixng shaders` census):
//! - the shader record's flag bytes at +0x38..+0x40. Byte 4 is a render-bucket
//!   bit mask (0x01 opaque, 0x02 alpha, 0x04 decal, 0x08 cutout, 0x40 water);
//!   byte 1 repeats it as an index and sets 0x80 for instanced / batch-LOD
//!   draws.
//! - known shader families, identified from the models that use them.
use serde_json::{json, Value};

/// (shader name hash, class, family description)
const FAMILIES: &[(&str, &str, &str)] = &[
    // vehicles
    ("4aa38282", "car_paint", "vehicle body (paint is a runtime colour; colour slot = engine-default white)"),
    ("476ac73a", "car_paint", "vehicle body variant"),
    ("03b27476", "car_paint", "vehicle body variant"),
    ("4d2b05e7", "car_paint", "vehicle body variant"),
    ("c5cdbdd2", "car_paint", "vehicle body variant"),
    ("389206d1", "opaque", "vehicle parts (bumpers, trim)"),
    ("29648098", "opaque", "vehicle parts"),
    ("fa9cc069", "window", "vehicle windows"),
    ("4dc0ac6a", "window", "vehicle windows (helicopters, boats)"),
    ("bd71bdbf", "window", "vehicle windows, untextured (mid LOD)"),
    ("921b19e5", "decal", "vehicle livery / sign decal"),
    // glass
    ("298c4c0f", "glass", "exterior window panes"),
    ("653582a4", "glass", "exterior glass"),
    ("6a817597", "glass", "interior glass, curtains, doors"),
    ("db8b21bd", "glass", "glass railings, door glass"),
    ("e3ba3129", "glass", "building doors / glazing"),
    ("656a1cdb", "glass", "bottles and drinking glasses"),
    ("12839ca7", "glass", "drinking glasses with liquid"),
    ("9ceb543e", "glass", "cups with liquid"),
    ("9f8ae601", "glass", "bottles"),
    ("0357215a", "glass", "cups"),
    ("d5a93b6f", "glass", "glass / sheer cloth"),
    ("d9ec330c", "emissive", "light bulbs (clear glass)"),
    // emissive
    ("faec4fc8", "emissive", "digital signs / screens"),
    ("f5ba53da", "emissive", "baked lightmap overlay"),
    ("3a09c775", "emissive", "LED lights and panels"),
    ("3bc8ac60", "emissive", "emissive detail"),
    ("60643f2e", "emissive", "traffic-light counter"),
    ("5b775918", "emissive", "phone screens"),
    // water
    ("7f3a6aa9", "water", "water surface"),
    ("97596162", "water", "fountain / VFX water"),
    ("735d2de6", "water", "waterfall VFX"),
    // terrain / multilayer
    ("61e09df7", "terrain_multilayer", "landscape blend (17 params)"),
    ("360e396b", "terrain_multilayer", "landscape blend (17 params)"),
    ("b21f1ba5", "terrain_multilayer", "landscape blend (17 params)"),
    ("0ad54cb9", "terrain_multilayer", "landscape blend (17 params)"),
    ("77d8ab27", "terrain_multilayer", "vertex-blended multilayer: ground, walls, interiors (12 params)"),
    ("1251cae5", "terrain_multilayer", "vertex-blended multilayer: ground, walls, interiors (12 params)"),
    ("cd8ceb5e", "terrain_multilayer", "vertex-blended multilayer: ground, walls, interiors (12 params)"),
    ("4f0b809b", "terrain_multilayer", "vertex-blended multilayer: ground, walls, interiors (12 params)"),
    ("b217b263", "terrain_multilayer", "vertex-blended multilayer: ground, walls, interiors (12 params)"),
    // other
    ("255417c3", "cutout", "vegetation"),
    ("e05f1a42", "decal", "road signs"),
    ("89a1cf72", "opaque", "ped head / skin"),
    ("379efcef", "opaque", "ped eyes"),
];

/// What each known texture parameter slot holds. Roles marked "likely" come
/// from formats and channel statistics, not from names.
pub const SLOTS: &[(&str, &str)] = &[
    ("c8e8c282", "base_color (sRGB)"),
    ("1e77bf4c", "normal (BC1, XY in RG, DirectX green)"),
    ("84c69350", "normal (BC5, DirectX green)"),
    ("58b6e707", "normal (BC5, DirectX green)"),
    ("4d56d7fe", "normal atlas (vehicle, BC1)"),
    ("2bb2ede8", "normal atlas (vehicle, BC1)"),
    ("739e3d0d", "packed material data, linear (likely roughness/metal/AO; channel order unverified)"),
    ("c6f46e79", "packed material data, linear (G/B near 0.5: likely detail-normal or mask pair)"),
    ("0df47048", "secondary colour / detail albedo (sRGB)"),
    ("7485dafb", "colour with opacity in alpha (sRGB)"),
    ("f151c063", "vehicle decal layer (RGBA)"),
    ("d37026cb", "vehicle dirt mask"),
    ("640111c6", "vehicle dirt mask"),
    ("b9f4bdac", "vehicle grime"),
    ("84d22f74", "vehicle scratches"),
    ("fb03ae36", "vehicle mask pack (green-dominant)"),
    ("2c8d7182", "tint palette (embedded in the model: rows of RGBA8 colours, picked per instance by the placement tint row)"),
];

pub fn slot_role(param_hash: &str) -> Option<&'static str> {
    SLOTS.iter().find(|s| s.0 == param_hash).map(|s| s.1)
}

/// Classify a shader. `flags` = the 8 record bytes at +0x38.
pub fn classify(name_hash: &str, flags: &[u8]) -> Value {
    let b1 = flags.get(1).copied().unwrap_or(0);
    let b4 = flags.get(4).copied().unwrap_or(0);
    let bucket = match b4 {
        x if x & 0x40 != 0 => "water",
        x if x & 0x04 != 0 => "decal",
        x if x & 0x02 != 0 => "alpha",
        x if x & 0x08 != 0 => "cutout",
        x if x & 0x01 != 0 => "opaque",
        _ => "unknown",
    };
    let fam = FAMILIES.iter().find(|f| f.0 == name_hash);
    let class = fam.map(|f| f.1).unwrap_or(bucket);
    // Known opaque families keep OPAQUE even when the bucket says alpha
    // (vehicle parts 389206d1 sit in the alpha bucket, likely for lenses).
    let alpha_mode = match (class, bucket) {
        ("opaque" | "car_paint", "cutout") => "MASK",
        ("opaque" | "car_paint" | "terrain_multilayer", _) => "OPAQUE",
        ("glass" | "window" | "water" | "alpha" | "decal", _) => "BLEND",
        (_, "alpha" | "decal" | "water") => "BLEND",
        (_, "cutout") => "MASK",
        _ => "OPAQUE",
    };
    // Decal-bucket materials (road markings, grime, signage, terrain blend
    // layers) are coplanar with the surface below them. No per-material
    // priority is known in the shader record (bytes 2-3 are a u16, likely
    // the constant-buffer size), so the order comes from the class:
    // terrain blend layers first (0), then markings / grime / signage (1).
    // bias_m is a recommended offset along the normal (or an equivalent
    // depth bias) for converters.
    let decal = if bucket == "decal" {
        let order: u8 = if class == "terrain_multilayer" { 0 } else { 1 };
        json!({"bias_m": 0.004 + 0.002 * order as f64, "order": order, "order_source": "class: terrain blend layers 0, other decals 1 (no per-material priority found in the record)", "depth_write": alpha_mode == "OPAQUE" || alpha_mode == "MASK"})
    } else {
        Value::Null
    };
    json!({
        "class": class,
        "render_bucket": bucket,
        "decal": decal,
        "alpha_mode": alpha_mode,
        "instanced_or_batch_lod": b1 & 0x80 != 0,
        "family": fam.map(|f| f.2),
        "source": if fam.is_some() { "known shader family + record flags" } else { "record flags (+0x38 byte 4)" },
        "record_flags": flags.iter().map(|x| format!("{x:02x}")).collect::<String>(),
    })
}

/// Paint block for car-paint materials: what a converter needs to rebuild it.
pub fn paint_block() -> Value {
    json!({
        "colour": null,
        "colour_source": "runtime (per-vehicle colour tables not decoded); the converter chooses the colour",
        "suggested_default_linear_rgb": [0.55, 0.55, 0.55],
        "masks": {"decal": "f151c063", "dirt": ["d37026cb", "640111c6"], "grime": "b9f4bdac", "scratches": "84d22f74", "mask_pack": "fb03ae36"},
        "csp_hint": "smCarPaint: txDiffuse = white * paint colour, use dirt/scratch masks for txMaps where wanted; clear coat on",
        "beamng_hint": "instanceDiffuse + colorPaletteMap; base colour white so the paint colour shows",
    })
}

/// glTF material fields for a classification: alpha mode and stand-in PBR
/// factors so untextured classes (paint, glass) render sensibly.
pub fn gltf_fields(c: &Value, has_base: bool) -> Value {
    let class = c["class"].as_str().unwrap_or("opaque");
    let mut m = json!({"alphaMode": c["alpha_mode"]});
    if c["alpha_mode"] == "MASK" {
        m["alphaCutoff"] = json!(0.5);
    }
    match class {
        "car_paint" => {
            m["pbrMetallicRoughness"] = json!({"metallicFactor": 0.6, "roughnessFactor": 0.25});
            if !has_base {
                m["pbrMetallicRoughness"]["baseColorFactor"] = json!([0.55, 0.55, 0.55, 1.0]);
            }
        }
        "glass" | "window" => {
            m["pbrMetallicRoughness"] = json!({"metallicFactor": 0.0, "roughnessFactor": 0.05});
            if !has_base {
                m["pbrMetallicRoughness"]["baseColorFactor"] = json!([0.08, 0.09, 0.1, 0.35]);
            }
        }
        "water" => m["pbrMetallicRoughness"] = json!({"metallicFactor": 0.0, "roughnessFactor": 0.05, "baseColorFactor": [0.1, 0.2, 0.25, 0.6]}),
        _ => {}
    }
    m
}

pub fn is_paint(name_hash: &str) -> bool {
    FAMILIES.iter().any(|f| f.0 == name_hash && f.1 == "car_paint")
}

/// Layer layout of the two-layer vertex-blended families (terrain_multilayer
/// walls/ground and the terrain blend decal), or None.
///
/// Evidence (2026-11-20, 2,868-model sample, 4.7 K multilayer materials):
/// * Slots group by texture size: inside a material the slots of one group
///   always share their aspect ratio, and the groups differ from each other
///   (b217b263 / 1251cae5 / cd8ceb5e / 77d8ab27: {p0 colour, p6 normal,
///   p7 data, p9 two-channel} and {p4 colour, p10 normal, p5 data, p2
///   two-channel}; 4f0b809b: {p2, p10, p3, p0} and {p7, p4, p5, p9}).
///   Confidence: high for the grouping.
/// * COLOR0.a is the only vertex channel painted over the full 0..255 range
///   (R is mostly a constant 255, G stays within 0..45, B has a few
///   distinct values); A = 0 on most vertices, so A weights layer 2 over
///   the base. Confidence: medium.
/// * UV sets: layer 2 follows TEXCOORD_1 (texel anisotropy matches its
///   textures in 108 of 117 primitives for 1251cae5, 54 of 72 for
///   cd8ceb5e); the base uses TEXCOORD_0 by elimination (TEXCOORD_2 is
///   often constant). Confidence: medium (layer 2), low (base).
/// * Data maps (BC7): R varies (AO-like), G is nearly constant per texture,
///   B and A vary. Two-channel maps (BC1): R = 0, G/B vary and are
///   anti-correlated. Meanings unverified.
pub fn layers(name_hash: &str, entries: &[Value]) -> Option<Value> {
    // (colour, normal, data, two-channel) for base and layer 2, overlay, extra data
    let (base, l2, overlay, extra): ([usize; 4], [usize; 4], usize, usize) = match name_hash {
        "b217b263" | "1251cae5" | "cd8ceb5e" | "77d8ab27" => ([0, 6, 7, 9], [4, 10, 5, 2], 8, 11),
        "4f0b809b" => ([2, 10, 3, 0], [7, 4, 5, 9], 6, 11),
        _ => return None,
    };
    let tex = |p: usize| -> Value {
        match entries.iter().find(|e| e["param"].as_u64() == Some(p as u64)) {
            Some(e) => json!({"param": p, "png": e.get("png").cloned().unwrap_or(Value::Null), "status": e.get("status").cloned().unwrap_or(Value::Null)}),
            None => json!({"param": p, "png": null, "status": "no texture bound"}),
        }
    };
    let layer = |name: &str, s: [usize; 4], uv: &str, weight: &str| {
        json!({"name": name, "colour": tex(s[0]), "normal": tex(s[1]), "data": tex(s[2]), "two_channel": tex(s[3]), "uv_set": uv, "weight": weight})
    };
    Some(json!({
        "layers": [
            layer("base", base, "TEXCOORD_0", "1 - COLOR0.a"),
            layer("layer2", l2, "TEXCOORD_1", "COLOR0.a"),
        ],
        "overlay": {"colour_alpha": tex(overlay), "uv_set": null, "weight": "texture alpha (grime/dirt); UV set unknown"},
        "extra_data": tex(extra),
        "vertex_color_channels": {"r": "unverified: a baked shading term, mostly 255 (dark blobs on ground tiles where trees stand, 23 on average on trees, 252 on interior shaders, so not sky occlusion alone)", "g": "unverified: a per-surface mask, equal to r on 63 % of ordinary vertices; low on ground shaders (street tile: grass 0, road mid grey, pavement 255)", "b": "tint palette index (entry within the palette row)", "a": "layer 2 weight"},
        "confidence": {"grouping": "high", "weight_channel": "medium", "uv_layer2": "medium", "uv_base": "low", "data_channels": "unknown"},
        "note": "Derived from texture-size grouping and vertex/UV statistics, not from shader code. Blend: lerp(base, layer2, COLOR0.a), then the overlay by its alpha. Normal maps: DirectX green.",
    }))
}
