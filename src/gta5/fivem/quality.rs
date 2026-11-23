//! `[t6m:quality]` build diagnostics for live tests: counts plus examples
//! per category, printed at the end of a build and written to
//! `<out>/t6m_report/quality.json`.
//!
//! Categories: `palette_fill` (triangles whose tint-palette entry is the
//! unused-entry fill, per model), `colour_pick` (colour slot hash ->
//! texture per material), `flat_decode` (textures rejected as flat /
//! undecoded BCPack), `bcpack_skipped_mips` (TTPF textures whose top mips
//! had no clean BCPack data). 4-layer terrain binds: `[t6m:terrain]`.
//!
//! `missing_textures.tsv` (same folder) lists every texture behind a
//! placeholder: hash64, pool key, hash32, the archetypes that reference it
//! and where it was looked for (every indexed rpf texture dictionary by pool
//! key, then every TTPF store by hash64), so one build answers whether the
//! texture exists anywhere in the install.

use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Mutex, OnceLock},
};

const EXAMPLES: usize = 40;

#[derive(Default)]
struct Cat {
    count: u64,
    examples: Vec<String>,
}

/// hash64 -> (pool key, hash32, owning archetypes, lookup result)
type Missing = BTreeMap<u64, (u64, u32, std::collections::BTreeSet<String>, String)>;

fn missing_state() -> &'static Mutex<Missing> {
    static S: OnceLock<Mutex<Missing>> = OnceLock::new();
    S.get_or_init(Default::default)
}

/// Records a texture that resolved nowhere (the material got a placeholder).
pub(super) fn missing(h64: u64, key: u64, hash: u32, owner: &str, why: &str) {
    let mut m = missing_state().lock().unwrap();
    let e = m.entry(h64).or_insert_with(|| (key, hash, Default::default(), why.to_string()));
    e.2.insert(owner.to_string());
}

fn state() -> &'static Mutex<BTreeMap<&'static str, Cat>> {
    static S: OnceLock<Mutex<BTreeMap<&'static str, Cat>>> = OnceLock::new();
    S.get_or_init(Default::default)
}

/// Counts `n` events of `cat`; keeps the first examples (the first few are
/// also printed as they happen).
pub(super) fn hit(cat: &'static str, n: u64, example: impl FnOnce() -> String) {
    let mut s = state().lock().unwrap();
    let c = s.entry(cat).or_default();
    c.count += n;
    if c.examples.len() < EXAMPLES {
        let e = example();
        if c.examples.len() < 5 {
            println!("[t6m:quality] {cat}: {e}");
        }
        c.examples.push(e);
    }
}

/// Prints the per-category totals and writes `<out>/t6m_report/quality.json`.
pub(super) fn finish(out: &Path) {
    let s = state().lock().unwrap();
    let mut j = serde_json::Map::new();
    for (k, c) in s.iter() {
        println!("[t6m:quality] {k}: {} (examples: {})", c.count, c.examples.iter().take(3).cloned().collect::<Vec<_>>().join("; "));
        j.insert(k.to_string(), json!({"count": c.count, "examples": c.examples}));
    }
    if s.is_empty() {
        println!("[t6m:quality] no events");
    }
    let dir = out.join("t6m_report");
    let _ = std::fs::create_dir_all(&dir);
    let m = missing_state().lock().unwrap();
    let refs: usize = m.values().map(|v| v.2.len()).sum();
    println!("[t6m:quality] missing_textures: {} distinct hash64, {refs} archetype references (t6m_report/missing_textures.tsv)", m.len());
    let mut tsv = String::from("hash64\tpool_key\thash32\tarchetypes\tlookup\n");
    for (h64, (key, hash, owners, why)) in m.iter() {
        tsv.push_str(&format!("{h64:016x}\t{key:016x}\t{hash:08x}\t{}\t{}\n", owners.iter().cloned().collect::<Vec<_>>().join(","), why.replace(['\t', '\n'], " ")));
    }
    let mp = dir.join("missing_textures.tsv");
    if let Err(e) = std::fs::write(&mp, tsv) {
        println!("[t6m:quality] cannot write {}: {e}", mp.display());
    }
    let p = dir.join("quality.json");
    match serde_json::to_vec_pretty(&Value::Object(j)) {
        Ok(b) => {
            if let Err(e) = std::fs::write(&p, b) {
                println!("[t6m:quality] cannot write {}: {e}", p.display());
            }
        }
        Err(e) => println!("[t6m:quality] {e}"),
    }
}
