//! Every reader of `libfileparser-3d`: the first byte picks one, the rest is the file.
#![no_main]
libfuzzer_sys::fuzz_target!(|data: &[u8]| viewer_fuzz::category(libfileparser_3d::readers(), data));
