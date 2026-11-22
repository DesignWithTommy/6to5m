//! `--split <metres>`: after a build, hard-link the files in <out>/stream into
//! several FiveM resources under <out>/[<base>]/, one per square region, plus
//! one `<base>_common` resource. Each has its own fxmanifest.lua; region
//! resources declare `dependency '<base>_common'` so the archetypes load first.
//!
//! Assignment:
//! - t6m_bld_XX_YY.ymap and t6m_col_XX_YY.ybn go to the region holding their
//!   512 m cell centre.
//! - A building ydr goes to the region of the HD ymaps placing it when that is
//!   one region; used by several regions or by a LOD/SLOD ymap -> common.
//!   (FiveM resolves streamed names globally, so a shared asset needs to exist
//!   once, not once per resource; duplicating it only costs disk and RAM.)
//! - A family txd (t6m_txdNNNN) follows its archetypes the same way; the
//!   shared chain t6m_shrNNNN and gtxd.meta live in common, so every parent
//!   txd is loaded wherever a child is.
//! - Common also holds the ytyps, every LOD/SLOD ymap (t6m_lodN: a whole LOD
//!   level is one ymap and each is the parent of the one below, so the chain
//!   stays intact; HD cell ymaps name `t6m_lod1` as parent, which resolves
//!   across resources), the terrain (one ymap, ~130 MB for the whole map),
//!   water.xml and anything unrecognised.
//! - A region over the size cap (--split-max, MB) is split into quarters, down
//!   to one 512 m cell; common is only reported.
//!
//! Hard links keep <out>/stream as the incremental build cache; copy only the
//! [<base>] folder to the server.

use super::Ctx;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    error::Error,
    fs,
    path::Path,
};

/// What `buildings` placed where, kept in <out>/.fivem-layout.json so a
/// later `--only collision --split ...` run can still split buildings.
#[derive(Serialize, Deserialize, Default)]
pub struct Layout {
    /// archetype (ydr stem) -> family txd
    pub txd: BTreeMap<String, String>,
    /// HD cell ymap stem -> archetypes it places
    pub ymaps: BTreeMap<String, BTreeSet<String>>,
    /// LOD/SLOD ymap stem -> archetypes it places
    pub lod_ymaps: BTreeMap<String, BTreeSet<String>>,
}

pub fn load(out: &Path) -> Layout {
    fs::read(out.join(".fivem-layout.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

pub fn save(ctx: &Ctx) -> Result<(), String> {
    let p = ctx.o.out.join(".fivem-layout.json");
    fs::write(&p, serde_json::to_vec(&*ctx.layout.lock().unwrap()).unwrap()).map_err(|e| format!("{}: {e}", p.display()))
}

const CELL: i64 = 512;

/// (cx, cy) of a `<prefix>XX_YY.<ext>` cell file (XX = cell + 50).
fn cell_of(stem: &str) -> Option<(i64, i64)> {
    // 128 m collision cells (t6m_cc_XXX_YYY, XXX = cell + 200) -> their 512 m cell
    if let Some((a, b)) = stem.strip_prefix("t6m_cc_").and_then(|r| r.split_once('_')) {
        return Some(((a.parse::<i64>().ok()? - 200).div_euclid(CELL / 128), (b.parse::<i64>().ok()? - 200).div_euclid(CELL / 128)));
    }
    let rest = stem.strip_prefix("t6m_bld_").or_else(|| stem.strip_prefix("t6m_col_"))?;
    let (a, b) = rest.split_once('_')?;
    Some((a.parse::<i64>().ok()? - 50, b.parse::<i64>().ok()? - 50))
}

/// Region key: (size in metres, x index, y index).
type Region = (i64, i64, i64);

fn region_of(c: (i64, i64), top: i64, sub: &BTreeSet<Region>) -> Region {
    let centre = (c.0 * CELL + CELL / 2, c.1 * CELL + CELL / 2);
    let mut s = top;
    loop {
        let r = (s, centre.0.div_euclid(s), centre.1.div_euclid(s));
        if s <= CELL || !sub.contains(&r) {
            return r;
        }
        s /= 2;
    }
}

fn idx(v: i64) -> String {
    if v < 0 { format!("m{}", -v) } else { v.to_string() }
}

pub fn split(ctx: &Ctx, metres: f32, max_mb: u64) -> Result<(), Box<dyn Error>> {
    let top = ((metres.max(CELL as f32) / CELL as f32).round() as i64).max(1) * CELL;
    let base: String = ctx.o.out.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_else(|| "t6m".into()).chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }).collect();
    let common = format!("{base}_common");
    let lay = ctx.layout.lock().unwrap();
    let files: Vec<(String, u64)> = fs::read_dir(&ctx.stream)?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.metadata().map(|m| m.len()).unwrap_or(0)))
        .filter(|(n, _)| !n.ends_with(".tmp"))
        .collect();
    let lod_used: BTreeSet<&String> = lay.lod_ymaps.values().flatten().collect();
    let cap = max_mb * 1024 * 1024;
    let mut sub: BTreeSet<Region> = BTreeSet::new();
    loop {
        // None = common
        let mut arch_r: HashMap<&str, Option<Region>> = HashMap::new();
        for (ym, ans) in &lay.ymaps {
            let Some(c) = cell_of(ym) else { continue };
            let r = region_of(c, top, &sub);
            for a in ans {
                let e = arch_r.entry(a.as_str()).or_insert(Some(r));
                if *e != Some(r) {
                    *e = None;
                }
            }
        }
        for a in &lod_used {
            arch_r.insert(a.as_str(), None);
        }
        let mut txd_r: HashMap<&str, Option<Region>> = HashMap::new();
        for (a, t) in &lay.txd {
            let r = arch_r.get(a.as_str()).copied().flatten();
            let e = txd_r.entry(t.as_str()).or_insert(r);
            if *e != r {
                *e = None;
            }
        }
        let place = |name: &str| -> Option<Region> {
            let (stem, ext) = name.rsplit_once('.')?;
            match ext {
                "ymap" if lay.ymaps.contains_key(stem) => cell_of(stem).map(|c| region_of(c, top, &sub)),
                "ybn" if stem.starts_with("t6m_col_") || stem.starts_with("t6m_cc_") => cell_of(stem).map(|c| region_of(c, top, &sub)),
                "ydr" => arch_r.get(stem).copied().flatten(),
                "ytd" => txd_r.get(stem).copied().flatten(),
                _ => None,
            }
        };
        let mut groups: BTreeMap<Option<Region>, Vec<&(String, u64)>> = BTreeMap::new();
        for f in &files {
            groups.entry(place(&f.0)).or_default().push(f);
        }
        let size = |v: &Vec<&(String, u64)>| v.iter().map(|f| f.1).sum::<u64>();
        let over: Vec<Region> = groups.iter().filter_map(|(r, v)| r.filter(|r| r.0 > CELL && size(v) > cap)).collect();
        if !over.is_empty() {
            sub.extend(over);
            continue;
        }
        // write
        let root = ctx.o.out.join(format!("[{base}]"));
        // synced in place: unchanged files keep their link (and mtime), so
        // FXServer only rehashes resources whose files changed
        fs::create_dir_all(&root)?;
        let mut names = Vec::new();
        let mut summary = Vec::new();
        let ytyps: Vec<&str> = files.iter().map(|f| f.0.as_str()).filter(|n| n.ends_with(".ytyp")).collect();
        for (r, v) in &groups {
            let name = match r {
                None => common.clone(),
                Some((s, x, y)) => format!("{base}_{s}_{}_{}", idx(*x), idx(*y)),
            };
            let dir = root.join(&name);
            let st = dir.join("stream");
            fs::create_dir_all(&st)?;
            let want: BTreeSet<&str> = v.iter().map(|f| f.0.as_str()).collect();
            for e in fs::read_dir(&st)?.flatten() {
                if !want.contains(e.file_name().to_string_lossy().as_ref()) {
                    fs::remove_file(e.path())?;
                }
            }
            for (f, _) in v {
                let (src, dst) = (ctx.stream.join(f), st.join(f));
                let stamp = |p: &Path| fs::metadata(p).ok().map(|m| (m.len(), m.modified().ok()));
                if stamp(&dst).is_none() || stamp(&dst) != stamp(&src) {
                    super::link_into(&src, &dst)?;
                }
            }
            let mut t = String::from("fx_version 'cerulean'\ngame 'gta5'\nthis_is_a_map 'yes'\n");
            if r.is_none() {
                let mut ys = ytyps.clone();
                ys.sort();
                for y in ys {
                    t += &format!("data_file 'DLC_ITYP_REQUEST' 'stream/{y}'\n");
                }
                let extra: Vec<&str> = ["water.xml", "gtxd.meta"].into_iter().filter(|f| ctx.o.out.join(f).exists()).collect();
                for f in &extra {
                    write_if_changed(&dir.join(f), &fs::read(ctx.o.out.join(f))?)?;
                }
                if !extra.is_empty() {
                    t += &format!("files {{ {} }}\n", extra.iter().map(|f| format!("'{f}'")).collect::<Vec<_>>().join(", "));
                }
                if extra.contains(&"water.xml") {
                    t += "data_file 'WATER_FILE' 'water.xml'\n";
                }
                if extra.contains(&"gtxd.meta") {
                    t += "data_file 'GTXD_PARENTING_DATA' 'gtxd.meta'\n";
                }
            } else {
                t += &format!("dependency '{common}'\n");
            }
            write_if_changed(&dir.join("fxmanifest.lua"), t.as_bytes())?;
            let bytes = size(v);
            let mb = bytes as f64 / 1048576.0;
            if bytes > cap {
                eprintln!("split: {name} is {mb:.0} MB, over --split-max {max_mb} MB");
            }
            summary.push(json!({"resource": name, "files": v.len(), "mb": (mb * 10.0).round() / 10.0}));
            names.push(name);
        }
        if !names.contains(&common) {
            // regions depend on it: always write an (empty) common resource
            let dir = root.join(&common);
            fs::create_dir_all(&dir)?;
            fs::write(dir.join("fxmanifest.lua"), "fx_version 'cerulean'\ngame 'gta5'\nthis_is_a_map 'yes'\n")?;
            names.insert(0, common.clone());
        }
        names.sort_by_key(|n| *n != common);
        let cfg: String = names.iter().map(|n| format!("ensure {n}\n")).collect();
        write_if_changed(&root.join("ensure.cfg"), cfg.as_bytes())?;
        // resources from an earlier split layout
        for e in fs::read_dir(&root)?.flatten() {
            if e.path().is_dir() && !names.iter().any(|n| e.file_name().to_string_lossy() == n.as_str()) {
                fs::remove_dir_all(e.path())?;
            }
        }
        // <out> itself must not be a resource any more, or FiveM loads both
        let _ = fs::remove_file(ctx.o.out.join("fxmanifest.lua"));
        eprintln!("split: {} resources in {} ({} m regions, cap {max_mb} MB)", names.len(), root.display(), top);
        ctx.report.lock().unwrap().outputs.push(json!({"file": format!("[{base}]"), "status": "split", "resources": summary}));
        return Ok(());
    }
}

fn write_if_changed(p: &Path, b: &[u8]) -> std::io::Result<()> {
    if fs::read(p).is_ok_and(|x| x == b) {
        return Ok(());
    }
    fs::write(p, b)
}
