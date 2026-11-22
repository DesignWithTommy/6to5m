//! Persistent index of a full archive scan (`Scan::new`), so repeat runs of
//! `rpf9 [-n] <file>` skip the walk. Stored under %LOCALAPPDATA%/rpf9/scan
//! (or $XDG_CACHE_HOME / ~/.cache) and valid only while the archive's
//! canonical path, length, mtime and first 64 bytes match.

use crate::{
    carve::Format,
    container::{Archive, Header},
    scan::{Coverage, File, Kind, Part, Scan},
};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs,
    io::{self, Write},
    ops::Range,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

const MAGIC: &[u8; 8] = b"R9SCAN01";

fn root(ns: &str) -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("XDG_CACHE_HOME"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".cache")))?;
    Some(base.join("rpf9").join(ns))
}

/// Identity of the archive: canonical path, length, mtime, first 64 bytes.
pub(crate) fn stamp(source: &Path, data: &[u8], ns: &str, tag: &str) -> Option<(PathBuf, Vec<u8>)> {
    let full = fs::canonicalize(source).ok()?;
    let meta = fs::metadata(&full).ok()?;
    let mtime = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_nanos();
    let name = full.to_string_lossy().to_uppercase();
    let mut h = Sha256::new();
    h.update(name.as_bytes());
    let id: String = h.finalize()[..12].iter().map(|b| format!("{b:02x}")).collect();
    let mut s = Vec::new();
    s.extend_from_slice(MAGIC);
    put_bytes(&mut s, name.as_bytes());
    put(&mut s, meta.len() as usize);
    s.extend_from_slice(&mtime.to_le_bytes());
    put_bytes(&mut s, &data[..data.len().min(64)]);
    put_bytes(&mut s, tag.as_bytes());
    Some((root(ns)?.join(format!("{id}{tag}.idx")), s))
}

/// Loads the cached scan when it is valid, otherwise scans and stores it.
pub fn scan<'a>(source: &Path, data: &'a [u8], use_cache: bool) -> Scan<'a> {
    let stamp = if use_cache { stamp(source, data, "scan", "") } else { None };
    if let Some((path, head)) = &stamp {
        if let Some(scan) = fs::read(path).ok().and_then(|bytes| load(&bytes, head, data)) {
            return scan;
        }
    }
    let scan = Scan::new(data);
    if let Some((path, head)) = &stamp {
        let _ = store(path, head, &scan);
    }
    scan
}

/// Writes `bytes` atomically (temp file + rename).
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    fs::create_dir_all(path.parent().unwrap_or(path))?;
    let tmp = path.with_extension("tmp");
    fs::File::create(&tmp)?.write_all(bytes)?;
    fs::rename(tmp, path)
}

fn store(path: &Path, head: &[u8], scan: &Scan) -> io::Result<()> {
    let mut b = head.to_vec();
    put(&mut b, scan.archives.len());
    for a in &scan.archives {
        let h = &a.header;
        for v in [h.at, h.len, h.split, h.trailer] {
            put(&mut b, v);
        }
        b.extend_from_slice(&h.id.to_le_bytes());
        b.extend_from_slice(&h.files.to_le_bytes());
        let ([u0, u1], u2, [u3, u4]) = h.unknown;
        for v in [u0, u1] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(&u2.to_le_bytes());
        for v in [u3, u4] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        put_range(&mut b, &a.body);
        put_range(&mut b, &a.trailer);
    }
    put(&mut b, scan.files.len());
    for f in &scan.files {
        put_range(&mut b, &f.span);
        match &f.kind {
            Kind::Resource => b.push(0),
            Kind::Unknown => b.push(1),
            Kind::Known(fmt) => {
                b.push(2);
                put_bytes(&mut b, fmt.ext.as_bytes());
                put_bytes(&mut b, fmt.name.as_bytes());
                put_bytes(&mut b, fmt.note.as_bytes());
            }
        }
        put(&mut b, f.parts.len());
        for p in &f.parts {
            put_range(&mut b, &p.range);
            put(&mut b, p.inflated.map_or(usize::MAX, |n| n));
        }
    }
    let c = &scan.coverage;
    for v in [c.structure, c.zlib, c.media, c.stored, c.padding, c.unexplained] {
        put(&mut b, v);
    }
    write_atomic(path, &b)
}

fn load<'a>(bytes: &[u8], head: &[u8], data: &'a [u8]) -> Option<Scan<'a>> {
    let mut r = Reader { b: bytes.strip_prefix(head)?, strings: HashMap::new() };
    let n = r.len(data.len())?;
    let mut archives = Vec::with_capacity(n);
    for _ in 0..n {
        let [at, len, split, trailer] = [r.usize()?, r.usize()?, r.usize()?, r.usize()?];
        let id = u64::from_le_bytes(r.take(8)?.try_into().ok()?);
        let files = u32::from_le_bytes(r.take(4)?.try_into().ok()?);
        let u16s = |r: &mut Reader| Some(u16::from_le_bytes(r.take(2)?.try_into().ok()?));
        let (u0, u1) = (u16s(&mut r)?, u16s(&mut r)?);
        let u2 = u32::from_le_bytes(r.take(4)?.try_into().ok()?);
        let (u3, u4) = (u16s(&mut r)?, u16s(&mut r)?);
        let header = Header { at, len, split, trailer, id, files, unknown: ([u0, u1], u2, [u3, u4]) };
        let (body, trailer) = (r.range(data.len())?, r.range(data.len())?);
        archives.push(Archive { header, body, trailer });
    }
    let n = r.len(data.len())?;
    let mut files = Vec::with_capacity(n);
    for _ in 0..n {
        let span = r.range(data.len())?;
        let kind = match r.take(1)?[0] {
            0 => Kind::Resource,
            1 => Kind::Unknown,
            2 => {
                let (ext, name) = (r.str()?, r.str()?);
                let note = String::from_utf8(r.bytes()?.to_vec()).ok()?;
                Kind::Known(Format { ext, name, note })
            }
            _ => return None,
        };
        let parts_n = r.len(data.len())?;
        let mut parts = Vec::with_capacity(parts_n);
        for _ in 0..parts_n {
            let range = r.range(data.len())?;
            let inflated = Some(r.usize()?).filter(|&v| v != usize::MAX);
            parts.push(Part { range, inflated });
        }
        files.push(File { span, parts, kind });
    }
    let coverage = Coverage {
        structure: r.usize()?,
        zlib: r.usize()?,
        media: r.usize()?,
        stored: r.usize()?,
        padding: r.usize()?,
        unexplained: r.usize()?,
    };
    r.b.is_empty().then_some(Scan { data, archives, files, coverage })
}

fn put(b: &mut Vec<u8>, v: usize) {
    b.extend_from_slice(&(v as u64).to_le_bytes());
}
fn put_range(b: &mut Vec<u8>, r: &Range<usize>) {
    put(b, r.start);
    put(b, r.end);
}
fn put_bytes(b: &mut Vec<u8>, s: &[u8]) {
    put(b, s.len());
    b.extend_from_slice(s);
}

struct Reader<'b> {
    b: &'b [u8],
    strings: HashMap<Vec<u8>, &'static str>,
}

impl<'b> Reader<'b> {
    fn take(&mut self, n: usize) -> Option<&'b [u8]> {
        let (head, rest) = self.b.split_at_checked(n)?;
        self.b = rest;
        Some(head)
    }
    fn usize(&mut self) -> Option<usize> {
        usize::try_from(u64::from_le_bytes(self.take(8)?.try_into().ok()?)).ok()
    }
    /// A count, bounded so a corrupt index can't trigger a huge allocation.
    fn len(&mut self, limit: usize) -> Option<usize> {
        self.usize().filter(|&n| n <= limit)
    }
    fn range(&mut self, limit: usize) -> Option<Range<usize>> {
        let (s, e) = (self.usize()?, self.usize()?);
        (s <= e && e <= limit).then_some(s..e)
    }
    fn bytes(&mut self) -> Option<&'b [u8]> {
        let n = self.len(1 << 20)?;
        self.take(n)
    }
    /// Labels are a handful of distinct values; intern them as 'static.
    fn str(&mut self) -> Option<&'static str> {
        let raw = self.bytes()?;
        if let Some(s) = self.strings.get(raw) {
            return Some(s);
        }
        let s: &'static str = Box::leak(std::str::from_utf8(raw).ok()?.to_owned().into_boxed_str());
        self.strings.insert(raw.to_vec(), s);
        Some(s)
    }
}
