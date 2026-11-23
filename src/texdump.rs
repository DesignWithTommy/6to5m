//! `6ixpack colour-textures`: every GTA VI colour texture as a top-mip PNG.
//!
//! Walks the drawables (dr / ft / dd) of every RPF9 archive straight from the
//! archive, reads each shader's texture slots and decodes the colour ones from
//! the TTPF stores (BCPack included). Which slots are colour comes from the
//! shader parameter hash (sixng_core::material::SLOTS) and, for slots no
//! table names, the texture's DXGI format (sRGB = colour). Normal, packed
//! ORM / specular, mask, dirt, palette and other linear data slots are
//! skipped. Alpha is kept only where it is opacity: the colour-with-opacity
//! slot, or a base / unnamed sRGB colour slot on a shader whose render bucket
//! draws alpha (alpha_mode MASK / BLEND). Everything else is written RGB, so
//! height or other data packed in A never reaches the PNG.
//!
//! GTA VI textures carry no names (TTPF stores key them by 64-bit hash), so a
//! file is `<archive>/<model>/<param name or hash>_<hash64>.png`, written
//! once per hash64 under the first model that binds it.

use rayon::prelude::*;
use sixng_core::{classify::Kind, export::Workspace};
use std::{
    collections::HashSet,
    error::Error,
    ffi::OsString,
    fs,
    io::BufWriter,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    },
};

const USAGE: &str = "usage: 6ixpack colour-textures --game <6m dir> [--out <dir>] [--work <dir>] [--archive <id>]
  --out      output folder (default: C:\\Users\\Desig\\Downloads\\gta 6 textures)
  --work     sixng work dir holding the archive indexes (default: work, or SIXNG_WORK)
  --archive  only this archive (id or path fragment, repeatable)
  --min-size skip textures whose decoded top mip is narrower or shorter than this (default 600)";

const BASE: &str = "c8e8c282";
const DETAIL: &str = "0df47048";
const WITH_OPACITY: &str = "7485dafb";
const EMISSIVE: &str = "8e0d63ee";

/// Engine-default white (in no store).
const ENGINE_WHITE: u64 = 0x1e2c_9340_e14a_7d07;

fn srgb(dxgi: u32) -> bool {
    matches!(dxgi, 29 | 72 | 75 | 78 | 91 | 99)
}

/// Whether a slot is a colour image, and whether its alpha is opacity.
/// None: not colour.
fn colour_slot(param: &str, dxgi: u32, alpha_opacity: bool) -> Option<bool> {
    match param {
        WITH_OPACITY => Some(true),
        BASE => Some(alpha_opacity),
        DETAIL | EMISSIVE => Some(false),
        p if sixng_core::material::slot_role(p).is_some() => None,
        _ if srgb(dxgi) => Some(alpha_opacity),
        _ => None,
    }
}

fn sanitize(s: &str) -> String {
    let s: String = s.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') { c } else { '_' }).collect();
    let s = s.trim_matches('.').to_string();
    if s.is_empty() { "_".into() } else { s }
}

fn write_png(path: &Path, rgba: &[u8], w: u32, h: u32, alpha: bool) -> Result<(), String> {
    let f = fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut enc = png::Encoder::new(BufWriter::new(f), w, h);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_color(if alpha { png::ColorType::Rgba } else { png::ColorType::Rgb });
    enc.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
    let mut wr = enc.write_header().map_err(|e| e.to_string())?;
    if alpha {
        wr.write_image_data(rgba)
    } else {
        let rgb: Vec<u8> = rgba.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
        wr.write_image_data(&rgb)
    }
    .map_err(|e| e.to_string())
}

pub fn run(args: &[OsString]) -> Result<(), Box<dyn Error>> {
    let mut game = std::env::var_os("SIXNG_GAME").map(PathBuf::from);
    let mut out = PathBuf::from(r"C:\Users\Desig\Downloads\gta 6 textures");
    let mut work = std::env::var_os("SIXNG_WORK").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("work"));
    let mut only: Vec<String> = Vec::new();
    let mut min = 600u32;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().ok_or_else(|| format!("{} needs a value\n{USAGE}", a.to_string_lossy()));
        match a.to_str() {
            Some("--game") => game = Some(PathBuf::from(val()?)),
            Some("--out") => out = PathBuf::from(val()?),
            Some("--work") => work = PathBuf::from(val()?),
            Some("--min-size") => min = val()?.to_str().and_then(|v| v.parse().ok()).ok_or("--min-size needs a number")?,
            Some("--archive") => only.push(val()?.to_string_lossy().into_owned()),
            Some("-h" | "--help") => {
                println!("{USAGE}");
                return Ok(());
            }
            _ => return Err(format!("colour-textures: unknown argument {}\n{USAGE}", a.to_string_lossy()).into()),
        }
    }
    let game = game.ok_or(USAGE)?;
    let ws = Workspace::new(&game, &work);
    let set = sixng_core::ttpf::global(&game);
    if set.stores.is_empty() {
        return Err(format!("no TTPF stores under {}", game.display()).into());
    }
    let seen: Mutex<HashSet<u64>> = Mutex::new(HashSet::from([ENGINE_WHITE]));
    let (written, rgba_n, skipped_data, missing, failed) = (AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0));
    let small = AtomicUsize::new(0);
    let quiet: sixng_core::export::Progress = sixng_core::export::no_progress();
    for a in ws.archives().into_iter().filter(|a| a.container == "rpf9") {
        if !only.is_empty() && !only.iter().any(|o| a.id.contains(o.as_str())) {
            continue;
        }
        let ix = match ws.ensure_index(&a, false, true, quiet.clone()) {
            Ok(ix) => ix,
            Err(e) => {
                eprintln!("[colour-textures] {}: index: {e}", a.id);
                continue;
            }
        };
        let entries: Vec<_> = ix.entries.iter().filter(|e| e.kind == Kind::RageObj && matches!(e.rtype.as_deref(), Some("dr" | "ft" | "dd"))).collect();
        let before = written.load(Ordering::Relaxed);
        let adir = out.join(a.id.trim_end_matches(".rpf").split('/').map(sanitize).collect::<PathBuf>());
        entries.par_iter().for_each(|e| {
            let Ok(r) = sixng_core::resource::Resource::load_sys(&a.path, e) else { return };
            let model = sanitize(&e.name.clone().unwrap_or_else(|| e.id.rsplit('@').next().unwrap_or(&e.id).to_string()));
            for s in sixng_core::drawable::find_shaders(&r) {
                let alpha_mode = sixng_core::material::classify(&s.name_hash, &s.flags)["alpha_mode"].as_str().unwrap_or("OPAQUE").to_string();
                for t in &s.textures {
                    let Ok(h) = u64::from_str_radix(&t.hash64, 16) else { continue };
                    let Some((store, tx)) = set.get(h) else {
                        if !seen.lock().unwrap().contains(&h) {
                            missing.fetch_add(1, Ordering::Relaxed);
                        }
                        continue;
                    };
                    let Some(alpha) = colour_slot(&t.param_hash, tx.dxgi_format, alpha_mode != "OPAQUE") else {
                        skipped_data.fetch_add(1, Ordering::Relaxed);
                        continue;
                    };
                    // Low-res copies (LOD bindings, small variants) are other
                    // hash64s whose stored top mip is below the minimum.
                    if tx.width < min || tx.height < min {
                        small.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                    if !seen.lock().unwrap().insert(h) {
                        continue;
                    }
                    let res = (|| -> Result<(), String> {
                        if !tx.decodable() {
                            return Err(format!("format {} / tile mode {} not decodable", tx.dxgi_format, tx.tile_mode()));
                        }
                        let (data, skipped) = store.data_ex(tx).map_err(|e| e.to_string())?;
                        let mip = if skipped == 0 { 0 } else { store.best_mip(tx).ok_or("only BCPack mips")? };
                        let (rgba, w, hh) = sixng_core::ttpf::decode_rgba(tx, &data, mip).map_err(|e| e.to_string())?;
                        if w < min || hh < min {
                            return Err(format!("only a {w}x{hh} mip decodes (top mips BCPack), below --min-size"));
                        }
                        let dir = adir.join(&model);
                        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                        let slot = sixng_core::names::param(&t.param_hash).unwrap_or_else(|| t.param_hash.clone());
                        write_png(&dir.join(format!("{}_{:016x}.png", sanitize(&slot), h)), &rgba, w, hh, alpha)
                    })();
                    match res {
                        Ok(()) => {
                            written.fetch_add(1, Ordering::Relaxed);
                            if alpha {
                                rgba_n.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        Err(err) => {
                            failed.fetch_add(1, Ordering::Relaxed);
                            eprintln!("[colour-textures] {} {:016x}: {err}", e.id, h);
                        }
                    }
                }
            }
        });
        println!("[colour-textures] {}: {} drawables, {} new PNGs", a.id, entries.len(), written.load(Ordering::Relaxed) - before);
    }
    println!(
        "[colour-textures] wrote {} PNGs ({} with opacity alpha) to {}; {} data-slot bindings skipped, {} bindings not in any store, {} below {min} px, {} failed",
        written.into_inner(),
        rgba_n.into_inner(),
        out.display(),
        skipped_data.into_inner(),
        missing.into_inner(),
        small.into_inner(),
        failed.into_inner()
    );
    Ok(())
}
