//! Standalone RSC7 resource header (port of OpenRPF9 Rsc9.cs): page-flag
//! sizes and the system/graphics split of an extracted resource file.
//! Pointer fixups are not applied: the drawable decoder works directly on
//! 0x5/0x6 virtual addresses.

use crate::bytes::le32;

pub const MAGIC: u32 = 0x3743_5352; // "RSC7"

pub struct Header {
    pub version: u32,
    pub system_flags: u32,
    pub graphics_flags: u32,
}

fn pages(flags: u32) -> impl Iterator<Item = u32> {
    (0..8).map(move |i| (flags >> (i * 4)) & 15).filter(|&code| code != 0)
}

impl Header {
    pub fn read(data: &[u8]) -> Option<Self> {
        (le32(data, 0)? == MAGIC).then(|| Self {
            version: le32(data, 4).unwrap_or(0),
            system_flags: le32(data, 8).unwrap_or(0),
            graphics_flags: le32(data, 12).unwrap_or(0),
        })
    }
    pub fn page_count(flags: u32) -> usize {
        pages(flags).count().max(1)
    }
    pub fn size(flags: u32) -> usize {
        let sizes: Vec<usize> = pages(flags).map(|code| 0x2000 << ((code - 1) & 7)).collect();
        if sizes.is_empty() { 0x2000 } else { sizes.iter().sum() }
    }
    /// (system, graphics) slices, clamped to the data present.
    pub fn split<'a>(&self, data: &'a [u8]) -> (&'a [u8], &'a [u8]) {
        let body = data.get(16..).unwrap_or(&[]);
        let s = Self::size(self.system_flags).min(body.len());
        let g = Self::size(self.graphics_flags).min(body.len() - s);
        (&body[..s], &body[s..s + g])
    }
}
