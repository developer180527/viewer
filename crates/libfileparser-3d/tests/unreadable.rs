//! Files a model names but Viewer can't read are reported on the document, as
//! blocked (not permitted: on iOS, the user picked the model alone) or missing.

use std::path::{Path, PathBuf};

use libfileparser_core::{Document, FileSource, ParseContext, Reader};
use libfileparser_core::DependencyStatus::{Blocked, Missing};

const TRIANGLE: &str = "v 0 0 0\nv 1 0 0\nv 0 1 0\nvt 0 0\nusemtl wood\nf 1/1 2/1 3/1\n";

fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("libfileparser-3d-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn open_obj(path: &Path) -> Document {
    // The default context reads neighbours from the file system (StdFiles).
    match libfileparser_3d::ObjReader.open(&FileSource::open(path).unwrap(), &ParseContext::new()) {
        Ok(doc) => doc,
        Err(e) => panic!("the triangle doesn't open: {e}"),
    }
}

#[test]
fn a_material_library_that_isnt_there_is_missing() {
    let dir = folder("missing-mtl");
    std::fs::write(dir.join("model.obj"), format!("mtllib gone.mtl\n{TRIANGLE}")).unwrap();
    let doc = open_obj(&dir.join("model.obj"));
    assert_eq!(doc.dependencies().with_status(Missing), ["gone.mtl"]);
    assert_eq!(doc.dependencies().count(Blocked), 0);
    std::fs::remove_dir_all(dir).unwrap();
}

/// A folder with no permissions stands in for iOS refusing the model's neighbours.
#[cfg(unix)]
#[test]
fn a_texture_in_a_folder_that_cant_be_read_is_blocked() {
    use std::os::unix::fs::PermissionsExt;
    let dir = folder("blocked-texture");
    std::fs::create_dir_all(dir.join("textures")).unwrap();
    std::fs::write(dir.join("model.obj"), format!("mtllib wood.mtl\n{TRIANGLE}")).unwrap();
    std::fs::write(dir.join("wood.mtl"), "newmtl wood\nKd 1 1 1\nmap_Kd textures/wood.png\n").unwrap();
    std::fs::write(dir.join("textures/wood.png"), b"not really a png").unwrap();
    std::fs::set_permissions(dir.join("textures"), std::fs::Permissions::from_mode(0o000)).unwrap();
    let doc = open_obj(&dir.join("model.obj"));
    // Root (some CI images) can read the locked folder anyway; nothing is blocked then.
    let locked = std::fs::read_dir(dir.join("textures")).is_err();
    std::fs::set_permissions(dir.join("textures"), std::fs::Permissions::from_mode(0o755)).unwrap();
    if locked {
        assert_eq!(doc.dependencies().with_status(Blocked), ["wood.png"]);
        assert_eq!(doc.dependencies().count(Missing), 0);
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_texture_that_isnt_there_is_missing() {
    let dir = folder("missing-texture");
    std::fs::write(dir.join("model.obj"), format!("mtllib wood.mtl\n{TRIANGLE}")).unwrap();
    std::fs::write(dir.join("wood.mtl"), "newmtl wood\nKd 1 1 1\nmap_Kd wood.png\n").unwrap();
    let doc = open_obj(&dir.join("model.obj"));
    assert_eq!(doc.dependencies().with_status(Missing), ["wood.png"]);
    std::fs::remove_dir_all(dir).unwrap();
}
