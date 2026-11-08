//! What every fuzz target shares: limits a fuzzing machine has, and opening a file
//! with one reader of a category and reading it as a viewer would.

use libfileparser_core::{FileSource, Limits, ParseContext, Reader};

/// libFuzzer's panic hook aborts on every panic, caught or not. Our readers catch
/// panics in third-party decoders (calamine, resvg…) on purpose and report the file as
/// damaged, so a panic located in a dependency is let unwind to that catch; one in our
/// own code is still a finding. A third-party panic nothing catches still aborts:
/// `fuzz_target!` catches it at the top.
pub fn init() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let theirs = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let ours = info.location().is_none_or(|l| !l.file().contains("/.cargo/registry/") && !l.file().contains("/.cargo/git/"));
            if ours {
                theirs(info);
            }
        }));
    });
}

/// Desktop limits with allocations and documents held to sizes a fuzzer can afford
/// (the testkit fuzzer's).
pub fn limits() -> Limits {
    let mut l = Limits::desktop();
    l.allocation = 256 << 20;
    l.memory = 512 << 20;
    l.image_pixels = 1 << 24;
    l.unpack_always = 16 << 20;
    l
}

/// `data[0]` picks a reader, the rest is the file, named with the reader's first
/// extension so detection by name has its say. A panic anywhere is a finding,
/// including one a facet's maker turned into an error (`exercise` checks).
pub fn category(readers: &[&'static dyn Reader], data: &[u8]) {
    init();
    let Some((&pick, file)) = data.split_first() else { return };
    let reader = readers[pick as usize % readers.len()];
    let name = format!("fuzz.{}", reader.info().extensions.first().copied().unwrap_or("bin"));
    let src = FileSource::from_bytes(&name, file.to_vec());
    let _ = reader.detect(src.header(), &name);
    let cx = ParseContext::new().with_limits(limits());
    if let Ok(doc) = reader.open(&src, &cx) {
        libfileparser_testkit::exercise(&doc, &cx);
    }
}

/// The byte that picks `id` among `readers`, for seeding.
pub fn pick(readers: &[&'static dyn Reader], id: &str) -> Option<u8> {
    readers.iter().position(|r| r.info().id == id).and_then(|i| u8::try_from(i).ok())
}
