//! Fakes for every hook, so a test controls files, threads, progress and limits.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use libfileparser_core::{FileResolver, FileSource, Limits, Notify, Spawn, Unreadable};

/// Neighbouring files served from memory, by the name the document uses.
#[derive(Clone, Default)]
pub struct MemFiles(Arc<HashMap<String, Vec<u8>>>);

impl MemFiles {
    pub fn new(files: impl IntoIterator<Item = (String, Vec<u8>)>) -> Self {
        Self(Arc::new(files.into_iter().map(|(k, v)| (normalise(&k), v)).collect()))
    }
}

impl FileResolver for MemFiles {
    fn resolve(&self, _: &FileSource, reference: &str) -> Result<FileSource, Unreadable> {
        let name = normalise(reference);
        self.0.get(&name).map(|b| FileSource::from_bytes(name, b.clone())).ok_or(Unreadable::Missing)
    }
}

fn normalise(reference: &str) -> String {
    reference.replace('\\', "/").trim_start_matches("./").to_owned()
}

/// Refuses every neighbouring file, as iOS does when the user picked one file alone.
#[derive(Clone, Copy, Default)]
pub struct DenyFiles;

impl FileResolver for DenyFiles {
    fn resolve(&self, _: &FileSource, _: &str) -> Result<FileSource, Unreadable> {
        Err(Unreadable::Blocked)
    }
}

/// Wraps a resolver and records every reference asked for. Clones share the record.
#[derive(Clone)]
pub struct RecordingFiles<F> {
    inner: F,
    asked: Arc<Mutex<Vec<String>>>,
}

impl<F: FileResolver> RecordingFiles<F> {
    pub fn new(inner: F) -> Self {
        Self { inner, asked: Arc::default() }
    }

    /// Every reference asked for, in order, repeats included.
    pub fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }
}

impl<F: FileResolver> FileResolver for RecordingFiles<F> {
    fn resolve(&self, from: &FileSource, reference: &str) -> Result<FileSource, Unreadable> {
        self.asked.lock().unwrap().push(reference.to_owned());
        self.inner.resolve(from, reference)
    }
}

type Job = Box<dyn FnOnce() + Send + 'static>;

/// Holds background work until the test runs it, so it can cancel in between.
/// Clones share the queue.
#[derive(Clone, Default)]
pub struct QueueSpawn(Arc<Mutex<Vec<Job>>>);

impl QueueSpawn {
    /// Runs every queued job, and any they queue, until none are left.
    pub fn run_all(&self) {
        loop {
            let jobs = std::mem::take(&mut *self.0.lock().unwrap());
            if jobs.is_empty() {
                return;
            }
            for job in jobs {
                job();
            }
        }
    }

    pub fn queued(&self) -> usize {
        self.0.lock().unwrap().len()
    }
}

impl Spawn for QueueSpawn {
    fn spawn(&self, job: Job) {
        self.0.lock().unwrap().push(job);
    }
}

/// Counts notifications. Clones share the count.
#[derive(Clone, Default)]
pub struct CountingNotify(Arc<AtomicUsize>);

impl CountingNotify {
    pub fn count(&self) -> usize {
        self.0.load(Ordering::Acquire)
    }
}

impl Notify for CountingNotify {
    fn notify(&self) {
        self.0.fetch_add(1, Ordering::AcqRel);
    }
}

/// Limits that trip on almost any real file, to check a reader stops cleanly.
pub fn tiny_limits() -> Limits {
    let mut l = Limits::desktop();
    l.image_pixels = 64;
    l.allocation = 4096;
    l.unpack_ratio = 2;
    l.unpack_always = 4096;
    l.depth = 4;
    l.geometry = 16;
    l.nodes = 16;
    l.rows = 16;
    l.entries = 16;
    l.expansion = 16;
    l.memory = 64 << 10;
    l.dependencies = 2;
    l
}
