//! The conformance suite, checked on a small made-up format ("kv": `key = value`
//! lines, `include other.kv` for another file). A correct reader passes; each
//! deliberately broken version fails on exactly what it breaks. The correct one also
//! shows how little a reader is: detect, open, facets, through the hooks.

use std::sync::Arc;

use libfileparser_core::{
    Category, Confidence, Content, Dependencies, Detection, Document, FacetInfo, FileSource, FormatInfo, Limit, Model, OwnedTable,
    OwnedTree, ParseContext, Reader, ReaderInfo, Result, Role, open,
};
use libfileparser_testkit::{Sample, check};

#[derive(Clone, Copy, PartialEq)]
enum Flaw {
    None,
    /// Reads a neighbour straight through the resolver, so it isn't in Dependencies.
    UnrecordedRead,
    /// Its background work notifies even after the document was closed.
    IgnoresCancel,
    /// Never checks the limits.
    IgnoresLimits,
    /// Puts the Inspector before the Primary facet.
    InspectorFirst,
    /// Panics on short input.
    PanicsInDetect,
}

struct KvReader(Flaw);

static INFO: ReaderInfo = ReaderInfo::new("kv", "Key-value", Category::Data).extensions(&["kv"]);

impl Reader for KvReader {
    fn info(&self) -> &ReaderInfo {
        &INFO
    }

    fn detect(&self, header: &[u8], name: &str) -> Detection {
        if self.0 == Flaw::PanicsInDetect {
            let _ = header[3];
        }
        if header.starts_with(b"#kv\n") {
            Detection::new(Confidence::Magic).with_language("kv")
        } else if INFO.matches_extension(name) {
            Detection::new(Confidence::Extension).with_language("kv")
        } else {
            Detection::NONE
        }
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        let deps = Dependencies::new();
        let mut pairs = Vec::new();
        read_pairs(self.0, src, cx, &deps, &mut pairs, 0)?;

        let mut tree = OwnedTree::new();
        for (k, v) in &pairs {
            tree.add(None, k.clone(), v.clone());
        }
        // Some background work, to exercise spawn, notify and cancel.
        let (notify, cancel, flaw) = (cx.notify.clone(), cx.cancel.clone(), self.0);
        cx.spawn.spawn(Box::new(move || {
            if flaw == Flaw::IgnoresCancel || !cancel.is_cancelled() {
                notify.notify();
            }
        }));

        let rows: Vec<Vec<String>> = pairs.iter().map(|(k, v)| vec![k.clone(), v.clone()]).collect();
        let inspector = FacetInfo::new(Role::Inspector, "Pairs", Model::Table);
        let make_table = move |_: &ParseContext| Ok(Content::Table(Arc::new(OwnedTable::text(vec!["Key".into(), "Value".into()], rows.clone()))));
        let doc = Document::new(FormatInfo::new("kv", "Key-value", Category::Data)).with_dependencies(deps);
        let doc = if self.0 == Flaw::InspectorFirst {
            doc.with_lazy_facet(inspector, make_table).with_facet(Role::Primary, "Keys", Content::Tree(Arc::new(tree)))
        } else {
            doc.with_facet(Role::Primary, "Keys", Content::Tree(Arc::new(tree))).with_lazy_facet(inspector, make_table)
        };
        Ok(doc.with_meta("Pairs", pairs.len()).with_file_facets(src, Some("kv")))
    }
}

fn read_pairs(flaw: Flaw, src: &FileSource, cx: &ParseContext, deps: &Dependencies, out: &mut Vec<(String, String)>, depth: u64) -> Result<()> {
    if flaw != Flaw::IgnoresLimits {
        cx.limits.check(Limit::Depth, depth)?;
    }
    let text = std::str::from_utf8(src.bytes()).map_err(libfileparser_core::ParseError::corrupt)?;
    for line in text.lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()) {
        if flaw != Flaw::IgnoresLimits {
            cx.limits.check(Limit::Rows, out.len() as u64 + 1)?;
        }
        if let Some(name) = line.strip_prefix("include ") {
            let found = if flaw == Flaw::UnrecordedRead { cx.files.resolve(src, name).ok() } else { deps.resolve(cx, src, name) };
            if let Some(file) = found {
                read_pairs(flaw, &file, cx, deps, out, depth + 1)?;
            }
        } else {
            let (k, v) = line.split_once('=').ok_or_else(|| libfileparser_core::ParseError::corrupt(format!("no '=' in {line:?}")))?;
            out.push((k.trim().into(), v.trim().into()));
        }
    }
    Ok(())
}

fn samples() -> Vec<Sample> {
    let many: String = (0..40).map(|i| format!("key{i} = {i}\n")).collect();
    vec![
        Sample::new("cube.kv", format!("#kv\nname = cube\ninclude colours.kv\n{many}")).with_neighbour("colours.kv", "#kv\ncolour = red\n"),
        Sample::new("small.kv", "#kv\nsize = 1\n").fits_tiny_limits(),
    ]
}

#[test]
fn a_correct_reader_conforms() {
    let failures = check(&KvReader(Flaw::None), &samples());
    assert!(failures.is_empty(), "{failures:#?}");
}

/// Each flaw is caught, and by the check meant for it.
#[test]
fn each_flaw_is_caught() {
    let cases = [
        (Flaw::UnrecordedRead, "without recording it in Dependencies"),
        (Flaw::IgnoresCancel, "after being cancelled"),
        (Flaw::IgnoresLimits, "opened within tiny limits"),
        (Flaw::InspectorFirst, "not Primary"),
        (Flaw::PanicsInDetect, "detect panicked"),
    ];
    for (flaw, expected) in cases {
        let failures = check(&KvReader(flaw), &samples());
        assert!(failures.iter().any(|f| f.contains(expected)), "flaw not caught (wanted {expected:?}): {failures:#?}");
    }
}

/// A category library is a list of readers plus core's detect and open over it.
#[test]
fn readers_route_through_core() {
    let readers: [&dyn Reader; 1] = [&KvReader(Flaw::None)];
    let src = FileSource::from_bytes("x.kv", b"#kv\na = 1\n".to_vec());
    let doc = open(&readers, &src, &ParseContext::new()).unwrap();
    assert_eq!(doc.format().reader, "kv");
    assert_eq!(doc.facets().map(|f| f.role).collect::<Vec<_>>(), [Role::Primary, Role::Inspector, Role::Source, Role::Raw]);
    assert!(!doc.is_made(1), "the inspector is made on demand");
}
