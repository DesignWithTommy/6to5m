//! DXGI format tables and linear pixel decoding (RageTextureDecoder helpers).

pub type R<T> = Result<T, String>;

pub const MAX_PREVIEW_PIXELS: u64 = 16 * 1024 * 1024;

pub fn is_bc(format: u32) -> bool {
    (70..=84).contains(&format) || (94..=99).contains(&format)
}

pub fn element_bytes(format: u32) -> R<usize> {
    Ok(match format {
        61 | 65 => 1,
        49 | 54 | 56 => 2,
        24 | 28 | 29 | 34 | 41 | 87 | 88 | 91 | 93 => 4,
        10 | 11 | 71 | 72 | 80 | 81 => 8,
        74 | 75 | 77 | 78 | 83 | 84 | 95 | 96 | 98 | 99 => 16,
        _ => return Err(format!("unsupported DXGI format {format}")),
    })
}

pub fn element_log2(eb: usize) -> R<usize> {
    match eb {
        1 => Ok(0),
        2 => Ok(1),
        4 => Ok(2),
        8 => Ok(3),
        16 => Ok(4),
        _ => Err("invalid element size".into()),
    }
}

pub fn name(format: u32) -> String {
    let known = match format {
        10 => "R16G16B16A16_FLOAT",
        11 => "R16G16B16A16_UNORM",
        24 => "R10G10B10A2_UNORM",
        28 => "R8G8B8A8_UNORM",
        29 => "R8G8B8A8_UNORM_SRGB",
        34 => "R16G16_FLOAT",
        41 => "R32_FLOAT",
        49 => "R8G8_UNORM",
        54 => "R16_FLOAT",
        56 => "R16_UNORM",
        61 => "R8_UNORM",
        65 => "A8_UNORM",
        87 => "B8G8R8A8_UNORM",
        88 => "B8G8R8X8_UNORM",
        91 => "B8G8R8A8_UNORM_SRGB",
        93 => "B8G8R8X8_UNORM_SRGB",
        71 => "BC1_UNORM",
        72 => "BC1_UNORM_SRGB",
        74 => "BC2_UNORM",
        75 => "BC2_UNORM_SRGB",
        77 => "BC3_UNORM",
        78 => "BC3_UNORM_SRGB",
        80 => "BC4_UNORM",
        81 => "BC4_SNORM",
        83 => "BC5_UNORM",
        84 => "BC5_SNORM",
        95 => "BC6H_UF16",
        96 => "BC6H_SF16",
        98 => "BC7_UNORM",
        99 => "BC7_UNORM_SRGB",
        _ => return format!("DXGI_{format}"),
    };
    known.to_string()
}

pub fn choose_preview_mip(width: u32, height: u32, mips: u32) -> u32 {
    let mut level = 0;
    while u64::from((width >> level).max(1)) * u64::from((height >> level).max(1)) > MAX_PREVIEW_PIXELS
        && level + 1 < mips
    {
        level += 1;
    }
    level
}

pub fn linear_len(width: u32, height: u32, format: u32) -> R<usize> {
    let px = if is_bc(format) { 4 } else { 1 };
    Ok(width.div_ceil(px) as usize * height.div_ceil(px) as usize * element_bytes(format)?)
}

fn quantize(value: f64) -> u8 {
    if value.is_nan() { 0 } else { (value.clamp(0.0, 1.0) * 255.0).round() as u8 }
}

fn half(bits: u16) -> f64 {
    let sign = if bits & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = i32::from((bits >> 10) & 0x1f);
    let frac = f64::from(bits & 0x3ff);
    sign * match exp {
        0 => frac * 2f64.powi(-24),
        31 => if frac == 0.0 { f64::INFINITY } else { f64::NAN },
        _ => (1.0 + frac / 1024.0) * 2f64.powi(exp - 15),
    }
}

/// One BC4_SNORM block to 16 channel values mapped from [-1, 1] to [0, 255].
fn snorm_block(b: &[u8]) -> [u8; 16] {
    let end = |v: u8| f64::from((v as i8).max(-127)) / 127.0;
    let (r0, r1) = (end(b[0]), end(b[1]));
    let mut palette = [r0, r1, 0.0, 0.0, 0.0, 0.0, -1.0, 1.0];
    if b[0] as i8 > b[1] as i8 {
        for i in 1..7 {
            palette[i + 1] = (f64::from(7 - i as u8) * r0 + f64::from(i as u8) * r1) / 7.0;
        }
    } else {
        for i in 1..5 {
            palette[i + 1] = (f64::from(5 - i as u8) * r0 + f64::from(i as u8) * r1) / 5.0;
        }
    }
    let bits = u64::from_le_bytes([b[2], b[3], b[4], b[5], b[6], b[7], 0, 0]);
    std::array::from_fn(|i| quantize((palette[((bits >> (3 * i)) & 7) as usize] + 1.0) / 2.0))
}

/// BC4_SNORM (grey) / BC5_SNORM (red-green) to BGRA u32 pixels.
fn decode_snorm(data: &[u8], w: usize, h: usize, two: bool, image: &mut [u32]) {
    let stride = if two { 16 } else { 8 };
    let bw = w.div_ceil(4);
    for (n, block) in data.chunks_exact(stride).enumerate() {
        let (bx, by) = (n % bw * 4, n / bw * 4);
        let red = snorm_block(&block[..8]);
        let green = if two { snorm_block(&block[8..]) } else { red };
        for i in 0..16 {
            let (x, y) = (bx + i % 4, by + i / 4);
            if x < w && y < h {
                let blue = if two { 0 } else { red[i] };
                image[y * w + x] = u32::from_le_bytes([blue, green[i], red[i], 255]);
            }
        }
    }
}

/// Decodes a tightly packed mip plane to straight RGBA8.
pub fn decode_rgba(linear: &[u8], width: u32, height: u32, format: u32) -> R<Vec<u8>> {
    if width < 1 || height < 1 || u64::from(width) * u64::from(height) > MAX_PREVIEW_PIXELS {
        return Err("preview exceeds the 16-megapixel limit".into());
    }
    if linear.len() != linear_len(width, height, format)? {
        return Err("linear texture length disagrees with its descriptor".into());
    }
    let (w, h) = (width as usize, height as usize);
    if is_bc(format) {
        let mut image = vec![0u32; w * h];
        use texture2ddecoder as t;
        let result = match format {
            71 | 72 => t::decode_bc1a(linear, w, h, &mut image),
            74 | 75 => t::decode_bc2(linear, w, h, &mut image),
            77 | 78 => t::decode_bc3(linear, w, h, &mut image),
            80 => t::decode_bc4(linear, w, h, &mut image),
            83 => t::decode_bc5(linear, w, h, &mut image),
            95 => t::decode_bc6_unsigned(linear, w, h, &mut image),
            96 => t::decode_bc6_signed(linear, w, h, &mut image),
            98 | 99 => t::decode_bc7(linear, w, h, &mut image),
            81 | 84 => {
                decode_snorm(linear, w, h, format == 84, &mut image);
                Ok(())
            }
            _ => return Err(format!("{} has no 8-bit preview decoder", name(format))),
        };
        result.map_err(str::to_string)?;
        let mut out = Vec::with_capacity(w * h * 4);
        for pixel in image {
            let [b, g, r, a] = pixel.to_le_bytes();
            out.extend_from_slice(&[r, g, b, a]);
        }
        return Ok(out);
    }
    let eb = element_bytes(format)?;
    let u16at = |p: usize| u16::from_le_bytes([linear[p], linear[p + 1]]);
    let u32at = |p: usize| u32::from_le_bytes(linear[p..p + 4].try_into().unwrap());
    let mut out = Vec::with_capacity(w * h * 4);
    for i in 0..w * h {
        let p = i * eb;
        let d = &linear[p..p + eb];
        let grey = |v: u8| [v, v, v, 255];
        let hf = |o: usize| quantize(half(u16at(p + o)));
        let pixel = match format {
            28 | 29 => [d[0], d[1], d[2], d[3]],
            87 | 91 => [d[2], d[1], d[0], d[3]],
            88 | 93 => [d[2], d[1], d[0], 255],
            61 | 65 => grey(d[0]),
            49 => [d[0], d[1], 0, 255],
            56 => grey(quantize(f64::from(u16at(p)) / 65535.0)),
            54 => grey(hf(0)),
            41 => grey(quantize(f64::from(f32::from_bits(u32at(p))))),
            34 => [hf(0), hf(2), 0, 255],
            10 => [hf(0), hf(2), hf(4), hf(6)],
            11 => {
                let n = |o| quantize(f64::from(u16at(p + o)) / 65535.0);
                [n(0), n(2), n(4), n(6)]
            }
            24 => {
                let v = u32at(p);
                let c = |s: u32| quantize(f64::from((v >> s) & 1023) / 1023.0);
                [c(0), c(10), c(20), ((v >> 30) * 85) as u8]
            }
            _ => return Err(format!("{} has no pixel decoder", name(format))),
        };
        out.extend_from_slice(&pixel);
    }
    Ok(out)
}

pub fn write_png(path: &std::path::Path, width: u32, height: u32, rgba: &[u8]) -> R<()> {
    let file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(rgba).map_err(|e| e.to_string())
}

/// Writes the 148-byte DX10 DDS header shared by RAGE and TTPF exports.
#[allow(clippy::too_many_arguments)]
pub fn dds_header(
    out: &mut Vec<u8>,
    width: u32,
    height: u32,
    depth: u32,
    dimension: u32,
    mips: u32,
    format: u32,
    pitch: u32,
) {
    let (bc, volume, cube) = (is_bc(format), dimension == 3, dimension == 2);
    let mut w = |v: u32| out.extend_from_slice(&v.to_le_bytes());
    w(u32::from_le_bytes(*b"DDS "));
    w(124);
    w((if bc { 0xA1007 } else { 0x2100F }) | if volume { 0x800000 } else { 0 });
    w(height);
    w(width);
    w(pitch);
    w(if volume { depth } else { 0 });
    w(mips);
    for _ in 0..11 {
        w(0);
    }
    w(32);
    w(4);
    w(0x30315844);
    for _ in 0..5 {
        w(0);
    }
    w((if mips > 1 { 0x401008 } else { 0x1000 }) | if volume || cube || depth > 1 { 8 } else { 0 });
    w(if volume { 0x200000 } else if cube { 0xFE00 } else { 0 });
    for _ in 0..3 {
        w(0);
    }
    w(format);
    w(if volume { 4 } else { 3 });
    w(if cube { 4 } else { 0 });
    w(if volume { 1 } else if cube { depth / 6 } else { depth });
    w(0);
}
