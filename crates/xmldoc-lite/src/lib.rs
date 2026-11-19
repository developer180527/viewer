//! Documents written in XML vocabularies, as HTML: a small, embeddable reader of each.
//!
//! - **DocBook** 4 and 5 ([`docbook`]): articles, books and chapters, in XML or in SGML
//!   (unclosed `<para>`s read as a browser would read them).
//! - **TEI** P5 ([`tei`]): the Text Encoding Initiative's texts, its header's title.
//! - **FictionBook** 2 ([`fictionbook`]): its title, author, sections and poems.
//! - **OpenDocument presentations** ([`presentation`]): an ODP's `content.xml`, a
//!   heading per slide, its text, tables and images, its speaker notes quoted.
//!
//! Each element maps to the HTML it means, read as a stream (quick-xml), so a
//! document's size, not its depth, is what it costs. Nothing is fetched or run: a DTD
//! isn't loaded, XIncludes and external entities aren't followed. Damaged XML shows
//! what came before the damage, with a warning.
//!
//! ```
//! let doc = xmldoc_lite::docbook(r#"<article xmlns="http://docbook.org/ns/docbook"><info><title>Notes</title></info><para>Hello, <emphasis>you</emphasis>.</para></article>"#);
//! assert_eq!(doc.title.as_deref(), Some("Notes"));
//! assert!(doc.html.contains("<p>Hello, <em>you</em>.</p>"));
//! assert!(xmldoc_lite::is_docbook(br#"<article xmlns="http://docbook.org/ns/docbook">"#));
//! ```

use quick_xml::events::{BytesStart, Event};

/// A document as HTML, its title, and what couldn't be read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Html {
    pub html: String,
    pub title: Option<String>,
    pub warnings: Vec<String>,
}

/// A presentation as HTML, and its number of slides.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Presentation {
    pub html: Html,
    pub slides: usize,
}

/// A DocBook document, by its root element (`<book>`, `<article>`, `<chapter>`…) in
/// DocBook's namespace, or under a DocBook DTD (4.x, and SGML).
pub fn is_docbook(header: &[u8]) -> bool {
    root(header).is_some_and(|(r, ns)| docbook_root(&r, &ns, header))
}

/// A TEI document, by its root: `<TEI>`, P4's `<TEI.2>`, or `<teiCorpus>`.
pub fn is_tei(header: &[u8]) -> bool {
    root(header).is_some_and(|(r, _)| r == "TEI" || r == "TEI.2" || r == "teiCorpus")
}

/// A FictionBook, by its root, `<FictionBook>`.
pub fn is_fictionbook(header: &[u8]) -> bool {
    root(header).is_some_and(|(r, _)| r == "FictionBook")
}

fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

fn strip_tags(html: &str) -> String {
    let mut out = String::new();
    let mut tag = false;
    for c in html.chars() {
        match c {
            '<' => tag = true,
            '>' => tag = false,
            c if !tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&amp;", "&")
}

/// The text of the first `<tag>…</tag>` in `xml`.
pub fn element_text(xml: &str, tag: &str) -> Option<String> {
    let start = xml.find(&format!("<{tag}"))?;
    let open_end = xml[start..].find('>')? + start + 1;
    let end = xml[open_end..].find(&format!("</{tag}>"))? + open_end;
    Some(unescape(&strip_tags(&xml[open_end..end])).trim().to_string()).filter(|t| !t.is_empty())
}

/// A presentation's `content.xml` as HTML: a heading per slide (its title, or its
/// number), then its frames; its speaker notes quoted after it. And the slide count.
pub fn presentation(text: &str) -> Presentation {
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
    Presentation { html: r, slides: slides.get() }
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

fn walk(text: &str, map: impl Fn(&str, &BytesStart, &[Option<String>]) -> Map, empty: impl Fn(&str, &BytesStart) -> Option<String>) -> Html {
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
                    // A paragraph inside inline markup (a heading, a mark, a note): its
                    // text, a word apart, since HTML has no paragraph there.
                    Map::Tag(t) if (t == "p" || t.starts_with("p>")) && in_inline(&w.stack) => {
                        if !w.out.ends_with(['>', ' ', '[']) {
                            w.out.push(' ');
                        }
                        None
                    }
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
                        if c.starts_with("</h")
                            && let Some(at) = w.heading_at.take() {
                                w.title = Some(strip_tags(&w.out[at..]).trim().to_string()).filter(|t| !t.is_empty());
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
    Html { html: w.out, title: w.title, warnings }
}

/// The nearest element written so far is inline: what it closes with says so.
fn in_inline(stack: &[Option<String>]) -> bool {
    stack.iter().rev().find_map(|c| c.as_deref()).is_some_and(|c| {
        (c.starts_with("</h") && c.len() == 5) || ["</strong", "</em>", "</sup>", "]</sup>", "</sub>", "</a>", "</code>", "</u>", "</s>", "”"].iter().any(|i| c.starts_with(i))
    })
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

pub fn docbook(text: &str) -> Html {
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
    r
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

pub fn tei(text: &str) -> Html {
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
    r
}

/// A FictionBook: its title and author from `<title-info>`, then its bodies.
/// Its pictures are `<img>`s by id; their bytes are [`fictionbook_images`]'s.
pub fn fictionbook(text: &str) -> Html {
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
        |name, e, _| match name {
            "description" | "binary" | "stylesheet" => Map::Skip,
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
            // A picture the book carries (`#id`, see [`fictionbook_images`]), by its id.
            "image" => attr(e, "href").map(|h| format!("<img src=\"{}\" alt=\"{}\">", esc(h.trim_start_matches('#')), esc(&attr(e, "alt").unwrap_or_default()))),
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
    r
}

/// A picture a FictionBook carries: its id (what `<image l:href="#id">` names, and
/// [`fictionbook`]'s `<img src>`), its content type, and its decoded bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub id: String,
    pub content_type: String,
    pub bytes: Vec<u8>,
}

/// The pictures in a FictionBook's `<binary>` elements (base64), in order. One that
/// doesn't decode is left out.
pub fn fictionbook_images(text: &str) -> Vec<Image> {
    let mut out = Vec::new();
    let mut r = quick_xml::Reader::from_str(text);
    r.config_mut().check_end_names = false;
    let mut current: Option<(String, String, String)> = None;
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) if e.local_name().as_ref() == b"binary" => {
                current = Some((attr(&e, "id").unwrap_or_default(), attr(&e, "content-type").unwrap_or_default(), String::new()));
            }
            Ok(Event::Text(t)) => {
                if let Some((_, _, data)) = current.as_mut() {
                    data.push_str(&String::from_utf8_lossy(t.as_ref()));
                }
            }
            Ok(Event::End(e)) if e.local_name().as_ref() == b"binary" => {
                if let Some((id, content_type, data)) = current.take()
                    && let Some(bytes) = base64(&data)
                    && !id.is_empty()
                {
                    out.push(Image { id, content_type, bytes });
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

/// Standard base64, white space ignored; `None` when it isn't.
fn base64(text: &str) -> Option<Vec<u8>> {
    let value = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        } as u32)
    };
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0);
    for c in text.bytes().filter(|c| !c.is_ascii_whitespace()) {
        if c == b'=' {
            break;
        }
        acc = (acc << 6) | value(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
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
