//! odt-lite through its public API: the corpus's sample, events that nest properly,
//! limits, and damaged files that fail cleanly.

use odt_lite::{Error, Limits, Sink, Stop, Tag};

/// Events as text, checking as it goes that every close matches the last open.
#[derive(Default)]
struct Log {
    out: String,
    open: Vec<Tag>,
}

impl Sink for Log {
    fn open(&mut self, tag: Tag) -> Result<(), Stop> {
        self.out += &format!("<{tag:?}>");
        self.open.push(tag);
        Ok(())
    }
    fn close(&mut self, tag: Tag) -> Result<(), Stop> {
        assert_eq!(self.open.pop().as_ref(), Some(&tag), "closed out of order in {}", self.out);
        self.out += &format!("</{tag:?}>");
        Ok(())
    }
    fn text(&mut self, text: &str) -> Result<(), Stop> {
        self.out += text;
        Ok(())
    }
    fn image(&mut self, part: &str, _: &str) -> Result<(), Stop> {
        self.out += &format!("[img {part}]");
        Ok(())
    }
}

fn sample() -> Vec<u8> {
    std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/files/docs/sample.odt")).unwrap()
}

#[test]
fn reads_the_sample() {
    let mut log = Log::default();
    let info = odt_lite::parse(&sample(), &mut log, &Limits::default()).unwrap();
    assert_eq!((info.title.as_deref(), info.pages, info.words), (Some("Corpus text"), Some(1), Some(12)));
    assert!(log.open.is_empty(), "everything closed: {}", log.out);
    let l = log.out;
    assert!(l.contains("<Heading(1)>Corpus text</Heading(1)>"), "{l}");
    assert!(l.contains("<Strong>bold</Strong>"), "{l}");
    assert!(l.contains("<Link(\"https://example.org\")>a link</Link(\"\")>") || l.contains("a link</Link"), "{l}");
    assert!(l.contains("<List { ordered: true }><Item><Paragraph>One</Paragraph></Item>"), "{l}");
    assert!(l.contains("<Table><Row { header: false }><Cell><Paragraph>A</Paragraph></Cell>"), "{l}");
}

#[test]
fn limits_and_refusals() {
    assert!(matches!(odt_lite::parse(b"not a zip", &mut Log::default(), &Limits::default()), Err(Error::Zip(_))));
    assert!(matches!(odt_lite::parse(&sample(), &mut Log::default(), &Limits::default().with_entries(2)), Err(Error::TooLarge { what: "entries", .. })));
    assert!(matches!(odt_lite::parse(&sample(), &mut Log::default(), &Limits::default().with_depth(1)), Err(Error::TooDeep(1))));
    struct Quit;
    impl Sink for Quit {
        fn text(&mut self, _: &str) -> Result<(), Stop> {
            Err(Stop)
        }
    }
    assert_eq!(odt_lite::parse(&sample(), &mut Quit, &Limits::default()), Err(Error::Stopped));
}

#[test]
fn damaged_files_fail_cleanly() {
    let good = sample();
    let mut seed = 0x2545F4914F6CDD1Du64;
    for i in 0..400 {
        let mut b = good.clone();
        if i % 2 == 0 {
            b.truncate(good.len() * (i % 97) / 97);
        } else {
            for _ in 0..8 {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let at = (seed as usize) % b.len();
                b[at] = (seed >> 32) as u8;
            }
        }
        // Errors are fine; panics and out-of-order events aren't.
        let _ = odt_lite::parse(&b, &mut Log::default(), &Limits::default());
    }
}
