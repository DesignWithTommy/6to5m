//! RPF9 archive header (GTA VI, Xbox Series build).
//!
//! Layout as reverse-engineered from the files themselves:
//!
//! ```text
//! 0x00 u32  magic        0x52504639 ("9FPR" on disk, "RPF9" read little-endian)
//! 0x04 u32  toc_end      absolute end offset of the encrypted TOC block (starts at 0x40)
//! 0x08 u32  toc_field_b  unknown size (< toc_end; entries table size?)
//! 0x0C u32  names_size   probably name-table size (scales with entry count)
//! 0x10 [8]  toc_hash     per-archive 8 bytes (hash / salt; not a CBC IV)
//! 0x18 u32  0x00047A00   constant in every archive
//! 0x1C u32  0x00000150   constant in every archive
//! 0x20 u16  unk_a        3..7
//! 0x22 u16  unk_b        always 1
//! 0x24 u32  count        scales with archive size; probably top-level entry count
//! 0x28 u32  key_id       one of 4 values; likely selects the TOC key
//! 0x2C [4]  flags        01|02 0F 09 00
//! 0x30 u32  0x10         constant
//! 0x34..0x40             zero
//! 0x40..toc_end          TOC, encrypted. 16-byte block cipher, chained
//!                        (no repeated blocks, the first 32 bytes are identical in every archive).
//! data                   16-byte aligned entries, NOT encrypted: zlib streams
//!                        (single stream, or 64 KiB chunked streams).
//! ```
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::Read;
use std::path::Path;

pub const MAGIC: u32 = 0x5250_4639;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rpf9Header {
    pub magic: String,
    pub toc_end: u32,
    pub toc_field_b: u32,
    pub names_size: u32,
    pub toc_hash: String,
    pub const_18: u32,
    pub const_1c: u32,
    pub unk_a: u16,
    pub unk_b: u16,
    pub count: u32,
    pub key_id: String,
    pub flags: String,
    pub const_30: u32,
    /// Heuristic: TOC looks encrypted (high entropy).
    pub toc_encrypted: bool,
    pub toc_entropy: f64,
}

pub fn entropy(b: &[u8]) -> f64 {
    if b.is_empty() {
        return 0.0;
    }
    let mut c = [0u64; 256];
    for &x in b {
        c[x as usize] += 1;
    }
    let n = b.len() as f64;
    c.iter()
        .filter(|&&v| v > 0)
        .map(|&v| {
            let p = v as f64 / n;
            -p * p.log2()
        })
        .sum()
}

fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

pub fn read_header(path: &Path) -> Result<Rpf9Header> {
    let mut f = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut h = [0u8; 0x40];
    f.read_exact(&mut h)?;
    let magic = u32le(&h, 0);
    if magic != MAGIC {
        bail!("{}: not an RPF9 archive (magic {:08x})", path.display(), magic);
    }
    let toc_end = u32le(&h, 4);
    let toc_len = (toc_end.saturating_sub(0x40)).min(1 << 20) as usize;
    let mut toc = vec![0u8; toc_len];
    f.read_exact(&mut toc).ok();
    // Use the part after the common 32-byte prefix.
    let sample = &toc[toc.len().min(32)..toc.len().min(32 + 4096)];
    let e = entropy(sample);
    Ok(Rpf9Header {
        magic: "RPF9".into(),
        toc_end,
        toc_field_b: u32le(&h, 8),
        names_size: u32le(&h, 12),
        toc_hash: hex(&h[0x10..0x18]),
        const_18: u32le(&h, 0x18),
        const_1c: u32le(&h, 0x1c),
        unk_a: u16::from_le_bytes([h[0x20], h[0x21]]),
        unk_b: u16::from_le_bytes([h[0x22], h[0x23]]),
        count: u32le(&h, 0x24),
        key_id: format!("{:08x}", u32le(&h, 0x28)),
        flags: hex(&h[0x2c..0x30]),
        const_30: u32le(&h, 0x30),
        toc_encrypted: e > 7.0 || (sample.len() < 1024 && e > 6.0),
        toc_entropy: (e * 1000.0).round() / 1000.0,
    })
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{:02x}", x)).collect()
}

pub fn is_rpf9(path: &Path) -> bool {
    let mut b = [0u8; 4];
    File::open(path)
        .and_then(|mut f| f.read_exact(&mut b))
        .map(|_| u32::from_le_bytes(b) == MAGIC)
        .unwrap_or(false)
}
