//! Viewer's text loader: plain text and source code, with syntect highlighting.
//! (Markdown is libfileparser-docs'.)

use std::sync::OnceLock;

use syntect::highlighting::{FontStyle, HighlightIterator, HighlightState, Highlighter, ThemeSet};
use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};
use viewer_core::{Confidence, Content, Document, FileSource, FormatLoader, Result, Span, TextDoc};

pub static LOADERS: &[&dyn FormatLoader] = &[&TextLoader];

/// Files above this size are shown without highlighting so they open instantly.
const HIGHLIGHT_LIMIT: usize = 512 * 1024;

/// Extensions we claim without sniffing. Kept as a static list so probing never
/// has to load syntect's syntax set.
const TEXT_EXTS: &[&str] = &[
    "txt", "log", "text", "rs", "c", "h", "cc", "cpp", "hpp", "cxx", "m", "mm", "swift", "kt", "kts", "java",
    "scala", "go", "py", "pyi", "rb", "php", "pl", "lua", "js", "mjs", "cjs", "ts", "tsx", "jsx", "css", "scss",
    "less", "html", "htm", "xml", "xsl", "sh", "bash", "zsh", "fish", "ps1", "bat", "cmd", "sql", "yaml", "yml",
    "ini", "cfg", "conf", "env", "properties", "gradle", "cmake", "mk", "makefile", "dockerfile", "tex", "bib",
    "r", "jl", "hs", "ml", "mli", "fs", "fsx", "cs", "vb", "dart", "zig", "nim", "ex", "exs", "erl", "clj",
    "lisp", "el", "vim", "diff", "patch", "proto", "graphql", "gql", "srt", "vtt", "rst", "adoc", "org", "lock",
];

pub struct TextLoader;

impl FormatLoader for TextLoader {
    fn id(&self) -> &'static str {
        "text"
    }

    fn name(&self) -> &'static str {
        "Text"
    }

    fn probe(&self, header: &[u8], ext: Option<&str>, name: &str) -> Confidence {
        // Archive entries are named by their full path; only the last part matters.
        let base = name.rsplit(['/', '\\']).next().unwrap_or(name).to_ascii_lowercase();
        if ext.is_some_and(|e| TEXT_EXTS.contains(&e))
            || matches!(base.as_str(), "makefile" | "dockerfile" | "license" | "readme" | "cmakelists.txt")
        {
            Confidence::EXTENSION
        } else if looks_like_text(header) {
            Confidence::SNIFF
        } else {
            Confidence::NONE
        }
    }

    fn open(&self, src: &FileSource) -> Result<Document> {
        let bytes = src.bytes();
        let mut line_starts = vec![0];
        line_starts.extend(memchr::memchr_iter(b'\n', bytes).map(|i| i + 1).filter(|&i| i < bytes.len()));

        let mut doc = TextDoc { source: src.clone(), line_starts, syntax: None, highlights: None };
        if bytes.len() <= HIGHLIGHT_LIMIT {
            let ss = syntax_set();
            if let Some(syntax) = find_syntax(ss, src) {
                doc.syntax = Some(syntax.name.clone());
                doc.highlights = Some(highlight(ss, syntax, &doc));
            }
        }

        let lines = doc.line_count();
        let format = doc.syntax.clone().map_or("Plain text".into(), |s| format!("{s} source"));
        let mut out = Document::new(format, Content::Text(doc))
            .with_meta("Lines", lines)
            .with_meta("Encoding", if std::str::from_utf8(bytes).is_ok() { "UTF-8" } else { "binary / non-UTF-8" });
        if bytes.len() > HIGHLIGHT_LIMIT {
            out = out.with_meta("Highlighting", "off (file too large)");
        }
        Ok(out)
    }
}

/// No NUL bytes and valid UTF-8 (a multi-byte char may be cut at the end).
pub fn looks_like_text(header: &[u8]) -> bool {
    if header.is_empty() || memchr::memchr(0, header).is_some() {
        return false;
    }
    match std::str::from_utf8(header) {
        Ok(_) => true,
        Err(e) => e.error_len().is_none() && header.len() - e.valid_up_to() < 4,
    }
}

fn syntax_set() -> &'static SyntaxSet {
    static SS: OnceLock<SyntaxSet> = OnceLock::new();
    SS.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn themes() -> &'static ThemeSet {
    static TS: OnceLock<ThemeSet> = OnceLock::new();
    TS.get_or_init(ThemeSet::load_defaults)
}

fn find_syntax<'a>(ss: &'a SyntaxSet, src: &FileSource) -> Option<&'a SyntaxReference> {
    let by_ext = src.ext().and_then(|e| ss.find_syntax_by_extension(&e));
    let by_name = || ss.find_syntax_by_extension(src.name());
    let by_first_line = || {
        let first = src.bytes().split(|&b| b == b'\n').next()?;
        ss.find_syntax_by_first_line(std::str::from_utf8(first).ok()?)
    };
    by_ext
        .or_else(by_name)
        .or_else(by_first_line)
        .filter(|s| s.name != "Plain Text")
}

/// Parses once and resolves colours for a dark and a light theme.
fn highlight(ss: &SyntaxSet, syntax: &SyntaxReference, doc: &TextDoc) -> [Vec<Vec<Span>>; 2] {
    let ts = themes();
    let highlighters = [
        Highlighter::new(&ts.themes["base16-ocean.dark"]),
        Highlighter::new(&ts.themes["InspiredGitHub"]),
    ];
    let mut states = highlighters.each_ref().map(|h| HighlightState::new(h, ScopeStack::new()));
    let mut parse = ParseState::new(syntax);
    let mut out: [Vec<Vec<Span>>; 2] = Default::default();

    for i in 0..doc.line_count() {
        let line = String::from_utf8_lossy(doc.raw_line(i));
        // The "newlines" syntaxes expect each line to end with '\n'.
        let line = if line.ends_with('\n') { line.into_owned() } else { format!("{line}\n") };
        let ops = parse.parse_line(&line, ss).unwrap_or_default();
        for t in 0..2 {
            let spans = HighlightIterator::new(&mut states[t], &ops, &line, &highlighters[t])
                .scan(0, |pos, (style, text)| {
                    let range = *pos..*pos + text.len();
                    *pos = range.end;
                    Some(Span {
                        range,
                        color: [style.foreground.r, style.foreground.g, style.foreground.b],
                        bold: style.font_style.contains(FontStyle::BOLD),
                        italic: style.font_style.contains(FontStyle::ITALIC),
                    })
                })
                .collect();
            out[t].push(spans);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexes_lines_and_highlights_rust() {
        let src = FileSource::from_bytes("main.rs", b"fn main() {\n    println!(\"hi\");\n}".to_vec());
        let doc = TextLoader.open(&src).unwrap();
        let Content::Text(t) = doc.content else { panic!() };
        assert_eq!(t.line_count(), 3);
        assert_eq!(t.line(1), b"    println!(\"hi\");");
        assert_eq!(t.syntax.as_deref(), Some("Rust"));
        assert_eq!(t.highlights.unwrap()[0].len(), 3);
    }

    #[test]
    fn claims_well_known_names_inside_archives() {
        assert_eq!(TextLoader.probe(b"\0", None, "dir/Makefile"), Confidence::EXTENSION);
    }

    #[test]
    fn sniffs_text_but_not_binary() {
        assert!(looks_like_text("héllo".as_bytes()));
        assert!(looks_like_text(&"héllo".as_bytes()[..2])); // cut mid-character
        assert!(!looks_like_text(&[0x89, b'P', b'N', b'G', 0, 0]));
    }
}
