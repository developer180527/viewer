//! `-core`'s own pieces, through its public API only.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use libfileparser_core::*;

#[test]
fn line_text_splits_lines_and_drops_terminators() {
    let t = LineText::new(FileSource::from_bytes("a.txt", b"one\r\ntwo\n\nlast".to_vec()), Some("text"));
    assert_eq!(t.info().lines, Count::Exact(4));
    let lines = t.lines(0..4).unwrap();
    let lines: Vec<&[u8]> = lines.iter().map(|l| l.as_slice()).collect();
    assert_eq!(lines, [&b"one"[..], b"two", b"", b"last"]);
    assert_eq!(t.lines(2..99).unwrap().len(), 2, "fewer past the end, not an error");
    assert!(t.lines(9..12).unwrap().is_empty());
    assert_eq!(t.info().language.as_deref(), Some("text"));
    assert_eq!(LineText::new(FileSource::from_bytes("e", Vec::new()), None).info().lines, Count::Exact(0));
    assert_eq!(LineText::new(FileSource::from_bytes("n", b"a\n".to_vec()), None).info().lines, Count::Exact(1));
    // Lines outlive the source they came from: they share its bytes.
    let first = t.lines(0..1).unwrap().remove(0);
    drop(t);
    assert_eq!(first.as_slice(), b"one");
}

#[test]
fn limits_check_and_cap_unpacking() {
    let l = Limits::desktop();
    assert!(l.check(Limit::ImagePixels, 1 << 28).is_ok());
    assert!(matches!(l.check(Limit::ImagePixels, (1 << 28) + 1), Err(ParseError::TooLarge(e)) if e.limit == Limit::ImagePixels));
    assert_eq!(l.unpack_cap(1000), 64 << 20, "small inputs may always inflate to the floor");
    assert_eq!(l.unpack_cap(1 << 20), 200 << 20, "large ones by the ratio");
    assert_eq!(l.unpack_cap(u64::MAX), l.allocation, "never past one allocation");
    assert!(Limits::mobile().memory < l.memory);
}

#[test]
fn dependencies_record_every_read_and_respect_the_limit() {
    let src = FileSource::from_bytes("model.gltf", Vec::new());
    let mut limits = Limits::desktop();
    limits.dependencies = 1;
    let cx = ParseContext::new().with_files(NoFiles).with_limits(limits);
    let deps = Dependencies::new();
    assert!(deps.resolve(&cx, &src, "a.bin").is_none());
    assert!(deps.resolve(&cx, &src, "a.bin").is_none(), "asking again doesn't count twice");
    assert!(deps.resolve(&cx, &src, "b.bin").is_none());
    assert_eq!(deps.with_status(DependencyStatus::Missing), ["a.bin", "b.bin"]);
    deps.record("a.bin", DependencyStatus::Read);
    assert_eq!(deps.count(DependencyStatus::Read), 1, "a later status replaces the earlier one");
}

#[test]
fn folder_files_read_inside_the_folder_only() {
    let base = std::env::temp_dir().join(format!("lfp-core-{}", std::process::id()));
    let dir = base.join("model");
    std::fs::create_dir_all(dir.join("textures")).unwrap();
    std::fs::create_dir_all(base.join("textures")).unwrap();
    std::fs::write(dir.join("textures/a.png"), b"png").unwrap();
    std::fs::write(base.join("textures/up.png"), b"up").unwrap();
    std::fs::write(base.join("secret.txt"), b"secret").unwrap();
    std::fs::write(dir.join("m.gltf"), b"{}").unwrap();
    let src = FileSource::open(dir.join("m.gltf")).unwrap();

    let f = FolderFiles::new();
    assert_eq!(f.resolve(&src, "textures/a.png").unwrap().bytes(), b"png");
    assert_eq!(f.resolve(&src, "textures\\a.png").unwrap().bytes(), b"png", "Windows separators");
    assert_eq!(f.resolve(&src, "./textures/../textures/a.png").unwrap().bytes(), b"png", "`..` that stays inside");
    assert_eq!(f.resolve(&src, "nope.png").unwrap_err(), Unreadable::Missing);
    assert_eq!(f.resolve(&src, "textures").unwrap_err(), Unreadable::Missing, "a folder isn't a file");
    // Leaving the folder, by any spelling, isn't read.
    for out in ["../secret.txt", "textures/../../secret.txt", "../textures/up.png", "C:/Windows/win.ini", "file:///etc/passwd"] {
        assert_eq!(f.resolve(&src, out).unwrap_err(), Unreadable::Outside, "{out}");
    }
    let absolute = base.join("secret.txt").to_string_lossy().into_owned();
    assert_eq!(f.resolve(&src, &absolute).unwrap_err(), Unreadable::Outside, "an absolute path");
    // One level up, when allowed, and no further.
    let up = FolderFiles::new().allow_parent_levels(1);
    assert_eq!(up.resolve(&src, "../textures/up.png").unwrap().bytes(), b"up");
    assert_eq!(up.resolve(&src, "../../x.png").unwrap_err(), Unreadable::Outside);
    // A symlink inside the folder that points out of it.
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(base.join("secret.txt"), dir.join("textures/link.png")).unwrap();
        assert_eq!(f.resolve(&src, "textures/link.png").unwrap_err(), Unreadable::Outside, "a symlink out");
    }
    let in_memory = FileSource::from_bytes("m.gltf", Vec::new());
    assert_eq!(f.resolve(&in_memory, "textures/a.png").unwrap_err(), Unreadable::Missing, "no folder, no neighbours");

    // AnyFiles reads what it's told; the default reads nothing.
    assert_eq!(AnyFiles.resolve(&src, &absolute).unwrap().bytes(), b"secret");
    assert_eq!(ParseContext::new().files.resolve(&src, "textures/a.png").unwrap_err(), Unreadable::Missing);
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn sources_hand_out_ranges_and_reject_unknown_ids() {
    let mut tree = OwnedTree::new();
    let root = tree.add(None, "root", "");
    for i in 0..10 {
        tree.add(Some(root), format!("child {i}"), i.to_string());
    }
    assert_eq!(tree.child_count(Some(root)).unwrap(), Count::Exact(10));
    let page = tree.children(Some(root), 3..5).unwrap();
    assert_eq!(page.iter().map(|n| n.label.as_str()).collect::<Vec<_>>(), ["child 3", "child 4"]);
    assert!(!page[0].has_children);
    assert_eq!(tree.children(Some(root), 8..100).unwrap().len(), 2);
    assert!(matches!(tree.children(Some(NodeId(999)), 0..1), Err(ParseError::InvalidRequest(_))));

    let table = OwnedTable::new(vec!["a".into()], (0..5).map(|i| vec![i.to_string()]).collect());
    assert_eq!(table.info().rows, Count::Exact(5));
    assert_eq!(table.rows(3..10).unwrap(), [vec!["3".to_string()], vec!["4".to_string()]]);
}


#[test]
fn a_lazy_facet_is_made_once_and_retried_after_failing() {
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let doc = Document::new(FormatInfo::new("t", "Test", Category::Data))
        .with_facet(Role::Primary, "Raw", Content::Hex(FileSource::from_bytes("x", vec![1, 2, 3])))
        .with_lazy_facet(FacetInfo::new(Role::Inspector, "Rows", Model::Table), move |_| {
            if c.fetch_add(1, Ordering::SeqCst) == 0 {
                Err(ParseError::corrupt("first try fails"))
            } else {
                Ok(Content::Table(Arc::new(OwnedTable::new(vec!["a".into()], vec![vec!["1".into()]]))))
            }
        });
    let cx = ParseContext::new();
    assert!(!doc.is_made(1));
    assert!(doc.facet(1, &cx).is_err(), "a failing facet fails alone");
    assert!(doc.facet(0, &cx).is_ok());
    assert!(doc.facet(1, &cx).is_ok(), "and is tried again");
    assert!(doc.facet(1, &cx).is_ok());
    assert_eq!(calls.load(Ordering::SeqCst), 2, "made once it succeeded");
    assert_eq!(doc.memory().mapped, 3);
    cx.cancel.cancel();
}

#[test]
fn routing_tries_the_next_reader_on_damage_and_stops_on_other_errors() {
    struct Fails(&'static str, fn() -> ParseError);
    static A: ReaderInfo = ReaderInfo::new("a", "A", Category::Data);
    static B: ReaderInfo = ReaderInfo::new("b", "B", Category::Data);
    impl Reader for Fails {
        fn info(&self) -> &ReaderInfo {
            if self.0 == "a" { &A } else { &B }
        }
        fn detect(&self, _: &[u8], _: &str) -> Detection {
            Detection::new(if self.0 == "a" { Confidence::Magic } else { Confidence::Extension })
        }
        fn open(&self, _: &FileSource, _: &ParseContext) -> Result<Document> {
            Err((self.1)())
        }
    }
    let src = FileSource::from_bytes("f", vec![0]);
    let cx = ParseContext::new();
    let damaged = Fails("a", || ParseError::corrupt("bad"));
    let too_large = Fails("a", || ParseError::TooLarge(match Limits::desktop().check(Limit::Items, u64::MAX) {
        Err(ParseError::TooLarge(e)) => e,
        _ => unreachable!(),
    }));
    let second = Fails("b", || ParseError::Cancelled);
    assert!(matches!(open(&[&damaged, &second], &src, &cx), Err(ParseError::Cancelled)), "damage moves on to b");
    assert!(matches!(open(&[&too_large, &second], &src, &cx), Err(ParseError::TooLarge(_))), "too large is final");
    let cancelled = ParseContext::new();
    cancelled.cancel.cancel();
    assert!(matches!(open(&[&damaged], &src, &cancelled), Err(ParseError::Cancelled)));
}

#[test]
fn panics_in_format_code_stay_inside() {
    let cx = ParseContext::new();
    // A facet whose maker panics fails alone; the document and its other facets live on.
    let doc = Document::new(FormatInfo::new("t", "Test", Category::Data))
        .with_facet(Role::Primary, "Fine", Content::Table(Arc::new(OwnedTable::new(vec!["a".into()], Vec::new()))))
        .with_lazy_facet(FacetInfo::new(Role::Inspector, "Broken", Model::Tree), |_| panic!("bad node"));
    assert!(matches!(doc.facet(1, &cx), Err(ParseError::Corrupt(why)) if why.contains("bad node")));
    assert!(doc.facet(0, &cx).is_ok());
    assert!(matches!(doc.facet(7, &cx), Err(ParseError::InvalidRequest(_))), "a facet that doesn't exist is the caller's mistake");
    assert!(!ParseError::invalid("x").try_next_reader() && !ParseError::unsupported("x").try_next_reader());

    // A background job that panics doesn't take the caller's thread with it, and work
    // after a caught panic still runs.
    let done = Arc::new(AtomicUsize::new(0));
    let d = done.clone();
    cx.spawn_job(move || {
        let decoded: Option<u32> = ParseContext::catching(|| panic!("decoder"));
        assert!(decoded.is_none());
        d.fetch_add(1, Ordering::SeqCst);
    });
    cx.spawn_job(|| panic!("a job that fails outright"));
    assert_eq!(done.load(Ordering::SeqCst), 1);
}
