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
//! - Collision material ids (all static collision is material 0), normal/spec shaders,
//!   skinned models (skipped and reported), prop-batch LOD parents.
//! - Rotation: placements store the inverse entity rotation, which is what
//!   CEntityDef stores too, so it is copied as is (like buildings_port.py).
//!   Not verified in game; `--conjugate-rot` writes the conjugate instead.

mod buildings;
mod collision;
mod geom;
mod terrain;
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
  --weapon-collision  keep weapon-only collision shapes (bullets); off by default (~20% of triangles)
  --conjugate-rot      write the conjugate of the stored placement quaternion
  --force              rebuild outputs even when up to date";

const TOOL: &str = "fivem-17";

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
}

impl Ctx {
    /// Manifest info for `file` when it is present, newer than the inputs and
    /// was built from the same fingerprint.
    pub fn up_to_date(&self, file: &str, fp: &str) -> Option<Value> {
        if self.o.force {
            return None;
        }
        let e = self.manifest.lock().unwrap().files.get(file).cloned()?;
        let m = fs::metadata(self.stream.join(file)).ok()?.modified().ok()?;
        (e.fp == fp && m >= self.input_time).then_some(e.info)
    }

    /// Write (or keep) a stream file, then re-parse it. `info` is stored in
    /// the manifest for later incremental runs. Returns the verify summary.
    pub fn emit(&self, file: &str, fp: &str, info: Value, bytes: Option<Vec<u8>>) -> Result<String, String> {
        let path = self.stream.join(file);
        let status = match &bytes {
            Some(b) => {
                if file.len() > 63 + 5 {
                    return Err(format!("{file}: name too long"));
                }
                let tmp = path.with_extension("tmp");
                fs::write(&tmp, b).and_then(|_| fs::rename(&tmp, &path)).map_err(|e| format!("{}: {e}", path.display()))?;
                "written"
            }
            None => "up-to-date",
        };
        let data = match bytes {
            Some(b) => b,
            None => fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?,
        };
        let summary = verify::verify(file, &data)?;
        self.manifest.lock().unwrap().files.insert(file.to_string(), MEntry { fp: fp.to_string(), info });
        self.report.lock().unwrap().outputs.push(json!({"file": file, "status": status, "bytes": data.len(), "verify": summary}));
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
            "--weapon-collision" => o.weapon_collision = true,
            "--force" => o.force = true,
            "--max-tex" => o.max_tex = Some(val(&mut it, a)?.parse().map_err(|e| format!("--max-tex: {e}"))?),
            _ => return Err(format!("unknown argument {a}\n{USAGE}")),
        }
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
    if manifest.tool != TOOL {
        manifest = Manifest { tool: TOOL.into(), ..Default::default() };
    }
    let ctx = Ctx { o, stream, levels, input_time, manifest: Mutex::new(manifest), report: Mutex::new(Report::default()) };
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
        fxmanifest(&ctx)?;
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

/// water.xml: 512 m ocean quads at z 0 over the 6m terrain extent plus a
/// 4 km margin (water_port.py), or over --bbox plus one quad.
fn water(ctx: &Ctx) -> Result<(), Box<dyn Error>> {
    let s = 512i64;
    let (x0, y0, x1, y1, m) = match ctx.o.bbox {
        Some(b) => (b[0].floor() as i64, b[1].floor() as i64, b[2].ceil() as i64, b[3].ceil() as i64, s),
        None => (-9607, -7957, 2442, 9474, 4000),
    };
    let mut q = String::new();
    let mut n = 0;
    let start = |v: i64| (v - m).div_euclid(s) * s;
    let mut x = start(x0);
    while x < x1 + m {
        let mut y = start(y0);
        while y < y1 + m {
            q += &format!(
                "  <Item><minX value=\"{x}\" /><maxX value=\"{}\" /><minY value=\"{y}\" /><maxY value=\"{}\" /><Type value=\"0\" /><IsInvisible value=\"false\" /><HasLimitedDepth value=\"false\" /><z value=\"0.0\" /><a1 value=\"26\" /><a2 value=\"26\" /><a3 value=\"26\" /><a4 value=\"26\" /><NoStencil value=\"false\" /></Item>\n",
                x + s,
                y + s
            );
            n += 1;
            y += s;
        }
        x += s;
    }
    let xml = format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<WaterData>\n <WaterQuads>\n{q} </WaterQuads>\n <CalmingQuads />\n <WaveQuads />\n</WaterData>\n");
    let p = ctx.o.out.join("water.xml");
    let same = fs::read_to_string(&p).is_ok_and(|t| t == xml);
    if !same {
        fs::write(&p, &xml)?;
    }
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

/// Write a file under the out dir's texture cache atomically.
pub fn write_atomic(p: &Path, b: &[u8]) -> std::io::Result<()> {
    let tmp = p.with_extension(format!("tmp{}", std::process::id()));
    fs::write(&tmp, b)?;
    fs::rename(&tmp, p)
}
