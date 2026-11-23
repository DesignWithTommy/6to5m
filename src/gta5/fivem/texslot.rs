//! Source-texture cache slots: `<dir>/<id>.dds` (texture `r6m_<id>`).
//!
//! The id is the 32-bit texture hash, but different 6m textures share 32-bit
//! hashes: the cache used to hand one texture's DDS to every later texture
//! with the same hash (a weapon's 4 x 4 placeholder came out as a building
//! wall). The texture's identity is its 64-bit hash (TTPF / texture record
//! +8) or its pool asset key, so each slot now records the identity that
//! owns it (`<id>.id`). A texture whose 32-bit slot belongs to another
//! identity moves to a slot derived from its 64-bit hash.
//!
//! Slots cached before this check have no `.id`: they are adopted when the
//! DDS size fits the texture's stored size (or a mip of it), otherwise the
//! texture moves.

use std::{
    fs,
    io::Read,
    path::Path,
    sync::atomic::{AtomicUsize, Ordering},
};

/// Lookups that moved off their 32-bit slot (collision), this process.
pub(super) static MOVED: AtomicUsize = AtomicUsize::new(0);

fn alt(ident: u64) -> u32 {
    ((ident ^ (ident >> 32)) as u32) ^ 0x9e37_79b9
}

fn dds_dims(p: &Path) -> Option<(u32, u32)> {
    let mut b = [0u8; 20];
    fs::File::open(p).ok()?.read_exact(&mut b).ok()?;
    (&b[..4] == b"DDS ").then(|| (u32::from_le_bytes([b[16], b[17], b[18], b[19]]), u32::from_le_bytes([b[12], b[13], b[14], b[15]])))
}

fn same(stored: &str, key: u64, h64: u64) -> bool {
    let mut it = stored.split_whitespace().map(|x| u64::from_str_radix(x, 16).unwrap_or(0));
    let (k, h) = (it.next().unwrap_or(0), it.next().unwrap_or(0));
    if h64 != 0 && h != 0 {
        h == h64
    } else {
        key != 0 && k == key
    }
}

/// Cache id for a texture. `expect` gives its stored size (TTPF or pool).
/// `suffix` is the file suffix of the slot (`""` colour, `"_n"` normal).
pub(super) fn slot(dir: &Path, suffix: &str, hash: u32, key: u64, h64: u64, expect: &dyn Fn() -> Option<(u32, u32)>) -> u32 {
    if key == 0 && h64 == 0 {
        return hash;
    }
    let ident = format!("{key:016x} {h64:016x}");
    let ids = [hash, alt(if h64 != 0 { h64 } else { key })];
    for (i, &id) in ids.iter().enumerate() {
        let side = dir.join(format!("{id:08x}{suffix}.id"));
        match fs::read_to_string(&side) {
            Ok(s) if same(&s, key, h64) => return id,
            Ok(_) => {}
            Err(_) => {
                let dds = dir.join(format!("{id:08x}{suffix}.dds"));
                let fits = !dds.exists()
                    || match (dds_dims(&dds), expect()) {
                        (Some(d), Some(e)) => (0..12).any(|k| (e.0 >> k).max(1) == d.0 && (e.1 >> k).max(1) == d.1),
                        _ => true,
                    };
                if fits {
                    let _ = fs::write(&side, &ident);
                    return id;
                }
            }
        }
        if i == 0 {
            MOVED.fetch_add(1, Ordering::Relaxed);
        }
    }
    ids[1]
}
