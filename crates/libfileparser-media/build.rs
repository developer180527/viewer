//! Builds dav1d (AV1) and then FFmpeg from their submodules (`third_party/dav1d`,
//! `third_party/ffmpeg`, release tags), each with its own build out of tree in OUT_DIR
//! (dav1d: meson and ninja; FFmpeg: configure and make), then the shim (shim/media.c)
//! against them. Each configures once, and again only when its arguments change; the
//! builds after that are incremental.
//!
//! FFmpeg's configuration (docs/design/media.md): every demuxer, decoder and parser,
//! plus swscale and swresample, and dav1d for AV1 (FFmpeg's own AV1 decoder only drives
//! hardware); no programs, encoders, muxers, network, devices or filters; nothing from
//! the system but zlib and iconv. LGPL: no --enable-gpl. dav1d is BSD-2-Clause.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let target = std::env::var("TARGET").unwrap();
    if target.contains("windows") {
        // FFmpeg's configure needs a POSIX shell (MSYS2) on Windows, not wired in yet.
        // Until then the crate is empty there, as libfileparser-mupdf is, so a
        // workspace build (and `--all-features`) still builds on Windows.
        return;
    }
    let host = std::env::var("HOST").unwrap();
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let third = manifest.join("../../third_party");
    let ffmpeg_src = third.join("ffmpeg").canonicalize().expect("third_party/ffmpeg is missing: run `git submodule update --init third_party/ffmpeg`");
    let dav1d_src = third.join("dav1d").canonicalize().expect("third_party/dav1d is missing: run `git submodule update --init third_party/dav1d`");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());

    let cc = cc::Build::new().get_compiler();
    let ar = cc::Build::new().get_archiver();
    // Their own optimisation, not cc-rs's: a debug build's -O0 would make every decode
    // in tests crawl, and both choose their levels with care.
    let flags: Vec<String> =
        cc.args().iter().map(|a| a.to_string_lossy().into_owned()).filter(|f| !f.starts_with("-O") && !f.starts_with("-g")).collect();
    // What the linker needs of them: the target, architecture and SDK.
    let link_flags: Vec<String> =
        flags.iter().enumerate().filter(|(i, f)| f.starts_with("-arch") || f.starts_with("-m") || f.starts_with("--target") || *f == "-isysroot" || (*i > 0 && flags[i - 1] == "-isysroot")).map(|(_, f)| f.clone()).collect();
    let jobs = std::env::var("NUM_JOBS").unwrap_or_else(|_| "4".into());

    let dav1d = build_dav1d(&dav1d_src, &out, &target, &cc, &ar, &flags, &link_flags, &jobs);
    let prefix = build_ffmpeg(&ffmpeg_src, &out, &target, &host, &cc, &flags, &link_flags, &jobs, &dav1d);

    cc::Build::new().file(manifest.join("shim/media.c")).include(prefix.join("include")).warnings(false).compile("viewer_media_shim");

    println!("cargo:rustc-link-search=native={}", prefix.join("lib").display());
    println!("cargo:rustc-link-search=native={}", dav1d.join("lib").display());
    // Order matters for static libraries: users before what they use.
    for lib in ["avformat", "avcodec", "swscale", "swresample", "avutil", "dav1d"] {
        println!("cargo:rustc-link-lib=static={lib}");
    }
    println!("cargo:rustc-link-lib=z");
    if target.contains("apple") {
        println!("cargo:rustc-link-lib=iconv");
        println!("cargo:rustc-link-lib=framework=CoreFoundation");
    }
    println!("cargo:rerun-if-changed=shim/media.c");
    println!("cargo:rerun-if-changed=build.rs");
    for (name, src) in [("ffmpeg", &ffmpeg_src), ("dav1d", &dav1d_src)] {
        rerun_on_tag(name, src);
    }
}

/// dav1d as a static library, installed under OUT_DIR/dav1d; returns that prefix.
#[allow(clippy::too_many_arguments)]
fn build_dav1d(src: &Path, out: &Path, target: &str, cc: &cc::Tool, ar: &Command, flags: &[String], link_flags: &[String], jobs: &str) -> PathBuf {
    let build = out.join("dav1d-build");
    let prefix = out.join("dav1d");
    // Always a cross file, for the native build too: it carries the target's compiler
    // and flags (deployment target, SDK) as cc-rs has them.
    let (family, cpu) = if target.starts_with("aarch64") { ("aarch64", "aarch64") } else { ("x86_64", "x86_64") };
    let system = if target.contains("apple") { "darwin" } else if target.contains("android") { "android" } else { "linux" };
    let list = |v: &[String]| v.iter().map(|f| format!("'{}'", f.replace('\'', "\\'"))).collect::<Vec<_>>().join(", ");
    let cross = format!(
        "[binaries]\nc = '{}'\nar = '{}'\nstrip = 'strip'\npkg-config = 'false'\n\n[built-in options]\nc_args = [{}]\nc_link_args = [{}]\n\n[host_machine]\nsystem = '{system}'\ncpu_family = '{family}'\ncpu = '{cpu}'\nendian = 'little'\n",
        cc.path().display(),
        ar.get_program().to_string_lossy(),
        list(flags),
        list(link_flags),
    );
    let cross_file = out.join("dav1d-cross.ini");
    let options = [
        format!("--prefix={}", prefix.display()),
        "--libdir=lib".into(),
        "--default-library=static".into(),
        "--buildtype=release".into(),
        "-Denable_tools=false".into(),
        "-Denable_tests=false".into(),
        "-Denable_docs=false".into(),
        "-Dlogging=false".into(),
        format!("--cross-file={}", cross_file.display()),
    ];
    let stamp = format!("{cross}\n{}", options.join("\n"));
    if !build.join("build.ninja").exists() || std::fs::read_to_string(out.join("dav1d-stamp")).ok().as_deref() != Some(&stamp) {
        std::fs::write(&cross_file, &cross).unwrap();
        let _ = std::fs::remove_dir_all(&build);
        let status = without_ios_default(Command::new("meson"))
            .arg("setup")
            .arg(&build)
            .arg(src)
            .args(&options)
            .status()
            .expect("couldn't run meson for dav1d: install it (`brew install meson`, or `pip install meson`)");
        assert!(status.success(), "configuring dav1d failed (see {}/meson-logs/meson-log.txt)", build.display());
        std::fs::write(out.join("dav1d-stamp"), &stamp).unwrap();
    }
    let status = without_ios_default(Command::new("ninja")).arg("-C").arg(&build).arg(format!("-j{jobs}")).arg("install").status().expect("couldn't run ninja for dav1d");
    assert!(status.success(), "building dav1d failed");
    prefix
}

/// FFmpeg as static libraries, installed under OUT_DIR/ffmpeg; returns that prefix.
#[allow(clippy::too_many_arguments)]
fn build_ffmpeg(src: &Path, out: &Path, target: &str, host: &str, cc: &cc::Tool, flags: &[String], link_flags: &[String], jobs: &str, dav1d: &Path) -> PathBuf {
    let build = out.join("ffmpeg-build");
    let prefix = out.join("ffmpeg");
    std::fs::create_dir_all(&build).unwrap();
    let mut args = vec![
        format!("--prefix={}", prefix.display()),
        "--enable-static".into(),
        "--disable-shared".into(),
        "--enable-pic".into(),
        "--disable-programs".into(),
        "--disable-doc".into(),
        "--disable-avdevice".into(),
        "--disable-avfilter".into(),
        "--disable-network".into(),
        "--disable-encoders".into(),
        "--disable-muxers".into(),
        "--disable-devices".into(),
        "--disable-filters".into(),
        "--disable-protocols".into(),
        // Hardware decoding is the player's business (VideoToolbox), not the parser's.
        "--disable-hwaccels".into(),
        "--disable-autodetect".into(),
        "--enable-zlib".into(),
        "--enable-iconv".into(),
        // AV1 in software.
        "--enable-libdav1d".into(),
        "--pkg-config=pkg-config".into(),
        "--pkg-config-flags=--static".into(),
        "--disable-debug".into(),
        format!("--cc={}", cc.path().display()),
        format!("--extra-cflags={}", flags.join(" ")),
        format!("--extra-ldflags={}", link_flags.join(" ")),
    ];
    if let Some(cross) = cross_args(target, host, flags) {
        args.extend(cross);
    }
    // FFmpeg builds a few programs it runs during the build (swscale's assembly
    // generator). They're the host's, always: without a target of their own, Apple's
    // clang takes .cargo/config.toml's IPHONEOS_DEPLOYMENT_TARGET as a request for iOS,
    // and macOS kills what it builds.
    if host.contains("apple-darwin") {
        let arch = if host.starts_with("aarch64") { "arm64" } else { "x86_64" };
        let t = format!("--target={arch}-apple-macosx14.0");
        args.push(format!("--host-cflags={t}"));
        args.push(format!("--host-ldflags={t}"));
    }
    let pkg_config_path = dav1d.join("lib/pkgconfig");
    let stamp = args.join("\n");
    if !build.join("ffbuild/config.mak").exists() || std::fs::read_to_string(out.join("ffmpeg-stamp")).ok().as_deref() != Some(&stamp) {
        let status = without_ios_default(Command::new(src.join("configure")))
            .current_dir(&build)
            .env("PKG_CONFIG_PATH", &pkg_config_path)
            .env("PKG_CONFIG_LIBDIR", &pkg_config_path)
            .args(&args)
            .status()
            .expect("couldn't run FFmpeg's configure");
        assert!(status.success(), "configuring FFmpeg failed (see {}/ffbuild/config.log)", build.display());
        std::fs::write(out.join("ffmpeg-stamp"), &stamp).unwrap();
    }
    let status = without_ios_default(Command::new("make")).current_dir(&build).arg(format!("-j{jobs}")).arg("install").status().expect("couldn't run make for FFmpeg");
    assert!(status.success(), "building FFmpeg failed");
    prefix
}

/// The command without .cargo/config.toml's IPHONEOS_DEPLOYMENT_TARGET: every target
/// compile here carries its own target, and with that variable set Apple's clang
/// builds untargeted host programs and probes for iOS.
fn without_ios_default(mut c: Command) -> Command {
    c.env_remove("IPHONEOS_DEPLOYMENT_TARGET");
    c
}

/// configure's cross-compiling arguments for Apple's phones and simulators.
fn cross_args(target: &str, host: &str, flags: &[String]) -> Option<Vec<String>> {
    if host == target {
        return None;
    }
    let arch = if target.starts_with("aarch64") { "aarch64" } else { "x86_64" };
    let os = if target.contains("apple") { "darwin" } else { "linux" };
    let mut args = vec!["--enable-cross-compile".to_string(), format!("--arch={arch}"), format!("--target-os={os}")];
    if let Some(i) = flags.iter().position(|f| f == "-isysroot") {
        args.push(format!("--sysroot={}", flags[i + 1]));
    }
    Some(args)
}

/// A new checkout (another tag) rebuilds.
fn rerun_on_tag(name: &str, src: &Path) {
    let head = src.join(format!("../../.git/modules/third_party/{name}/HEAD"));
    if head.exists() {
        println!("cargo:rerun-if-changed={}", head.display());
    }
    for f in ["RELEASE", "meson.build"] {
        if src.join(f).exists() {
            println!("cargo:rerun-if-changed={}", src.join(f).display());
        }
    }
}
