//! Content sniffing for carved entries (no names are available while the TOC is encrypted).
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// RAGE resource with a "PRD0" header at 0x10 (type id in `sub`).
    Resource,
    /// RAGE resource without PRD0 tag (pointers into 0x5xxxxxxx).
    RageObj,
    Xml,
    /// RAGE binary XML ("RBF0").
    Rbf,
    /// RAGE PSO meta ("PSIN").
    Pso,
    /// Scaleform movie ("GFX").
    Gfx,
    Dds,
    Png,
    Text,
    Bin,
}

impl Kind {
    pub fn all() -> &'static [Kind] {
        use Kind::*;
        &[Resource, RageObj, Xml, Rbf, Pso, Gfx, Dds, Png, Text, Bin]
    }
    pub fn name(&self) -> String {
        format!("{:?}", self).to_lowercase()
    }
}

fn u32le(b: &[u8], o: usize) -> u32 {
    if b.len() < o + 4 {
        return 0;
    }
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

/// Returns (kind, sub-type string, file extension).
pub fn classify(h: &[u8]) -> (Kind, String, String) {
    let s = |k: Kind, sub: &str, ext: &str| (k, sub.to_string(), ext.to_string());
    let body = if h.starts_with(&[0xef, 0xbb, 0xbf]) { &h[3..] } else { h };
    if body.starts_with(b"<?xml") || body.starts_with(b"<") && body.len() > 1 && body[1].is_ascii_alphabetic() {
        return s(Kind::Xml, "", "xml");
    }
    if h.starts_with(b"RBF0") {
        return s(Kind::Rbf, "", "rbf");
    }
    if h.starts_with(b"PSIN") {
        return s(Kind::Pso, "", "pso");
    }
    if h.starts_with(b"GFX") || h.starts_with(b"CFX") {
        return s(Kind::Gfx, "", "gfx");
    }
    if h.starts_with(b"DDS ") {
        return s(Kind::Dds, "", "dds");
    }
    if h.starts_with(b"\x89PNG") {
        return s(Kind::Png, "", "png");
    }
    if h.len() >= 0x18 && &h[0x10..0x14] == b"0DRP" {
        let t = u32le(h, 0x14);
        return (Kind::Resource, format!("{:x}", t), format!("r{:x}", t));
    }
    let p = u32le(h, 8);
    if h.len() >= 16 && (p & 0xF000_0000) == 0x5000_0000 && u32le(h, 12) == 0 {
        return s(Kind::RageObj, "", "rsc");
    }
    let printable = h.iter().take(128).filter(|&&c| c == b'\n' || c == b'\r' || c == b'\t' || (0x20..0x7f).contains(&c)).count();
    if h.len() >= 32 && printable * 100 / h.len().min(128) > 95 {
        return s(Kind::Text, "", "txt");
    }
    (Kind::Bin, format!("{:08x}", u32le(h, 0)), "bin".into())
}

/// Refine the classification of a grouped entry (needs the stream layout).
pub fn refine(e: &mut crate::scan::Entry) {
    if e.kind == Kind::Bin && e.streams.len() == 1 && e.usize >= 0x10000 {
        // stand-alone graphics page or raw data; leave as bin
    }
    if e.kind == Kind::RageObj {
        e.ext = "rsc".into();
    }
}

/// Heuristic test for a texture dictionary system page (pgDictionary<grcTexture>).
pub fn looks_like_txd(sys: &[u8]) -> bool {
    if sys.len() < 0x40 {
        return false;
    }
    let q = |o: usize| u64::from_le_bytes(sys[o..o + 8].try_into().unwrap());
    let h = |o: usize| u16::from_le_bytes([sys[o], sys[o + 1]]);
    q(0x10) == 0
        && q(0x20) >> 28 == 5
        && q(0x30) >> 28 == 5
        && h(0x28) > 0
        && h(0x28) == h(0x38)
        && h(0x2a) == h(0x3a)
}
