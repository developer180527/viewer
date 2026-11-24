//! docx-lite through its public API: the corpus's sample, events that nest properly,
//! limits, and damaged files that fail cleanly.

use docx_lite::{Error, Limits, Sink, Stop, Tag};

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
    std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/files/docs/sample.docx")).unwrap()
}

#[test]
fn reads_the_sample() {
    let mut log = Log::default();
    let info = docx_lite::parse(&sample(), &mut log, &Limits::default()).unwrap();
    assert_eq!((info.title.as_deref(), info.author.as_deref()), (Some("Corpus document"), Some("Viewer")));
    assert!(log.open.is_empty(), "everything closed: {}", log.out);
    let l = log.out;
    assert!(l.contains("<Heading(1)>Corpus document</Heading(1)>"), "{l}");
    assert!(l.contains("<Strong>bold</Strong>") && l.contains("<Emphasis>italic</Emphasis>"), "{l}");
    assert!(l.contains("<Link(\"https://example.org\")>a link</Link(\"https://example.org\")>"), "{l}");
    assert!(l.contains("<List { ordered: true }><Item><Paragraph>First</Paragraph></Item><Item><Paragraph>Second</Paragraph></Item></List { ordered: true }>"), "{l}");
    assert!(l.contains("<Table><Row><Cell><Paragraph>A</Paragraph></Cell>"), "{l}");
}

#[test]
fn limits_and_refusals() {
    assert!(matches!(docx_lite::parse(b"not a zip", &mut Log::default(), &Limits::default()), Err(Error::Zip(_))));
    assert!(matches!(docx_lite::parse(&sample(), &mut Log::default(), &Limits::default().with_entries(2)), Err(Error::TooLarge { what: "entries", .. })));
    assert!(matches!(docx_lite::parse(&sample(), &mut Log::default(), &Limits::default().with_depth(1)), Err(Error::TooDeep(1))));
    struct Quit;
    impl Sink for Quit {
        fn text(&mut self, _: &str) -> Result<(), Stop> {
            Err(Stop)
        }
    }
    assert_eq!(docx_lite::parse(&sample(), &mut Quit, &Limits::default()), Err(Error::Stopped));
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
        let _ = docx_lite::parse(&b, &mut Log::default(), &Limits::default());
    }
}

/// A package of these parts, stored.
fn package(parts: &[(&str, &str)]) -> Vec<u8> {
    use std::io::Write;
    let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, text) in parts {
        z.start_file(*name, zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored)).unwrap();
        z.write_all(text.as_bytes()).unwrap();
    }
    z.finish().unwrap().into_inner()
}

#[test]
fn numbered_headings_carry_their_numbers() {
    let w = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main""#;
    let styles = format!(
        r#"<w:styles {w}>
        <w:style w:styleId="Heading1"><w:name w:val="heading 1"/><w:pPr><w:numPr><w:numId w:val="1"/></w:numPr></w:pPr></w:style>
        <w:style w:styleId="Heading2"><w:name w:val="heading 2"/><w:basedOn w:val="Heading1"/><w:pPr><w:numPr><w:ilvl w:val="1"/></w:numPr></w:pPr></w:style>
        </w:styles>"#
    );
    let numbering = format!(
        r#"<w:numbering {w}>
        <w:abstractNum w:abstractNumId="0">
          <w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl>
          <w:lvl w:ilvl="1"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1.%2"/></w:lvl>
        </w:abstractNum>
        <w:abstractNum w:abstractNumId="1"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="upperRoman"/><w:lvlText w:val="Part %1:"/></w:lvl></w:abstractNum>
        <w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>
        <w:num w:numId="2"><w:abstractNumId w:val="1"/><w:lvlOverride w:ilvl="0"><w:startOverride w:val="3"/></w:lvlOverride></w:num>
        </w:numbering>"#
    );
    let h = |style: &str, text: &str| format!(r#"<w:p><w:pPr><w:pStyle w:val="{style}"/></w:pPr><w:r><w:t>{text}</w:t></w:r></w:p>"#);
    let body = format!(
        r#"<w:document {w}><w:body>{}{}{}{}<w:p><w:r><w:t>Body.</w:t></w:r></w:p>{}{}</w:body></w:document>"#,
        h("Heading1", "Introduction"),
        h("Heading2", "Context"),
        h("Heading2", "Scope"),
        h("Heading1", "Method"),
        h("Heading2", "Data"),
        r#"<w:p><w:pPr><w:pStyle w:val="Heading1"/><w:numPr><w:ilvl w:val="0"/><w:numId w:val="2"/></w:numPr></w:pPr><w:r><w:t>Appendix</w:t></w:r></w:p>"#,
    );
    let docx = package(&[
        ("[Content_Types].xml", "<Types/>"),
        ("_rels/.rels", r#"<Relationships><Relationship Id="r" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#),
        ("word/document.xml", &body),
        ("word/styles.xml", &styles),
        ("word/numbering.xml", &numbering),
    ]);
    let mut log = Log::default();
    docx_lite::parse(&docx, &mut log, &Limits::default()).unwrap();
    let l = log.out;
    for want in ["<Heading(1)>1. Introduction</Heading(1)>", "<Heading(2)>1.1 Context</Heading(2)>", "<Heading(2)>1.2 Scope</Heading(2)>", "<Heading(1)>2. Method</Heading(1)>", "<Heading(2)>2.1 Data</Heading(2)>", "<Heading(1)>Part III: Appendix</Heading(1)>", "<Paragraph>Body.</Paragraph>"] {
        assert!(l.contains(want), "{want} in {l}");
    }
}
