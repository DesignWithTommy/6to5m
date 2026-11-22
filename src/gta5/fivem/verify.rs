//! Re-parse written RSC7 outputs: decompress, check header / page flags /
//! sizes, walk the root pointers and count what is reachable. Any mismatch
//! is an error (the caller aborts the build).

use super::super::rsc::flags_size;
use std::io::Read;

struct Res<'a> {
    sys: &'a [u8],
    gfx: &'a [u8],
}

fn u16_at(d: &[u8], o: usize) -> Result<u16, String> {
    d.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]])).ok_or_else(|| format!("read u16 past end at {o:#x}"))
}
fn u32_at(d: &[u8], o: usize) -> Result<u32, String> {
    d.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).ok_or_else(|| format!("read u32 past end at {o:#x}"))
}
fn u64_at(d: &[u8], o: usize) -> Result<u64, String> {
    d.get(o..o + 8).map(|b| u64::from_le_bytes(b.try_into().unwrap())).ok_or_else(|| format!("read u64 past end at {o:#x}"))
}

impl<'a> Res<'a> {
    /// Resolve a resource pointer to `len` bytes; 0 -> None.
    fn at(&self, p: u64, len: usize) -> Result<Option<&'a [u8]>, String> {
        if p == 0 {
            return Ok(None);
        }
        let (buf, base) = match p >> 28 {
            5 => (self.sys, 0x5000_0000u64),
            6 => (self.gfx, 0x6000_0000u64),
            _ => return Err(format!("bad pointer {p:#x}")),
        };
        let o = (p - base) as usize;
        buf.get(o..o.checked_add(len).ok_or("overflow")?).map(Some).ok_or_else(|| format!("pointer {p:#x}+{len} outside its segment ({} bytes)", buf.len()))
    }
    fn need(&self, p: u64, len: usize, what: &str) -> Result<&'a [u8], String> {
        self.at(p, len)?.ok_or_else(|| format!("null {what} pointer"))
    }
}

/// Verify one output file; returns a one-line summary.
pub fn verify(file: &str, bytes: &[u8]) -> Result<String, String> {
    let ext = file.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    if ext == "xml" || ext == "lua" {
        return Ok(String::new());
    }
    let want = match ext.as_str() {
        "ytd" => 13,
        "ydr" => 165,
        "yft" => 162,
        "ybn" => 43,
        "ymap" | "ytyp" => 2,
        _ => return Err(format!("{file}: unknown output type")),
    };
    let e = |m: String| format!("{file}: {m}");
    if bytes.len() < 16 || u32_at(bytes, 0)? != 0x3743_5352 {
        return Err(e("not an RSC7 file".into()));
    }
    let version = u32_at(bytes, 4)?;
    if version != want {
        return Err(e(format!("resource version {version}, expected {want}")));
    }
    let (sf, gf) = (u32_at(bytes, 8)?, u32_at(bytes, 12)?);
    if (sf >> 28, gf >> 28) != ((want >> 4) & 0xF, want & 0xF) {
        return Err(e(format!("page flag version nibbles {:x}/{:x} do not match version {want}", sf >> 28, gf >> 28)));
    }
    let (ss, gs) = (flags_size(sf) as usize, flags_size(gf) as usize);
    let mut raw = Vec::with_capacity(ss + gs);
    flate2::read::DeflateDecoder::new(&bytes[16..]).read_to_end(&mut raw).map_err(|x| e(format!("inflate: {x}")))?;
    if raw.len() != ss + gs {
        return Err(e(format!("decompressed {} bytes, page flags say {} + {}", raw.len(), ss, gs)));
    }
    if ss == 0 {
        return Err(e("empty system segment".into()));
    }
    let r = Res { sys: &raw[..ss], gfx: &raw[ss..] };
    let body = match ext.as_str() {
        "ytd" => dict(&r, 0x5000_0000).map(|(n, mb)| format!("textures={n} tex_mb={:.1}", mb as f64 / 1048576.0)),
        "ydr" => drawable(&r),
        "yft" => Ok("fragment".to_string()),
        "ybn" => bounds(&r),
        _ => meta(&r, &ext),
    }
    .map_err(e)?;
    Ok(format!("{ext} ok: {body} sys={ss} gfx={gs}"))
}

/// TextureDictionary at `p`: (texture count, pixel bytes).
fn dict(r: &Res, p: u64) -> Result<(usize, usize), String> {
    let d = r.need(p, 64, "dictionary")?;
    let (hp, hn) = (u64_at(d, 0x20)?, u16_at(d, 0x28)? as usize);
    let (tp, tn) = (u64_at(d, 0x30)?, u16_at(d, 0x38)? as usize);
    if hn != tn {
        return Err(format!("dictionary has {hn} hashes but {tn} textures"));
    }
    let hashes = r.at(hp, 4 * hn)?.unwrap_or(&[]);
    let ptrs = r.at(tp, 8 * tn)?.unwrap_or(&[]);
    let mut prev = None;
    let mut px = 0;
    for i in 0..tn {
        let h = u32_at(hashes, 4 * i)?;
        if prev.is_some_and(|q| q >= h) {
            return Err("texture hashes not sorted/unique".into());
        }
        prev = Some(h);
        let t = r.need(u64_at(ptrs, 8 * i)?, 144, "texture")?;
        r.need(u64_at(t, 0x28)?, 1, "texture name")?;
        let (w, hgt, stride) = (u16_at(t, 0x50)? as usize, u16_at(t, 0x52)? as usize, u16_at(t, 0x56)? as usize);
        let levels = t[0x5D] as usize;
        if w == 0 || hgt == 0 || levels == 0 {
            return Err(format!("texture {i}: bad size {w}x{hgt} levels {levels}"));
        }
        let fmt = u32_at(t, 0x58)?;
        let rows = if [0x3154_5844, 0x3354_5844, 0x3554_5844, 0x3149_5441, 0x3249_5441, 0x2037_4342].contains(&fmt) { hgt.div_ceil(4) } else { hgt };
        let len = stride * rows;
        let gp = u64_at(t, 0x70)?;
        if gp >> 28 != 6 {
            return Err(format!("texture {i}: pixel pointer {gp:#x} not in the graphics segment"));
        }
        r.need(gp, len, "pixel data")?;
        px += len;
    }
    Ok((tn, px))
}

fn drawable(r: &Res) -> Result<String, String> {
    let d = r.need(0x5000_0000, 208, "drawable")?;
    let sg = r.need(u64_at(d, 0x10)?, 0x40, "shader group")?;
    let embedded = match u64_at(sg, 0x08)? {
        0 => 0,
        p => dict(r, p)?.0,
    };
    let ns = u16_at(sg, 0x18)? as usize;
    let sp = r.need(u64_at(sg, 0x10)?, 8 * ns, "shader array")?;
    for i in 0..ns {
        let s = r.need(u64_at(sp, 8 * i)?, 48, "shader")?;
        r.need(u64_at(s, 0)?, 16, "shader parameters")?;
    }
    r.need(u64_at(d, 0xA8)?, 1, "name")?;
    let mb = r.need(u64_at(d, 0x50)?, 16, "models")?;
    let nm = u16_at(mb, 8)? as usize;
    let mp = r.need(u64_at(mb, 0)?, 8 * nm, "model array")?;
    let (mut geoms, mut verts, mut idx) = (0, 0, 0);
    for m in 0..nm {
        let md = r.need(u64_at(mp, 8 * m)?, 48, "model")?;
        let ng = u16_at(md, 0x10)? as usize;
        let gp = r.need(u64_at(md, 8)?, 8 * ng, "geometry array")?;
        for g in 0..ng {
            let gd = r.need(u64_at(gp, 8 * g)?, 152, "geometry")?;
            let vb = r.need(u64_at(gd, 0x18)?, 64, "vertex buffer")?;
            let stride = u16_at(vb, 8)? as usize;
            let nv = u32_at(vb, 0x18)? as usize;
            r.need(u64_at(vb, 0x10)?, nv * stride, "vertex data")?;
            r.need(u64_at(vb, 0x30)?, 16, "vertex declaration")?;
            if u16_at(gd, 0x60)? as usize != nv {
                return Err(format!("geometry {g}: vertex count {} != buffer {nv}", u16_at(gd, 0x60)?));
            }
            let ib = r.need(u64_at(gd, 0x38)?, 32, "index buffer")?;
            let ni = u32_at(ib, 8)? as usize;
            if ni % 3 != 0 || u32_at(gd, 0x58)? as usize != ni {
                return Err(format!("geometry {g}: bad index count {ni}"));
            }
            let id = r.need(u64_at(ib, 0x10)?, 2 * ni, "index data")?;
            if id.chunks_exact(2).any(|c| u16::from_le_bytes([c[0], c[1]]) as usize >= nv) {
                return Err(format!("geometry {g}: index out of range ({nv} verts)"));
            }
            geoms += 1;
            verts += nv;
            idx += ni;
        }
    }
    if geoms == 0 {
        return Err("no geometries".into());
    }
    Ok(format!("models={nm} geoms={geoms} verts={verts} idx={idx} shaders={ns} embedded_tex={embedded}"))
}

fn bounds(r: &Res) -> Result<String, String> {
    let b = r.need(0x5000_0000, 0x80, "bounds")?;
    let comp = b[0x10] == 10;
    let b = if comp { r.need(0x5000_0000, 0xB0, "composite")? } else { b };
    let t = b[0x10];
    let name = match t {
        8 => "GeometryBVH",
        10 => "Composite",
        _ => return Err(format!("unexpected root bound type {t}")),
    };
    if t == 10 {
        let n = u16_at(b, 0xA0)? as usize;
        let cp = r.need(u64_at(b, 0x70)?, 8 * n, "children")?;
        for i in 0..n {
            let c = r.need(u64_at(cp, 8 * i)?, 0x80, "child bound")?;
            if c[0x10] != 8 {
                return Err(format!("child {i}: type {}", c[0x10]));
            }
        }
        return Ok(format!("type={name} children={n}"));
    }
    Ok(format!("type={name}"))
}

fn meta(r: &Res, ext: &str) -> Result<String, String> {
    let m = r.need(0x5000_0000, 0x50, "meta")?;
    if u32_at(m, 0x10)? != 0x5052_4430 {
        return Err("meta root has no PRD0 tag".into());
    }
    let nb = u16_at(m, 0x4C)? as usize;
    let db = r.need(u64_at(m, 0x30)?, 16 * nb, "data blocks")?;
    let mut root = None;
    for i in 0..nb {
        let len = u32_at(db, 16 * i + 4)? as usize;
        let data = r.need(u64_at(db, 16 * i + 8)?, len, "data block")?;
        root.get_or_insert(data);
    }
    let root = root.ok_or("no data blocks")?;
    let n = match ext {
        "ymap" => u16_at(root, 0x68)?,
        _ => u16_at(root, 0x20)?,
    };
    let what = if ext == "ymap" { "entities" } else { "archetypes" };
    Ok(format!("blocks={nb} {what}={n}"))
}

/// Count summary values ("geoms=3") for the report.
pub fn field(summary: &str, key: &str) -> Option<u64> {
    summary.split_whitespace().find_map(|t| t.strip_prefix(key)?.strip_prefix('=')?.parse().ok())
}
