//! Live-test log for fragment / vehicle .yft outputs: `[t6m:vehicles]`
//! lines on stdout and `<out>/t6m_report/vehicles.json`. Counts plus the
//! first few example names, no per-entity lines.
//!
//! Records, per yft written this run: the `verify` result (pointer walk,
//! composite without child flag arrays), how many composites were written
//! with null child flags (eaa7ef9), and fragments whose mesh misses their
//! 6m collider union with the per-axis bbox delta (045e440).

use serde_json::json;
use std::{fs, path::Path, sync::Mutex};

const EXAMPLES: usize = 8;

#[derive(Default)]
struct Log {
    verified_ok: usize,
    ok_examples: Vec<String>,
    verify_failed: Vec<(String, String)>,
    composites_null_flags: usize,
    physics_children: usize,
    outside: usize,
    outside_examples: Vec<(String, Option<[f32; 3]>)>,
}

static LOG: Mutex<Option<Log>> = Mutex::new(None);

fn with(f: impl FnOnce(&mut Log)) {
    if let Ok(mut g) = LOG.lock() {
        f(g.get_or_insert_with(Log::default));
    }
}

/// A written .yft: verify it and count its composite (null child flags).
pub(crate) fn yft(file: &str, bytes: &[u8], children: usize) {
    let r = super::verify::verify(file, bytes);
    with(|l| {
        l.composites_null_flags += 1;
        l.physics_children += children;
        match r {
            Ok(_) => {
                l.verified_ok += 1;
                if l.ok_examples.len() < EXAMPLES {
                    l.ok_examples.push(file.to_string());
                }
            }
            Err(e) => l.verify_failed.push((file.to_string(), e)),
        }
    });
}

/// A fragment whose mesh misses its collision; `delta` = how far the mesh
/// bbox lies outside the collider union per axis (None: no colliders).
pub(crate) fn outside(name: &str, delta: Option<[f32; 3]>) {
    with(|l| {
        l.outside += 1;
        if l.outside_examples.len() < EXAMPLES {
            l.outside_examples.push((name.to_string(), delta));
        }
    });
}

/// Print the summary lines and write the report (nothing when no yft).
pub(crate) fn flush(out: &Path) {
    let Some(l) = LOG.lock().ok().and_then(|mut g| g.take()) else { return };
    let total = l.verified_ok + l.verify_failed.len();
    println!("[t6m:vehicles] yft verify: {} ok, {} failed of {total} (examples: {})", l.verified_ok, l.verify_failed.len(), l.ok_examples.join(", "));
    for (f, e) in l.verify_failed.iter().take(EXAMPLES) {
        println!("[t6m:vehicles] yft verify FAILED {f}: {e}");
    }
    println!("[t6m:vehicles] composites written with null child flag arrays: {} ({} physics children)", l.composites_null_flags, l.physics_children);
    let ex: Vec<String> = l
        .outside_examples
        .iter()
        .map(|(n, d)| match d {
            Some(d) => format!("{n} (delta {:.2},{:.2},{:.2} m)", d[0], d[1], d[2]),
            None => format!("{n} (no colliders)"),
        })
        .collect();
    println!("[t6m:vehicles] fragments outside their collision: {} (examples: {})", l.outside, ex.join(", "));
    let doc = json!({
        "yft_total": total,
        "yft_verified_ok": l.verified_ok,
        "yft_ok_examples": l.ok_examples,
        "yft_verify_failed": l.verify_failed.iter().map(|(f, e)| json!({"file": f, "error": e})).collect::<Vec<_>>(),
        "composites_null_child_flags": l.composites_null_flags,
        "physics_children": l.physics_children,
        "fragments_outside_collision": l.outside,
        "fragments_outside_examples": l.outside_examples.iter().map(|(n, d)| json!({"name": n, "bbox_delta_m": d})).collect::<Vec<_>>(),
    });
    let dir = out.join("t6m_report");
    let _ = fs::create_dir_all(&dir).and_then(|_| fs::write(dir.join("vehicles.json"), serde_json::to_vec_pretty(&doc).unwrap_or_default()));
}
