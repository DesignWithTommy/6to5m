//! Build progress on stderr: the `{"event":"progress","stage","done","total",
//! "pct"}` JSON lines of `sixng index` (at most one per second per stage,
//! plus the last), or a redrawn progress bar when stderr is a terminal.

use std::{
    io::{IsTerminal, Write},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Instant,
};

pub struct Bar {
    stage: String,
    total: u64,
    done: AtomicU64,
    last: Mutex<Option<Instant>>,
}

fn tty() -> bool {
    std::io::stderr().is_terminal()
}

/// Report `done` of `total` for `stage` (rate-limited unless finished).
pub fn report(stage: &str, done: u64, total: u64, last: &Mutex<Option<Instant>>) {
    let fin = done >= total;
    {
        let mut l = last.lock().unwrap();
        if !fin && l.is_some_and(|t| t.elapsed().as_millis() < if tty() { 200 } else { 1000 }) {
            return;
        }
        *l = Some(Instant::now());
    }
    let pct = if total > 0 { (done as f64 * 1000.0 / total as f64).round() / 10.0 } else { 100.0 };
    let mut err = std::io::stderr().lock();
    if tty() {
        let w = 30usize;
        let fill = ((pct / 100.0) * w as f64).round() as usize;
        let _ = write!(err, "\r[{}{}] {pct:5.1}% {stage} ({done}/{total})\x1b[K", "#".repeat(fill.min(w)), " ".repeat(w - fill.min(w)));
        if fin {
            let _ = writeln!(err);
        }
    } else {
        let _ = writeln!(err, "{}", serde_json::json!({"event": "progress", "stage": stage, "done": done, "total": total, "pct": pct}));
    }
}

impl Bar {
    pub fn new(stage: &str, total: usize) -> Bar {
        let b = Bar { stage: stage.trim().to_string(), total: total as u64, done: AtomicU64::new(0), last: Mutex::new(None) };
        report(&b.stage, 0, b.total.max(1), &b.last);
        b
    }
    pub fn tick(&self) {
        let d = self.done.fetch_add(1, Ordering::Relaxed) + 1;
        report(&self.stage, d, self.total, &self.last);
    }
}

/// Progress callback for the sixng prep passes, reported as `stage` (sixng
/// passes an entry id as its own stage name).
pub fn sixng(stage: &str) -> sixng_core::export::Progress {
    let stage = stage.trim().to_string();
    let last = Arc::new(Mutex::new(None));
    Arc::new(move |_: &str, done: u64, total: u64| report(&stage, done, total, &last))
}
