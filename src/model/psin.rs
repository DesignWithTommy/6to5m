//! PSIN metadata container check (port of PsinMetadata.cs).

const MAX_LEN: usize = 16 << 20;
const TAGS: [&[u8; 4]; 7] = [b"PSIN", b"LSCH", b"LENM", b"LSNS", b"LGID", b"LRSC", b"CHKS"];

pub struct Section {
    pub tag: &'static str,
    pub offset: usize,
    pub len: usize,
}

pub struct Psin {
    pub sections: Vec<Section>,
    pub checksum: u32,
    pub platform: u32,
}

fn be32(d: &[u8], at: usize) -> Option<u32> {
    crate::bytes::be32(d, at)
}

pub fn inspect(data: &[u8]) -> Option<Psin> {
    if data.len() < 68 || data.len() > MAX_LEN || !data.starts_with(b"PSIN") {
        return None;
    }
    let mut sections = Vec::with_capacity(TAGS.len());
    let mut at = 0;
    for (i, tag) in TAGS.iter().enumerate() {
        if at + 8 > data.len() || &data[at..at + 4] != *tag {
            return None;
        }
        let len = be32(data, at + 4)? as usize;
        if len < 8 || len > data.len() - at || (i == TAGS.len() - 1 && len != 20) {
            return None;
        }
        sections.push(Section { tag: std::str::from_utf8(*tag).ok()?, offset: at, len });
        at += len;
    }
    if at != data.len() {
        return None;
    }
    let trailer = sections.last()?.offset;
    if be32(data, trailer + 8)? as usize != data.len() {
        return None;
    }
    Some(Psin { sections, checksum: be32(data, trailer + 12)?, platform: be32(data, trailer + 16)? })
}
