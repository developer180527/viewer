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

/// A text encoding other than UTF-8, as [`legacy_encoding`] finds it: its name and a
/// decoder. Its own type, so which library decodes stays out of this crate's API.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TextEncoding(&'static encoding_rs::Encoding);

impl TextEncoding {
    /// The WHATWG name: "windows-1252", "Shift_JIS", "UTF-16LE"…
    pub fn name(self) -> &'static str {
        self.0.name()
    }

    /// `bytes` as UTF-8; a byte-order mark is honoured and dropped, and what doesn't
    /// decode becomes U+FFFD.
    pub fn decode(self, bytes: &[u8]) -> std::borrow::Cow<'_, str> {
        self.0.decode(bytes).0
    }
}

/// The encoding of text that isn't UTF-8: a byte-order mark's, UTF-16's pattern of
/// zero bytes, or chardetng's guess for 8-bit text (Windows code pages, ISO-8859,
/// KOI8, Shift_JIS, GB18030, Big5, EUC, ISO-2022…). `None` for UTF-8, and for bytes
/// that aren't text in any encoding: NULs, or more than a few control bytes, mean
/// binary, which the Raw facet shows better than garbled characters.
pub fn legacy_encoding(header: &[u8]) -> Option<TextEncoding> {
    guess(header).map(TextEncoding)
}

fn guess(header: &[u8]) -> Option<&'static encoding_rs::Encoding> {
    if header.is_empty() || is_text(header) {
        return None;
    }
    if let Some((enc, _)) = encoding_rs::Encoding::for_bom(header) {
        return (enc != encoding_rs::UTF_8).then_some(enc);
    }
    if let Some(enc) = utf16_without_bom(header) {
        return Some(enc);
    }
    if header.contains(&0) {
        return None;
    }
    // Tab, line ends, form feed and escape (ISO-2022's shifts) are text; other controls
    // are what binary is made of.
    let controls = header.iter().filter(|&&b| (b < 0x20 && !matches!(b, b'\t' | b'\n' | b'\r' | 0x0c | 0x1b)) || b == 0x7f).count();
    if controls * 100 > header.len() {
        return None;
    }
    let mut detector = chardetng::EncodingDetector::new();
    detector.feed(header, header.len() < 4096);
    let enc = detector.guess(None, false);
    // What it decodes to has to read as text too.
    let (decoded, malformed) = enc.decode_without_bom_handling(header);
    let bad = decoded.chars().filter(|&c| c == '\u{fffd}' || (c.is_control() && !matches!(c, '\t' | '\n' | '\r' | '\x0c' | '\x1b'))).count();
    (!malformed || bad * 50 < decoded.chars().count()).then_some(enc)
}

/// UTF-16 without a byte-order mark: mostly ASCII text, so nearly every other byte is
/// zero, on the same side.
fn utf16_without_bom(header: &[u8]) -> Option<&'static encoding_rs::Encoding> {
    let pairs = header.len() / 2;
    if pairs < 8 {
        return None;
    }
    let (even, odd) = header.as_chunks::<2>().0.iter().fold((0, 0), |(e, o), p| (e + (p[0] == 0) as usize, o + (p[1] == 0) as usize));
    if odd * 10 > pairs * 7 && even * 10 < pairs {
        return Some(encoding_rs::UTF_16LE);
    }
    if even * 10 > pairs * 7 && odd * 10 < pairs {
        return Some(encoding_rs::UTF_16BE);
    }
    // Text in a script with no zero bytes (Japanese, Chinese): it decodes cleanly, with
    // no broken surrogates and next to no control characters, which binary never does.
    // The header may end inside a character.
    let whole = &header[..pairs * 2 - if pairs > 1 { 2 } else { 0 }];
    let clean = |enc: &'static encoding_rs::Encoding| {
        enc.decode_without_bom_handling_and_without_replacement(whole).filter(|t| {
            let n = t.chars().count().max(1);
            let controls = t.chars().filter(|&c| c.is_control() && !matches!(c, '\t' | '\n' | '\r')).count();
            let lines = t.chars().filter(|&c| c == '\n' || c == ' ' || c == '\u{3000}').count();
            controls * 100 <= n && lines > 0
        })
    };
    match (clean(encoding_rs::UTF_16LE), clean(encoding_rs::UTF_16BE)) {
        (Some(_), None) => Some(encoding_rs::UTF_16LE),
        (None, Some(_)) => Some(encoding_rs::UTF_16BE),
        _ => None,
    }
}

/// `src` decoded from `enc` into UTF-8, as a source of its own: read whole, so held to
/// the context's memory limit.
fn decoded(src: &FileSource, enc: TextEncoding, cx: &ParseContext) -> Result<FileSource> {
    let len = src.len();
    // UTF-8 can take up to three bytes for each byte of an 8-bit encoding.
    cx.limits.check(libfileparser_core::Limit::Memory, len.saturating_mul(3))?;
    let owned;
    let bytes: &[u8] = if src.is_positional() {
        let mut buf = vec![0u8; len as usize];
        let n = src.read_at(0, &mut buf);
        buf.truncate(n);
        owned = buf;
        &owned
    } else {
        src.bytes()
    };
    let text = enc.decode(bytes);
    cx.cancel.check()?;
    Ok(FileSource::from_bytes(src.name(), text.into_owned().into_bytes()))
}

/// Plain text, code and logs.
pub struct TextReader;

static TEXT: ReaderInfo = ReaderInfo::new("text", "Text", Category::Text)
    .extensions(&[
        "txt", "text", "log", "rs", "c", "h", "cc", "cpp", "cxx", "hpp", "m", "mm", "swift", "kt", "java", "go", "py", "rb", "php",
        "lua", "js", "mjs", "ts", "tsx", "jsx", "css", "xml", "sh", "bash", "zsh", "sql", "yaml", "yml", "ini", "cfg", "conf", "tex",
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
        let text = is_text(header) || legacy_encoding(header).is_some();
        let d = if text || (header.is_empty() && TEXT.matches_extension(name)) { Detection::new(Confidence::Extension) } else { Detection::NONE };
        match language(name, header) {
            Some(l) if d.is_match() => d.with_language(l),
            _ => d,
        }
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        cx.cancel.check()?;
        let legacy = legacy_encoding(src.header());
        if !src.is_empty() && !is_text(src.header()) && legacy.is_none() {
            return Err(ParseError::NotThisFormat);
        }
        let language = language(src.name(), src.header());
        let text = match legacy {
            Some(enc) => lines(&decoded(src, enc, cx)?, language, cx)?,
            None => lines(src, language, cx)?,
        };
        let lines = libfileparser_core::TextSource::info(&text).lines;
        let format = match language {
            Some("text") | None => "Plain text".to_string(),
            Some(l) => format!("Text ({l})"),
        };
        // A file read by position is never read whole for this: its start says.
        let utf8 = if src.is_positional() { is_text(src.header()) } else { std::str::from_utf8(src.bytes()).is_ok() };
        Ok(Document::new(FormatInfo::new("text", format, Category::Text))
            .with_facet(Role::Primary, "Text", Content::Text(Arc::new(text)))
            .with_meta("Lines", lines)
            .with_meta("Encoding", match legacy {
                Some(enc) => enc.name(),
                None if utf8 => "UTF-8",
                None => "UTF-8, with bytes that aren't",
            })
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
    fn text_in_other_encodings_is_found_and_binary_is_not() {
        let (cp1251, _, _) = encoding_rs::WINDOWS_1251.encode("Съешь же ещё этих мягких французских булок, да выпей чаю.\n");
        assert_eq!(legacy_encoding(&cp1251), Some(TextEncoding(encoding_rs::WINDOWS_1251)));
        let (sjis, _, _) = encoding_rs::SHIFT_JIS.encode("いろはにほへと ちりぬるを わかよたれそ つねならむ\n");
        assert_eq!(legacy_encoding(&sjis), Some(TextEncoding(encoding_rs::SHIFT_JIS)));
        let utf16le: Vec<u8> = "plain words in UTF-16, little end first\n".encode_utf16().flat_map(u16::to_le_bytes).collect();
        assert_eq!(legacy_encoding(&utf16le), Some(TextEncoding(encoding_rs::UTF_16LE)));
        let bom: Vec<u8> = [0xfe, 0xff].into_iter().chain("big end".encode_utf16().flat_map(u16::to_be_bytes)).collect();
        assert_eq!(legacy_encoding(&bom), Some(TextEncoding(encoding_rs::UTF_16BE)));
        let ja: Vec<u8> = "いろはにほへと ちりぬるを\nわかよたれそ つねならむ\n".encode_utf16().flat_map(u16::to_be_bytes).collect();
        assert_eq!(legacy_encoding(&ja), Some(TextEncoding(encoding_rs::UTF_16BE)), "UTF-16 with no ASCII to give it away");
        assert_eq!(legacy_encoding(b"already UTF-8"), None);
        assert_eq!(legacy_encoding(b"PK\x03\x04\x14\0\0\0\x08\0"), None, "binary with NULs");
        let noise: Vec<u8> = (0..2000u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8 | 1).collect();
        assert_eq!(legacy_encoding(&noise), None, "control bytes all through: binary");
    }

    #[test]
    fn a_cp1251_file_opens_as_its_text() {
        let (bytes, _, _) = encoding_rs::WINDOWS_1251.encode("Привет, мир\nвторая строка\n");
        let src = FileSource::from_bytes("ru.txt", bytes.into_owned());
        let doc = TextReader.open(&src, &ParseContext::new()).expect("opens");
        let Ok(Content::Text(t)) = doc.facet(0, &ParseContext::new()) else { panic!("text") };
        let first = t.lines(0..1).unwrap();
        assert_eq!(String::from_utf8_lossy(&first[0]).trim_end(), "Привет, мир");
        assert!(doc.metadata().iter().any(|(k, v)| k == "Encoding" && v == "windows-1251"), "{:?}", doc.metadata());
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
