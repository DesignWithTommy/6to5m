//! Engine-resident default textures (ResidentTextureDefaults.cs): metadata
//! and SHA-256 pixel checks for the blank textures RAGE keeps resident.

use super::rage::TextureInfo;
use sha2::{Digest, Sha256};

pub struct Default {
    pub asset_key: u64,
    pub name_hash: u32,
    pub name: &'static str,
    pub width: u32,
    pub height: u32,
    pub formats: &'static [u32],
    pub pixel_sha256: &'static str,
}

pub const PROFILES: [Default; 4] = [
    Default {
        asset_key: 0x1E2C9340E14A7D07,
        name_hash: 0x82079169,
        name: "blank_white",
        width: 4,
        height: 4,
        formats: &[72, 87],
        pixel_sha256: "8667E718294E9E0DF1D30600BA3EEB201F764AAD2DAD72748643E4A285E1D1F7",
    },
    Default {
        asset_key: 0x2E4EE77724C11A54,
        name_hash: 0x82DD5BD8,
        name: "blank_white2x1",
        width: 8,
        height: 4,
        formats: &[72],
        pixel_sha256: "E9175DB65A9789096CA9CB5524D3ABC2107DF03E3C9BA3AF1ACA628F9C5D3BD2",
    },
    Default {
        asset_key: 0xAA3EEFF23A15DC75,
        name_hash: 0xA082D829,
        name: "blank_ma",
        width: 4,
        height: 4,
        formats: &[71],
        pixel_sha256: "73DB2A85EF58266233629093BF4CB68E74327560745510E741133DD7276C98C0",
    },
    Default {
        asset_key: 0x4FADAEAD4CD8F15F,
        name_hash: 0xEF8E2912,
        name: "blank_mb_temp",
        width: 4,
        height: 4,
        formats: &[72],
        pixel_sha256: "510A355C88E58F527DEEE6A3FC8BBBF7EB633909E892F9A9E6CFA4796F7AB787",
    },
];

pub fn find(asset_key: u64, name_hash: u32) -> Option<&'static Default> {
    PROFILES.iter().find(|p| p.asset_key == asset_key && p.name_hash == name_hash)
}

/// Profile whose name/hash/shape/format match a dictionary texture.
pub fn match_metadata(t: &TextureInfo) -> Option<&'static Default> {
    PROFILES.iter().find(|p| {
        t.name_hash == p.name_hash
            && t.asset_key.is_none_or(|k| k == p.asset_key)
            && (t.name == p.name || t.asset_key == Some(p.asset_key))
            && t.width == p.width
            && t.height == p.height
            && t.depth == 1
            && t.dimension == 1
            && t.mips == 1
            && p.formats.contains(&t.format)
    })
}

pub fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data).iter().map(|b| format!("{b:02X}")).collect()
}

/// Checks decoded RGBA pixels against the profile's BGRA32 SHA-256.
pub fn matches_pixels(p: &Default, width: u32, height: u32, rgba: &[u8]) -> bool {
    if width != p.width || height != p.height || rgba.len() != (p.width * p.height * 4) as usize {
        return false;
    }
    let bgra: Vec<u8> = rgba.chunks_exact(4).flat_map(|c| [c[2], c[1], c[0], c[3]]).collect();
    sha256_hex(&bgra) == p.pixel_sha256
}
