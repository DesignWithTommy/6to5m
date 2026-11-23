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

/// Per-archive index side file: `<base>.<ext>` for levels.rpf (the name
/// every earlier build wrote), `<base>__<index stem>.<ext>` for any other
/// archive, so indexing story000 or shared_minigame000 does not overwrite
/// the levels.rpf results.
pub fn per_archive_name(base: &str, ext: &str, archive_id: &str) -> String {
    let stem = index_stem(archive_id);
    if stem == "levels" { format!("{base}.{ext}") } else { format!("{base}__{stem}.{ext}") }
}

/// Every per-archive side file of `base` in `dir`: levels.rpf's first,
/// then the others in name order (earlier files win on duplicate keys).
pub fn per_archive_files(dir: &Path, base: &str, ext: &str) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = vec![dir.join(format!("{base}.{ext}"))];
    let pre = format!("{base}__");
    let suf = format!(".{ext}");
    let mut rest: Vec<PathBuf> = crate::vfs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_str().is_some_and(|n| n.starts_with(&pre) && n.ends_with(&suf)))
        .map(|e| e.path())
        .collect();
    rest.sort();
    v.extend(rest);
    v.retain(|p| crate::vfs::len(p).is_some());
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
