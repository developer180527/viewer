//! The conformance suite every reader passes (docs/design/libfileparser.md §9).
//!
//! It checks a reader only through the `Reader` trait and the hooks, so it says the
//! same thing however the reader works inside.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

use libfileparser_core::{
    Cancel, Content, DependencyStatus, Document, FileSource, InlineSpawn, Model, NodeId, ParseContext, ParseError, Place, Reader, Role, SceneItem,
    TreeSource, is_text, open_guarded,
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

    // 9. It ends with the file's own facets: Raw always, Source when it is text.
    let cx = ParseContext::new().with_files(sample.files()).with_spawn(InlineSpawn);
    if let Ok(doc) = open_guarded(reader, &src, &cx) {
        let roles: Vec<Role> = doc.facets().map(|f| f.role).collect();
        if roles.last() != Some(&Role::Raw) {
            fail("doesn't end with a Raw facet: end the document with Document::with_file_facets".into());
        }
        let text_primary = doc.facets().next().is_some_and(|f| f.model == Model::Text);
        if is_text(src.header()) && !text_primary && !roles.contains(&Role::Source) {
            fail("is text but has no Source facet: Document::with_file_facets adds it".into());
        }
        // 10. Trees answer the same way twice and find their own paths; 11. every link
        // leads somewhere that exists.
        check_trees_and_links(&doc, &cx, fail);
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

/// Nodes walked per tree, and links followed per document: enough to find a broken
/// reader, few enough to stay fast on a big sample.
const WALK: usize = 2000;

fn check_trees_and_links(doc: &Document, cx: &ParseContext, fail: &mut dyn FnMut(String)) {
    let mut places: Vec<Place> = Vec::new();
    for (i, info) in doc.facets().enumerate() {
        let Ok(content) = doc.facet(i, cx) else { continue };
        match content {
            Content::Tree(t) => walk_tree(i, &info.name, &*t, &mut places, fail),
            Content::Table(t) => {
                let rows = t.info().rows.known().min(50);
                places.extend((0..rows).map(|row| Place::Row { facet: i, row }));
            }
            Content::Scene(s) => {
                let read = s.scene().read();
                let n = |len: usize| (len as u32).min(50);
                places.extend((0..n(read.nodes().len())).map(|k| Place::Scene { facet: i, item: SceneItem::Node(k) }));
                places.extend((0..n(read.meshes().len())).map(|k| Place::Scene { facet: i, item: SceneItem::Mesh(k) }));
                places.extend((0..n(read.materials().len())).map(|k| Place::Scene { facet: i, item: SceneItem::Material(k) }));
            }
            _ => {}
        }
    }
    let source_lines = doc.facets().position(|f| f.role == Role::Source).and_then(|i| match doc.facet(i, cx) {
        Ok(Content::Text(t)) => Some(t.info().lines.known()),
        _ => None,
    });
    if let Some(lines) = source_lines {
        places.extend((0..lines.min(50)).map(|l| Place::Lines(l..l + 1)));
    }
    for from in places.iter().take(WALK) {
        match doc.links(from, cx) {
            Ok(to) => {
                for p in to {
                    if let Err(why) = place_exists(doc, cx, &p, source_lines) {
                        fail(format!("{from:?} links to {p:?}, which {why}"));
                    }
                }
            }
            Err(e) => fail(format!("links of {from:?} failed: {e}")),
        }
    }
}

/// Breadth first, up to [`WALK`] nodes: the same children twice, a count that matches
/// them, and every path found again.
fn walk_tree(facet: usize, name: &str, t: &dyn TreeSource, places: &mut Vec<Place>, fail: &mut dyn FnMut(String)) {
    let mut queue: std::collections::VecDeque<Option<NodeId>> = [None].into();
    let mut seen = 0usize;
    while let Some(parent) = queue.pop_front() {
        let count = match t.child_count(parent) {
            Ok(c) => c,
            Err(e) => return fail(format!("{name}: child_count({parent:?}) failed: {e}")),
        };
        let want = count.known().min(200);
        let (a, b) = (t.children(parent, 0..want), t.children(parent, 0..want));
        let (a, b) = match (a, b) {
            (Ok(a), Ok(b)) => (a, b),
            (Err(e), _) | (_, Err(e)) => return fail(format!("{name}: children({parent:?}) failed: {e}")),
        };
        if a != b {
            fail(format!("{name}: children({parent:?}) differ when asked twice"));
        }
        if count.is_exact() && a.len() as u64 != want {
            fail(format!("{name}: child_count({parent:?}) is {}, but {} children came", count.known(), a.len()));
        }
        for n in a {
            seen += 1;
            if seen > WALK {
                return;
            }
            match t.path(n.id) {
                Ok(Some(path)) => match t.find(&path) {
                    Ok(Some(found)) if found == n.id => {}
                    other => fail(format!("{name}: node {:?} has path {path:?}, but find gives {other:?}", n.label)),
                },
                Ok(None) => {}
                Err(e) => fail(format!("{name}: path of {:?} failed: {e}", n.label)),
            }
            places.push(Place::Node { facet, node: n.id });
            if n.has_children {
                queue.push_back(Some(n.id));
            }
        }
    }
}

/// Whether a place a link gives is there.
fn place_exists(doc: &Document, cx: &ParseContext, place: &Place, source_lines: Option<u64>) -> std::result::Result<(), String> {
    let content = |facet: usize| doc.facet(facet, cx).map_err(|e| format!("is in facet {facet}, which can't be made: {e}"));
    match place {
        Place::Node { facet, node } => match content(*facet)? {
            Content::Tree(t) => t.children(Some(*node), 0..1).map(drop).map_err(|e| format!("isn't a node of that tree: {e}")),
            _ => Err("isn't in a tree".into()),
        },
        Place::Row { facet, row } => match content(*facet)? {
            Content::Table(t) if *row < t.info().rows.known() => Ok(()),
            Content::Table(_) => Err("is past the table's last row".into()),
            _ => Err("isn't in a table".into()),
        },
        Place::Entry { facet, entry } => match content(*facet)? {
            Content::Container(c) if *entry < c.entry_count().known() => Ok(()),
            _ => Err("isn't an entry of a container".into()),
        },
        Place::Scene { facet, item } => match content(*facet)? {
            Content::Scene(s) => {
                let read = s.scene().read();
                let ok = match *item {
                    SceneItem::Node(i) => (i as usize) < read.nodes().len(),
                    SceneItem::Mesh(i) => (i as usize) < read.meshes().len(),
                    SceneItem::Material(i) => (i as usize) < read.materials().len(),
                    SceneItem::Texture(i) => (i as usize) < read.textures().len(),
                    _ => true,
                };
                if ok { Ok(()) } else { Err("isn't in the scene".into()) }
            }
            _ => Err("isn't in a scene".into()),
        },
        Place::Lines(r) => match source_lines {
            Some(n) if r.start < r.end && r.end <= n => Ok(()),
            Some(n) => Err(format!("isn't within the Source's {n} lines")),
            None => Err("has no Source facet to be in".into()),
        },
        Place::Bytes(r) => if r.start <= r.end { Ok(()) } else { Err("is an empty range backwards".into()) },
        _ => Ok(()),
    }
}

/// The last part of a reference, `/` or `\\` separated.
fn file_name(reference: &str) -> String {
    reference.rsplit(['/', '\\']).next().unwrap_or(reference).to_owned()
}

fn make_all(doc: &Document, cx: &ParseContext) -> libfileparser_core::Result<()> {
    (0..doc.facets().len()).try_for_each(|i| doc.facet(i, cx).map(drop))
}
