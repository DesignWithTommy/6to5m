//! `[t6m:quality]` build diagnostics for live tests: counts plus examples
//! per category, printed at the end of a build and written to
//! `<out>/t6m_report/quality.json`.
//!
//! Categories: `palette_fill` (triangles whose tint-palette entry is the
//! unused-entry fill, per model), `colour_pick` (colour slot hash ->
//! texture per material), `flat_decode` (textures rejected as flat /
//! undecoded BCPack), `bcpack_skipped_mips` (TTPF textures whose top mips
//! had no clean BCPack data). 4-layer terrain binds: `[t6m:terrain]`.

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
