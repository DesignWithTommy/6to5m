//! Region build log for live tests: counts with a few examples, printed
//! with the `[t6m:region]` tag and written to <out>/t6m_report/region.json.

use serde_json::{json, Map, Value};
use std::{collections::BTreeMap, fs, path::Path, sync::Mutex};

const EXAMPLES: usize = 5;

#[derive(Default)]
struct Bucket {
    count: u64,
    examples: Vec<String>,
}

/// category -> bucket -> count + examples
static LOG: Mutex<BTreeMap<String, BTreeMap<String, Bucket>>> = Mutex::new(BTreeMap::new());
/// category -> plain value
static VALS: Mutex<BTreeMap<String, Value>> = Mutex::new(BTreeMap::new());

/// Count one `example` under `cat` / `bucket`.
pub fn bump(cat: &str, bucket: &str, example: &str) {
    let mut l = LOG.lock().unwrap();
    let b = l.entry(cat.to_string()).or_default().entry(bucket.to_string()).or_default();
    b.count += 1;
    if b.examples.len() < EXAMPLES && !b.examples.iter().any(|e| e == example) {
        b.examples.push(example.to_string());
    }
}

/// Store a plain value under `cat`.
pub fn set(cat: &str, v: Value) {
    VALS.lock().unwrap().insert(cat.to_string(), v);
}

/// Add stream size stats, print every entry tagged and write region.json.
pub fn finish(out: &Path, stream: &Path) {
    let mut files: Vec<(String, u64)> = fs::read_dir(stream).into_iter().flatten().flatten().filter_map(|e| Some((e.file_name().to_string_lossy().into_owned(), e.metadata().ok()?.len()))).collect();
    files.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let mb = |b: u64| (b as f64 / 1048576.0 * 10.0).round() / 10.0;
    let total: u64 = files.iter().map(|f| f.1).sum();
    let ytd: Vec<&(String, u64)> = files.iter().filter(|f| f.0.ends_with(".ytd")).collect();
    let over: Vec<&&(String, u64)> = ytd.iter().filter(|f| f.1 > 14 << 20).collect();
    set("stream", json!({"files": files.len(), "total_mb": mb(total)}));
    set("ytd", json!({"count": ytd.len(), "total_mb": mb(ytd.iter().map(|f| f.1).sum()), "max_mb": ytd.first().map_or(0.0, |f| mb(f.1)), "max_file": ytd.first().map_or("", |f| f.0.as_str()),
        "over_14mb": over.len(), "over_14mb_examples": over.iter().take(EXAMPLES).map(|f| json!({"file": f.0, "mb": mb(f.1)})).collect::<Vec<_>>()}));
    let mut doc = Map::new();
    for (k, v) in VALS.lock().unwrap().iter() {
        eprintln!("[t6m:region] {k}: {v}");
        doc.insert(k.clone(), v.clone());
    }
    for (cat, bs) in LOG.lock().unwrap().iter() {
        let mut m = Map::new();
        for (b, x) in bs {
            eprintln!("[t6m:region] {cat} {b}: {} (e.g. {})", x.count, x.examples.join(", "));
            m.insert(b.clone(), json!({"count": x.count, "examples": x.examples}));
        }
        doc.insert(cat.clone(), Value::Object(m));
    }
    let dir = out.join("t6m_report");
    let p = dir.join("region.json");
    match fs::create_dir_all(&dir).and_then(|_| fs::write(&p, serde_json::to_vec_pretty(&Value::Object(doc)).unwrap())) {
        Ok(()) => eprintln!("[t6m:region] report -> {}", p.display()),
        Err(e) => eprintln!("[t6m:region] {}: {e}", p.display()),
    }
}
