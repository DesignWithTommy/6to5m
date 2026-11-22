//! Optional progress/cancel hook for long scans. The CLI never installs one,
//! so every call is a cheap no-op there; the GUI installs one per worker thread.

use std::{
    cell::RefCell,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

#[derive(Default)]
pub struct Progress {
    pub done: AtomicU64,
    pub total: AtomicU64,
    pub cancel: AtomicBool,
}

impl Progress {
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

thread_local! {
    static CURRENT: RefCell<Option<Arc<Progress>>> = const { RefCell::new(None) };
}

/// Install (or clear) the hook for the calling thread.
pub fn install(p: Option<Arc<Progress>>) {
    CURRENT.with(|c| *c.borrow_mut() = p);
}

/// The calling thread's hook, to hand to helper threads.
pub fn current() -> Option<Arc<Progress>> {
    CURRENT.with(|c| c.borrow().clone())
}

/// Report a byte position; true when the scan should stop.
pub fn at(pos: usize) -> bool {
    CURRENT.with(|c| match &*c.borrow() {
        Some(p) => {
            p.done.store(pos as u64, Ordering::Relaxed);
            p.cancelled()
        }
        None => false,
    })
}

pub fn cancelled() -> bool {
    CURRENT.with(|c| c.borrow().as_ref().is_some_and(|p| p.cancelled()))
}

/// Rate limiter: [`Ticker::step`] calls [`at`] about every 4 MiB of input.
#[derive(Default)]
pub struct Ticker(usize);

impl Ticker {
    pub fn new() -> Self {
        Self(0)
    }

    pub fn step(&mut self, pos: usize) -> bool {
        if pos < self.0 {
            return false;
        }
        self.0 = pos + (4 << 20);
        at(pos)
    }
}
