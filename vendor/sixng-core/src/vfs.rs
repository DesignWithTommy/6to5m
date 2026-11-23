//! Process-wide in-memory files for derived work data.
//!
//! After `set_memory(work)`, the derived data under `work` (placements,
//! collision, batches, LOD / interior tables, fivem side tables: see
//! `derived`) is kept in memory instead of on disk; the scan indexes and
//! fingerprint / texture records stay on disk. Paths outside are passed straight to `std::fs`, so the
//! functions here are drop-in replacements for the `std::fs` calls of the
//! producers (export, lod links, interior entities) and their consumers:
//! nothing extracted or derived is written to disk during a build.

use std::{
    collections::BTreeMap,
    fs, io,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
    time::SystemTime,
};

static ROOT: RwLock<Option<PathBuf>> = RwLock::new(None);
static FILES: Mutex<BTreeMap<PathBuf, (Arc<Vec<u8>>, SystemTime)>> = Mutex::new(BTreeMap::new());

/// Keep derived files under `work` in memory from now on.
pub fn set_memory(work: &Path) {
    *ROOT.write().unwrap() = Some(norm(work));
}

fn norm(p: &Path) -> PathBuf {
    PathBuf::from(p.to_string_lossy().replace('\\', "/").trim_end_matches('/').to_lowercase())
}

/// Derived data under the work dir (relative path, lowercase, `/`): the
/// sixng exports and the tables computed from them. Everything else (scan
/// indexes, fingerprint and texture records, a default --out under the work
/// dir) stays on disk.
fn derived(rel: &str) -> bool {
    let top = rel.split('/').next().unwrap_or("");
    // surface categories come from a separate `sixng surfaces` run: on disk
    if rel.ends_with("/surface_categories.json") {
        return false;
    }
    if ["placements", "collision", "fivem_batches", "interiors", "lights"].contains(&top) {
        return true;
    }
    let file = |name: &str| ["fivem_meta_xy", "fivem_ground", "fivem_water_low", "fivem_paths_src", "fivem_navmesh_index", "fivem_collision_index", "zone_names"].iter().any(|d| name.starts_with(d));
    match rel.strip_prefix("index/") {
        Some(name) => !name.contains('/') && ["lod_parents", "archetype_lod", "archetype_links", "interior_entities", "light_index"].iter().any(|d| name.starts_with(d)),
        None => !rel.contains('/') && file(rel),
    }
}

/// Memory key of `p`, None = on disk.
fn mem(p: &Path) -> Option<PathBuf> {
    let root = ROOT.read().unwrap();
    let root = root.as_ref()?;
    let n = norm(p);
    let rel = n.strip_prefix(root).ok()?;
    derived(&rel.to_string_lossy()).then_some(n)
}

pub fn write(p: impl AsRef<Path>, b: impl AsRef<[u8]>) -> io::Result<()> {
    let p = p.as_ref();
    match mem(p) {
        Some(k) => {
            FILES.lock().unwrap().insert(k, (Arc::new(b.as_ref().to_vec()), SystemTime::now()));
            Ok(())
        }
        None => fs::write(p, b),
    }
}

pub fn read(p: impl AsRef<Path>) -> io::Result<Vec<u8>> {
    let p = p.as_ref();
    match mem(p) {
        Some(k) => FILES.lock().unwrap().get(&k).map(|f| f.0.to_vec()).ok_or_else(|| not_found(p)),
        None => fs::read(p),
    }
}

/// Shared bytes (no copy for memory files).
pub fn read_arc(p: impl AsRef<Path>) -> io::Result<Arc<Vec<u8>>> {
    let p = p.as_ref();
    match mem(p) {
        Some(k) => FILES.lock().unwrap().get(&k).map(|f| f.0.clone()).ok_or_else(|| not_found(p)),
        None => fs::read(p).map(Arc::new),
    }
}

pub fn read_to_string(p: impl AsRef<Path>) -> io::Result<String> {
    String::from_utf8(read(p)?).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

fn not_found(p: &Path) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, format!("{}: not in memory", p.display()))
}

/// No-op for memory dirs.
pub fn create_dir_all(p: impl AsRef<Path>) -> io::Result<()> {
    let p = p.as_ref();
    match mem(p) {
        Some(_) => Ok(()),
        None => fs::create_dir_all(p),
    }
}

pub fn exists(p: impl AsRef<Path>) -> bool {
    let p = p.as_ref();
    match mem(p) {
        Some(k) => {
            let f = FILES.lock().unwrap();
            f.contains_key(&k) || f.range(k.clone()..).next().is_some_and(|(x, _)| x.starts_with(&k))
        }
        None => p.exists(),
    }
}

/// Modification time (memory files: when written).
pub fn modified(p: impl AsRef<Path>) -> Option<SystemTime> {
    let p = p.as_ref();
    match mem(p) {
        Some(k) => FILES.lock().unwrap().get(&k).map(|f| f.1),
        None => fs::metadata(p).and_then(|m| m.modified()).ok(),
    }
}

pub fn len(p: impl AsRef<Path>) -> Option<u64> {
    let p = p.as_ref();
    match mem(p) {
        Some(k) => FILES.lock().unwrap().get(&k).map(|f| f.0.len() as u64),
        None => fs::metadata(p).map(|m| m.len()).ok(),
    }
}

pub fn remove_file(p: impl AsRef<Path>) -> io::Result<()> {
    let p = p.as_ref();
    match mem(p) {
        Some(k) => FILES.lock().unwrap().remove(&k).map(|_| ()).ok_or_else(|| not_found(p)),
        None => fs::remove_file(p),
    }
}

/// Drop a whole memory dir (or remove it on disk).
pub fn remove_dir_all(p: impl AsRef<Path>) -> io::Result<()> {
    let p = p.as_ref();
    match mem(p) {
        Some(k) => {
            FILES.lock().unwrap().retain(|x, _| !x.starts_with(&k));
            Ok(())
        }
        None => fs::remove_dir_all(p),
    }
}

pub fn rename(a: impl AsRef<Path>, b: impl AsRef<Path>) -> io::Result<()> {
    let (a, b) = (a.as_ref(), b.as_ref());
    match (mem(a), mem(b)) {
        (None, None) => fs::rename(a, b),
        _ => {
            let x = read(a)?;
            write(b, x)?;
            remove_file(a)
        }
    }
}

/// Entry of `read_dir` (files only for memory dirs).
pub struct DirEntry(PathBuf);

impl DirEntry {
    pub fn path(&self) -> PathBuf {
        self.0.clone()
    }
    pub fn file_name(&self) -> std::ffi::OsString {
        self.0.file_name().map(|n| n.to_os_string()).unwrap_or_default()
    }
}

/// Entries of `dir` like `std::fs::read_dir`: memory files directly in it
/// (a memory dir lists no subdirs), plus the disk entries when `dir` is
/// not a memory dir (e.g. derived tables beside the scan indexes).
pub fn read_dir(dir: impl AsRef<Path>) -> io::Result<std::vec::IntoIter<io::Result<DirEntry>>> {
    let dir = dir.as_ref();
    let k = norm(dir);
    let mut v: Vec<io::Result<DirEntry>> = {
        let f = FILES.lock().unwrap();
        f.range(k.clone()..).take_while(|(x, _)| x.starts_with(&k)).filter(|(x, _)| x.parent() == Some(k.as_path())).map(|(x, _)| Ok(DirEntry(x.clone()))).collect()
    };
    if mem(dir).is_none() {
        match fs::read_dir(dir) {
            // derived files left on disk by older builds are not read
            Ok(rd) => v.extend(rd.filter_map(|e| e.map(|e| Some(DirEntry(e.path())).filter(|d| mem(&d.0).is_none())).transpose())),
            Err(e) if v.is_empty() => return Err(e),
            Err(_) => {}
        }
    }
    Ok(v.into_iter())
}

/// Stand-in for `std::fs::File`: a disk file, or a memory file being
/// written (stored on drop) or read.
pub enum File {
    Disk(fs::File),
    MemW(PathBuf, Vec<u8>),
    MemR(io::Cursor<Arc<Vec<u8>>>),
}

impl File {
    pub fn create(p: impl AsRef<Path>) -> io::Result<File> {
        let p = p.as_ref();
        match mem(p) {
            Some(k) => Ok(File::MemW(k, Vec::new())),
            None => fs::File::create(p).map(File::Disk),
        }
    }
    pub fn open(p: impl AsRef<Path>) -> io::Result<File> {
        let p = p.as_ref();
        match mem(p) {
            Some(_) => read_arc(p).map(|b| File::MemR(io::Cursor::new(b))),
            None => fs::File::open(p).map(File::Disk),
        }
    }
}

impl Write for File {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        match self {
            File::Disk(f) => f.write(b),
            File::MemW(_, v) => {
                v.extend_from_slice(b);
                Ok(b.len())
            }
            File::MemR(_) => Err(io::Error::other("memory file opened for reading")),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            File::Disk(f) => f.flush(),
            _ => Ok(()),
        }
    }
}

impl Read for File {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        match self {
            File::Disk(f) => f.read(b),
            File::MemR(c) => {
                let pos = c.position() as usize;
                let src = c.get_ref();
                let n = b.len().min(src.len().saturating_sub(pos));
                b[..n].copy_from_slice(&src[pos..pos + n]);
                c.set_position((pos + n) as u64);
                Ok(n)
            }
            File::MemW(..) => Err(io::Error::other("memory file opened for writing")),
        }
    }
}

impl Drop for File {
    fn drop(&mut self) {
        if let File::MemW(k, v) = self {
            FILES.lock().unwrap().insert(std::mem::take(k), (Arc::new(std::mem::take(v)), SystemTime::now()));
        }
    }
}
