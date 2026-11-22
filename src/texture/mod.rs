//! `rpf9 texture`: list, extract and replace RAGE textures in RPF9 archives
//! (carved zlib texture dictionaries) and RDS5/ETPF streaming pools (.ttpf).
//!
//! Other modules (e.g. the model decoder) can read a drawable's embedded
//! dictionary with [`embedded_dictionary`] / [`embedded_at`] and export its
//! textures with [`create_dds`] / [`decode_preview`].

mod catalog;
mod dds;
mod defaults;
mod format;
mod rage;
mod recover;
mod ttpf;

#[allow(unused_imports)]
pub use rage::{Embedded, TextureInfo, create_dds, decode_preview, embedded_at, embedded_dictionary};

use memmap2::Mmap;
use std::{
    error::Error,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

const USAGE: &str = "\
usage: 6ixpack texture list    [options] <rpf|ttpf>
       6ixpack texture extract [options] <rpf|ttpf> <outdir>
       6ixpack texture replace <rpf> <dictionary-offset> <index> <dds> <out.r9texdata>
       6ixpack texture replace <ttpf> <index> <dds> <out.ttpf>
       6ixpack texture catalog <pool-dir> [asset-key-hex]
       6ixpack texture topng   <dds> <png>

options (rpf only):
  --quick       scan only 16 MiB after the archive metadata, at most 32 dictionaries
  --no-cache    ignore and do not write the dictionary index cache
  --threads N   worker threads (default: all cores)
  --pools DIR   resolve keyed textures against the .ttpf pools under DIR
                (default: <content root>/data/ttdb found above the archive)

extract writes one .dds per texture plus a .png of the preview mip where the
format decodes (BC1-BC7 incl. signed BC4/BC5, RGBA/BGRA, R8/A8, R16, floats
clipped to 0-1). replace never modifies its inputs: an RPF dictionary is
staged as system+graphics (.r9texdata); a pool is written as a new verified
RDS5 copy.";

struct Options {
    quick: bool,
    cache: bool,
    pools: Option<PathBuf>,
}

pub fn run(args: &[OsString]) -> Result<(), Box<dyn Error>> {
    let cmd = args.first().and_then(|a| a.to_str()).unwrap_or("--help");
    let mut options = Options { quick: false, cache: true, pools: None };
    let mut rest = Vec::new();
    let mut it = args.iter().skip(1);
    while let Some(arg) = it.next() {
        match arg.to_str() {
            Some("--quick") => options.quick = true,
            Some("--no-cache") => options.cache = false,
            Some("--pools") => options.pools = Some(it.next().ok_or("--pools needs a directory")?.into()),
            _ => rest.push(PathBuf::from(arg)),
        }
    }
    match (cmd, rest.as_slice()) {
        ("list", [file]) => with_source(file, None, &options),
        ("extract", [file, out]) => with_source(file, Some(out), &options),
        ("replace", [file, index, dds, out]) => replace_pool(file, index, dds, out),
        ("replace", [file, offset, index, dds, out]) => replace_dictionary(file, offset, index, dds, out, &options),
        ("catalog", [dir]) => catalog_cmd(dir, None),
        ("catalog", [dir, key]) => catalog_cmd(dir, Some(key)),
        ("topng", [dds, png]) => topng(dds, png),
        ("-h" | "--help", _) => {
            println!("{USAGE}");
            Ok(())
        }
        _ => Err(USAGE.into()),
    }
}

fn map(file: &Path) -> Result<Mmap, Box<dyn Error>> {
    let handle = fs::File::open(file).map_err(|e| format!("{}: {e}", file.display()))?;
    // SAFETY: read-only map; the source is not expected to change while read.
    Ok(unsafe { Mmap::map(&handle) }?)
}

fn number(p: &Path) -> Result<usize, Box<dyn Error>> {
    let s = p.to_string_lossy();
    let parsed = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => usize::from_str_radix(hex, 16),
        None => s.parse(),
    };
    Ok(parsed.map_err(|_| format!("not a number: {s}"))?)
}

fn new_output(source: &Path, out: &Path) -> Result<(), Box<dyn Error>> {
    if out.exists() {
        return Err(format!("{} already exists; the output must be a new file", out.display()).into());
    }
    if fs::canonicalize(source)? == std::path::absolute(out)? {
        return Err("the output cannot be the source".into());
    }
    Ok(())
}

fn topng(src: &Path, dst: &Path) -> Result<(), Box<dyn Error>> {
    let data = fs::read(src)?;
    let (info, w, h, rgba) = dds::decode_preview(&data)?;
    format::write_png(dst, w, h, &rgba)?;
    println!(
        "{}: {}x{}x{} dim {} slices {} mips {} {} -> {} ({w}x{h})",
        src.display(),
        info.width,
        info.height,
        info.depth,
        info.dimension,
        info.slices,
        info.mips,
        format::name(info.format),
        dst.display()
    );
    Ok(())
}

fn catalog_cmd(dir: &Path, key: Option<&PathBuf>) -> Result<(), Box<dyn Error>> {
    let catalog = catalog::Catalog::build(dir)?;
    println!("{} pools inspected, {} skipped, {} asset keys", catalog.inspected, catalog.skipped, catalog.keys());
    if let Some(key) = key {
        let text = key.to_string_lossy();
        let key = u64::from_str_radix(text.trim_start_matches("0x").trim_start_matches("texture_"), 16)?;
        let found = catalog.find(key);
        if found.is_empty() {
            println!("{key:016X}: not found");
        }
        for l in found {
            let state = if l.extractable { format!("mips {}..{}", l.base_mip, l.base_mip + l.mips) } else { "unavailable".into() };
            println!("{key:016X}: {} #{} {} {state}", l.path.display(), l.index, l.name);
        }
    }
    Ok(())
}

fn replace_pool(file: &Path, index: &Path, dds: &Path, out: &Path) -> Result<(), Box<dyn Error>> {
    new_output(file, out)?;
    let data = map(file)?;
    let pool = ttpf::Pool::open(&data)?;
    let index = number(index)?;
    let replacement = fs::read(dds)?;
    let r = pool.write_replacement(index, &replacement, out)?;
    println!(
        "wrote {} ({} bytes from {}): texture #{index} replaced, {} chunks / {} decoded bytes changed, {} unaffected samples verified",
        out.display(),
        r.output_bytes,
        r.source_bytes,
        r.changed_chunks,
        r.changed_bytes,
        r.samples_verified
    );
    println!("note: ETPF metadata and asset keys are kept; game compatibility is not verified");
    Ok(())
}

fn replace_dictionary(
    file: &Path,
    offset: &Path,
    index: &Path,
    dds: &Path,
    out: &Path,
    _: &Options,
) -> Result<(), Box<dyn Error>> {
    new_output(file, out)?;
    let data = map(file)?;
    let offset = number(offset)?;
    let d = recover::recover_cached(&data, offset).ok_or("no texture dictionary at that offset")?;
    let index = number(index)?;
    let replacement = fs::read(dds)?;
    if d.system.len() + d.graphics.len() > rage::MAX_GRAPHICS_BYTES {
        return Err("decoded resource exceeds the 256MiB staging limit".into());
    }
    let staged = rage::retile_replacement(&d.system, &d.graphics, index, &replacement)?;
    let changed = staged.iter().zip(&d.graphics).filter(|(a, b)| a != b).count();
    let mut bundle = d.system.clone();
    bundle.extend_from_slice(&staged);
    fs::write(out, &bundle)?;
    println!(
        "wrote {}: {} system + {} graphics bytes; {} ({}) replaced, {changed} graphics bytes changed",
        out.display(),
        d.system.len(),
        staged.len(),
        d.textures[index].name,
        format::name(d.textures[index].format)
    );
    println!("graphics sha256 {} -> {}", defaults::sha256_hex(&d.graphics), defaults::sha256_hex(&staged));
    println!("note: the archive is not rewritten; this is the decoded resource with the texture retiled");
    Ok(())
}

fn with_source(file: &Path, out: Option<&PathBuf>, options: &Options) -> Result<(), Box<dyn Error>> {
    let data = map(file)?;
    if let Some(out) = out {
        fs::create_dir_all(out)?;
    }
    if ttpf::is_pool(&data) { pool(&data, out) } else { archive(file, &data, out, options) }
}

fn sanitize(name: &str) -> String {
    name.chars().map(|c| if c.is_alphanumeric() || "-_.".contains(c) { c } else { '_' }).collect()
}

struct Tally {
    textures: usize,
    dds: usize,
    png: usize,
    failed: usize,
}

impl Tally {
    fn save(&mut self, out: &Path, stem: &str, dds: Result<Vec<u8>, String>, png: impl FnOnce() -> format::R<(u32, u32, Vec<u8>)>) {
        self.count(stem, save(out, stem, dds, png));
    }

    fn count(&mut self, stem: &str, saved: Result<bool, String>) {
        match saved {
            Ok(png) => {
                self.dds += 1;
                self.png += usize::from(png);
            }
            Err(e) => {
                self.failed += 1;
                eprintln!("  {stem}: dds: {e}");
            }
        }
    }
}

/// Write `<stem>.dds` and, where it decodes, `<stem>.png`; Ok(png written).
fn save(out: &Path, stem: &str, dds: Result<Vec<u8>, String>, png: impl FnOnce() -> format::R<(u32, u32, Vec<u8>)>) -> Result<bool, String> {
    dds.and_then(|bytes| fs::write(out.join(format!("{stem}.dds")), bytes).map_err(|e| e.to_string()))?;
    Ok(png().is_ok_and(|(w, h, rgba)| format::write_png(&out.join(format!("{stem}.png")), w, h, &rgba).is_ok()))
}

fn dictionaries(file: &Path, data: &[u8], options: &Options) -> (Vec<recover::Dictionary>, &'static str) {
    let mode = if options.quick { "quick" } else { "full" };
    if options.cache
        && let Some(offsets) = recover::cache::load(file, data, mode)
    {
        let found: Vec<_> = offsets.iter().filter_map(|&at| recover::recover_cached(data, at)).collect();
        if found.len() == offsets.len() {
            return (found, "index cache");
        }
    }
    let found = if options.quick { recover::recover_quick(data) } else { recover::recover_all(data) };
    if options.cache {
        let offsets: Vec<_> = found.iter().map(|d| d.offset).collect();
        recover::cache::save(file, data, mode, &offsets);
    }
    (found, if options.quick { "quick scan (first 16 MiB, at most 32)" } else { "full scan" })
}

fn archive(file: &Path, data: &[u8], out: Option<&PathBuf>, options: &Options) -> Result<(), Box<dyn Error>> {
    let (dictionaries, coverage) = dictionaries(file, data, options);
    let keyed = dictionaries.iter().flat_map(|d| &d.textures).any(|t| t.asset_key.is_some());
    let pool_dir = options
        .pools
        .clone()
        .or_else(|| catalog::find_content_dir(file).map(|root| root.join("data").join("ttdb")));
    let catalog = match (&pool_dir, keyed) {
        (Some(dir), true) => catalog::Catalog::build(dir).ok(),
        _ => None,
    };
    let mut tally = Tally { textures: 0, dds: 0, png: 0, failed: 0 };
    let dirs: Vec<_> = dictionaries.iter().map(|d| out.map(|o| o.join(format!("dictionary_{:X}", d.offset)))).collect();
    for dir in dirs.iter().flatten() {
        fs::create_dir_all(dir)?;
    }
    // Textures are described, converted and written in parallel; lines are
    // printed in dictionary/texture order as the sequential loop did.
    let jobs: Vec<(usize, usize)> =
        dictionaries.iter().enumerate().flat_map(|(di, d)| (0..d.textures.len()).map(move |ti| (di, ti))).collect();
    let header = |d: &recover::Dictionary| {
        println!(
            "dictionary @0x{:X}: packed {} system {} graphics {} textures {}",
            d.offset,
            d.packed,
            d.system.len(),
            d.graphics.len(),
            d.textures.len()
        );
    };
    let mut headed = 0;
    crate::par::ordered(
        jobs.len(),
        crate::par::threads() * 4,
        |j| {
            let (di, i) = jobs[j];
            let (d, t) = (&dictionaries[di], &dictionaries[di].textures[i]);
            let mut note = String::new();
            if let Some(p) = defaults::match_metadata(t) {
                let verified = rage::decode_preview(t, &d.graphics)
                    .is_ok_and(|(w, h, rgba)| defaults::matches_pixels(p, w, h, &rgba));
                note = format!(" [resident default {}, pixels {}]", p.name, if verified { "verified" } else { "differ" });
            } else if let Some(p) = t.asset_key.and_then(|k| defaults::find(k, t.name_hash)) {
                note = format!(" [resident default key {}]", p.name);
            }
            if let (Some(key), Some(catalog)) = (t.asset_key, &catalog) {
                match catalog.find(key).first() {
                    Some(l) => note.push_str(&format!(" -> {}#{}", l.path.display(), l.index)),
                    None => note.push_str(" -> not in pools"),
                }
            }
            let line = format!(
                "  {i:4} {:<40} {:>5}x{:<5} d{} dim{} mips {:>2} {:<20} hash {:08X}{note}",
                t.name,
                t.width,
                t.height,
                t.depth,
                t.dimension,
                t.mips,
                format::name(t.format),
                t.name_hash,
            );
            let saved = dirs[di].as_ref().map(|dir| {
                let stem = format!("{i:04}_{}", sanitize(&t.name));
                let saved = save(dir, &stem, rage::create_dds(t, &d.graphics), || rage::decode_preview(t, &d.graphics));
                (stem, saved)
            });
            (line, saved)
        },
        |j, (line, saved)| {
            let di = jobs[j].0;
            while headed <= di {
                header(&dictionaries[headed]);
                headed += 1;
            }
            tally.textures += 1;
            println!("{line}");
            if let Some((stem, saved)) = saved {
                tally.count(&stem, saved);
            }
            true
        },
    );
    for d in &dictionaries[headed..] {
        header(d);
    }
    println!("\n{} dictionaries, {} textures ({coverage})", dictionaries.len(), tally.textures);
    if let Some(c) = &catalog {
        println!("pool catalog: {} pools, {} asset keys", c.inspected, c.keys());
    }
    if let Some(out) = out {
        println!("wrote {} dds, {} png to {} ({} failed)", tally.dds, tally.png, out.display(), tally.failed);
    }
    Ok(())
}

fn pool(data: &[u8], out: Option<&PathBuf>) -> Result<(), Box<dyn Error>> {
    let pool = ttpf::Pool::open(data)?;
    let mut tally = Tally { textures: 0, dds: 0, png: 0, failed: 0 };
    let mut unavailable = 0;
    for t in &pool.textures {
        tally.textures += 1;
        let state = match &t.limitation {
            Some(why) => {
                unavailable += 1;
                format!("unavailable: {why}")
            }
            None => format!("mips {}..{} of {}, dds {}", t.base_mip, t.base_mip + t.mips, t.source_mips, t.dds_size),
        };
        println!(
            "{} {:>5}x{:<5} d{} dim{} {:<20} key {:016X} {state}",
            t.name,
            t.width,
            t.height,
            t.depth,
            t.dimension,
            format::name(t.format),
            t.asset_key
        );
        if let Some(out) = out
            && t.limitation.is_none()
        {
            tally.save(out, &t.name, pool.create_dds(t), || pool.decode_preview(t));
        }
    }
    println!(
        "\nRDS5/ETPF634 pool: virtual {} bytes, {} textures, {} extractable",
        pool.virtual_size,
        tally.textures,
        tally.textures - unavailable
    );
    if let Some(out) = out {
        println!("wrote {} dds, {} png to {} ({} failed)", tally.dds, tally.png, out.display(), tally.failed);
    }
    Ok(())
}

// ---------------------------------------------------------------- GUI API

pub use ttpf::{Pool, Texture as PoolTexture, is_pool};
pub use catalog::{Catalog, Location, find_content_dir};

/// Format name for a DXGI-style format code.
pub fn format_name(format: u32) -> String {
    format::name(format)
}

/// Write RGBA8 pixels as PNG.
pub fn write_png(path: &Path, w: u32, h: u32, rgba: &[u8]) -> Result<(), Box<dyn Error>> {
    Ok(format::write_png(path, w, h, rgba)?)
}

/// Decode a DDS file's preview mip to RGBA8.
pub fn dds_preview(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), Box<dyn Error>> {
    let (_, w, h, rgba) = dds::decode_preview(bytes)?;
    Ok((w, h, rgba))
}

/// A recovered dictionary without its (large) decoded buffers.
pub struct DictionaryIndex {
    pub offset: usize,
    pub textures: Vec<TextureInfo>,
}

/// Full-mode dictionary index of an archive, sharing the CLI's index cache.
/// Decoded buffers are dropped as it goes; the cache is not written when the
/// scan was cancelled through [`crate::progress`].
pub fn index_archive(file: &Path, data: &[u8]) -> Vec<DictionaryIndex> {
    if let Some(offsets) = recover::cache::load(file, data, "full") {
        let mut found = Vec::new();
        for &at in &offsets {
            if crate::progress::at(at) {
                return found;
            }
            match recover::recover_cached(data, at) {
                Some(d) => found.push(DictionaryIndex { offset: d.offset, textures: d.textures }),
                None => break,
            }
        }
        if found.len() == offsets.len() {
            return found;
        }
    }
    let mut out: Vec<(usize, usize, Vec<TextureInfo>)> = Vec::new();
    // Decoded buffers are dropped as each dictionary is handed over.
    recover::recover_each(data, &recover::candidates(data), |d| {
        out.push((d.offset, d.packed, d.textures));
        !crate::progress::at(d.offset)
    });
    if !crate::progress::cancelled() {
        let offsets: Vec<_> = out.iter().map(|d| d.0).collect();
        recover::cache::save(file, data, "full", &offsets);
    }
    out.into_iter().map(|(offset, _, textures)| DictionaryIndex { offset, textures }).collect()
}

/// Decoded graphics buffer and textures of the dictionary at `offset`.
pub fn load_dictionary(data: &[u8], offset: usize) -> Option<(Vec<u8>, Vec<TextureInfo>)> {
    recover::recover_cached(data, offset).map(|d| (d.graphics, d.textures))
}

/// Replace texture `index` of the dictionary at `offset`; writes the staged
/// system+graphics bundle to the new file `out` (the archive is untouched).
pub fn replace_in_dictionary(source: &Path, offset: usize, index: usize, dds: &Path, out: &Path) -> Result<(), Box<dyn Error>> {
    let options = Options { quick: false, cache: true, pools: None };
    replace_dictionary(source, Path::new(&offset.to_string()), Path::new(&index.to_string()), dds, out, &options)
}

/// Replace texture `index` of a .ttpf pool, writing a new verified pool copy to `out`.
pub fn replace_in_pool(source: &Path, index: usize, dds: &Path, out: &Path) -> Result<(), Box<dyn Error>> {
    replace_pool(source, Path::new(&index.to_string()), dds, out)
}
