//! Startup tracing (`--trace-startup`). Times are measured from the start of `main`.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

static START: OnceLock<Instant> = OnceLock::new();
static TRACE: AtomicBool = AtomicBool::new(false);

pub fn init() {
    START.get_or_init(Instant::now);
}

pub fn set_tracing(on: bool) {
    TRACE.store(on, Ordering::Relaxed);
}

pub fn elapsed_ms() -> f64 {
    START.get().map_or(0.0, |s| s.elapsed().as_secs_f64() * 1000.0)
}

pub fn mark(label: &str) {
    if TRACE.load(Ordering::Relaxed) {
        eprintln!("[startup] {:>8.2} ms  {label}", elapsed_ms());
    }
}
