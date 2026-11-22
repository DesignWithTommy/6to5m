//! Texture size pass for building ytds: a BC7 texture (1 byte/px) used
//! only by opaque geometry (default.sps ignores diffuse alpha; 6m packs
//! masks there) becomes DXT1 (0.5 byte/px) with a rebuilt mip chain.
//! Textures on alpha/decal/cutout geometry stay BC7 (DXT5 is the same
//! size). The DXT1 copy is cached next to the source DDS (`<stem>_d1.dds`),
//! so the transcode runs once per texture across builds.

use super::rgba_dds;
use crate::gta5::ytd::{self, Tex, TexFormat};
use std::{fs, path::Path};

/// Tag for ytd fingerprints: bump when the pass changes its output.
pub const TAG: &str = "texopt2";

/// Loads cache texture `name` from `src` (a DDS), shrunk unless `alpha`
/// (some geometry using it reads its alpha).
pub fn load(name: &str, src: &Path, alpha: bool) -> Result<Tex, String> {
    let rd = |p: &Path| fs::read(p).map_err(|e| format!("{}: {e}", p.display()));
    let stem = src.with_extension("");
    let d1 = stem.with_file_name(format!("{}_d1.dds", stem.file_name().unwrap_or_default().to_string_lossy()));
    if !alpha && d1.exists() {
        return ytd::from_dds(name, &rd(&d1)?);
    }
    let b = rd(src)?;
    let tex = ytd::from_dds(name, &b)?;
    if alpha || tex.format != TexFormat::BC7 || tex.width < 4 || tex.height < 4 {
        return Ok(tex);
    }
    let (w, h) = (tex.width as usize, tex.height as usize);
    let top = tex.format.pitch(w, h).1;
    let mut px = vec![0u32; w * h];
    if top > tex.data.len() || texture2ddecoder::decode_bc7(&tex.data[..top], w, h, &mut px).is_err() {
        return Ok(tex);
    }
    let rgba: Vec<u8> = px.iter().flat_map(|p| {
        let [b, g, r, _] = p.to_le_bytes();
        [r, g, b, 255]
    }).collect();
    let dds = rgba_dds(&rgba, w as u32, h as u32);
    let mut out = ytd::from_dds(name, &dds)?;
    out.unk32 = tex.unk32;
    out.usage_data = tex.usage_data;
    out.extra_flags = tex.extra_flags;
    let tmp = d1.with_extension("tmp");
    if fs::write(&tmp, &dds).is_ok() {
        let _ = fs::rename(&tmp, &d1);
    }
    Ok(out)
}
