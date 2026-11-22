//! Game install discovery: which files can be indexed.
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveInfo {
    /// Path relative to the game root, forward slashes (e.g. "levels.rpf",
    /// "data/ttdb/vehicles/ttdb_vehicles.ttpf").
    pub id: String,
    pub path: PathBuf,
    pub size: u64,
    /// "rpf9", "rds5" (ttpf texture store) or "other".
    pub container: String,
}

pub fn container_kind(p: &Path) -> String {
    let mut b = [0u8; 4];
    if File::open(p).and_then(|mut f| f.read_exact(&mut b)).is_err() {
        return "other".into();
    }
    match &b {
        b"9FPR" => "rpf9".into(),
        b"RDS5" => "rds5".into(),
        _ => "other".into(),
    }
}

/// File name used for the index of an archive id.
pub fn index_stem(id: &str) -> String {
    let s = id.strip_suffix(".rpf").unwrap_or(id);
    s.replace(['/', '\\'], "__")
}

fn walk(dir: &Path, root: &Path, out: &mut Vec<ArchiveInfo>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, root, out);
            continue;
        }
        let ext = p.extension().map(|x| x.to_string_lossy().to_lowercase()).unwrap_or_default();
        if ext != "rpf" && ext != "ttpf" {
            continue;
        }
        let id = p.strip_prefix(root).unwrap_or(&p).to_string_lossy().replace('\\', "/");
        let size = e.metadata().map(|m| m.len()).unwrap_or(0);
        out.push(ArchiveInfo { id, container: container_kind(&p), path: p, size });
    }
}

/// All indexable archives under the game root, sorted by id.
pub fn discover(root: &Path) -> Vec<ArchiveInfo> {
    let mut v = Vec::new();
    walk(root, root, &mut v);
    v.sort_by(|a, b| a.id.cmp(&b.id));
    v
}

/// Resolve a user-supplied archive name ("levels", "levels.rpf",
/// "data/ttdb/vehicles/ttdb_vehicles.ttpf", or a unique substring).
pub fn resolve<'a>(all: &'a [ArchiveInfo], q: &str) -> Option<&'a ArchiveInfo> {
    let q = q.replace('\\', "/");
    all.iter()
        .find(|a| a.id == q || a.id == format!("{q}.rpf") || index_stem(&a.id) == q)
        .or_else(|| {
            let m: Vec<_> = all.iter().filter(|a| a.id.contains(&q)).collect();
            if m.len() == 1 { Some(m[0]) } else { None }
        })
}
