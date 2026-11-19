//! Manual pages (roff with the `man` or `mdoc` macros), read by `markup-lite`'s
//! interpreter into HTML, then formatted text (`crate::markup`).

use libfileparser_core::{Category, Confidence, Detection, Document, FileSource, ParseContext, Reader, ReaderInfo, Result, is_text};
use markup_lite::starts_like_a_manual;

use crate::markup::Rendered;

static INFO: ReaderInfo = ReaderInfo::new("man", "Manual page", Category::Docs)
    .extensions(&["man", "mdoc", "roff", "1", "2", "3", "4", "5", "6", "7", "8", "9", "1m", "3p", "n", "l"])
    .mime_types(&["text/troff", "application/x-troff-man", "text/x-mdoc"]);

pub struct ManReader;

impl Reader for ManReader {
    fn info(&self) -> &ReaderInfo {
        &INFO
    }

    /// A manual's first request (`.TH`, `.Dd`); a section number alone as extension
    /// (`ls.1`) counts only with one.
    fn detect(&self, header: &[u8], name: &str) -> Detection {
        if !is_text(header) {
            return Detection::NONE;
        }
        let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
        let named = matches!(ext.as_str(), "man" | "mdoc" | "roff");
        if starts_like_a_manual(header) {
            Detection::new(Confidence::Content).with_language("roff")
        } else if named {
            Detection::new(Confidence::Extension).with_language("roff")
        } else {
            Detection::NONE
        }
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        crate::markup::read(src, cx, "man", "Manual page", roff)
    }
}


fn roff(text: &str) -> Result<Rendered> {
    Ok(markup_lite::man(text).into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use libfileparser_core::Content;

    fn blocks(text: &str) -> (Document, String) {
        let src = FileSource::from_bytes("ls.1", text.as_bytes().to_vec());
        assert!(ManReader.detect(src.header(), "ls.1").confidence >= Confidence::Content);
        let doc = ManReader.open(&src, &ParseContext::new()).unwrap();
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
    fn man_macros() {
        let (doc, s) = blocks(".\\\" a comment\n.TH LS 1 \"2026\" \"GNU\"\n.SH NAME\nls \\- list directory contents\n.SH SYNOPSIS\n.B ls\n[\\fIOPTION\\fR]... [\\fIFILE\\fR]...\n.SH OPTIONS\n.TP\n.BR \\-a \", \" \\-\\-all\ndo not ignore entries starting with .\n.TP\n\\fB\\-l\\fR\nuse a long listing format\n.PP\nAfter the list \\(em done.\n.SH EXAMPLES\n.EX\nls \\-la /tmp\n.EE\n.SH SEE ALSO\n.UR https://www.gnu.org/software/coreutils/\nthe manual\n.UE\n");
        has(&s, &["Heading { level: 1, text: [Text(\"LS(1)\")]", "Heading { level: 2, text: [Text(\"NAME\")]", "ls - list directory contents", "Strong([Text(\"ls\")])", "Emphasis([Text(\"OPTION\")])", "Strong([Text(\"-a\")])", "--all", "Quote([Paragraph([Text(\"do not ignore", "Strong([Text(\"-l\")])", "—", "Code { language: None, text: \"ls -la /tmp\" }", "https://www.gnu.org/software/coreutils/"]);
        assert!(!s.contains("comment"));
        assert!(doc.metadata().iter().any(|(k, v)| k == "Title" && v == "LS(1)"));
    }

    #[test]
    fn mdoc_macros() {
        let (doc, s) = blocks(".Dd $Mdocdate: May 1 2026 $\n.Dt CAT 1\n.Os\n.Sh NAME\n.Nm cat\n.Nd concatenate files\n.Sh SYNOPSIS\n.Nm cat\n.Op Fl benstuv\n.Op Ar\n.Sh DESCRIPTION\nThe\n.Nm\nutility reads files.\n.Bl -tag -width Ds\n.It Fl b\nNumber the lines.\n.It Fl n\nNumber all lines.\n.El\n.Bd -literal -offset indent\n$ cat file1 file2 > file3\n.Ed\n.Sh SEE ALSO\n.Xr head 1 ,\n.Xr tail 1\n.Sh STANDARDS\nThe utility follows\n.St -p1003.1-2008 .\n");
        has(&s, &["Heading { level: 1, text: [Text(\"cat(1)\")]", "Strong([Text(\"cat\")])", "— concatenate files", "[", "Strong([Text(\"-benstuv\")])", "Emphasis([Text(\"file ...\")])", "utility reads files", "Strong([Text(\"-b\")])", "Quote([Paragraph([Text(\"Number the lines.\")", "Code { language: None, text: \"$ cat file1 file2 > file3\" }", "Strong([Text(\"head\")]), Text(\"(1),", "POSIX.1"]);
        assert!(doc.metadata().iter().any(|(k, v)| k == "Title" && v == "cat(1)"));
    }

    #[test]
    fn tables_and_definitions_are_kept_or_skipped() {
        let (_, s) = blocks(".TH T 7\n.de XX\nhidden macro body\n..\n.TS\ntab(:);\nl l.\na:b\n1:2\n.TE\n.ig\nignored too\n..\nshown\n");
        has(&s, &["Table", "Text(\"a\")", "Text(\"2\")", "shown"]);
        assert!(!s.contains("hidden") && !s.contains("ignored"), "{s}");
    }

    #[test]
    fn a_section_number_alone_isnt_enough() {
        assert_eq!(ManReader.detect(b"just some text\n", "notes.1"), Detection::NONE);
        assert!(ManReader.detect(b".TH X 1\n", "x").confidence >= Confidence::Content);
    }
}
