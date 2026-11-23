//! Live-test log for fragment / vehicle .yft outputs: `[t6m:vehicles]`
//! lines on stdout and `<out>/t6m_report/vehicles.json`. Counts plus the
//! first few example names, no per-entity lines.
//!
//! Records, per yft: the `verify` result (pointer walk, composite without
//! child flag arrays), the composite written with null child flags
//! (eaa7ef9), and whether the mesh misses its 6m collider union with the
//! per-axis bbox delta (045e440). A fresh yft's record is stored in its
//! output-cache info (`t6m_vehicles`), so a build served from the cache
//! still reports it: totals are fresh + cached. Cache entries written
//! before the record existed count as `cached_unrecorded`.

use serde_json::{json, Value};
use std::{fs, path::Path, sync::Mutex};

const EXAMPLES: usize = 8;

#[derive(Default)]
struct Tally {
    yft: usize,
    verified_ok: usize,
    ok_examples: Vec<String>,
    verify_failed: Vec<(String, String)>,
    composites_null_flags: usize,
    physics_children: usize,
    outside: usize,
    outside_examples: Vec<(String, Value)>,
}

#[derive(Default)]
struct Log {
    fresh: Tally,
    cached: Tally,
    cached_unrecorded: usize,
}

static LOG: Mutex<Option<Log>> = Mutex::new(None);

fn with(f: impl FnOnce(&mut Log)) {
    if let Ok(mut g) = LOG.lock() {
        f(g.get_or_insert_with(Log::default));
    }
}

fn add(t: &mut Tally, file: &str, rec: &Value) {
    t.yft += 1;
    t.composites_null_flags += 1;
    t.physics_children += rec["children"].as_u64().unwrap_or(0) as usize;
    match rec["verify"].as_str() {
        Some("ok") => {
            t.verified_ok += 1;
            if t.ok_examples.len() < EXAMPLES {
                t.ok_examples.push(file.to_string());
            }
        }
        e => t.verify_failed.push((file.to_string(), e.unwrap_or("not verified").to_string())),
    }
    if rec["outside"].as_bool() == Some(true) {
        t.outside += 1;
        if t.outside_examples.len() < EXAMPLES {
            t.outside_examples.push((file.to_string(), rec["bbox_delta_m"].clone()));
        }
    }
}

/// A yft built this run: verify it, record it, and return the record to
/// store in its cache info. `outside`: Some(delta) when the mesh misses its
/// collision (delta None: no colliders), None when it fits.
pub(crate) fn fresh(file: &str, bytes: &[u8], children: usize, outside: Option<Option<[f32; 3]>>) -> Value {
    let verify = match super::verify::verify(file, bytes) {
        Ok(_) => "ok".to_string(),
        Err(e) => e,
    };
    let rec = json!({"verify": verify, "children": children, "outside": outside.is_some(), "bbox_delta_m": outside.flatten()});
    with(|l| add(&mut l.fresh, file, &rec));
    rec
}

/// A yft served from the output cache: its stored record, if any.
pub(crate) fn cached(file: &str, rec: Option<&Value>) {
    with(|l| match rec {
        Some(r) => add(&mut l.cached, file, r),
        None => l.cached_unrecorded += 1,
    });
}

fn tally_json(t: &Tally) -> Value {
    json!({
        "yft": t.yft,
        "verified_ok": t.verified_ok,
        "ok_examples": t.ok_examples,
        "verify_failed": t.verify_failed.iter().map(|(f, e)| json!({"file": f, "error": e})).collect::<Vec<_>>(),
        "composites_null_child_flags": t.composites_null_flags,
        "physics_children": t.physics_children,
        "outside_collision": t.outside,
        "outside_examples": t.outside_examples.iter().map(|(n, d)| json!({"name": n, "bbox_delta_m": d})).collect::<Vec<_>>(),
    })
}

fn delta_str(d: &Value) -> String {
    match d.as_array() {
        Some(a) if a.len() == 3 => format!("delta {:.2},{:.2},{:.2} m", a[0].as_f64().unwrap_or(0.0), a[1].as_f64().unwrap_or(0.0), a[2].as_f64().unwrap_or(0.0)),
        _ => "no colliders".to_string(),
    }
}

/// Print the summary lines and write the report (nothing when no yft).
pub(crate) fn flush(out: &Path) {
    let Some(l) = LOG.lock().ok().and_then(|mut g| g.take()) else { return };
    let (f, c) = (&l.fresh, &l.cached);
    let ok = f.verified_ok + c.verified_ok;
    let failed = f.verify_failed.len() + c.verify_failed.len();
    let ex: Vec<&String> = f.ok_examples.iter().chain(&c.ok_examples).take(EXAMPLES).collect();
    println!(
        "[t6m:vehicles] yft verify: {ok} ok, {failed} failed of {} recorded ({} fresh, {} cached, {} cached without a record) (examples: {})",
        f.yft + c.yft,
        f.yft,
        c.yft,
        l.cached_unrecorded,
        ex.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
    );
    for (file, e) in f.verify_failed.iter().chain(&c.verify_failed).take(EXAMPLES) {
        println!("[t6m:vehicles] yft verify FAILED {file}: {e}");
    }
    println!(
        "[t6m:vehicles] composites written with null child flag arrays: {} ({} fresh, {} cached; {} physics children)",
        f.composites_null_flags + c.composites_null_flags,
        f.composites_null_flags,
        c.composites_null_flags,
        f.physics_children + c.physics_children
    );
    let oex: Vec<String> = f.outside_examples.iter().chain(&c.outside_examples).take(EXAMPLES).map(|(n, d)| format!("{n} ({})", delta_str(d))).collect();
    println!("[t6m:vehicles] fragments outside their collision: {} ({} fresh, {} cached) (examples: {})", f.outside + c.outside, f.outside, c.outside, oex.join(", "));
    let doc = json!({
        "yft_recorded": f.yft + c.yft,
        "yft_verified_ok": ok,
        "yft_verify_failed": failed,
        "composites_null_child_flags": f.composites_null_flags + c.composites_null_flags,
        "fragments_outside_collision": f.outside + c.outside,
        "cached_unrecorded": l.cached_unrecorded,
        "fresh": tally_json(f),
        "cached": tally_json(c),
    });
    let dir = out.join("t6m_report");
    let _ = fs::create_dir_all(&dir).and_then(|_| fs::write(dir.join("vehicles.json"), serde_json::to_vec_pretty(&doc).unwrap_or_default()));
}
