//! Documents written in XML vocabularies: DocBook (4 and 5) and TEI (the Text Encoding
//! Initiative's P5). Each element maps to the HTML it means, read as a stream by
//! quick-xml, then into formatted text through `crate::html`. Nothing is fetched: a
//! DTD isn't loaded, XIncludes and external entities aren't followed.

use libfileparser_core::{Category, Confidence, Content, Detection, Document, FileSource, FormatInfo, ParseContext, ParseError, Reader, ReaderInfo, Result, Role, is_text};
use quick_xml::events::{BytesStart, Event};

use crate::markup::{Rendered, esc, strip_tags};

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
        let (r, slides) = odp(&text);
        let mut b = crate::build::Builder::new(cx);
        crate::html::parse(&r.html, "content.xml", &mut b)?;
        let mut doc = Document::new(FormatInfo::new("odp", "OpenDocument presentation", Category::Docs))
            .with_facet(Role::Primary, "Slides", Content::Flow(Arc::new(crate::package::PackagedFlow::new(b.finish()?, package))))
            .with_meta("Slides", slides);
        if let Some(meta) = crate::package::Package::open(src, cx)?.read("meta.xml")? {
            let m = String::from_utf8_lossy(&meta);
            for (tag, key) in [("dc:title", "Title"), ("dc:creator", "Author"), ("meta:initial-creator", "Created by"), ("dc:subject", "Subject")] {
                if let Some(v) = element_text(&m, tag) {
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

/// The text of the first `<tag>…</tag>` in `xml`.
fn element_text(xml: &str, tag: &str) -> Option<String> {
    let start = xml.find(&format!("<{tag}"))?;
    let open_end = xml[start..].find('>')? + start + 1;
    let end = xml[open_end..].find(&format!("</{tag}>"))? + open_end;
    Some(unescape(&strip_tags(&xml[open_end..end])).trim().to_string()).filter(|t| !t.is_empty())
}

/// A presentation's `content.xml` as HTML: a heading per slide (its title, or its
/// number), then its frames; its speaker notes quoted after it. And the slide count.
fn odp(text: &str) -> (Rendered, usize) {
    let slides = std::cell::Cell::new(0usize);
    let r = walk(
        text,
        |name, e, _| match name {
            "automatic-styles" | "font-face-decls" | "scripts" | "settings" | "styles" | "master-styles" | "forms" | "title-skip" | "desc" | "presentation-settings" | "sequence-decls" | "tracked-changes" | "event-listeners" => Map::Skip,
            "page" if attr(e, "class").is_none() => {
                slides.set(slides.get() + 1);
                let n = slides.get();
                let name = attr(e, "name").filter(|n| !n.starts_with("page") && !n.starts_with("Slide") && !n.is_empty());
                Map::Raw(format!("<h2>{n}.{}</h2>", name.map(|n| format!(" {}", esc(&n))).unwrap_or_default()), "")
            }
            "frame" if attr(e, "class").as_deref() == Some("title") => Map::Tag("p><strong"),
            "notes" => Map::Raw("<blockquote><p><em>Notes</em></p>".into(), "</blockquote>"),
            "h" => Map::Tag("p><strong"),
            "p" => Map::Tag("p"),
            "list" => Map::Tag("ul"),
            "list-item" => Map::Tag("li"),
            "a" => Map::Tag("a"),
            "table" => Map::Tag("table"),
            "table-row" => Map::Tag("tr"),
            "table-cell" => Map::Tag("td"),
            "image" => match attr(e, "href") {
                Some(h) if !h.starts_with('#') => Map::Raw(format!("<img src=\"{}\" alt=\"\">", esc(&h)), ""),
                _ => Map::Pass,
            },
            "note" => Map::Note,
            "note-citation" => Map::Skip,
            _ => Map::Pass,
        },
        |name, e| match name {
            "line-break" => Some("<br>".into()),
            "s" | "tab" => Some(" ".into()),
            "image" => attr(e, "href").filter(|h| !h.starts_with('#')).map(|h| format!("<img src=\"{}\" alt=\"\">", esc(&h))),
            _ => None,
        },
    );
    // Every slide has a notes page; most are blank.
    let mut r = r;
    let label = "<blockquote><p><em>Notes</em></p>";
    let mut html = String::with_capacity(r.html.len());
    let mut rest = r.html.as_str();
    while let Some(at) = rest.find(label) {
        html.push_str(&rest[..at]);
        let after = &rest[at + label.len()..];
        let end = after.find("</blockquote>").map_or(after.len(), |e| e + "</blockquote>".len());
        if strip_tags(&after[..end]).trim().is_empty() {
            rest = &after[end..];
        } else {
            html.push_str(label);
            rest = after;
        }
    }
    html.push_str(rest);
    r.html = html;
    (r, slides.get())
}

/// The root element's name and namespace, past the prolog.
fn root(header: &[u8]) -> Option<(String, String)> {
    let mut r = quick_xml::Reader::from_reader(&header[..header.len().min(8192)]);
    let mut buf = Vec::new();
    loop {
        match r.read_event_into(&mut buf) {
            Ok(Event::Start(e) | Event::Empty(e)) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).into_owned();
                let ns = attr(&e, "xmlns").unwrap_or_default();
                return Some((name, ns));
            }
            Ok(Event::Eof) | Err(_) => return None,
            _ => {}
        }
        buf.clear();
    }
}

fn docbook_root(name: &str, ns: &str, header: &[u8]) -> bool {
    let head = String::from_utf8_lossy(&header[..header.len().min(2048)]);
    let dtd = head.contains("DocBook") || head.contains("docbook");
    let roots = ["book", "article", "chapter", "section", "sect1", "part", "refentry", "reference", "set", "preface", "appendix"];
    roots.contains(&name) && (ns.contains("docbook.org") || dtd)
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
        match root(header) {
            Some((r, ns)) if docbook_root(&r, &ns, header) => Detection::new(Confidence::Content).with_language("xml"),
            _ if DOCBOOK.matches_extension(name) => Detection::new(Confidence::Extension).with_language("xml"),
            _ => Detection::NONE,
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
        match root(header) {
            Some((r, _)) if r == "TEI" || r == "TEI.2" || r == "teiCorpus" => Detection::new(Confidence::Content).with_language("xml"),
            _ if TEI.matches_extension(name) => Detection::new(Confidence::Extension).with_language("xml"),
            _ => Detection::NONE,
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
        match root(header) {
            Some((r, _)) if r == "FictionBook" => Detection::new(Confidence::Content).with_language("xml"),
            _ if FB2.matches_extension(name) && is_text(header) => Detection::new(Confidence::Extension).with_language("xml"),
            _ => Detection::NONE,
        }
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        crate::markup::read(src, cx, "fb2", "FictionBook", fb2)
    }
}

fn attr(e: &BytesStart, name: &str) -> Option<String> {
    e.attributes().flatten().find_map(|a| {
        let key = String::from_utf8_lossy(a.key.as_ref()).into_owned();
        let local = String::from_utf8_lossy(a.key.local_name().as_ref()).into_owned();
        (key == name || local == name).then(|| unescape(&String::from_utf8_lossy(&a.value)))
    })
}

/// XML's entities and character references, and the HTML ones DocBook's DTD defines.
fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(end) = rest.bytes().take(16).position(|c| c == b';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        match entity(&rest[1..end]) {
            Some(c) => out.push_str(&c),
            None => out.push_str(&rest[..=end]),
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

fn entity(name: &str) -> Option<String> {
    if let Some(n) = name.strip_prefix("#x").or_else(|| name.strip_prefix("#X")) {
        return u32::from_str_radix(n, 16).ok().and_then(char::from_u32).map(String::from);
    }
    if let Some(n) = name.strip_prefix('#') {
        return n.parse().ok().and_then(char::from_u32).map(String::from);
    }
    Some(
        match name {
            "amp" => "&",
            "lt" => "<",
            "gt" => ">",
            "quot" => "\"",
            "apos" => "'",
            "nbsp" => "\u{a0}",
            "mdash" => "—",
            "ndash" => "–",
            "hellip" => "…",
            "copy" => "©",
            "reg" => "®",
            "trade" => "™",
            "ldquo" => "“",
            "rdquo" => "”",
            "lsquo" => "‘",
            "rsquo" => "’",
            "laquo" => "«",
            "raquo" => "»",
            "bull" => "•",
            "deg" => "°",
            "times" => "×",
            "eacute" => "é",
            "egrave" => "è",
            "auml" => "ä",
            "ouml" => "ö",
            "uuml" => "ü",
            "szlig" => "ß",
            _ => return None,
        }
        .to_string(),
    )
}

/// What an element becomes.
enum Map {
    /// An HTML element around its content.
    Tag(&'static str),
    /// Its content only.
    Pass,
    /// Nothing, content included.
    Skip,
    /// A heading at the current section depth.
    Heading,
    /// A section: headings inside are one level deeper.
    Section,
    /// Code shown as written (`<pre>`), with a language if it names one.
    Pre,
    /// A note: its text as `[note]` where it's referenced.
    Note,
    /// This HTML, the content, then the second (when not empty).
    Raw(String, &'static str),
}

/// The stream walker both vocabularies share.
struct Walk {
    out: String,
    /// What each open element emitted, to close it.
    stack: Vec<Option<String>>,
    sections: usize,
    /// Inside skipped elements, by depth.
    skip: usize,
    /// Inside `<pre>`: text is kept as written.
    pre: usize,
    title: Option<String>,
    /// The open heading's start in `out`, for the document's title.
    heading_at: Option<usize>,
}

fn walk(text: &str, map: impl Fn(&str, &BytesStart, &[Option<String>]) -> Map, empty: impl Fn(&str, &BytesStart) -> Option<String>) -> Rendered {
    let mut r = quick_xml::Reader::from_str(text);
    r.config_mut().check_end_names = false;
    let mut w = Walk { out: String::with_capacity(text.len()), stack: Vec::new(), sections: 0, skip: 0, pre: 0, title: None, heading_at: None };
    let mut warnings = Vec::new();
    let mut names: Vec<String> = Vec::new();
    loop {
        let ev = match r.read_event() {
            Ok(e) => e,
            Err(e) => {
                warnings.push(format!("The XML ends early: {e}. What came before it is shown."));
                break;
            }
        };
        match ev {
            Event::Eof => break,
            Event::Start(e) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).into_owned();
                names.push(name.clone());
                if w.skip > 0 {
                    w.skip += 1;
                    w.stack.push(None);
                    continue;
                }
                let m = map(&name, &e, &w.stack);
                let close = match m {
                    Map::Tag(t) => {
                        let (open, close) = tag_pair(t, &e);
                        w.out.push_str(&open);
                        Some(close)
                    }
                    Map::Pass => None,
                    Map::Skip => {
                        w.skip = 1;
                        None
                    }
                    Map::Heading => {
                        let level = w.sections.clamp(1, 6);
                        w.out.push_str(&format!("<h{level}>"));
                        if level == 1 && w.title.is_none() {
                            w.heading_at = Some(w.out.len());
                        }
                        Some(format!("</h{level}>"))
                    }
                    Map::Section => {
                        w.sections += 1;
                        Some("\u{0}section".into())
                    }
                    Map::Pre => {
                        w.pre += 1;
                        let lang = attr(&e, "language").or_else(|| attr(&e, "lang")).filter(|l| l.chars().all(|c| c.is_ascii_alphanumeric() || "+-_#.".contains(c)) && !l.is_empty());
                        w.out.push_str(&lang.map_or("<pre>".to_string(), |l| format!("<pre><code class=\"language-{}\">", l.to_ascii_lowercase())));
                        Some(if w.out.ends_with("\">") { "</code></pre>".into() } else { "</pre>".into() })
                    }
                    Map::Note => {
                        w.out.push_str("<sup>[");
                        Some("]</sup>".into())
                    }
                    Map::Raw(html, close) => {
                        w.out.push_str(&html);
                        (!close.is_empty()).then(|| close.to_string())
                    }
                };
                w.stack.push(close);
            }
            Event::Empty(e) => {
                if w.skip > 0 {
                    continue;
                }
                let name = String::from_utf8_lossy(e.local_name().as_ref()).into_owned();
                if let Some(html) = empty(&name, &e) {
                    w.out.push_str(&html);
                }
            }
            Event::End(_) => {
                names.pop();
                if w.skip > 0 {
                    w.skip -= 1;
                    w.stack.pop();
                    continue;
                }
                match w.stack.pop().flatten() {
                    Some(c) if c == "\u{0}section" => w.sections = w.sections.saturating_sub(1),
                    Some(c) => {
                        if c.starts_with("</h") {
                            if let Some(at) = w.heading_at.take() {
                                w.title = Some(strip_tags(&w.out[at..]).trim().to_string()).filter(|t| !t.is_empty());
                            }
                        }
                        if c.ends_with("</pre>") {
                            w.pre = w.pre.saturating_sub(1);
                        }
                        w.out.push_str(&c);
                    }
                    None => {}
                }
            }
            Event::Text(t) if w.skip == 0 => {
                let s = unescape(&String::from_utf8_lossy(t.as_ref()));
                w.out.push_str(&esc(&s));
            }
            Event::CData(t) if w.skip == 0 => w.out.push_str(&esc(&String::from_utf8_lossy(t.as_ref()))),
            Event::GeneralRef(g) if w.skip == 0 => {
                let name = String::from_utf8_lossy(g.as_ref()).into_owned();
                w.out.push_str(&esc(&entity(&name).unwrap_or_else(|| format!("&{name};"))));
            }
            _ => {}
        }
    }
    Rendered { html: w.out, title: w.title, warnings }
}

/// `t`'s opening and closing HTML; `a` takes its target from the element.
fn tag_pair(t: &str, e: &BytesStart) -> (String, String) {
    match t {
        "a" => {
            let href = attr(e, "href").or_else(|| attr(e, "url")).or_else(|| attr(e, "target")).or_else(|| attr(e, "linkend").map(|l| format!("#{l}"))).unwrap_or_default();
            (format!("<a href=\"{}\">", esc(&href)), "</a>".into())
        }
        "q" => ("“".into(), "”".into()),
        t if t.contains("><") => {
            let tags: Vec<&str> = t.split("><").collect();
            (tags.iter().map(|x| format!("<{x}>")).collect(), tags.iter().rev().map(|x| format!("</{x}>")).collect())
        }
        t => (format!("<{t}>"), format!("</{t}>")),
    }
}

fn docbook(text: &str) -> Result<Rendered> {
    let mut r = walk(
        text,
        |name, e, stack| {
            let in_section_head = stack.last().is_some_and(|c| c.as_deref() == Some("\u{0}section"));
            match name {
                "book" | "article" | "chapter" | "section" | "sect1" | "sect2" | "sect3" | "sect4" | "sect5" | "simplesect" | "preface" | "appendix" | "part" | "refentry" | "refsect1" | "refsect2" | "refsect3" | "refsection" | "colophon" | "glossary" | "bibliography" | "index-skip" => Map::Section,
                "title" if in_section_head || stack.is_empty() => Map::Heading,
                "title" | "subtitle" | "titleabbrev" => Map::Tag("p><strong"),
                "info" | "articleinfo" | "bookinfo" | "chapterinfo" | "sectioninfo" | "sect1info" | "prefaceinfo" | "appendixinfo" => Map::Skip,
                "para" | "simpara" | "formalpara" | "attribution" | "refpurpose" | "caption" => Map::Tag("p"),
                "emphasis" => match attr(e, "role").as_deref() {
                    Some("bold" | "strong") => Map::Tag("strong"),
                    Some("underline") => Map::Tag("u"),
                    Some("strikethrough") => Map::Tag("s"),
                    _ => Map::Tag("em"),
                },
                "literal" | "code" | "command" | "filename" | "option" | "varname" | "function" | "userinput" | "computeroutput" | "envar" | "classname" | "methodname" | "parameter" | "constant" | "type" | "prompt" | "systemitem" | "markup" | "tag" | "keycap" | "uri" | "email" => Map::Tag("code"),
                "replaceable" | "citetitle" | "firstterm" | "glossterm" | "foreignphrase" | "wordasword" => Map::Tag("em"),
                "programlisting" | "screen" | "literallayout" | "synopsis" | "funcsynopsisinfo" | "address" => Map::Pre,
                "itemizedlist" | "simplelist" | "variablelist" | "segmentedlist" | "glosslist" | "calloutlist" => Map::Tag("ul"),
                "orderedlist" | "procedure" | "substeps" => Map::Tag("ol"),
                // A variablelist entry's definition is inside its item.
                "listitem" if stack.last().is_some_and(|c| c.as_deref() == Some("</li>")) => Map::Tag("blockquote"),
                "listitem" | "varlistentry" | "step" | "member" | "glossentry" | "callout" => Map::Tag("li"),
                "term" => Map::Tag("p><strong"),
                "ulink" | "link" => Map::Tag("a"),
                "blockquote" | "note" | "warning" | "tip" | "important" | "caution" | "sidebar" | "abstract" | "epigraph" | "example" | "informalexample" => Map::Tag("blockquote"),
                "footnote" => Map::Note,
                "quote" => Map::Tag("q"),
                "superscript" => Map::Tag("sup"),
                "subscript" => Map::Tag("sub"),
                "table" | "informaltable" => Map::Tag("table"),
                "thead" => Map::Tag("thead"),
                "row" | "tr" => Map::Tag("tr"),
                "entry" | "td" => Map::Tag("td"),
                "th" => Map::Tag("th"),
                "indexterm" | "remark" | "comment" | "colspec" | "spanspec" | "alt" | "textobject-skip" | "titleabbrev-skip" | "keywordset" | "subjectset" | "revhistory" | "toc" | "lot" | "index" => Map::Skip,
                _ => Map::Pass,
            }
        },
        |name, e| match name {
            "imagedata" | "graphic" => attr(e, "fileref").or_else(|| attr(e, "entityref")).map(|f| format!("<img src=\"{}\" alt=\"\">", esc(&f))),
            "xref" => attr(e, "linkend").map(|l| format!("[{}]", esc(&l))),
            "ulink" | "link" => attr(e, "url").or_else(|| attr(e, "href")).map(|u| format!("<a href=\"{0}\">{0}</a>", esc(&u))),
            "sbr" => Some("<br>".into()),
            _ => None,
        },
    );
    // A book's or article's title in its info.
    if r.title.is_none() {
        r.title = info_title(text);
        if let Some(t) = &r.title {
            r.html = format!("<h1>{}</h1>{}", esc(t), r.html);
        }
    }
    Ok(r)
}

/// The title inside a DocBook `<info>` (or `<articleinfo>`, `<bookinfo>`).
fn info_title(text: &str) -> Option<String> {
    let at = ["<info>", "<articleinfo>", "<bookinfo>", "<info "].iter().filter_map(|t| text.find(t)).min()?;
    let rest = &text[at..];
    let start = rest.find("<title")?;
    let open_end = rest[start..].find('>')? + start + 1;
    let end = rest[open_end..].find("</title>")? + open_end;
    Some(unescape(&strip_tags(&rest[open_end..end])).trim().to_string()).filter(|t| !t.is_empty())
}

fn tei(text: &str) -> Result<Rendered> {
    let title = tei_title(text);
    let mut r = walk(
        text,
        |name, e, _| {
            let rend = attr(e, "rend").unwrap_or_default();
            match name {
                "teiHeader" | "facsimile" | "standOff" | "fw" | "figDesc-skip" | "interpGrp" | "linkGrp" => Map::Skip,
                "div" | "div1" | "div2" | "div3" | "div4" | "div5" | "div6" | "div7" | "front" | "back" => Map::Section,
                "head" => Map::Heading,
                "p" | "ab" | "byline" | "dateline" | "salute" | "signed" | "trailer" | "opener" | "closer" | "docAuthor" | "docTitle" | "titlePart" | "docImprint" | "docDate" | "argument" | "epigraph-p" | "figDesc" | "bibl" | "label" | "sp" => Map::Tag("p"),
                "speaker" => Map::Tag("strong"),
                "hi" | "seg" => match rend.as_str() {
                    r if r.contains("bold") || r.contains("strong") => Map::Tag("strong"),
                    r if r.contains("italic") || r.contains("emph") => Map::Tag("em"),
                    r if r.contains("underline") => Map::Tag("u"),
                    r if r.contains("sup") => Map::Tag("sup"),
                    r if r.contains("sub") => Map::Tag("sub"),
                    r if r.contains("strike") || r.contains("line-through") => Map::Tag("s"),
                    r if r.contains("code") || r.contains("mono") => Map::Tag("code"),
                    _ if name == "hi" => Map::Tag("em"),
                    _ => Map::Pass,
                },
                "emph" | "foreign" | "term" | "mentioned" | "soCalled" | "title" | "gloss" => Map::Tag("em"),
                "code" | "ident" | "gi" | "att" | "val" | "tag" => Map::Tag("code"),
                "eg" | "egXML" => Map::Pre,
                "list" | "listBibl" => match attr(e, "type").or_else(|| attr(e, "rend")).as_deref() {
                    Some("ordered" | "numbered" | "decimal") => Map::Tag("ol"),
                    _ => Map::Tag("ul"),
                },
                "item" | "biblStruct" => Map::Tag("li"),
                "table" => Map::Tag("table"),
                "row" => Map::Tag("tr"),
                "cell" => Map::Tag("td"),
                "quote" | "cit" | "epigraph" | "lg" | "floatingText" => Map::Tag("blockquote"),
                "q" | "said" => Map::Tag("q"),
                "l" => Map::Tag("p"),
                "note" => Map::Note,
                "ref" | "ptr" => Map::Tag("a"),
                "del" | "orig" | "abbr-skip" | "sic-skip" => Map::Skip,
                _ => Map::Pass,
            }
        },
        |name, e| match name {
            "lb" => Some("<br>".into()),
            "graphic" | "media" => attr(e, "url").map(|u| format!("<img src=\"{}\" alt=\"\">", esc(&u))),
            "ptr" | "ref" => attr(e, "target").map(|u| format!("<a href=\"{0}\">{0}</a>", esc(&u))),
            "gap" => Some("[…]".into()),
            _ => None,
        },
    );
    if let Some(t) = title {
        if r.title.as_deref() != Some(t.as_str()) {
            r.html = format!("<h1>{}</h1>{}", esc(&t), r.html);
        }
        r.title = Some(t);
    }
    Ok(r)
}

/// A FictionBook: its title and author from `<title-info>`, then its bodies.
/// Images are base64 in `<binary>` elements, named here but not shown yet.
fn fb2(text: &str) -> Result<Rendered> {
    let find = |tag: &str| -> Option<String> {
        let info = &text[text.find("<title-info")?..];
        let info = &info[..info.find("</title-info>").unwrap_or(info.len())];
        let start = info.find(&format!("<{tag}"))?;
        let open_end = info[start..].find('>')? + start + 1;
        let end = info[open_end..].find(&format!("</{tag}>"))? + open_end;
        // Name parts (`<first-name>`, `<last-name>`) are words apart.
        Some(unescape(&strip_tags(&info[open_end..end].replace("><", "> <"))).split_whitespace().collect::<Vec<_>>().join(" ")).filter(|t| !t.is_empty())
    };
    let title = find("book-title");
    let author = find("author");
    let mut r = walk(
        text,
        |name, e, stack| match name {
            "description" | "binary" | "stylesheet" => Map::Skip,
            // A title's paragraphs are its lines.
            "p" if stack.last().is_some_and(|c| c.as_deref().is_some_and(|c| c.starts_with("</h"))) => Map::Pass,
            "section" => Map::Section,
            "title" => Map::Heading,
            "subtitle" | "text-author" => Map::Tag("p><strong"),
            "p" | "v" => Map::Tag("p"),
            "emphasis" => Map::Tag("em"),
            "strong" => Map::Tag("strong"),
            "strikethrough" => Map::Tag("s"),
            "sub" => Map::Tag("sub"),
            "sup" => Map::Tag("sup"),
            "code" => Map::Tag("code"),
            "a" => match attr(e, "href") {
                // A note's reference (`#n1`): shown as its label.
                Some(h) if h.starts_with('#') => Map::Tag("sup"),
                _ => Map::Tag("a"),
            },
            "cite" | "epigraph" | "poem" | "annotation" => Map::Tag("blockquote"),
            "stanza" => Map::Tag("div"),
            "table" => Map::Tag("table"),
            "tr" => Map::Tag("tr"),
            "td" => Map::Tag("td"),
            "th" => Map::Tag("th"),
            _ => Map::Pass,
        },
        |name, e| match name {
            "empty-line" => Some("<p></p>".into()),
            "image" => attr(e, "href").map(|h| format!("<p><em>[Image: {}]</em></p>", esc(h.trim_start_matches('#')))),
            _ => None,
        },
    );
    if let Some(t) = &title {
        let by = author.as_ref().map(|a| format!("<p><em>{}</em></p>", esc(a))).unwrap_or_default();
        // A body often opens with the book's title itself: shown once.
        let heading = if r.title.as_deref() == Some(t.as_str()) { String::new() } else { format!("<h1>{}</h1>", esc(t)) };
        r.html = format!("{heading}{by}{}", r.html);
        r.title = Some(t.clone());
    }
    Ok(r)
}

/// The title in a TEI header's `<titleStmt>`.
fn tei_title(text: &str) -> Option<String> {
    let stmt = text.find("<titleStmt")?;
    let rest = &text[stmt..];
    let end_stmt = rest.find("</titleStmt>").unwrap_or(rest.len());
    let rest = &rest[..end_stmt];
    let start = rest.find("<title")?;
    let open_end = rest[start..].find('>')? + start + 1;
    let end = rest[open_end..].find("</title>")? + open_end;
    Some(unescape(&strip_tags(&rest[open_end..end])).trim().to_string()).filter(|t| !t.is_empty() && !t.starts_with('$'))
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
        has(&s, &["Heading { level: 1, text: [Text(\"War & Peace\")]", "Leo Tolstoy", "Book One", "Heading { level: 1, text: [Text(\"Chapter 1\")]", "Emphasis([Text(\"Prince\")])", "Strong([Text(\"Genoa\")])", "Superscript", "Quote", "A line", "[Image: pic.jpg]", "A note."]);
        assert!(!s.contains("/9j/"), "{s}");
        assert!(doc.metadata().iter().any(|(k, v)| k == "Title" && v == "War & Peace"));
    }

    #[test]
    fn a_presentation() {
        let content = r#"<office:document-content xmlns:office="o" xmlns:draw="d" xmlns:text="t" xmlns:presentation="p" xmlns:xlink="x" xmlns:table="tb"><office:automatic-styles><style>hidden</style></office:automatic-styles><office:body><office:presentation>
<draw:page draw:name="page1"><draw:frame presentation:class="title"><draw:text-box><text:p>Welcome</text:p></draw:text-box></draw:frame><draw:frame><draw:text-box><text:list><text:list-item><text:p>First <text:span>point</text:span></text:p></text:list-item></text:list></draw:text-box></draw:frame><draw:frame><draw:image xlink:href="Pictures/a.png"/></draw:frame><presentation:notes><draw:page-thumbnail/><draw:frame><draw:text-box><text:p>Say hello.</text:p></draw:text-box></draw:frame></presentation:notes></draw:page>
<draw:page draw:name="Overview"><draw:frame><table:table><table:table-row><table:table-cell><text:p>a</text:p></table:table-cell></table:table-row></table:table></draw:frame><presentation:notes><draw:page-thumbnail/></presentation:notes></draw:page>
</office:presentation></office:body></office:document-content>"#;
        let (r, n) = odp(content);
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
