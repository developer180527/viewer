//! Documents written in XML vocabularies (DocBook, TEI, FictionBook, an OpenDocument
//! presentation's slides), read by `xmldoc-lite` into HTML, then formatted text
//! through `crate::html`. Nothing is fetched: a DTD isn't loaded, XIncludes and
//! external entities aren't followed.

use libfileparser_core::{Category, Confidence, Content, Detection, Document, FileSource, FormatInfo, ParseContext, ParseError, Reader, ReaderInfo, Result, Role, is_text};
use crate::markup::Rendered;

static DOCBOOK: ReaderInfo = ReaderInfo::new("docbook", "DocBook", Category::Docs).extensions(&["dbk", "docbook"]).mime_types(&["application/docbook+xml"]);
static TEI: ReaderInfo = ReaderInfo::new("tei", "TEI", Category::Docs).extensions(&["tei"]).mime_types(&["application/tei+xml"]);

static ODP: ReaderInfo = ReaderInfo::new("odp", "OpenDocument presentation", Category::Docs).extensions(&["odp", "otp"]).mime_types(&["application/vnd.oasis.opendocument.presentation"]);
static FB2: ReaderInfo = ReaderInfo::new("fb2", "FictionBook", Category::Docs).extensions(&["fb2"]).mime_types(&["application/x-fictionbook+xml"]);

pub struct DocBookReader;
pub struct TeiReader;
pub struct Fb2Reader;
pub struct OdpReader;

const ODP_MIMETYPE: &[u8] = b"mimetypeapplication/vnd.oasis.opendocument.presentation";

impl Reader for OdpReader {
    fn info(&self) -> &ReaderInfo {
        &ODP
    }

    /// Certain from its `mimetype` part, stored first; a zip named `.odp` is tried.
    fn detect(&self, header: &[u8], name: &str) -> Detection {
        if header.starts_with(b"PK\x03\x04") && header.get(30..30 + ODP_MIMETYPE.len()).is_some_and(|m| m == ODP_MIMETYPE) {
            Detection::new(Confidence::Certain)
        } else if header.starts_with(b"PK\x03\x04") && ODP.matches_extension(name) {
            Detection::new(Confidence::Magic)
        } else {
            Detection::NONE
        }
    }

    /// Each slide's text, tables, images and speaker notes, from `content.xml`;
    /// images read from the package when shown.
    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        use std::sync::Arc;
        cx.cancel.check()?;
        if !src.header().starts_with(b"PK\x03\x04") {
            return Err(ParseError::NotThisFormat);
        }
        let package = crate::package::Package::open(src, cx)?;
        let content = package.read("content.xml")?.ok_or(ParseError::NotThisFormat)?;
        let text = String::from_utf8_lossy(&content);
        if !text.contains("office:presentation") {
            return Err(ParseError::NotThisFormat);
        }
        let xmldoc_lite::Presentation { html: r, slides } = xmldoc_lite::presentation(&text);
        let mut b = crate::build::Builder::new(cx);
        crate::html::parse(&r.html, "content.xml", &mut b)?;
        let mut doc = Document::new(FormatInfo::new("odp", "OpenDocument presentation", Category::Docs))
            .with_facet(Role::Primary, "Slides", Content::Flow(Arc::new(crate::package::PackagedFlow::new(b.finish()?, package))))
            .with_meta("Slides", slides);
        if let Some(meta) = crate::package::Package::open(src, cx)?.read("meta.xml")? {
            let m = String::from_utf8_lossy(&meta);
            for (tag, key) in [("dc:title", "Title"), ("dc:creator", "Author"), ("meta:initial-creator", "Created by"), ("dc:subject", "Subject")] {
                if let Some(v) = xmldoc_lite::element_text(&m, tag) {
                    doc = doc.with_meta(key, v);
                }
            }
        }
        for w in r.warnings {
            doc = doc.with_warning(libfileparser_core::Warning::new(w));
        }
        Ok(doc.with_file_facets(src, None))
    }
}

impl Reader for DocBookReader {
    fn info(&self) -> &ReaderInfo {
        &DOCBOOK
    }

    /// By its root element in DocBook's namespace or DTD (any extension, `.xml`
    /// included); by extension alone otherwise.
    fn detect(&self, header: &[u8], name: &str) -> Detection {
        if !is_text(header) {
            return Detection::NONE;
        }
        if xmldoc_lite::is_docbook(header) {
            Detection::new(Confidence::Content).with_language("xml")
        } else if DOCBOOK.matches_extension(name) {
            Detection::new(Confidence::Extension).with_language("xml")
        } else {
            Detection::NONE
        }
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        crate::markup::read(src, cx, "docbook", "DocBook document", docbook)
    }
}

impl Reader for TeiReader {
    fn info(&self) -> &ReaderInfo {
        &TEI
    }

    /// By its root, `<TEI>` (or P4's `<TEI.2>`); by extension alone otherwise.
    fn detect(&self, header: &[u8], name: &str) -> Detection {
        if !is_text(header) {
            return Detection::NONE;
        }
        if xmldoc_lite::is_tei(header) {
            Detection::new(Confidence::Content).with_language("xml")
        } else if TEI.matches_extension(name) {
            Detection::new(Confidence::Extension).with_language("xml")
        } else {
            Detection::NONE
        }
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        crate::markup::read(src, cx, "tei", "TEI document", tei)
    }
}

impl Reader for Fb2Reader {
    fn info(&self) -> &ReaderInfo {
        &FB2
    }

    /// By its root, `<FictionBook>`.
    fn detect(&self, header: &[u8], name: &str) -> Detection {
        if xmldoc_lite::is_fictionbook(header) {
            Detection::new(Confidence::Content).with_language("xml")
        } else if FB2.matches_extension(name) && is_text(header) {
            Detection::new(Confidence::Extension).with_language("xml")
        } else {
            Detection::NONE
        }
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        crate::markup::read(src, cx, "fb2", "FictionBook", fb2)
    }
}


fn docbook(text: &str) -> Result<Rendered> {
    Ok(xml(xmldoc_lite::docbook(text)))
}

fn tei(text: &str) -> Result<Rendered> {
    Ok(xml(xmldoc_lite::tei(text)))
}

/// A FictionBook, its pictures (base64 `<binary>` elements) served by their ids.
fn fb2(text: &str) -> Result<Rendered> {
    let mut r = xml(xmldoc_lite::fictionbook(text));
    r.resources = xmldoc_lite::fictionbook_images(text).into_iter().map(|i| (i.id.clone(), FileSource::from_bytes(i.id, i.bytes))).collect();
    Ok(r)
}

fn xml(h: xmldoc_lite::Html) -> Rendered {
    Rendered { html: h.html, title: h.title, warnings: h.warnings, resources: Vec::new() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libfileparser_core::Content;

    fn blocks(reader: &dyn Reader, name: &str, text: &str) -> (Document, String) {
        let src = FileSource::from_bytes(name, text.as_bytes().to_vec());
        assert!(reader.detect(src.header(), name).confidence >= Confidence::Content, "{name}");
        let doc = reader.open(&src, &ParseContext::new()).unwrap();
        let Content::Flow(f) = doc.facet(0, &ParseContext::new()).unwrap() else { panic!() };
        let s = format!("{:?}", f.blocks(0..1000).unwrap());
        (doc, s)
    }

    fn has(s: &str, wants: &[&str]) {
        for w in wants {
            assert!(s.contains(w), "{w} in {s}");
        }
    }

    #[test]
    fn docbook_article() {
        let (doc, s) = blocks(&DocBookReader, "a.xml", r#"<?xml version="1.0"?>
<article xmlns="http://docbook.org/ns/docbook" version="5.0"><info><title>The Article</title><author><personname>A</personname></author></info>
<section><title>First</title><para>Some <emphasis>em</emphasis>, <emphasis role="bold">bold</emphasis> and <command>ls</command> &amp; <link xlink:href="https://x.org" xmlns:xlink="http://www.w3.org/1999/xlink">a link</link>.<footnote><para>A note.</para></footnote></para>
<itemizedlist><listitem><para>one</para></listitem><listitem><para>two</para></listitem></itemizedlist>
<programlisting language="c">int main() { return 0; }</programlisting>
<informaltable><tgroup cols="2"><thead><row><entry>a</entry><entry>b</entry></row></thead><tbody><row><entry>1</entry><entry>2</entry></row></tbody></tgroup></informaltable>
<note><para>Careful.</para></note><mediaobject><imageobject><imagedata fileref="pic.png"/></imageobject></mediaobject>
<section><title>Nested</title><para>Deeper.</para></section><indexterm><primary>hidden</primary></indexterm></section></article>"#);
        has(&s, &["Heading { level: 1, text: [Text(\"The Article\")]", "Heading { level: 2, text: [Text(\"First\")]", "Emphasis([Text(\"em\")])", "Strong([Text(\"bold\")])", "Code(\"ls\")", "&", "https://x.org", "A note.", "List { ordered: false", "language: Some(\"c\")", "Table { header_rows: 1", "Quote", "pic.png", "Heading { level: 3, text: [Text(\"Nested\")]"]);
        assert!(!s.contains("hidden"));
        assert!(doc.metadata().iter().any(|(k, v)| k == "Title" && v == "The Article"));
    }

    #[test]
    fn tei_text() {
        let (doc, s) = blocks(&TeiReader, "a.xml", r#"<TEI xmlns="http://www.tei-c.org/ns/1.0"><teiHeader><fileDesc><titleStmt><title>A Poem</title></titleStmt></fileDesc></teiHeader>
<text><body><div><head>Part One</head><p>Some <hi rend="italic">italic</hi> and <hi rend="bold">bold</hi>, with <ref target="https://x.org">a link</ref>.<note>A note.</note></p>
<lg><l>First line</l><l>Second line</l></lg><list type="ordered"><item>one</item><item>two</item></list>
<table><row><cell>a</cell><cell>b</cell></row></table><div><head>Inner</head><p>Deeper.<lb/>Next.</p></div></div></body></text></TEI>"#);
        has(&s, &["Heading { level: 1, text: [Text(\"A Poem\")]", "Heading { level: 1, text: [Text(\"Part One\")]", "Emphasis([Text(\"italic\")])", "Strong([Text(\"bold\")])", "https://x.org", "A note.", "Quote", "First line", "List { ordered: true", "Table", "Heading { level: 2, text: [Text(\"Inner\")]", "LineBreak"]);
        assert!(doc.metadata().iter().any(|(k, v)| k == "Title" && v == "A Poem"));
    }

    #[test]
    fn fictionbook() {
        let (doc, s) = blocks(&Fb2Reader, "a.fb2", r##"<?xml version="1.0" encoding="utf-8"?>
<FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0" xmlns:l="http://www.w3.org/1999/xlink"><description><title-info><author><first-name>Leo</first-name><last-name>Tolstoy</last-name></author><book-title>War &amp; Peace</book-title></title-info></description>
<body><title><p>Book One</p></title><section><title><p>Chapter 1</p></title><p>Well, <emphasis>Prince</emphasis>, so <strong>Genoa</strong> and Lucca.<a l:href="#n1">1</a></p><empty-line/><poem><stanza><v>A line</v></stanza></poem><image l:href="#pic.jpg"/></section></body>
<body name="notes"><section id="n1"><title><p>1</p></title><p>A note.</p></section></body><binary id="pic.jpg" content-type="image/jpeg">/9j/4AAQ</binary></FictionBook>"##);
        has(&s, &["Heading { level: 1, text: [Text(\"War & Peace\")]", "Leo Tolstoy", "Book One", "Heading { level: 1, text: [Text(\"Chapter 1\")]", "Emphasis([Text(\"Prince\")])", "Strong([Text(\"Genoa\")])", "Superscript", "Quote", "A line", "Image { reference: \"pic.jpg\"", "A note."]);
        assert!(!s.contains("/9j/"), "{s}");
        assert!(doc.metadata().iter().any(|(k, v)| k == "Title" && v == "War & Peace"));
    }

    #[test]
    fn a_presentation() {
        let content = r#"<office:document-content xmlns:office="o" xmlns:draw="d" xmlns:text="t" xmlns:presentation="p" xmlns:xlink="x" xmlns:table="tb"><office:automatic-styles><style>hidden</style></office:automatic-styles><office:body><office:presentation>
<draw:page draw:name="page1"><draw:frame presentation:class="title"><draw:text-box><text:p>Welcome</text:p></draw:text-box></draw:frame><draw:frame><draw:text-box><text:list><text:list-item><text:p>First <text:span>point</text:span></text:p></text:list-item></text:list></draw:text-box></draw:frame><draw:frame><draw:image xlink:href="Pictures/a.png"/></draw:frame><presentation:notes><draw:page-thumbnail/><draw:frame><draw:text-box><text:p>Say hello.</text:p></draw:text-box></draw:frame></presentation:notes></draw:page>
<draw:page draw:name="Overview"><draw:frame><table:table><table:table-row><table:table-cell><text:p>a</text:p></table:table-cell></table:table-row></table:table></draw:frame><presentation:notes><draw:page-thumbnail/></presentation:notes></draw:page>
</office:presentation></office:body></office:document-content>"#;
        let xmldoc_lite::Presentation { html: r, slides: n } = xmldoc_lite::presentation(content);
        assert_eq!(n, 2);
        let cx = ParseContext::new();
        let mut b = crate::build::Builder::new(&cx);
        crate::html::parse(&r.html, "content.xml", &mut b).unwrap();
        let s = format!("{:?}", b.finish().unwrap());
        has(&s, &["Heading { level: 2, text: [Text(\"1.\")]", "Strong([Text(\"Welcome\")])", "List", "First point", "Pictures/a.png", "Notes", "Say hello.", "Text(\"2. Overview\")", "Table"]);
        assert!(!s.contains("hidden") && s.matches("Notes").count() == 1, "{s}");
    }

    #[test]
    fn other_xml_isnt_claimed() {
        assert_eq!(DocBookReader.detect(b"<?xml version=\"1.0\"?><svg xmlns=\"http://www.w3.org/2000/svg\"/>", "a.xml"), Detection::NONE);
        assert_eq!(TeiReader.detect(b"<rss/>", "a.xml"), Detection::NONE);
        assert!(DocBookReader.detect(b"<!DOCTYPE book PUBLIC \"-//OASIS//DTD DocBook XML V4.5//EN\" \"\"><book/>", "a.xml").confidence >= Confidence::Content);
    }

    #[test]
    fn broken_xml_shows_what_came_before() {
        let doc = DocBookReader.open(&FileSource::from_bytes("b.dbk", b"<article xmlns=\"http://docbook.org/ns/docbook\"><para>kept</para><para>cut <".to_vec()), &ParseContext::new()).unwrap();
        let Content::Flow(f) = doc.facet(0, &ParseContext::new()).unwrap() else { panic!() };
        assert!(format!("{:?}", f.blocks(0..10).unwrap()).contains("kept"));
    }
}
