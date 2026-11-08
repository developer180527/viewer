//! Opening grows in step with the file, never with its square: each reader here opens
//! a file at N and at 4N items, and the larger may take at most ~8 times as long. The
//! fuzzer uses small files, so it can't see a quadratic loop; a node or table looked up
//! by scanning everything before it once made a million-edge DOT file take hours.

use std::time::{Duration, Instant};

use libfileparser_core::{FileSource, ParseContext};

/// The best of three opens, so one slow run on a busy machine doesn't fail the test.
fn open_time(name: &str, bytes: &[u8]) -> Duration {
    (0..3)
        .map(|_| {
            let src = FileSource::from_bytes(name, bytes.to_vec());
            let t = Instant::now();
            let doc = libfileparser_data::open(&src, &ParseContext::new()).unwrap_or_else(|e| panic!("{name}: {e}"));
            doc.facet(0, &ParseContext::new()).unwrap();
            t.elapsed()
        })
        .min()
        .unwrap()
}

/// Fails when 4× the items take more than 8× the time (linear is 4×; quadratic 16×).
fn assert_linear(name: &str, make: impl Fn(usize) -> String, n: usize) {
    let small = open_time(name, make(n).as_bytes());
    let large = open_time(name, make(4 * n).as_bytes());
    // Below a few milliseconds, timer noise dominates the ratio.
    let ratio = large.as_secs_f64() / small.as_secs_f64().max(0.002);
    assert!(ratio < 8.0, "{name}: {n} items took {small:?}, {} took {large:?} ({ratio:.1}×): grows faster than the file", 4 * n);
}

#[test]
#[cfg(feature = "graphs")]
fn dot_nodes_and_edges() {
    assert_linear("g.dot", |n| format!("digraph g {{\n{}}}\n", (0..n).map(|i| format!("n{i} -> n{};\n", (i * 7919) % n)).collect::<String>()), 20_000);
    // Subgraphs listing many nodes.
    assert_linear("s.dot", |n| format!("graph g {{\na -- {{ {} }}\n}}\n", (0..n).map(|i| format!("n{i} ")).collect::<String>()), 20_000);
}

#[test]
#[cfg(feature = "sql")]
fn sql_with_many_tables() {
    assert_linear("d.sql", |n| (0..n).map(|i| format!("CREATE TABLE t{i} (id int, name text);\nINSERT INTO t{i} VALUES (1, 'a');\n")).collect(), 10_000);
}

#[test]
fn csv_and_json() {
    assert_linear("t.csv", |n| format!("a,b\n{}", (0..n).map(|i| format!("{i},x{i}\n")).collect::<String>()), 50_000);
    assert_linear("t.json", |n| format!("[{}]", (0..n).map(|i| format!("{{\"k\":{i}}}")).collect::<Vec<_>>().join(",")), 20_000);
}
