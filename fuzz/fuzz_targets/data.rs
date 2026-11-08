//! Every reader of `libfileparser-data`: the first byte picks one, the rest is the file.
#![no_main]
libfuzzer_sys::fuzz_target!(|data: &[u8]| viewer_fuzz::category(libfileparser_data::readers(), data));
