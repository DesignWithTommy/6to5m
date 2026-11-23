//! Los Santos removal (part `lsremove`): empty stand-ins for the vanilla
//! GTA V `_citye`, `_cityw` and `_hills` ymaps and ybns, read straight from
//! the GTA V install (pure-Rust RPF7, `crate::gta5::rpf7`). Port of the old
//! `ytdpack lsstub` (CodeWalker) step.
//!
//! - Index: name, kind and box of every vanilla file, cached in
//!   `<work>/gta5_ls_index.json` (keyed by the archives' sizes and mtimes).
//! - Scope: with --bbox only files whose box overlaps the region (moved by
//!   --offset into GTA V space); a ymap's box is its streaming extents, so
//!   distant LOD ymaps visible from the region are included. Without --bbox
//!   all of them.
//! - Ymap stubs keep name, parent, flags, content flags and extents, with no
//!   entities. The selection is closed over the vanilla parent links: every
//!   ymap whose parent (transitively) is stubbed is stubbed too, or a vanilla
//!   child left in place would index its parent's entities (parentIndex) in
//!   an emptied ymap.
//! - Ybn stubs are one tiny triangle at z -900, byte-identical: all ~11k share
//!   one output-cache file (hardlinks in stream/), so they cost one file on
//!   disk and one download in FiveM's hash-addressed client cache.

use super::{Ctx, Phase};
use crate::gta5::{meta, rpf7, ybn};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
};

const INDEX_V: &str = "lsidx3";
const STUB_V: &str = "lsstub2";
const DIRS: [&str; 3] = ["_citye", "_cityw", "_hills"];

#[derive(Serialize, Deserialize, Clone)]
struct Item {
    /// [min, max] (ymap: streaming extents)
    bx: [[f32; 3]; 2],
    /// ymap: entities extents, flags, content flags
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ee: Option<[[f32; 3]; 2]>,
    #[serde(default)]
    flags: u32,
    #[serde(default)]
    cflags: u32,
    /// ymap: parent ymap name hash (CMapData +0x0C), 0 = none
    #[serde(default)]
    parent: u32,
}

#[derive(Serialize, Deserialize, Default)]
struct Index {
    tool: String,
    stamp: String,
    files: BTreeMap<String, Item>,
}

/// GTA V install: $GTA5_DIR, else the usual Steam / Rockstar / Epic folders.
fn gta5_dir() -> Option<PathBuf> {
    let mut c: Vec<PathBuf> = std::env::var_os("GTA5_DIR").map(PathBuf::from).into_iter().collect();
    for p in [
        r"C:\Program Files (x86)\Steam\steamapps\common\Grand Theft Auto V",
        r"C:\Program Files\Rockstar Games\Grand Theft Auto V",
        r"C:\Program Files\Epic Games\GTAV",
    ] {
        c.push(p.into());
    }
    c.into_iter().find(|d| d.join("GTA5.exe").is_file() && d.join("x64a.rpf").is_file())
}

/// Top-level archives, base first, update last (later ones override).
fn archives(g: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(g).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().to_lowercase().starts_with("x64") && n.to_string_lossy().to_lowercase().ends_with(".rpf"))).collect();
    v.sort();
    for u in ["update/update.rpf", "update/update2.rpf"] {
        if g.join(u).is_file() {
            v.push(g.join(u));
        }
    }
    // DLC map additions under levels/gta5/_citye|_cityw|_hills too
    let mut dlc: Vec<PathBuf> = fs::read_dir(g.join("update/x64/dlcpacks")).into_iter().flatten().flatten().map(|e| e.path().join("dlc.rpf")).filter(|p| p.is_file()).collect();
    dlc.sort();
    v.extend(dlc);
    v
}

/// Descend only along levels/gta5/<_citye|_cityw|_hills>/ (optionally x64/).
fn descend(p: &str) -> bool {
    let p = p.strip_prefix("x64/").unwrap_or(p);
    if "levels/gta5/".starts_with(p) {
        return true;
    }
    DIRS.iter().any(|d| {
        let want = format!("levels/gta5/{d}/");
        want.starts_with(p) || p.starts_with(&want)
    })
}

fn f3(b: &[u8], o: usize) -> Option<[f32; 3]> {
    (b.len() >= o + 12).then(|| std::array::from_fn(|k| f32::from_le_bytes(b[o + k * 4..o + k * 4 + 4].try_into().unwrap())))
}

fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    b.get(o..o + 4).map(|x| u32::from_le_bytes(x.try_into().unwrap()))
}

/// CMapData of an inflated ymap: (streaming, entities extents, flags, content flags).
fn ymap_info(d: &[u8]) -> Option<Item> {
    let root = u32_at(d, 0x1C)? as usize;
    let blocks = (u32_at(d, 0x30)? & 0x0FFF_FFFF) as usize;
    let nb = u16::from_le_bytes(d.get(0x4C..0x4E)?.try_into().ok()?) as usize;
    if root == 0 || root > nb {
        return None;
    }
    let b = blocks + (root - 1) * 16;
    let p = (u32_at(d, b + 8)? & 0x0FFF_FFFF) as usize;
    if d.len() < p + 0x60 {
        return None;
    }
    Some(Item { bx: [f3(d, p + 0x20)?, f3(d, p + 0x30)?], ee: Some([f3(d, p + 0x40)?, f3(d, p + 0x50)?]), flags: u32_at(d, p + 0x10)?, cflags: u32_at(d, p + 0x14)?, parent: u32_at(d, p + 0x0C)? })
}

/// phBound box of an inflated ybn.
fn ybn_info(d: &[u8]) -> Option<Item> {
    let (mx, mn) = (f3(d, 0x20)?, f3(d, 0x30)?);
    (mn.iter().zip(&mx).all(|(a, b)| a.is_finite() && b.is_finite() && a <= b)).then_some(Item { bx: [mn, mx], ee: None, flags: 0, cflags: 0, parent: 0 })
}

fn load_index(ctx: &Ctx, g: &Path) -> Result<Index, String> {
    let ip = ctx.o.work.join("gta5_ls_index.json");
    let arcs = archives(g);
    let stamp = super::sha(&arcs.iter().map(|a| fs::metadata(a).map(|m| format!("{}:{}:{:?}", a.display(), m.len(), m.modified().ok())).unwrap_or_default()).collect::<Vec<_>>().join("|"));
    if let Some(ix) = fs::read(&ip).ok().and_then(|b| serde_json::from_slice::<Index>(&b).ok()).filter(|ix| ix.tool == INDEX_V && ix.stamp == stamp) {
        return Ok(ix);
    }
    eprintln!("lsremove: indexing vanilla _citye/_cityw/_hills in {} (once; cached in {})", g.display(), ip.display());
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_default();
    let cwd = std::env::current_dir().unwrap_or_default();
    let magic = rpf7::find_magic(&[&ctx.o.work, &cwd, &exe_dir, &ctx.o.game]);
    let keys = rpf7::load_keys(g, magic.as_deref(), &ctx.o.work.join("gta5_keys.bin"))?;
    let files: Mutex<BTreeMap<String, (usize, Item)>> = Mutex::default();
    let failed = Mutex::new(0usize);
    arcs.par_iter().enumerate().try_for_each(|(ai, a)| -> Result<(), String> {
        let data = rpf7::map(a)?;
        let name = a.file_name().unwrap().to_string_lossy().to_lowercase();
        let mut ents = Vec::new();
        rpf7::walk(&keys, &data, 0, data.len() as u64, &name, "", &descend, &mut |e| {
            let ok = DIRS.iter().any(|d| e.path.trim_start_matches("x64/").starts_with(&format!("levels/gta5/{d}/")));
            if ok && e.resource && (e.name.ends_with(".ymap") || e.name.ends_with(".ybn")) {
                ents.push(rpf7::FileEntry { path: e.path.clone(), name: e.name.clone(), ..*e });
            }
        })?;
        let got: Vec<(String, Option<Item>)> = ents
            .par_iter()
            .map(|e| {
                let d = rpf7::resource_data(&data, e);
                let it = d.and_then(|d| if e.name.ends_with(".ymap") { ymap_info(&d) } else { ybn_info(&d) });
                (e.name.clone(), it)
            })
            .collect();
        let mut f = files.lock().unwrap();
        for (n, it) in got {
            match it {
                Some(it) => {
                    if f.get(&n).is_none_or(|(k, _)| *k <= ai) {
                        f.insert(n, (ai, it));
                    }
                }
                None => *failed.lock().unwrap() += 1,
            }
        }
        Ok(())
    })?;
    let files: BTreeMap<String, Item> = files.into_inner().unwrap().into_iter().map(|(n, (_, it))| (n, it)).collect();
    let nf = failed.into_inner().unwrap();
    eprintln!("lsremove: {} ymaps, {} ybns indexed ({nf} unreadable)", files.keys().filter(|n| n.ends_with(".ymap")).count(), files.keys().filter(|n| n.ends_with(".ybn")).count());
    let ix = Index { tool: INDEX_V.into(), stamp, files };
    fs::write(&ip, serde_json::to_vec(&ix).map_err(|e| e.to_string())?).map_err(|e| format!("{}: {e}", ip.display()))?;
    Ok(ix)
}

pub fn build(ctx: &Ctx) -> Result<(), String> {
    let Some(g) = std::env::var_os("GTA5_DIR").map(PathBuf::from).filter(|d| d.join("GTA5.exe").is_file()).or_else(gta5_dir) else {
        ctx.skip("lsremove", "GTA V install not found (set GTA5_DIR); Los Santos not removed", json!({}));
        return Ok(());
    };
    let ix = {
        let _p = Phase::new(ctx, "  lsremove index");
        match load_index(ctx, &g) {
            Ok(ix) => ix,
            Err(e) => {
                ctx.skip("lsremove", &format!("cannot read GTA V archives: {e}"), json!({}));
                return Ok(());
            }
        }
    };
    // region in GTA V space (output coordinates)
    let region = ctx.o.bbox.map(|b| [b[0] + ctx.o.offset[0], b[1] + ctx.o.offset[1], b[2] + ctx.o.offset[0], b[3] + ctx.o.offset[1]]);
    // a ymap's streaming extents cover everywhere it can be seen from, so
    // distant LOD/SLOD ymaps visible from the region are caught too
    let hit = |_: &str, it: &Item| region.is_none_or(|r| it.bx[1][0] >= r[0] && it.bx[0][0] <= r[2] && it.bx[1][1] >= r[1] && it.bx[0][1] <= r[3]);
    let mut chosen: std::collections::HashSet<&String> = ix.files.iter().filter(|(n, it)| hit(n, it)).map(|(n, _)| n).collect();
    // close over parent links: a vanilla child of a stubbed ymap is stubbed too
    let hash_of: BTreeMap<&String, u32> = ix.files.keys().filter(|n| n.ends_with(".ymap")).map(|n| (n, crate::gta5::rsc::joaat(n.trim_end_matches(".ymap")))).collect();
    let n_hit = chosen.len();
    loop {
        let stubbed: std::collections::HashSet<u32> = chosen.iter().filter_map(|n| hash_of.get(n).copied()).collect();
        let add: Vec<&String> = ix.files.iter().filter(|(n, it)| it.parent != 0 && !chosen.contains(n) && stubbed.contains(&it.parent)).map(|(n, _)| n).collect();
        if add.is_empty() {
            break;
        }
        chosen.extend(add);
    }
    if chosen.len() > n_hit {
        eprintln!("lsremove: {} more ymaps stubbed whose vanilla parent is stubbed", chosen.len() - n_hit);
    }
    let sel: Vec<(&String, &Item)> = ix.files.iter().filter(|(n, _)| chosen.contains(n)).collect();
    let ybn_stub = ybn::build_ybn(&[[0.0, 0.0, -900.0], [1.0, 0.0, -900.0], [0.0, 1.0, -900.0]], &[[0, 1, 2]], 0)?;
    let ybn_fp = format!("{STUB_V}|ybn");
    let off = ctx.o.offset;
    // write the shared ybn stub once, so every ybn links the same cache file
    let first = sel.iter().find(|(n, _)| n.ends_with(".ybn")).map(|(n, _)| n.as_str());
    if let Some(n) = first {
        match ctx.up_to_date(n, &ybn_fp) {
            Some(info) => ctx.emit(n, &ybn_fp, info, None)?,
            None => ctx.emit(n, &ybn_fp, json!({"stub": true}), Some(ybn_stub.clone()))?,
        };
    }
    let res: Vec<Result<(), String>> = sel
        .par_iter()
        .map(|(n, it)| {
            if n.ends_with(".ybn") {
                if Some(n.as_str()) == first {
                    return Ok(());
                }
                if ctx.up_to_date(n, &ybn_fp).is_none() {
                    let src = ctx.stream.join(first.unwrap());
                    super::link_into(&src, &ctx.stream.join(n)).map_err(|e| format!("{n}: {e}"))?;
                }
                return ctx.emit(n, &ybn_fp, json!({"stub": true}), None).map(|_| ());
            }
            let fp = format!("{STUB_V}|ymap|{n}|{}|{}|{:?}|{:?}|{:08x}", it.flags, it.cflags, it.bx, it.ee, it.parent);
            if let Some(info) = ctx.up_to_date(n, &fp) {
                return ctx.emit(n, &fp, info, None).map(|_| ());
            }
            // meta::build_ymap adds --offset to extents; these are GTA V space already
            let un = |b: [[f32; 3]; 2]| b.map(|v| std::array::from_fn(|k| v[k] - off[k]));
            let stem = n.trim_end_matches(".ymap");
            let map: meta::MapData = serde_json::from_value(json!({"name": stem, "parent": if it.parent == 0 { String::new() } else { format!("hash:{:08x}", it.parent) }, "flags": it.flags, "content_flags": it.cflags, "exported_by": "6ixpack lsremove"})).map_err(|e| e.to_string())?;
            let map = meta::MapData { streaming_extents: Some(un(it.bx)), entities_extents: Some(un(it.ee.unwrap_or(it.bx))), ..map };
            ctx.emit(n, &fp, json!({"stub": true}), Some(meta::build_ymap(&map)?)).map(|_| ())
        })
        .collect();
    for r in res {
        r?;
    }
    let ny = sel.iter().filter(|(n, _)| n.ends_with(".ymap")).count();
    eprintln!("lsremove: {ny} ymap and {} ybn stand-ins (of {} vanilla files){}", sel.len() - ny, ix.files.len(), if region.is_some() { " seen from the region" } else { "" });
    ctx.report.lock().unwrap().outputs.push(json!({"file": "(lsremove)", "status": "stubs", "ymaps": ny, "ybns": sel.len() - ny, "vanilla": ix.files.len()}));
    Ok(())
}

/// `6ixpack fivem gta5 <substr> [--out <dir>]`: list (and optionally extract,
/// inflated) GTA V archive files whose path contains `substr`; a debugging aid
/// for vanilla names (minimap tiles, gtxd parents, ...).
pub fn gta5_cli(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let pat = args.first().ok_or("usage: 6ixpack fivem gta5 <path substring> [--out <dir>] [--work <dir>]")?.to_lowercase();
    let mut out: Option<PathBuf> = None;
    let mut work: PathBuf = std::env::var_os("SIXNG_WORK").map(PathBuf::from).unwrap_or_else(|| "work".into());
    let mut it = args[1..].iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--out" => out = it.next().map(PathBuf::from),
            "--work" => work = it.next().map(PathBuf::from).ok_or("--work needs a value")?,
            _ => return Err(format!("unknown argument {a}").into()),
        }
    }
    let g = gta5_dir().ok_or("GTA V install not found (set GTA5_DIR)")?;
    fs::create_dir_all(&work)?;
    let cwd = std::env::current_dir().unwrap_or_default();
    let keys = rpf7::load_keys(&g, rpf7::find_magic(&[&work, &cwd]).as_deref(), &work.join("gta5_keys.bin"))?;
    let mut arcs = archives(&g);
    arcs.insert(0, g.join("common.rpf"));
    for a in arcs {
        let data = rpf7::map(&a)?;
        let name = a.file_name().unwrap().to_string_lossy().to_lowercase();
        let pre = a.strip_prefix(&g).unwrap_or(&a).to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/").to_lowercase() + "/";
        rpf7::walk(&keys, &data, 0, data.len() as u64, &name, &pre, &|_| true, &mut |e| {
            if e.path.contains(&pat) {
                println!("{}\t{}", e.path, e.size.max(e.usize));
                if let Some(o) = &out {
                    let b = if e.resource { rpf7::resource_data(&data, e) } else { rpf7::binary_data(&keys, &data, e) };
                    if let Some(b) = b {
                        let _ = fs::create_dir_all(o);
                        let _ = fs::write(o.join(&e.name), b);
                    }
                }
            }
        })?;
    }
    Ok(())
}
