//! AWC ("ADAT") container parsing and Opus packet framing.
//!
//! Layout (little-endian):
//!   "ADAT" | flags | stream_count | header_length
//!   [u32 word]                   flags & 0x00400000
//!   [u16 first_tag per stream]   flags & 0x00010000
//!   [u32 * (streams + 1)]        flags & 0x00200000
//!   [u32 id:29 | tag_count:3] * streams
//!   [u64 type:8 | size:28 | offset:28] * tags

use crate::bytes::{le16, le32, le64};

pub const KNOWN_FLAGS: [u32; 4] = [0xff02_0001, 0xff21_0001, 0xff25_0001, 0xff61_0001];
const FLAG_FIRST_TAGS: u32 = 0x0001_0000;
const FLAG_EXTRA_TABLE: u32 = 0x0020_0000;
const FLAG_WORD: u32 = 0x0040_0000;
pub const FLAG_MULTICHANNEL: u32 = 0x0004_0000;

pub const TAG_DATA: u8 = 0x55;
pub const TAG_FORMAT: u8 = 0x76;
pub const TAG_FORMAT_ALT: u8 = 0xfa;
pub const TAG_STREAM_FORMAT: u8 = 0x48;

pub const CODEC_XMA2: u8 = 0x05;
pub const CODEC_OPUS: u8 = 0x0b;

pub fn chunk_name(kind: u8) -> String {
    match kind {
        0x55 => "data",
        0x76 => "format",
        0xfa => "format_alt",
        0x48 => "stream_format",
        0x36 => "peak",
        0xa3 => "seek_table",
        0x5c => "animation",
        0xb2 => "metadata",
        0xbd => "markers",
        0x68 => "midi",
        0x2b => "gesture",
        0x5a => "granular_grains",
        0xd9 => "granular_loops",
        _ => return format!("0x{kind:02x}"),
    }
    .to_string()
}

#[derive(Clone, Copy)]
pub struct Chunk {
    pub kind: u8,
    pub size: usize,
    pub offset: usize,
}

pub struct Stream {
    pub id: u32,
    pub chunks: Vec<Chunk>,
}

impl Stream {
    pub fn chunk(&self, kinds: &[u8]) -> Option<Chunk> {
        self.chunks.iter().copied().find(|c| kinds.contains(&c.kind))
    }
}

pub struct Channel {
    pub id: u32,
    pub samples: u32,
    pub rate: u16,
    pub codec: u8,
}

pub struct StreamFormat {
    pub blocks: u32,
    pub block_size: u32,
    pub channels: Vec<Channel>,
}

pub struct AudioFormat {
    pub samples: u32,
    pub rate: u16,
    pub codec: u8,
    /// Codec/channel bytes of the 32-byte variant (see rpf9-explorer AwcRecovery).
    pub codec32: Option<(u8, u8)>,
}

pub struct Awc<'a> {
    pub data: &'a [u8],
    pub flags: u32,
    pub header_length: usize,
    pub streams: Vec<Stream>,
}

impl<'a> Awc<'a> {
    /// Parse an AWC at the start of `data` (which may extend past the AWC).
    /// `strict` requires every chunk to lie after the header and inside `data`.
    pub fn parse(data: &'a [u8], strict: bool) -> Result<Self, String> {
        if data.len() < 16 || !data.starts_with(b"ADAT") {
            return Err("not an ADAT/AWC file".into());
        }
        let flags = le32(data, 4).unwrap_or(0);
        let count = le32(data, 8).unwrap_or(0) as usize;
        let header_length = le32(data, 12).unwrap_or(0) as usize;
        if count > 1_000_000 || count * 4 > header_length.max(16) {
            return Err(format!("implausible stream count {count}"));
        }
        if header_length < 16 || header_length > data.len() {
            return Err("header length runs past the file".into());
        }
        let has = |f: u32| flags & f != 0;
        let word = if has(FLAG_WORD) { 4 } else { 0 };
        let first_tags = if has(FLAG_FIRST_TAGS) { 2 * count } else { 0 };
        let extra = if has(FLAG_EXTRA_TABLE) { 4 * (count + 1) } else { 0 };
        let canonical = 16 + word + first_tags + extra;
        let mut layouts = vec![canonical];
        if has(FLAG_MULTICHANNEL) {
            for guess in [2 * count, 4 * count, 4 * (count + 1), 8 * count] {
                layouts.push(canonical + guess);
            }
        }
        let header = &data[..header_length];
        'layout: for ids in layouts {
            let tags = ids + 4 * count;
            if tags > header_length {
                continue;
            }
            let packed: Vec<u32> = (0..count).map(|i| le32(header, ids + 4 * i).unwrap_or(0)).collect();
            let total: usize = packed.iter().map(|v| (v >> 29) as usize).sum();
            if tags + 8 * total != header_length {
                continue;
            }
            let mut running = 0;
            let mut streams = Vec::with_capacity(count);
            for (i, &value) in packed.iter().enumerate() {
                if first_tags > 0 && le16(header, 16 + word + 2 * i).map(usize::from) != Some(running) {
                    continue 'layout;
                }
                let mut chunks = Vec::new();
                for k in 0..(value >> 29) as usize {
                    let record = le64(header, tags + 8 * (running + k)).unwrap_or(0);
                    let chunk = Chunk {
                        kind: (record >> 56) as u8,
                        size: (record >> 28 & 0x0fff_ffff) as usize,
                        offset: (record & 0x0fff_ffff) as usize,
                    };
                    if strict
                        && ((chunk.size > 0 && chunk.offset < header_length)
                            || chunk.offset + chunk.size > data.len())
                    {
                        continue 'layout;
                    }
                    chunks.push(chunk);
                }
                running += (value >> 29) as usize;
                streams.push(Stream { id: value & 0x1fff_ffff, chunks });
            }
            return Ok(Self { data, flags, header_length, streams });
        }
        Err(format!("header table does not match header length {header_length:#x}"))
    }

    /// Strict parse used for carving; returns the declared total length.
    pub fn probe(data: &'a [u8]) -> Option<(usize, Self)> {
        let awc = Self::parse(data, true).ok()?;
        let flags_ok = KNOWN_FLAGS.contains(&awc.flags)
            || (awc.flags & 0xff00_ffff == 0xff00_0001 && awc.flags & 0x0098_0000 == 0);
        if !flags_ok {
            return None;
        }
        let end = awc.streams.iter().flat_map(|s| &s.chunks).map(|c| c.offset + c.size).fold(awc.header_length, usize::max);
        let has_data = awc.streams.iter().flat_map(|s| &s.chunks).any(|c| c.kind == TAG_DATA && c.size > 0);
        (end <= data.len() && (has_data || awc.streams.is_empty())).then_some((end, awc))
    }

    pub fn multichannel(&self) -> bool {
        self.flags & FLAG_MULTICHANNEL != 0
    }

    pub fn bytes(&self, chunk: Chunk) -> Option<&'a [u8]> {
        self.data.get(chunk.offset..chunk.offset.checked_add(chunk.size)?)
    }

    pub fn length(&self) -> usize {
        self.streams.iter().flat_map(|s| &s.chunks).map(|c| c.offset + c.size).fold(self.header_length, usize::max)
    }

    pub fn stream_format(&self, stream: &Stream) -> Option<StreamFormat> {
        let body = self.bytes(stream.chunk(&[TAG_STREAM_FORMAT])?)?;
        let (blocks, block_size, n) = (le32(body, 0)?, le32(body, 4)?, le32(body, 8)? as usize);
        if n > 64 || 12 + n * 16 > body.len() {
            return None;
        }
        let channels = (0..n)
            .map(|i| {
                let f = &body[12 + i * 16..28 + i * 16];
                Channel { id: le32(f, 0).unwrap_or(0), samples: le32(f, 4).unwrap_or(0), rate: le16(f, 10).unwrap_or(0), codec: f[12] }
            })
            .collect();
        Some(StreamFormat { blocks, block_size, channels })
    }

    pub fn audio_format(&self, stream: &Stream) -> Option<AudioFormat> {
        let body = self.bytes(stream.chunk(&[TAG_FORMAT, TAG_FORMAT_ALT])?)?;
        if body.len() < 20 {
            return None;
        }
        Some(AudioFormat {
            samples: le32(body, 0)?,
            rate: le16(body, 8)?,
            codec: body[19],
            codec32: (body.len() >= 32).then(|| (body[28], body[29])),
        })
    }
}

/// Opus packets from 2 KiB blocks of `u16 length | packet` records.
pub fn packets_2k(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    for block in data.chunks(2048) {
        let mut pos = 0;
        while let Some(len) = le16(block, pos).map(usize::from) {
            if len == 0 || pos + 2 + len > block.len() {
                break;
            }
            out.push(&block[pos + 2..pos + 2 + len]);
            pos += 2 + len;
        }
    }
    out
}

pub const MUSIC_BLOCK: usize = 0x80000;
pub const MUSIC_HEADERS: [usize; 4] = [0x800, 0x1000, 0x2000, 0x4000];
const SUB_BLOCK: usize = 0x800;
const MAX_PACKET: usize = 1500;

/// Opus packets from 512 KiB music blocks with an opaque per-block header.
pub fn packets_music(data: &[u8], header: usize, max_blocks: Option<usize>) -> Vec<&[u8]> {
    let mut out = Vec::new();
    for (index, block) in data.chunks(MUSIC_BLOCK).enumerate() {
        if max_blocks.is_some_and(|m| index >= m) {
            break;
        }
        let mut pos = header;
        while let Some(len) = le16(block, pos).map(usize::from) {
            if len == 0 || len > MAX_PACKET || pos + 2 + len > block.len() {
                let next = (pos / SUB_BLOCK + 1) * SUB_BLOCK;
                if next >= block.len() {
                    break;
                }
                pos = next;
                continue;
            }
            out.push(&block[pos + 2..pos + 2 + len]);
            pos += 2 + len;
        }
    }
    out
}

/// Pick the music block header yielding the most packets (ties -> larger header).
pub fn detect_header(data: &[u8]) -> (usize, usize) {
    let mut best = (MUSIC_HEADERS[0], 0, 0);
    for header in MUSIC_HEADERS {
        let packets = packets_music(data, header, Some(2));
        let bytes: usize = packets.iter().map(|p| p.len()).sum();
        if (packets.len(), bytes) >= (best.1, best.2) && !packets.is_empty() {
            best = (header, packets.len(), bytes);
        }
    }
    (best.0, best.1)
}

pub enum Framing {
    TwoK,
    Music(usize),
}

/// Choose framing for a stream's data chunk the way rpf9-audio's remux does.
pub fn framing(awc: &Awc, stream: &Stream, data: &[u8]) -> Framing {
    let music = awc.multichannel() || stream.chunk(&[TAG_STREAM_FORMAT]).is_some();
    if !music {
        return Framing::TwoK;
    }
    match detect_header(data) {
        (_, 0) => Framing::TwoK,
        (header, _) => Framing::Music(header),
    }
}

pub fn packets<'d>(framing: &Framing, data: &'d [u8]) -> Vec<&'d [u8]> {
    match framing {
        Framing::TwoK => packets_2k(data),
        Framing::Music(header) => packets_music(data, *header, None),
    }
}

/// One channel of a de-interleaved music stream.
#[derive(Default)]
pub struct ChannelPackets<'d> {
    pub packets: Vec<&'d [u8]>,
    /// (index of the block's first packet, samples to skip after decoding it).
    pub block_skips: Vec<(usize, u32)>,
}

/// De-interleave 512 KiB music blocks.
///
/// Observed layout (audio.rpf): each block starts with `channels` records of six
/// u32 `[0xffffffff, sub_blocks, skip_samples, samples, 0, 0]`, then one u32
/// seek entry per sub-block for every channel, padded to 0x800. Channel 0's
/// 2 KiB sub-blocks follow, then channel 1's, and so on.
pub fn deinterleave(data: &[u8], channels: usize, block_size: usize) -> Option<Vec<ChannelPackets<'_>>> {
    if channels == 0 || block_size < SUB_BLOCK || channels > 64 {
        return None;
    }
    let mut out: Vec<ChannelPackets> = (0..channels).map(|_| ChannelPackets::default()).collect();
    for block in data.chunks(block_size) {
        let records: Option<Vec<(usize, u32)>> =
            (0..channels).map(|c| Some((le32(block, c * 24 + 4)? as usize, le32(block, c * 24 + 8)?))).collect();
        let records = records?;
        let total: usize = records.iter().map(|r| r.0).sum();
        let header = (channels * 24 + total * 4).next_multiple_of(SUB_BLOCK);
        if header + total * SUB_BLOCK > block.len() {
            return None;
        }
        let mut pos = header;
        for (c, &(count, skip)) in records.iter().enumerate() {
            let ch = &mut out[c];
            ch.block_skips.push((ch.packets.len(), skip));
            ch.packets.extend(packets_2k(&block[pos..pos + count * SUB_BLOCK]));
            pos += count * SUB_BLOCK;
        }
    }
    Some(out)
}
