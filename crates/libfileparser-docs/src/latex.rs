//! LaTeX and Texinfo, read by `markup-lite` (our own interpreters of the commands
//! documents use) into HTML, then formatted text (`crate::markup`).

use libfileparser_core::Result;

use crate::markup::{Rendered, markup};

markup!(
    /// LaTeX (and plain TeX) documents.
    LatexReader, LATEX, "latex", "LaTeX", "LaTeX document", ["tex", "latex", "ltx"], ["application/x-latex", "text/x-tex"], latex
);

markup!(
    /// Texinfo (GNU's manuals' source).
    TexinfoReader, TEXINFO, "texinfo", "Texinfo", "Texinfo manual", ["texi", "texinfo", "txi"], ["application/x-texinfo"], texinfo
);

fn latex(text: &str) -> Result<Rendered> {
    Ok(markup_lite::latex(text).into())
}

fn texinfo(text: &str) -> Result<Rendered> {
    Ok(markup_lite::texinfo(text).into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use libfileparser_core::{Content, Document, FileSource, ParseContext, Reader};

    fn blocks(reader: &dyn Reader, name: &str, text: &str) -> (Document, String) {
        let doc = reader.open(&FileSource::from_bytes(name, text.as_bytes().to_vec()), &ParseContext::new()).unwrap();
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
    fn latex_document() {
        let (doc, s) = blocks(
            &LatexReader,
            "a.tex",
            "\\documentclass{article}\n\\usepackage[utf8]{inputenc}\n\\title{On Things}\n\\author{A. Writer}\n\\begin{document}\n\\maketitle\n\\section{Intro}\\label{s:intro}\nSome \\textbf{bold}, \\emph{em} and \\texttt{code} with $x^2$ % a comment\nand \\href{https://x.org}{a link}\\footnote{A note.}.\n\nSecond paragraph, caf\\'e -- ``quoted''.\n\\begin{itemize}\n\\item One\n\\item Two \\begin{enumerate}\\item nested\\end{enumerate}\n\\end{itemize}\n\\begin{verbatim}\n  keep \\this\n\\end{verbatim}\n\\begin{tabular}{ll}\n\\hline\na & b \\\\\n1 & 2 \\\\\n\\end{tabular}\n\\begin{equation}\nE = mc^2\n\\end{equation}\n\\begin{quote}Quoted.\\end{quote}\n\\mystery{kept text}\n\\end{document}\n",
        );
        has(&s, &["Heading { level: 1, text: [Text(\"On Things\")]", "A. Writer", "Heading { level: 2, text: [Text(\"Intro\")]", "Strong([Text(\"bold\")])", "Emphasis([Text(\"em\")])", "Code(\"code\")", "Code(\"x^2\")", "https://x.org", "A note.", "café – “quoted”", "List { ordered: false", "List { ordered: true", "nested", "keep \\\\this", "Table", "Text(\"b\")", "E = mc^2", "Quote", "kept text"]);
        for not in ["utf8", "inputenc", "s:intro", "a comment", "hline", "documentclass"] {
            assert!(!s.contains(not), "{not} in {s}");
        }
        assert!(doc.metadata().iter().any(|(k, v)| k == "Title" && v == "On Things"));
    }

    #[test]
    fn latex_fragments_and_font_switches() {
        let (_, s) = blocks(&LatexReader, "f.tex", "Text with {\\bf bold \\it both} and back.\n\\begin{lstlisting}[language=Python]\nprint(1)\n\\end{lstlisting}\n");
        has(&s, &["Strong([Text(\"bold \"), Emphasis([Text(\"both\")])])", "and back", "language: Some(\"python\")"]);
    }

    #[test]
    fn latex_deep_and_broken_input_ends() {
        let deep = "\\textbf{".repeat(5000) + "x";
        let _ = blocks(&LatexReader, "d.tex", &deep);
        let _ = blocks(&LatexReader, "e.tex", "\\begin{itemize}\\item a \\verb|x");
        let _ = blocks(&LatexReader, "g.tex", "$$ unclosed \\[ \\( \\href{");
    }

    #[test]
    fn texinfo_manual() {
        let (doc, s) = blocks(&TexinfoReader, "a.texi", "\\input texinfo\n@setfilename a.info\n@settitle The Manual\n@node Top\n@top The Manual\n@chapter Overview\nThis is @emph{important}, see @code{ls} and @uref{https://x.org, the site}.\n\n@itemize @bullet\n@item\nFirst\n@item Second\n@end itemize\n@example\nls -l @@home\n@end example\n@table @code\n@item -v\nVerbose.\n@end table\n@multitable @columnfractions .5 .5\n@headitem A @tab B\n@item 1 @tab 2\n@end multitable\n@ignore\nhidden\n@end ignore\n@bye\n");
        has(&s, &["Heading { level: 1, text: [Text(\"The Manual\")]", "Heading { level: 1, text: [Text(\"Overview\")]", "Emphasis([Text(\"important\")])", "Code(\"ls\")", "https://x.org", "List { ordered: false", "First", "Second", "ls -l @home", "Code(\"-v\")", "Verbose.", "Table { header_rows: 1"]);
        assert!(!s.contains("hidden") && !s.contains("setfilename"), "{s}");
        assert!(doc.metadata().iter().any(|(k, v)| k == "Title" && v == "The Manual"));
    }
}
