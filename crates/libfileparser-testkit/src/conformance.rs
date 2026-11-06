//! The conformance suite every reader passes (docs/design/libfileparser.md §9).
//!
//! It checks a reader only through the `Reader` trait and the hooks, so it says the
//! same thing however the reader works inside.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

use libfileparser_core::{
    Cancel, Content, DependencyStatus, Document, FileSource, InlineSpawn, ParseContext, ParseError, Reader, Role, open_guarded,
};

use crate::{CountingNotify, DenyFiles, MemFiles, QueueSpawn, RecordingFiles, summarize, tiny_limits};

/// A file to check a reader against, with the neighbouring files it refers to.
#[derive(Clone)]
pub struct Sample {
    name: String,
    bytes: Vec<u8>,
    neighbours: Vec<(String, Vec<u8>)>,
    fits_tiny_limits: bool,
}

impl Sample {
    pub fn new(name: impl Into<String>, bytes: impl Into<Vec<u8>>) -> Self {
        Self { name: name.into(), bytes: bytes.into(), neighbours: Vec::new(), fits_tiny_limits: false }
    }

    /// Reads a sample from disk; its neighbours must be added by name.
    pub fn from_file(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let path = path.as_ref();
        let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
        Ok(Self::new(name, std::fs::read(path)?))
    }

    /// A file the sample refers to, by the name it uses ("cube.bin", "textures/a.png").
    pub fn with_neighbour(mut self, reference: impl Into<String>, bytes: impl Into<Vec<u8>>) -> Self {
        self.neighbours.push((reference.into(), bytes.into()));
        self
    }

    /// Marks a sample small enough to open within [`tiny_limits`]. Others must fail
    /// with `TooLarge` under them, which is how the suite knows limits are checked.
    pub fn fits_tiny_limits(mut self) -> Self {
        self.fits_tiny_limits = true;
        self
    }

    fn source(&self) -> FileSource {
        FileSource::from_bytes(self.name.clone(), self.bytes.clone())
    }

    fn files(&self) -> MemFiles {
        MemFiles::new(self.neighbours.clone())
    }
}

/// Runs every check on every sample; returns what failed, empty when the reader conforms.
pub fn check(reader: &dyn Reader, samples: &[Sample]) -> Vec<String> {
    let mut failures = Vec::new();
    if samples.is_empty() {
        failures.push(format!("{}: no samples to check", reader.info().id));
    }
    detect_is_safe(reader, &mut failures);
    for sample in samples {
        let mut fail = |what: String| failures.push(format!("{} on {}: {what}", reader.info().id, sample.name));
        check_sample(reader, sample, &mut fail);
    }
    failures
}

/// [`check`], panicking with every failure.
#[track_caller]
pub fn assert_conforms(reader: &dyn Reader, samples: &[Sample]) {
    let failures = check(reader, samples);
    assert!(failures.is_empty(), "{} conformance failures:\n{}", failures.len(), failures.join("\n"));
}

/// 1. `detect` never panics, on anything.
fn detect_is_safe(reader: &dyn Reader, failures: &mut Vec<String>) {
    let garbage: [&[u8]; 6] = [b"", &[0; 64], &[0xff; 64], b"\x89PNG", b"glTF\x02\0\0\0", b"#usda 1.0\n(\n"];
    for bytes in garbage {
        for name in ["", "x", "file.bin", "a.gltf"] {
            if catch_unwind(AssertUnwindSafe(|| reader.detect(bytes, name))).is_err() {
                failures.push(format!("{}: detect panicked on {} bytes named {name:?}", reader.info().id, bytes.len()));
            }
        }
    }
}

fn check_sample(reader: &dyn Reader, sample: &Sample, fail: &mut dyn FnMut(String)) {
    let src = sample.source();

    // 1. It recognises its own sample, the same way twice.
    let d = reader.detect(src.header(), &sample.name);
    if !d.is_match() {
        fail("detect doesn't recognise its own sample".into());
    }
    if reader.detect(src.header(), &sample.name) != d {
        fail("detect gave two different answers".into());
    }

    // 2. Opens through the hooks only.
    let files = RecordingFiles::new(sample.files());
    let spawn = QueueSpawn::default();
    let notify = CountingNotify::default();
    let cx = ParseContext::new().with_files(files.clone()).with_spawn(spawn.clone()).with_notify(notify.clone());
    let doc = match open_guarded(reader, &src, &cx) {
        Ok(doc) => doc,
        Err(e) => return fail(format!("doesn't open: {e}")),
    };
    spawn.run_all();
    // A file searched for in several places (Dependencies::resolve_any) is recorded
    // once, under its own name: match by file name.
    let known: Vec<String> = doc.dependencies().list().into_iter().map(|d| file_name(&d.reference)).collect();
    for asked in files.asked() {
        if !known.contains(&file_name(&asked)) {
            fail(format!("read {asked:?} without recording it in Dependencies (use Dependencies::resolve or resolve_any)"));
        }
    }

    // 3. A primary facet first; every facet can be made, the same twice.
    match doc.facets().next() {
        None => fail("no facets".into()),
        Some(f) if f.role != Role::Primary => fail(format!("first facet is {:?}, not Primary", f.role)),
        _ => {}
    }
    for i in 0..doc.facets().len() {
        match (doc.facet(i, &cx), doc.facet(i, &cx)) {
            (Ok(a), Ok(b)) => {
                let (mut sa, mut sb) = (String::new(), String::new());
                crate::summary::content_summary(&a, &mut sa);
                crate::summary::content_summary(&b, &mut sb);
                if sa != sb {
                    fail(format!("facet {i} differs when asked twice"));
                }
            }
            (Err(e), _) | (_, Err(e)) => fail(format!("facet {i} can't be made: {e}")),
        }
    }
    if let Ok(Content::Scene(s)) = doc.facet(0, &cx)
        && s.textures_pending() > 0
    {
        fail(format!("{} textures still pending after all spawned work ran: work not run through cx.spawn", s.textures_pending()));
    }

    // 8. Reports its memory.
    if !sample.bytes.is_empty() && doc.memory().total() == 0 {
        fail("memory() reports nothing".into());
    }

    // 4. Refused neighbours are recorded as blocked.
    let asked = files.asked();
    if !asked.is_empty() {
        let files = RecordingFiles::new(DenyFiles);
        let cx = ParseContext::new().with_files(files.clone()).with_spawn(InlineSpawn);
        match open_guarded(reader, &src, &cx) {
            Ok(doc) => {
                let blocked: Vec<String> = doc.dependencies().with_status(DependencyStatus::Blocked).iter().map(|r| file_name(r)).collect();
                for a in files.asked() {
                    if !blocked.contains(&file_name(&a)) {
                        fail(format!("{a:?} was refused but isn't recorded as blocked"));
                    }
                }
                if doc.facet(0, &cx).is_err() {
                    fail("the primary facet fails when neighbours are refused".into());
                }
            }
            Err(ParseError::Unreadable { .. }) => {}
            Err(e) => fail(format!("with neighbours refused, failed with {e} instead of opening or reporting them")),
        }
    }

    // 5. Tiny limits stop it cleanly.
    let cx = ParseContext::new().with_files(sample.files()).with_limits(tiny_limits());
    match (open_guarded(reader, &src, &cx).and_then(|doc| make_all(&doc, &cx)), sample.fits_tiny_limits) {
        (Err(ParseError::TooLarge(_)), _) | (Ok(()), true) => {}
        (Ok(()), false) => fail("opened within tiny limits: are limits checked? (mark small samples with fits_tiny_limits)".into()),
        (Err(e), _) => fail(format!("with tiny limits, failed with {e} instead of TooLarge")),
    }

    // 6. Nothing is notified after cancelling.
    let spawn = QueueSpawn::default();
    let notify = CountingNotify::default();
    let cancel = Cancel::new();
    let cx = ParseContext::new().with_files(sample.files()).with_spawn(spawn.clone()).with_notify(notify.clone()).with_cancel(cancel.clone());
    if open_guarded(reader, &src, &cx).is_ok() {
        let before = notify.count();
        cancel.cancel();
        spawn.run_all();
        if notify.count() != before {
            fail(format!("notified {} more times after being cancelled", notify.count() - before));
        }
    }

    // 7. The same file gives the same document.
    let cx = ParseContext::new().with_files(sample.files()).with_spawn(InlineSpawn);
    match (open_guarded(reader, &src, &cx), open_guarded(reader, &src, &cx)) {
        (Ok(a), Ok(b)) => {
            let (sa, sb) = (summarize(&a, &cx), summarize(&b, &cx));
            if sa != sb {
                fail(format!("two opens differ:\n--- first\n{sa}--- second\n{sb}"));
            }
        }
        _ => fail("doesn't open the same way twice".into()),
    }
}

/// The last part of a reference, `/` or `\\` separated.
fn file_name(reference: &str) -> String {
    reference.rsplit(['/', '\\']).next().unwrap_or(reference).to_owned()
}

fn make_all(doc: &Document, cx: &ParseContext) -> libfileparser_core::Result<()> {
    (0..doc.facets().len()).try_for_each(|i| doc.facet(i, cx).map(drop))
}
