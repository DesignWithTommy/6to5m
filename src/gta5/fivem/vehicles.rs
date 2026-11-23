//! `6ixpack fivem vehicle <name>`: one GTA VI (6m) vehicle -> a streamable
//! FiveM vehicle resource.
//!
//! Sources (levels.rpf, via the sixng index): `<name>_mid` (LOD0 -> high,
//! LOD1 -> medium), `<name>` (LOD0 -> low) and `<name>_hi` (LOD0 -> the
//! `_hi.yft`). 6m vehicle meshes are skinned to a ~300-bone skeleton whose
//! names already follow the GTA V convention (chassis, wheel_lf,
//! door_dside_f, seat_dside_f, steeringwheel, ...). Each triangle is given
//! to its dominant bone and the parts are written as rigid bone-bound models
//! (GTA V vehicle style). The skeleton is pruned to at most 255 bones (model
//! bone binding is one byte): bones carrying geometry and their ancestors
//! first, then GTA-relevant names, then the rest.
//!
//! Output (<out> is the resource):
//!   <out>/stream/<model>.yft, <model>_hi.yft, <model>.ytd
//!   <out>/data/vehicles.meta, handling.meta, carvariations.meta, carcols.meta
//!   <out>/fxmanifest.lua
//!
//! With `sixng vehicles` data: the configuration's rim + tyre drawables
//! (scaled 1.625 x wheelScale, a fitted factor), wheelScale and the body
//! colour (6m paint slot -> solid texture); without it a generated tyre.
//! Physics: a box child per 6m fragment physics group (doors, bonnet, boot,
//! bumpers, windows ...; masses from the 6m colliders) on its bone, so GTA V
//! can open and detach parts by bone name; joint limits are GTA V's own.
//! Limits: physics is a box per child (chassis + 4
//! wheels), so doors, bonnet and boot are drawn on their bones but do not
//! open or break; plain default/alpha shaders (no paint colours, no dirt or
//! damage); handling/meta values are sedan defaults.

use super::geom::{aabb, push_geoms, V3};
use super::write_atomic;
use crate::gta5::{
    yft::{self, Bone, Child, Frag, Group, Model},
    ydr::Geom,
    ytd::{self, Tex, TexFormat},
};
use memmap2::Mmap;
use sixng_core::{drawable, resource::Resource, scan::Entry};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    error::Error,
    fs,
    path::{Path, PathBuf},
};

pub const USAGE: &str = "\
usage: 6ixpack fivem vehicle <name> --game <dir> --out <resource dir> [options]
  <name>               6m vehicle (levels.rpf fragment name, e.g. chino; uses <name>, <name>_mid, <name>_hi)
  --game <dir>         GTA VI install (levels.rpf, data/ttdb)
  --out <dir>          FiveM resource directory (stream/, data/, fxmanifest.lua)
  --work <dir>         sixng work dir with index/levels.json (default $SIXNG_WORK or ./work)
  --model <name>       GTA V model / spawn name (default r6m_<name>)
  --wheel-radius <m>   tyre radius (default 0.34)
  --mass <kg>          handling mass (default 1500)
  --max-tex <px>       drop top mips so textures are at most this size
  --vehicle-data <f>   vehicles.json from `6ixpack sixng vehicles --out <work>` (default <work>/vehicles/vehicles.json):
                       real wheelScale, rim + tyre models and body colour; without it a generated tyre and grey paint
  --paint <hex>        body colour as sRGB RRGGBB or AARRGGBB (as `sixng vehicles` lists it); overrides the configuration's
  --config <n>         which 6m spawn configuration to take rim, tyre and colour from (default 0)
  --list               list 6m vehicle fragments in the index and exit";

pub(super) const MISSING: &str = "r6m_missing";
const TYRE: &str = "r6m_tyre";
const PAINT: &str = "r6m_paint";
/// `diffusetex` on 6m vehicle shaders: an engine-default white that the
/// runtime paint colour tints (GTA VI format notes, vehicle paint).
const PAINT_SLOT: u32 = 0xc8e8_c282;
/// Packed material slot (roughness/metal/AO) and the synthesized-map id salt.
const PACKED_SLOT: u32 = 0x739e_3d0d;
const SYNTH: u32 = 0x5e17_0001;
const WHEELS: [&str; 4] = ["wheel_lf", "wheel_rf", "wheel_lr", "wheel_rr"];

/// Diffuse/colour parameter hashes in priority order (buildings.rs).
const COLOR_SLOTS: [u32; 16] = [
    3370697346, 756347250, 0x346394EA, 0x7D4F12E2, 3464205966, 3632507550, 851042057, 1024521143, 3853173992, 2464948076, 3389132183, 3547013225,
    234123336, 3831897388, 3489704073, 1380592926,
];

/// Bone name prefixes GTA V vehicles use (kept before other bones).
const GTA_BONES: [&str; 40] = [
    "chassis", "bodyshell", "wheel_", "door_", "seat_", "steer", "bonnet", "boot", "headlight", "taillight", "indicator", "brakelight", "reversinglight",
    "window", "windscreen", "exhaust", "engine", "petrolcap", "fuel_cap", "bumper_", "platelight", "interiorlight", "dashglow", "hub_", "suspension_",
    "transmission_", "misc_", "extra_", "neon_", "siren", "spoiler", "wing_", "plate_", "mirror", "handle_", "light_", "aerial", "roof", "gear", "overheat",
];

struct Opts {
    name: String,
    game: PathBuf,
    out: PathBuf,
    work: PathBuf,
    model: String,
    wheel_r: f32,
    mass: f32,
    max_tex: Option<u16>,
    vdata: Option<PathBuf>,
    config: usize,
    paint: Option<[u8; 3]>,
    list: bool,
}

fn parse(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts {
        name: String::new(),
        game: PathBuf::new(),
        out: PathBuf::new(),
        work: std::env::var_os("SIXNG_WORK").map(PathBuf::from).unwrap_or_else(|| "work".into()),
        model: String::new(),
        wheel_r: 0.34,
        mass: 1500.0,
        max_tex: None,
        vdata: None,
        config: 0,
        paint: None,
        list: false,
    };
    let mut it = args.iter();
    let val = |it: &mut std::slice::Iter<String>, k: &str| it.next().cloned().ok_or(format!("{k} needs a value"));
    while let Some(a) = it.next() {
        match a.as_str() {
            "--game" => o.game = val(&mut it, a)?.into(),
            "--out" => o.out = val(&mut it, a)?.into(),
            "--work" => o.work = val(&mut it, a)?.into(),
            "--model" => o.model = val(&mut it, a)?.to_lowercase(),
            "--wheel-radius" => o.wheel_r = val(&mut it, a)?.parse().map_err(|e| format!("{a}: {e}"))?,
            "--mass" => o.mass = val(&mut it, a)?.parse().map_err(|e| format!("{a}: {e}"))?,
            "--max-tex" => o.max_tex = Some(val(&mut it, a)?.parse().map_err(|e| format!("{a}: {e}"))?),
            "--vehicle-data" => o.vdata = Some(val(&mut it, a)?.into()),
            "--paint" => o.paint = Some(parse_paint(&val(&mut it, a)?).ok_or(format!("{a} wants RRGGBB or AARRGGBB hex"))?),
            "--config" => o.config = val(&mut it, a)?.parse().map_err(|e| format!("{a}: {e}"))?,
            "--list" => o.list = true,
            x if !x.starts_with("--") && o.name.is_empty() => o.name = x.to_lowercase(),
            _ => return Err(format!("unknown argument {a}\n{USAGE}")),
        }
    }
    if o.game.as_os_str().is_empty() || (!o.list && (o.name.is_empty() || o.out.as_os_str().is_empty())) {
        return Err(USAGE.into());
    }
    if o.model.is_empty() {
        o.model = format!("r6m_{}", o.name);
    }
    if o.model.len() > 23 || !o.model.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') {
        return Err(format!("--model {}: use at most 23 chars of a-z, 0-9, _", o.model));
    }
    Ok(o)
}

// ------------------------------------------------------------- textures

/// Keyed textures from the ttdb pools / TTPF stores, cached as DDS under
/// <work>/fivem_tex_cache_v{N}/<hash>.dds (same cache and checks as buildings).
pub(super) struct Pools {
    game: PathBuf,
    catalog: Option<crate::texture::Catalog>,
    maps: HashMap<PathBuf, Mmap>,
    pub(super) dir: PathBuf,
}

impl Pools {
    /// Pools over <game>/data/ttdb + TTPF stores, cache under <work>.
    pub(super) fn new(game: &Path, work: &Path) -> Result<Pools, String> {
        let dir = work.join(format!("fivem_tex_cache_v{}i", sixng_core::bcpack::BCPACK_DECODER_VERSION));
        fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let ttdb = game.join("data").join("ttdb");
        Ok(Pools { game: game.to_path_buf(), catalog: crate::texture::Catalog::build(&ttdb).map_err(|e| eprintln!("{}: {e}", ttdb.display())).ok(), maps: HashMap::new(), dir })
    }
    fn path(&self, hash: u32) -> PathBuf {
        self.dir.join(format!("{hash:08x}.dds"))
    }
    /// Cache slot of a texture (fivem/texslot.rs): its 32-bit hash unless
    /// another texture owns that file.
    fn slot(&self, suffix: &str, hash: u32, key: u64, h64: u64) -> u32 {
        super::texslot::slot(&self.dir, suffix, hash, key, h64, &|| self.dims(key, h64))
    }

    /// Stored size of a texture: TTPF by 64-bit hash, else its pool entry.
    fn dims(&self, key: u64, h64: u64) -> Option<(u32, u32)> {
        if h64 != 0 {
            if let Some((_, t)) = sixng_core::ttpf::global(&self.game).get(h64) {
                return Some((t.width, t.height));
            }
        }
        let cat = self.catalog.as_ref()?;
        let loc = cat.find(key).iter().find(|l| l.extractable)?;
        let f = fs::File::open(&loc.path).ok()?;
        // SAFETY: read-only map of a game file.
        let map = unsafe { Mmap::map(&f) }.ok()?;
        let pool = crate::texture::Pool::open(&map).ok()?;
        let t = pool.textures.get(loc.index)?;
        Some((t.width, t.height))
    }

    fn get(&mut self, hash: u32, key: u64, h64: u64) -> Result<PathBuf, String> {
        let hash = self.slot("", hash, key, h64);
        let e1 = match self.from_pools(hash, key).and_then(|p| check_dds(&p).map(|_| p)) {
            Ok(p) => return Ok(p),
            Err(e) => e,
        };
        self.from_ttpf(hash, h64).and_then(|p| check_dds(&p).map(|_| p)).map_err(|e2| format!("{e1}; ttpf: {e2}"))
    }
    fn from_ttpf(&self, hash: u32, h64: u64) -> Result<PathBuf, String> {
        if h64 == 0 {
            return Err("no 64-bit hash".into());
        }
        let set = sixng_core::ttpf::global(&self.game);
        let (store, t) = set.get(h64).ok_or("not in any ttpf store")?;
        if !t.decodable() {
            return Err(format!("format {} / tile mode {} not decodable", t.dxgi_format, t.tile_mode()));
        }
        let (data, skipped) = store.data_ex(t).map_err(|e| e.to_string())?;
        let mip = if skipped == 0 { 0 } else { store.best_mip(t).ok_or("only BCPack mips")? };
        let (rgba, w, h) = sixng_core::ttpf::decode_rgba(t, &data, mip).map_err(|e| e.to_string())?;
        if let Some(why) = bad_colour(&rgba) {
            return Err(why.into());
        }
        let dds = rgba_dds(&rgba, w, h);
        ytd::from_dds("check", &dds)?;
        let p = self.path(hash);
        write_atomic(&p, &dds).map_err(|e| e.to_string())?;
        Ok(p)
    }
    /// Decoded RGBA of any texture (no colour-map check): pools, else TTPF.
    fn raw(&mut self, _hash: u32, key: u64, h64: u64) -> Result<(Vec<u8>, u32, u32), String> {
        let pooled = (|| -> Result<Vec<u8>, String> {
            let cat = self.catalog.as_ref().ok_or("no pool catalog")?;
            let loc = cat.find(key).iter().find(|l| l.extractable).ok_or("not in pools")?;
            let (lp, li) = (loc.path.clone(), loc.index);
            if !self.maps.contains_key(&lp) {
                let f = fs::File::open(&lp).map_err(|e| e.to_string())?;
                // SAFETY: read-only map of a game file.
                self.maps.insert(lp.clone(), unsafe { Mmap::map(&f) }.map_err(|e| e.to_string())?);
            }
            let pool = crate::texture::Pool::open(&self.maps[&lp]).map_err(|e| e.to_string())?;
            let t = pool.textures.get(li).ok_or("pool index out of range")?;
            pool.create_dds(t).map_err(|e| e.to_string())
        })();
        if let Ok(dds) = pooled.as_ref().map_err(|_| ()).and_then(|d| crate::texture::dds_preview(d).map_err(|_| ())) {
            return Ok((dds.2, dds.0, dds.1));
        }
        let set = sixng_core::ttpf::global(&self.game);
        let (store, t) = set.get(h64).ok_or("not in pools or any ttpf store")?;
        let (data, skipped) = store.data_ex(t).map_err(|e| e.to_string())?;
        let mip = if skipped == 0 { 0 } else { store.best_mip(t).ok_or("only BCPack mips")? };
        let (rgba, w, h) = sixng_core::ttpf::decode_rgba(t, &data, mip).map_err(|e| e.to_string())?;
        Ok((rgba, w, h))
    }

    /// Material shaders with no colour map (6m weapons, e68a4a98): a
    /// gunmetal colour map shaded by the packed material texture (slot
    /// 739e3d0d, R/G/B likely roughness/metal/AO; the darkest-mean channel
    /// is used as occlusion). Cached as `<packed ^ SYNTH>.dds`.
    fn synth(&mut self, packed: u32, key: u64, h64: u64) -> Result<u32, String> {
        let id = packed ^ SYNTH;
        if self.path(id).exists() {
            return Ok(id);
        }
        let (rgba, w, h) = self.raw(packed, key, h64)?;
        let n = (rgba.len() / 4).max(1) as f64;
        let mean = |c: usize| rgba.chunks_exact(4).map(|p| p[c] as f64).sum::<f64>() / n;
        // R-only maps with small values are material ids (0, 1, 2 ...): one
        // weapon finish per id. Otherwise shade gunmetal by the brightest
        // channel as occlusion.
        const FINISH: [[u8; 3]; 6] = [[0x2c, 0x2d, 0x2f], [0x55, 0x57, 0x5b], [0x3a, 0x38, 0x34], [0x6a, 0x6c, 0x70], [0x1f, 0x20, 0x21], [0x4a, 0x42, 0x36]];
        let ids = mean(1) < 4.0 && mean(2) < 4.0;
        let ao = (0..3).max_by(|&a, &b| mean(a).total_cmp(&mean(b))).unwrap_or(2);
        let out: Vec<u8> = rgba
            .chunks_exact(4)
            .flat_map(|p| {
                if ids {
                    let [r, g, b] = FINISH[if p[0] > 64 { p[0] as usize / 64 } else { p[0] as usize } % FINISH.len()];
                    [r, g, b, 255]
                } else {
                    let k = 0.35 + 0.65 * p[ao] as f32 / 255.0;
                    [(0x4a as f32 * k) as u8, (0x4c as f32 * k) as u8, (0x50 as f32 * k) as u8, 255]
                }
            })
            .collect();
        let dds = rgba_dds(&out, w, h);
        write_atomic(&self.path(id), &dds).map_err(|e| e.to_string())?;
        Ok(id)
    }

    fn from_pools(&mut self, hash: u32, key: u64) -> Result<PathBuf, String> {
        let p = self.path(hash);
        if p.exists() {
            return Ok(p);
        }
        if key == 0 {
            return Err("no asset key".into());
        }
        let cat = self.catalog.as_ref().ok_or("no pool catalog")?;
        let loc = cat.find(key).iter().find(|l| l.extractable).ok_or("not in pools")?;
        let (lp, li) = (loc.path.clone(), loc.index);
        if !self.maps.contains_key(&lp) {
            let f = fs::File::open(&lp).map_err(|e| e.to_string())?;
            // SAFETY: read-only map of a game file.
            self.maps.insert(lp.clone(), unsafe { Mmap::map(&f) }.map_err(|e| e.to_string())?);
        }
        let pool = crate::texture::Pool::open(&self.maps[&lp]).map_err(|e| e.to_string())?;
        let t = pool.textures.get(li).ok_or("pool index out of range")?;
        let dds = pool.create_dds(t).map_err(|e| e.to_string())?;
        ytd::from_dds("check", &dds)?;
        write_atomic(&p, &dds).map_err(|e| e.to_string())?;
        Ok(p)
    }
}

fn check_dds(p: &Path) -> Result<(), String> {
    let bytes = fs::read(p).map_err(|e| e.to_string())?;
    let (_, _, rgba) = crate::texture::dds_preview(&bytes).map_err(|e| e.to_string())?;
    match bad_colour(&rgba) {
        None => Ok(()),
        Some(why) => {
            let _ = fs::remove_file(p);
            Err(why.into())
        }
    }
}

/// Flat decodes, normal maps and packed masks are not colour maps.
fn bad_colour(rgba: &[u8]) -> Option<&'static str> {
    let n = (rgba.len() / 4).max(1) as f64;
    let (mut s, mut q) = ([0f64; 3], [0f64; 3]);
    for px in rgba.chunks_exact(4) {
        for k in 0..3 {
            let v = px[k] as f64;
            s[k] += v;
            q[k] += v * v;
        }
    }
    let m = s.map(|x| x / n);
    let var: f64 = (0..3).map(|k| q[k] / n - m[k] * m[k]).sum();
    if var < 12.0 {
        return Some("flat decode (undecoded BCPack data)");
    }
    if m[2] > 200.0 && (100.0..160.0).contains(&m[0]) && (100.0..160.0).contains(&m[1]) {
        return Some("normal map, not a colour map");
    }
    let corr = |a: usize, b: usize| {
        let (mut sab, mut saa, mut sbb) = (0f64, 0f64, 0f64);
        for px in rgba.chunks_exact(4) {
            let (x, y) = (px[a] as f64 - m[a], px[b] as f64 - m[b]);
            sab += x * y;
            saa += x * x;
            sbb += y * y;
        }
        if saa < 1.0 || sbb < 1.0 { 1.0 } else { sab / (saa * sbb).sqrt() }
    };
    if corr(0, 1).min(corr(1, 2)).min(corr(0, 2)) < 0.2 {
        return Some("mask/packed data, not a colour map");
    }
    None
}

/// DXT1/DXT5 DDS with a box-filtered mip chain (texpresso).
fn rgba_dds(rgba: &[u8], w: u32, h: u32) -> Vec<u8> {
    let mut levels = vec![(rgba.to_vec(), w, h)];
    while levels.len() < 12 {
        let (src, sw, sh) = levels.last().unwrap();
        let (sw, sh) = (*sw, *sh);
        if sw < 8 || sh < 8 {
            break;
        }
        let (nw, nh) = (sw / 2, sh / 2);
        let mut d = vec![0u8; (nw * nh * 4) as usize];
        for y in 0..nh {
            for x in 0..nw {
                for c in 0..4 {
                    let at = |xx: u32, yy: u32| src[((yy * sw + xx) * 4 + c) as usize] as u32;
                    d[((y * nw + x) * 4 + c) as usize] = ((at(2 * x, 2 * y) + at(2 * x + 1, 2 * y) + at(2 * x, 2 * y + 1) + at(2 * x + 1, 2 * y + 1) + 2) / 4) as u8;
                }
            }
        }
        levels.push((d, nw, nh));
    }
    let opaque = rgba.chunks_exact(4).all(|p| p[3] == 255);
    let (fmt, cc) = if opaque { (texpresso::Format::Bc1, *b"DXT1") } else { (texpresso::Format::Bc3, *b"DXT5") };
    let u = |v: u32, b: &mut Vec<u8>| b.extend_from_slice(&v.to_le_bytes());
    let mut b = Vec::new();
    b.extend_from_slice(b"DDS ");
    u(124, &mut b);
    u(0x1 | 0x2 | 0x4 | 0x1000 | 0x20000 | 0x80000, &mut b);
    u(h, &mut b);
    u(w, &mut b);
    u(fmt.compressed_size(w as usize, h as usize) as u32, &mut b);
    u(0, &mut b);
    u(levels.len() as u32, &mut b);
    for _ in 0..11 {
        u(0, &mut b);
    }
    u(32, &mut b);
    u(0x4, &mut b);
    b.extend_from_slice(&cc);
    for _ in 0..5 {
        u(0, &mut b);
    }
    u(0x1000 | 0x400000 | 0x8, &mut b);
    for _ in 0..4 {
        u(0, &mut b);
    }
    for (px, lw, lh) in &levels {
        let mut out = vec![0u8; fmt.compressed_size(*lw as usize, *lh as usize)];
        fmt.compress(px, *lw as usize, *lh as usize, texpresso::Params::default(), &mut out);
        b.extend_from_slice(&out);
    }
    b
}

/// Solid-colour A8R8G8B8 texture with a full mip chain.
pub(super) fn solid(name: &str, bgra: [u8; 4], size: usize) -> Tex {
    let mut data = Vec::new();
    let (mut w, mut levels) = (size, 0u8);
    while w >= 1 {
        for _ in 0..w * w {
            data.extend_from_slice(&bgra);
        }
        levels += 1;
        w /= 2;
    }
    Tex { name: name.into(), width: size as u16, height: size as u16, levels, format: TexFormat::A8R8G8B8, data, unk32: 0, usage_data: 0, extra_flags: 0 }
}

fn kind_of(bucket: u8) -> u8 {
    match bucket {
        x if x & 0x04 != 0 => 2,
        x if x & 0x42 != 0 => 1,
        x if x & 0x08 != 0 => 3,
        _ => 0,
    }
}

// --------------------------------------------------------------- source

/// One decoded source fragment: skeleton, and per LOD a list of
/// (6m bone index, model-space geometry) parts.
pub(super) struct Source {
    pub(super) bones: Vec<drawable::Bone>,
    pub(super) lods: Vec<Vec<(usize, Geom)>>,
    groups: Vec<PGroup>,
}

/// One 6m fragment physics group (GTA6-Asset-Info 13.4): first bone name,
/// parent group, summed collider mass.
#[derive(Clone, Debug)]
struct PGroup {
    name: String,
    parent: Option<usize>,
    mass: f32,
}

/// Physics groups: root +0x20 groups (stride 0x70; bone hash +0x08, parent
/// u16 +0x16, collider range +0x1A..+0x1C), +0x28 colliders (stride 0x50;
/// mass f32 +0x2C), counts u16 at +0x34 / +0x36.
fn phys_groups(r: &Resource, bones: &[drawable::Bone]) -> Vec<PGroup> {
    let u16at = |o: usize| r.sys.get(o..o + 2).map_or(0xFFFF, |b| u16::from_le_bytes([b[0], b[1]]));
    let f32at = |o: usize| r.sys.get(o..o + 4).map_or(0.0, |b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let (Some(gp), Some(cp)) = (r.sys_off(r.u64(0x20)), r.sys_off(r.u64(0x28))) else { return Vec::new() };
    let (ng, nc) = (u16at(0x34) as usize, u16at(0x36) as usize);
    if gp + ng * 0x70 > r.sys.len() || cp + nc * 0x50 > r.sys.len() {
        return Vec::new();
    }
    let by_hash: HashMap<u32, &str> = bones.iter().map(|b| (crate::gta5::rsc::joaat(&b.name), b.name.as_str())).collect();
    (0..ng)
        .map(|g| {
            let o = gp + g * 0x70;
            let h = r.u32(o + 8);
            let (c0, c1) = (u16at(o + 0x1a) as usize, (u16at(o + 0x1c) as usize).min(nc));
            let parent = Some(u16at(o + 0x16) as usize).filter(|&p| p < ng);
            PGroup { name: by_hash.get(&h).map_or(format!("{h:08x}"), |n| n.to_string()), parent, mass: (c0..c1).map(|c| f32at(cp + c * 0x50 + 0x2c)).sum() }
        })
        .collect()
}

pub(super) fn load_source(rpf: &Path, e: &Entry, pools: &mut Pools, texs: &mut Vec<String>, missing: &mut Vec<String>) -> Result<Source, String> {
    let r = Resource::load(rpf, e).map_err(|x| x.to_string())?;
    let info = drawable::describe(&r);
    let slice = |i: usize| {
        let b = &info.buffers[i];
        let end = (b.gfx_off + b.bytes).min(r.gfx.len() as u64) as usize;
        &r.gfx[(b.gfx_off as usize).min(end)..end]
    };
    let key_of = |sh: &drawable::Shader, param: usize| -> u64 {
        (|| {
            let ta = r.sys_off(r.u64(sh.record_off as usize + 0x10))?;
            let t = r.sys_off(r.u64(ta + 8 * param))?;
            let tr = r.sys_off(r.u64(t + 0x30))?;
            Some(r.u64(tr + 0x28))
        })()
        .unwrap_or(0)
    };
    let dr = r.sys_off(r.u64(0x18)).unwrap_or(0);
    let emb = [dr, 0].into_iter().find_map(|d| crate::texture::embedded_dictionary(&r.sys, &r.gfx, d));
    let nb = info.skeleton.len();
    let maxlod = info.models.iter().filter(|m| !m.part).map(|m| m.lod).max().unwrap_or(0);
    let mut lods: Vec<Vec<(usize, Geom)>> = vec![Vec::new(); maxlod + 1];
    let mut tex_cache: HashMap<usize, String> = HashMap::new();
    for md in info.models.iter().filter(|m| !m.part) {
        for (k, &gi) in md.geometries.iter().enumerate() {
            let g = &info.geometries[gi];
            let Some(h) = info.buffers[g.vertex_buffer].mesh_blob.as_ref() else { continue };
            let m = drawable::decode_mesh(slice(g.vertex_buffer), h, slice(g.index_buffer));
            if m.indices.is_empty() {
                continue;
            }
            let si = md.shader_indices.get(k).map(|&s| s as usize);
            let sh = si.and_then(|s| info.shaders.get(s));
            let tex = match si.and_then(|s| tex_cache.get(&s).cloned()) {
                Some(t) => t,
                None => {
                    let t = sh.map_or_else(
                        || MISSING.to_string(),
                        |sh| {
                            let ph = |t: &&drawable::TexRef| u32::from_str_radix(&t.param_hash, 16).ok();
                            let mut cands: Vec<_> = COLOR_SLOTS.iter().filter_map(|slot| sh.textures.iter().find(|t| ph(t) == Some(*slot))).collect();
                            cands.extend(sh.textures.iter().filter(|t| !ph(t).is_some_and(|h| COLOR_SLOTS.contains(&h))));
                            let mut errs = Vec::new();
                            for t in cands {
                                let hash = u32::from_str_radix(&t.hash32, 16).unwrap_or(0);
                                let key = emb.as_ref().and_then(|e| e.by_hash(hash)).and_then(|x| x.asset_key).unwrap_or_else(|| key_of(sh, t.param));
                                match pools.get(hash, key, u64::from_str_radix(&t.hash64, 16).unwrap_or(0)) {
                                    Ok(p) => return format!("r6m_{}", p.file_stem().map_or(String::new(), |s| s.to_string_lossy().into_owned())),
                                    // the paint slot's white is in no store: the body colour
                                    Err(_) if ph(&t) == Some(PAINT_SLOT) || sixng_core::material::is_paint(&sh.name_hash) => return PAINT.to_string(),
                                    Err(e) => errs.push(format!("{hash:08x}: {e}")),
                                }
                            }
                            if let Some(t) = sh.textures.iter().find(|t| ph(t) == Some(PACKED_SLOT)) {
                                let hash = u32::from_str_radix(&t.hash32, 16).unwrap_or(0);
                                let key = emb.as_ref().and_then(|e| e.by_hash(hash)).and_then(|x| x.asset_key).unwrap_or_else(|| key_of(sh, t.param));
                                match pools.synth(hash, key, u64::from_str_radix(&t.hash64, 16).unwrap_or(0)) {
                                    Ok(id) => return format!("r6m_{id:08x}"),
                                    Err(e) => errs.push(format!("{hash:08x} (packed): {e}")),
                                }
                            }
                            missing.extend(errs.into_iter().take(1));
                            MISSING.to_string()
                        },
                    );
                    if let Some(s) = si {
                        tex_cache.insert(s, t.clone());
                    }
                    t
                }
            };
            if !texs.contains(&tex) {
                texs.push(tex.clone());
            }
            let kind = sh.map_or(0, |sh| if is_emissive(sh) { yft::EMISSIVE } else { kind_of(sh.flags.get(4).copied().unwrap_or(0)) });
            let uv = m.uvs.iter().find(|u| u.0 == 3).or(m.uvs.first()).map(|u| u.1.as_slice());
            let col: Option<Vec<u32>> = m.colors.as_ref().map(|c| c.iter().map(|x| u32::from_le_bytes(*x)).collect());
            // dominant joint per vertex (joints index the drawable skeleton)
            let nv = m.positions.len();
            let dom: Vec<usize> = (0..nv)
                .map(|v| {
                    let (Some(j), w) = (m.joints.as_ref().and_then(|j| j.get(v)), m.weights.as_ref().and_then(|w| w.get(v))) else { return 0 };
                    let k = w.map_or(0, |w| (0..4).max_by_key(|&k| w[k]).unwrap_or(0));
                    let b = j[k] as usize;
                    if b < nb { b } else { 0 }
                })
                .collect();
            let mut by_bone: BTreeMap<usize, Vec<u32>> = BTreeMap::new();
            for t in m.indices.chunks_exact(3) {
                if t.iter().any(|&i| i as usize >= nv) {
                    continue;
                }
                let d = [dom[t[0] as usize], dom[t[1] as usize], dom[t[2] as usize]];
                let b = if d[1] == d[2] { d[1] } else { d[0] };
                by_bone.entry(b).or_default().extend_from_slice(t);
            }
            for (b, idx) in by_bone {
                let mut out = Vec::new();
                push_geoms(&mut out, &m.positions, &idx, uv, col.as_deref(), &tex, kind);
                lods[md.lod].extend(out.into_iter().map(|g| (b, g)));
            }
        }
    }
    let groups = phys_groups(&r, &info.skeleton);
    Ok(Source { bones: info.skeleton, lods, groups })
}

// ---------------------------------------------------------------- build

pub fn run(args: &[String]) -> Result<(), Box<dyn Error>> {
    if args.is_empty() || args[0] == "--help" {
        println!("{USAGE}");
        return Ok(());
    }
    let o = parse(args)?;
    let t0 = std::time::Instant::now();
    let ip = o.work.join("index").join("levels.json");
    let index = sixng_core::scan::load_index(&ip).map_err(|e| format!("{}: {e:#} (run `6ixpack sixng index levels.rpf --work ...` first)", ip.display()))?;
    let by_name: HashMap<&str, &Entry> = index.entries.iter().filter(|e| e.rtype.as_deref() == Some("ft")).filter_map(|e| Some((e.name.as_deref()?, e))).collect();
    let drawables: HashMap<&str, &Entry> = index.entries.iter().filter(|e| e.rtype.as_deref() == Some("dr")).filter_map(|e| Some((e.name.as_deref()?, e))).collect();
    if o.list {
        return list(&o, &index.entries);
    }
    let src = |s: &str| by_name.get(format!("{}{s}", o.name).as_str()).copied();
    let (e_base, e_mid, e_hi) = (src(""), src("_mid"), src("_hi"));
    if e_base.is_none() && e_mid.is_none() && e_hi.is_none() {
        return Err(format!("no fragment named {0}, {0}_mid or {0}_hi in {1} (try --list)", o.name, ip.display()).into());
    }
    let cache = o.work.join(format!("fivem_tex_cache_v{}i", sixng_core::bcpack::BCPACK_DECODER_VERSION));
    fs::create_dir_all(&cache)?;
    let ttdb = o.game.join("data").join("ttdb");
    let mut pools = Pools { game: o.game.clone(), catalog: crate::texture::Catalog::build(&ttdb).map_err(|e| eprintln!("{}: {e}", ttdb.display())).ok(), maps: HashMap::new(), dir: cache.clone() };
    let (mut texs, mut missing) = (Vec::new(), Vec::new());
    let mut load = |e: Option<&Entry>| -> Result<Option<Source>, String> {
        e.map(|e| {
            eprintln!("vehicle: decoding {} ({})", e.name.as_deref().unwrap_or("?"), e.id);
            load_source(&o.game.join("levels.rpf"), e, &mut pools, &mut texs, &mut missing)
        })
        .transpose()
    };
    let s_mid = load(e_mid)?;
    let s_base = load(e_base)?;
    let s_hi = load(e_hi)?;
    let vd = vehicle_data(&o);
    // real rim + tyre drawables, vertices in wheel space scaled to the car
    let mut wheel_geoms = |front: bool| -> Result<(Vec<Geom>, f32), String> {
        let Some(vd) = &vd else { return Ok((Vec::new(), 0.0)) };
        let s = 1.625 * if front { vd.wheel_scale } else { vd.wheel_scale_rear };
        let (mut gs, mut radius) = (Vec::new(), 0.0f32);
        for part in [&vd.tyre, &vd.rim].into_iter().flatten() {
            let Some(e) = drawables.get(format!("{part}_mid").as_str()).or(drawables.get(part.as_str())).copied() else {
                eprintln!("vehicle: wheel part {part} not in the index");
                continue;
            };
            let src = load(Some(e))?.unwrap();
            for (_, mut g) in src.lods.into_iter().next().unwrap_or_default() {
                for p in g.positions.iter_mut() {
                    *p = [p[0] * s, p[1] * s, p[2] * s];
                    radius = radius.max((p[1] * p[1] + p[2] * p[2]).sqrt());
                }
                gs.push(g);
            }
        }
        Ok((gs, radius))
    };
    let (wf, wr) = (wheel_geoms(true)?, wheel_geoms(false)?);

    // shared skeleton: from the richest source
    let skel = [&s_hi, &s_mid, &s_base].into_iter().flatten().map(|s| &s.bones).max_by_key(|b| b.len()).ok_or("no source")?;
    if skel.is_empty() {
        return Err("6m fragment has no skeleton (not a vehicle?)".into());
    }
    let same = |s: &Option<Source>| s.as_ref().is_none_or(|s| s.bones.len() == skel.len() && s.bones.iter().zip(skel).all(|(a, b)| a.name == b.name));
    if !(same(&s_mid) && same(&s_base) && same(&s_hi)) {
        return Err("sources have different skeletons".into());
    }

    // LOD plan: (yft lod slot, source, source lod)
    let mut main_plan: Vec<(usize, &Source, usize)> = Vec::new();
    if let Some(s) = &s_mid {
        main_plan.push((0, s, 0));
        if s.lods.len() > 1 {
            main_plan.push((1, s, 1));
        }
    }
    if let Some(s) = &s_base {
        let slot = if main_plan.is_empty() { 0 } else if main_plan.len() == 1 { 1 } else { 2 };
        main_plan.push((slot, s, 0));
    }
    if main_plan.is_empty() {
        main_plan.push((0, s_hi.as_ref().unwrap(), 0));
    }

    // bone pruning
    let used: HashSet<usize> = [&s_hi, &s_mid, &s_base].into_iter().flatten().flat_map(|s| s.lods.iter().flatten().map(|(b, _)| *b)).collect();
    let keep = prune(skel, &used);
    let remap: HashMap<usize, usize> = keep.iter().enumerate().map(|(n, &o)| (o, n)).collect();
    let bones: Vec<Bone> = keep
        .iter()
        .map(|&i| {
            let b = &skel[i];
            Bone { name: b.name.clone(), parent: b.parent.and_then(|p| remap.get(&(p as usize)).copied()), t: b.translation, r: b.rotation }
        })
        .collect();
    let g = yft::globals(&bones);
    let to_models = |parts: &[(usize, Geom)]| -> Vec<Model> {
        let mut by: BTreeMap<usize, Vec<Geom>> = BTreeMap::new();
        for (b, geom) in parts {
            let nb = remap[b];
            let mut geom = geom.clone();
            let q = g[nb].1;
            let qi = [-q[0], -q[1], -q[2], q[3]];
            for p in geom.positions.iter_mut() {
                *p = yft::to_local(g[nb], *p);
            }
            for n in geom.normals.iter_mut() {
                *n = yft::qrot(qi, *n);
            }
            by.entry(nb).or_default().push(geom);
        }
        by.into_iter().map(|(bone, geoms)| Model { bone, geoms }).collect()
    };

    // physics: chassis, wheels, then the 6m physics groups
    let pgroups: Vec<PGroup> = [&s_mid, &s_base, &s_hi].into_iter().flatten().map(|s| s.groups.clone()).find(|g| !g.is_empty()).unwrap_or_default();
    let high = to_models(&main_plan[0].1.lods[main_plan[0].2]);
    let chassis_parts: Vec<V3> = main_plan[0].1.lods[main_plan[0].2].iter().flat_map(|(b, gm)| {
        let wheelish = skel[*b].name.contains("wheel");
        gm.positions.iter().copied().filter(move |_| !wheelish)
    }).collect();
    let (cmin, cmax) = aabb(chassis_parts.iter());
    let mut groups = vec![Group { name: "chassis".into(), parent: None }];
    let mut children = vec![Child { bone: 0, group: 0, min: cmin, max: cmax, mass: o.mass, drawable: Vec::new() }];
    let generated = |r: f32| vec![yft::tyre(r, r * 0.65, TYRE, 24)];
    let mut wheels = Vec::new();
    for w in WHEELS {
        if let Some(bi) = bones.iter().position(|b| b.name == w) {
            groups.push(Group { name: w.into(), parent: Some(0) });
            let (gs, r) = if w.ends_with('f') { &wf } else { &wr };
            let (drawable, r) = if gs.is_empty() { (generated(o.wheel_r), o.wheel_r) } else { (gs.clone(), *r) };
            let wd = r * 0.65;
            let mass = pgroups.iter().find(|g| g.name == w).map_or(20.0, |g| g.mass.max(1.0));
            children.push(Child { bone: bi, group: groups.len() - 1, min: [-wd / 2.0, -r, -r], max: [wd / 2.0, r, r], mass, drawable });
            wheels.push(w);
        }
    }
    // the exterior 6m physics groups (doors, bonnet, boot, bumpers, windows ...):
    // one box child per group on its bone, so GTA V can open / detach them by
    // bone name. Box = the part's own geometry in bone space; mass from the 6m
    // colliders. Other groups (dummies, droop pivots, internals) are folded
    // into their nearest kept parent.
    // exterior parts GTA V cars articulate or detach; internals (seats,
    // engine, axles, brake discs) stay in the chassis so they never fall off
    const PARTS: [&str; 11] = ["bonnet", "boot", "bumper_", "door_dside", "door_pside", "window", "windscreen", "wing_", "mirror_", "plate_", "spoiler"];
    let skip = |n: &str| !PARTS.iter().any(|p| n.starts_with(p));
    let mut gmap: HashMap<usize, usize> = HashMap::new(); // 6m group -> yft group
    gmap.insert(0, 0);
    for (k, pg) in pgroups.iter().enumerate() {
        if k == 0 || skip(&pg.name) {
            continue;
        }
        let Some(bi) = bones.iter().position(|b| b.name == pg.name) else { continue };
        // nearest kept ancestor group
        let mut p = pg.parent;
        let parent = loop {
            match p {
                Some(q) if gmap.contains_key(&q) => break gmap[&q],
                Some(q) => p = pgroups[q].parent,
                None => break 0,
            }
        };
        let pts: Vec<V3> = high.iter().filter(|m| m.bone == bi).flat_map(|m| m.geoms.iter().flat_map(|g| g.positions.iter().copied())).collect();
        let (mn, mx) = if pts.is_empty() { ([-0.05; 3], [0.05; 3]) } else { aabb(pts.iter()) };
        groups.push(Group { name: pg.name.clone(), parent: Some(parent) });
        gmap.insert(k, groups.len() - 1);
        children.push(Child { bone: bi, group: groups.len() - 1, min: mn, max: mx, mass: pg.mass.clamp(0.5, 500.0), drawable: Vec::new() });
    }
    // children must be contiguous per group and groups ordered parent first:
    // one child per group, appended in group order, so both already hold.
    let parts_mass: f32 = children[1..].iter().map(|c| c.mass).sum();
    children[0].mass = (o.mass - parts_mass).max(o.mass * 0.5);
    if wheels.len() < 4 {
        eprintln!("vehicle: warning: only wheels {wheels:?} found in the skeleton");
    }

    let lod_dist = [9998.0f32; 4];
    let mut frag = Frag { name: o.model.clone(), bones: bones.clone(), lods: Default::default(), lod_dist, groups, children, mass: o.mass };
    for (k, (slot, s, l)) in main_plan.iter().enumerate() {
        frag.lods[*slot] = if k == 0 { high.clone() } else { to_models(&s.lods[*l]) };
    }
    let stream = o.out.join("stream");
    let data = o.out.join("data");
    fs::create_dir_all(&stream)?;
    fs::create_dir_all(&data)?;
    let mut written = Vec::new();
    let mut put = |file: PathBuf, b: &[u8]| -> Result<(), Box<dyn Error>> {
        write_atomic(&file, b).map_err(|e| format!("{}: {e}", file.display()))?;
        written.push((file, b.len()));
        Ok(())
    };
    let y = yft::build_yft(&frag)?;
    super::vlog::yft(&format!("{}.yft", o.model), &y, frag.children.len());
    put(stream.join(format!("{}.yft", o.model)), &y)?;
    if let Some(s) = &s_hi {
        let mut hi = frag.clone();
        hi.lods = Default::default();
        hi.lods[0] = to_models(&s.lods[0]);
        let y = yft::build_yft(&hi)?;
        super::vlog::yft(&format!("{}_hi.yft", o.model), &y, hi.children.len());
        put(stream.join(format!("{}_hi.yft", o.model)), &y)?;
    }
    super::vlog::flush(&o.out);

    // texture dictionary
    let mut tx = Vec::new();
    for t in &texs {
        if t == MISSING || t == PAINT {
            continue;
        }
        let p = cache.join(format!("{}.dds", &t[4..]));
        let bytes = fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        let mut x = ytd::from_dds(t, &bytes)?;
        if let Some(m) = o.max_tex {
            x.limit_size(m);
        }
        tx.push(x);
    }
    if wf.0.is_empty() || wr.0.is_empty() {
        tx.push(solid(TYRE, [0x20, 0x20, 0x20, 0xFF], 16));
    }
    if texs.iter().any(|t| t == PAINT) {
        let [r, g, b] = o.paint.or(vd.as_ref().map(|v| v.colour)).unwrap_or([0x80; 3]);
        tx.push(solid(PAINT, [b, g, r, 0xFF], 16));
    }
    if texs.iter().any(|t| t == MISSING) {
        tx.push(solid(MISSING, [0x80, 0x80, 0x80, 0xFF], 16));
    }
    put(stream.join(format!("{}.ytd", o.model)), &ytd::build_ytd(&tx)?)?;

    // meta + manifest
    put(data.join("vehicles.meta"), vehicles_meta(&o, vd.as_ref()).as_bytes())?;
    put(data.join("handling.meta"), handling_meta(&o).as_bytes())?;
    put(data.join("carvariations.meta"), carvariations_meta(&o).as_bytes())?;
    put(data.join("carcols.meta"), CARCOLS.as_bytes())?;
    put(o.out.join("fxmanifest.lua"), FXMANIFEST.as_bytes())?;

    let ntris: usize = frag.lods[0].iter().flat_map(|m| m.geoms.iter()).map(|g| g.indices.len() / 3).sum();
    for (f, n) in &written {
        println!("  {} ({n} bytes)", f.display());
    }
    if !missing.is_empty() {
        println!("vehicle: {} textures missing (grey placeholder): {}", missing.len(), missing.join("; "));
    }
    println!(
        "vehicle: {} -> {} ({} of {} bones, {} high-LOD parts / {} tris, {} textures, wheels {:?}), {:.1}s -> {}",
        o.name,
        o.model,
        bones.len(),
        skel.len(),
        frag.lods[0].len(),
        ntris,
        tx.len(),
        wheels,
        t0.elapsed().as_secs_f32(),
        o.out.display()
    );
    Ok(())
}

/// Bones to keep (original indices, ascending = parents first), at most 255.
fn prune(skel: &[drawable::Bone], used: &HashSet<usize>) -> Vec<usize> {
    let n = skel.len();
    let mut keep = vec![false; n];
    let mut count = 0;
    let add = |i: usize, keep: &mut Vec<bool>, count: &mut usize| -> bool {
        // the bone and its missing ancestors
        let mut chain = Vec::new();
        let mut c = Some(i);
        while let Some(x) = c {
            if keep[x] {
                break;
            }
            chain.push(x);
            c = skel[x].parent.map(|p| p as usize).filter(|&p| p < n);
        }
        if *count + chain.len() > 255 {
            return false;
        }
        for x in chain {
            keep[x] = true;
            *count += 1;
        }
        true
    };
    add(0, &mut keep, &mut count);
    let mut u: Vec<usize> = used.iter().copied().collect();
    u.sort();
    for i in u {
        add(i, &mut keep, &mut count);
    }
    let junk = |s: &str| s.starts_with("deform_skinbone") || s.contains("_attach") || s.starts_with("mod_") || s.contains("_target") || s.contains("lookat");
    for i in 0..n {
        if GTA_BONES.iter().any(|p| skel[i].name.starts_with(p)) && !junk(&skel[i].name) {
            add(i, &mut keep, &mut count);
        }
    }
    for i in 0..n {
        if !junk(&skel[i].name) {
            add(i, &mut keep, &mut count);
        }
    }
    (0..n).filter(|&i| keep[i]).collect()
}

fn list(o: &Opts, entries: &[Entry]) -> Result<(), Box<dyn Error>> {
    // vehicle fragments: name has a _hi sibling and a chassis bone (checked lazily: name pattern only)
    let names: HashSet<&str> = entries.iter().filter(|e| e.rtype.as_deref() == Some("ft")).filter_map(|e| e.name.as_deref()).collect();
    let mut v: Vec<&str> = names
        .iter()
        .copied()
        .filter(|n| !n.ends_with("_hi") && !n.ends_with("_mid") && names.contains(format!("{n}_hi").as_str()) && names.contains(format!("{n}_mid").as_str()))
        .collect();
    v.sort();
    let mut shown = 0;
    for n in v {
        let e = entries.iter().find(|e| e.name.as_deref() == Some(n) && e.rtype.as_deref() == Some("ft")).unwrap();
        let Ok(r) = Resource::load(&o.game.join("levels.rpf"), e) else { continue };
        let sk = drawable::find_skeleton(&r);
        if sk.first().is_some_and(|b| b.name == "chassis") && sk.iter().any(|b| b.name.starts_with("wheel_")) {
            println!("{n}");
            shown += 1;
        }
    }
    eprintln!("{shown} vehicles");
    Ok(())
}

// ------------------------------------------------------------------ meta

const FXMANIFEST: &str = "fx_version 'cerulean'
game 'gta5'

files {
  'data/vehicles.meta',
  'data/handling.meta',
  'data/carvariations.meta',
  'data/carcols.meta',
}

data_file 'HANDLING_FILE' 'data/handling.meta'
data_file 'VEHICLE_METADATA_FILE' 'data/vehicles.meta'
data_file 'CARCOLS_FILE' 'data/carcols.meta'
data_file 'VEHICLE_VARIATION_FILE' 'data/carvariations.meta'
";

const CARCOLS: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<CVehicleModelInfoVarGlobal>
  <Kits />
  <Lights />
  <Sirens />
</CVehicleModelInfoVarGlobal>
";

fn vehicles_meta(o: &Opts, vd: Option<&VData>) -> String {
    let (wsf, wsr) = vd.map_or((0.25, 0.25), |v| (v.wheel_scale, v.wheel_scale_rear));
    let m = &o.model;
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<CVehicleModelInfo__InitDataList>
  <residentTxd>vehshare</residentTxd>
  <residentAnims />
  <InitDatas>
    <Item>
      <modelName>{m}</modelName>
      <txdName>{m}</txdName>
      <handlingId>{hid}</handlingId>
      <gameName>{gn}</gameName>
      <vehicleMakeName>6M</vehicleMakeName>
      <expressionDictName>null</expressionDictName>
      <expressionName>null</expressionName>
      <animConvRoofDictName>null</animConvRoofDictName>
      <animConvRoofName>null</animConvRoofName>
      <animConvRoofWindowsAffected />
      <ptfxAssetName>null</ptfxAssetName>
      <audioNameHash>BLISTA</audioNameHash>
      <layout>LAYOUT_STANDARD</layout>
      <coverBoundOffsets>STANDARD_COVER_OFFSET_INFO</coverBoundOffsets>
      <explosionInfo>EXPLOSION_INFO_DEFAULT</explosionInfo>
      <scenarioLayout />
      <cameraName>DEFAULT_FOLLOW_VEHICLE_CAMERA</cameraName>
      <aimCameraName>DEFAULT_THIRD_PERSON_VEHICLE_AIM_CAMERA</aimCameraName>
      <bonnetCameraName>VEHICLE_BONNET_CAMERA_STANDARD</bonnetCameraName>
      <povCameraName>DEFAULT_POV_CAMERA</povCameraName>
      <FirstPersonDriveByIKOffset x="0.000000" y="-0.060000" z="-0.060000" />
      <FirstPersonDriveByUnarmedIKOffset x="0.000000" y="-0.060000" z="0.000000" />
      <FirstPersonProjectileDriveByIKOffset x="0.000000" y="0.000000" z="0.000000" />
      <FirstPersonProjectileDriveByPassengerIKOffset x="0.000000" y="0.000000" z="0.000000" />
      <FirstPersonDriveByRightPassengerIKOffset x="0.000000" y="-0.060000" z="-0.060000" />
      <FirstPersonDriveByRightPassengerUnarmedIKOffset x="0.000000" y="-0.060000" z="0.000000" />
      <FirstPersonMobilePhoneOffset x="0.155000" y="0.300000" z="0.520000" />
      <FirstPersonPassengerMobilePhoneOffset x="0.136000" y="0.223000" z="0.425000" />
      <PovCameraOffset x="0.000000" y="-0.150000" z="0.650000" />
      <PovCameraVerticalAdjustmentForRollCage value="0.000000" />
      <PovPassengerCameraOffset x="0.000000" y="0.000000" z="0.000000" />
      <PovRearPassengerCameraOffset x="0.000000" y="0.000000" z="0.000000" />
      <vfxInfoName>VFXVEHICLEINFO_CAR_GENERIC</vfxInfoName>
      <shouldUseCinematicViewMode value="true" />
      <shouldCameraTransitionOnClimbUpDown value="false" />
      <shouldCameraIgnoreExiting value="false" />
      <AllowPretendOccupants value="true" />
      <AllowJoyriding value="true" />
      <AllowSundayDriving value="true" />
      <AllowBodyColorMapping value="true" />
      <wheelScale value="{wsf:.6}" />
      <wheelScaleRear value="{wsr:.6}" />
      <dirtLevelMin value="0.000000" />
      <dirtLevelMax value="0.300000" />
      <envEffScaleMin value="0.000000" />
      <envEffScaleMax value="1.000000" />
      <envEffScaleMin2 value="0.000000" />
      <envEffScaleMax2 value="1.000000" />
      <damageMapScale value="0.600000" />
      <damageOffsetScale value="1.000000" />
      <diffuseTint value="0x00FFFFFF" />
      <steerWheelMult value="1.000000" />
      <HDTextureDist value="5.000000" />
      <lodDistances content="float_array">
        15.000000
        30.000000
        60.000000
        120.000000
        500.000000
        500.000000
      </lodDistances>
      <minSeatHeight value="0.85" />
      <identicalModelSpawnDistance value="20" />
      <maxNumOfSameColor value="10" />
      <defaultBodyHealth value="1000.000000" />
      <pretendOccupantsScale value="1.000000" />
      <visibleSpawnDistScale value="1.000000" />
      <trackerPathWidth value="2.000000" />
      <weaponForceMult value="1.000000" />
      <frequency value="100" />
      <swankness>SWANKNESS_2</swankness>
      <maxNum value="999" />
      <flags>FLAG_AVERAGE_CAR FLAG_HAS_INTERIOR_EXTRAS</flags>
      <type>VEHICLE_TYPE_CAR</type>
      <plateType>VPT_FRONT_AND_BACK_PLATES</plateType>
      <dashboardType>VDT_GENTAXI</dashboardType>
      <vehicleClass>VC_SEDAN</vehicleClass>
      <wheelType>VWT_SPORT</wheelType>
      <trailers />
      <additionalTrailers />
      <drivers />
      <extraIncludes />
      <doorsWithCollisionWhenClosed />
      <driveableDoors />
      <bumpersNeedToCollideWithMap value="false" />
      <needsRopeTexture value="false" />
      <requiredExtras />
      <rewards />
      <cinematicPartCamera>
        <Item>WHEEL_FRONT_RIGHT_CAMERA</Item>
        <Item>WHEEL_FRONT_LEFT_CAMERA</Item>
        <Item>WHEEL_REAR_RIGHT_CAMERA</Item>
        <Item>WHEEL_REAR_LEFT_CAMERA</Item>
      </cinematicPartCamera>
      <NmBraceOverrideSet />
      <buoyancySphereOffset x="0.000000" y="0.000000" z="0.000000" />
      <buoyancySphereSizeScale value="1.000000" />
      <pOverrideRagdollThreshold type="NULL" />
      <firstPersonDrivebyData>
        <Item>STD_BLISTA_FRONT_LEFT</Item>
        <Item>STD_BLISTA_FRONT_RIGHT</Item>
      </firstPersonDrivebyData>
    </Item>
  </InitDatas>
  <txdRelationships />
</CVehicleModelInfo__InitDataList>
"#,
        hid = m.to_uppercase(),
        gn = m.to_uppercase().chars().take(11).collect::<String>(),
    )
}

fn handling_meta(o: &Opts) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<CHandlingDataMgr>
  <HandlingData>
    <Item type="CHandlingData">
      <handlingName>{hid}</handlingName>
      <fMass value="{mass:.1}" />
      <fInitialDragCoeff value="8.0" />
      <fPercentSubmerged value="85.0" />
      <vecCentreOfMassOffset x="0.0" y="0.0" z="0.0" />
      <vecInertiaMultiplier x="1.0" y="1.0" z="1.4" />
      <fDriveBiasFront value="0.0" />
      <nInitialDriveGears value="5" />
      <fInitialDriveForce value="0.28" />
      <fDriveInertia value="1.0" />
      <fClutchChangeRateScaleUpShift value="2.5" />
      <fClutchChangeRateScaleDownShift value="2.5" />
      <fInitialDriveMaxFlatVel value="150.0" />
      <fBrakeForce value="0.8" />
      <fBrakeBiasFront value="0.6" />
      <fHandBrakeForce value="0.7" />
      <fSteeringLock value="40.0" />
      <fTractionCurveMax value="2.2" />
      <fTractionCurveMin value="2.0" />
      <fTractionCurveLateral value="22.5" />
      <fTractionSpringDeltaMax value="0.15" />
      <fLowSpeedTractionLossMult value="1.0" />
      <fCamberStiffnesss value="0.0" />
      <fTractionBiasFront value="0.49" />
      <fTractionLossMult value="1.0" />
      <fSuspensionForce value="2.0" />
      <fSuspensionCompDamp value="1.3" />
      <fSuspensionReboundDamp value="2.0" />
      <fSuspensionUpperLimit value="0.12" />
      <fSuspensionLowerLimit value="-0.14" />
      <fSuspensionRaise value="0.0" />
      <fSuspensionBiasFront value="0.5" />
      <fAntiRollBarForce value="0.8" />
      <fAntiRollBarBiasFront value="0.6" />
      <fRollCentreHeightFront value="0.3" />
      <fRollCentreHeightRear value="0.3" />
      <fCollisionDamageMult value="1.0" />
      <fWeaponDamageMult value="1.0" />
      <fDeformationDamageMult value="0.8" />
      <fEngineDamageMult value="1.5" />
      <fPetrolTankVolume value="65.0" />
      <fOilVolume value="5.0" />
      <fSeatOffsetDistX value="0.0" />
      <fSeatOffsetDistY value="0.0" />
      <fSeatOffsetDistZ value="0.0" />
      <nMonetaryValue value="25000" />
      <strModelFlags>440010</strModelFlags>
      <strHandlingFlags>0</strHandlingFlags>
      <strDamageFlags>0</strDamageFlags>
      <AIHandling>AVERAGE</AIHandling>
      <SubHandlingData>
        <Item type="CCarHandlingData">
          <fBackEndPopUpCarImpulseMult value="0.1" />
          <fBackEndPopUpBuildingImpulseMult value="0.03" />
          <fBackEndPopUpMaxDeltaSpeed value="0.6" />
        </Item>
        <Item type="NULL" />
        <Item type="NULL" />
      </SubHandlingData>
    </Item>
  </HandlingData>
</CHandlingDataMgr>
"#,
        hid = o.model.to_uppercase(),
        mass = o.mass,
    )
}

fn carvariations_meta(o: &Opts) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<CVehicleModelInfoVariation>
  <variationData>
    <Item>
      <modelName>{m}</modelName>
      <colors>
        <Item>
          <indices content="char_array">0 0 0 156</indices>
          <liveries />
        </Item>
        <Item>
          <indices content="char_array">111 111 0 156</indices>
          <liveries />
        </Item>
      </colors>
      <kits>
        <Item>0_default_modkit</Item>
      </kits>
      <windowsWithExposedEdges />
      <plateProbabilities>
        <Probabilities>
          <Item>
            <Name>Standard White</Name>
            <Value value="100" />
          </Item>
        </Probabilities>
      </plateProbabilities>
      <lightSettings value="1" />
      <sirenSettings value="0" />
    </Item>
  </variationData>
</CVehicleModelInfoVariation>
"#,
        m = o.model
    )
}

// ---------------------------------------------------------- vehicle data

/// From `sixng vehicles` (vehicles.json): wheel scale, one spawn
/// configuration's rim, tyre and body colour.
struct VData {
    wheel_scale: f32,
    wheel_scale_rear: f32,
    rim: Option<String>,
    tyre: Option<String>,
    /// sRGB, reading the stored ARGB as linear (format notes; unverified)
    colour: [u8; 3],
}

fn vehicle_data(o: &Opts) -> Option<VData> {
    let p = o.vdata.clone().unwrap_or_else(|| o.work.join("vehicles").join("vehicles.json"));
    let Ok(bytes) = fs::read(&p) else {
        eprintln!("vehicle: no {} (run `6ixpack sixng vehicles --out <work>`): generated tyre, grey paint, wheelScale 0.25", p.display());
        return None;
    };
    let v: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| eprintln!("vehicle: {}: {e}", p.display())).ok()?;
    let Some(m) = v["vehicles"].as_array()?.iter().find(|m| m["model"].as_str() == Some(o.name.as_str())) else {
        eprintln!("vehicle: {} not in {}", o.name, p.display());
        return None;
    };
    let mi = &m["model_info"];
    let wsf = mi["wheelScale"].as_f64().unwrap_or(0.25) as f32;
    let wsr = mi["wheelScaleRear"].as_f64().map_or(wsf, |x| x as f32);
    let cfgs = m["configurations"].as_array().cloned().unwrap_or_default();
    let c = cfgs.get(o.config).or(cfgs.first());
    let s = |k: &str| c.and_then(|c| c[k].as_str()).map(String::from);
    let hex = c.and_then(|c| c["colour"]["srgb_if_linear"].as_str()).and_then(|h| u32::from_str_radix(h.trim_start_matches('#'), 16).ok()).unwrap_or(0x808080);
    Some(VData { wheel_scale: wsf, wheel_scale_rear: wsr, rim: s("rim"), tyre: s("tyre"), colour: [(hex >> 16) as u8, (hex >> 8) as u8, hex as u8] })
}

/// Lamps, neon, screens: emissive shaders by recovered name or material
/// family (ported from SixNG 4a1bd3d, glTF emissive materials).
fn is_emissive(sh: &drawable::Shader) -> bool {
    sixng_core::names::shader(&sh.name_hash).is_some_and(|n| n.contains("emissive"))
        || sixng_core::material::classify(&sh.name_hash, &sh.flags)["class"] == "emissive"
}

/// `--paint`: sRGB hex RRGGBB or AARRGGBB (`#` / `0x` optional, alpha
/// ignored), as SixNG's `--paint` (4a1bd3d) takes it.
fn parse_paint(s: &str) -> Option<[u8; 3]> {
    let h = s.trim().trim_start_matches('#').trim_start_matches("0x");
    let v = u32::from_str_radix(h, 16).ok().filter(|_| h.len() == 6 || h.len() == 8)?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}
