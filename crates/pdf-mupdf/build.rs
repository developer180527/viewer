//! Builds MuPDF from the `third_party/mupdf` submodule with its own makefiles, then
//! the shim (shim/shim.c) against it. `make` is incremental in OUT_DIR, so a rebuild
//! with nothing changed costs a second.
//!
//! Configuration (docs/design/pdf.md §4): no JavaScript (`mujs=no`), no DOCX export, no
//! Brotli, and only the base fonts (the big CJK ones aren't built in).

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let target = std::env::var("TARGET").unwrap();
    if target.contains("windows") {
        // MuPDF builds on Windows with its Visual Studio projects (platform/win32);
        // wiring those in is docs/design/pdf.md §10 step 7. Until then the crate is
        // empty there and nothing depends on it (viewer-fmt-pdf has no Windows backend).
        return;
    }
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let src = manifest.join("../../third_party/mupdf").canonicalize().expect(
        "third_party/mupdf is missing: run `git submodule update --init --recursive`",
    );
    if !src.join("thirdparty/freetype/README").exists() {
        panic!("MuPDF's own submodules are missing: run `git submodule update --init --recursive`");
    }
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("mupdf");

    // The target's compiler and flags as cc-rs sees them, so cross builds (Android
    // NDK, BSD) use the same toolchain as the shim.
    let cc = cc::Build::new().get_compiler();
    let cxx = cc::Build::new().cpp(true).get_compiler();
    let ar = cc::Build::new().get_archiver();
    let flags = |t: &cc::Tool| t.args().iter().map(|a| a.to_string_lossy().into_owned()).collect::<Vec<_>>().join(" ");
    let xcflags = format!("{} -DTOFU_CJK_EXT -DTOFU_CJK_LANG", flags(&cc));

    let jobs = std::env::var("NUM_JOBS").unwrap_or_else(|_| "4".into());
    let status = Command::new(if target.contains("bsd") { "gmake" } else { "make" })
        .current_dir(&src)
        .arg(format!("-j{jobs}"))
        .arg("libs")
        .arg("build=release")
        .args(["mujs=no", "extract=no", "brotli=no"])
        .arg(format!("OUT={}", out.display()))
        .arg(format!("CC={}", cc.path().display()))
        .arg(format!("CXX={}", cxx.path().display()))
        .arg(format!("AR={}", ar.get_program().to_string_lossy()))
        .arg(format!("XCFLAGS={xcflags}"))
        .arg(format!("XCXXFLAGS={}", flags(&cxx)))
        // Plain static libraries: no shared build, no system copies of the libraries.
        .args(["shared=no", "USE_SYSTEM_LIBS=no"])
        .status()
        .expect("couldn't run make for MuPDF");
    assert!(status.success(), "building MuPDF failed");

    cc::Build::new().file(manifest.join("shim/shim.c")).include(src.join("include")).warnings(false).compile("viewer_mupdf_shim");

    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=mupdf");
    println!("cargo:rustc-link-lib=static=mupdf-third");
    // HarfBuzz (in mupdf-third) is C++.
    if target.contains("apple") || target.contains("bsd") || target.contains("android") {
        println!("cargo:rustc-link-lib=c++");
    } else {
        println!("cargo:rustc-link-lib=stdc++");
    }
    println!("cargo:rerun-if-changed=shim/shim.c");
    println!("cargo:rerun-if-changed=../../third_party/mupdf/include");
    println!("cargo:rerun-if-changed=../../third_party/mupdf/source");
}
