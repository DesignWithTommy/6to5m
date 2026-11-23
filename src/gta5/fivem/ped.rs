//! `6ixpack fivem ped <name>`: a 6m ped (story character or NPC) -> a
//! streamable FiveM ped resource.
//!
//! Output (GTA V ambient component-ped layout, one set of files per ped):
//!   <out>/fxmanifest.lua, <out>/peds.meta
//!   <out>/stream/<ped>.yft   skeleton (GTA V ped bone names and tags)
//!   <out>/stream/<ped>.ydd   one skinned drawable per component
//!   <out>/stream/<ped>.ytd   textures (head_diff_000_a_uni, ...)
//!   <out>/stream/<ped>.ymt   CPedVariationInfo
//!
//! Skinning: 6m clothing vertices index the shared 6m ped skeleton through
//! each model's bone palette (sixng-core `pedskel`). Every 6m bone is mapped
//! to a bone of the standard GTA V ped skeleton (`gta5::ped::BONES`): by
//! name, else to its nearest mapped ancestor. The GTA V skeleton keeps GTA V
//! bone frames (so GTA V clips play) with the 6m joint positions (so the
//! character keeps its proportions).

#[path = "ped_groom.rs"]
mod groom;

use crate::gta5::ydr::uv_tangents;
use crate::gta5::ped::{self as gp, ShaderDef, SkelBone, Skeleton, SkinDrawable, SkinGeom, SkinVert};
use crate::gta5::{meta, ped_bones, ytd};
use serde_json::json;
use sixng_core::drawable::{self, Bone, DrawableInfo};
use sixng_core::resource::Resource;
use sixng_core::scan::Entry;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::error::Error;
use std::path::{Path, PathBuf};

pub const USAGE: &str = "\
usage: 6ixpack fivem ped <name> --game <6m dir> --out <resource dir> [options]
  <name>             ped prefix, e.g. cs_lucia, cs_jason, hc_luciasgrandma
                     (every packs.rpf drawable named <name>_* is a candidate)
  --work <dir>       sixng work dir with index/packs.json (default $SIXNG_WORK or work)
  --ped <model>      GTA V model name for the output (default: <name>)
  --list             list the candidate drawables and their components, write nothing
  --hair <id>        strand groom for the hair (packs.rpf@<hex>; see --grooms), drawn as cards
  --beard <id>       strand groom for the beard / moustache
  --grooms [scalp|face|brows|all]
                     list grooms whose roots fit this ped's head (no resource links
                     peds to grooms, so pick one by eye), write nothing
  --hair-color r,g,b card colour (default 40,28,20)
  --hair-width <m>   card half-width at the root (default 0.012)
  --all              every part of the character, not just the default look
  --only <a,b>       keep only drawables whose name contains one of these words
  --max-tex <px>     cap texture size (default 1024)";

pub struct PedOpts {
    pub name: String,
    pub game: PathBuf,
    pub out: PathBuf,
    pub work: PathBuf,
    pub ped: String,
    pub list: bool,
    pub only: Vec<String>,
    pub all: bool,
    pub hair: Option<String>,
    pub beard: Option<String>,
    pub grooms: Option<String>,
    pub hair_color: [u8; 3],
    pub hair_width: f32,
    pub max_tex: u16,
}

fn parse(args: &[String]) -> Result<PedOpts, String> {
    let mut o = PedOpts {
        name: String::new(),
        game: PathBuf::new(),
        out: PathBuf::new(),
        work: std::env::var_os("SIXNG_WORK").map(PathBuf::from).unwrap_or_else(|| "work".into()),
        ped: String::new(),
        list: false,
        only: Vec::new(),
        all: false,
        hair: None,
        beard: None,
        grooms: None,
        hair_color: [40, 28, 20],
        hair_width: 0.012,
        max_tex: 1024,
    };
    let mut it = args.iter();
    let val = |it: &mut std::slice::Iter<String>, k: &str| it.next().cloned().ok_or(format!("{k} needs a value"));
    while let Some(a) = it.next() {
        match a.as_str() {
            "--game" => o.game = val(&mut it, a)?.into(),
            "--out" => o.out = val(&mut it, a)?.into(),
            "--work" => o.work = val(&mut it, a)?.into(),
            "--ped" => o.ped = val(&mut it, a)?.to_lowercase(),
            "--list" => o.list = true,
            "--all" => o.all = true,
            "--hair" => o.hair = Some(val(&mut it, a)?),
            "--beard" => o.beard = Some(val(&mut it, a)?),
            "--grooms" => o.grooms = Some(it.clone().next().filter(|v| ["scalp", "face", "brows", "all"].contains(&v.to_lowercase().as_str())).map(|v| v.to_lowercase()).unwrap_or_else(|| "scalp".into())),
            "--hair-width" => o.hair_width = val(&mut it, a)?.parse().map_err(|e| format!("--hair-width: {e}"))?,
            "--hair-color" => {
                let v: Vec<u8> = val(&mut it, a)?.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                o.hair_color = <[u8; 3]>::try_from(v.as_slice()).map_err(|_| "--hair-color: expected r,g,b")?;
            }
            "--only" => o.only = val(&mut it, a)?.split(',').map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()).collect(),
            "--max-tex" => o.max_tex = val(&mut it, a)?.parse().map_err(|e| format!("--max-tex: {e}"))?,
            s if s.starts_with('-') => return Err(format!("unknown argument {s}\n{USAGE}")),
            s if o.grooms.as_deref() == Some(s) && ["scalp", "face", "brows", "all"].contains(&s) => {}
            s if o.name.is_empty() => o.name = s.to_lowercase(),
            s => return Err(format!("unexpected argument {s}\n{USAGE}")),
        }
    }
    if o.name.is_empty() || o.game.as_os_str().is_empty() || (o.out.as_os_str().is_empty() && !o.list && o.grooms.is_none()) {
        return Err(USAGE.into());
    }
    if o.ped.is_empty() {
        o.ped = o.name.clone();
    }
    if o.ped.len() > 40 || !o.ped.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') {
        return Err(format!("--ped {}: use letters, digits and _ only (max 40)", o.ped));
    }
    Ok(o)
}

/// GTA V ped component slots (ePedVarComp order) and their file tokens.
const SLOTS: [&str; 12] = ["head", "berd", "hair", "uppr", "lowr", "hand", "feet", "teef", "accs", "task", "decl", "jbib"];

// Part selection ported from SixNG `outfit.rs` (GPL-3.0, upstream de366fb):
// a character is `<prefix>_<item>_<fit>_...` with a fit tag (`fr1`, `mr1`);
// the default look is one part per outfit slot, the first in name order.

/// Fit tag (`mr1`, `fr1`, `mk1` ...): sex + body type + number.
fn is_fit(t: &str) -> bool {
    let b = t.as_bytes();
    b.len() == 3 && (b[0] == b'm' || b[0] == b'f') && b[1].is_ascii_lowercase() && b[2].is_ascii_digit()
}

/// Character prefix (tokens before `<item>_<fit>`, at least two) and item word.
fn prefix_item(name: &str) -> Option<(String, String)> {
    let t: Vec<&str> = name.split('_').collect();
    let i = t.iter().position(|x| is_fit(x))?;
    (i >= 3).then(|| (t[..i - 1].join("_"), t[i - 1].to_string()))
}

/// SixNG outfit slot of an item word; None = accessory (off by default).
fn outfit_slot(item: &str) -> Option<&'static str> {
    const SL: &[(&str, &str)] = &[
        ("hair", "hair"), ("beard", "beard"), ("brows", "brows"), ("lashes", "lashes"), ("eyes", "eyes"), ("teeth", "teeth"),
        ("bodyupper", "body_upper"), ("bodylower", "body_lower"), ("fullbody", "full"), ("dress", "full"), ("jumpsuit", "full"), ("overalls", "full"),
        ("jacket", "outer"), ("coat", "outer"), ("blazer", "outer"), ("cardigan", "outer"),
        ("tshirt", "top"), ("shirt", "top"), ("sweatshirt", "top"), ("hoodie", "top"), ("tanktop", "top"), ("blouse", "top"), ("bra", "top"),
        ("pants", "bottom"), ("shorts", "bottom"), ("skirt", "bottom"), ("undies", "bottom"), ("leggings", "bottom"),
        ("boots", "feet"), ("shoes", "feet"), ("sneakers", "feet"), ("sandals", "feet"), ("heels", "feet"), ("piercing", "piercing"),
    ];
    if item == "head" {
        return Some("head");
    }
    SL.iter().filter(|(w, _)| item.starts_with(w)).max_by_key(|(w, _)| w.len()).map(|(_, s)| *s)
}

/// Selection group of a part (SixNG `outfit_options`).
fn group_of(name: &str) -> String {
    let item = prefix_item(name).map(|x| x.1).unwrap_or_default();
    match outfit_slot(&item) {
        Some("head") if name.contains("_eyes") => "head eyes".into(),
        Some("head") if name.contains("_earring") => "head earring".into(),
        Some(s) => s.into(),
        None => item,
    }
}

/// GTA V component slot of a 6m drawable. Each default-look group gets its
/// own slot, because a ped shows one drawable per slot. Eyes, brows and
/// lashes are merged into the head.
fn slot_of(name: &str) -> Option<usize> {
    let n = name.to_ascii_lowercase();
    if ["bipedbounds", "_cloth_sim", "physics"].iter().any(|k| n.contains(k)) {
        return None;
    }
    let item = prefix_item(&n).map(|x| x.1).unwrap_or_default();
    Some(match outfit_slot(&item) {
        Some("head" | "eyes" | "brows" | "lashes") => 0,
        Some("beard") => 1,
        Some("hair") => 2,
        Some("body_upper") => 3,
        Some("bottom") => 4,
        Some("feet") => 6,
        Some("teeth") => 7,
        Some("top" | "full") => 8,
        Some("body_lower") => 9,
        Some("piercing") => 10,
        Some("outer") => 11,
        _ if item.starts_with("legacyhead") => 0,
        _ if item.starts_with("legacyped") => 3,
        _ if item.starts_with("glove") => 5,
        _ => 9, // accessories (only with --all / --only)
    })
}

/// 6m bone name -> GTA V bone name, for names that differ.
const ALIASES: &[(&str, &str)] = &[
    ("SKEL_Neck0", "SKEL_Neck_1"),
    ("FB_Root", "FACIAL_facialRoot"),
    ("FB_Jaw", "FB_Jaw_001"),
    ("FB_Tongue_Root", "FB_Tongue_001"),
    ("RB_L_Wrist02", "RB_L_ForeArmRoll"),
    ("RB_R_Wrist02", "RB_R_ForeArmRoll"),
    ("RB_L_Shoulder01", "RB_L_ArmRoll"),
    ("RB_R_Shoulder01", "RB_R_ArmRoll"),
    ("MH_L_OuterElbow", "MH_L_Elbow"),
    ("MH_R_OuterElbow", "MH_R_Elbow"),
    ("BM_L_Breast", "SPR_L_Breast"),
    ("BM_R_Breast", "SPR_R_Breast"),
];

fn mul(a: &[f32; 16], b: &[f32; 16]) -> [f32; 16] {
    let mut o = [0f32; 16];
    for c in 0..4 {
        for r in 0..4 {
            o[c * 4 + r] = (0..4).map(|k| a[k * 4 + r] * b[c * 4 + k]).sum();
        }
    }
    o
}

/// The GTA V skeleton for a 6m ped skeleton, and the 6m bone -> GTA V bone
/// map. Bones present in 6m (by name or alias) take the 6m bind pose,
/// re-expressed relative to their GTA V parent; the others keep the GTA V
/// local pose under their (6m-placed) parent.
fn retarget(six: &[Bone], female: bool) -> (Skeleton, Vec<u8>, usize) {
    let (defs, sig) = if female { (ped_bones::FEMALE, ped_bones::FEMALE_SIG) } else { (ped_bones::MALE, ped_bones::MALE_SIG) };
    let gidx: HashMap<String, usize> = defs.iter().enumerate().map(|(i, d)| (d.0.to_ascii_lowercase(), i)).collect();
    let alias: HashMap<String, &str> = ALIASES.iter().map(|(a, g)| (a.to_ascii_lowercase(), *g)).collect();
    let direct: Vec<Option<usize>> = six
        .iter()
        .map(|b| {
            let l = b.name.to_ascii_lowercase();
            gidx.get(&l).copied().or_else(|| alias.get(&l).and_then(|g| gidx.get(&g.to_ascii_lowercase()).copied()))
        })
        .collect();
    let mut src: Vec<Option<usize>> = vec![None; defs.len()];
    for (s, g) in direct.iter().enumerate() {
        if let Some(g) = *g {
            if src[g].is_none() {
                src[g] = Some(s);
            }
        }
    }
    let matched = src.iter().filter(|s| s.is_some()).count();
    let mut world: Vec<[f32; 16]> = Vec::with_capacity(defs.len());
    for (i, d) in defs.iter().enumerate() {
        let w = match src[i].and_then(|s| six[s].bind_matrix) {
            Some(m) => m,
            None => {
                let l = gp::m_trs(d.5, d.6);
                if d.2 >= 0 { mul(&world[d.2 as usize], &l) } else { l }
            }
        };
        world.push(w);
    }
    let bones = defs
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let l = if d.2 >= 0 { mul(&gp::m_inv(&world[d.2 as usize]), &world[i]) } else { world[i] };
            SkelBone { name: d.0.into(), tag: d.1, parent: d.2, sibling: d.3, flags: d.4, trans: [l[12], l[13], l[14]], rot: gp::m_quat(&l), unk: d.7 }
        })
        .collect();
    let mut map = vec![0u8; six.len()];
    for (s, m) in map.iter_mut().enumerate() {
        let mut c = Some(s);
        let mut hops = 0;
        while let Some(k) = c {
            if let Some(g) = direct[k] {
                *m = g as u8;
                break;
            }
            c = six[k].parent.map(|p| p as usize);
            hops += 1;
            if hops > 64 {
                break;
            }
        }
    }
    (Skeleton { bones, sig }, map, matched)
}

/// Diffuse/colour parameter hashes in priority order (model::COLOR_SLOTS).
const COLOR_SLOTS: [u32; 16] = [
    3370697346, 756347250, 0x346394EA, 0x7D4F12E2, 3464205966, 3632507550, 851042057, 1024521143, 3853173992, 2464948076, 3389132183, 3547013225,
    234123336, 3831897388, 3489704073, 1380592926,
];

struct TexSource {
    game: PathBuf,
    catalog: Option<crate::texture::Catalog>,
}

impl TexSource {
    /// RGBA of a texture: TTPF stores by 64-bit hash, then the ttdb pools by
    /// asset key.
    fn rgba(&self, h64: u64, key: u64) -> Result<(Vec<u8>, u32, u32), String> {
        let mut errs = Vec::new();
        if h64 != 0 {
            let set = sixng_core::ttpf::global(&self.game);
            match set.get(h64) {
                Some((store, t)) if t.decodable() => match store.data_ex(t) {
                    Ok((data, skipped)) => {
                        let mip = if skipped == 0 { Some(0) } else { store.best_mip(t) };
                        match mip.map(|m| sixng_core::ttpf::decode_rgba(t, &data, m)) {
                            Some(Ok(x)) => return Ok(x),
                            Some(Err(e)) => errs.push(format!("ttpf: {e}")),
                            None => errs.push("ttpf: only BCPack mips".into()),
                        }
                    }
                    Err(e) => errs.push(format!("ttpf: {e}")),
                },
                Some(_) => errs.push("ttpf: not decodable".into()),
                None => errs.push("ttpf: not found".into()),
            }
        }
        if key != 0 {
            if let Some(loc) = self.catalog.as_ref().and_then(|c| c.find(key).iter().find(|l| l.extractable)) {
                let r = (|| -> Result<(Vec<u8>, u32, u32), String> {
                    let f = fs::File::open(&loc.path).map_err(|e| e.to_string())?;
                    // SAFETY: read-only map of a game file.
                    let map = unsafe { memmap2::Mmap::map(&f) }.map_err(|e| e.to_string())?;
                    let pool = crate::texture::Pool::open(&map).map_err(|e| e.to_string())?;
                    let t = pool.textures.get(loc.index).ok_or("pool index out of range")?;
                    let dds = pool.create_dds(t).map_err(|e| e.to_string())?;
                    let (w, h, rgba) = crate::texture::dds_preview(&dds).map_err(|e| e.to_string())?;
                    Ok((rgba, w, h))
                })();
                match r {
                    Ok(x) => return Ok(x),
                    Err(e) => errs.push(format!("pools: {e}")),
                }
            }
        }
        Err(if errs.is_empty() { "no texture reference".into() } else { errs.join("; ") })
    }
}

/// Box resample to (tw, th).
fn resize(src: &[u8], w: u32, h: u32, tw: u32, th: u32) -> Vec<u8> {
    let mut d = vec![0u8; (tw * th * 4) as usize];
    for y in 0..th {
        for x in 0..tw {
            let (x0, y0) = (x * w / tw, y * h / th);
            let x1 = ((x + 1) * w / tw).max(x0 + 1).min(w);
            let y1 = ((y + 1) * h / th).max(y0 + 1).min(h);
            let mut acc = [0u32; 4];
            let mut n = 0u32;
            for yy in y0..y1 {
                for xx in x0..x1 {
                    let o = ((yy * w + xx) * 4) as usize;
                    for c in 0..4 {
                        acc[c] += src[o + c] as u32;
                    }
                    n += 1;
                }
            }
            let o = ((y * tw + x) * 4) as usize;
            for c in 0..4 {
                d[o + c] = (acc[c] / n.max(1)) as u8;
            }
        }
    }
    d
}

/// DXT1/DXT5 texture with a box-filtered mip chain (pure Rust, texpresso).
fn make_tex(name: &str, rgba: &[u8], w: u32, h: u32, max: u16) -> ytd::Tex {
    let (mut rgba, mut w, mut h) = (rgba.to_vec(), w, h);
    let (tw, th) = (w.next_power_of_two().max(4), h.next_power_of_two().max(4));
    if (tw, th) != (w, h) {
        rgba = resize(&rgba, w, h, tw, th);
        (w, h) = (tw, th);
    }
    while w.max(h) > max as u32 && w >= 8 && h >= 8 {
        rgba = resize(&rgba, w, h, w / 2, h / 2);
        (w, h) = (w / 2, h / 2);
    }
    let opaque = rgba.chunks_exact(4).all(|p| p[3] >= 250);
    let (fmt, tf) = if opaque { (texpresso::Format::Bc1, ytd::TexFormat::DXT1) } else { (texpresso::Format::Bc3, ytd::TexFormat::DXT5) };
    let mut data = Vec::new();
    let (mut cur, mut cw, mut ch, mut levels) = (rgba, w, h, 0u8);
    loop {
        let mut out = vec![0u8; fmt.compressed_size(cw as usize, ch as usize)];
        fmt.compress(&cur, cw as usize, ch as usize, texpresso::Params::default(), &mut out);
        data.extend_from_slice(&out);
        levels += 1;
        if cw < 8 || ch < 8 || levels >= 12 {
            break;
        }
        cur = resize(&cur, cw, ch, cw / 2, ch / 2);
        (cw, ch) = (cw / 2, ch / 2);
    }
    ytd::Tex { name: name.into(), width: w as u16, height: h as u16, levels, format: tf, data, unk32: 0, usage_data: 0, extra_flags: 0 }
}

fn flat_tex(name: &str, px: [u8; 4]) -> ytd::Tex {
    let rgba: Vec<u8> = (0..16 * 16).flat_map(|_| px).collect();
    make_tex(name, &rgba, 16, 16, 16)
}

/// A decoded component drawable before naming.
struct Part {
    src: String,
    slot: usize,
    geoms: Vec<(SkinGeom, bool)>, // (geometry, alpha)
    diffuse: Option<(Vec<u8>, u32, u32)>,
    warnings: Vec<String>,
}

fn tri_area(p: &[[f32; 3]], idx: &[u32]) -> f64 {
    idx.chunks_exact(3)
        .filter(|t| t.iter().all(|&i| (i as usize) < p.len()))
        .map(|t| {
            let (a, b, c) = (p[t[0] as usize], p[t[1] as usize], p[t[2] as usize]);
            let e1 = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
            let e2 = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
            let x = [e1[1] * e2[2] - e1[2] * e2[1], e1[2] * e2[0] - e1[0] * e2[2], e1[0] * e2[1] - e1[1] * e2[0]];
            ((x[0] * x[0] + x[1] * x[1] + x[2] * x[2]) as f64).sqrt() * 0.5
        })
        .sum()
}

/// Body bone a `GM_<set>_<side>_<part>_<dir>` helper (girth `U01`, cull `CULL`) sits on. The
/// helpers are stored parentless (parent index none) in the clothing
/// drawable, their attachment only named; they are runtime correctives, so
/// they skin rigidly to that body bone instead of being dropped.
fn gm_parent(name: &str) -> Option<&'static str> {
    // GM_<set>_<side>_<part>_<dir>: set U01 (girth) or CULL (cull-region helpers)
    let rest = name.strip_prefix("GM_")?.split_once('_')?.1;
    let (side, part) = rest.split_once('_')?;
    let part = part.split('_').next()?;
    // limb segments are numbered along the limb (Forearm01, Forearm02, ...)
    let limb = part.trim_end_matches(|c: char| c.is_ascii_digit());
    let lr = |l: &'static str, r: &'static str| if side == "L" { Some(l) } else if side == "R" { Some(r) } else { None };
    match limb {
        "Forearm" | "Elbow" => return lr("SKEL_L_Forearm", "SKEL_R_Forearm"),
        "UpperArm" => return lr("SKEL_L_UpperArm", "SKEL_R_UpperArm"),
        "Calf" => return lr("SKEL_L_Calf", "SKEL_R_Calf"),
        "Thigh" if side != "C" => return lr("SKEL_L_Thigh", "SKEL_R_Thigh"),
        _ => {}
    }
    Some(match (side, part) {
        ("C", "Pelvis") => "SKEL_Pelvis",
        ("C", "Spine0") => "SKEL_Spine0",
        ("C", "Spine1") => "SKEL_Spine1",
        ("C", "Spine2") => "SKEL_Spine2",
        ("C", "Spine3") => "SKEL_Spine3",
        ("C", "Spine4") => "SKEL_Spine4",
        ("C", "Spine5") | ("C", "Trap") => "SKEL_Spine5",
        ("C", "Neck1") => "SKEL_Neck1",
        ("C", "Thigh01") => "SKEL_Pelvis",
        ("L", "Thigh01") => "SKEL_L_Thigh",
        ("R", "Thigh01") => "SKEL_R_Thigh",
        ("L", "Trap") => "SKEL_L_Clavicle",
        ("R", "Trap") => "SKEL_R_Clavicle",
        ("L", "Delt") | ("L", "Armpit") | ("L", "UpperArm01") => "SKEL_L_UpperArm",
        ("R", "Delt") | ("R", "Armpit") | ("R", "UpperArm01") => "SKEL_R_UpperArm",
        ("L", "Forearm01") => "SKEL_L_Forearm",
        ("R", "Forearm01") => "SKEL_R_Forearm",
        _ => return None,
    })
}

/// Bone palettes also name helpers the drawable's own skeleton leaves out
/// (hash only): joaat of every `GM_U01_<side>_<part>[_F|_B][_<dir>]` name
/// `gm_parent` knows -> its body bone.
fn gm_by_hash() -> &'static HashMap<u32, &'static str> {
    static M: std::sync::OnceLock<HashMap<u32, &'static str>> = std::sync::OnceLock::new();
    M.get_or_init(|| {
        let mut parts: Vec<String> = ["Pelvis", "Spine0", "Spine1", "Spine2", "Spine3", "Spine4", "Spine5", "Trap", "Neck1", "Delt", "Armpit", "Elbow"].map(String::from).to_vec();
        for l in ["Forearm", "UpperArm", "Calf", "Thigh"] {
            parts.extend((1..=6).map(|i| format!("{l}{i:02}")));
        }
        let dirs = ["", "_O", "_I", "_CF", "_CB", "_L", "_R", "_LF", "_RF", "_LB", "_RB", "_F", "_B"];
        let mut m = HashMap::new();
        for (set, side) in ["U01", "CULL"].into_iter().flat_map(|a| ["L", "R", "C"].map(move |b| (a, b))) {
            for p in &parts {
                for mid in ["", "_F", "_B"] {
                    for d in dirs {
                        let n = format!("GM_{set}_{side}_{p}{mid}{d}");
                        if let Some(b) = gm_parent(&n) {
                            m.insert(sixng_core::meta7a::joaat(&n), b);
                        }
                    }
                }
            }
        }
        m
    })
}

/// Decode one 6m drawable into skinned geometry on the GTA V skeleton.
fn decode_part(game: &Path, e: &Entry, six: &sixng_core::pedskel::PedSkeleton, map6: &[u8], texs: &TexSource) -> Result<Part, String> {
    let name = e.name.clone().unwrap_or_default();
    let r = Resource::load(&game.join("packs.rpf"), e).map_err(|x| x.to_string())?;
    let info: DrawableInfo = drawable::describe(&r);
    let mut part = Part { src: name.clone(), slot: slot_of(&name).unwrap_or(8), geoms: Vec::new(), diffuse: None, warnings: Vec::new() };
    let slice = |i: usize| -> &[u8] {
        let b = &info.buffers[i];
        let end = ((b.gfx_off + b.bytes) as usize).min(r.gfx.len());
        &r.gfx[(b.gfx_off as usize).min(end)..end]
    };
    let joaat = sixng_core::meta7a::joaat;
    // own bone -> 6m ped bone (walk up the drawable's own skeleton)
    let own: Vec<Option<usize>> = (0..info.skeleton.len())
        .map(|i| {
            let mut c = Some(i);
            let mut hops = 0;
            while let Some(k) = c {
                if let Some(&p) = six.by_hash.get(&joaat(&info.skeleton[k].name)) {
                    return Some(p);
                }
                c = info.skeleton[k].parent.map(|p| p as usize);
                hops += 1;
                if hops > 64 {
                    break;
                }
            }
            gm_parent(&info.skeleton[i].name).and_then(|n| six.by_hash.get(&joaat(n)).copied())
        })
        .collect();
    let own_hash: HashMap<u32, usize> = info.skeleton.iter().enumerate().map(|(i, b)| (joaat(&b.name), i)).collect();
    let to_gta = |h: u32| -> Option<u8> {
        if let Some(&i) = six.by_hash.get(&h) {
            return Some(map6[i]);
        }
        own_hash.get(&h).and_then(|&i| own[i]).or_else(|| gm_by_hash().get(&h).and_then(|n| six.by_hash.get(&joaat(n)).copied())).map(|p| map6[p])
    };
    let main: Vec<_> = info.models.iter().filter(|m| !m.part && m.lod == 0).collect();
    let models: Vec<_> = if main.iter().any(|m| !m.geometries.is_empty()) { main } else { info.models.iter().filter(|m| m.part).collect() };
    let key_of = |sh: &drawable::Shader, param: usize| -> u64 {
        (|| {
            let ta = r.sys_off(r.u64(sh.record_off as usize + 0x10))?;
            let t = r.sys_off(r.u64(ta + 8 * param))?;
            let tr = r.sys_off(r.u64(t + 0x30))?;
            Some(r.u64(tr + 0x28))
        })()
        .unwrap_or(0)
    };
    let mut best_area = 0f64;
    let mut unmapped = 0usize;
    let mut miss: HashMap<String, usize> = HashMap::new();
    for md in models {
        let pal: Option<Vec<Option<u8>>> = md.bone_palette.as_ref().map(|p| p.iter().map(|&h| to_gta(h)).collect());
        for (k, &gi) in md.geometries.iter().enumerate() {
            let g = &info.geometries[gi];
            let Some(h) = info.buffers[g.vertex_buffer].mesh_blob.as_ref() else { continue };
            let m = drawable::decode_mesh(slice(g.vertex_buffer), h, slice(g.index_buffer));
            let n = m.positions.len();
            if m.indices.is_empty() || n <= 3 {
                continue; // hair/brows/lashes are 3-vertex stubs: the strands are separate type-3 resources
            }
            if n > 65535 {
                part.warnings.push(format!("geometry {gi}: {n} vertices (max 65535), skipped"));
                continue;
            }
            let joint = |j: u16| -> Option<u8> {
                match &pal {
                    Some(p) => p.get(j as usize).copied().flatten(),
                    None => own.get(j as usize).copied().flatten().map(|b| map6[b]),
                }
            };
            let uv = m.uvs.iter().find(|u| u.0 == 3).or(m.uvs.first()).map(|u| u.1.clone()).filter(|u| u.len() == n).unwrap_or_else(|| vec![[0.0; 2]; n]);
            let normals = m.normals.clone().filter(|v| v.len() == n).unwrap_or_else(|| vec![[0.0, 0.0, 1.0]; n]);
            let tangents = m.tangents.clone().filter(|v| v.len() == n).unwrap_or_else(|| uv_tangents(&m.positions, &normals, &uv, &m.indices));
            let mut verts = Vec::with_capacity(n);
            for i in 0..n {
                let mut infl: Vec<(u8, u32)> = Vec::with_capacity(8);
                for (js, ws) in [(m.joints.as_ref(), m.weights.as_ref()), (m.joints1.as_ref(), m.weights1.as_ref())] {
                    let (Some(js), Some(ws)) = (js, ws) else { continue };
                    for c in 0..4 {
                        let w = ws[i][c] as u32;
                        if w == 0 {
                            continue;
                        }
                        match joint(js[i][c]) {
                            Some(b) => match infl.iter_mut().find(|x| x.0 == b) {
                                Some(x) => x.1 += w,
                                None => infl.push((b, w)),
                            },
                            None => {
                                unmapped += 1;
                                let j = js[i][c] as usize;
                                let h = md.bone_palette.as_ref().and_then(|p| p.get(j).copied());
                                *miss.entry(match h { Some(h) => format!("{h:08x}"), None => format!("#{j}") }).or_default() += 1;
                            }
                        }
                    }
                }
                infl.sort_by(|a, b| b.1.cmp(&a.1));
                infl.truncate(4);
                if infl.is_empty() {
                    infl.push((0, 255));
                }
                let tot: u32 = infl.iter().map(|x| x.1).sum::<u32>().max(1);
                let (mut wts, mut ids, mut acc) = ([0u8; 4], [0u8; 4], 0u32);
                for (c, (b, w)) in infl.iter().enumerate() {
                    let v = if c + 1 == infl.len() { 255 - acc } else { ((w * 255 + tot / 2) / tot).min(255 - acc) };
                    acc += v;
                    wts[c] = v as u8;
                    ids[c] = *b;
                }
                verts.push(SkinVert { pos: m.positions[i], normal: normals[i], uv: uv[i], tangent: tangents[i], weights: wts, bones: ids });
            }
            let indices: Vec<u16> = m.indices.iter().map(|&x| x as u16).collect();
            let sh = md.shader_indices.get(k).and_then(|&s| info.shaders.get(s as usize));
            let alpha = sh.is_some_and(|sh| sh.flags.first().copied().unwrap_or(0) & 0x2 != 0);
            let area = tri_area(&m.positions, &m.indices);
            if area > best_area {
                if let Some(sh) = sh {
                    let ph = |t: &drawable::TexRef| u32::from_str_radix(t.param_hash.trim_start_matches("0x"), 16).ok();
                    let mut cands: Vec<&drawable::TexRef> = COLOR_SLOTS.iter().filter_map(|slot| sh.textures.iter().find(|t| ph(t) == Some(*slot))).collect();
                    cands.extend(sh.textures.iter().filter(|t| !ph(t).is_some_and(|h| COLOR_SLOTS.contains(&h))));
                    for t in cands {
                        let h64 = u64::from_str_radix(t.hash64.trim_start_matches("0x"), 16).unwrap_or(0);
                        match texs.rgba(h64, key_of(sh, t.param)) {
                            Ok(x) => {
                                part.diffuse = Some(x);
                                best_area = area;
                                break;
                            }
                            Err(err) => part.warnings.push(format!("texture {}: {err}", t.hash32)),
                        }
                    }
                }
            }
            part.geoms.push((SkinGeom { verts, indices, shader: 0 }, alpha));
        }
    }
    if unmapped > 0 {
        let mut top: Vec<_> = miss.into_iter().collect();
        top.sort_by(|a, b| b.1.cmp(&a.1));
        let top: Vec<String> = top.iter().take(8).map(|(k, n)| format!("{k} x{n}")).collect();
        part.warnings.push(format!("{unmapped} vertex influences name bones in neither skeleton (dropped): {}", top.join(", ")));
    }
    if part.geoms.is_empty() {
        return Err("no geometry".into());
    }
    Ok(part)
}

/// Candidate drawables: packs.rpf drawables of character `<name>` (one entry
/// per name, the largest copy). Default: SixNG's default look, one part per
/// outfit slot (accessories, `_alt_`, holsters, `_cull` and first-person
/// copies left out; a one-piece outfit replaces top and bottom). `--all`
/// keeps every part, `--only` filters by name words.
fn candidates<'a>(ix: &'a sixng_core::scan::ArchiveIndex, o: &PedOpts) -> Vec<&'a Entry> {
    let mut by: BTreeMap<String, &Entry> = BTreeMap::new();
    for e in &ix.entries {
        let Some(n) = e.name.as_deref() else { continue };
        let l = n.to_ascii_lowercase();
        if e.rtype.as_deref() != Some("dr") || slot_of(&l).is_none() || prefix_item(&l).map(|x| x.0).as_deref() != Some(o.name.as_str()) {
            continue;
        }
        if !o.only.is_empty() && !o.only.iter().any(|w| l.contains(w.as_str())) {
            continue;
        }
        let cur = by.entry(l).or_insert(e);
        if (e.usize, std::cmp::Reverse(e.off)) > (cur.usize, std::cmp::Reverse(cur.off)) {
            *cur = e;
        }
    }
    if o.all || !o.only.is_empty() {
        return by.into_values().collect();
    }
    let skip = |n: &str| ["_cull", "_fp", "eyecap", "tattooprojection"].iter().any(|x| n.contains(x));
    let mut chosen: BTreeMap<String, &Entry> = BTreeMap::new();
    for pass in [false, true] {
        // second pass: a slot with only `_cull` copies takes one of those
        for (n, e) in &by {
            let item = prefix_item(n).map(|x| x.1).unwrap_or_default();
            if outfit_slot(&item).is_none() || n.contains("_alt_") || n.contains("holster") || skip(n) != pass {
                continue;
            }
            chosen.entry(group_of(n)).or_insert(e);
        }
    }
    if chosen.contains_key("full") {
        chosen.remove("top");
        chosen.remove("bottom");
    }
    // no head of its own (cs_jason): the legacy head and whole-body copies
    if !chosen.contains_key("head") {
        for (n, e) in &by {
            let item = prefix_item(n).map(|x| x.1).unwrap_or_default();
            if item.starts_with("legacyhead") || item.starts_with("legacyped") {
                chosen.entry(item).or_insert(e);
            }
        }
    }
    chosen.into_values().collect()
}

/// Order variations: the plain drawable first.
fn rank(name: &str) -> (u8, String) {
    let n = name.to_ascii_lowercase();
    (u8::from(n.contains("legacy")) * 2 + u8::from(n.contains("_old")) + u8::from(n.contains("cull")) * 4, n)
}

pub fn run(args: &[String]) -> Result<(), Box<dyn Error>> {
    let o = parse(args)?;
    sixng_core::names::set_work_dir(&o.work);
    let ip = sixng_core::scan::index_path(&o.work.join("index"), "packs.rpf");
    let ix = sixng_core::scan::load_index(&ip).map_err(|e| format!("{}: {e:#} (run `6ixpack sixng index packs.rpf` first)", ip.display()))?;
    let mut cands = candidates(&ix, &o);
    if cands.is_empty() {
        return Err(format!("no packs.rpf drawable is named {}_*", o.name).into());
    }
    let nm = |e: &Entry| e.name.clone().unwrap_or_default();
    cands.sort_by_key(|e| (slot_of(&nm(e)).unwrap_or(8), rank(&nm(e))));
    let female = cands.iter().filter(|e| nm(e).contains("_fr1")).count() >= cands.iter().filter(|e| nm(e).contains("_mr1")).count();
    if o.list {
        println!("{} ({}): {} drawables", o.name, if female { "female" } else { "male" }, cands.len());
        for e in &cands {
            println!("  {:<5} {}  {}", SLOTS[slot_of(&nm(e)).unwrap_or(8)], nm(e), e.id);
        }
        return Ok(());
    }
    let six = sixng_core::pedskel::get(&o.game, female).ok_or_else(|| format!("6m ped skeleton ({}) not found: index packs.rpf into {}", sixng_core::pedskel::source_name(female), o.work.display()))?;
    let (skel, map6, matched) = retarget(&six.bones, female);
    let nbones = skel.bones.len();
    let texs = TexSource { game: o.game.clone(), catalog: crate::texture::Catalog::build(&o.game.join("data").join("ttdb")).ok() };
    use rayon::prelude::*;
    let decoded: Vec<(String, Result<Part, String>)> = cands.par_iter().map(|e| (nm(e), decode_part(&o.game, e, six, &map6, &texs))).collect();

    // eyes/lashes join the head drawable they belong to
    let (mut parts, mut extra, mut report) = (Vec::new(), Vec::new(), Vec::new());
    for (n, r) in decoded {
        match r {
            Ok(p) => {
                report.push(json!({"source": n, "slot": SLOTS[p.slot], "geometries": p.geoms.len(), "texture": p.diffuse.is_some(), "warnings": p.warnings}));
                if n.contains("_eyes") || n.contains("lash") || n.contains("_brows") { extra.push(p) } else { parts.push(p) }
            }
            Err(e) => report.push(json!({"source": n, "error": e})),
        }
    }
    for e in extra {
        let base = e.src.replace("_eyes", "").replace("lashes", "head").replace("brows", "head");
        let common = |a: &str, b: &str| a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count();
        let best = parts.iter_mut().filter(|p| p.slot == 0).max_by_key(|p| common(&p.src, &base));
        match best {
            Some(h) => h.geoms.extend(e.geoms),
            None => parts.push(e),
        }
    }
    // strand grooms: list candidates, or add hair / beard cards
    if o.grooms.is_some() || o.hair.is_some() || o.beard.is_some() {
        let head_box = parts.iter().filter(|p| p.slot == 0).flat_map(|p| p.geoms.iter().flat_map(|g| g.0.verts.iter().map(|v| v.pos))).fold(None, |b: Option<[[f32; 3]; 2]>, p| {
            Some(match b {
                None => [p, p],
                Some([lo, hi]) => [std::array::from_fn(|i| lo[i].min(p[i])), std::array::from_fn(|i| hi[i].max(p[i]))],
            })
        });
        if let Some(cat) = &o.grooms {
            let names: HashMap<u32, String> = six.bones.iter().map(|b| (sixng_core::meta7a::joaat(&b.name), b.name.clone())).collect();
            let all = groom::catalog(&o.game, &o.work, &ix)?;
            let fits = |b: &[[f32; 3]; 2]| head_box.is_none_or(|h| (0..3).all(|i| b[0][i] >= h[0][i] - 0.03 && b[1][i] <= h[1][i] + 0.03));
            let mut v: Vec<_> = all.iter().filter(|g| g.lod == 0 && fits(&g.root_box)).map(|g| (groom::category(&g.bones, &g.root_box, &names), g)).filter(|(c, _)| cat == "all" || c == cat).collect();
            v.sort_by_key(|(_, g)| std::cmp::Reverse(g.strands));
            println!("{} grooms of {} ({}) fit the head of {}{}:", v.len(), all.len(), cat, o.name, if head_box.is_none() { " (no head decoded: unfiltered)" } else { "" });
            for (c, g) in v.iter().take(60) {
                println!("  {}  {:<6} strands={:<5} roots z {:.2}..{:.2} y {:.2}..{:.2}", g.id, c, g.strands, g.root_box[0][2], g.root_box[1][2], g.root_box[0][1], g.root_box[1][1]);
            }
            println!("use: --hair <id> / --beard <id> (cards are a stand-in: nothing in the game files says which groom a ped wears)");
            return Ok(());
        }
        let head_bone = skel.bones.iter().position(|b| b.name.eq_ignore_ascii_case("SKEL_Head")).unwrap_or(0) as u8;
        let rgba = [o.hair_color[0], o.hair_color[1], o.hair_color[2], 255];
        for (id, slot) in [(&o.hair, 2usize), (&o.beard, 1usize)] {
            let Some(id) = id else { continue };
            let g = groom::load(&o.game, &ix, id)?;
            parts.retain(|p| p.slot != slot); // the 3-vertex placeholders, if any
            let mut geoms = Vec::new();
            let per = (60000 / (g.k * 4)).max(1);
            let mut s0 = 0;
            while s0 < g.strands {
                let s1 = (s0 + per).min(g.strands);
                let (pos, nrm, uv, idx) = g.ribbons(o.hair_width, s0..s1);
                s0 = s1;
                if idx.is_empty() {
                    continue;
                }
                let tangents = uv_tangents(&pos, &nrm, &uv, &idx);
                let verts = (0..pos.len()).map(|i| SkinVert { pos: pos[i], normal: nrm[i], uv: uv[i], tangent: tangents[i], weights: [255, 0, 0, 0], bones: [head_bone, 0, 0, 0] }).collect();
                geoms.push((SkinGeom { verts, indices: idx.iter().map(|&x| x as u16).collect(), shader: 0 }, false));
            }
            report.push(json!({"source": id, "slot": SLOTS[slot], "groom_strands": g.strands, "points_per_strand": g.k, "geometries": geoms.len()}));
            parts.push(Part { src: format!("groom {id}"), slot, geoms, diffuse: Some((rgba.repeat(16 * 16), 16, 16)), warnings: Vec::new() });
        }
    }
    if parts.is_empty() {
        return Err(format!("{}: nothing decoded: {}", o.name, serde_json::to_string(&report)?).into());
    }

    // GTA V names: <slot>_<nnn>_u, <slot>_diff_<nnn>_a_uni
    let ped = &o.ped;
    let mut per_slot = [0usize; 12];
    let (mut drawables, mut textures, mut comps) = (Vec::new(), Vec::new(), Vec::new());
    for p in &parts {
        let k = per_slot[p.slot];
        if k > 127 {
            continue;
        }
        per_slot[p.slot] += 1;
        let s = SLOTS[p.slot];
        let (dn, diff, nrm, spec) = (format!("{s}_{k:03}_u"), format!("{s}_diff_{k:03}_a_uni"), format!("{s}_normal_{k:03}"), format!("{s}_spec_{k:03}"));
        textures.push(match &p.diffuse {
            Some((rgba, w, h)) => make_tex(&diff, rgba, *w, *h, o.max_tex),
            None => flat_tex(&diff, [160, 160, 160, 255]),
        });
        textures.push(flat_tex(&nrm, [128, 128, 255, 255]));
        textures.push(flat_tex(&spec, [32, 32, 32, 255]));
        let cut = p.slot == 1 || p.slot == 2;
        let mut shaders: Vec<ShaderDef> = Vec::new();
        let mut geoms = Vec::new();
        for (g, alpha) in &p.geoms {
            let si = usize::from(*alpha && cut);
            while shaders.len() <= si {
                shaders.push(ShaderDef::ped(&diff, &nrm, &spec, shaders.len() == 1));
            }
            geoms.push(SkinGeom { shader: si, ..g.clone() });
        }
        drawables.push(SkinDrawable { name: dn, shaders, geoms });
        comps.push(meta::PedComponent { slot: p.slot as u8, drawable: k as u8 });
    }

    let stream = o.out.join("stream");
    fs::create_dir_all(&stream)?;
    let mut files = Vec::new();
    for (name, b) in [
        (format!("{ped}.yft"), gp::build_yft(ped, &skel)?),
        (format!("{ped}.ydd"), gp::build_ydd(&drawables, nbones)?),
        (format!("{ped}.ytd"), ytd::build_ytd(&textures)?),
        (format!("{ped}.ymt"), meta::build_ped_ymt(&comps)?),
    ] {
        super::write_atomic(&stream.join(&name), &b)?;
        files.push(name);
    }
    fs::write(o.out.join("peds.meta"), peds_meta(ped, female))?;
    fs::write(o.out.join("fxmanifest.lua"), "fx_version 'cerulean'\ngame 'gta5'\n\n-- generated by 6ixpack fivem ped\nfiles { 'peds.meta' }\ndata_file 'PED_METADATA_FILE' 'peds.meta'\n")?;
    let summary = json!({"ped": ped, "source": o.name, "female": female, "bones": nbones, "bones_from_6m": matched, "files": files, "drawables": drawables.iter().map(|d| &d.name).collect::<Vec<_>>(), "parts": report});
    fs::write(o.out.join("ped-report.json"), serde_json::to_vec_pretty(&summary)?)?;
    // live-test log: dropped bone influences and missing textures, counts plus examples
    let (mut dropped, mut drop_ex, mut missing) = (0usize, Vec::new(), Vec::new());
    for (d, p) in drawables.iter().zip(&parts) {
        for w in &p.warnings {
            if let Some((n, rest)) = w.split_once(" vertex influences") {
                dropped += n.trim().parse::<usize>().unwrap_or(0);
                drop_ex.push(format!("{}: {}{}", d.name, n, rest.split_once("): ").map(|x| format!(" ({})", x.1)).unwrap_or_default()));
            } else if let Some(h) = w.strip_prefix("texture ") {
                missing.push(json!({"drawable": d.name, "source": p.src, "texture": h.split(':').next().unwrap_or(h).trim(), "reason": h.split_once(": ").map(|x| x.1).unwrap_or("")}));
            }
        }
    }
    eprintln!("[t6m:peds] {ped}: {} drawables, {nbones} bones ({matched} from 6m)", drawables.len());
    eprintln!("[t6m:peds] {ped}: {dropped} dropped bone influences{}", if drop_ex.is_empty() { String::new() } else { format!(": {}", drop_ex.join("; ")) });
    eprintln!("[t6m:peds] {ped}: {} missing textures{}", missing.len(), missing.iter().take(8).map(|m| format!(" {}/{}", m["drawable"].as_str().unwrap_or(""), m["texture"].as_str().unwrap_or(""))).collect::<String>());
    let rd = o.out.join("t6m_report");
    fs::create_dir_all(&rd)?;
    fs::write(rd.join("peds.json"), serde_json::to_vec_pretty(&json!({"ped": ped, "dropped_influences": dropped, "dropped_examples": drop_ex, "missing_textures": missing, "summary": summary}))?)?;
    println!("fivem ped: {} -> {ped} ({} drawables, {} textures, {nbones} bones, {matched} placed from 6m) -> {}", o.name, drawables.len(), textures.len(), o.out.display());
    for (d, p) in drawables.iter().zip(&parts) {
        println!("  {:<10} {}", d.name, p.src);
    }
    Ok(())
}

/// peds.meta with one CPedModelInfo entry (ambient civilian defaults).
fn peds_meta(ped: &str, female: bool) -> String {
    // values all vanilla (DLC peds.meta, 435 items): personality
    // Streamed_*, voice SILENT_PVG, expression dict null.
    // pose matcher: every vanilla ped (female too) uses Male / Male_prone
    // (update.rpf peds.ymt); any other name takes a new PoseMatcherStore
    // slot and the full pool (28) crashes the data file mounter
    let (ptype, clip, expr, gest, face, visem, pose, capsule, perso) = if female {
        ("CIVFEMALE", "move_f@generic", "expr_set_ambient_female", "ANIM_GROUP_GESTURE_F_GENERIC", "facial_clipset_group_gen_female", "ANIM_GROUP_VISEMES_F_LO", "Male", "STANDARD_FEMALE", "Streamed_Female")
    } else {
        ("CIVMALE", "move_m@generic", "expr_set_ambient_male", "ANIM_GROUP_GESTURE_M_GENERIC", "facial_clipset_group_gen_male", "ANIM_GROUP_VISEMES_M_LO", "Male", "STANDARD_MALE", "Streamed_Male")
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<CPedModelInfo__InitDataList>
  <InitDatas>
    <Item>
      <Name>{ped}</Name>
      <PropsName>null</PropsName>
      <ClipDictionaryName>{clip}</ClipDictionaryName>
      <BlendShapeFileName>null</BlendShapeFileName>
      <ExpressionSetName>{expr}</ExpressionSetName>
      <ExpressionDictionaryName>null</ExpressionDictionaryName>
      <ExpressionName>null</ExpressionName>
      <Pedtype>{ptype}</Pedtype>
      <MovementClipSet>{clip}</MovementClipSet>
      <StrafeClipSet>move_ped_strafing</StrafeClipSet>
      <MovementToStrafeClipSet>move_ped_to_strafe</MovementToStrafeClipSet>
      <InjuredStrafeClipSet>move_strafe_injured</InjuredStrafeClipSet>
      <FullBodyDamageClipSet>dam_ko</FullBodyDamageClipSet>
      <AdditiveDamageClipSet>dam_ad</AdditiveDamageClipSet>
      <DefaultGestureClipSet>{gest}</DefaultGestureClipSet>
      <FacialClipsetGroupName>{face}</FacialClipsetGroupName>
      <DefaultVisemeClipSet>{visem}</DefaultVisemeClipSet>
      <SidestepClipSet>CLIP_SET_ID_INVALID</SidestepClipSet>
      <PoseMatcherName>{pose}</PoseMatcherName>
      <PoseMatcherProneName>{pose}_prone</PoseMatcherProneName>
      <GetupSetHash>NMBS_SLOW_GETUPS</GetupSetHash>
      <CreatureMetadataName>null</CreatureMetadataName>
      <DecisionMakerName>DEFAULT</DecisionMakerName>
      <MotionTaskDataSetName>STANDARD_PED</MotionTaskDataSetName>
      <DefaultTaskDataSetName>STANDARD_PED</DefaultTaskDataSetName>
      <PedCapsuleName>{capsule}</PedCapsuleName>
      <PedLayoutName />
      <PedComponentSetName />
      <PedComponentClothName />
      <PedIKSettingsName />
      <TaskDataName />
      <IsStreamedGfx value="false" />
      <AmbulanceShouldRespondTo value="true" />
      <CanRideBikeWithNoHelmet value="false" />
      <CanSpawnInCar value="true" />
      <IsHeadBlendPed value="false" />
      <bOnlyBulkyItemVariations value="false" />
      <RelationshipGroup>{ptype}</RelationshipGroup>
      <NavCapabilitiesName>STANDARD_PED</NavCapabilitiesName>
      <PerceptionInfo>DEFAULT_PERCEPTION</PerceptionInfo>
      <DefaultBrawlingStyle>BS_AI</DefaultBrawlingStyle>
      <DefaultUnarmedWeapon>WEAPON_UNARMED</DefaultUnarmedWeapon>
      <Personality>{perso}</Personality>
      <CombatInfo>DEFAULT</CombatInfo>
      <VfxInfoName>VFXPEDINFO_HUMAN_GENERIC</VfxInfoName>
      <AmbientClipsForFlee>FLEE</AmbientClipsForFlee>
      <Radio1>RADIO_GENRE_PUNK</Radio1>
      <Radio2>RADIO_GENRE_JAZZ</Radio2>
      <FUpOffset value="0.000000" />
      <RUpOffset value="0.000000" />
      <FFrontOffset value="0.000000" />
      <RFrontOffset value="0.147000" />
      <MinActivationImpulse value="20.000000" />
      <Stubble value="0.000000" />
      <HDDist value="3.000000" />
      <TargetingThreatModifier value="1.000000" />
      <KilledPerceptionRangeModifer value="-1.000000" />
      <Sexiness />
      <Age value="0" />
      <MaxPassengersInCar value="0" />
      <ExternallyDrivenDOFs />
      <PedVoiceGroup>SILENT_PVG</PedVoiceGroup>
      <AnimalAudioObject />
      <AbilityType>SAT_NONE</AbilityType>
      <ThermalBehaviour>TB_WARM</ThermalBehaviour>
      <SuperlodType>SLOD_HUMAN</SuperlodType>
      <ScenarioPopStreamingSlot>SCENARIO_POP_STREAMING_NORMAL</ScenarioPopStreamingSlot>
      <DefaultSpawningPreference>DSP_NORMAL</DefaultSpawningPreference>
      <DefaultRemoveRangeMultiplier value="1.000000" />
      <AllowCloseSpawning value="false" />
    </Item>
  </InitDatas>
  <txdRelationships />
  <multiTxdRelationships />
</CPedModelInfo__InitDataList>
"#
    )
}
