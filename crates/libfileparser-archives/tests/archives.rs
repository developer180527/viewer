//! Zip, tar, gzip and folders (moved here from Viewer's `fmt-archive`), and the
//! conformance suite (docs/design/libfileparser.md §9).

use std::io::{Cursor, Write};
use std::sync::Arc;

use libfileparser_archives::{GzipReader, ZipReader, open_folder};
use libfileparser_core::{Container, Content, Document, FileSource, ParseContext, ParseError, Reader};
use libfileparser_testkit::{Sample, assert_conforms, tiny_limits};

fn container(doc: &Document, cx: &ParseContext) -> Arc<dyn Container> {
    match doc.facet(0, cx).unwrap() {
        Content::Container(c) => c,
        _ => panic!("not a container"),
    }
}

fn paths(c: &Arc<dyn Container>) -> Vec<String> {
    c.entries(0..100).unwrap().into_iter().map(|e| e.path).collect()
}

fn zip(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut buf = Cursor::new(Vec::new());
    let mut w = zip::ZipWriter::new(&mut buf);
    for (name, data) in files {
        w.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
        w.write_all(data).unwrap();
    }
    w.finish().unwrap();
    buf.into_inner()
}

fn tar_gz() -> Vec<u8> {
    let mut tar = tar::Builder::new(Vec::new());
    let mut h = tar::Header::new_gnu();
    h.set_size(3);
    h.set_cksum();
    tar.append_data(&mut h, "a/b.txt", &b"abc"[..]).unwrap();
    let tar = tar.into_inner().unwrap();
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(&tar).unwrap();
    gz.finish().unwrap()
}

#[test]
fn zip_round_trip() {
    let cx = ParseContext::new();
    let doc = ZipReader.open(&FileSource::from_bytes("a.zip", zip(&[("dir/hello.txt", b"hello")])), &cx).unwrap();
    let c = container(&doc, &cx);
    assert_eq!(paths(&c), ["dir/hello.txt"]);
    assert_eq!(c.open(0).unwrap().bytes(), b"hello");
    assert!(matches!(c.open(1), Err(ParseError::InvalidRequest(_))));
}

#[test]
fn tar_gz_round_trip() {
    let cx = ParseContext::new();
    let src = FileSource::from_bytes("x.tar.gz", tar_gz());
    // A .tar.gz is a tar before it's a gzip.
    assert_eq!(libfileparser_archives::detect(src.header(), src.name())[0].0.info().id, "tar");
    let doc = libfileparser_archives::open(&src, &cx).unwrap();
    let c = container(&doc, &cx);
    assert_eq!(paths(&c), ["a/b.txt"]);
    assert_eq!(c.open(0).unwrap().bytes(), b"abc");
    // Gzip alone: one entry, the file inside.
    let doc = GzipReader.open(&src, &cx).unwrap();
    assert_eq!(paths(&container(&doc, &cx)), ["x.tar"]);
}

#[test]
fn a_decompression_bomb_stops_at_the_limit() {
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    gz.write_all(&vec![0u8; 1 << 20]).unwrap();
    let src = FileSource::from_bytes("zeros.gz", gz.finish().unwrap());
    let cx = ParseContext::new().with_limits(tiny_limits());
    assert!(matches!(GzipReader.open(&src, &cx), Err(ParseError::TooLarge(_))));
    let zipped = FileSource::from_bytes("z.zip", zip(&[("zeros", &vec![0u8; 1 << 20])]));
    let doc = ZipReader.open(&zipped, &cx).unwrap();
    assert!(matches!(container(&doc, &cx).open(0), Err(ParseError::TooLarge(_))));
}

#[test]
fn folder_lists_files_under_it_and_opens_them_from_disk() {
    let root = std::env::temp_dir().join(format!("lfp-folder-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("textures")).unwrap();
    std::fs::write(root.join("model.obj"), b"v 0 0 0").unwrap();
    std::fs::write(root.join("textures/albedo.png"), b"png").unwrap();
    std::fs::write(root.join(".DS_Store"), b"junk").unwrap();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::write(root.join(".git/HEAD"), b"ref").unwrap();

    let cx = ParseContext::new();
    let doc = open_folder(&root, &cx).unwrap();
    let c = container(&doc, &cx);
    assert_eq!(paths(&c), ["model.obj", "textures/albedo.png"]);
    let src = c.open(1).unwrap();
    assert_eq!(src.bytes(), b"png");
    // A real path, so a reader can find the files next to it.
    assert_eq!(src.path(), Some(root.join("textures/albedo.png").as_path()));
    std::fs::remove_dir_all(&root).unwrap();
}

#[cfg(unix)]
#[test]
fn unreadable_subfolders_are_skipped_not_fatal() {
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir().join(format!("lfp-folder-locked-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("locked")).unwrap();
    std::fs::write(root.join("model.obj"), b"v 0 0 0").unwrap();
    std::fs::write(root.join("locked/secret.png"), b"x").unwrap();
    std::fs::set_permissions(root.join("locked"), std::fs::Permissions::from_mode(0o000)).unwrap();
    let cx = ParseContext::new();
    let doc = open_folder(&root, &cx);
    std::fs::set_permissions(root.join("locked"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let doc = doc.unwrap();
    let note = doc.metadata().iter().find(|(k, _)| k == "Not readable").map(|(_, v)| v.clone());
    let p = paths(&container(&doc, &cx));
    // Root runs this as root on some CI images, where nothing is unreadable.
    if p.len() == 1 {
        assert_eq!(p, ["model.obj"]);
        assert!(note.unwrap().contains("locked"));
    }
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn every_reader_conforms() {
    let mut tar = tar::Builder::new(Vec::new());
    let mut h = tar::Header::new_gnu();
    h.set_size(2);
    h.set_cksum();
    tar.append_data(&mut h, "hi.txt", &b"hi"[..]).unwrap();
    let tar = tar.into_inner().unwrap();
    for reader in libfileparser_archives::readers() {
        let samples = match reader.info().id {
            // Two entries listed: within even the tiny limits (entries inflate when opened).
            "zip" => vec![Sample::new("a.zip", zip(&[("a.txt", b"a"), ("b/c.txt", b"c")])).fits_tiny_limits()],
            "tar" => vec![Sample::new("a.tar", tar.clone()).fits_tiny_limits(), Sample::new("x.tar.gz", tar_gz()).fits_tiny_limits()],
            "gzip" => vec![Sample::new("x.tar.gz", tar_gz()).fits_tiny_limits()],
            id => panic!("reader {id:?} has no conformance samples"),
        };
        assert_conforms(*reader, &samples);
    }
}
