//! ods-lite's XML (flat ODS, and `content.xml` of a zipped one) without the zip around it.
#![no_main]
libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let limits = ods_lite::Limits::default().with_cells(1 << 22).with_text(64 << 20).with_part_bytes(64 << 20);
    let _ = ods_lite::read_flat(data, &limits);
});
