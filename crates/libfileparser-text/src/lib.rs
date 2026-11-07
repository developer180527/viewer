//! Any text file into a document (docs/design/libfileparser.md §2): its text by lines,
//! with the language it's written in when the name or first line says, read straight
//! from the file (a 2 GB log costs only its line index).
//!
//! The same text is the generic **Source** facet the front door adds to any file that
//! is text, whatever else reads it (a USDA's Stage, an OBJ's Objects): [`source`].
//!
//! The API is the shape every libfileparser category has: [`readers`], [`detect`],
//! [`open`] and [`open_with`].

use std::sync::Arc;

use libfileparser_core::{
    Category, Confidence, Content, Detection, Document, FileSource, FormatInfo, LineText, ParseContext, ParseError, Reader,
    ReaderInfo, Result, Role,
};

/// Every format this category reads.
pub fn readers() -> &'static [&'static dyn Reader] {
    &[&TextReader]
}

pub fn detect(header: &[u8], name: &str) -> Vec<(&'static dyn Reader, Detection)> {
    libfileparser_core::detect(readers(), header, name)
}

pub fn open(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    libfileparser_core::open(readers(), src, cx)
}

pub fn open_with(id: &str, src: &FileSource, cx: &ParseContext) -> Result<Document> {
    libfileparser_core::open_with(readers(), id, src, cx)
}

/// Whether bytes (a file's first few KB) are text: `-core`'s test, which every
/// category's Source facet uses.
pub use libfileparser_core::is_text;

/// The language a text is in, from its name, else its first line (`#!/bin/sh`,
/// `#usda 1.0`): an id like `rust`, `json`, `usd`, as an app's highlighter knows them.
pub fn language(name: &str, header: &[u8]) -> Option<&'static str> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name).to_ascii_lowercase();
    let by_name = match base.as_str() {
        "makefile" | "gnumakefile" => Some("makefile"),
        "dockerfile" => Some("dockerfile"),
        "cmakelists.txt" => Some("cmake"),
        _ => None,
    };
    let ext = base.rsplit_once('.').map(|(_, e)| e);
    by_name.or_else(|| ext.and_then(|e| LANGUAGES.iter().find(|(exts, _)| exts.contains(&e)).map(|(_, l)| *l))).or_else(|| {
        let first = header.split(|&b| b == b'\n').next()?;
        let first = std::str::from_utf8(first).ok()?.trim();
        if first.starts_with("#usda") {
            Some("usd")
        } else if let Some(shebang) = first.strip_prefix("#!") {
            let program = shebang.split_whitespace().rev().find(|w| !w.starts_with('-'))?.rsplit('/').next()?;
            match program.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.') {
                "sh" | "bash" | "zsh" | "dash" => Some("shell"),
                "python" => Some("python"),
                "node" => Some("javascript"),
                "ruby" => Some("ruby"),
                "perl" => Some("perl"),
                _ => None,
            }
        } else if first.starts_with("<?xml") {
            Some("xml")
        } else {
            None
        }
    })
}

/// Extensions by language.
const LANGUAGES: &[(&[&str], &str)] = &[
    (&["txt", "text", "log"], "text"),
    (&["md", "markdown"], "markdown"),
    (&["rs"], "rust"),
    (&["c", "h"], "c"),
    (&["cc", "cpp", "cxx", "hpp", "hh", "hxx"], "cpp"),
    (&["m", "mm"], "objective-c"),
    (&["swift"], "swift"),
    (&["kt", "kts"], "kotlin"),
    (&["java"], "java"),
    (&["scala"], "scala"),
    (&["go"], "go"),
    (&["py", "pyi"], "python"),
    (&["rb"], "ruby"),
    (&["php"], "php"),
    (&["pl", "pm"], "perl"),
    (&["lua"], "lua"),
    (&["js", "mjs", "cjs", "jsx"], "javascript"),
    (&["ts", "tsx"], "typescript"),
    (&["css", "scss", "less"], "css"),
    (&["html", "htm"], "html"),
    (&["xml", "xsl", "svg", "dae"], "xml"),
    (&["json", "gltf", "jsonl", "ndjson"], "json"),
    (&["toml"], "toml"),
    (&["yaml", "yml"], "yaml"),
    (&["ini", "cfg", "conf", "properties", "env"], "ini"),
    (&["sh", "bash", "zsh", "fish"], "shell"),
    (&["ps1"], "powershell"),
    (&["bat", "cmd"], "batch"),
    (&["sql"], "sql"),
    (&["csv", "tsv"], "csv"),
    (&["tex", "bib"], "latex"),
    (&["diff", "patch"], "diff"),
    (&["proto"], "protobuf"),
    (&["graphql", "gql"], "graphql"),
    (&["cmake"], "cmake"),
    (&["cs"], "csharp"),
    (&["dart"], "dart"),
    (&["zig"], "zig"),
    (&["hs"], "haskell"),
    (&["ml", "mli"], "ocaml"),
    (&["ex", "exs"], "elixir"),
    (&["erl"], "erlang"),
    (&["clj"], "clojure"),
    (&["r"], "r"),
    (&["jl"], "julia"),
    (&["srt", "vtt"], "subtitles"),
    (&["rst"], "restructuredtext"),
    (&["usda"], "usd"),
    (&["obj"], "obj"),
    (&["mtl"], "mtl"),
    (&["fbx"], "fbx"),
    (&["ply"], "ply"),
    (&["stl"], "stl"),
];

/// The Source facet for any file that is text: the file itself, by line, in its
/// language (`language`, else as [`language`] finds it). `None` when it isn't text.
pub fn source(src: &FileSource, language: Option<&str>, cx: &ParseContext) -> Result<Option<Content>> {
    if !is_text(src.header()) {
        return Ok(None);
    }
    let language = language.or_else(|| self::language(src.name(), src.header()));
    Ok(Some(Content::Text(Arc::new(lines(src, language, cx)?))))
}

fn lines(src: &FileSource, language: Option<&str>, cx: &ParseContext) -> Result<LineText> {
    LineText::within(src.clone(), language, &cx.limits)
}

/// Plain text, code and logs.
pub struct TextReader;

static TEXT: ReaderInfo = ReaderInfo::new("text", "Text", Category::Text)
    .extensions(&[
        "txt", "text", "log", "md", "markdown", "rs", "c", "h", "cc", "cpp", "cxx", "hpp", "m", "mm", "swift", "kt", "java", "go", "py", "rb", "php",
        "lua", "js", "mjs", "ts", "tsx", "jsx", "css", "html", "htm", "xml", "sh", "bash", "zsh", "sql", "yaml", "yml", "ini", "cfg", "conf", "tex",
        "diff", "patch", "proto", "cs", "dart", "zig", "srt", "vtt", "rst",
    ])
    .mime_types(&["text/plain"]);

impl Reader for TextReader {
    fn info(&self) -> &ReaderInfo {
        &TEXT
    }

    /// Never more than [`Confidence::Extension`]: nearly every readable file is text,
    /// so being text says little. Listed last, plain text then loses every tie, and a
    /// `.gltf` or `.obj` opens as a model, with its text as the Source facet.
    fn detect(&self, header: &[u8], name: &str) -> Detection {
        let text = is_text(header);
        let d = if text || (header.is_empty() && TEXT.matches_extension(name)) { Detection::new(Confidence::Extension) } else { Detection::NONE };
        match language(name, header) {
            Some(l) if d.is_match() => d.with_language(l),
            _ => d,
        }
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        cx.cancel.check()?;
        if !src.is_empty() && !is_text(src.header()) {
            return Err(ParseError::NotThisFormat);
        }
        let language = language(src.name(), src.header());
        let text = lines(src, language, cx)?;
        let lines = libfileparser_core::TextSource::info(&text).lines;
        let format = match language {
            Some("text") | None => "Plain text".to_string(),
            Some(l) => format!("Text ({l})"),
        };
        let utf8 = std::str::from_utf8(src.bytes()).is_ok();
        Ok(Document::new(FormatInfo::new("text", format, Category::Text))
            .with_facet(Role::Primary, "Text", Content::Text(Arc::new(text)))
            .with_meta("Lines", lines)
            .with_meta("Encoding", if utf8 { "UTF-8" } else { "UTF-8, with bytes that aren't" })
            .with_file_facets(src, language))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn languages_by_name_and_first_line() {
        assert_eq!(language("src/main.rs", b""), Some("rust"));
        assert_eq!(language("Makefile", b""), Some("makefile"));
        assert_eq!(language("scene.usda", b""), Some("usd"));
        assert_eq!(language("build", b"#!/usr/bin/env python3\nprint()"), Some("python"));
        assert_eq!(language("run", b"#!/bin/bash -e\n"), Some("shell"));
        assert_eq!(language("layer", b"#usda 1.0\n"), Some("usd"));
        assert_eq!(language("notes", b"hello"), None);
    }

    #[test]
    fn text_is_utf8_without_nuls() {
        assert!(is_text(b"hello\n"));
        assert!(is_text("caf\u{e9}".as_bytes()));
        // A character cut off at the end of the header is still text.
        assert!(is_text(&"caf\u{e9}".as_bytes()[..4]));
        assert!(!is_text(b"PK\x03\x04\0\0"));
        assert!(!is_text(b"\xff\xfe\xfd\xfc\xfb"));
        assert!(!is_text(b""));
    }
}
