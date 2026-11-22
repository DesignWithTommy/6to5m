//! `rpf9 model`: recover RAGE drawables from RPF9 archives and export OBJ.
//! Port of rpf9-explorer Rpf9ModelRecovery / RageModelDecoder / RageSkeleton /
//! ModelTextureBindings / ObjModelReader / PsinMetadata /
//! Rpf9StructuredRecovery and OpenRPF9 Rsc9.

mod decode;
mod obj;
mod psin;
mod rsc;
mod skeleton;
mod structured;
mod texture;

use crate::zlib::Inflater;
use decode::{MAX_GRAPHICS, MAX_SYSTEM, Model};
use flate2::{Decompress, FlushDecompress, Status};
use memmap2::Mmap;
use std::{
    collections::HashMap,
    error::Error,
    ffi::OsString,
    fmt::Write as _,
    fs::{self, File},
    path::{Path, PathBuf},
};

const USAGE: &str = "\
usage: 6ixpack model list     <rpf> [options]
       6ixpack model export   <rpf> <outdir> [options]
       6ixpack model textures <rpf> [options]     texture dictionary names found
       6ixpack model meta     <rpf> [outdir]      PSIN / XML metadata after the tables
       6ixpack model obj      <in.obj> [out.obj]  import (validate/triangulate) an OBJ
       6ixpack model rsc      <file> <outdir>     decode a standalone RSC7 drawable

  --skeleton        list: show joint counts; export: also write <name>.skel.txt
  --pools <dir>     export: ttdb pool directory for keyed embedded textures
                    (default: data/ttdb above the archive)
  --textures <rpf>  export: also take texture names from this archive (repeatable)
  --start <offset>  begin scanning at this byte offset (hex with 0x)
  --len <bytes>     scan only this many bytes
  --limit <n>       stop after n model resources
  --threads <n>     worker threads (default: all cores)
  --no-cache        ignore and rebuild the saved walk index (%LOCALAPPDATA%/rpf9/models)";

const PAGE: usize = 4096;
const MAX_BLOCKS: usize = 4096;
const RAW_SYSTEM_TAIL: usize = 16;
const RAW_GRAPHICS_TAIL: usize = 48;

struct Options {
    skeleton: bool,
    start: usize,
    len: Option<usize>,
    limit: usize,
    textures: Vec<PathBuf>,
    pools: Option<PathBuf>,
    /// Archive path for the walk cache; None disables it.
    source: Option<PathBuf>,
    cache: bool,
}

impl Options {
    fn whole() -> Self {
        Self { skeleton: false, start: 0, len: None, limit: usize::MAX, textures: Vec::new(), pools: None, source: None, cache: true }
    }
}

fn number(text: Option<&OsString>) -> Result<usize, Box<dyn Error>> {
    let text = text.and_then(|t| t.to_str()).ok_or(USAGE)?;
    let parsed = match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        Some(hex) => usize::from_str_radix(hex, 16),
        None => text.parse(),
    };
    Ok(parsed.map_err(|e| format!("{text}: {e}"))?)
}

fn open(path: &Path) -> Result<Mmap, Box<dyn Error>> {
    // SAFETY: the map is only read; nothing should write the archive meanwhile.
    let map = File::open(path).and_then(|f| unsafe { Mmap::map(&f) });
    Ok(map.map_err(|e| format!("{}: {e}", path.display()))?)
}

pub fn run(args: &[OsString]) -> Result<(), Box<dyn Error>> {
    let mut positional = Vec::new();
    let mut options = Options::whole();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.to_str() {
            Some("--skeleton") => options.skeleton = true,
            Some("--no-cache") => options.cache = false,
            Some("--start") => options.start = number(it.next())?,
            Some("--len") => options.len = Some(number(it.next())?),
            Some("--limit") => options.limit = number(it.next())?,
            Some("--pools") => options.pools = Some(it.next().ok_or(USAGE)?.into()),
            Some("--textures") => options.textures.push(it.next().ok_or(USAGE)?.into()),
            Some("-h" | "--help") => {
                println!("{USAGE}");
                return Ok(());
            }
            Some(o) if o.starts_with("--") => return Err(format!("unknown option {o}\n{USAGE}").into()),
            _ => positional.push(PathBuf::from(arg)),
        }
    }
    if let Some(rpf) = positional.get(1) {
        options.source = Some(rpf.clone());
    }
    let command = positional.first().and_then(|c| c.to_str()).unwrap_or("").to_owned();
    let rest = positional.get(1..).unwrap_or(&[]);
    match (command.as_str(), rest) {
        ("list", [rpf]) => scan_models(&open(rpf)?, &options, None),
        ("export", [rpf, out]) => {
            if options.pools.is_none() {
                options.pools = crate::texture::find_content_dir(rpf).map(|root| root.join("data").join("ttdb"));
            }
            scan_models(&open(rpf)?, &options, Some(out))
        }
        ("textures", [rpf]) => list_textures(&open(rpf)?, &options),
        ("meta", [rpf]) => meta(&open(rpf)?, None),
        ("meta", [rpf, out]) => meta(&open(rpf)?, Some(out)),
        ("obj", [input]) => import_obj(input, None),
        ("obj", [input, out]) => import_obj(input, Some(out)),
        ("rsc", [file, out]) => standalone(file, out, &options),
        _ => Err(USAGE.into()),
    }
}

mod walk_cache;

// ---------------------------------------------------------------- commands

enum Candidate {
    Model,
    Textures,
}

struct Names {
    map: HashMap<u32, String>,
    dictionaries: usize,
    named: usize,
    joaat_matches: usize,
}

impl Names {
    fn new() -> Self {
        Self { map: HashMap::new(), dictionaries: 0, named: 0, joaat_matches: 0 }
    }
    fn add(&mut self, names: Vec<(u32, String)>) {
        self.dictionaries += 1;
        for (hash, name) in names {
            self.named += 1;
            self.joaat_matches += usize::from(texture::joaat(&name) == hash);
            self.map.entry(hash).or_insert(name);
        }
    }
}

enum Hit {
    Model(Found),
    Names(Vec<(u32, String)>),
}

/// Walk page-aligned resource starts; `found` gets models, texture names are collected.
/// Cached walk: replays recorded hits (texture names stored, models re-decoded
/// from their offsets in parallel) instead of probing every page again.
fn walk(data: &[u8], options: &Options, names: &mut Names, mut found: impl FnMut(usize, Found)) -> usize {
    if options.limit == 0 {
        return 0;
    }
    let tag = format!("-{:x}-{:x}", options.start, options.len.unwrap_or(usize::MAX));
    let stamp = options.source.as_deref().filter(|_| options.cache).and_then(|s| crate::scancache::stamp(s, data, "models", &tag));
    if let Some((path, head)) = &stamp {
        if let Some(hits) = fs::read(path).ok().and_then(|b| walk_cache::load(&b, head, data.len())) {
            return replay(data, &hits, options.limit, names, found);
        }
    }
    let mut record = stamp.as_ref().map(|_| Vec::new());
    let mut names_seen = |list: &Vec<(u32, String)>| {
        if let Some(r) = record.as_mut() {
            r.push(walk_cache::Hit::Names(list.clone()));
        }
    };
    let mut models = Vec::new();
    let resources = walk_scan(data, options, names, &mut names_seen, |at, f| {
        models.push(at);
        found(at, f)
    });
    if let (Some((path, head)), Some(mut hits)) = (stamp, record) {
        // Only a complete walk is reusable; a --limit stop leaves the rest unknown.
        if resources < options.limit {
            hits.extend(models.into_iter().map(walk_cache::Hit::Model));
            let _ = crate::scancache::write_atomic(&path, &walk_cache::store(&head, hits));
        }
    }
    resources
}

fn replay(data: &[u8], hits: &[walk_cache::Hit], limit: usize, names: &mut Names, mut found: impl FnMut(usize, Found)) -> usize {
    const AHEAD: usize = 1024;
    let mut resources = 0;
    let models: Vec<usize> = hits
        .iter()
        .filter_map(|h| match h {
            walk_cache::Hit::Names(list) => {
                names.add(list.clone());
                None
            }
            walk_cache::Hit::Model(at) => Some(*at),
        })
        .collect();
    crate::par::ordered(models.len(), AHEAD, |i| recover(data, models[i]), |i, f| {
        if let Some(f) = f {
            resources += 1;
            found(models[i], f);
        }
        resources < limit
    });
    resources
}

fn walk_scan(
    data: &[u8],
    options: &Options,
    names: &mut Names,
    mut names_seen: impl FnMut(&Vec<(u32, String)>),
    mut found: impl FnMut(usize, Found),
) -> usize {
    const REGION: usize = 8 << 20;
    let end = options.len.map_or(data.len(), |l| data.len().min(options.start.saturating_add(l)));
    // The sequential loop ran while page + 18 <= len && page + 16 < end.
    let bound = data.len().saturating_sub(17).min(end.saturating_sub(16));
    let first = options.start / PAGE * PAGE;
    let mut resources = 0;
    if options.limit == 0 {
        return 0;
    }
    let step = |_: &mut (), page: usize| {
        let at = page + 16;
        if at < options.start {
            return None;
        }
        let names = |at| texture_names(data, at).map(|(stop, list)| (stop.next_multiple_of(PAGE), Hit::Names(list)));
        match candidate(data, at)? {
            Candidate::Model => match recover(data, at) {
                Some(f) => Some((f.end.next_multiple_of(PAGE), Hit::Model(f))),
                // Drawable and texture dictionaries share the pgDictionary header.
                None => names(at),
            },
            Candidate::Textures => names(at),
        }
    };
    crate::par::greedy(first..bound, PAGE, REGION, || (), step, |page, hit| {
        match hit {
            Hit::Model(f) => {
                resources += 1;
                found(page + 16, f);
            }
            Hit::Names(list) => {
                names_seen(&list);
                names.add(list)
            }
        }
        resources < options.limit
    });
    resources
}

fn list_textures(data: &[u8], options: &Options) -> Result<(), Box<dyn Error>> {
    let mut names = Names::new();
    walk(data, options, &mut names, |_, _| {});
    let mut all: Vec<_> = names.map.iter().collect();
    all.sort_by(|a, b| a.1.cmp(b.1));
    for (hash, name) in &all {
        println!("0x{hash:08X} {name}");
    }
    println!(
        "\n{} dictionaries, {} named textures, {} unique; hash == joaat(name) for {}",
        names.dictionaries,
        names.named,
        all.len(),
        names.joaat_matches
    );
    Ok(())
}

struct Pending {
    path: PathBuf,
    model: Model,
}

fn scan_models(data: &[u8], options: &Options, out: Option<&PathBuf>) -> Result<(), Box<dyn Error>> {
    let mut names = Names::new();
    for extra in &options.textures {
        let map = open(extra)?;
        let whole = Options { source: Some(extra.clone()), cache: options.cache, ..Options::whole() };
        walk(&map, &whole, &mut names, |_, _| {});
        eprintln!("{}: {} texture names so far", extra.display(), names.map.len());
    }
    match out {
        Some(out) => fs::create_dir_all(out)?,
        None => println!("{:<18} {:<10} {:>6} {:>8} {:>8} {:>4}  name", "offset", "kind", "meshes", "verts", "tris", "mats"),
    }
    let (mut models_total, mut pending, mut error) = (0, Vec::new(), None);
    let resources = walk(data, options, &mut names, |at, found| {
        let dictionary = decode::looks_like_dictionary(&found.system);
        for mut model in found.models {
            models_total += 1;
            let joints = options.skeleton.then(|| skeleton::read_for_drawable(&found.system, model.drawable));
            let Some(out) = out else {
                print_model(at, &model, &joints, found.raw_tail);
                continue;
            };
            let mut base = format!("{}_at_{at:016X}", safe_name(&model.name, dictionary));
            if dictionary {
                let _ = write!(base, "_child_{:08X}", model.drawable);
            }
            let joints = joints.flatten();
            let mut result = fs::write(out.join(format!("{base}.obj")), obj_text(&model, &base, joints.as_deref()));
            if let Some(j) = &joints {
                result = result.and(fs::write(out.join(format!("{base}.skel.txt")), skeleton_text(j)));
            }
            if let Err(e) = result {
                error.get_or_insert(e);
            }
            model.meshes.clear();
            pending.push(Pending { path: out.join(format!("{base}.mtl")), model });
        }
    });
    if let Some(e) = error {
        return Err(e.into());
    }
    // Materials are written last so names from dictionaries anywhere in the scan apply.
    let (mut refs, mut resolved) = (0, 0);
    let mut pools = Pools::new(options.pools.clone());
    for p in &pending {
        if let Some(dir) = p.path.parent() {
            pools.extract(&p.model, dir);
        }
        let (text, r, ok) = mtl(&p.model, &names.map, &pools.files);
        refs += r;
        resolved += ok;
        fs::write(&p.path, text)?;
    }
    if pools.keyed > 0 {
        let dir = pools.dir.as_ref().map_or("no pool dir".into(), |d| d.display().to_string());
        let catalog = match &pools.catalog {
            Some(Some(c)) => format!("; catalog {} pools, {} keys", c.inspected, c.keys()),
            _ => String::new(),
        };
        println!(
            "keyed textures: {} referenced, {} written from pools in {dir}, {} not found{catalog}",
            pools.keyed,
            pools.files.len(),
            pools.missing
        );
    }
    match out {
        None => println!(
            "\n{resources} resources, {models_total} models; {} texture dictionaries, {} names",
            names.dictionaries,
            names.map.len()
        ),
        Some(out) => println!(
            "wrote {} models from {resources} resources to {}; texture refs named {resolved}/{refs} ({} names from {} dictionaries)",
            pending.len(),
            out.display(),
            names.map.len(),
            names.dictionaries
        ),
    }
    Ok(())
}

fn print_model(at: usize, model: &Model, joints: &Option<Option<Vec<skeleton::Joint>>>, raw: bool) {
    let kind = if model.dictionary_child {
        "dict-child"
    } else if model.fragment {
        "fragment"
    } else {
        "drawable"
    };
    let verts: usize = model.meshes.iter().map(|m| m.positions.len()).sum();
    let tris: usize = model.meshes.iter().map(|m| m.indices.len() / 3).sum();
    let mats = model.materials.as_ref().map_or(0, Vec::len);
    let bones = match joints {
        Some(Some(j)) => format!("  [{} joints]", j.len()),
        Some(None) => "  [no skeleton]".into(),
        None => String::new(),
    };
    let raw = if raw { "  [raw tail]" } else { "" };
    println!("0x{at:016X} {kind:<10} {:>6} {verts:>8} {tris:>8} {mats:>4}  {}{bones}{raw}", model.meshes.len(), model.name);
}

fn meta(data: &[u8], out: Option<&PathBuf>) -> Result<(), Box<dyn Error>> {
    let entries = structured::recover(data);
    if let Some(out) = out {
        fs::create_dir_all(out)?;
    }
    for e in &entries {
        let detail = match psin::inspect(&e.data) {
            Some(p) => {
                let tags: Vec<_> = p.sections.iter().map(|s| format!("{}@{}+{}", s.tag, s.offset, s.len)).collect();
                format!("checksum 0x{:08X} platform 0x{:08X} {}", p.checksum, p.platform, tags.join(" "))
            }
            None => format!("<{}>", e.root),
        };
        println!("0x{:016X} {:<4} {:>9} -> {:>9}  {detail}", e.offset, e.kind, e.packed, e.data.len());
        if let Some(out) = out {
            let name = match e.kind {
                "psin" => format!("metadata_PSIN_at_{:016X}.psin", e.offset),
                _ => format!("xml_{}_at_{:016X}.xml", e.root, e.offset),
            };
            fs::write(out.join(name), &e.data)?;
        }
    }
    println!("\n{} metadata entries", entries.len());
    Ok(())
}

fn import_obj(input: &Path, out: Option<&PathBuf>) -> Result<(), Box<dyn Error>> {
    let text = fs::read_to_string(input)?;
    let stem = input.file_stem().unwrap_or_default().to_string_lossy();
    let model = obj::parse(&text, &stem)?;
    let verts: usize = model.meshes.iter().map(|m| m.positions.len()).sum();
    let tris: usize = model.meshes.iter().map(|m| m.indices.len() / 3).sum();
    println!(
        "{}: {} meshes, {verts} vertices, {tris} triangles, bounds {:?} {:?}",
        model.name,
        model.meshes.len(),
        model.min,
        model.max
    );
    if let Some(out) = out {
        let base = out.file_stem().unwrap_or_default().to_string_lossy();
        fs::write(out, obj_text(&model, &base, None))?;
    }
    Ok(())
}

fn standalone(file: &Path, out: &Path, options: &Options) -> Result<(), Box<dyn Error>> {
    let data = fs::read(file)?;
    let header = rsc::Header::read(&data).ok_or("not an RSC7 resource")?;
    let (system, graphics) = header.split(&data);
    println!(
        "RSC7 version {} system {} bytes ({} pages) graphics {} bytes ({} pages)",
        header.version,
        system.len(),
        rsc::Header::page_count(header.system_flags),
        graphics.len(),
        rsc::Header::page_count(header.graphics_flags)
    );
    let models = decode::decode_resource(system, graphics).ok_or("no decodable drawable in resource")?;
    fs::create_dir_all(out)?;
    let stem = file.file_stem().unwrap_or_default().to_string_lossy();
    for model in &models {
        let base = format!("{}_{:08X}", safe_name(&stem, false), model.drawable);
        let joints = if options.skeleton { skeleton::read_for_drawable(system, model.drawable) } else { None };
        fs::write(out.join(format!("{base}.obj")), obj_text(model, &base, joints.as_deref()))?;
        fs::write(out.join(format!("{base}.mtl")), mtl(model, &HashMap::new(), &HashMap::new()).0)?;
        if let Some(j) = &joints {
            fs::write(out.join(format!("{base}.skel.txt")), skeleton_text(j))?;
        }
    }
    println!("wrote {} models to {}", models.len(), out.display());
    Ok(())
}

// ---------------------------------------------------------------- recovery

fn zlib_header(b: &[u8]) -> bool {
    matches!(b, [cmf, flg, ..] if cmf & 15 == 8 && cmf >> 4 <= 7 && flg & 32 == 0
        && (u16::from(*cmf) << 8 | u16::from(*flg)) % 31 == 0)
}

/// Page-aligned resource start: 16 zero bytes, then a zlib stream whose first
/// inflated bytes look like a model resource or a texture dictionary.
fn candidate(data: &[u8], at: usize) -> Option<Candidate> {
    if data[at - 16..at].iter().any(|&b| b != 0) || !zlib_header(&data[at..]) {
        return None;
    }
    let input = &data[at..data.len().min(at + 65536)];
    let mut z = Decompress::new(true);
    let mut out = Vec::with_capacity(176);
    loop {
        let (i, o) = (z.total_in(), out.len());
        let status = z.decompress_vec(&input[i as usize..], &mut out, FlushDecompress::None);
        let full = out.len() >= 176;
        if full || !matches!(status, Ok(Status::Ok)) || (z.total_in() == i && out.len() == o) {
            if full && decode::looks_like_resource(&out) {
                return Some(Candidate::Model);
            }
            return texture::looks_like_dictionary(&out, 0).then_some(Candidate::Textures);
        }
    }
}

/// Inflate blocks of a standalone texture dictionary until its names are readable.
fn texture_names(data: &[u8], at: usize) -> Option<(usize, Vec<(u32, String)>)> {
    let mut inflater = Inflater::new();
    let (mut system, mut end) = (Vec::new(), at);
    for _ in 0..512 {
        let len = inflater.inflate(&data[end..])?;
        system.extend_from_slice(&inflater.out);
        end += len;
        match texture::names(&system) {
            texture::Names::Done(list) => return Some((end, list)),
            texture::Names::Bad => return None,
            texture::Names::Need => {}
        }
        let next = end.next_multiple_of(16);
        if system.len() > MAX_SYSTEM || next >= data.len() || data[end..next].iter().any(|&b| b != 0) {
            return None;
        }
        end = next;
    }
    None
}

struct Found {
    end: usize,
    system: Vec<u8>,
    models: Vec<Model>,
    raw_tail: bool,
}

struct Block {
    data: Vec<u8>,
    raw: bool,
}

enum Split {
    Done(Vec<u8>, Vec<Model>),
    More,
    No,
}

fn join(blocks: &[Block]) -> Vec<u8> {
    blocks.iter().flat_map(|b| b.data.iter().copied()).collect()
}

fn recover(data: &[u8], at: usize) -> Option<Found> {
    let mut inflater = Inflater::new();
    let mut end = at + inflater.inflate(&data[at..])?;
    let first = std::mem::take(&mut inflater.out);
    if first.len() < 8 || first.len() > MAX_SYSTEM || !decode::looks_like_resource(&first) {
        return None;
    }
    let maximum = MAX_SYSTEM + MAX_GRAPHICS;
    let mut total = first.len();
    let mut blocks = vec![Block { data: first, raw: false }];
    while blocks.len() <= MAX_BLOCKS {
        let last = blocks.last()?;
        if !(last.raw && last.data.len() == RAW_SYSTEM_TAIL) {
            match split(&blocks, total) {
                Split::Done(system, models) => {
                    let raw_tail = blocks.iter().any(|b| b.raw);
                    return Some(Found { end, system, models, raw_tail });
                }
                Split::No => return None,
                Split::More => {}
            }
        }
        let next = end.next_multiple_of(16);
        if blocks.len() == MAX_BLOCKS || total >= maximum || next + 2 > data.len() || data[end..next].iter().any(|&b| b != 0) {
            return None;
        }
        let block = match inflater.inflate(&data[next..]) {
            Some(len) => {
                end = next + len;
                Block { data: std::mem::take(&mut inflater.out), raw: false }
            }
            None => {
                let tail = raw_tail(data, &blocks, total, next)?;
                end = next + tail.data.len();
                tail
            }
        };
        total += block.data.len();
        blocks.push(block);
    }
    None
}

/// Uncompressed 16-byte system or 48-byte graphics tail stored after a full
/// 64 KiB block (TryRawTail).
fn raw_tail(data: &[u8], blocks: &[Block], total: usize, next: usize) -> Option<Block> {
    let last = blocks.last()?;
    if last.raw || last.data.len() & 0xFFFF != 0 || zlib_header(&data[next..]) {
        return None;
    }
    if total <= MAX_SYSTEM - RAW_SYSTEM_TAIL && next + RAW_SYSTEM_TAIL + 2 <= data.len() {
        let system = join(blocks);
        if let Some(e) = decode::probe_extents(&system)
            && !e.complete
            && e.system > total
            && e.system - total <= RAW_SYSTEM_TAIL
            && zlib_header(&data[next + RAW_SYSTEM_TAIL..])
        {
            let tail = &data[next..next + RAW_SYSTEM_TAIL];
            let combined = [system.as_slice(), tail].concat();
            if decode::probe_extents(&combined).is_some_and(|e| e.complete) {
                return Some(Block { data: tail.to_vec(), raw: true });
            }
        }
    }
    let mut system = Vec::new();
    for count in 1..blocks.len() {
        system.extend_from_slice(&blocks[count - 1].data);
        if system.len() > MAX_SYSTEM {
            break;
        }
        let Some(extent) = decode::probe_extents(&system).filter(|e| e.complete) else { continue };
        let graphics_len = total - system.len();
        if extent.graphics.checked_sub(graphics_len) != Some(RAW_GRAPHICS_TAIL) || next + RAW_GRAPHICS_TAIL > data.len() {
            continue;
        }
        let tail = &data[next..next + RAW_GRAPHICS_TAIL];
        let graphics = [join(&blocks[count..]).as_slice(), tail].concat();
        if decode::decode_resource(&system, &graphics).is_some() {
            return Some(Block { data: tail.to_vec(), raw: true });
        }
    }
    None
}

/// Try every block boundary as the system/graphics split (TrySplit).
fn split(blocks: &[Block], total: usize) -> Split {
    let mut needs_more = false;
    let mut system = Vec::new();
    for count in 1..=blocks.len() {
        system.extend_from_slice(&blocks[count - 1].data);
        if system.len() > MAX_SYSTEM {
            break;
        }
        let Some(extent) = decode::probe_extents(&system) else { continue };
        if !extent.complete {
            needs_more = true;
            continue;
        }
        if extent.system > system.len() {
            continue;
        }
        let graphics_len = total - system.len();
        if graphics_len < extent.graphics {
            needs_more = true;
            continue;
        }
        if graphics_len > MAX_GRAPHICS {
            continue;
        }
        let graphics = join(&blocks[count..]);
        if let Some(models) = decode::decode_resource(&system, &graphics) {
            return Split::Done(system, models);
        }
    }
    if needs_more { Split::More } else { Split::No }
}

// ---------------------------------------------------------------- output

fn safe_name(name: &str, keep_hash: bool) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || "_-.".contains(c) || (keep_hash && c == '#') { c } else { '_' })
        .collect();
    let cleaned = cleaned.trim_matches('.');
    if cleaned.is_empty() { "drawable".into() } else { cleaned.chars().take(160).collect() }
}

/// Diffuse/colour parameter hashes in priority order (ModelTextureBindings).
const COLOR_SLOTS: [u32; 16] = [
    3370697346, 756347250, 0x346394EA, 0x7D4F12E2, 3464205966, 3632507550, 851042057, 1024521143,
    3853173992, 2464948076, 3389132183, 3547013225, 234123336, 3831897388, 3489704073, 1380592926,
];

/// Name of a texture in the drawable's own dictionary (keyed entries carry no name).
fn embedded_name(model: &Model, hash: u32) -> Option<&String> {
    let t = model.embedded.as_ref()?.by_hash(hash)?;
    t.asset_key.is_none().then_some(&t.name)
}

/// Asset key of a keyed (unnamed) texture in the drawable's own dictionary.
fn embedded_key(model: &Model, hash: u32) -> Option<u64> {
    model.embedded.as_ref()?.by_hash(hash)?.asset_key
}

/// Writes pixels of keyed embedded textures from the ttdb streaming pools.
struct Pools {
    dir: Option<PathBuf>,
    catalog: Option<Option<crate::texture::Catalog>>,
    maps: HashMap<PathBuf, Mmap>,
    /// name hash -> file written next to the OBJ (png, else dds).
    files: HashMap<u32, String>,
    keyed: usize,
    missing: usize,
}

impl Pools {
    fn new(dir: Option<PathBuf>) -> Self {
        Self { dir, catalog: None, maps: HashMap::new(), files: HashMap::new(), keyed: 0, missing: 0 }
    }

    fn extract(&mut self, model: &Model, out: &Path) {
        for material in model.materials.iter().flatten() {
            for t in &material.textures {
                let key = embedded_key(model, t.hash).unwrap_or(t.key);
                if self.files.contains_key(&t.hash) {
                    continue;
                }
                self.keyed += 1;
                match self.write(key, t.hash, out) {
                    Ok(file) => {
                        self.files.insert(t.hash, file);
                    }
                    Err(e) => {
                        self.missing += 1;
                        eprintln!("  texture 0x{:08X} key {key:016X}: {e}", t.hash);
                    }
                }
            }
        }
    }

    fn write(&mut self, key: u64, hash: u32, out: &Path) -> Result<String, String> {
        let dir = self.dir.as_ref().ok_or("no ttdb pool directory")?;
        let catalog = self.catalog.get_or_insert_with(|| {
            crate::texture::Catalog::build(dir).map_err(|e| eprintln!("{}: {e}", dir.display())).ok()
        });
        let catalog = catalog.as_ref().ok_or("pool catalog unavailable")?;
        let location = catalog.find(key).iter().find(|l| l.extractable).ok_or("not in pools")?;
        if !self.maps.contains_key(&location.path) {
            let map = open(&location.path).map_err(|e| e.to_string())?;
            self.maps.insert(location.path.clone(), map);
        }
        let pool = crate::texture::Pool::open(&self.maps[&location.path]).map_err(|e| e.to_string())?;
        let texture = pool.textures.get(location.index).ok_or("pool index out of range")?;
        let stem = format!("0x{hash:08X}");
        let dds = pool.create_dds(texture).map_err(|e| e.to_string())?;
        fs::write(out.join(format!("{stem}.dds")), dds).map_err(|e| e.to_string())?;
        match pool.decode_preview(texture) {
            Ok((w, h, rgba)) if crate::texture::write_png(&out.join(format!("{stem}.png")), w, h, &rgba).is_ok() => {
                Ok(format!("{stem}.png"))
            }
            _ => Ok(format!("{stem}.dds")),
        }
    }
}

/// .mtl text plus (texture refs, refs with a resolved name).
fn mtl(model: &Model, global: &HashMap<u32, String>, files: &HashMap<u32, String>) -> (String, usize, usize) {
    let mut text = String::from("# Texture names: embedded dictionary, then texture dictionaries in the scanned archive(s); else 0x<name hash>.\n");
    let (mut refs, mut resolved) = (0, 0);
    let mut name = |hash: u32| -> String {
        refs += 1;
        match embedded_name(model, hash).or_else(|| global.get(&hash)) {
            Some(n) => {
                resolved += 1;
                n.clone()
            }
            None => format!("0x{hash:08X}"),
        }
    };
    for (i, material) in model.materials.iter().flatten().enumerate() {
        let _ = writeln!(text, "\nnewmtl material_{i}\n# shader 0x{:08X}\nKd 1 1 1", material.shader_hash);
        let mut color = None;
        for slot in COLOR_SLOTS {
            if let Some(t) = material.textures.iter().find(|t| t.param == slot) {
                color = Some(t.hash);
                break;
            }
        }
        for t in &material.textures {
            let key = embedded_key(model, t.hash).unwrap_or(t.key);
            let key = format!(" key {key:016X}");
            let _ = writeln!(text, "# param 0x{:08X} texture 0x{:08X} {}{key}", t.param, t.hash, name(t.hash));
        }
        if let Some(hash) = color {
            let n = embedded_name(model, hash).or_else(|| global.get(&hash)).cloned().unwrap_or_else(|| format!("0x{hash:08X}"));
            let file = files.get(&hash).cloned().unwrap_or(format!("{n}.dds"));
            let _ = writeln!(text, "map_Kd {file}");
        }
    }
    (text, refs, resolved)
}

fn obj_text(model: &Model, base: &str, joints: Option<&[skeleton::Joint]>) -> String {
    let mut text = String::from("# Recovered native drawable geometry.\n");
    if let Some(note) = model.note {
        let _ = writeln!(text, "# {note}");
    }
    let _ = writeln!(text, "# bounds {:?} {:?}; bone bindings: {}", model.min, model.max, model.bones);
    let _ = writeln!(text, "mtllib {base}.mtl");
    let (mut first, mut first_uv) = (1usize, 1usize);
    for mesh in &model.meshes {
        let _ = writeln!(text, "o {}", mesh.name.replace(['\r', '\n'], "_"));
        if let Some(palette) = &mesh.palette {
            let hashes: Vec<String> = palette.iter().map(|h| format!("0x{h:08X}")).collect();
            let _ = writeln!(text, "# bone palette: {}", hashes.join(" "));
            if let Some(indices) = joints.and_then(|j| skeleton::resolve_palette(j, palette)) {
                let list: Vec<String> = indices.iter().map(usize::to_string).collect();
                let _ = writeln!(text, "# bone palette joints: {}", list.join(" "));
            }
        }
        if let Some(shader) = mesh.shader {
            let _ = writeln!(text, "usemtl material_{shader}");
        }
        let colors = mesh.colors.as_ref().filter(|c| c.len() == mesh.positions.len());
        for (i, p) in mesh.positions.iter().enumerate() {
            let _ = write!(text, "v {} {} {}", p[0], p[1], p[2]);
            if let Some(c) = colors {
                // R8G8B8A8: red in the low byte; OBJ vertex-colour extension.
                let [r, g, b, _] = c[i].to_le_bytes().map(|v| v as f32 / 255.0);
                let _ = write!(text, " {r:.4} {g:.4} {b:.4}");
            }
            text.push('\n');
        }
        let uvs = mesh.uvs.as_ref().filter(|uv| uv.len() == mesh.positions.len());
        for uv in uvs.into_iter().flatten() {
            let _ = writeln!(text, "vt {} {}", uv[0], uv[1]);
        }
        for tri in mesh.indices.chunks_exact(3) {
            text.push('f');
            for &i in tri {
                let _ = write!(text, " {}", first + i as usize);
                if uvs.is_some() {
                    let _ = write!(text, "/{}", first_uv + i as usize);
                }
            }
            text.push('\n');
        }
        first += mesh.positions.len();
        if uvs.is_some() {
            first_uv += mesh.positions.len();
        }
    }
    text
}

fn skeleton_text(joints: &[skeleton::Joint]) -> String {
    let mut text = String::from("# index hash parent | inverse bind matrix (row-major 4x4)\n");
    for (i, j) in joints.iter().enumerate() {
        let parent = j.parent.map_or("-".into(), |p| p.to_string());
        let m: Vec<String> = j.inverse_bind.iter().map(f32::to_string).collect();
        let _ = writeln!(text, "{i} 0x{:08X} {parent} | {}", j.hash, m.join(" "));
    }
    text
}

// ---------------------------------------------------------------- GUI API

pub use decode::{Material, Mesh, Model as Drawable, TexRef, V3};

/// One recovered model, without its geometry.
pub struct ModelIndex {
    pub at: usize,
    pub child: usize,
    pub name: String,
    pub kind: &'static str,
    pub meshes: usize,
    pub verts: usize,
    pub tris: usize,
}

/// Every model resource in the archive (whole scan; honours [`crate::progress`]).
pub fn index_models(data: &[u8]) -> Vec<ModelIndex> {
    let mut names = Names::new();
    let mut out = Vec::new();
    walk(data, &Options::whole(), &mut names, |at, found| {
        for (child, m) in found.models.iter().enumerate() {
            let kind = if m.dictionary_child {
                "dict-child"
            } else if m.fragment {
                "fragment"
            } else {
                "drawable"
            };
            out.push(ModelIndex {
                at,
                child,
                name: m.name.clone(),
                kind,
                meshes: m.meshes.len(),
                verts: m.meshes.iter().map(|m| m.positions.len()).sum(),
                tris: m.meshes.iter().map(|m| m.indices.len() / 3).sum(),
            });
        }
    });
    out
}

/// Re-decode model `child` of the resource at `at`.
pub fn load_model(data: &[u8], at: usize, child: usize) -> Option<Drawable> {
    recover(data, at)?.models.into_iter().nth(child)
}

/// The first (system page) stream of the resource at `at`, inflated only.
pub fn system_page(data: &[u8], at: usize) -> Option<Vec<u8>> {
    let mut inflater = Inflater::new();
    inflater.inflate(data.get(at..)?)?;
    Some(inflater.out)
}

/// Every model of the resource at `at` (empty when it no longer decodes).
pub fn load_models(data: &[u8], at: usize) -> Vec<Drawable> {
    recover(data, at).map(|f| f.models).unwrap_or_default()
}

/// Write `<base>.obj` + `<base>.mtl` for one model into `out`; returns the OBJ path.
pub fn export_model(data: &[u8], at: usize, child: usize, out: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let found = recover(data, at).ok_or("model no longer decodes")?;
    let dictionary = decode::looks_like_dictionary(&found.system);
    let model = found.models.get(child).ok_or("no such model")?;
    let mut base = format!("{}_at_{at:016X}", safe_name(&model.name, dictionary));
    if dictionary {
        let _ = write!(base, "_child_{:08X}", model.drawable);
    }
    fs::create_dir_all(out)?;
    let obj = out.join(format!("{base}.obj"));
    fs::write(&obj, obj_text(model, &base, None))?;
    fs::write(out.join(format!("{base}.mtl")), mtl(model, &HashMap::new(), &HashMap::new()).0)?;
    Ok(obj)
}
