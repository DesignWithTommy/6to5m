//! `6ixpack audio`: find, list, carve and extract AWC/ADAT audio (port of rpf9-audio,
//! plus the AWC recovery cache and XMA2 handling from rpf9-explorer).

mod awc;
mod carve;
mod ogg;
mod opus;
mod util;
mod wav;

use awc::{Awc, CODEC_OPUS, CODEC_XMA2, Framing, TAG_DATA};
use memmap2::Mmap;
use std::{
    error::Error,
    ffi::OsString,
    fs::{self, File},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
};
use util::{Json, obj};

const USAGE: &str = "\
usage: 6ixpack audio <command> [options]

  scan <file>                 list validated AWC/ADAT payloads (cached per archive)
      [--limit N] [--no-cache] [--rescan] [--json]
  list-awc <file>             streams, chunks, formats and block framing
      [--offset N] [--length N] [--block-header H] [--json]
  extract <awc|rpf> <outdir>  Opus -> .ogg (verbatim remux) and .wav (needs libopus);
                              >2-channel music -> one mono .ogg per channel + one
                              multichannel .wav; XMA2 / PCM streams -> .wav (undecoded XMA2)
      [--offset N] [--length N] [--limit N] [--stream N] [--block-header H]
      [--no-wav] [--rate 48000|32768] [--no-cache] [--json]
      [--title T] [--artist A] [--album A] [--genre G] [--date D]
      [--comment NAME=VALUE]... [--cover IMAGE]
  strings <file>              printable strings in metadata/animation/marker chunks
      [--offset N] [--chunk 0xB2,0x5C,0xBD] [--min-len N] [--json]
  carve <file> [outdir]       carve AWC/RIFF/Ogg/PNG/Bink/AMR + zlib streams; with outdir
                              writes files plus index.json, index.tsv, report.txt
      [--no-zlib] [--zlib-all] [--force] [--json]
  index <outdir|index.json>   summarize a carve index [--json]
  doctor                      decoder and supported formats [--json]

H is auto, 0x800, 0x1000, 0x2000 or 0x4000. Numbers accept 0x. No decryption.";

type Res<T> = Result<T, Box<dyn Error>>;

#[derive(Default)]
struct Opts {
    paths: Vec<PathBuf>,
    offset: Option<usize>,
    length: Option<usize>,
    limit: Option<usize>,
    stream: Option<usize>,
    block_header: Option<usize>,
    no_wav: bool,
    rate: u32,
    json: bool,
    no_cache: bool,
    rescan: bool,
    comments: Vec<(String, String)>,
    cover: Option<PathBuf>,
    chunks: Option<Vec<u8>>,
    min_len: usize,
    no_zlib: bool,
    zlib_all: bool,
    force: bool,
    no_load: bool,
}

fn parse_num(text: &str) -> Res<usize> {
    let parsed = match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        Some(hex) => usize::from_str_radix(hex, 16),
        None => text.parse(),
    };
    Ok(parsed.map_err(|_| format!("bad number {text}"))?)
}

fn parse_opts(args: &[OsString]) -> Res<Opts> {
    let mut o = Opts { rate: 48000, min_len: 3, ..Default::default() };
    let mut it = args.iter();
    let value = |name: &str, it: &mut std::slice::Iter<OsString>| -> Res<String> {
        Ok(it.next().and_then(|v| v.to_str()).ok_or(format!("{name} needs a value"))?.to_string())
    };
    while let Some(arg) = it.next() {
        let Some(flag) = arg.to_str().filter(|s| s.starts_with("--")) else {
            o.paths.push(arg.into());
            continue;
        };
        match flag {
            "--offset" => o.offset = Some(parse_num(&value(flag, &mut it)?)?),
            "--length" => o.length = Some(parse_num(&value(flag, &mut it)?)?),
            "--limit" => o.limit = Some(parse_num(&value(flag, &mut it)?)?),
            "--stream" => o.stream = Some(parse_num(&value(flag, &mut it)?)?),
            "--min-len" => o.min_len = parse_num(&value(flag, &mut it)?)?,
            "--block-header" => {
                let v = value(flag, &mut it)?;
                if v != "auto" {
                    let h = parse_num(&v)?;
                    if !awc::MUSIC_HEADERS.contains(&h) {
                        return Err("--block-header must be auto, 0x800, 0x1000, 0x2000 or 0x4000".into());
                    }
                    o.block_header = Some(h);
                }
            }
            "--rate" => {
                o.rate = parse_num(&value(flag, &mut it)?)? as u32;
                if o.rate != 48000 && o.rate != 32768 {
                    return Err("--rate must be 48000 or 32768".into());
                }
            }
            "--chunk" => {
                let v = value(flag, &mut it)?;
                o.chunks = Some(v.split(',').map(|c| parse_num(c.trim()).map(|n| n as u8)).collect::<Res<_>>()?);
            }
            "--cover" => o.cover = Some(value(flag, &mut it)?.into()),
            "--title" | "--artist" | "--album" | "--genre" | "--date" => {
                o.comments.push((flag[2..].to_ascii_uppercase(), value(flag, &mut it)?));
            }
            "--comment" => {
                let v = value(flag, &mut it)?;
                let (k, val) = v.split_once('=').ok_or("--comment needs NAME=VALUE")?;
                o.comments.push((k.to_string(), val.to_string()));
            }
            "--no-wav" => o.no_wav = true,
            "--json" => o.json = true,
            "--no-cache" => o.no_cache = true,
            "--rescan" => o.rescan = true,
            "--no-zlib" => o.no_zlib = true,
            "--zlib-all" => o.zlib_all = true,
            "--force" => o.force = true,
            "--no-load" => o.no_load = true,
            _ => return Err(format!("unknown option {flag}\n{USAGE}").into()),
        }
    }
    Ok(o)
}

fn map(path: &Path) -> Res<Mmap> {
    let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    // SAFETY: read-only map; inputs are not modified while mapped.
    Ok(unsafe { Mmap::map(&file)? })
}

pub fn run(args: &[OsString]) -> Res<()> {
    let Some(command) = args.first().and_then(|a| a.to_str()) else {
        return Err(USAGE.into());
    };
    if matches!(command, "-h" | "--help" | "help") {
        println!("{USAGE}");
        return Ok(());
    }
    let o = parse_opts(&args[1..])?;
    match (command, o.paths.as_slice()) {
        ("scan", [input]) => scan(input, &o),
        ("list-awc", [input]) => list_awc(input, &o),
        ("extract", [input, outdir]) => extract(input, outdir, &o),
        ("strings", [input]) => strings(input, &o),
        ("carve", [input]) => carve_cmd(input, None, &o),
        ("carve", [input, outdir]) => carve_cmd(input, Some(outdir), &o),
        ("index", [path]) => index(path, &o),
        ("doctor", []) => doctor(&o),
        _ => Err(USAGE.into()),
    }
}

fn print_json(v: &Json) {
    println!("{}", v.dump(2));
}

/// The AWC window selected by --offset/--length.
fn window<'d>(data: &'d [u8], o: &Opts) -> Res<&'d [u8]> {
    let base = o.offset.unwrap_or(0);
    let rest = data.get(base..).ok_or("offset past end of file")?;
    Ok(match o.length {
        Some(n) => rest.get(..n).ok_or("--length runs past end of file")?,
        None => rest,
    })
}

// ---------------------------------------------------------------- AWC discovery + cache

/// Every validated AWC start and length, non-overlapping, searched in parallel.
fn find_awcs(data: &[u8]) -> Vec<(usize, usize)> {
    // Many pieces, claimed in order by the worker pool, even out uneven ones.
    const PIECE: usize = 16 << 20;
    let pieces: Vec<usize> = (0..data.len().div_ceil(PIECE)).collect();
    let progress = crate::progress::current();
    let finder = memchr::memmem::Finder::new(b"ADAT");
    let done = std::sync::atomic::AtomicU64::new(0);
    let found: Vec<Vec<(usize, usize)>> = crate::par::map(&pieces, |&i| {
        let (start, end) = (i * PIECE, ((i + 1) * PIECE + 3).min(data.len()));
        let mut hits = Vec::new();
        if progress.as_ref().is_some_and(|p| p.cancelled()) {
            return hits;
        }
        let mut pos = start;
        while let Some(rel) = finder.find(&data[pos..end]) {
            let at = pos + rel;
            if let Some((len, _)) = Awc::probe(&data[at..]) {
                hits.push((at, len));
            }
            pos = at + 1;
        }
        let total = done.fetch_add((end - start) as u64, std::sync::atomic::Ordering::Relaxed);
        if let Some(p) = &progress {
            p.done.store(total + (end - start) as u64, std::sync::atomic::Ordering::Relaxed);
        }
        hits
    });
    let mut found: Vec<(usize, usize)> = found.into_iter().flatten().collect();
    found.sort_unstable();
    let mut out: Vec<(usize, usize)> = Vec::new();
    for hit in found {
        if out.last().is_none_or(|&(at, len)| hit.0 >= at + len) {
            out.push(hit);
        }
    }
    out
}

/// Cache of AWC ranges per archive, keyed like rpf9-explorer's AwcRecovery cache
/// (path, length, mtime, SHA-256 of the first 64 KiB), stored under
/// %LOCALAPPDATA%\rpf9\audio-index. Ranges are re-validated on load.
fn cache_path(source: &Path) -> Option<PathBuf> {
    let full = fs::canonicalize(source).ok()?;
    let key = util::sha256_hex(full.to_string_lossy().to_uppercase().as_bytes());
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("rpf9").join("audio-index").join(format!("{key}.awc.json")))
}

fn source_identity(source: &Path, data: &[u8]) -> Option<Json> {
    let meta = fs::metadata(source).ok()?;
    let mtime = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos() as f64;
    Some(obj![
        ("source", fs::canonicalize(source).ok()?.to_string_lossy().to_string()),
        ("source_length", data.len()),
        ("mtime_ns", mtime),
        ("header_sha256", util::sha256_hex(&data[..data.len().min(65536)])),
    ])
}

fn load_cache(source: &Path, data: &[u8]) -> Option<Vec<(usize, usize)>> {
    let doc = Json::parse(&fs::read_to_string(cache_path(source)?).ok()?).ok()?;
    let id = source_identity(source, data)?;
    if doc.get("schema")?.num()? != 1.0 {
        return None;
    }
    for key in ["source", "source_length", "mtime_ns", "header_sha256"] {
        if format!("{:?}", doc.get(key)?) != format!("{:?}", id.get(key)?) {
            return None;
        }
    }
    let Json::Arr(ranges) = doc.get("ranges")? else { return None };
    let mut out = Vec::with_capacity(ranges.len());
    for r in ranges {
        let Json::Arr(pair) = r else { return None };
        let (at, len) = (pair.first()?.num()? as usize, pair.get(1)?.num()? as usize);
        if Awc::probe(data.get(at..)?)?.0 != len || out.last().is_some_and(|&(p, _)| p >= at) {
            return None;
        }
        out.push((at, len));
    }
    Some(out)
}

fn write_cache(source: &Path, data: &[u8], ranges: &[(usize, usize)]) -> Option<PathBuf> {
    let path = cache_path(source)?;
    let mut doc = obj![("schema", 1u32)];
    if let Json::Obj(items) = source_identity(source, data)? {
        for (k, v) in items {
            doc.push(&k, v);
        }
    }
    doc.push("complete_scan", true);
    doc.push("ranges", ranges.iter().map(|&(a, l)| Json::Arr(vec![a.into(), l.into()])).collect::<Vec<_>>());
    fs::create_dir_all(path.parent()?).ok()?;
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, doc.dump(0)).ok()?;
    fs::rename(&tmp, &path).ok()?;
    Some(path)
}

fn awcs_in(source: &Path, data: &[u8], o: &Opts) -> Vec<(usize, usize)> {
    let cached = if o.no_cache || o.rescan { None } else { load_cache(source, data) };
    let mut ranges = match cached {
        Some(r) => {
            eprintln!("using cached AWC index ({} ranges)", r.len());
            r
        }
        None => {
            let r = find_awcs(data);
            if !o.no_cache && data.len() > 64 << 20 && !crate::progress::cancelled() {
                if let Some(p) = write_cache(source, data, &r) {
                    eprintln!("cached AWC index -> {}", p.display());
                }
            }
            r
        }
    };
    if let Some(limit) = o.limit {
        ranges.truncate(limit);
    }
    ranges
}

fn scan(input: &Path, o: &Opts) -> Res<()> {
    let data = map(input)?;
    let hits = awcs_in(input, &data, o);
    let mut rows = Vec::new();
    let mut total = 0;
    for &(at, len) in &hits {
        let awc = Awc::parse(&data[at..at + len], false)?;
        total += len;
        if o.json {
            rows.push(obj![("offset", at), ("length", len), ("streams", awc.streams.len()), ("flags", format!("{:#010x}", awc.flags)), ("multichannel", awc.multichannel())]);
        } else {
            let mc = if awc.multichannel() { ", multichannel" } else { "" };
            println!("{at:#012x}  {len:>10}  {} streams, flags {:#010x}{mc}", awc.streams.len(), awc.flags);
        }
    }
    if o.json {
        print_json(&obj![("name", input.display().to_string()), ("size", data.len()), ("awc_count", hits.len()), ("awc_bytes", total), ("awcs", rows)]);
    } else {
        println!("{} AWC payload(s), {total} bytes", hits.len());
    }
    Ok(())
}

// ---------------------------------------------------------------- list-awc / strings

fn codec_of(awc: &Awc, stream: &awc::Stream) -> Option<u8> {
    if let Some(sf) = awc.stream_format(stream) {
        return sf.channels.first().map(|c| c.codec);
    }
    let fmt = awc.audio_format(stream)?;
    Some(fmt.codec32.map_or(fmt.codec, |(codec, _)| codec))
}

fn framing_for(awc: &Awc, stream: &awc::Stream, data: &[u8], forced: Option<usize>) -> Framing {
    match forced {
        Some(h) if awc.multichannel() || stream.chunk(&[awc::TAG_STREAM_FORMAT]).is_some() => Framing::Music(h),
        _ => awc::framing(awc, stream, data),
    }
}

fn list_awc(input: &Path, o: &Opts) -> Res<()> {
    let data = map(input)?;
    let awc = Awc::parse(window(&data, o)?, false)?;
    let base = o.offset.unwrap_or(0);
    let mut streams = Vec::new();
    if !o.json {
        println!(
            "{} at {base:#x}: flags {:#010x}, {} streams, header {:#x}, length {:#x}{}",
            input.display(),
            awc.flags,
            awc.streams.len(),
            awc.header_length,
            awc.length(),
            if awc.multichannel() { ", multichannel" } else { "" }
        );
    }
    for (i, stream) in awc.streams.iter().enumerate() {
        let codec = codec_of(&awc, stream);
        let mut entry = obj![("index", i), ("id", format!("{:08x}", stream.id)), ("codec", codec)];
        let chunks: Vec<Json> = stream.chunks.iter().map(|c| obj![("type", format!("0x{:02x}", c.kind)), ("name", awc::chunk_name(c.kind)), ("offset", c.offset), ("size", c.size)]).collect();
        if !o.json {
            println!("  stream {i:4}  id {:08x}  codec {}", stream.id, codec.map_or("-".into(), |c| format!("{c:#04x}")));
            for c in &stream.chunks {
                println!("      {:<16} offset {:#010x}  size {:#x}", awc::chunk_name(c.kind), c.offset, c.size);
            }
        }
        entry.push("chunks", chunks);
        if let Some(f) = awc.audio_format(stream) {
            if !o.json {
                let extra = f.codec32.map_or(String::new(), |(c, ch)| format!(", codec32 {c:#04x}, channels {ch}"));
                println!("      format: {} samples, {} Hz, codec {:#04x}{extra}", f.samples, f.rate, f.codec);
            }
            entry.push("format", obj![("samples", f.samples), ("rate", f.rate), ("codec", f.codec), ("codec32", f.codec32.map(|c| c.0)), ("channels", f.codec32.map(|c| c.1))]);
        }
        if let Some(sf) = awc.stream_format(stream) {
            if !o.json {
                println!("      stream format: {} blocks, {} bytes/block, {} channel(s)", sf.blocks, sf.block_size, sf.channels.len());
                for ch in &sf.channels {
                    println!("        channel {:08x}: {} samples, {} Hz, codec {:#04x}", ch.id, ch.samples, ch.rate, ch.codec);
                }
            }
            let chans: Vec<Json> = sf.channels.iter().map(|c| obj![("id", format!("{:08x}", c.id)), ("samples", c.samples), ("rate", c.rate), ("codec", c.codec)]).collect();
            entry.push("stream_format", obj![("blocks", sf.blocks), ("block_size", sf.block_size), ("channels", chans)]);
        }
        if let Some(data) = stream.chunk(&[TAG_DATA]).and_then(|c| awc.bytes(c)) {
            let sf = awc.stream_format(stream);
            let deint = sf.as_ref().filter(|_| o.block_header.is_none()).and_then(|sf| awc::deinterleave(data, sf.channels.len(), sf.block_size as usize));
            let framing = if let Some(chs) = &deint {
                let counts: Vec<usize> = chs.iter().map(|c| c.packets.len()).collect();
                if !o.json {
                    println!("      framing: 512 KiB blocks, de-interleaved, packets per channel {counts:?}");
                }
                obj![("kind", "music-deinterleaved"), ("packets", counts.into_iter().map(Json::from).collect::<Vec<_>>())]
            } else {
                match framing_for(&awc, stream, data, o.block_header) {
                    Framing::TwoK => {
                        let n = awc::packets_2k(data).len();
                        if !o.json {
                            println!("      framing: 2 KiB blocks, {n} packets");
                        }
                        obj![("kind", "2k"), ("packets", n)]
                    }
                    Framing::Music(h) => {
                        let n = awc::packets_music(data, h, None).len();
                        if !o.json {
                            println!("      framing: 512 KiB blocks, header {h:#x}, {n} packets");
                        }
                        obj![("kind", "512k"), ("block_header", format!("{h:#x}")), ("packets", n)]
                    }
                }
            };
            entry.push("framing", framing);
        }
        streams.push(entry);
    }
    if o.json {
        print_json(&obj![
            ("file", input.display().to_string()),
            ("offset", base),
            ("flags", format!("{:#010x}", awc.flags)),
            ("header_length", awc.header_length),
            ("length", awc.length()),
            ("multichannel", awc.multichannel()),
            ("streams", streams),
        ]);
    }
    Ok(())
}

fn strings(input: &Path, o: &Opts) -> Res<()> {
    let data = map(input)?;
    let awc = Awc::parse(window(&data, o)?, false)?;
    let wanted = o.chunks.clone().unwrap_or_else(|| vec![0xb2, 0x5c, 0xbd]);
    let mut records = Vec::new();
    for (index, stream) in awc.streams.iter().enumerate() {
        for chunk in stream.chunks.iter().filter(|c| wanted.contains(&c.kind)) {
            let Some(body) = awc.bytes(*chunk) else { continue };
            let mut start = None;
            for (i, &b) in body.iter().chain([&0u8]).enumerate() {
                match (b.is_ascii_graphic() || b == b' ', start) {
                    (true, None) => start = Some(i),
                    (false, Some(s)) => {
                        if i - s >= o.min_len.max(1) {
                            let text = String::from_utf8_lossy(&body[s..i]).to_string();
                            if o.json {
                                records.push(obj![
                                    ("stream", index),
                                    ("stream_id", format!("{:08x}", stream.id)),
                                    ("chunk", format!("0x{:02x}", chunk.kind)),
                                    ("chunk_name", awc::chunk_name(chunk.kind)),
                                    ("chunk_offset", chunk.offset),
                                    ("offset", s),
                                    ("text", text),
                                ]);
                            } else {
                                let line = format!("{:<10} stream={index} id={:08x} +{s:#06x}  {text}
", awc::chunk_name(chunk.kind), stream.id);
                                if std::io::stdout().write_all(line.as_bytes()).is_err() {
                                    return Ok(());
                                }
                            }
                        }
                        start = None;
                    }
                    _ => {}
                }
            }
        }
    }
    if o.json {
        print_json(&Json::Arr(records));
    }
    Ok(())
}

// ---------------------------------------------------------------- extract

#[derive(Default)]
struct Totals {
    awcs: usize,
    ogg: usize,
    wav: usize,
    xma: usize,
    skipped: usize,
    results: Vec<Json>,
}

struct Ctx<'a> {
    dll: Option<PathBuf>,
    tags: Vec<u8>,
    o: &'a Opts,
}

fn extract(input: &Path, outdir: &Path, o: &Opts) -> Res<()> {
    let data = map(input)?;
    let dll = if o.no_wav { None } else { opus::find(None) };
    let cover = o.cover.as_deref().map(fs::read).transpose()?;
    if cover.as_ref().is_some_and(|c| c.len() > 16 << 20) {
        return Err("cover image is larger than the 16 MiB safety limit".into());
    }
    let ctx = Ctx { dll, tags: ogg::build_tags(&o.comments, cover.as_deref())?, o };
    let single = o.offset.is_some() || data.starts_with(b"ADAT");
    let targets = if single {
        let w = window(&data, o)?;
        vec![(o.offset.unwrap_or(0), Awc::parse(w, false)?.length().min(w.len()))]
    } else {
        awcs_in(input, &data, o)
    };
    fs::create_dir_all(outdir)?;
    let mut t = Totals::default();
    for &(at, len) in &targets {
        let dir = if single { outdir.to_path_buf() } else { outdir.join(format!("{at:011x}")) };
        match Awc::parse(&data[at..(at + len).min(data.len())], false) {
            Ok(awc) => {
                if let Err(e) = extract_awc(&awc, at, &dir, &ctx, &mut t) {
                    eprintln!("{at:#x}: {e}");
                }
            }
            Err(e) => eprintln!("{at:#x}: {e}"),
        }
        t.awcs += 1;
    }
    if o.json {
        print_json(&obj![
            ("source", input.display().to_string()),
            ("outdir", outdir.display().to_string()),
            ("awcs", t.awcs),
            ("ogg", t.ogg),
            ("wav", t.wav),
            ("xma2_wav", t.xma),
            ("skipped", t.skipped),
            ("outputs", t.results),
        ]);
    } else {
        println!(
            "{} AWC(s): {} .ogg, {} .wav (PCM), {} .wav (XMA2), {} stream(s) skipped -> {}",
            t.awcs,
            t.ogg,
            t.wav,
            t.xma,
            t.skipped,
            outdir.display()
        );
    }
    Ok(())
}

fn write_ogg(path: &Path, packets: &[&[u8]], channels: u8, rate: u32, serial: u32, ctx: &Ctx) -> Res<u64> {
    let file = BufWriter::new(File::create(path)?);
    let mut w = ogg::OggOpusWriter::new(file, channels, ogg::DEFAULT_PRE_SKIP, rate, serial, &ctx.tags)?;
    for p in packets {
        w.write_packet(p)?;
    }
    let (_, granule) = w.finish()?;
    Ok(granule - u64::from(ogg::DEFAULT_PRE_SKIP))
}

/// Decode channel packet lists (each mono or one stereo list) and write one WAV.
/// `skips` drops decoded samples at music block starts.
fn write_wav(path: &Path, dll: &Path, lists: &[(&[&[u8]], &[(usize, u32)])], channels_each: u8, rate: u32) -> Res<(u64, usize)> {
    let mut decoded: Vec<Vec<i16>> = Vec::new();
    let mut bad = 0;
    for (packets, skips) in lists {
        let mut dec = opus::Decoder::new(dll, channels_each)?;
        let mut pcm = Vec::new();
        let mut skip_at = skips.iter().peekable();
        for (i, p) in packets.iter().enumerate() {
            let mut skip = 0usize;
            while let Some(&&(at, s)) = skip_at.peek() {
                if at > i {
                    break;
                }
                if at == i {
                    skip = s as usize;
                }
                skip_at.next();
            }
            let out = dec.decode(p);
            if out.is_empty() {
                bad += 1;
            }
            let from = (skip * usize::from(channels_each)).min(out.len());
            pcm.extend_from_slice(&out[from..]);
        }
        decoded.push(pcm);
    }
    let total_ch = decoded.len() * usize::from(channels_each);
    let frames = decoded.iter().map(|d| d.len() / usize::from(channels_each)).min().unwrap_or(0);
    let mut inter = Vec::with_capacity(frames * total_ch);
    for f in 0..frames {
        for d in &decoded {
            let c = usize::from(channels_each);
            inter.extend_from_slice(&d[f * c..f * c + c]);
        }
    }
    if rate != 48000 {
        let mut r = wav::Resampler::new(total_ch, 48000, rate);
        let mut v = r.process(&inter);
        v.extend(r.flush());
        inter = v;
    }
    let mut w = wav::WavWriter::new(BufWriter::new(File::create(path)?), total_ch as u16, rate)?;
    w.write(&inter)?;
    w.finish(total_ch as u16, rate)?;
    Ok(((inter.len() / total_ch.max(1)) as u64, bad))
}

fn extract_awc(awc: &Awc, at: usize, dir: &Path, ctx: &Ctx, t: &mut Totals) -> Res<()> {
    let o = ctx.o;
    for (index, stream) in awc.streams.iter().enumerate() {
        if o.stream.is_some_and(|s| s != index) {
            continue;
        }
        let Some(data) = stream.chunk(&[TAG_DATA]).and_then(|c| awc.bytes(c)).filter(|d| !d.is_empty()) else {
            continue;
        };
        let name = format!("s{:04}_{:08x}", index + 1, stream.id);
        let fmt = awc.audio_format(stream);
        let sf = awc.stream_format(stream);
        let codec = codec_of(awc, stream);
        let mut result = obj![("awc_offset", at), ("index", index), ("id", format!("{:08x}", stream.id))];
        fs::create_dir_all(dir)?;

        // Non-streamed XMA2 / raw PCM (rpf9-explorer AwcRecovery handling).
        if sf.is_none() && matches!(codec, Some(CODEC_XMA2) | Some(0)) {
            let f = fmt.as_ref().ok_or("stream without format chunk")?;
            let channels = u16::from(f.codec32.map_or(1, |(_, ch)| ch.clamp(1, 8)));
            let len = u32::try_from(data.len())?;
            let path = dir.join(format!("{name}.wav"));
            let mut out = if codec == Some(CODEC_XMA2) && channels <= 2 {
                t.xma += 1;
                result.push("codec", "xma2");
                wav::xma2_header(channels, f.rate.into(), f.samples, len).to_vec()
            } else if codec == Some(0) && u64::from(len) == u64::from(f.samples) * u64::from(channels) * 2 {
                t.wav += 1;
                result.push("codec", "pcm_s16le");
                wav::pcm_header(channels, f.rate.into(), len)
            } else {
                eprintln!("{name}: unsupported codec {:#04x} layout", codec.unwrap_or(0));
                t.skipped += 1;
                continue;
            };
            out.extend_from_slice(data);
            fs::write(&path, out)?;
            result.push("outputs", vec![Json::from(path.display().to_string())]);
            t.results.push(result);
            continue;
        }
        if codec.is_some_and(|c| c != CODEC_OPUS) {
            eprintln!("{name}: codec {:#04x} is not supported", codec.unwrap_or(0));
            t.skipped += 1;
            continue;
        }
        let rate = fmt.as_ref().map(|f| u32::from(f.rate)).filter(|r| (8000..=192_000).contains(r)).unwrap_or(48000);
        let mut outputs = Vec::new();

        // Music streams: de-interleave the per-channel sub-blocks.
        let deint = sf.as_ref().filter(|_| o.block_header.is_none()).and_then(|sf| awc::deinterleave(data, sf.channels.len(), sf.block_size as usize).map(|c| (sf, c)));
        if let Some((sf, chans)) = deint {
            let mut lists = Vec::new();
            for (c, (info, ch)) in sf.channels.iter().zip(&chans).enumerate() {
                if ch.packets.is_empty() {
                    continue;
                }
                let idx = awc.streams.iter().position(|s| s.id == info.id).unwrap_or(awc.streams.len() + c);
                let path = dir.join(format!("{name}_c{c}_s{:04}_{:08x}.ogg", idx + 1, info.id));
                let rate = u32::from(info.rate).clamp(8000, 192_000);
                write_ogg(&path, &ch.packets, 1, rate, 0x5250_4639 ^ info.id, ctx)?;
                outputs.push(Json::from(path.display().to_string()));
                t.ogg += 1;
                lists.push((ch.packets.as_slice(), ch.block_skips.as_slice()));
            }
            if let Some(dll) = &ctx.dll {
                let path = dir.join(format!("{name}.wav"));
                let (frames, bad) = write_wav(&path, dll, &lists, 1, o.rate)?;
                if bad > 0 {
                    eprintln!("{name}: {bad} undecodable packet(s)");
                }
                result.push("wav_frames", frames);
                outputs.push(Json::from(path.display().to_string()));
                t.wav += 1;
            }
            result.push("framing", "music-deinterleaved");
            result.push("channels", sf.channels.len());
            result.push("outputs", outputs);
            t.results.push(result);
            continue;
        }
        if sf.as_ref().is_some_and(|s| s.channels.len() > 2) {
            eprintln!("{name}: {}-channel music stream could not be de-interleaved", sf.as_ref().map_or(0, |s| s.channels.len()));
            t.skipped += 1;
            continue;
        }
        let framing = framing_for(awc, stream, data, o.block_header);
        let packets = awc::packets(&framing, data);
        if packets.is_empty() {
            eprintln!("{name}: no Opus packets");
            t.skipped += 1;
            continue;
        }
        let channels = match sf.as_ref().map(|s| s.channels.len()) {
            Some(n @ 1..=2) => n as u8,
            _ => packets.iter().map(|p| ogg::toc_channels(p)).max().unwrap_or(1),
        };
        let path = dir.join(format!("{name}.ogg"));
        let samples = write_ogg(&path, &packets, channels, rate, 0x5250_4639 ^ stream.id, ctx)?;
        outputs.push(Json::from(path.display().to_string()));
        t.ogg += 1;
        if let Some(dll) = &ctx.dll {
            let path = dir.join(format!("{name}.wav"));
            let (_, bad) = write_wav(&path, dll, &[(packets.as_slice(), &[])], channels, o.rate)?;
            if bad > 0 {
                eprintln!("{name}: {bad} undecodable packet(s)");
            }
            outputs.push(Json::from(path.display().to_string()));
            t.wav += 1;
        }
        result.push("framing", match framing {
            Framing::TwoK => "2k".to_string(),
            Framing::Music(h) => format!("512k@{h:#x}"),
        });
        result.push("channels", channels);
        result.push("packets", packets.len());
        result.push("opus_samples", samples);
        result.push("outputs", outputs);
        t.results.push(result);
    }
    Ok(())
}

// ---------------------------------------------------------------- carve / index / doctor

fn carve_cmd(input: &Path, outdir: Option<&PathBuf>, o: &Opts) -> Res<()> {
    let data = map(input)?;
    let files = carve::carve(&data, !o.no_zlib, o.zlib_all);
    let Some(outdir) = outdir else {
        let mut groups: std::collections::BTreeMap<&str, (usize, u64, &str)> = Default::default();
        for f in &files {
            let g = groups.entry(f.ext).or_insert((0, 0, f.kind));
            g.0 += 1;
            g.1 += f.length as u64;
        }
        if o.json {
            let list: Vec<Json> = files.iter().map(|f| obj![("offset", f.offset), ("length", f.length), ("ext", f.ext), ("kind", f.kind), ("note", f.note.clone()), ("inflated", f.inflated)]).collect();
            print_json(&obj![("name", input.display().to_string()), ("size", data.len()), ("files", list)]);
        } else {
            println!("{}  {}  {} file(s)", input.display(), util::human_size(data.len() as u64), files.len());
            println!("\n  type   files        size  kind");
            for (ext, (n, size, kind)) in groups {
                println!("{ext:>6}  {n:>6}  {:>10}  {kind}", util::human_size(size));
            }
        }
        return Ok(());
    };
    if outdir.exists() && fs::read_dir(outdir)?.next().is_some() && !o.force {
        return Err(format!("{} is not empty (use --force to write into it)", outdir.display()).into());
    }
    fs::create_dir_all(outdir)?;
    let mut entries = Vec::new();
    let mut tsv = String::from("offset\ttype\tsize\tsha256\tduration_seconds\tsample_rate\tchannels\tkind\tpath\n");
    let mut payload = 0u64;
    for f in &files {
        let rel = format!("{:08x}.{}", f.offset, f.ext);
        let path = outdir.join(&rel);
        let mut hasher = util::Sha256::new();
        let mut out = BufWriter::new(File::create(&path)?);
        let written = if f.inflated {
            let mut err = None;
            let r = carve::inflate(&data[f.offset..], 1 << 32, |chunk| {
                hasher.update(chunk);
                out.write_all(chunk).inspect_err(|e| err = Some(e.to_string()))
            });
            if let Some(e) = err {
                return Err(e.into());
            }
            r.ok_or("zlib stream failed to inflate")?.1
        } else {
            let body = &data[f.offset..f.offset + f.length];
            hasher.update(body);
            out.write_all(body)?;
            body.len() as u64
        };
        out.flush()?;
        payload += written;
        let mut entry = obj![("offset", f.offset), ("offset_hex", format!("0x{:08x}", f.offset)), ("type", f.ext), ("kind", f.kind), ("note", f.note.clone())];
        if f.inflated {
            entry.push("inflated", true);
            entry.push("compressed_size", f.length);
        } else {
            carve::describe(&data[f.offset..f.offset + f.length], f.ext, &mut entry);
        }
        let sha = hasher.hex();
        let field = |k: &str| entry.get(k).map_or(String::new(), |v| v.dump(0));
        tsv.push_str(&format!(
            "{:#x}\t{}\t{written}\t{sha}\t{}\t{}\t{}\t{}\t{rel}\n",
            f.offset,
            f.ext,
            field("duration_seconds"),
            field("sample_rate"),
            field("channels"),
            f.kind
        ));
        entry.push("size", written);
        entry.push("sha256", sha);
        entry.push("path", rel);
        entries.push(entry);
    }
    let n = entries.len();
    let index = obj![
        ("schema_version", 1u32),
        ("tool", "rpf9 audio"),
        ("source", obj![("name", input.file_name().map_or(String::new(), |s| s.to_string_lossy().to_string())), ("size", data.len())]),
        ("files", entries),
    ];
    fs::write(outdir.join("index.json"), index.dump(2) + "\n")?;
    fs::write(outdir.join("index.tsv"), tsv)?;
    fs::write(outdir.join("report.txt"), format!("{}\nbytes: {}\nfiles: {n}\npayload bytes: {payload}\n", input.display(), data.len()))?;
    if o.json {
        print_json(&obj![("source", input.display().to_string()), ("outdir", outdir.display().to_string()), ("files", n), ("bytes", payload)]);
    } else {
        println!("wrote {n} files to {}\nindex: {}", outdir.display(), outdir.join("index.json").display());
    }
    Ok(())
}

fn index(path: &Path, o: &Opts) -> Res<()> {
    let file = if path.is_dir() { path.join("index.json") } else { path.to_path_buf() };
    let text = fs::read_to_string(&file).map_err(|e| format!("{}: {e} (run carve first)", file.display()))?;
    let doc = Json::parse(&text)?;
    let Some(Json::Arr(files)) = doc.get("files") else {
        return Err(format!("{}: not an rpf9 audio index", file.display()).into());
    };
    if o.json {
        print_json(&doc);
        return Ok(());
    }
    let source = doc.get("source");
    let size = source.and_then(|s| s.get("size")).and_then(Json::num).unwrap_or(0.0) as u64;
    let bytes: f64 = files.iter().filter_map(|f| f.get("size")?.num()).sum();
    println!("source: {}  {}", source.and_then(|s| s.get("name")).and_then(Json::str).unwrap_or("?"), util::human_size(size));
    println!("files: {}  bytes: {}", files.len(), util::human_size(bytes as u64));
    println!("\n  type   files  seconds");
    for (ext, n) in util::counts(files.iter().map(|f| f.get("type").and_then(Json::str).unwrap_or("?"))) {
        let secs: f64 = files.iter().filter(|f| f.get("type").and_then(Json::str) == Some(ext)).filter_map(|f| f.get("duration_seconds")?.num()).fold(0.0, |a, b| a + b);
        println!("{ext:>6}  {n:>6}  {secs:>7.1}");
    }
    Ok(())
}

fn doctor(o: &Opts) -> Res<()> {
    let formats = [
        "AWC/ADAT container headers and chunk tables (strict carving validation)",
        "Opus packet blocks: 2 KiB (single-channel)",
        "Opus music: 512 KiB blocks, per-channel de-interleave (any channel count)",
        "Ogg Opus remux (no re-encoding) with Vorbis comments and cover art",
        "WAV output: 16-bit PCM via the built-in pure-Rust Opus decoder, 48 kHz or 32.768 kHz, multichannel",
        "XMA2 (non-streamed) wrapped as RIFF/WAVE, raw PCM AWC streams",
        "Carving: AWC, RIFF/WAVE/AVI/WebP, Ogg, PNG, Bink, AMR, zlib streams",
        "Printable strings from AWC metadata/animation/marker chunks",
    ];
    let opus = opus::version(Path::new("")).unwrap_or_default();
    if o.json {
        print_json(&obj![
            ("name", "6ixpack audio"),
            ("version", env!("CARGO_PKG_VERSION")),
            ("platform", obj![("os", std::env::consts::OS), ("arch", std::env::consts::ARCH)]),
            ("opus_decoder", opus.as_str()),
            ("supported_formats", formats.iter().map(|f| Json::from(*f)).collect::<Vec<_>>()),
        ]);
        return Ok(());
    }
    println!("6ixpack audio {}", env!("CARGO_PKG_VERSION"));
    println!("  Platform    {} ({})", std::env::consts::OS, std::env::consts::ARCH);
    println!("  Opus        {opus}");
    println!("  Formats:");
    for f in formats {
        println!("    - {f}");
    }
    Ok(())
}

// ---------------------------------------------------------------- GUI API

/// One AWC bank found in an archive.
pub struct AwcIndex {
    pub at: usize,
    pub len: usize,
    pub streams: usize,
    pub multichannel: bool,
}

/// Every AWC bank (shares the CLI's audio index cache; honours [`crate::progress`]).
pub fn index_awcs(source: &Path, data: &[u8]) -> Vec<AwcIndex> {
    let o = Opts { rate: 48000, min_len: 3, ..Default::default() };
    awcs_in(source, data, &o)
        .into_iter()
        .filter_map(|(at, len)| {
            let awc = Awc::parse(&data[at..at + len], false).ok()?;
            Some(AwcIndex { at, len, streams: awc.streams.len(), multichannel: awc.multichannel() })
        })
        .collect()
}

/// Extract one AWC bank to `dir` (Opus -> .ogg, plus .wav when libopus is
/// found and `wav` is set); returns the files written.
pub fn export_awc(data: &[u8], at: usize, len: usize, dir: &Path, wav: bool) -> Res<Vec<PathBuf>> {
    let o = Opts { rate: 48000, min_len: 3, no_wav: !wav, ..Default::default() };
    let dll = if wav { opus::find(None) } else { None };
    let ctx = Ctx { dll, tags: ogg::build_tags(&[], None)?, o: &o };
    let awc = Awc::parse(&data[at..(at + len).min(data.len())], false)?;
    let mut t = Totals::default();
    let listing = |d: &Path| -> Vec<PathBuf> { fs::read_dir(d).map(|r| r.flatten().map(|e| e.path()).collect()).unwrap_or_default() };
    let before = listing(dir);
    extract_awc(&awc, at, dir, &ctx, &mut t)?;
    let mut files: Vec<PathBuf> = listing(dir).into_iter().filter(|p| !before.contains(p)).collect();
    files.sort();
    Ok(files)
}
