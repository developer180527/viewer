//! Mail as documents: a message (`.eml`, RFC 5322 and MIME), a mailbox of them
//! (`.mbox`), a web page saved with its resources (`.mht`, MIME's multipart/related),
//! all through Stalwart's `mail-parser`; and Outlook's `.msg` through `msg_parser`.
//!
//! A message reads as its headers (From, To, Date, Subject…), then its body (the HTML
//! part where there is one, through `crate::html`, else the text with its quoting
//! kept), then its attachments by name, type and size, with a forwarded message
//! nested inside. Nothing in a message is run or fetched: remote images aren't loaded.

use std::sync::Arc;

use libfileparser_core::{Category, Column, Confidence, Content, Detection, Document, FileSource, FormatInfo, OwnedFlow, OwnedTable, ParseContext, ParseError, Reader, ReaderInfo, Result, Role, ValueKind, Warning, is_text};
use libfileparser_core::Cell as Value;
use mail_parser::{MessageParser, MessageRef};

use crate::build::Builder;
use crate::markup::esc;

static EML: ReaderInfo = ReaderInfo::new("eml", "Email message", Category::Docs).extensions(&["eml", "email", "msg.txt", "mime", "emlx"]).mime_types(&["message/rfc822"]);
static MBOX: ReaderInfo = ReaderInfo::new("mbox", "Mailbox", Category::Docs).extensions(&["mbox", "mbx", "mbs"]).mime_types(&["application/mbox"]);
static MHT: ReaderInfo = ReaderInfo::new("mhtml", "Web archive", Category::Docs).extensions(&["mht", "mhtml"]).mime_types(&["multipart/related", "application/x-mimearchive"]);
static MSG: ReaderInfo = ReaderInfo::new("msg", "Outlook message", Category::Docs).extensions(&["msg", "oft"]).mime_types(&["application/vnd.ms-outlook"]);

pub struct EmlReader;
pub struct MboxReader;
pub struct MhtReader;
pub struct MsgReader;

/// Headers a message starts with, any of which marks it as one.
const HEADERS: &[&str] = &["received:", "return-path:", "from:", "to:", "date:", "subject:", "message-id:", "mime-version:", "delivered-to:", "x-mailer:", "reply-to:", "content-type:", "x-original-to:", "dkim-signature:", "arc-seal:", "authentication-results:", "sender:", "cc:", "in-reply-to:", "references:", "thread-topic:", "x-received:", "x-google-smtp-source:", "list-id:", "user-agent:"];

/// The well-known headers among a message's first lines (unfolded lines only), each
/// by its lower-case name with the colon.
fn known_headers_in(header: &[u8]) -> Vec<&'static str> {
    let head = String::from_utf8_lossy(&header[..header.len().min(4096)]);
    head.lines()
        .take_while(|l| !l.trim().is_empty())
        .filter(|l| !l.starts_with([' ', '\t']))
        .filter_map(|l| {
            let lower = l.to_ascii_lowercase();
            HEADERS.iter().find(|h| lower.starts_with(*h)).copied()
        })
        .collect()
}

/// How many of a message's first lines are well-known headers.
fn known_headers(header: &[u8]) -> usize {
    known_headers_in(header).len()
}

/// Headers a message has and a list of `Name: value` lines doesn't: the trail of its
/// delivery, or a sender with a date, subject or MIME header. A flat YAML file of
/// translations (`Cc: …`, `To: …`) or settings (`from: markdown`, `to: html`) has two
/// known names but neither.
fn looks_like_mail(header: &[u8]) -> bool {
    let found = known_headers_in(header);
    let has = |h: &str| found.contains(&h);
    let delivered = has("received:") || has("message-id:") || has("return-path:") || has("delivered-to:") || has("dkim-signature:");
    let sent = has("from:") && (has("date:") || has("subject:") || has("mime-version:"));
    found.len() >= 2 && (delivered || sent)
}

impl Reader for EmlReader {
    fn info(&self) -> &ReaderInfo {
        &EML
    }

    /// Two or more well-known headers at the start, among them a delivery header
    /// (`Received:`, `Message-ID:`…) or a sender with a date or subject; or its extension.
    fn detect(&self, header: &[u8], name: &str) -> Detection {
        // Not by being text: real mail carries raw 8-bit headers and UTF-16 parts.
        if looks_like_mail(header) && !header.starts_with(b"From ") {
            Detection::new(Confidence::Content).with_language("email")
        } else if EML.matches_extension(name) {
            Detection::new(Confidence::Extension).with_language("email")
        } else {
            Detection::NONE
        }
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        cx.cancel.check()?;
        let bytes = crate::whole(src, cx)?;
        let message = MessageParser::default().parse(&bytes[..]).ok_or_else(|| ParseError::invalid("not an email message"))?;
        let m = message.root();
        if m.headers().is_empty() {
            return Err(ParseError::NotThisFormat);
        }
        let mut b = Builder::new(cx);
        let mut html = String::new();
        message_html(m, &mut html, 0);
        crate::html::parse(&html, src.name(), &mut b)?;
        let flow = with_parts(OwnedFlow::new(b.finish()?), m, cx)?;
        let mut doc = Document::new(FormatInfo::new("eml", "Email message", Category::Docs)).with_facet(Role::Primary, "Message", Content::Flow(Arc::new(flow)));
        doc = with_message_facets(doc, m);
        Ok(doc.with_file_facets(src, Some("email")))
    }
}

/// A message's Headers and Attachments tables, and its metadata.
fn with_message_facets(mut doc: Document, m: MessageRef) -> Document {
    let headers: Vec<Vec<Value>> = m.headers().iter().map(|h| vec![Value::Text(h.raw_name().to_string()), Value::Text(header_text(&h))]).collect();
    doc = doc.with_facet(Role::Structure, "Headers", Content::Table(Arc::new(OwnedTable::new(vec![Column::new("Name"), Column::new("Value")], headers))));
    let attachments: Vec<Vec<Value>> = m
        .attachments()
        .map(|a| {
            let kind = a.content_type().map(|c| format!("{}/{}", c.ctype(), c.subtype().unwrap_or(""))).unwrap_or_default();
            vec![Value::Text(a.attachment_name().unwrap_or("(unnamed)").to_string()), Value::Text(kind), Value::Int(a.decoded_len() as i64)]
        })
        .collect();
    if !attachments.is_empty() {
        let n = attachments.len();
        let cols = vec![Column::new("Name"), Column::new("Type"), Column::new("Size").with_kind(ValueKind::Number)];
        doc = doc.with_facet(Role::Structure, "Attachments", Content::Table(Arc::new(OwnedTable::new(cols, attachments)))).with_meta("Attachments", n);
    }
    if let Some(s) = m.subject() {
        doc = doc.with_meta("Title", s);
    }
    if let Some(f) = m.from() {
        doc = doc.with_meta("From", addresses(&f));
    }
    if let Some(d) = m.date() {
        doc = doc.with_meta("Date", d.to_rfc3339());
    }
    doc
}

/// `flow` serving the message's pictures as its HTML names them: an inline one by
/// `cid:` and its Content-ID, a saved page's by its `Content-Location`.
fn with_parts(mut flow: OwnedFlow, m: MessageRef, cx: &ParseContext) -> Result<OwnedFlow> {
    for (n, a) in m.attachments().enumerate() {
        let Some(ct) = a.content_type() else { continue };
        if !ct.ctype().eq_ignore_ascii_case("image") {
            continue;
        }
        let (cid, location) = (a.content_id().map(|c| c.trim_matches(['<', '>']).to_string()), a.content_location().map(str::to_string));
        if cid.is_none() && location.is_none() {
            continue;
        }
        let bytes = a.decoded();
        cx.limits.check(libfileparser_core::Limit::Allocation, bytes.len() as u64)?;
        let name = a.attachment_name().map(str::to_string).or_else(|| location.as_deref().and_then(|l| l.rsplit('/').next()).filter(|s| !s.is_empty()).map(str::to_string)).unwrap_or_else(|| format!("part-{}.{}", n + 1, ct.subtype().unwrap_or("bin")));
        let file = FileSource::from_bytes(name, bytes.into_owned());
        if let Some(c) = cid {
            flow = flow.with_resource(format!("cid:{c}"), file.clone());
        }
        if let Some(l) = location {
            flow = flow.with_resource(l, file);
        }
    }
    Ok(flow)
}

/// A header's value as text: decoded where mail-parser understands it, raw otherwise.
fn header_text(h: &mail_parser::Header) -> String {
    let v = h.value();
    if let Some(t) = v.as_text() {
        return t.to_string();
    }
    if let Some(a) = v.as_address() {
        return addresses(&a);
    }
    if let Some(d) = v.as_datetime() {
        return d.to_rfc3339();
    }
    if let Some(l) = v.as_text_list() {
        return l.iter().collect::<Vec<_>>().join(", ");
    }
    String::from_utf8_lossy(h.raw_value()).split_whitespace().collect::<Vec<_>>().join(" ")
}

fn addresses(list: &mail_parser::AddressList) -> String {
    list.mailboxes()
        .map(|m| match (m.name(), m.address()) {
            (Some(n), Some(a)) => format!("{n} <{a}>"),
            (Some(n), None) => n.to_string(),
            (None, Some(a)) => a.to_string(),
            (None, None) => String::new(),
        })
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Addresses as HTML, each linked to write to it.
fn addresses_html(list: &mail_parser::AddressList) -> String {
    list.mailboxes()
        .filter_map(|m| {
            let a = m.address()?;
            Some(match m.name() {
                Some(n) => format!("{} &lt;<a href=\"mailto:{1}\">{1}</a>&gt;", esc(n), esc(a)),
                None => format!("<a href=\"mailto:{0}\">{0}</a>", esc(a)),
            })
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Plain text as HTML: paragraphs at blank lines, line breaks kept (mail is written
/// in lines), `>` quoting as quotes, web addresses as links.
pub(crate) fn text_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 64);
    let text = text.replace("\r\n", "\n");
    let mut quoted: Vec<String> = Vec::new();
    let mut para: Vec<String> = Vec::new();
    let flush_para = |para: &mut Vec<String>, out: &mut String| {
        if !para.is_empty() {
            out.push_str(&format!("<p>{}</p>", para.join("<br>")));
            para.clear();
        }
    };
    let flush_quote = |quoted: &mut Vec<String>, out: &mut String| {
        if !quoted.is_empty() {
            let inner = quoted.join("\n");
            out.push_str(&format!("<blockquote>{}</blockquote>", text_html(&inner)));
            quoted.clear();
        }
    };
    for line in text.lines() {
        if let Some(q) = line.strip_prefix('>') {
            flush_para(&mut para, &mut out);
            quoted.push(q.strip_prefix(' ').unwrap_or(q).to_string());
            continue;
        }
        flush_quote(&mut quoted, &mut out);
        if line == "-- " {
            // A signature follows.
            flush_para(&mut para, &mut out);
            out.push_str("<hr>");
        } else if line.trim().is_empty() {
            flush_para(&mut para, &mut out);
        } else {
            para.push(markup_lite::linkify(line));
        }
    }
    flush_quote(&mut quoted, &mut out);
    flush_para(&mut para, &mut out);
    out
}

/// One message as HTML: headers, body, attachments; nested messages inside, to a depth.
fn message_html(m: MessageRef, out: &mut String, depth: usize) {
    out.push_str("<table>");
    let row = |out: &mut String, k: &str, v: String| {
        if !v.is_empty() {
            out.push_str(&format!("<tr><th>{k}</th><td>{v}</td></tr>"));
        }
    };
    if let Some(f) = m.from() {
        row(out, "From", addresses_html(&f));
    }
    if let Some(t) = m.to() {
        row(out, "To", addresses_html(&t));
    }
    if let Some(c) = m.cc() {
        row(out, "Cc", addresses_html(&c));
    }
    if let Some(d) = m.date() {
        row(out, "Date", esc(&d.to_rfc822()));
    }
    if let Some(s) = m.subject() {
        row(out, "Subject", format!("<strong>{}</strong>", esc(s)));
    }
    out.push_str("</table>");
    // The HTML body where the message has one: it's what the sender wrote.
    let html = m.html_body().any(|p| p.is_content_type("text", "html"));
    if html {
        for body in m.html_bodies() {
            out.push_str("<div>");
            out.push_str(&body);
            out.push_str("</div>");
        }
    } else {
        for body in m.text_bodies() {
            out.push_str(&text_html(&body));
        }
    }
    let mut listed = Vec::new();
    for a in m.attachments() {
        if let Some(nested) = a.nested() {
            if depth < 8 {
                out.push_str("<blockquote><p><em>Forwarded message</em></p>");
                message_html(nested, out, depth + 1);
                out.push_str("</blockquote>");
            }
            continue;
        }
        let kind = a.content_type().map(|c| format!("{}/{}", c.ctype(), c.subtype().unwrap_or(""))).unwrap_or_default();
        listed.push(format!("<li>{} <em>({}, {})</em></li>", esc(a.attachment_name().unwrap_or("(unnamed)")), esc(&kind), size(a.decoded_len())));
    }
    if !listed.is_empty() {
        out.push_str(&format!("<p><strong>Attachments</strong></p><ul>{}</ul>", listed.join("")));
    }
}

fn size(n: usize) -> String {
    match n {
        n if n < 1024 => format!("{n} bytes"),
        n if n < 1024 * 1024 => format!("{:.1} KB", n as f64 / 1024.0),
        n => format!("{:.1} MB", n as f64 / (1024.0 * 1024.0)),
    }
}

// ---------------------------------------------------------------- mailboxes

/// Messages a mailbox shows in full; past it, they're in the Messages table only.
const SHOWN: usize = 500;

impl Reader for MboxReader {
    fn info(&self) -> &ReaderInfo {
        &MBOX
    }

    /// A `From ` separator line first, with a message's headers after it.
    fn detect(&self, header: &[u8], name: &str) -> Detection {
        if header.starts_with(b"From ") {
            let after = header.iter().position(|&b| b == b'\n').map_or(&header[..0], |n| &header[n + 1..]);
            if known_headers(after) >= 1 {
                return Detection::new(Confidence::Content).with_language("email");
            }
        }
        if MBOX.matches_extension(name) && (is_text(header) || header.is_ascii()) { Detection::new(Confidence::Extension).with_language("email") } else { Detection::NONE }
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        cx.cancel.check()?;
        let bytes = crate::whole(src, cx)?;
        let parser = MessageParser::default();
        let mut b = Builder::new(cx);
        let mut rows: Vec<Vec<Value>> = Vec::new();
        let mut shown = 0usize;
        for (n, item) in mail_parser::mailbox::mbox::MessageIterator::new(&bytes[..]).enumerate() {
            if n % 64 == 0 {
                cx.cancel.check()?;
            }
            let Ok(item) = item else { break };
            let Some(message) = parser.parse(item.contents()) else { continue };
            let m = message.root();
            let subject = m.subject().unwrap_or("(no subject)").to_string();
            let from = m.from().map(|f| addresses(&f)).unwrap_or_else(|| item.from().to_string());
            let date = m.date().map(|d| d.to_rfc3339()).unwrap_or_default();
            rows.push(vec![Value::Int(n as i64 + 1), if date.is_empty() { Value::Null } else { Value::Date(date) }, Value::Text(from), Value::Text(subject.clone()), Value::Int(item.contents().len() as i64)]);
            if shown < SHOWN {
                shown += 1;
                let mut html = format!("<h2>{}</h2>", esc(&subject));
                message_html(m, &mut html, 0);
                if n > 0 {
                    b.block(libfileparser_core::Block::Rule)?;
                }
                crate::html::parse(&html, src.name(), &mut b)?;
            }
        }
        if rows.is_empty() {
            return Err(ParseError::invalid("a mailbox with no messages"));
        }
        let count = rows.len();
        let cols = vec![Column::new("#").with_kind(ValueKind::Number), Column::new("Date").with_kind(ValueKind::Date), Column::new("From"), Column::new("Subject"), Column::new("Size").with_kind(ValueKind::Number)];
        let mut doc = Document::new(FormatInfo::new("mbox", "Mailbox", Category::Docs))
            .with_facet(Role::Primary, "Mailbox", Content::Flow(Arc::new(OwnedFlow::new(b.finish()?))))
            .with_facet(Role::Structure, "Messages", Content::Table(Arc::new(OwnedTable::new(cols, rows))))
            .with_meta("Messages", count);
        if count > SHOWN {
            doc = doc.with_warning(Warning::new(format!("The first {SHOWN} of {count} messages are shown in full; all are in Messages.")));
        }
        Ok(doc.with_file_facets(src, Some("email")))
    }
}

// ---------------------------------------------------------------- web archives

impl Reader for MhtReader {
    fn info(&self) -> &ReaderInfo {
        &MHT
    }

    /// Its extension, with a MIME header; or MIME headers naming multipart/related
    /// with an HTML root.
    fn detect(&self, header: &[u8], name: &str) -> Detection {
        let head = String::from_utf8_lossy(&header[..header.len().min(4096)]).to_ascii_lowercase();
        let related = head.contains("multipart/related") && (head.contains("text/html") || head.contains("snapshot-content-location") || head.contains("saved by"));
        let named = MHT.matches_extension(name) && (related || head.contains("mime-version") || head.contains("content-type"));
        // Unnamed, a saved page's headers: multipart/related, and not a message's From.
        let saved = related && known_headers(header) >= 1 && head.contains("mime-version") && !head.contains("\nfrom:") && !head.starts_with("from:");
        if named || saved {
            Detection::new(Confidence::Content).with_language("email")
        } else {
            Detection::NONE
        }
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        cx.cancel.check()?;
        let bytes = crate::whole(src, cx)?;
        let message = MessageParser::default().parse(&bytes[..]).ok_or_else(|| ParseError::invalid("not a MIME web archive"))?;
        let m = message.root();
        // The page: the first HTML part (the archive's root).
        let html = m.html_bodies().next().map(|h| h.into_owned()).or_else(|| m.attachments().find(|a| a.is_content_type("text", "html")).and_then(|a| a.text().map(|t| t.into_owned()))).ok_or_else(|| ParseError::invalid("a web archive with no page in it"))?;
        let mut b = Builder::new(cx);
        crate::html::parse(&html, src.name(), &mut b)?;
        let resources: Vec<Vec<Value>> = m
            .attachments()
            .map(|a| {
                let kind = a.content_type().map(|c| format!("{}/{}", c.ctype(), c.subtype().unwrap_or(""))).unwrap_or_default();
                vec![Value::Text(a.content_location().or(a.attachment_name()).unwrap_or("").to_string()), Value::Text(kind), Value::Int(a.decoded_len() as i64)]
            })
            .collect();
        let n = resources.len();
        let flow = with_parts(OwnedFlow::new(b.finish()?), m, cx)?;
        let mut doc = Document::new(FormatInfo::new("mhtml", "Web archive", Category::Docs)).with_facet(Role::Primary, "Page", Content::Flow(Arc::new(flow)));
        if n > 0 {
            let cols = vec![Column::new("Location"), Column::new("Type"), Column::new("Size").with_kind(ValueKind::Number)];
            doc = doc.with_facet(Role::Structure, "Resources", Content::Table(Arc::new(OwnedTable::new(cols, resources)))).with_meta("Resources", n);
        }
        if let Some(s) = m.subject().filter(|s| !s.trim().is_empty()) {
            doc = doc.with_meta("Title", s);
        }
        if let Some(loc) = m.headers().get("Snapshot-Content-Location").map(|h| header_text(&h)) {
            doc = doc.with_meta("Saved from", loc);
        }
        if let Some(d) = m.date() {
            doc = doc.with_meta("Saved", d.to_rfc3339());
        }
        Ok(doc.with_file_facets(src, Some("email")))
    }
}

// ---------------------------------------------------------------- Outlook

const OLE: &[u8] = &[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

impl Reader for MsgReader {
    fn info(&self) -> &ReaderInfo {
        &MSG
    }

    /// OLE's signature with its extension: Word and Excel files share the signature.
    fn detect(&self, header: &[u8], name: &str) -> Detection {
        if header.starts_with(OLE) && MSG.matches_extension(name) { Detection::new(Confidence::Certain) } else { Detection::NONE }
    }

    /// msg_parser panics on some damaged files (found by fuzzing: an overflow, and
    /// strings sliced inside a character), so its work is guarded here and a panic is
    /// a damaged file, whoever calls.
    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| open_msg(src, cx))).unwrap_or_else(|_| Err(ParseError::corrupt("not a readable Outlook message: msg_parser panicked on it")))
    }
}

fn open_msg(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    {
        cx.cancel.check()?;
        if !src.header().starts_with(OLE) {
            return Err(ParseError::NotThisFormat);
        }
        let bytes = crate::whole(src, cx)?;
        let o = msg_parser::Outlook::from_slice(&bytes[..]).map_err(|e| ParseError::corrupt(format!("not an Outlook message: {e}")))?;
        if o.subject.is_empty() && o.body.is_empty() && o.html.is_empty() && o.sender.email.to_string().is_empty() && o.message_class.is_empty() {
            return Err(ParseError::NotThisFormat);
        }
        let mut html = msg_html(&o, 0);
        if html.is_empty() {
            html = "<p></p>".into();
        }
        let mut b = Builder::new(cx);
        crate::html::parse(&html, src.name(), &mut b)?;
        // Inline pictures, which its HTML names `cid:…`.
        let flow = o.attachments.iter().filter(|a| !a.content_id.is_empty() && !a.payload_bytes.is_empty()).fold(OwnedFlow::new(b.finish()?), |f, a| {
            f.with_resource(format!("cid:{}", a.content_id.trim_matches(['<', '>'])), FileSource::from_bytes(attachment_name(a), a.payload_bytes.clone()))
        });
        let mut doc = Document::new(FormatInfo::new("msg", "Outlook message", Category::Docs)).with_facet(Role::Primary, "Message", Content::Flow(Arc::new(flow)));
        let attachments: Vec<Vec<Value>> = o.attachments.iter().map(|a| vec![Value::Text(attachment_name(a)), Value::Text(a.mime_tag.clone()), Value::Int(a.payload_bytes.len() as i64)]).collect();
        if !attachments.is_empty() {
            let n = attachments.len();
            let cols = vec![Column::new("Name"), Column::new("Type"), Column::new("Size").with_kind(ValueKind::Number)];
            doc = doc.with_facet(Role::Structure, "Attachments", Content::Table(Arc::new(OwnedTable::new(cols, attachments)))).with_meta("Attachments", n);
        }
        if !o.headers.raw.trim().is_empty() {
            let rows: Vec<Vec<Value>> = unfold(&o.headers.raw).into_iter().map(|(k, v)| vec![Value::Text(k), Value::Text(v)]).collect();
            doc = doc.with_facet(Role::Structure, "Headers", Content::Table(Arc::new(OwnedTable::new(vec![Column::new("Name"), Column::new("Value")], rows))));
        }
        for (k, v) in [("Title", o.subject.clone()), ("From", person(&o.sender)), ("Date", o.message_delivery_time.clone()), ("Class", o.message_class.clone())] {
            if !v.trim().is_empty() {
                doc = doc.with_meta(k, v);
            }
        }
        Ok(doc.with_file_facets(src, None))
    }
}

fn person(p: &msg_parser::Person) -> String {
    let (name, email) = (p.name.to_string(), p.email.to_string());
    match (name.trim().is_empty(), email.trim().is_empty()) {
        (false, false) if name != email => format!("{name} <{email}>"),
        (false, _) => name,
        (true, false) => email,
        _ => String::new(),
    }
}

fn attachment_name(a: &msg_parser::Attachment) -> String {
    [&a.long_file_name, &a.file_name, &a.display_name].into_iter().find(|s| !s.trim().is_empty()).cloned().unwrap_or_else(|| "(unnamed)".into())
}

/// Transport headers as (name, value), continuation lines unfolded.
fn unfold(raw: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in raw.lines() {
        if line.starts_with([' ', '\t']) {
            if let Some(last) = out.last_mut() {
                last.1.push(' ');
                last.1.push_str(line.trim());
            }
        } else if let Some((k, v)) = line.split_once(':') {
            out.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    out
}

fn msg_html(o: &msg_parser::Outlook, depth: usize) -> String {
    let mut out = String::from("<table>");
    let list = |people: &[msg_parser::Person]| people.iter().map(person).filter(|s| !s.is_empty()).map(|s| esc(&s)).collect::<Vec<_>>().join(", ");
    for (k, v) in [("From", esc(&person(&o.sender))), ("To", list(&o.to)), ("Cc", list(&o.cc)), ("Date", esc(&o.message_delivery_time)), ("Subject", if o.subject.is_empty() { String::new() } else { format!("<strong>{}</strong>", esc(&o.subject)) })] {
        if !v.trim().is_empty() {
            out.push_str(&format!("<tr><th>{k}</th><td>{v}</td></tr>"));
        }
    }
    out.push_str("</table>");
    if !o.html.trim().is_empty() {
        out.push_str(&o.html);
    } else if !o.body.trim().is_empty() {
        out.push_str(&text_html(&o.body));
    } else if let Some(h) = o.html_from_rtf() {
        out.push_str(&h);
    }
    let mut listed = Vec::new();
    for a in &o.attachments {
        if a.is_embedded_message() && depth < 8
            && let Some(Ok(nested)) = a.as_message() {
                out.push_str(&format!("<blockquote><p><em>Attached message</em></p>{}</blockquote>", msg_html(&nested, depth + 1)));
                continue;
            }
        listed.push(format!("<li>{} <em>({}{})</em></li>", esc(&attachment_name(a)), if a.mime_tag.is_empty() { String::new() } else { format!("{}, ", esc(&a.mime_tag)) }, size(a.payload_bytes.len())));
    }
    if !listed.is_empty() {
        out.push_str(&format!("<p><strong>Attachments</strong></p><ul>{}</ul>", listed.join("")));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flow(doc: &Document) -> String {
        let Content::Flow(f) = doc.facet(0, &ParseContext::new()).unwrap() else { panic!() };
        format!("{:?}", f.blocks(0..1000).unwrap())
    }

    fn has(s: &str, wants: &[&str]) {
        for w in wants {
            assert!(s.contains(w), "{w} in {s}");
        }
    }

    const EMAIL: &[u8] = b"Received: from mx.example.com by mail.example.org; Sat, 20 Nov 2021 14:22:05 -0800\r\nFrom: Art Vandelay <art@vandelay.com>\r\nTo: jane@example.com\r\nSubject: =?utf-8?q?Caf=C3=A9_plans?=\r\nDate: Sat, 20 Nov 2021 14:22:01 -0800\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"b1\"\r\n\r\n--b1\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nHi Jane,\r\nsee https://x.org today.\r\n\r\n> you wrote\r\n> before\r\n\r\n-- \r\nArt\r\n--b1\r\nContent-Type: application/pdf; name=\"plan.pdf\"\r\nContent-Disposition: attachment; filename=\"plan.pdf\"\r\nContent-Transfer-Encoding: base64\r\n\r\nJVBERi0xLjQK\r\n--b1--\r\n";

    #[test]
    fn a_message() {
        let src = FileSource::from_bytes("m", EMAIL.to_vec());
        assert_eq!(EmlReader.detect(src.header(), "m").confidence, Confidence::Content);
        let doc = EmlReader.open(&src, &ParseContext::new()).unwrap();
        let s = flow(&doc);
        has(&s, &["Art Vandelay <", "mailto:art@vandelay.com", "Café plans", "Hi Jane,", "LineBreak", "https://x.org", "Quote([Paragraph([Text(\"you wrote\"), LineBreak, Text(\"before\")", "Rule", "Attachments", "plan.pdf"]);
        assert!(doc.metadata().iter().any(|(k, v)| k == "Title" && v == "Café plans"));
        assert!(doc.metadata().iter().any(|(k, v)| k == "Attachments" && v == "1"));
        assert!(doc.facets().any(|f| f.name == "Headers"));
    }

    #[test]
    fn an_html_message_shows_its_html() {
        let m = b"From: a@b.c\r\nSubject: s\r\nContent-Type: multipart/alternative; boundary=x\r\n\r\n--x\r\nContent-Type: text/plain\r\n\r\nplain version\r\n--x\r\nContent-Type: text/html\r\n\r\n<html><head><style>p{}</style></head><body><p>The <b>rich</b> one</p><script>bad()</script></body></html>\r\n--x--\r\n";
        let doc = EmlReader.open(&FileSource::from_bytes("a.eml", m.to_vec()), &ParseContext::new()).unwrap();
        let s = flow(&doc);
        has(&s, &["Strong([Text(\"rich\")])"]);
        assert!(!s.contains("plain version") && !s.contains("bad()") && !s.contains("p{}"), "{s}");
    }

    #[test]
    fn inline_pictures_by_content_id() {
        let m = b"From: a@b.c\r\nSubject: s\r\nContent-Type: multipart/related; boundary=x\r\n\r\n--x\r\nContent-Type: text/html\r\n\r\n<p>Logo: <img src=\"cid:logo@here\"></p>\r\n--x\r\nContent-Type: image/png\r\nContent-ID: <logo@here>\r\nContent-Transfer-Encoding: base64\r\n\r\niVBORw0KGgo=\r\n--x--\r\n";
        let doc = EmlReader.open(&FileSource::from_bytes("a.eml", m.to_vec()), &ParseContext::new()).unwrap();
        has(&flow(&doc), &["Image { reference: \"cid:logo@here\""]);
        let Content::Flow(f) = doc.facet(0, &ParseContext::new()).unwrap() else { panic!() };
        assert_eq!(f.resource("cid:logo@here").unwrap().range(0, 4).to_vec(), b"\x89PNG");
    }

    #[test]
    fn a_mailbox() {
        let mbox = b"From ann@example.com Sat Jan  3 01:05:34 1996\nFrom: Ann <ann@example.com>\nSubject: first\n\nOne.\n\nFrom bob@example.com Sat Jan  3 02:05:34 1996\nFrom: bob@example.com\nSubject: second\n\n>From the start\n";
        let src = FileSource::from_bytes("box", mbox.to_vec());
        assert_eq!(MboxReader.detect(src.header(), "box").confidence, Confidence::Content);
        assert_eq!(EmlReader.detect(src.header(), "box"), Detection::NONE);
        let doc = MboxReader.open(&src, &ParseContext::new()).unwrap();
        let s = flow(&doc);
        has(&s, &["Heading { level: 2, text: [Text(\"first\")]", "One.", "Heading { level: 2, text: [Text(\"second\")]", "From the start"]);
        assert!(doc.metadata().iter().any(|(k, v)| k == "Messages" && v == "2"));
    }

    #[test]
    fn a_web_archive() {
        let mht = b"From: <Saved by Blink>\r\nSnapshot-Content-Location: https://example.org/page\r\nSubject: The Page\r\nMIME-Version: 1.0\r\nContent-Type: multipart/related; type=\"text/html\"; boundary=\"----x\"\r\n\r\n------x\r\nContent-Type: text/html\r\nContent-Location: https://example.org/page\r\n\r\n<html><body><h1>Saved page</h1><p>Text <img src=\"cid:img1\"></p></body></html>\r\n------x\r\nContent-Type: image/png\r\nContent-Location: https://example.org/a.png\r\nContent-Transfer-Encoding: base64\r\n\r\niVBORw0KGgo=\r\n------x--\r\n";
        let src = FileSource::from_bytes("p.mht", mht.to_vec());
        assert_eq!(MhtReader.detect(src.header(), "p.mht").confidence, Confidence::Content);
        let doc = MhtReader.open(&src, &ParseContext::new()).unwrap();
        has(&flow(&doc), &["Heading { level: 1, text: [Text(\"Saved page\")]"]);
        // The page's picture, by the URL it was saved from.
        let Content::Flow(f) = doc.facet(0, &ParseContext::new()).unwrap() else { panic!() };
        assert_eq!(f.resource("https://example.org/a.png").unwrap().range(0, 4).to_vec(), b"\x89PNG");
        assert!(doc.metadata().iter().any(|(k, v)| k == "Saved from" && v == "https://example.org/page"));
        assert!(doc.metadata().iter().any(|(k, v)| k == "Resources" && v == "1"));
    }

    #[test]
    fn text_with_no_headers_isnt_mail() {
        assert_eq!(EmlReader.detect(b"just text\nmore\n", "notes.txt"), Detection::NONE);
        assert_eq!(MboxReader.detect(b"From here on, prose.\n", "a.txt"), Detection::NONE);
        assert_eq!(MsgReader.detect(OLE, "a.doc"), Detection::NONE);
    }
}
