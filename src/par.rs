//! Parallel helpers shared by the scanners. `--threads N` sets the worker
//! count once per process (0 = every core); every helper gives the same
//! output, in the same order, as the single-threaded loop it replaces.

use crate::progress::{self, Progress};
use std::{
    collections::BTreeMap,
    ops::Range,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

static THREADS: AtomicUsize = AtomicUsize::new(0);

/// Set the worker count (0 = all cores).
pub fn set_threads(n: usize) {
    THREADS.store(n, Ordering::Relaxed);
}

/// The worker count in effect.
pub fn threads() -> usize {
    match THREADS.load(Ordering::Relaxed) {
        0 => std::thread::available_parallelism().map_or(4, |n| n.get()),
        n => n,
    }
}

/// Parse `--threads N` out of an argument list, setting the worker count.
pub fn take_threads(args: &[std::ffi::OsString]) -> Result<Vec<std::ffi::OsString>, String> {
    let mut rest = Vec::with_capacity(args.len());
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if arg == "--threads" || arg == "-j" {
            let value = it.next().and_then(|v| v.to_str()).ok_or("--threads needs a number")?;
            set_threads(value.parse().map_err(|e| format!("--threads {value}: {e}"))?);
        } else {
            rest.push(arg.clone());
        }
    }
    Ok(rest)
}

/// A shared bounded, ordered work queue: `count` jobs are computed by
/// `work(index)` on worker threads, at most `ahead` jobs past the one being
/// consumed, and handed to `take` strictly in index order. `take` returns
/// false to stop early. Jobs run with the caller's progress hook installed.
pub fn ordered<T, W, F>(count: usize, ahead: usize, work: W, mut take: F)
where
    T: Send,
    W: Fn(usize) -> T + Sync,
    F: FnMut(usize, T) -> bool,
{
    let threads = threads().min(count.max(1));
    if threads <= 1 {
        for i in 0..count {
            if progress::cancelled() || !take(i, work(i)) {
                return;
            }
        }
        return;
    }
    let hook = progress::current();
    let ahead = ahead.max(threads);
    struct State<T> {
        done: BTreeMap<usize, T>,
        consumed: usize,
    }
    let state = Mutex::new(State { done: BTreeMap::new(), consumed: 0 });
    let (ready, room) = (Condvar::new(), Condvar::new());
    let (next, stop) = (AtomicUsize::new(0), AtomicBool::new(false));
    let cancelled = |hook: &Option<Arc<Progress>>| hook.as_ref().is_some_and(|p| p.cancelled());
    std::thread::scope(|s| {
        for _ in 0..threads {
            let hook = hook.clone();
            let (state, ready, room, next, stop, work) = (&state, &ready, &room, &next, &stop, &work);
            s.spawn(move || {
                progress::install(hook.clone());
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= count {
                        break;
                    }
                    {
                        let mut g = state.lock().unwrap();
                        while i >= g.consumed + ahead && !stop.load(Ordering::Relaxed) {
                            g = room.wait(g).unwrap();
                        }
                    }
                    if stop.load(Ordering::Relaxed) || cancelled(&hook) {
                        break;
                    }
                    let out = work(i);
                    state.lock().unwrap().done.insert(i, out);
                    ready.notify_all();
                }
                // Wake the consumer so it notices finished or stopped workers.
                ready.notify_all();
            });
        }
        let mut i = 0;
        while i < count {
            let item = {
                let mut g = state.lock().unwrap();
                loop {
                    if let Some(item) = g.done.remove(&i) {
                        break Some(item);
                    }
                    if cancelled(&hook) {
                        break None;
                    }
                    g = ready.wait_timeout(g, std::time::Duration::from_millis(50)).unwrap().0;
                }
            };
            let keep = match item {
                Some(item) => take(i, item),
                None => false,
            };
            i += 1;
            state.lock().unwrap().consumed = i;
            room.notify_all();
            if !keep {
                break;
            }
        }
        {
            let _g = state.lock().unwrap();
            stop.store(true, Ordering::Relaxed);
        }
        room.notify_all();
    });
}

/// Ordered parallel map over `items` (collected in input order).
pub fn map<I: Sync, T: Send>(items: &[I], f: impl Fn(&I) -> T + Sync) -> Vec<T> {
    let mut out = Vec::with_capacity(items.len());
    ordered(items.len(), threads() * 4, |i| f(&items[i]), |_, t| {
        out.push(t);
        true
    });
    out
}

/// Number of zero bytes in `bytes`, counted in parallel.
pub fn zeros(bytes: &[u8]) -> usize {
    const PIECE: usize = 64 << 20;
    if bytes.len() <= PIECE || threads() <= 1 {
        return bytes.iter().filter(|&&b| b == 0).count();
    }
    let pieces: Vec<_> = bytes.chunks(PIECE).collect();
    map(&pieces, |p| p.iter().filter(|&&b| b == 0).count()).into_iter().sum()
}

/// One hit of a greedy scan: an item found at `at` after which the scan
/// resumes at `next` (> `at`).
struct Hit<T> {
    at: usize,
    next: usize,
    item: T,
}

struct Region<T> {
    hits: Vec<Hit<T>>,
    /// First position the worker did not visit (>= the region end).
    stop: usize,
}

/// Parallel form of the greedy loop
///
/// ```text
/// pos = range.start
/// while pos < range.end:
///     match step(pos) { Some((next, item)) => { emit(pos, item); pos = next } None => pos += stride }
/// ```
///
/// `step` must depend only on `pos` (and the immutable data it reads), with
/// `next` on the same `stride` grid. The range is cut into `region`-byte
/// pieces scanned speculatively in parallel, each from its own start; while
/// merging, a piece whose start was skipped over by a hit of the previous
/// one is re-synchronised by stepping from the true position until it meets
/// a position the worker also visited, from where both runs coincide. Items
/// reach `emit` in scan order; `emit` returns false to stop. Progress (bytes
/// merged) and cancel go through the caller's [`progress`] hook.
pub fn greedy<S, T, M, P, E>(range: Range<usize>, stride: usize, region: usize, make: M, step: P, mut emit: E)
where
    T: Send,
    M: Fn() -> S + Sync,
    P: Fn(&mut S, usize) -> Option<(usize, T)> + Sync,
    E: FnMut(usize, T) -> bool,
{
    let Range { start, end } = range;
    if start >= end {
        return;
    }
    // Regions only bound the speculative work; any size gives the same
    // output. Cap them so every core gets many pieces to balance over.
    let pieces = threads() * 16;
    let region = region.min(((end - start) / pieces).max(256 << 10));
    let region = region.next_multiple_of(stride).max(stride);
    let hook = progress::current();
    let report = |pos: usize| hook.as_ref().is_some_and(|p| {
        p.done.store(pos as u64, Ordering::Relaxed);
        p.cancelled()
    });
    if threads() <= 1 {
        let (mut state, mut pos, mut ticker) = (make(), start, 0);
        while pos < end {
            if pos >= ticker {
                ticker = pos + (4 << 20);
                if report(pos) {
                    return;
                }
            }
            match step(&mut state, pos) {
                Some((next, item)) => {
                    if !emit(pos, item) {
                        return;
                    }
                    pos = next;
                }
                None => pos += stride,
            }
        }
        return;
    }

    let count = (end - start).div_ceil(region);
    let bounds = |i: usize| (start + i * region, (start + (i + 1) * region).min(end));
    let (mut pos, mut resync) = (start, make());
    let mut stopped = false;
    ordered(
        count,
        threads() * 8,
        |i| {
            let (from, to) = bounds(i);
            let (mut state, mut at, mut hits) = (make(), from, Vec::new());
            let mut ticker = from + (4 << 20);
            while at < to {
                if at >= ticker {
                    ticker = at + (4 << 20);
                    if progress::cancelled() {
                        break;
                    }
                }
                match step(&mut state, at) {
                    Some((next, item)) => {
                        hits.push(Hit { at, next, item });
                        at = next;
                    }
                    None => at += stride,
                }
            }
            Region { hits, stop: at.max(to) }
        },
        |i, r: Region<T>| {
            let (_, to) = bounds(i);
            if hook.as_ref().is_some_and(|p| p.cancelled()) {
                return false;
            }
            // Step from the true position until it is one the worker visited.
            loop {
                if pos >= r.stop || pos >= end {
                    break;
                }
                let inside = r.hits.partition_point(|h| h.at < pos);
                let skipped = inside > 0 && r.hits[inside - 1].next > pos;
                if !skipped {
                    for h in r.hits.into_iter().skip(inside) {
                        if !emit(h.at, h.item) {
                            stopped = true;
                            return false;
                        }
                        pos = h.next;
                    }
                    pos = pos.max(r.stop);
                    break;
                }
                match step(&mut resync, pos) {
                    Some((next, item)) => {
                        if !emit(pos, item) {
                            stopped = true;
                            return false;
                        }
                        pos = next;
                    }
                    None => pos += stride,
                }
            }
            !stopped && !report(to)
        },
    );
}
