//! Ogg Opus muxing: AWC packets are copied verbatim, only OpusHead/OpusTags are synthesized.

use std::io::{self, Write};

pub const DEFAULT_PRE_SKIP: u16 = 312;
const MAX_PAGE_BYTES: usize = 4096;

fn crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    for (i, slot) in table.iter_mut().enumerate() {
        let mut v = (i as u32) << 24;
        for _ in 0..8 {
            v = if v & 0x8000_0000 != 0 { (v << 1) ^ 0x04c1_1db7 } else { v << 1 };
        }
        *slot = v;
    }
    table
}

/// Opus TOC stereo flag.
pub fn toc_channels(packet: &[u8]) -> u8 {
    if packet.first().is_some_and(|t| t & 4 != 0) { 2 } else { 1 }
}

/// Samples per channel at 48 kHz represented by one Opus packet.
pub fn packet_samples(packet: &[u8]) -> u64 {
    let Some(&toc) = packet.first() else { return 0 };
    let config = toc >> 3;
    // Frame size in units of 2.5 ms.
    let units: u64 = if config < 12 {
        [4, 8, 16, 24][(config & 3) as usize]
    } else if config < 16 {
        [4, 8][(config & 1) as usize]
    } else {
        [1, 2, 4, 8][(config & 3) as usize]
    };
    let frames = match toc & 3 {
        0 => 1,
        1 | 2 => 2,
        _ => packet.get(1).map_or(0, |b| u64::from(b & 0x3f)),
    };
    frames * units * 120
}

pub struct OggOpusWriter<W: Write> {
    out: W,
    crc: [u32; 256],
    serial: u32,
    sequence: u32,
    granule: u64,
    pending: Vec<u8>,
    lacing: Vec<u8>,
}

impl<W: Write> OggOpusWriter<W> {
    pub fn new(out: W, channels: u8, pre_skip: u16, input_rate: u32, serial: u32, tags: &[u8]) -> io::Result<Self> {
        let mut w = Self { out, crc: crc_table(), serial, sequence: 0, granule: u64::from(pre_skip), pending: Vec::new(), lacing: Vec::new() };
        let mut head = b"OpusHead\x01".to_vec();
        head.push(channels);
        head.extend(pre_skip.to_le_bytes());
        head.extend(input_rate.to_le_bytes());
        head.extend([0, 0, 0]);
        w.page(&head, &lacing(head.len()), 0x02, 0)?;
        // OpusTags may exceed one page (cover art): split into continued pages.
        let lace = lacing(tags.len());
        let (mut at, mut first) = (0, true);
        for seg in lace.chunks(255) {
            let n: usize = seg.iter().map(|&v| usize::from(v)).sum();
            w.page(&tags[at..at + n], seg, if first { 0 } else { 1 }, 0)?;
            at += n;
            first = false;
        }
        Ok(w)
    }

    fn page(&mut self, payload: &[u8], lacing: &[u8], header_type: u8, granule: u64) -> io::Result<()> {
        let mut page = Vec::with_capacity(27 + lacing.len() + payload.len());
        page.extend(b"OggS\x00");
        page.push(header_type);
        page.extend(granule.to_le_bytes());
        page.extend(self.serial.to_le_bytes());
        page.extend(self.sequence.to_le_bytes());
        page.extend([0; 4]);
        page.push(lacing.len() as u8);
        page.extend(lacing);
        page.extend(payload);
        let crc = page.iter().fold(0u32, |c, &b| (c << 8) ^ self.crc[((c >> 24) as u8 ^ b) as usize]);
        page[22..26].copy_from_slice(&crc.to_le_bytes());
        self.sequence += 1;
        self.out.write_all(&page)
    }

    fn flush_page(&mut self, eos: bool) -> io::Result<()> {
        if self.lacing.is_empty() && !eos {
            return Ok(());
        }
        let (payload, lace) = (std::mem::take(&mut self.pending), std::mem::take(&mut self.lacing));
        self.page(&payload, &lace, if eos { 0x04 } else { 0 }, self.granule)
    }

    pub fn write_packet(&mut self, packet: &[u8]) -> io::Result<()> {
        let lace = lacing(packet.len());
        if self.lacing.len() + lace.len() > 255 || self.pending.len() + packet.len() > MAX_PAGE_BYTES {
            self.flush_page(false)?;
        }
        self.pending.extend(packet);
        self.lacing.extend(lace);
        self.granule += packet_samples(packet);
        Ok(())
    }

    /// Finish the stream; returns the writer and the final granule position.
    pub fn finish(mut self) -> io::Result<(W, u64)> {
        self.flush_page(true)?;
        self.out.flush()?;
        Ok((self.out, self.granule))
    }
}

fn lacing(mut len: usize) -> Vec<u8> {
    let mut v = vec![255; len / 255];
    len %= 255;
    v.push(len as u8);
    v
}

/// Build an OpusTags packet from `KEY=value` comments and an optional cover image.
pub fn build_tags(comments: &[(String, String)], cover: Option<&[u8]>) -> Result<Vec<u8>, String> {
    let vendor = b"rpf9";
    let mut entries: Vec<Vec<u8>> = Vec::new();
    for (k, v) in comments {
        if k.is_empty() || !k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return Err(format!("invalid comment name {k:?}"));
        }
        entries.push(format!("{}={v}", k.to_ascii_uppercase()).into_bytes());
    }
    if let Some(image) = cover {
        let (mime, w, h) = detect_image(image).ok_or("cover is not PNG, JPEG, GIF or WebP")?;
        let mut block = Vec::new();
        block.extend(3u32.to_be_bytes()); // front cover
        block.extend((mime.len() as u32).to_be_bytes());
        block.extend(mime.as_bytes());
        block.extend(0u32.to_be_bytes()); // empty description
        for v in [w, h, 24, 0, image.len() as u32] {
            block.extend(v.to_be_bytes());
        }
        block.extend(image);
        entries.push(format!("METADATA_BLOCK_PICTURE={}", crate::audio::util::base64(&block)).into_bytes());
    }
    let mut tags = b"OpusTags".to_vec();
    tags.extend((vendor.len() as u32).to_le_bytes());
    tags.extend(vendor);
    tags.extend((entries.len() as u32).to_le_bytes());
    for e in entries {
        tags.extend((e.len() as u32).to_le_bytes());
        tags.extend(e);
    }
    Ok(tags)
}

/// (mime, width, height) for common image formats.
pub fn detect_image(d: &[u8]) -> Option<(&'static str, u32, u32)> {
    let be16 = |at: usize| d.get(at..at + 2).map(|b| u32::from(u16::from_be_bytes([b[0], b[1]])));
    let be32 = |at: usize| d.get(at..at + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
    if d.starts_with(&[0x89, b'P', b'N', b'G', 13, 10, 26, 10]) {
        return Some(("image/png", be32(16)?, be32(20)?));
    }
    if d.starts_with(b"GIF8") {
        let le16 = |at: usize| d.get(at..at + 2).map(|b| u32::from(u16::from_le_bytes([b[0], b[1]])));
        return Some(("image/gif", le16(6)?, le16(8)?));
    }
    if d.starts_with(b"RIFF") && d.get(8..12) == Some(b"WEBP") {
        return Some(("image/webp", 0, 0));
    }
    if d.starts_with(&[0xff, 0xd8]) {
        let mut p = 2;
        while p + 9 < d.len() && d[p] == 0xff {
            let marker = d[p + 1];
            if (0xc0..=0xcf).contains(&marker) && ![0xc4, 0xc8, 0xcc].contains(&marker) {
                return Some(("image/jpeg", be16(p + 7)?, be16(p + 5)?));
            }
            p += 2 + be16(p + 2)? as usize;
        }
        return Some(("image/jpeg", 0, 0));
    }
    None
}
