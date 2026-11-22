//! `6ixpack fivem build`: GTA VI (6m) world data -> FiveM map resource, in
//! pure Rust. Port of the 6m-port Python pipeline (terrain_port.py,
//! buildings_port.py, water_port.py, obj2ydrxml.py, ytdpack) onto the GTA V
//! writers in this crate.
//!
//! Output: <out>/stream/*.{ydr,ybn,ytd,ytyp,ymap}, <out>/fxmanifest.lua,
//! <out>/water.xml, <out>/.fivem-manifest.json (incremental state) and
//! <work>/fivem_tex_cache/*.dds (resolved source textures).
//!
//! Fragments are written as static ydr (main drawable, bind pose, no
//! breakage/physics). Interior (MLO) entities are moved into world space as
//! plain entities (no MLO ytyp, rooms or portals). LOD chains come from
//! `sixng lod-links`: HD cells -> t6m_lod1 (LOD) -> t6m_lod2 (SLOD1) ...
//!
//! TODO (not done here):
//! - Normal/spec shaders,
//!   skinned models (skipped and reported), prop-batch LOD parents.
//! - Rotation: placements store the inverse entity rotation, which is what
//!   CEntityDef stores too, so it is copied as is (like buildings_port.py).
//!   Not verified in game; `--conjugate-rot` writes the conjugate instead.

mod buildings;
pub mod ped;
mod collision;
mod geom;
pub mod split;
mod terrain;
mod vehicles;
mod weapons;
mod frags;
pub mod verify;

use memmap2::Mmap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    error::Error,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
    time::SystemTime,
};

pub const USAGE: &str = "\
usage: 6ixpack fivem build --game <dir> --out <resource dir> [options]
  --game <dir>         GTA VI install (levels.rpf, data/ttdb)
  --out <dir>          FiveM resource directory (stream/, fxmanifest.lua, water.xml)
  --bbox x0,y0,x1,y1   world region (metres); default: everything
  --name <substr>      only building archetypes whose name contains this
  --only <list>        terrain,buildings,collision,water (default: all)
  --report <file>      JSON report: converted / skipped with reason / outputs
  --work <dir>         sixng work dir with index/levels.json (default $SIXNG_WORK or ./work)
  --placements <dir>   sixng placement JSON dir with index.jsonl, or its parent
                       (default <work>/placements/levels; generated when missing)
  Source-texture cache: <work>/fivem_tex_cache (kept out of the resource).
  --max-tex <px>       drop top mips so building textures are at most this size (e.g. 512, 1024)
  --interiors         also place interior (MLO) contents in world space; off by
                       default because the MLO shell itself is not converted,
                       so contents of multi-floor interiors float
  --no-lod            HD entities only, all ORPHANHD (no LOD/SLOD chain)
  --simplify          simplify dense meshes (meshopt; off by default, flattens buildings)
  --weapon-collision  keep weapon-only collision shapes (bullets); off by default (~20% of triangles)
  --conjugate-rot      write the conjugate of the stored placement quaternion
  --force              rebuild outputs even when up to date
  --split <metres>     also write <out>/[<name>]/: one resource per square region
                       (rounded to 512 m cells) plus <name>_common (ytyps, LOD/SLOD
                       ymaps, shared txds, terrain, water); see ensure.cfg there
  --split-max <MB>     quarter any region resource bigger than this (default 1024)
  --bake               whole-map pass (no --bbox) that only fills the output cache
                       <work>/fivem_out_cache; --out defaults to <work>/fivem_bake.
                       Later regional builds then hardlink from the cache
also: 6ixpack fivem vehicle <name> --game <dir> --out <dir>   (one 6m vehicle -> FiveM vehicle resource; --help)
      6ixpack fivem weapon <name|all> --game <dir> --out <dir>  (6m weapons -> FiveM weapon resource; --help)";

/// Per-stage output versions: bump only the one whose writer changed, so
/// other outputs stay cached (each goes into its own outputs' fingerprints).
pub const TEX_V: &str = "tex1";
pub const YDR_V: &str = "ydr7";
/// Fragment props as dynamic .yft (fivem/frags.rs).
pub const FRAG_V: &str = "frag1";
pub const YBN_V: &str = "ybn2";
pub const TERRAIN_V: &str = "terrain2";
/// Meta files (ymap/ytyp) and water.xml are rebuilt every run and compared
/// byte for byte, so they need no version. MANIFEST_V is the manifest layout.
const MANIFEST_V: &str = "fivem-m2";

pub struct Opts {
    pub game: PathBuf,
    pub out: PathBuf,
    pub bbox: Option<[f32; 4]>,
    pub name: Option<String>,
    pub only: Vec<String>,
    pub report: Option<PathBuf>,
    pub work: PathBuf,
    pub placements: Option<PathBuf>,
    pub conjugate: bool,
    pub interiors: bool,
    pub no_lod: bool,
    pub weapon_collision: bool,
    pub force: bool,
    pub max_tex: Option<u16>,
    pub split: Option<f32>,
    pub split_max: u64,
    pub bake: bool,
}

impl Opts {
    pub fn in_bbox(&self, lo: [f32; 3], hi: [f32; 3]) -> bool {
        self.bbox.is_none_or(|b| hi[0] >= b[0] && lo[0] <= b[2] && hi[1] >= b[1] && lo[1] <= b[3])
    }
}

#[derive(Serialize, Deserialize, Default)]
struct Manifest {
    tool: String,
    files: BTreeMap<String, MEntry>,
}

#[derive(Serialize, Deserialize, Clone)]
struct MEntry {
    fp: String,
    #[serde(default)]
    info: Value,
    /// verify summary, so up-to-date files are not re-read
    #[serde(default)]
    summary: String,
}

/// Output cache sidecar (<key>.<ext>.json): info and verify summary.
#[derive(Serialize, Deserialize)]
struct CEntry {
    info: Value,
    summary: String,
}

#[derive(Default, Serialize)]
pub struct Report {
    pub converted: Vec<Value>,
    pub skipped: Vec<Value>,
    pub skipped_counts: BTreeMap<String, usize>,
    pub warnings: Vec<Value>,
    pub outputs: Vec<Value>,
}

/// Shared build state.
pub struct Ctx {
    pub o: Opts,
    pub stream: PathBuf,
    pub levels: Mmap,
    /// Newest input mtime: outputs older than this are rebuilt.
    pub input_time: SystemTime,
    manifest: Mutex<Manifest>,
    pub report: Mutex<Report>,
    pub layout: Mutex<split::Layout>,
    /// stream files emitted this run (for pruning stale outputs)
    emitted: Mutex<std::collections::HashSet<String>>,
    /// <work>/fivem_out_cache
    cache: PathBuf,
    /// levels.rpf size and mtime, part of every cache key
    input_stamp: String,
}

impl Ctx {
    fn cache_key(&self, fp: &str) -> String {
        sha(&format!("{fp}|{}", self.input_stamp))
    }

    fn cache_path(&self, file: &str, key: &str) -> PathBuf {
        let ext = file.rsplit_once('.').map_or("bin", |x| x.1);
        self.cache.join(format!("{key}.{ext}"))
    }

    /// Manifest info for `file` when stream/ already holds it from the same
    /// fingerprint and inputs; else, when the output cache has it, link it
    /// into stream/ and return the cached info. None = convert it.
    pub fn up_to_date(&self, file: &str, fp: &str) -> Option<Value> {
        if self.o.force {
            return None;
        }
        let key = self.cache_key(fp);
        if let Some(e) = self.manifest.lock().unwrap().files.get(file).cloned() {
            if e.fp == key && self.stream.join(file).is_file() {
                return Some(e.info);
            }
        }
        let c = self.cache_path(file, &key);
        let ce: CEntry = serde_json::from_slice(&fs::read(side(&c)).ok()?).ok()?;
        if !c.is_file() {
            return None;
        }
        link_into(&c, &self.stream.join(file)).ok()?;
        self.manifest.lock().unwrap().files.insert(file.to_string(), MEntry { fp: key, info: ce.info.clone(), summary: ce.summary });
        Some(ce.info)
    }

    /// Write (or keep) a stream file. New bytes are verified, stored in the
    /// output cache and linked into stream/ unless stream/ already holds the
    /// same bytes (left untouched: mtime and FXServer's hash stay valid).
    /// `info` goes to the manifest for later runs. Returns the verify summary.
    pub fn emit(&self, file: &str, fp: &str, info: Value, bytes: Option<Vec<u8>>) -> Result<String, String> {
        let path = self.stream.join(file);
        let meta = fp == "meta";
        let key = if meta { "meta".to_string() } else { self.cache_key(fp) };
        let (status, summary, len) = match bytes {
            Some(b) => {
                if file.len() > 63 + 5 {
                    return Err(format!("{file}: name too long"));
                }
                let summary = verify::verify(file, &b)?;
                let same = fs::metadata(&path).is_ok_and(|m| m.len() == b.len() as u64) && fs::read(&path).is_ok_and(|x| x == b);
                if meta {
                    if !same {
                        write_atomic(&path, &b).map_err(|e| format!("{}: {e}", path.display()))?;
                    }
                } else {
                    let c = self.cache_path(file, &key);
                    write_atomic(&c, &b).map_err(|e| format!("{}: {e}", c.display()))?;
                    let j = serde_json::to_vec(&CEntry { info: info.clone(), summary: summary.clone() }).unwrap();
                    write_atomic(&side(&c), &j).map_err(|e| format!("{}: {e}", c.display()))?;
                    if !same {
                        link_into(&c, &path).map_err(|e| format!("{}: {e}", path.display()))?;
                    }
                }
                (if same { "unchanged" } else { "written" }, summary, b.len() as u64)
            }
            None => {
                let known = self.manifest.lock().unwrap().files.get(file).filter(|e| e.fp == key && !e.summary.is_empty()).map(|e| e.summary.clone());
                let len = fs::metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?.len();
                let summary = match known {
                    Some(s) => s,
                    None => verify::verify(file, &fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?)?,
                };
                ("up-to-date", summary, len)
            }
        };
        self.emitted.lock().unwrap().insert(file.to_string());
        self.manifest.lock().unwrap().files.insert(file.to_string(), MEntry { fp: key, info, summary: summary.clone() });
        self.report.lock().unwrap().outputs.push(json!({"file": file, "status": status, "bytes": len, "verify": summary}));
        Ok(summary)
    }

    pub fn skip(&self, name: &str, reason: &str, extra: Value) {
        let mut r = self.report.lock().unwrap();
        *r.skipped_counts.entry(reason.to_string()).or_default() += 1;
        let mut v = json!({"name": name, "reason": reason});
        if let (Some(m), Value::Object(x)) = (v.as_object_mut(), extra) {
            m.extend(x);
        }
        r.skipped.push(v);
    }

    fn save_manifest(&self) -> Result<(), String> {
        let m = self.manifest.lock().unwrap();
        let p = self.o.out.join(".fivem-manifest.json");
        fs::write(&p, serde_json::to_vec(&*m).unwrap()).map_err(|e| format!("{}: {e}", p.display()))
    }
}

/// `r6m_<name>` when it fits in 63 chars, else `r6m_<joaat hex>`.
pub fn safe_name(name: &str) -> String {
    let clean: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c.to_ascii_lowercase() } else { '_' }).collect();
    let n = format!("r6m_{clean}");
    if n.len() < 64 { n } else { format!("r6m_{:08x}", super::rsc::joaat(name)) }
}

pub fn sha(s: &str) -> String {
    use sha2::{Digest, Sha256};
    let d = Sha256::digest(s.as_bytes());
    d.iter().take(12).map(|b| format!("{b:02x}")).collect()
}

fn parse(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts {
        game: PathBuf::new(),
        out: PathBuf::new(),
        bbox: None,
        name: None,
        only: vec!["terrain".into(), "buildings".into(), "collision".into(), "water".into()],
        report: None,
        work: std::env::var_os("SIXNG_WORK").map(PathBuf::from).unwrap_or_else(|| "work".into()),
        placements: None,
        conjugate: false,
        interiors: false,
        no_lod: false,
        weapon_collision: false,
        force: false,
        max_tex: None,
        split: None,
        split_max: 1024,
        bake: false,
    };
    let mut it = args.iter();
    let val = |it: &mut std::slice::Iter<String>, k: &str| it.next().cloned().ok_or(format!("{k} needs a value"));
    while let Some(a) = it.next() {
        match a.as_str() {
            "--game" => o.game = val(&mut it, a)?.into(),
            "--out" => o.out = val(&mut it, a)?.into(),
            "--work" => o.work = val(&mut it, a)?.into(),
            "--placements" => o.placements = Some(val(&mut it, a)?.into()),
            "--report" => o.report = Some(val(&mut it, a)?.into()),
            "--name" => o.name = Some(val(&mut it, a)?.to_lowercase()),
            "--only" => o.only = val(&mut it, a)?.split(',').map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()).collect(),
            "--bbox" => {
                let v: Vec<f32> = val(&mut it, a)?.split(',').map(|s| s.trim().parse::<f32>()).collect::<Result<_, _>>().map_err(|e| format!("--bbox: {e}"))?;
                let [a, b, c, d] = v[..] else { return Err("--bbox needs minx,miny,maxx,maxy".into()) };
                o.bbox = Some([a.min(c), b.min(d), a.max(c), b.max(d)]);
            }
            "--conjugate-rot" => o.conjugate = true,
            "--interiors" => o.interiors = true,
            "--no-lod" => o.no_lod = true,
            "--simplify" => geom::SIMPLIFY.store(true, std::sync::atomic::Ordering::Relaxed),
            "--weapon-collision" => o.weapon_collision = true,
            "--force" => o.force = true,
            "--max-tex" => o.max_tex = Some(val(&mut it, a)?.parse().map_err(|e| format!("--max-tex: {e}"))?),
            "--split" => o.split = Some(val(&mut it, a)?.parse().map_err(|e| format!("--split: {e}"))?),
            "--bake" => o.bake = true,
            "--split-max" => o.split_max = val(&mut it, a)?.parse().map_err(|e| format!("--split-max: {e}"))?,
            _ => return Err(format!("unknown argument {a}\n{USAGE}")),
        }
    }
    if o.bake {
        if o.bbox.is_some() || o.name.is_some() {
            return Err("--bake is a whole-map pass: drop --bbox / --name".into());
        }
        if o.out.as_os_str().is_empty() {
            o.out = o.work.join("fivem_bake");
        }
        o.split = None;
    }
    if o.game.as_os_str().is_empty() || o.out.as_os_str().is_empty() {
        return Err(USAGE.into());
    }
    if let Some(p) = o.placements.take() {
        o.placements = Some(resolve_placements(&p)?);
    }
    if let Some(k) = o.only.iter().find(|k| !["terrain", "buildings", "collision", "water"].contains(&k.as_str())) {
        return Err(format!("--only: unknown part {k}"));
    }
    Ok(o)
}

/// A placement dir must hold index.jsonl; accept its parent when exactly one
/// subfolder (or `levels`) has one.
fn resolve_placements(p: &std::path::Path) -> Result<PathBuf, String> {
    if p.join("index.jsonl").is_file() {
        return Ok(p.to_path_buf());
    }
    if p.join("levels").join("index.jsonl").is_file() {
        return Ok(p.join("levels"));
    }
    let subs: Vec<PathBuf> = fs::read_dir(p)
        .map_err(|e| format!("--placements {}: {e}", p.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|d| d.join("index.jsonl").is_file())
        .collect();
    match subs.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err(format!("--placements {}: no index.jsonl here or in a subfolder", p.display())),
        many => Err(format!("--placements {}: several subfolders have index.jsonl ({}); pick one", p.display(), many.iter().filter_map(|d| d.file_name()).map(|n| n.to_string_lossy()).collect::<Vec<_>>().join(", "))),
    }
}

pub fn run(args: &[OsString]) -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
    match args.first().map(String::as_str) {
        Some("build") => {}
        Some("ped") => return ped::run(&args[1..]),
        Some("vehicle") => return vehicles::run(&args[1..]),
        Some("weapon") => return weapons::run(&args[1..]),
        _ => {
            println!("{USAGE}");
            return Ok(());
        }
    }
    let o = parse(&args[1..])?;
    let t0 = std::time::Instant::now();
    let stream = o.out.join("stream");
    fs::create_dir_all(&stream)?;
    let lp = o.game.join("levels.rpf");
    let lf = fs::File::open(&lp).map_err(|e| format!("{}: {e}", lp.display()))?;
    let input_time = lf.metadata()?.modified()?;
    // SAFETY: read-only map; nothing writes the archive while we run.
    let levels = unsafe { Mmap::map(&lf) }?;
    let mut manifest: Manifest = fs::read(o.out.join(".fivem-manifest.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    if manifest.tool != MANIFEST_V {
        // older manifest layout; outputs come back from the cache
        manifest = Manifest { tool: MANIFEST_V.into(), ..Default::default() };
    }
    let layout = split::load(&o.out);
    let cache = o.work.join("fivem_out_cache");
    fs::create_dir_all(&cache)?;
    let input_stamp = format!("{}:{}", lf.metadata()?.len(), input_time.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0));
    let ctx = Ctx { o, stream, levels, input_time, manifest: Mutex::new(manifest), report: Mutex::new(Report::default()), layout: Mutex::new(layout), emitted: Default::default(), cache, input_stamp };
    let part = |p: &str| ctx.o.only.iter().any(|x| x == p);

    let result = (|| -> Result<(), Box<dyn Error>> {
        let needs_index = part("terrain") || part("buildings") || part("collision");
        let index = if needs_index {
            let ip = ctx.o.work.join("index").join("levels.json");
            eprintln!("loading {}", ip.display());
            Some(sixng_core::scan::load_index(&ip).map_err(|e| format!("{}: {e:#} (run `6ixpack sixng index levels.rpf --work ...` first)", ip.display()))?)
        } else {
            None
        };
        // buildings first: it records the flat ground tiles (asphalt, pads)
        // that terrain must sink below (<work>/fivem_ground.json)
        if part("buildings") {
            buildings::build(&ctx, index.as_ref().unwrap())?;
            ctx.save_manifest()?;
            split::save(&ctx)?;
        }
        if part("terrain") {
            terrain::build(&ctx, index.as_ref().unwrap())?;
            ctx.save_manifest()?;
        }
        if part("collision") {
            collision::build(&ctx, index.as_ref().unwrap())?;
            ctx.save_manifest()?;
        }
        if part("water") {
            water(&ctx)?;
        }
        if ["terrain", "buildings", "collision", "water"].iter().all(|p| part(p)) && ctx.o.name.is_none() {
            prune(&ctx)?;
        }
        fxmanifest(&ctx)?;
        if let Some(m) = ctx.o.split {
            split::split(&ctx, m, ctx.o.split_max)?;
        }
        Ok(())
    })();
    ctx.save_manifest()?;
    let r = ctx.report.lock().unwrap();
    let written = r.outputs.iter().filter(|v| v["status"] == "written").count();
    println!(
        "fivem: {} outputs ({} written, {} up to date), {} converted, {} skipped {:?}, {:.1}s -> {}",
        r.outputs.len(),
        written,
        r.outputs.len() - written,
        r.converted.len(),
        r.skipped.len(),
        r.skipped_counts,
        t0.elapsed().as_secs_f32(),
        ctx.o.out.display()
    );
    if let Some(p) = &ctx.o.report {
        fs::write(p, serde_json::to_vec_pretty(&*r)?).map_err(|e| format!("{}: {e}", p.display()))?;
    }
    result
}

/// Water cell size (water_port.py).
pub const WATER_CELL: f32 = 256.0;

/// After a full build (all parts, no --name): delete stream files and
/// manifest entries this run did not emit, so old outputs don't pile up.
fn prune(ctx: &Ctx) -> Result<(), Box<dyn Error>> {
    let keep = ctx.emitted.lock().unwrap();
    let (mut n, mut bytes) = (0usize, 0u64);
    for e in fs::read_dir(&ctx.stream)?.flatten() {
        let f = e.file_name().to_string_lossy().into_owned();
        if e.file_type().is_ok_and(|t| t.is_file()) && !keep.contains(&f) {
            bytes += e.metadata().map(|m| m.len()).unwrap_or(0);
            fs::remove_file(e.path())?;
            n += 1;
        }
    }
    ctx.manifest.lock().unwrap().files.retain(|f, _| keep.contains(f));
    if n > 0 {
        eprintln!("pruned {n} stale stream files ({:.0} MB)", bytes as f64 / 1048576.0);
    }
    ctx.report.lock().unwrap().outputs.push(json!({"file": "stream/", "status": "pruned", "files": n, "bytes": bytes}));
    Ok(())
}

/// water.xml (water_port.py): 256 m ocean cells at z 0 over the 6m terrain
/// extent plus a 4 km margin (or --bbox plus one cell), wet only where the
/// lowest terrain vertex in the cell is below z + 0.5 or there is no terrain
/// (<work>/fivem_water_low.json from the terrain stage); runs in a row merge.
fn water(ctx: &Ctx) -> Result<(), Box<dyn Error>> {
    let s = WATER_CELL as i64;
    let z = 0.0f32;
    let (x0, y0, x1, y1, m) = match ctx.o.bbox {
        Some(b) => (b[0].floor() as i64, b[1].floor() as i64, b[2].ceil() as i64, b[3].ceil() as i64, s),
        None => (-9607, -7957, 2442, 9474, 4000),
    };
    let lp = ctx.o.work.join("fivem_water_low.json");
    let low: std::collections::HashMap<(i64, i64), f32> = match fs::read_to_string(&lp).ok().and_then(|t| serde_json::from_str::<BTreeMap<String, f32>>(&t).ok()) {
        Some(m) => m.into_iter().filter_map(|(k, v)| { let (a, b) = k.split_once(',')?; Some(((a.parse().ok()?, b.parse().ok()?), v)) }).collect(),
        None => {
            ctx.skip("water.xml", "no terrain heights (run the terrain part); ocean everywhere", json!({}));
            Default::default()
        }
    };
    let wet = |i: i64, j: i64| low.get(&(i, j)).is_none_or(|h| *h < z + 0.5);
    let (i0, j0) = ((x0 - m).div_euclid(s), (y0 - m).div_euclid(s));
    let (i1, j1) = ((x1 + m).div_euclid(s), (y1 + m).div_euclid(s));
    let mut q = String::new();
    let mut n = 0;
    for j in j0..=j1 {
        let mut i = i0;
        while i <= i1 {
            if !wet(i, j) {
                i += 1;
                continue;
            }
            let a = i;
            while i <= i1 && wet(i, j) {
                i += 1;
            }
            q += &format!(
                "  <Item><minX value=\"{}\" /><maxX value=\"{}\" /><minY value=\"{}\" /><maxY value=\"{}\" /><Type value=\"0\" /><IsInvisible value=\"false\" /><HasLimitedDepth value=\"false\" /><z value=\"{z:.1}\" /><a1 value=\"26\" /><a2 value=\"26\" /><a3 value=\"26\" /><a4 value=\"26\" /><NoStencil value=\"false\" /></Item>
",
                a * s,
                i * s,
                j * s,
                (j + 1) * s
            );
            n += 1;
        }
    }
    let xml = format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<WaterData>
 <WaterQuads>
{q} </WaterQuads>
 <CalmingQuads />
 <WaveQuads />
</WaterData>
");
    let p = ctx.o.out.join("water.xml");
    let same = fs::read_to_string(&p).is_ok_and(|t| t == xml);
    if !same {
        fs::write(&p, &xml)?;
    }
    eprintln!("water: {n} quads");
    ctx.report.lock().unwrap().outputs.push(json!({"file": "water.xml", "status": if same {"up-to-date"} else {"written"}, "quads": n}));
    Ok(())
}

fn fxmanifest(ctx: &Ctx) -> Result<(), Box<dyn Error>> {
    let mut ytyps: Vec<String> = fs::read_dir(&ctx.stream)?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".ytyp"))
        .collect();
    ytyps.sort();
    let mut t = String::from("fx_version 'cerulean'\ngame 'gta5'\nthis_is_a_map 'yes'\n");
    for y in &ytyps {
        t += &format!("data_file 'DLC_ITYP_REQUEST' 'stream/{y}'\n");
    }
    let files: Vec<&str> = ["water.xml", "gtxd.meta"].into_iter().filter(|f| ctx.o.out.join(f).exists()).collect();
    if !files.is_empty() {
        t += &format!("files {{ {} }}\n", files.iter().map(|f| format!("'{f}'")).collect::<Vec<_>>().join(", "));
    }
    if files.contains(&"water.xml") {
        t += "data_file 'WATER_FILE' 'water.xml'\n";
    }
    if files.contains(&"gtxd.meta") {
        t += "data_file 'GTXD_PARENTING_DATA' 'gtxd.meta'\n";
    }
    fs::write(ctx.o.out.join("fxmanifest.lua"), t)?;
    Ok(())
}

/// Sidecar json of a cache file.
fn side(c: &Path) -> PathBuf {
    let mut n = c.as_os_str().to_owned();
    n.push(".json");
    PathBuf::from(n)
}

/// Put `src` at `dst` as a hard link (copy when linking fails, e.g. across
/// volumes), replacing what was there.
pub fn link_into(src: &Path, dst: &Path) -> std::io::Result<()> {
    let tmp = dst.with_extension(format!("lnk{}", std::process::id()));
    let _ = fs::remove_file(&tmp);
    if fs::hard_link(src, &tmp).is_err() {
        fs::copy(src, &tmp)?;
    }
    fs::rename(&tmp, dst)
}

/// Write a file under the out dir's texture cache atomically.
pub fn write_atomic(p: &Path, b: &[u8]) -> std::io::Result<()> {
    let tmp = p.with_extension(format!("tmp{}", std::process::id()));
    fs::write(&tmp, b)?;
    fs::rename(&tmp, p)
}
