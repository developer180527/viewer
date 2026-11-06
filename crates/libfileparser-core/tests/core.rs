//! `-core`'s own pieces, through its public API only.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use libfileparser_core::*;

#[test]
fn line_text_splits_lines_and_drops_terminators() {
    let t = LineText::new(FileSource::from_bytes("a.txt", b"one\r\ntwo\n\nlast".to_vec()), Some("text"));
    assert_eq!(t.line_count(), 4);
    assert_eq!((t.line(0), t.line(1), t.line(2), t.line(3)), (&b"one"[..], &b"two"[..], &b""[..], &b"last"[..]));
    assert_eq!(t.line(9), b"", "past the end is empty, not a panic");
    assert_eq!(t.language(), Some("text"));
    assert_eq!(LineText::new(FileSource::from_bytes("e", Vec::new()), None).line_count(), 0);
    assert_eq!(LineText::new(FileSource::from_bytes("n", b"a\n".to_vec()), None).line_count(), 1);
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
fn std_files_reads_beside_the_source_only_when_it_has_a_path() {
    let dir = std::env::temp_dir().join(format!("lfp-core-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("textures")).unwrap();
    std::fs::write(dir.join("textures/a.png"), b"png").unwrap();
    std::fs::write(dir.join("m.gltf"), b"{}").unwrap();
    let src = FileSource::open(dir.join("m.gltf")).unwrap();
    assert_eq!(StdFiles.resolve(&src, "textures/a.png").unwrap().bytes(), b"png");
    assert_eq!(StdFiles.resolve(&src, "textures\\a.png").unwrap().bytes(), b"png", "Windows separators");
    assert_eq!(StdFiles.resolve(&src, "nope.png").unwrap_err(), Unreadable::Missing);
    assert_eq!(StdFiles.resolve(&src, "textures").unwrap_err(), Unreadable::Missing, "a folder isn't a file");
    let in_memory = FileSource::from_bytes("m.gltf", Vec::new());
    assert_eq!(StdFiles.resolve(&in_memory, "textures/a.png").unwrap_err(), Unreadable::Missing);
    std::fs::remove_dir_all(dir).unwrap();
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
