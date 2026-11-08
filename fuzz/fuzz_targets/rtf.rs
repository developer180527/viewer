//! rtf-lite on its own: every event, nothing kept.
#![no_main]
struct Count(usize);
impl rtf_lite::Sink for Count {
    fn text(&mut self, t: &str) -> Result<(), rtf_lite::Stop> {
        self.0 += t.len();
        Ok(())
    }
}
libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let _ = rtf_lite::parse(data, &mut Count(0), &rtf_lite::Limits::default());
});
