//! Asset-key index over every streaming pool under a directory
//! (StreamingTextureCatalog.cs). Keyed dictionary textures
//! (`texture_<KEY>`) resolve to the pool texture holding their pixels.

use super::ttpf::Pool;
use memmap2::Mmap;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

const MAX_POOLS: usize = 256;

pub struct Location {
    pub path: PathBuf,
    pub index: usize,
    pub name: String,
    pub extractable: bool,
    pub base_mip: u32,
    pub mips: u32,
}

pub struct Catalog {
    locations: HashMap<u64, Vec<Location>>,
    pub inspected: usize,
    pub skipped: usize,
}

/// Outermost ancestor (up to 6 levels) of `archive` containing `data/ttdb`.
pub fn find_content_dir(archive: &Path) -> Option<PathBuf> {
    let full = fs::canonicalize(archive).ok()?;
    let mut found = None;
    for dir in full.ancestors().skip(1).take(6) {
        if dir.join("data").join("ttdb").is_dir() {
            found = Some(dir.to_path_buf());
        }
    }
    found
}

fn pools(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else { continue };
        let path = entry.path();
        if kind.is_dir() {
            pools(&path, out);
        } else if kind.is_file() && path.extension().is_some_and(|e| e.eq_ignore_ascii_case("ttpf")) {
            out.push(path);
        }
        if out.len() > MAX_POOLS {
            return;
        }
    }
}

impl Catalog {
    pub fn build(dir: &Path) -> Result<Self, String> {
        let mut files = Vec::new();
        pools(dir, &mut files);
        if files.len() > MAX_POOLS {
            return Err("texture catalog exceeds the 256-pool limit".into());
        }
        files.sort_by_key(|p| p.to_string_lossy().to_lowercase());
        let mut locations: HashMap<u64, Vec<Location>> = HashMap::new();
        let (mut inspected, mut skipped) = (0, 0);
        for path in files {
            let opened = fs::File::open(&path).ok().and_then(|f| {
                // SAFETY: read-only map of a pool that is not modified while read.
                unsafe { Mmap::map(&f) }.ok()
            });
            let Some(map) = opened else {
                skipped += 1;
                continue;
            };
            let Ok(pool) = Pool::open(&map) else {
                skipped += 1;
                continue;
            };
            inspected += 1;
            for t in pool.textures.iter().filter(|t| t.asset_key != 0) {
                locations.entry(t.asset_key).or_default().push(Location {
                    path: path.clone(),
                    index: t.index,
                    name: t.name.clone(),
                    extractable: t.limitation.is_none(),
                    base_mip: t.base_mip,
                    mips: t.mips,
                });
            }
        }
        for list in locations.values_mut() {
            list.sort_by(|a, b| {
                b.extractable.cmp(&a.extractable).then(a.base_mip.cmp(&b.base_mip)).then(b.mips.cmp(&a.mips))
            });
        }
        Ok(Self { locations, inspected, skipped })
    }

    pub fn find(&self, key: u64) -> &[Location] {
        self.locations.get(&key).map_or(&[], Vec::as_slice)
    }

    pub fn keys(&self) -> usize {
        self.locations.len()
    }
}
