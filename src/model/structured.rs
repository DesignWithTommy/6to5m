//! PSIN / XML metadata recovery from the region right after an archive's
//! tables (port of Rpf9StructuredRecovery.cs).

use crate::{bytes::le32, zlib::Inflater};
use std::collections::{HashMap, HashSet};

const MAX_SCAN: usize = 16 << 20;
const MAX_OUTPUT: usize = 16 << 20;
const BUDGET: usize = 128 << 20;
const XML_BLOCK: usize = 65536;
const MAX_STREAMS: usize = 16_384;
const MAX_ENTRIES: usize = 2048;
const MAX_DEPTH: usize = 128;

pub struct Entry {
    pub offset: usize,
    pub packed: usize,
    pub kind: &'static str,
    pub root: String,
    pub data: Vec<u8>,
}

struct Stream {
    offset: usize,
    packed: usize,
    data: Vec<u8>,
}

fn zlib_header(b: &[u8]) -> bool {
    matches!(b, [cmf, flg, ..] if cmf & 15 == 8 && cmf >> 4 <= 7 && flg & 32 == 0
        && (u16::from(*cmf) << 8 | u16::from(*flg)) % 31 == 0)
}

fn prefix_text(p: &[u8]) -> String {
    let p = &p[..p.len().min(512)];
    let utf16 = |be: bool, b: &[u8]| {
        let units: Vec<u16> =
            b.chunks_exact(2).map(|c| if be { u16::from_be_bytes([c[0], c[1]]) } else { u16::from_le_bytes([c[0], c[1]]) }).collect();
        String::from_utf16_lossy(&units)
    };
    let text = match p {
        [0xFF, 0xFE, ..] | [b'<', 0, ..] => utf16(false, p),
        [0xFE, 0xFF, ..] | [0, b'<', ..] => utf16(true, p),
        _ => String::from_utf8_lossy(p).into_owned(),
    };
    text.trim_start_matches(['\u{FEFF}', ' ', '\t', '\r', '\n']).to_owned()
}

fn looks_xml(p: &[u8]) -> bool {
    prefix_text(p).starts_with('<')
}
fn declares_xml(p: &[u8]) -> bool {
    prefix_text(p).starts_with("<?xml")
}
fn looks_psin(p: &[u8]) -> bool {
    p.len() >= 8 && p.starts_with(b"PSIN")
}

/// Minimal well-formedness check: one root element, balanced tags, no DTD.
/// Returns the root element name.
fn validate_xml(bytes: &[u8]) -> Option<String> {
    let text = match bytes {
        [0xFF, 0xFE, rest @ ..] => String::from_utf16(&rest.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect::<Vec<_>>()).ok()?,
        [0xFE, 0xFF, rest @ ..] => String::from_utf16(&rest.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect::<Vec<_>>()).ok()?,
        _ => String::from_utf8(bytes.to_vec()).ok()?,
    };
    let mut rest = text.trim_start_matches('\u{FEFF}');
    let mut stack: Vec<&str> = Vec::new();
    let (mut root, mut closed) = (None, false);
    while let Some(lt) = rest.find('<') {
        let between = &rest[..lt];
        if (stack.is_empty() && !between.trim().is_empty()) || (closed && !between.trim().is_empty()) {
            return None;
        }
        rest = &rest[lt..];
        let skip = |rest: &mut &str, end: &str| -> Option<()> {
            let at = rest.find(end)?;
            *rest = &rest[at + end.len()..];
            Some(())
        };
        if rest.starts_with("<?") {
            skip(&mut rest, "?>")?;
        } else if rest.starts_with("<!--") {
            skip(&mut rest, "-->")?;
        } else if rest.starts_with("<![CDATA[") {
            if stack.is_empty() {
                return None;
            }
            skip(&mut rest, "]]>")?;
        } else if rest.starts_with("<!") {
            return None; // DTDs are prohibited
        } else {
            let gt = rest.find('>')?;
            let tag = &rest[1..gt];
            rest = &rest[gt + 1..];
            if let Some(name) = tag.strip_prefix('/') {
                if stack.pop()? != name.trim() {
                    return None;
                }
                closed |= stack.is_empty();
            } else {
                if closed {
                    return None;
                }
                let empty = tag.ends_with('/');
                let name = tag.trim_end_matches('/').split(|c: char| c.is_whitespace()).next()?;
                if name.is_empty() {
                    return None;
                }
                if stack.is_empty() {
                    root = Some(name.to_owned());
                }
                if empty {
                    closed |= stack.is_empty();
                } else {
                    stack.push(name);
                    if stack.len() > MAX_DEPTH {
                        return None;
                    }
                }
            }
        }
    }
    (closed && stack.is_empty() && rest.trim().is_empty()).then_some(root?)
}

pub fn recover(data: &[u8]) -> Vec<Entry> {
    let mut entries = Vec::new();
    let Some(metadata_end) = le32(data, 4).map(|v| v as usize) else { return entries };
    if data.len() < 64 || &data[..4] != b"9FPR" || metadata_end < 64 || metadata_end > data.len() || metadata_end > 16 << 20 {
        return entries;
    }
    let start = metadata_end;
    let sample = &data[start..data.len().min(start + MAX_SCAN)];
    let (mut budget, mut streams) = (0usize, Vec::new());
    {
        let step = |inflater: &mut Inflater, p: usize| {
            let rest = &sample[p..];
            if !zlib_header(rest) {
                return None;
            }
            let packed = inflater.inflate(rest)?;
            if inflater.out.len() < 8 || inflater.out.len() > MAX_OUTPUT {
                return None;
            }
            Some((p + packed, Stream { offset: p, packed, data: std::mem::take(&mut inflater.out) }))
        };
        crate::par::greedy(0..sample.len().saturating_sub(5), 1, 1 << 20, Inflater::new, step, |_, stream| {
            budget += stream.data.len();
            streams.push(stream);
            streams.len() < MAX_STREAMS && budget < BUDGET
        });
    }
    let by_offset: HashMap<usize, usize> = streams.iter().enumerate().map(|(i, s)| (s.offset, i)).collect();
    let mut consumed = HashSet::new();
    for (index, first) in streams.iter().enumerate() {
        if entries.len() >= MAX_ENTRIES {
            break;
        }
        if consumed.contains(&first.offset) {
            continue;
        }
        let psin = looks_psin(&first.data);
        if !psin && !looks_xml(&first.data) {
            continue;
        }
        let validate = |blocks: &[usize]| -> Option<String> {
            let joined: Vec<u8> = blocks.iter().flat_map(|&b| streams[b].data.iter().copied()).collect();
            if psin { super::psin::inspect(&joined).map(|_| "PSIN".to_owned()) } else { validate_xml(&joined) }
        };
        let mut blocks = vec![index];
        let mut root = validate(&blocks);
        if root.is_none() && first.data.len() == XML_BLOCK && (psin || declares_xml(&first.data)) {
            let (mut current, mut total) = (index, first.data.len());
            while streams[current].data.len() == XML_BLOCK && blocks.len() <= MAX_OUTPUT / XML_BLOCK {
                let end = streams[current].offset + streams[current].packed;
                let next = end.next_multiple_of(16);
                let Some(&cont) = by_offset.get(&next) else { break };
                let c = &streams[cont];
                if next > sample.len()
                    || sample[end..next].iter().any(|&b| b != 0)
                    || consumed.contains(&next)
                    || declares_xml(&c.data)
                    || (psin && looks_psin(&c.data))
                    || c.data.len() > XML_BLOCK
                    || c.data.len() > MAX_OUTPUT - total
                {
                    break;
                }
                blocks.push(cont);
                total += c.data.len();
                current = cont;
            }
            if blocks.len() > 1 {
                root = validate(&blocks);
            }
        }
        let Some(root) = root else { continue };
        for &b in &blocks {
            consumed.insert(streams[b].offset);
        }
        entries.push(Entry {
            offset: start + first.offset,
            packed: blocks.iter().map(|&b| streams[b].packed).sum(),
            kind: if psin { "psin" } else { "xml" },
            root: root.chars().take(64).map(|c| if c.is_ascii_alphanumeric() || "_-".contains(c) { c } else { '_' }).collect(),
            data: blocks.iter().flat_map(|&b| streams[b].data.iter().copied()).collect(),
        });
    }
    entries
}
