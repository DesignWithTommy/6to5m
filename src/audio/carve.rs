//! Signature-based media carving, zlib stream detection and per-file metadata
//! (port of rpf9-audio carve.py / media.py).

use super::{awc, ogg, util::Json};
use crate::bytes::{be32, le16, le32};
use flate2::{Decompress, FlushDecompress, Status};

pub struct Carved {
    pub offset: usize,
    pub length: usize,
    pub ext: &'static str,
    pub kind: &'static str,
    pub note: String,
    pub inflated: bool,
}

type Media = Option<(usize, &'static str, &'static str, String)>;

pub fn probe_media(buf: &[u8], pos: usize) -> Media {
    let d = &buf[pos..];
    match d.get(..4)? {
        b"ADAT" => {
            let (len, a) = awc::Awc::probe(d)?;
            let mc = if a.multichannel() { ", multichannel" } else { "" };
            Some((len, "awc", "AWC audio bank", format!("{} streams, flags {:#010x}{mc}", a.streams.len(), a.flags)))
        }
        b"RIFF" => riff(d),
        b"OggS" => ogg_stream(d),
        [0x89, b'P', b'N', b'G'] => png(d),
        [b'B', b'I', b'K', _] | [b'K', b'B', b'2', _] => bink(d),
        b"#!AM" => amr(d),
        _ => None,
    }
}

fn riff(d: &[u8]) -> Media {
    let len = le32(d, 4)? as usize + 8;
    if d.len() < 16 || len < 16 || len > d.len() {
        return None;
    }
    match (&d[8..12], &d[12..16]) {
        (b"WAVE", b"fmt ") => {
            let (fmt, ch, rate, bits) = (le16(d, 20)?, le16(d, 22)?, le32(d, 24)?, le16(d, 34)?);
            if ch == 0 || ch > 64 || rate == 0 || rate > 1_000_000 {
                return None;
            }
            Some((len, "wav", "WAVE audio", format!("{ch} ch, {rate} Hz, {bits}-bit, format {fmt:#06x}")))
        }
        (b"AVI ", b"LIST") => Some((len, "avi", "AVI video", String::new())),
        (b"WEBP", _) => Some((len, "webp", "WebP image", String::new())),
        _ => None,
    }
}

/// (page length, header flags, payload offset within page)
fn ogg_page(d: &[u8], pos: usize) -> Option<(usize, u8, usize)> {
    if d.get(pos..pos + 4)? != b"OggS" {
        return None;
    }
    let segs = usize::from(*d.get(pos + 26)?);
    let payload: usize = d.get(pos + 27..pos + 27 + segs)?.iter().map(|&v| usize::from(v)).sum();
    let len = 27 + segs + payload;
    (pos + len <= d.len()).then_some((len, d[pos + 5], 27 + segs))
}

fn ogg_stream(d: &[u8]) -> Media {
    let (len0, flags0, body0) = ogg_page(d, 0)?;
    if flags0 & 2 == 0 {
        return None;
    }
    let body = &d[body0..len0];
    let codec = [(&b"\x01vorbis"[..], "Vorbis"), (b"OpusHead", "Opus"), (b"\x7fFLAC", "FLAC"), (b"\x80theora", "Theora"), (b"Speex   ", "Speex")]
        .iter()
        .find(|(m, _)| body.starts_with(m))
        .map_or("unknown codec", |(_, n)| n);
    let (mut total, mut pages) = (0, 0);
    loop {
        let (len, flags, _) = ogg_page(d, total)?;
        total += len;
        pages += 1;
        if flags & 4 != 0 {
            return Some((total, "ogg", "Ogg stream", format!("{codec}, {pages} pages")));
        }
    }
}

fn png(d: &[u8]) -> Media {
    if !d.starts_with(&[0x89, b'P', b'N', b'G', 13, 10, 26, 10]) || d.get(8..16)? != b"\0\0\0\x0dIHDR" {
        return None;
    }
    let (w, h) = (be32(d, 16)?, be32(d, 20)?);
    if w == 0 || h == 0 || w > 1_000_000 || h > 1_000_000 {
        return None;
    }
    let mut len = 8;
    while len + 12 <= d.len() {
        let size = be32(d, len)? as usize;
        let kind = &d[len + 4..len + 8];
        if size > 256 << 20 || len + 12 + size > d.len() {
            return None;
        }
        len += 12 + size;
        if kind == b"IEND" {
            return Some((len, "png", "PNG image", format!("{w}x{h}")));
        }
    }
    None
}

fn bink(d: &[u8]) -> Media {
    let ext = if d.starts_with(b"BIK") { "bik" } else { "bk2" };
    if !(0x61..=0x7a).contains(d.get(3)?) {
        return None;
    }
    let (len, frames) = (le32(d, 4)? as usize + 8, le32(d, 8)?);
    ((1..=1 << 20).contains(&frames) && len >= 12 && len <= d.len()).then(|| (len, ext, "Bink video", format!("{frames} frames")))
}

fn amr(d: &[u8]) -> Media {
    const NARROW: [usize; 16] = [13, 14, 16, 18, 20, 21, 27, 32, 6, 0, 0, 0, 0, 0, 0, 1];
    const WIDE: [usize; 16] = [18, 24, 33, 37, 41, 47, 51, 59, 61, 6, 0, 0, 0, 0, 1, 1];
    let (ext, name, mut len, sizes) = if d.starts_with(b"#!AMR-WB\n") {
        ("awb", "AMR-WB audio", 9, WIDE)
    } else if d.starts_with(b"#!AMR\n") {
        ("amr", "AMR-NB audio", 6, NARROW)
    } else {
        return None;
    };
    let mut frames = 0;
    while len < d.len() && frames < 10_000_000 {
        let code = d[len];
        let frame = sizes[usize::from(code >> 3 & 0xf)];
        if frame == 0 || code & 0x83 != 0 || len + frame > d.len() {
            break;
        }
        len += frame;
        frames += 1;
    }
    (frames >= 8).then(|| (len, ext, name, format!("{frames} frames")))
}

pub fn zlib_header_ok(d: &[u8], all: bool) -> bool {
    let [cmf, flg, ..] = *d else { return false };
    cmf & 0x0f == 8 && cmf >> 4 <= 7 && flg & 0x20 == 0 && (u16::from(cmf) << 8 | u16::from(flg)) % 31 == 0
        && (all || (cmf == 0x78 && [0x01, 0x5e, 0x9c, 0xda].contains(&flg)))
}

/// Inflate a zlib stream, passing output to `sink`; returns (compressed length, output length).
pub fn inflate(d: &[u8], max_output: u64, mut sink: impl FnMut(&[u8]) -> std::io::Result<()>) -> Option<(usize, u64)> {
    let mut z = Decompress::new(true);
    let mut out = vec![0u8; 1 << 16];
    loop {
        let (before_in, before_out) = (z.total_in(), z.total_out());
        let status = z.decompress(&d[before_in as usize..], &mut out, FlushDecompress::None).ok()?;
        let produced = (z.total_out() - before_out) as usize;
        sink(&out[..produced]).ok()?;
        if z.total_out() > max_output {
            return None;
        }
        match status {
            Status::StreamEnd => return (z.total_out() > 0).then(|| (z.total_in() as usize, z.total_out())),
            _ if z.total_in() == before_in && produced == 0 => return None,
            _ => {}
        }
    }
}

fn inflated_kind(head: &[u8]) -> (&'static str, &'static str) {
    let s = head.strip_prefix(b"\xef\xbb\xbf").unwrap_or(head);
    match () {
        _ if head.starts_with(b"ADAT") => ("awc", "AWC audio bank"),
        _ if head.starts_with(b"RIFF") => ("riff", "RIFF container"),
        _ if head.starts_with(b"OggS") => ("ogg", "Ogg stream"),
        _ if head.starts_with(b"\x89PNG") => ("png", "PNG image"),
        _ if head.starts_with(b"PSIN") => ("pso", "PSO resource"),
        _ if s.starts_with(b"<?xml") => ("xml", "XML"),
        _ => ("bin", "inflated data"),
    }
}

fn carve_range(buf: &[u8], start: usize, end: usize, zlib: bool, zlib_all: bool) -> Vec<Carved> {
    let mut found = Vec::new();
    let mut pos = start;
    while pos < end {
        let b = buf[pos];
        let candidate = matches!(b, b'A' | b'R' | b'O' | 0x89 | b'B' | b'K' | b'#') || (zlib && (b == 0x78 || (zlib_all && b & 0x0f == 8)));
        if !candidate {
            pos += 1;
            continue;
        }
        if let Some((length, ext, kind, note)) = probe_media(buf, pos) {
            found.push(Carved { offset: pos, length, ext, kind, note, inflated: false });
            pos += length.max(1);
            continue;
        }
        if zlib && zlib_header_ok(&buf[pos..], zlib_all) {
            let mut head = Vec::new();
            if let Some((length, _)) = inflate(&buf[pos..], 1 << 30, |o| {
                if head.len() < 64 {
                    head.extend_from_slice(&o[..o.len().min(64 - head.len())]);
                }
                Ok(())
            }) {
                let (ext, kind) = inflated_kind(&head);
                found.push(Carved { offset: pos, length, ext, kind, note: "zlib".into(), inflated: true });
                pos += length;
                continue;
            }
        }
        pos += 1;
    }
    found
}

/// Carve the whole buffer on several threads; overlapping hits keep the earliest.
pub fn carve(buf: &[u8], zlib: bool, zlib_all: bool) -> Vec<Carved> {
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(16);
    let piece = buf.len().div_ceil(threads * 8).max(16 << 20);
    let ranges: Vec<_> = (0..buf.len()).step_by(piece).map(|s| (s, (s + piece).min(buf.len()))).collect();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let mut all: Vec<Carved> = std::thread::scope(|s| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let mut out = Vec::new();
                    while let Some(&(a, b)) = ranges.get(next.fetch_add(1, std::sync::atomic::Ordering::Relaxed)) {
                        out.extend(carve_range(buf, a, b, zlib, zlib_all));
                    }
                    out
                })
            })
            .collect();
        workers.into_iter().flat_map(|w| w.join().unwrap_or_default()).collect()
    });
    all.sort_by_key(|c| c.offset);
    let mut out: Vec<Carved> = Vec::new();
    for c in all {
        if out.last().is_none_or(|l| c.offset >= l.offset + l.length) {
            out.push(c);
        }
    }
    out
}

/// Best-effort audio metadata for a carved file (media.py `describe`).
pub fn describe(d: &[u8], ext: &str, entry: &mut Json) {
    match ext {
        "awc" => describe_awc(d, entry),
        "wav" | "riff" => describe_wav(d, entry),
        "ogg" => describe_ogg(d, entry),
        _ => {}
    }
}

fn describe_wav(d: &[u8], e: &mut Json) {
    if d.len() < 12 || &d[..4] != b"RIFF" || &d[8..12] != b"WAVE" {
        return;
    }
    let (mut pos, mut fmt, mut data) = (12, None, None);
    while let (Some(size), Some(kind)) = (le32(d, pos + 4), d.get(pos..pos + 4)) {
        let (size, body) = (size as usize, pos + 8);
        if body + size > d.len() {
            break;
        }
        if kind == b"fmt " && size >= 16 && le16(d, body) == Some(1) {
            fmt = Some((le16(d, body + 2).unwrap_or(0), le32(d, body + 4).unwrap_or(0), le16(d, body + 14).unwrap_or(0)));
        } else if kind == b"data" {
            data = Some(size);
        }
        pos = body + size + (size & 1);
    }
    if let Some((ch, rate, 16)) = fmt.filter(|f| f.0 > 0 && f.1 > 0) {
        e.push("sample_rate", rate);
        e.push("channels", ch);
        e.push("codec", "pcm_s16le");
        if let Some(n) = data {
            e.push("duration_seconds", n as f64 / (f64::from(rate) * f64::from(ch) * 2.0));
        }
    }
}

fn describe_ogg(d: &[u8], e: &mut Json) {
    let Some((_, _, body)) = ogg_page(d, 0) else { return };
    let head = &d[body..];
    if !head.starts_with(b"OpusHead") || head.len() < 19 {
        return;
    }
    let (channels, pre_skip) = (head[9], u64::from(le16(head, 10).unwrap_or(0)));
    let tail = &d[d.len().saturating_sub(1 << 16)..];
    let Some(rel) = tail.windows(4).rposition(|w| w == b"OggS") else { return };
    let granule = tail.get(rel + 6..rel + 14).map_or(0, |b| u64::from_le_bytes(b.try_into().unwrap_or([0; 8])));
    e.push("codec", "opus");
    e.push("sample_rate", 48000u32);
    e.push("channels", channels);
    e.push("pre_skip", pre_skip);
    if granule > pre_skip {
        e.push("duration_seconds", (granule - pre_skip) as f64 / 48000.0);
    }
}

fn describe_awc(d: &[u8], e: &mut Json) {
    let Ok(a) = awc::Awc::parse(d, false) else { return };
    e.push("codec", "opus");
    for stream in &a.streams {
        let Some(data) = stream.chunk(&[awc::TAG_DATA]).and_then(|c| a.bytes(c)) else { continue };
        let fmt = a.audio_format(stream);
        let sf = a.stream_format(stream);
        let rate = fmt.as_ref().map(|f| u32::from(f.rate)).or(sf.as_ref().and_then(|s| s.channels.first()).map(|c| u32::from(c.rate)));
        let rate = rate.filter(|r| (8000..=192_000).contains(r)).unwrap_or(48000);
        let samples = fmt.as_ref().map(|f| f.samples).or(sf.as_ref().and_then(|s| s.channels.first()).map(|c| c.samples)).unwrap_or(0);
        let channels = match &sf {
            Some(s) => s.channels.len(),
            None => usize::from(awc::packets_2k(data).first().map_or(1, |p| ogg::toc_channels(p))),
        };
        e.push("channels", channels);
        e.push("sample_rate", rate);
        e.push("streams", a.streams.len());
        if samples > 0 {
            e.push("duration_seconds", f64::from(samples) / f64::from(rate));
        }
        break;
    }
}
