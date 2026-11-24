//! Org (Emacs Org mode), parsed by orgize: its parse, written out as HTML by orgize,
//! becomes formatted text through html5ever (`crate::html`), so headings, lists,
//! tables, source blocks, quotes, links and marks come out as Org defines them.

use std::sync::Arc;

use std::borrow::Cow;

use libfileparser_core::{Category, Confidence, Content, Detection, Document, FileSource, FormatInfo, OwnedFlow, ParseContext, ParseError, Reader, ReaderInfo, Result, Role, Warning, is_text};

use crate::build::Builder;

static INFO: ReaderInfo = ReaderInfo::new("org", "Org", Category::Docs).extensions(&["org"]).mime_types(&["text/org"]);

pub struct OrgReader;

/// Emphasis marks in one paragraph past which orgize is too slow: its inline parse
/// grows with about the cube of their number (1,000 take 35 ms, 2,000 a quarter of a
/// second, a crafted 100 KB paragraph tens of seconds), and it can't be cancelled. No
/// paragraph written by hand comes near.
const MARKS: usize = 1000;

/// The text with every paragraph of more than [`MARKS`] emphasis marks made an example
/// block, shown as written (Org's comma escapes keep its lines from being headings or
/// keywords), and how many were. Emphasis never spans a blank line, so a paragraph is
/// as far as the parse's cost reaches.
fn tame(text: &str) -> (Cow<'_, str>, usize) {
    let marks = |s: &str| s.bytes().filter(|b| matches!(b, b'*' | b'/' | b'_' | b'=' | b'~' | b'+')).count();
    if marks(text) <= MARKS {
        return (Cow::Borrowed(text), 0);
    }
    let (mut out, mut tamed) = (String::with_capacity(text.len() + 64), 0);
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim().is_empty() {
            out.push_str(lines[i]);
            i += 1;
            continue;
        }
        let end = lines[i..].iter().position(|l| l.trim().is_empty()).map_or(lines.len(), |n| i + n);
        let para = &lines[i..end];
        if para.iter().map(|l| marks(l)).sum::<usize>() > MARKS {
            tamed += 1;
            out.push_str("#+BEGIN_EXAMPLE\n");
            for l in para {
                let bare = l.trim_start_matches([' ', '\t']).trim_start_matches(',');
                if bare.starts_with('*') || bare.starts_with("#+") {
                    out.push(',');
                }
                out.push_str(l);
            }
            if !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str("#+END_EXAMPLE\n");
        } else {
            para.iter().for_each(|l| out.push_str(l));
        }
        i = end;
    }
    (Cow::Owned(out), tamed)
}

/// Headings' TODO keywords marked bold, as Emacs shows them: orgize's HTML leaves them
/// out, and a checklist without them loses its point. The keywords are TODO and DONE,
/// or the document's own (`#+TODO: NEXT WAIT | DONE CANCELLED`).
fn keywords(text: &str) -> Cow<'_, str> {
    let mut words: Vec<String> = Vec::new();
    for line in text.lines() {
        let l = line.trim_start();
        let upper = l.get(..12).unwrap_or(l).to_ascii_uppercase();
        if let Some(rest) = ["#+TODO:", "#+SEQ_TODO:", "#+TYP_TODO:"].iter().find(|k| upper.starts_with(*k)).map(|k| &l[k.len()..]) {
            words.extend(rest.split_whitespace().filter(|w| *w != "|").map(|w| w.split('(').next().unwrap_or(w).to_string()));
        }
    }
    if words.is_empty() {
        words = vec!["TODO".into(), "DONE".into()];
    }
    let keyword = |line: &str| -> Option<usize> {
        let stars = line.bytes().take_while(|&b| b == b'*').count();
        let rest = line.get(stars..)?;
        if stars == 0 || !rest.starts_with(' ') {
            return None;
        }
        let word = rest.split_whitespace().next()?;
        words.iter().any(|w| w == word).then(|| line.len() - rest.trim_start().len())
    };
    if !text.lines().any(|l| keyword(l).is_some()) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len() + 64);
    for line in text.split_inclusive('\n') {
        match keyword(line) {
            Some(at) => {
                let word_len = line[at..].split_whitespace().next().map_or(0, str::len);
                out.push_str(&line[..at]);
                out.push('*');
                out.push_str(&line[at..at + word_len]);
                out.push('*');
                out.push_str(&line[at + word_len..]);
            }
            None => out.push_str(line),
        }
    }
    Cow::Owned(out)
}

impl Reader for OrgReader {
    fn info(&self) -> &ReaderInfo {
        &INFO
    }

    /// By extension only: Org has no signature.
    fn detect(&self, header: &[u8], name: &str) -> Detection {
        if INFO.matches_extension(name) && is_text(header) { Detection::new(Confidence::Extension).with_language("org") } else { Detection::NONE }
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        cx.cancel.check()?;
        if !is_text(src.header()) {
            return Err(ParseError::NotThisFormat);
        }
        let bytes = crate::whole(src, cx)?;
        let text = String::from_utf8_lossy(&bytes);
        let (text, tamed) = tame(&text);
        let text = keywords(&text);
        let org = orgize::Org::parse(&text);
        let title = org.document().title();
        let mut b = Builder::new(cx);
        crate::html::parse(&org.to_html(), src.name(), &mut b)?;
        let mut doc = Document::new(FormatInfo::new("org", "Org document", Category::Docs)).with_facet(Role::Primary, "Document", Content::Flow(Arc::new(OwnedFlow::new(b.finish()?))));
        if let Some(t) = title.filter(|t| !t.trim().is_empty()) {
            doc = doc.with_meta("Title", t.trim());
        }
        if tamed > 0 {
            doc = doc.with_warning(Warning::new(format!("{tamed} paragraph(s) with over {MARKS} emphasis marks shown as written, without formatting: reading them as Org takes too long.")));
        }
        Ok(doc.with_file_facets(src, Some("org")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn org_through_orgize() {
        let src = FileSource::from_bytes("n.org", b"#+TITLE: Notes\n* Top\n** Sub\nSome *bold*, /it/, =verb= and [[https://x.org][a link]].\n- one\n- two\n#+BEGIN_SRC sh\nls\n#+END_SRC\n| a | b |\n|---+---|\n| 1 | 2 |\n".to_vec());
        let doc = OrgReader.open(&src, &ParseContext::new()).unwrap();
        assert!(doc.metadata().iter().any(|(k, v)| k == "Title" && v == "Notes"));
        let Content::Flow(f) = doc.facet(0, &ParseContext::new()).unwrap() else { panic!() };
        let s = format!("{:?}", f.blocks(0..100).unwrap());
        for want in ["Heading { level: 1", "Heading { level: 2", "Strong", "Emphasis", "Code(\"verb\")", "https://x.org", "List", "Code { language", "Table { header_rows: 1"] {
            assert!(s.contains(want), "{want} in {s}");
        }
        assert!(doc.warnings().is_empty());
    }

    #[test]
    fn todo_keywords_are_shown() {
        let flow = |t: &str| {
            let doc = OrgReader.open(&FileSource::from_bytes("n.org", t.as_bytes().to_vec()), &ParseContext::new()).unwrap();
            let Content::Flow(f) = doc.facet(0, &ParseContext::new()).unwrap() else { panic!() };
            format!("{:?}", f.blocks(0..100).unwrap())
        };
        let s = flow("* TODO Write it\n** DONE Plan\n* TODOS are not keywords\n");
        assert!(s.contains("Strong([Text(\"TODO\")]), Text(\" Write it\")") && s.contains("Strong([Text(\"DONE\")])"), "{s}");
        assert!(s.contains("TODOS are not keywords") && !s.contains("Strong([Text(\"TODOS"), "{s}");
        let s = flow("#+TODO: NEXT(n) | CANCELLED\n* NEXT Call\n* TODO plain word here\n");
        assert!(s.contains("Strong([Text(\"NEXT\")])") && !s.contains("Strong([Text(\"TODO"), "{s}");
    }

    /// Found by the overnight soak: a paragraph of thousands of unclosed marks, which
    /// orgize took minutes over.
    #[test]
    fn a_paragraph_of_endless_emphasis_is_shown_as_written() {
        let text = format!("* Top\nSome *bold*.\n\n{}\n* heading-like\n\nAfter, /it/.\n", "/ital *bold, ".repeat(2000));
        let (tamed, n) = tame(&text);
        assert_eq!(n, 1);
        assert!(tamed.contains("#+BEGIN_EXAMPLE\n/ital") && tamed.contains("\n,* heading-like\n#+END_EXAMPLE\n\nAfter"), "{}", &tamed[tamed.len() - 80..]);
        assert!(tamed.starts_with("* Top\nSome *bold*.\n\n"));
        let t = std::time::Instant::now();
        let doc = OrgReader.open(&FileSource::from_bytes("n.org", text.into_bytes()), &ParseContext::new()).unwrap();
        assert!(t.elapsed().as_secs() < 2, "{:?}", t.elapsed());
        assert!(doc.warnings()[0].message.contains("1 paragraph"));
        let Content::Flow(f) = doc.facet(0, &ParseContext::new()).unwrap() else { panic!() };
        let s = format!("{:?}", f.blocks(0..100).unwrap());
        assert!(s.contains("Strong") && s.contains("Emphasis") && s.contains("* heading-like"), "{}", &s[..300.min(s.len())]);
        // Under the limit, untouched.
        assert!(matches!(tame("*a* /b/\n"), (Cow::Borrowed(_), 0)));
        assert!(matches!(tame(&"*".repeat(MARKS)), (Cow::Borrowed(_), 0)));
        assert_eq!(tame(&"*".repeat(MARKS + 1)).1, 1);
    }
}
