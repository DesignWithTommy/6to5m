//! DDS reader (DdsTextureReader.cs): validates a DDS, exposes its mip planes
//! and decodes its preview mip.

use super::format::{self, R, element_bytes, is_bc};
use crate::bytes::le32;

const MAX_FILE_BYTES: usize = 256 * 1024 * 1024;
const MAX_SLICES: u32 = 2048;

pub struct Info {
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub dimension: u32,
    pub slices: u32,
    pub mips: u32,
    pub format: u32,
    pub alpha_mode: u32,
    pub resource_dimension: u32,
    pub array_size: u32,
    pub data_offset: usize,
    /// (offset, plane bytes, planes) per subresource, slice-major then mip.
    planes: Vec<(usize, usize, usize)>,
}

impl Info {
    /// One mip plane of one slice (for volumes, `slice` is the z plane).
    pub fn plane<'d>(&self, d: &'d [u8], mip: u32, slice: u32) -> R<&'d [u8]> {
        if mip >= self.mips {
            return Err("mip out of range".into());
        }
        let (index, z) = if self.dimension == 3 { (mip, slice) } else { (slice * self.mips + mip, 0) };
        let &(offset, plane, count) = self.planes.get(index as usize).ok_or("slice out of range")?;
        if z as usize >= count {
            return Err("slice out of range".into());
        }
        Ok(&d[offset + z as usize * plane..offset + (z as usize + 1) * plane])
    }
}

fn u32at(d: &[u8], at: usize) -> u32 {
    le32(d, at).unwrap_or(0)
}

fn dim(v: u32) -> R<u32> {
    if (1..=16384).contains(&v) { Ok(v) } else { Err("DDS dimension is invalid or exceeds 16384".into()) }
}

fn legacy_format(d: &[u8], flags: u32, fourcc: u32) -> R<(u32, u32)> {
    if flags & 4 != 0 {
        return Ok(match fourcc {
            0x31545844 => (71, 0),
            0x32545844 => (74, 2),
            0x33545844 => (74, 0),
            0x34545844 => (77, 2),
            0x35545844 => (77, 0),
            0x31495441 | 0x55344342 => (80, 0),
            0x53344342 => (81, 0),
            0x32495441 | 0x55354342 => (83, 0),
            0x53354342 => (84, 0),
            111 => (54, 0),
            112 => (34, 0),
            113 => (10, 0),
            114 => (41, 0),
            _ => return Err("unsupported legacy DDS FOURCC".into()),
        });
    }
    let (bits, r, g, b, a) = (u32at(d, 88), u32at(d, 92), u32at(d, 96), u32at(d, 100), u32at(d, 104));
    if flags & 0x40 != 0 && bits == 32 {
        match (r, g, b, a) {
            (0xFF, 0xFF00, 0xFF0000, 0xFF000000) => return Ok((28, 1)),
            (0xFF0000, 0xFF00, 0xFF, 0xFF000000) => return Ok((87, 1)),
            (0xFF0000, 0xFF00, 0xFF, 0) => return Ok((88, 3)),
            (0x3FF, 0xFFC00, 0x3FF00000, 0xC0000000) => return Ok((24, 1)),
            _ => {}
        }
    }
    if flags & 0x20000 != 0 && g == 0 && b == 0 && a == 0 {
        if bits == 8 && r == 0xFF {
            return Ok((61, 0));
        }
        if bits == 16 && r == 0xFFFF {
            return Ok((56, 0));
        }
    }
    if flags & 2 != 0 && bits == 8 && a == 0xFF {
        return Ok((65, 0));
    }
    Err("unsupported legacy DDS pixel masks".into())
}

pub fn inspect(d: &[u8]) -> R<Info> {
    if d.len() < 128 || d.len() > MAX_FILE_BYTES || &d[..4] != b"DDS " || u32at(d, 4) != 124 || u32at(d, 76) != 32 {
        return Err("invalid, truncated or oversized DDS header".into());
    }
    let (width, height) = (dim(u32at(d, 16))?, dim(u32at(d, 12))?);
    let mips = u32at(d, 28).max(1);
    let (flags, fourcc, caps2) = (u32at(d, 80), u32at(d, 84), u32at(d, 112));
    let (mut slices, mut depth, mut dimension, mut start, mut res, mut array) = (1, 1, 1, 128, 3, 1);
    let (format, alpha_mode);
    if flags & 4 != 0 && fourcc == 0x30315844 {
        if d.len() < 148 {
            return Err("truncated DDS DX10 extension".into());
        }
        format = u32at(d, 128);
        res = u32at(d, 132);
        let misc = u32at(d, 136);
        array = u32at(d, 140);
        alpha_mode = u32at(d, 144) & 7;
        if !(1..=MAX_SLICES).contains(&array) || alpha_mode > 4 {
            return Err("invalid DDS array or alpha mode".into());
        }
        let cube = misc & 4 != 0;
        match res {
            2 if height == 1 && !cube => (slices, depth) = (array, array),
            3 => {
                if cube && width != height {
                    return Err("cube faces must be square".into());
                }
                dimension = if cube { 2 } else { 1 };
                slices = array * if cube { 6 } else { 1 };
                depth = slices;
            }
            4 if array == 1 && !cube => {
                dimension = 3;
                depth = dim(u32at(d, 24))?;
                slices = depth;
            }
            _ => return Err("unsupported or inconsistent DDS resource dimension".into()),
        }
        start = 148;
    } else {
        (format, alpha_mode) = legacy_format(d, flags, fourcc)?;
        let (volume, cube) = (caps2 & 0x200000 != 0, caps2 & 0x200 != 0);
        if volume && cube {
            return Err("DDS cannot be both a cube and a volume".into());
        }
        if volume {
            dimension = 3;
            res = 4;
            depth = dim(u32at(d, 24))?;
            slices = depth;
        } else if cube {
            if caps2 & 0xFC00 != 0xFC00 || width != height {
                return Err("incomplete or nonsquare legacy cube maps are not supported".into());
            }
            dimension = 2;
            (depth, slices) = (6, 6);
        }
    }
    if slices > MAX_SLICES {
        return Err("DDS has too many slices".into());
    }
    let mut max_mips = 1;
    let mut size = width.max(height).max(if dimension == 3 { depth } else { 1 });
    while size > 1 {
        size >>= 1;
        max_mips += 1;
    }
    if mips > max_mips {
        return Err("DDS mip count exceeds its dimensions".into());
    }
    let eb = element_bytes(format)?;
    let px = if is_bc(format) { 4 } else { 1 };
    let mut offset = start;
    let mut planes = Vec::new();
    for _slice in 0..if dimension == 3 { 1 } else { slices } {
        for mip in 0..mips {
            let (w, h) = ((width >> mip).max(1), (height >> mip).max(1));
            let dz = if dimension == 3 { (depth >> mip).max(1) } else { 1 } as usize;
            let plane = w.div_ceil(px) as usize * eb * h.div_ceil(px) as usize;
            let size = plane * dz;
            if size > MAX_FILE_BYTES || offset + size > d.len() {
                return Err("DDS mip/slice data is truncated".into());
            }
            planes.push((offset, plane, dz));
            offset += size;
        }
    }
    if offset != d.len() {
        return Err("DDS has trailing bytes or inconsistent mip/slice data".into());
    }
    Ok(Info {
        width,
        height,
        depth,
        dimension,
        slices,
        mips,
        format,
        alpha_mode,
        resource_dimension: res,
        array_size: array,
        data_offset: start,
        planes,
    })
}

/// Decodes slice 0 at the preview mip to straight RGBA.
pub fn decode_preview(d: &[u8]) -> R<(Info, u32, u32, Vec<u8>)> {
    let info = inspect(d)?;
    let mip = format::choose_preview_mip(info.width, info.height, info.mips);
    let (w, h) = ((info.width >> mip).max(1), (info.height >> mip).max(1));
    let mut rgba = format::decode_rgba(info.plane(d, mip, 0)?, w, h, info.format)?;
    if info.alpha_mode == 2 {
        for p in rgba.chunks_exact_mut(4) {
            let a = u32::from(p[3]);
            for c in &mut p[..3] {
                *c = if a == 0 { 0 } else { ((u32::from(*c) * 255 + a / 2) / a).min(255) as u8 };
            }
        }
    }
    Ok((info, w, h, rgba))
}
