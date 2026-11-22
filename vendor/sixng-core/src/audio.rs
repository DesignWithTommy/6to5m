//! Audio banks (`ADAT` containers) in `audio.rpf`.
//!
//! The archive's file table is encrypted, so banks are found by scanning for
//! the `ADAT` magic and validating the header. A bank holds one or more
//! streams; each stream is a set of chunks (format, data, seek table, peaks,
//! markers …). The audio itself is Opus at 48 kHz, stored as
//! `[u16 length][packet]` records packed into 2 KiB blocks (a record never
//! crosses a block; a zero length or the end of the block moves to the next
//! block). Export is a lossless remux into Ogg Opus (`.opus`), or decoded to
//! 16-bit WAV with libopus.
//!
//! Header (little-endian): `ADAT`, u32 flags, u32 stream count N, u32 header
//! length; then (by flag) a u32 word (0x0040_0000), N u16 first-chunk indices
//! (0x0001_0000), N+1 u32 of unknown use (0x0020_0000); N u32 stream entries
//! (bits 0-28 id, 29-31 chunk count); the chunk records (u64: type 63-56,
//! size 55-28, offset 27-0 from the bank start), ending at the header length.
//! Stream ids and chunk types are hashes; names are not recovered.

use crate::export::Progress;
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

pub const CH_DATA: u8 = 0x55;
pub const CH_FORMAT: u8 = 0x76;
pub const CH_FORMAT_ALT: u8 = 0xFA;
pub const CH_STREAM_FORMAT: u8 = 0x48;
pub const CH_PEAK: u8 = 0x36;
pub const CH_SEEK: u8 = 0xA3;
pub const CH_MARKERS: u8 = 0xBD;

const BLOCK: usize = 0x800;
/// Opus default encoder delay, used when the packet total does not give one.
const DEFAULT_PRE_SKIP: u32 = 312;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chunk {
    pub kind: u8,
    pub off: u32,
    pub size: u32,
}

/// Saved as a tuple to keep the index small (1.6 M streams in audio.rpf).
type StreamT = (u32, u32, u32, u8, u32, u8, u32, u32);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(from = "StreamT", into = "StreamT")]
pub struct Stream {
    /// 29-bit hash id (hex in reports).
    pub id: u32,
    /// All chunks (only while scanning; not saved in the index).
    pub chunks: Vec<Chunk>,
    /// Data chunk (offset from the bank start, size); size 0 = none.
    pub data: (u32, u32),
    /// Sample count from the format chunk. For Opus streams it is in 48 kHz
    /// units over all channels (a stereo stream counts each frame twice),
    /// whatever the rate field says (most say 32000: the source rate).
    pub samples: u32,
    /// Rate field of the format chunk.
    pub rate: u32,
    /// Codec byte of the format chunk (Opus streams have 0).
    pub codec: u8,
    /// Channels: from the first packet for Opus streams, from the
    /// stream-format chunk for music streams.
    pub channels: u32,
    /// "blocks" (Opus, 2 KiB packet blocks), "music" (512 KiB blocks with an
    /// undecoded header, multichannel), "other" (data with a codec byte other
    /// than 0: not decoded), or "none" (no data chunk: a channel entry of a
    /// music stream, or metadata only).
    pub layout: String,
}

impl From<StreamT> for Stream {
    fn from(t: StreamT) -> Self {
        let layout = match t.5 { 1 => "blocks", 2 => "music", 3 => "other", _ => "none" }.into();
        Stream { id: t.0, chunks: Vec::new(), data: (t.6, t.7), samples: t.1, rate: t.2, codec: t.3, channels: t.4, layout }
    }
}

impl From<Stream> for StreamT {
    fn from(s: Stream) -> Self {
        let l = match s.layout.as_str() { "blocks" => 1, "music" => 2, "other" => 3, _ => 0 };
        (s.id, s.samples, s.rate, s.codec, s.channels, l, s.data.0, s.data.1)
    }
}

impl Stream {
    pub fn chunk(&self, kind: u8) -> Option<&Chunk> {
        self.chunks.iter().find(|c| c.kind == kind)
    }
    /// Samples per channel at 48 kHz (Opus streams).
    pub fn frames(&self) -> u32 {
        self.samples / self.channels.max(1)
    }
    pub fn playable(&self) -> bool {
        self.layout == "blocks"
    }
    pub fn seconds(&self) -> f64 {
        match self.layout.as_str() {
            "blocks" => self.frames() as f64 / 48000.0,
            "music" => self.samples as f64 / 48000.0,
            _ if self.rate > 0 => self.samples as f64 / self.rate as f64,
            _ => 0.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bank {
    /// Offset of the `ADAT` magic in the archive.
    pub off: u64,
    pub size: u64,
    pub flags: u32,
    pub streams: Vec<Stream>,
}

impl Bank {
    /// Total length of the playable streams, in seconds.
    pub fn seconds(&self) -> f64 {
        self.streams.iter().filter(|s| s.layout != "none").map(|s| s.seconds()).sum()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AudioIndex {
    pub archive: String,
    pub archive_size: u64,
    pub banks: Vec<Bank>,
    pub note: String,
}

fn u16le(b: &[u8], o: usize) -> Option<u32> {
    b.get(o..o + 2).map(|x| u16::from_le_bytes([x[0], x[1]]) as u32)
}
fn u32le(b: &[u8], o: usize) -> Option<u32> {
    b.get(o..o + 4).map(|x| u32::from_le_bytes(x.try_into().unwrap()))
}

/// Parse a bank header (`h` = the first `header length` bytes or more).
/// None when it is not a plausible bank.
pub fn parse_header(h: &[u8], off: u64) -> Option<Bank> {
    if h.get(0..4)? != b"ADAT" {
        return None;
    }
    let flags = u32le(h, 4)?;
    let n = u32le(h, 8)? as usize;
    let hl = u32le(h, 12)? as usize;
    if flags >> 24 != 0xFF || flags & 0xFFFF != 1 || n == 0 || n > 20000 || !(16..=(4 << 20)).contains(&hl) || h.len() < hl {
        return None;
    }
    let mut p = 16;
    if flags & 0x0040_0000 != 0 {
        p += 4;
    }
    let first: Option<Vec<u32>> = if flags & 0x0001_0000 != 0 {
        let v = (0..n).map(|i| u16le(h, p + 2 * i)).collect::<Option<Vec<_>>>()?;
        p += 2 * n;
        Some(v)
    } else {
        None
    };
    if flags & 0x0020_0000 != 0 {
        p += 4 * (n + 1);
    }
    let entries: Vec<(u32, usize)> = (0..n).map(|i| u32le(h, p + 4 * i).map(|v| (v & 0x1FFF_FFFF, (v >> 29) as usize))).collect::<Option<Vec<_>>>()?;
    p += 4 * n;
    let total: usize = entries.iter().map(|e| e.1).sum();
    if p + 8 * total != hl {
        return None;
    }
    if let Some(f) = &first {
        let mut acc = 0u32;
        for (i, e) in entries.iter().enumerate() {
            if f[i] != acc {
                return None;
            }
            acc += e.1 as u32;
        }
    }
    let mut size = hl as u64;
    let mut streams = Vec::with_capacity(n);
    let mut k = 0;
    for (id, cnt) in entries {
        let mut chunks = Vec::with_capacity(cnt);
        for _ in 0..cnt {
            let r = u64::from_le_bytes(h.get(p + 8 * k..p + 8 * k + 8)?.try_into().ok()?);
            k += 1;
            let c = Chunk { kind: (r >> 56) as u8, size: ((r >> 28) & 0x0FFF_FFFF) as u32, off: (r & 0x0FFF_FFFF) as u32 };
            if c.size > 0 && (c.off as usize) < hl {
                return None;
            }
            size = size.max(c.off as u64 + c.size as u64);
            chunks.push(c);
        }
        streams.push(Stream { id, chunks, data: (0, 0), samples: 0, rate: 0, codec: 0, channels: 0, layout: String::new() });
    }
    Some(Bank { off, size, flags, streams })
}

/// Fill in format fields from the bank bytes (header + small chunks).
fn read_formats(f: &mut File, bank: &mut Bank) -> Result<()> {
    for s in bank.streams.iter_mut() {
        let fc = s.chunks.iter().find(|c| (c.kind == CH_FORMAT || c.kind == CH_FORMAT_ALT) && c.size >= 20).cloned();
        if let Some(c) = fc {
            let mut b = vec![0u8; c.size.min(64) as usize];
            f.seek(SeekFrom::Start(bank.off + c.off as u64))?;
            f.read_exact(&mut b)?;
            s.samples = u32le(&b, 0).unwrap_or(0);
            s.rate = u16le(&b, 8).unwrap_or(0);
            s.codec = b.get(0x13).copied().unwrap_or(0);
        }
        if let Some(c) = s.chunk(CH_STREAM_FORMAT).cloned() {
            let mut b = vec![0u8; c.size.min(12 + 16 * 64) as usize];
            f.seek(SeekFrom::Start(bank.off + c.off as u64))?;
            f.read_exact(&mut b)?;
            let ch = u32le(&b, 8).unwrap_or(0);
            if ch <= 64 && b.len() >= 12 + 16 * ch as usize {
                s.channels = ch;
                if ch > 0 {
                    s.samples = u32le(&b, 12 + 4).unwrap_or(0);
                    s.rate = u16le(&b, 12 + 10).unwrap_or(0);
                    s.codec = b[12 + 12];
                }
            }
        }
        if let Some(c) = s.chunk(CH_DATA) {
            s.data = (c.off, c.size);
        }
        s.layout = match (s.chunk(CH_DATA), s.chunk(CH_STREAM_FORMAT)) {
            (Some(_), Some(_)) => "music",
            (Some(_), None) if s.codec == 0 => "blocks",
            (Some(_), None) => "other",
            _ => "none",
        }
        .into();
        if s.layout == "blocks" && s.data.1 >= 3 {
            // channels from the first packet's TOC (stereo bit)
            let mut t = [0u8; 3];
            f.seek(SeekFrom::Start(bank.off + s.data.0 as u64))?;
            f.read_exact(&mut t)?;
            s.channels = if t[2] & 4 != 0 { 2 } else { 1 };
        }
    }
    Ok(())
}

/// Scan an archive for banks. Streams the file in 32 MiB windows.
pub fn scan(path: &Path, archive: &str, pr: Progress) -> Result<AudioIndex> {
    let mut f = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut g = File::open(path)?;
    let len = f.metadata()?.len();
    const WIN: usize = 32 << 20;
    let mut buf = vec![0u8; WIN + 3];
    let mut base = 0u64;
    let mut skip_until = 0u64;
    let mut banks = Vec::new();
    let mut carry = 0usize;
    loop {
        let n = read_full(&mut f, &mut buf[carry..])?;
        let avail = carry + n;
        if avail < 4 {
            break;
        }
        let mut i = 0;
        while let Some(rel) = find(&buf[i..avail], b"ADAT") {
            let at = i + rel;
            let abs = base + at as u64;
            i = at + 1;
            if abs < skip_until {
                continue;
            }
            // header: read 16 bytes, then the whole header
            let mut h16 = [0u8; 16];
            g.seek(SeekFrom::Start(abs))?;
            if g.read_exact(&mut h16).is_err() {
                continue;
            }
            let hl = u32::from_le_bytes(h16[12..16].try_into().unwrap()) as usize;
            if !(16..=(4 << 20)).contains(&hl) {
                continue;
            }
            let mut h = vec![0u8; hl];
            g.seek(SeekFrom::Start(abs))?;
            if g.read_exact(&mut h).is_err() {
                continue;
            }
            let Some(mut bank) = parse_header(&h, abs) else { continue };
            if abs + bank.size > len {
                continue;
            }
            read_formats(&mut g, &mut bank)?;
            for s in bank.streams.iter_mut() {
                s.chunks = Vec::new();
            }
            skip_until = abs + bank.size;
            banks.push(bank);
        }
        let consumed = avail.saturating_sub(3);
        base += consumed as u64;
        buf.copy_within(consumed..avail, 0);
        carry = avail - consumed;
        pr("scan", base.min(len), len);
        if n == 0 {
            break;
        }
    }
    Ok(AudioIndex {
        archive: archive.into(),
        archive_size: len,
        banks,
        note: "Audio banks (ADAT) found by scanning for the magic; offsets are in the archive. Stream ids are 29-bit hashes (names not recovered). layout: blocks = Opus packets in 2 KiB blocks (exported as Ogg Opus or WAV); music = 512 KiB blocks with an undecoded header (multichannel; exported raw); other = codec byte not 0, not decoded (exported raw); none = no data chunk. Opus sample counts are 48 kHz units over all channels.".into(),
    })
}

fn read_full(f: &mut File, buf: &mut [u8]) -> Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        let k = f.read(&mut buf[n..])?;
        if k == 0 {
            break;
        }
        n += k;
    }
    Ok(n)
}

fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}

/// Read a bank's bytes.
pub fn read_bank(path: &Path, b: &Bank) -> Result<Vec<u8>> {
    let mut f = File::open(path)?;
    f.seek(SeekFrom::Start(b.off))?;
    let mut v = vec![0u8; b.size as usize];
    f.read_exact(&mut v)?;
    Ok(v)
}

/// Opus packets of a stream in 2 KiB-block layout.
pub fn packets(bank: &[u8], s: &Stream) -> Vec<Vec<u8>> {
    let (off, size) = s.data;
    let Some(d) = bank.get(off as usize..(off + size) as usize) else { return Vec::new() };
    let mut out = Vec::new();
    for blk in d.chunks(BLOCK) {
        let mut p = 0;
        while p + 2 <= blk.len() {
            let n = u16::from_le_bytes([blk[p], blk[p + 1]]) as usize;
            if n == 0 || p + 2 + n > blk.len() {
                break;
            }
            out.push(blk[p + 2..p + 2 + n].to_vec());
            p += 2 + n;
        }
    }
    out
}

/// Samples (at 48 kHz) in an Opus packet, from its TOC byte (RFC 6716 3.1).
pub fn packet_samples(p: &[u8]) -> u32 {
    let Some(&toc) = p.first() else { return 0 };
    let cfg = toc >> 3;
    let frame = if cfg < 12 {
        [480, 960, 1920, 2880][(cfg & 3) as usize]
    } else if cfg < 16 {
        [480, 960][(cfg & 1) as usize]
    } else {
        [120, 240, 480, 960][(cfg & 3) as usize]
    };
    let frames = match toc & 3 {
        0 => 1,
        1 | 2 => 2,
        _ => p.get(1).map(|b| (b & 0x3F) as u32).unwrap_or(0),
    };
    frame * frames
}

pub fn packet_channels(pk: &[Vec<u8>]) -> u8 {
    if pk.iter().any(|p| p.first().map_or(false, |t| t & 4 != 0)) { 2 } else { 1 }
}

/// Encoder delay. The packets hold about 2.5 k samples more than the format
/// count, not frame-aligned; the split between start delay and end padding
/// is not known, so the Opus default delay is skipped and the end trimmed to
/// the count.
pub fn pre_skip() -> u32 {
    DEFAULT_PRE_SKIP
}

fn ogg_crc(data: &[u8]) -> u32 {
    let mut crc = 0u32;
    for &b in data {
        crc ^= (b as u32) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 { (crc << 1) ^ 0x04C1_1DB7 } else { crc << 1 };
        }
    }
    crc
}

fn ogg_page(out: &mut Vec<u8>, serial: u32, seq: u32, granule: u64, flags: u8, packet: &[u8]) {
    // one packet per page (packets here are < 64 KiB)
    let mut lacing = Vec::new();
    let mut n = packet.len();
    loop {
        if n >= 255 {
            lacing.push(255u8);
            n -= 255;
        } else {
            lacing.push(n as u8);
            break;
        }
    }
    let start = out.len();
    out.extend_from_slice(b"OggS");
    out.push(0);
    out.push(flags);
    out.extend_from_slice(&granule.to_le_bytes());
    out.extend_from_slice(&serial.to_le_bytes());
    out.extend_from_slice(&seq.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.push(lacing.len() as u8);
    out.extend_from_slice(&lacing);
    out.extend_from_slice(packet);
    let crc = ogg_crc(&out[start..]);
    out[start + 22..start + 26].copy_from_slice(&crc.to_le_bytes());
}

/// Lossless remux of Opus packets into an Ogg Opus file. `samples` = per
/// channel (`Stream::frames`).
pub fn ogg_opus(pk: &[Vec<u8>], samples: u32, tag: &str) -> Vec<u8> {
    let ch = packet_channels(pk);
    let skip = pre_skip();
    let serial = 0x5358_4E47; // "SXNG"
    let mut out = Vec::new();
    let mut head = b"OpusHead".to_vec();
    head.push(1);
    head.push(ch);
    head.extend_from_slice(&(skip as u16).to_le_bytes());
    head.extend_from_slice(&48000u32.to_le_bytes());
    head.extend_from_slice(&0i16.to_le_bytes());
    head.push(0);
    ogg_page(&mut out, serial, 0, 0, 0x02, &head);
    let vendor = b"sixng";
    let mut tags = b"OpusTags".to_vec();
    tags.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    tags.extend_from_slice(vendor);
    let c = format!("TITLE={tag}");
    tags.extend_from_slice(&1u32.to_le_bytes());
    tags.extend_from_slice(&(c.len() as u32).to_le_bytes());
    tags.extend_from_slice(c.as_bytes());
    ogg_page(&mut out, serial, 1, 0, 0, &tags);
    let mut gran = skip as u64;
    let end = if samples > 0 { skip as u64 + samples as u64 } else { u64::MAX };
    for (i, p) in pk.iter().enumerate() {
        gran += packet_samples(p) as u64;
        let last = i + 1 == pk.len();
        let g = if last { gran.min(end) } else { gran };
        ogg_page(&mut out, serial, 2 + i as u32, g, if last { 0x04 } else { 0 }, p);
    }
    out
}

/// Decode to interleaved 16-bit PCM at 48 kHz (pre-skip removed, trimmed to
/// `samples` per channel). Returns (channels, interleaved samples).
pub fn decode(pk: &[Vec<u8>], samples: u32) -> Result<(u8, Vec<i16>)> {
    let ch = packet_channels(pk);
    let mut dec = opus_decoder::OpusDecoder::new(48000, ch as usize).map_err(|e| anyhow!("opus: {e:?}"))?;
    let mut pcm = Vec::new();
    let mut buf = vec![0i16; 5760 * ch as usize];
    for p in pk {
        match dec.decode(p, &mut buf, false) {
            Ok(n) => pcm.extend_from_slice(&buf[..n * ch as usize]),
            Err(_) => {
                // keep timing: a lost packet decodes as concealment
                if let Ok(n) = dec.decode(&[], &mut buf, false) {
                    pcm.extend_from_slice(&buf[..n * ch as usize]);
                }
            }
        }
    }
    let skip = (pre_skip() as usize * ch as usize).min(pcm.len());
    pcm.drain(..skip);
    if samples > 0 {
        pcm.truncate(samples as usize * ch as usize);
    }
    Ok((ch, pcm))
}

pub fn wav(ch: u8, rate: u32, pcm: &[i16]) -> Vec<u8> {
    let data = pcm.len() * 2;
    let mut o = Vec::with_capacity(44 + data);
    o.extend_from_slice(b"RIFF");
    o.extend_from_slice(&((36 + data) as u32).to_le_bytes());
    o.extend_from_slice(b"WAVEfmt ");
    o.extend_from_slice(&16u32.to_le_bytes());
    o.extend_from_slice(&1u16.to_le_bytes());
    o.extend_from_slice(&(ch as u16).to_le_bytes());
    o.extend_from_slice(&rate.to_le_bytes());
    o.extend_from_slice(&(rate * ch as u32 * 2).to_le_bytes());
    o.extend_from_slice(&(ch as u16 * 2).to_le_bytes());
    o.extend_from_slice(&16u16.to_le_bytes());
    o.extend_from_slice(b"data");
    o.extend_from_slice(&(data as u32).to_le_bytes());
    for s in pcm {
        o.extend_from_slice(&s.to_le_bytes());
    }
    o
}

/// File name stem for a stream: `<bank offset>_<stream id>`.
pub fn stem(b: &Bank, s: &Stream) -> String {
    format!("{:x}_{:08x}", b.off, s.id)
}

/// Export one stream. `fmt`: opus | wav | both. Returns written files.
pub fn export_stream(bank_bytes: &[u8], b: &Bank, s: &Stream, out: &Path, fmt: &str) -> Result<Vec<String>> {
    std::fs::create_dir_all(out)?;
    if s.layout != "blocks" {
        return Err(anyhow!("stream {:08x}: layout '{}' is not exported per stream (export the bank raw)", s.id, s.layout));
    }
    let pk = packets(bank_bytes, s);
    if pk.is_empty() {
        return Err(anyhow!("stream {:08x}: no Opus packets", s.id));
    }
    let mut files = Vec::new();
    let st = stem(b, s);
    if fmt == "opus" || fmt == "both" {
        let p = out.join(format!("{st}.opus"));
        std::fs::write(&p, ogg_opus(&pk, s.frames(), &st))?;
        files.push(p.display().to_string());
    }
    if fmt == "wav" || fmt == "both" {
        let (ch, pcm) = decode(&pk, s.frames())?;
        let p = out.join(format!("{st}.wav"));
        let mut f = File::create(&p)?;
        f.write_all(&wav(ch, 48000, &pcm))?;
        files.push(p.display().to_string());
    }
    Ok(files)
}

pub fn index_path(work: &Path, archive: &str) -> std::path::PathBuf {
    work.join("index").join(format!("audio_{}.json.gz", archive.trim_end_matches(".rpf").replace(['/', '\\'], "_")))
}

pub fn load_index(work: &Path, archive: &str) -> Result<AudioIndex> {
    let p = index_path(work, archive);
    let f = File::open(&p).with_context(|| format!("no audio index {} (run `sixng audio` first)", p.display()))?;
    Ok(serde_json::from_reader(std::io::BufReader::new(flate2::read::GzDecoder::new(f)))?)
}

pub fn save_index(work: &Path, ix: &AudioIndex) -> Result<()> {
    let p = index_path(work, &ix.archive);
    std::fs::create_dir_all(p.parent().unwrap())?;
    let mut z = flate2::write::GzEncoder::new(std::io::BufWriter::new(File::create(p)?), flate2::Compression::fast());
    serde_json::to_writer(&mut z, ix)?;
    z.finish()?.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn toc() {
        assert_eq!(packet_samples(&[0xF8, 0]), 960); // CELT FB 20 ms
        assert_eq!(packet_samples(&[0x78, 0]), 960); // hybrid FB 20 ms
        assert_eq!(packet_samples(&[0x1B, 0x03]), 3 * 2880); // SILK 60 ms, code 3, 3 frames
    }
    #[test]
    fn header() {
        // one stream, two chunks, flags 0xFF210001
        let mut h = b"ADAT".to_vec();
        h.extend_from_slice(&0xFF21_0001u32.to_le_bytes());
        h.extend_from_slice(&1u32.to_le_bytes());
        let hl = 16 + 2 + 8 + 4 + 16;
        h.extend_from_slice(&(hl as u32).to_le_bytes());
        h.extend_from_slice(&0u16.to_le_bytes());
        h.extend_from_slice(&[0u8; 8]);
        h.extend_from_slice(&((2u32 << 29) | 0x1234).to_le_bytes());
        let rec = |t: u64, s: u64, o: u64| (t << 56) | (s << 28) | o;
        h.extend_from_slice(&rec(0x76, 32, hl as u64).to_le_bytes());
        h.extend_from_slice(&rec(0x55, 2048, hl as u64 + 32).to_le_bytes());
        let b = parse_header(&h, 0).unwrap();
        assert_eq!(b.streams.len(), 1);
        assert_eq!(b.streams[0].id, 0x1234);
        assert_eq!(b.size, hl as u64 + 32 + 2048);
    }
}
