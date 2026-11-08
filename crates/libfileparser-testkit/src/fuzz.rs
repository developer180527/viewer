//! Mutation fuzzing for readers and format libraries (docs/design/libfileparser.md §9):
//! samples damaged thousands of ways (cut short, bits flipped, bytes inserted and
//! deleted, regions repeated, samples spliced, sizes made huge) and each result
//! opened, every facet made and read, links followed. Nothing may panic or hang;
//! errors are fine. Deterministic: the same seed gives the same inputs, so a failure
//! reproduces, and each failing input is saved to `target/fuzz-failures/`.
//!
//! How hard: `LFP_FUZZ_ITERATIONS` mutations per sample (default 64, enough for CI),
//! `LFP_FUZZ_SEED` to explore other inputs. For depth, the coverage-guided targets in
//! `fuzz/` (cargo-fuzz) run the same checks for hours.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use libfileparser_core::{Content, Document, FileSource, Limits, NodeId, ParseContext, Place, Reader};

use crate::Sample;

/// A deterministic random source (xorshift64*): no dependency, same numbers everywhere.
#[derive(Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A number below `n` (0 when `n` is 0).
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { (self.next_u64() % n as u64) as usize }
    }

    pub fn chance(&mut self, one_in: usize) -> bool {
        self.below(one_in) == 0
    }
}

/// Bytes that matter to parsers: delimiters, quotes, brackets, digits, line ends, NUL.
const INTERESTING: &[u8] = b"\0\xff\x7f\x80\n\r\t \"'{}[]()<>:;,=/\\#@!-+.0159eE";

/// Integers that break size arithmetic, written at random places.
const BIG: [u64; 8] = [0, 1, 0x7f, 0xff, 0x7fff_ffff, 0x8000_0000, 0xffff_ffff, u64::MAX];

/// One way an input was damaged, for the report.
fn mutate(rng: &mut Rng, input: &mut Vec<u8>, others: &[Vec<u8>]) -> &'static str {
    let len = input.len();
    match rng.below(11) {
        0 => {
            input.truncate(rng.below(len + 1));
            "cut short"
        }
        1 => {
            for _ in 0..1 + rng.below(8) {
                if !input.is_empty() {
                    let i = rng.below(input.len());
                    input[i] ^= 1 << rng.below(8);
                }
            }
            "bits flipped"
        }
        2 => {
            for _ in 0..1 + rng.below(4) {
                if !input.is_empty() {
                    let i = rng.below(input.len());
                    input[i] = INTERESTING[rng.below(INTERESTING.len())];
                }
            }
            "bytes replaced"
        }
        3 => {
            let at = rng.below(len + 1);
            let n = 1 + rng.below(16);
            let bytes: Vec<u8> = (0..n).map(|_| if rng.chance(2) { INTERESTING[rng.below(INTERESTING.len())] } else { rng.next_u64() as u8 }).collect();
            input.splice(at..at, bytes);
            "bytes inserted"
        }
        4 => {
            if len > 0 {
                let at = rng.below(len);
                let n = 1 + rng.below((len - at).min(64));
                input.drain(at..at + n);
            }
            "bytes deleted"
        }
        5 => {
            // A region repeated many times: deep nesting, long runs, many items.
            if len > 0 {
                let at = rng.below(len);
                let n = 1 + rng.below((len - at).min(32));
                let piece = input[at..at + n].to_vec();
                let most = if rng.chance(8) { 5000 } else { 64 };
                let times = 2 + rng.below(most);
                let repeated: Vec<u8> = piece.iter().copied().cycle().take(n * times).collect();
                input.splice(at..at, repeated);
            }
            "region repeated"
        }
        6 => {
            // A region copied elsewhere.
            if len > 1 {
                let at = rng.below(len);
                let n = 1 + rng.below((len - at).min(256));
                let piece = input[at..at + n].to_vec();
                let to = rng.below(len);
                input.splice(to..to, piece);
            }
            "region copied"
        }
        7 => {
            // Spliced with another sample.
            if let Some(other) = others.get(rng.below(others.len())).filter(|o| !o.is_empty()) {
                let cut = rng.below(len + 1);
                let from = rng.below(other.len());
                input.truncate(cut);
                input.extend_from_slice(&other[from..]);
            }
            "spliced"
        }
        8 => {
            // A huge or odd number where a size might be.
            if len >= 8 {
                let at = rng.below(len - 7);
                let v = BIG[rng.below(BIG.len())];
                let bytes = if rng.chance(2) { v.to_le_bytes() } else { v.to_be_bytes() };
                let width = [1, 2, 4, 8][rng.below(4)];
                input[at..at + width].copy_from_slice(&bytes[8 - width..]);
            }
            "number written"
        }
        9 => {
            // A decimal number made huge, for text formats' counts and sizes.
            if let Some(at) = (0..len).map(|_| rng.below(len)).find(|&i| input[i].is_ascii_digit()) {
                input.splice(at..at, b"99999999999".iter().copied());
            }
            "digits grown"
        }
        _ => {
            if len > 1 {
                let a = rng.below(len);
                let b = rng.below(len);
                input.swap(a, b);
            }
            "bytes swapped"
        }
    }
}

/// A fuzz input that broke something.
#[derive(Debug)]
pub struct Failure {
    pub target: String,
    pub sample: String,
    pub what: String,
    pub how: String,
    /// Where the input was saved, to replay it.
    pub saved: Option<PathBuf>,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} on {} ({}): {}", self.target, self.sample, self.how, self.what)?;
        if let Some(p) = &self.saved {
            write!(f, " — saved to {}", p.display())?;
        }
        Ok(())
    }
}

/// How a fuzz run goes.
#[derive(Clone)]
pub struct Fuzz {
    pub iterations: usize,
    pub seed: u64,
    /// One input taking longer than this is a hang.
    pub time_limit: Duration,
    pub limits: Limits,
}

impl Default for Fuzz {
    fn default() -> Self {
        Self::new()
    }
}

impl Fuzz {
    /// Iterations and seed from `LFP_FUZZ_ITERATIONS` and `LFP_FUZZ_SEED`; desktop
    /// limits with allocations and documents held to sizes a test machine has.
    pub fn new() -> Self {
        let env = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<u64>().ok());
        let mut limits = Limits::desktop();
        limits.allocation = 256 << 20;
        limits.memory = 512 << 20;
        limits.image_pixels = 1 << 24;
        limits.unpack_always = 16 << 20;
        Self { iterations: env("LFP_FUZZ_ITERATIONS").unwrap_or(64) as usize, seed: env("LFP_FUZZ_SEED").unwrap_or(0x5eed_f00d), time_limit: Duration::from_secs(10), limits }
    }

    pub fn iterations(mut self, n: usize) -> Self {
        self.iterations = n;
        self
    }

    /// Mutated inputs from `seeds`, each with how it was made: the seeds themselves,
    /// empty and random inputs, then `iterations` mutations of each seed.
    pub fn inputs(&self, salt: &str, seeds: &[(String, Vec<u8>)]) -> Vec<(String, String, Vec<u8>)> {
        let mut rng = Rng::new(self.seed ^ salt.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3)));
        let all: Vec<Vec<u8>> = seeds.iter().map(|s| s.1.clone()).collect();
        let mut out: Vec<(String, String, Vec<u8>)> = Vec::new();
        let first = seeds.first().map_or_else(|| "input".to_string(), |s| s.0.clone());
        out.push((first.clone(), "empty".into(), Vec::new()));
        let noise: Vec<u8> = (0..1 + rng.below(4096)).map(|_| rng.next_u64() as u8).collect();
        out.push((first, "random bytes".into(), noise));
        for (name, bytes) in seeds {
            out.push((name.clone(), "as given".into(), bytes.clone()));
            for _ in 0..self.iterations {
                let mut input = bytes.clone();
                let mut how = Vec::new();
                for _ in 0..1 + rng.below(3) {
                    how.push(mutate(&mut rng, &mut input, &all));
                }
                // Keep inputs to a size a test run can afford.
                input.truncate(4 << 20);
                out.push((name.clone(), how.join(", "), input));
            }
        }
        out
    }

    /// Runs `check` on every input from `seeds` on a thread of its own, with a time
    /// limit; what panics or hangs is a failure.
    pub fn run(&self, target: &str, seeds: &[(String, Vec<u8>)], check: impl Fn(&str, &[u8]) + Send + Sync + 'static) -> Vec<Failure> {
        install_hook();
        let check = Arc::new(check);
        let mut failures = Vec::new();
        let inputs = self.inputs(target, seeds);
        eprintln!("fuzzing {target}: {} inputs from {} samples", inputs.len(), seeds.len());
        for (n, (sample, how, input)) in inputs.into_iter().enumerate() {
            let (tx, rx) = mpsc::channel();
            let (c, name, bytes) = (check.clone(), sample.clone(), input.clone());
            let worker = std::thread::Builder::new().name(format!("fuzz {target}")).stack_size(8 << 20).spawn(move || {
                let result = catch_unwind(AssertUnwindSafe(|| c(&name, &bytes)));
                let _ = tx.send(result.err().map(|p| {
                    let place = WHERE.with(|w| w.borrow_mut().take()).unwrap_or_default();
                    format!("{} (at {place})", panic_text(&*p))
                }));
            });
            if worker.is_err() {
                continue;
            }
            let what = match rx.recv_timeout(self.time_limit) {
                Ok(None) => continue,
                Ok(Some(panic)) => format!("panicked: {panic}"),
                Err(mpsc::RecvTimeoutError::Timeout) => format!("took over {:?} (a hang)", self.time_limit),
                Err(mpsc::RecvTimeoutError::Disconnected) => "the check died".into(),
            };
            let saved = save(target, &sample, n, &input);
            failures.push(Failure { target: target.into(), sample, what, how, saved });
            // Enough to fix; the rest are likely the same bug.
            if failures.len() >= 20 {
                break;
            }
        }
        failures
    }

    /// Fuzzes `reader` on `samples`: detects, opens, makes and reads every facet,
    /// follows links. A facet's maker that panicked (caught by the document, reported
    /// as damage) is a failure too.
    pub fn reader(&self, reader: &'static dyn Reader, samples: &[Sample]) -> Vec<Failure> {
        let seeds: Vec<(String, Vec<u8>)> = samples.iter().map(|s| (s.name().to_string(), s.bytes().to_vec())).collect();
        // A reader fuzzed on nothing would pass on nothing.
        if seeds.is_empty() {
            return vec![Failure { target: reader.info().id.into(), sample: String::new(), what: "no samples to fuzz from".into(), how: String::new(), saved: None }];
        }
        let limits = self.limits.clone();
        self.run(reader.info().id, &seeds, move |name, bytes| {
            let src = FileSource::from_bytes(name, bytes.to_vec());
            let _ = reader.detect(src.header(), name);
            let cx = ParseContext::new().with_limits(limits.clone());
            if let Ok(doc) = reader.open(&src, &cx) {
                exercise(&doc, &cx);
            }
        })
    }
}

thread_local! {
    /// Where the last panic on this thread happened, for the report.
    static WHERE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Records each panic's place for the thread it happened on, and keeps the default
/// report for panics outside fuzzing threads.
fn install_hook() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let default = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let fuzzing = std::thread::current().name().is_some_and(|n| n.starts_with("fuzz "));
            if fuzzing {
                let place = info.location().map(|l| format!("{}:{}", l.file(), l.line()));
                WHERE.with(|w| *w.borrow_mut() = place);
            } else {
                default(info);
            }
        }));
    });
}

fn panic_text(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_else(|| "unknown panic".into())
}

/// Saves a failing input under `target/fuzz-failures/<target>/`.
fn save(target: &str, sample: &str, n: usize, input: &[u8]) -> Option<PathBuf> {
    let dir = std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target")).join("fuzz-failures").join(target);
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("{n:05}-{}", sample.replace(['/', '\\'], "_")));
    std::fs::write(&path, input).ok()?;
    Some(path)
}

/// Makes and reads every facet of `doc` as a viewer would, and follows links from a
/// few places. Panics (the fuzzer's failure) when a facet's maker panicked.
pub fn exercise(doc: &Document, cx: &ParseContext) {
    let _ = doc.memory();
    for (i, info) in doc.facets().enumerate() {
        let content = match doc.facet(i, cx) {
            Ok(c) => c,
            Err(e) => {
                let e = e.to_string();
                // `Document::facet` turns a maker's panic into damage; here it is a bug.
                assert!(!e.contains(" panicked: "), "facet {} ({}) panicked: {e}", i, info.name);
                continue;
            }
        };
        read(&content);
        let _ = doc.links(&Place::Lines(0..1), cx);
        let _ = doc.links(&Place::Row { facet: i, row: 0 }, cx);
        let _ = doc.links(&Place::Node { facet: i, node: NodeId(0) }, cx);
    }
}

/// Reads a content as a view would: the start and end of a text or table, a few
/// thousand nodes of a tree, the entries of a container, the first bands of an image.
pub fn read(content: &Content) {
    match content {
        Content::Text(t) => {
            let n = t.info().lines.known();
            let _ = t.lines(0..50);
            let _ = t.lines(n.saturating_sub(50)..n);
        }
        Content::Table(t) => {
            let info = t.info();
            let n = info.rows.known();
            for rows in [t.rows(0..50), t.rows(n.saturating_sub(50)..n)].into_iter().flatten() {
                for row in rows {
                    let _ = row.iter().map(ToString::to_string).count();
                }
            }
        }
        Content::Tree(t) => {
            let mut queue = vec![None];
            let mut seen = 0;
            while let Some(parent) = queue.pop() {
                let Ok(count) = t.child_count(parent) else { continue };
                let Ok(kids) = t.children(parent, 0..count.known().min(200)) else { continue };
                for k in kids {
                    seen += 1;
                    let _ = t.value(k.id);
                    if let Ok(Some(path)) = t.path(k.id) {
                        let _ = t.find(&path);
                    }
                    if k.has_children && seen < 2000 {
                        queue.push(Some(k.id));
                    }
                }
                if seen >= 2000 {
                    break;
                }
            }
            let _ = t.find("/");
            let _ = t.find("/a/b[2]");
        }
        Content::Container(c) => {
            let n = c.entry_count().known();
            if let Ok(entries) = c.entries(0..n.min(100)) {
                for i in 0..entries.len().min(5) {
                    let _ = c.open(i as u64);
                }
            }
        }
        Content::Raster(r) => {
            let _ = (r.width(), r.height());
            if let libfileparser_core::Pixels::Bands(source) = r.pixels()
                && let Ok(bands) = source.bands()
            {
                for band in bands.take(4) {
                    let _ = band;
                }
            }
        }
        Content::Scene(s) => {
            let _ = s.scene().read().cpu_bytes();
            let _ = s.textures_pending();
        }
        Content::Hex(src) => {
            let mut buf = [0u8; 64];
            let _ = src.read_at(0, &mut buf);
            let _ = src.read_at(src.len().saturating_sub(64), &mut buf);
        }
        _ => {}
    }
}

/// Every file under `dir` (the corpus) that `reader` recognises, as samples.
pub fn corpus_samples(dir: &std::path::Path, reader: &dyn Reader) -> Vec<Sample> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let Ok(bytes) = std::fs::read(&path) else { continue };
            if bytes.len() <= 1 << 20 && reader.detect(&bytes[..bytes.len().min(8192)], &name).is_match() {
                out.push(Sample::new(name, bytes));
            }
        }
    }
    out
}

/// Fuzzes every reader in `readers` on the samples `samples` gives it, and fails with
/// every input that panicked or hung.
#[track_caller]
pub fn assert_fuzz_clean(readers: &[&'static dyn Reader], samples: impl Fn(&str) -> Vec<Sample>) {
    let fuzz = Fuzz::new();
    let mut failures = Vec::new();
    for &r in readers {
        failures.extend(fuzz.reader(r, &samples(r.info().id)));
    }
    assert!(failures.is_empty(), "{} fuzz failures:\n{}", failures.len(), failures.iter().map(|f| format!("  {f}")).collect::<Vec<_>>().join("\n"));
}

/// Fuzzes a format library's entry point: `check` gets each input, and may return
/// errors but not panic or hang.
#[track_caller]
pub fn assert_bytes_fuzz_clean(target: &str, seeds: &[(String, Vec<u8>)], check: impl Fn(&str, &[u8]) + Send + Sync + 'static) {
    let failures = Fuzz::new().run(target, seeds, check);
    assert!(failures.is_empty(), "{} fuzz failures:\n{}", failures.len(), failures.iter().map(|f| format!("  {f}")).collect::<Vec<_>>().join("\n"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inputs_are_deterministic_and_varied() {
        let fuzz = Fuzz::new().iterations(50);
        let seeds = vec![("a.json".to_string(), br#"{"a": [1, 2, {"b": "c"}]}"#.to_vec())];
        let a = fuzz.inputs("t", &seeds);
        let b = fuzz.inputs("t", &seeds);
        assert_eq!(a.iter().map(|i| &i.2).collect::<Vec<_>>(), b.iter().map(|i| &i.2).collect::<Vec<_>>());
        let distinct: std::collections::HashSet<&Vec<u8>> = a.iter().map(|i| &i.2).collect();
        assert!(distinct.len() > 40, "{}", distinct.len());
    }

    #[test]
    fn panics_and_hangs_are_caught() {
        let mut fuzz = Fuzz::new().iterations(3);
        fuzz.time_limit = Duration::from_millis(200);
        let seeds = vec![("x".to_string(), b"abc".to_vec())];
        let failures = fuzz.run("selftest", &seeds, |_, bytes| {
            if bytes.is_empty() {
                panic!("empty!");
            }
            if bytes == b"abc" {
                std::thread::sleep(Duration::from_secs(2));
            }
        });
        assert!(failures.iter().any(|f| f.what.contains("empty!")));
        assert!(failures.iter().any(|f| f.what.contains("hang")));
    }
}
