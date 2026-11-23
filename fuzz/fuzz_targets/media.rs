//! The media reader (FFmpeg and dav1d through the shim): the first byte is ignored, the
//! rest is the file. Every facet is made, so frames and waveforms are decoded.
#![no_main]
libfuzzer_sys::fuzz_target!(|data: &[u8]| viewer_fuzz::category(libfileparser_media::readers(), data));
