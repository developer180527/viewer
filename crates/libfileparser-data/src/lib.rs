//! Any data file written as text into a document (docs/design/libfileparser.md §2.1):
//! structured formats as **trees**, delimited ones as **tables**, every node and row
//! linked to the lines it was written on, so an app selecting one shows the other.
//!
//! | Reader | Formats | Primary facet |
//! |---|---|---|
//! | `json` | JSON, and JSON with comments and trailing commas (`.jsonc`, or a `.json` that has them) | Tree |
//! | `json-lines` | JSON Lines (`.jsonl`, `.ndjson`) | Table of the lines' keys, else Tree; the Tree too |
//! | `json5`, `hjson` | JSON5, Hjson | Tree |
//! | `yaml` | YAML 1.2, every document | Tree |
//! | `toml` | TOML 1.1 | Tree |
//! | `xml` | XML, and the formats written in it that no other reader here owns yet; also SVG and XHTML, which other categories own | Tree |
//! | `plist` | Property lists, XML and binary | Tree |
//! | `ini`, `properties`, `env` | INI-like config, Java properties, `.env` (feature `config`) | Tree |
//! | `delimited` | CSV, TSV and other delimited text, read by row range | Table |
//!
//! Numbers are shown as written; aliases (YAML) are shown, never expanded. Every size
//! is checked against the context's limits: nodes and rows against `items`, nesting
//! against `depth`.
//!
//! The API is the shape every libfileparser category has: [`readers`], [`detect`],
//! [`open`] and [`open_with`].

#[cfg(feature = "config")]
mod config;
mod json;
mod plist;
mod table;
mod toml;
mod tree;
mod xml;
mod yaml;

use libfileparser_core::{
    Category, Confidence, Detection, Document, FileSource, FormatInfo, Model, ParseContext,
    ParseError, Reader, ReaderInfo, Result,
};

use json::Dialect;

/// Every format this category reads.
pub fn readers() -> &'static [&'static dyn Reader] {
    static READERS: &[&dyn Reader] = &[
        &PLIST,
        &JSON,
        &JSON_LINES,
        &JSON5,
        &HJSON,
        &YAML,
        &TOML,
        &XML,
        #[cfg(feature = "config")]
        &INI,
        #[cfg(feature = "config")]
        &PROPERTIES,
        #[cfg(feature = "config")]
        &ENV,
        &DELIMITED,
    ];
    READERS
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

/// One reader: its description, how it detects, how it opens.
struct Data {
    info: ReaderInfo,
    language: &'static str,
    detect: fn(&ReaderInfo, &[u8], &str) -> Confidence,
    open: fn(&FileSource, &ParseContext) -> Result<Document>,
}

impl Reader for Data {
    fn info(&self) -> &ReaderInfo {
        &self.info
    }

    fn detect(&self, header: &[u8], name: &str) -> Detection {
        match (self.detect)(&self.info, header, name) {
            Confidence::None => Detection::NONE,
            c => Detection::new(c).with_language(self.language),
        }
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        cx.cancel.check()?;
        (self.open)(src, cx)
    }
}

/// By extension: owned or also read.
fn by_extension(info: &ReaderInfo, _: &[u8], name: &str) -> Confidence {
    if info.matches_extension(name) || info.matches_also_reads(name) {
        Confidence::Extension
    } else {
        Confidence::None
    }
}

/// The file's name without its folders, lower case.
fn base(name: &str) -> String {
    name.rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .to_ascii_lowercase()
}

/// An object or array start, then what JSON can have next: not `[section]` (INI) or
/// `{{ template }}`.
fn looks_like_json(header: &[u8]) -> bool {
    let header = header.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(header);
    let mut it = header.iter().copied().filter(|b| !b.is_ascii_whitespace());
    match (it.next(), it.next()) {
        (Some(b'{'), Some(b'"' | b'}')) => true,
        (Some(b'['), Some(c)) => matches!(
            c,
            b'"' | b'{' | b'[' | b']' | b'-' | b'0'..=b'9' | b't' | b'f' | b'n'
        ),
        _ => false,
    }
}

/// The file as UTF-8 text, without a byte-order mark.
fn text<'a>(src: &'a FileSource, format: &str) -> Result<&'a str> {
    let bytes = src.bytes();
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    std::str::from_utf8(bytes).map_err(|e| {
        ParseError::corrupt(format!(
            "{format}: not UTF-8 text (byte {})",
            e.valid_up_to()
        ))
    })
}

fn format(id: &'static str, name: &str) -> FormatInfo {
    FormatInfo::new(id, name, Category::Data)
}

// ---------------------------------------------------------------- JSON family

static JSON: Data = Data {
    info: ReaderInfo::new("json", "JSON", Category::Data)
        // Formats written in JSON that no reader of their own owns yet (§2.1).
        .extensions(&[
            "json",
            "jsonc",
            "geojson",
            "topojson",
            "har",
            "ipynb",
            "webmanifest",
            "jsonld",
            "avsc",
            "arb",
            "openapi",
            "asyncapi",
        ])
        .mime_types(&["application/json"]),
    language: "json",
    detect: |info, header, name| match by_extension(info, header, name) {
        // A guess, so no stronger than another reader's extension: a `.gltf` is glTF.
        Confidence::None if looks_like_json(header) => Confidence::Extension,
        c => c,
    },
    open: |src, cx| {
        let comments = src.ext().as_deref() == Some("jsonc");
        let strict = if comments {
            None
        } else {
            Some(json::parse(src.bytes(), Dialect::Json, cx))
        };
        let (tree, name) = match strict {
            Some(Ok(tree)) => (tree, "JSON"),
            Some(Err(e)) if !e.try_next_reader() => return Err(e),
            // VS Code settings, tsconfig: comments and trailing commas in a `.json`.
            Some(Err(strict)) => match json::parse(src.bytes(), Dialect::Jsonc, cx) {
                Ok(tree) => (tree, "JSON with comments"),
                Err(_) => return Err(strict),
            },
            None => (
                json::parse(src.bytes(), Dialect::Jsonc, cx)?,
                "JSON with comments",
            ),
        };
        Ok(json::tree_document(format("json", name), tree, src, "json"))
    },
};

static JSON_LINES: Data = Data {
    info: ReaderInfo::new("json-lines", "JSON Lines", Category::Data)
        .extensions(&["jsonl", "ndjson"])
        .mime_types(&["application/x-ndjson"]),
    language: "json",
    detect: by_extension,
    open: |src, cx| {
        Ok(json::lines_document(
            json::parse_lines(src.bytes(), cx)?,
            src,
        ))
    },
};

static JSON5: Data = Data {
    info: ReaderInfo::new("json5", "JSON5", Category::Data)
        .extensions(&["json5"])
        .mime_types(&["application/json5"]),
    language: "json",
    detect: by_extension,
    open: |src, cx| {
        Ok(json::tree_document(
            format("json5", "JSON5"),
            json::parse(src.bytes(), Dialect::Json5, cx)?,
            src,
            "json",
        ))
    },
};

static HJSON: Data = Data {
    info: ReaderInfo::new("hjson", "Hjson", Category::Data).extensions(&["hjson"]),
    language: "hjson",
    detect: by_extension,
    open: |src, cx| {
        Ok(json::tree_document(
            format("hjson", "Hjson"),
            json::parse(src.bytes(), Dialect::Hjson, cx)?,
            src,
            "hjson",
        ))
    },
};

// ---------------------------------------------------------------- YAML, TOML

static YAML: Data = Data {
    info: ReaderInfo::new("yaml", "YAML", Category::Data)
        .extensions(&["yaml", "yml"])
        .mime_types(&["application/yaml"]),
    language: "yaml",
    detect: |info, header, name| match by_extension(info, header, name) {
        Confidence::None if header.starts_with(b"%YAML") => Confidence::Content,
        c => c,
    },
    open: |src, cx| {
        Ok(json::tree_document(
            format("yaml", "YAML"),
            yaml::parse(text(src, "YAML")?, cx)?,
            src,
            "yaml",
        ))
    },
};

static TOML: Data = Data {
    info: ReaderInfo::new("toml", "TOML", Category::Data)
        .extensions(&["toml"])
        .mime_types(&["application/toml"]),
    language: "toml",
    detect: |info, header, name| match base(name).as_str() {
        "cargo.lock" | "pipfile" | "poetry.lock" | "uv.lock" => Confidence::Extension,
        _ => by_extension(info, header, name),
    },
    open: |src, cx| {
        Ok(json::tree_document(
            format("toml", "TOML"),
            toml::parse(text(src, "TOML")?, cx)?,
            src,
            "toml",
        ))
    },
};

// ---------------------------------------------------------------- XML, plist

static XML: Data = Data {
    info: ReaderInfo::new("xml", "XML", Category::Data)
        // Formats written in XML that no reader of their own owns yet (§2.1).
        .extensions(&[
            "xml", "xsd", "xsl", "xslt", "rng", "sch", "wsdl", "rss", "atom", "xspf", "kml", "gpx",
            "gml", "graphml", "gexf", "gxl", "xlf", "xliff", "tmx", "resx", "xbrl", "trix", "rdf",
            "owl",
        ])
        .mime_types(&["application/xml", "text/xml"])
        .also_reads(&["svg", "xhtml", "ttml"], Model::Tree),
    language: "xml",
    detect: |info, header, name| match by_extension(info, header, name) {
        // Any XML-based format starts so: no stronger than its own reader's extension.
        Confidence::None
            if header
                .strip_prefix(b"\xEF\xBB\xBF")
                .unwrap_or(header)
                .starts_with(b"<?xml") =>
        {
            Confidence::Extension
        }
        c => c,
    },
    open: |src, cx| {
        Ok(json::tree_document(
            format("xml", "XML"),
            xml::parse(src.bytes(), cx)?,
            src,
            "xml",
        ))
    },
};

static PLIST: Data = Data {
    info: ReaderInfo::new("plist", "Property list", Category::Data)
        .extensions(&["plist"])
        .mime_types(&["application/x-plist"]),
    language: "xml",
    detect: |info, header, name| {
        if header.starts_with(b"bplist00")
            || (header.starts_with(b"<?xml")
                && memchr::memmem::find(header, b"<!DOCTYPE plist").is_some())
        {
            Confidence::Magic
        } else {
            by_extension(info, header, name)
        }
    },
    open: |src, cx| {
        let binary = src.header().starts_with(b"bplist");
        let name = if binary {
            "Property list (binary)"
        } else {
            "Property list"
        };
        let doc = json::tree_document(
            format("plist", name),
            plist::parse(src.bytes(), cx)?,
            src,
            "xml",
        );
        Ok(doc)
    },
};

// ---------------------------------------------------------------- config

#[cfg(feature = "config")]
static INI: Data = Data {
    info: ReaderInfo::new("ini", "INI", Category::Data).extensions(&[
        "ini",
        "cfg",
        "conf",
        "desktop",
        "inf",
        "editorconfig",
        "gitconfig",
    ]),
    language: "ini",
    detect: |info, header, name| {
        let base = base(name);
        let ext = base.rsplit_once('.').map(|(_, e)| e);
        match (base.as_str(), ext) {
            (
                ".editorconfig" | ".gitconfig" | ".gitmodules" | ".pypirc" | "setup.cfg"
                | "tox.ini",
                _,
            ) => Confidence::Extension,
            // Often a program's own syntax (nginx, Apache): INI only if it looks like it.
            (_, Some("conf" | "cfg")) if !config::looks_like_ini(header) => Confidence::None,
            _ => by_extension(info, header, name),
        }
    },
    open: |src, cx| {
        Ok(json::tree_document(
            format("ini", "INI"),
            config::parse_ini(text(src, "INI")?, cx)?,
            src,
            "ini",
        ))
    },
};

#[cfg(feature = "config")]
static PROPERTIES: Data = Data {
    info: ReaderInfo::new("properties", "Java properties", Category::Data)
        .extensions(&["properties"]),
    language: "properties",
    detect: by_extension,
    open: |src, cx| {
        // ISO 8859-1 by the specification, UTF-8 in practice.
        let text = String::from_utf8_lossy(src.bytes());
        Ok(json::tree_document(
            format("properties", "Java properties"),
            config::parse_properties(&text, cx)?,
            src,
            "properties",
        ))
    },
};

#[cfg(feature = "config")]
static ENV: Data = Data {
    info: ReaderInfo::new("env", "Environment file", Category::Data).extensions(&["env"]),
    language: "shell",
    detect: |info, header, name| {
        let base = base(name);
        if base == ".env" || base.starts_with(".env.") {
            Confidence::Extension
        } else {
            by_extension(info, header, name)
        }
    },
    open: |src, cx| {
        Ok(json::tree_document(
            format("env", "Environment file"),
            config::parse_env(text(src, ".env")?, cx)?,
            src,
            "shell",
        ))
    },
};

// ---------------------------------------------------------------- delimited

static DELIMITED: Data = Data {
    info: ReaderInfo::new("delimited", "Delimited text", Category::Data)
        .extensions(&["csv", "tsv", "tab", "psv", "dsv", "ssv", "dat", "prn"])
        .mime_types(&["text/csv", "text/tab-separated-values"]),
    language: "csv",
    detect: |info, header, name| match name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .as_deref()
    {
        // Anything at all: only when it has a consistent delimiter.
        Some("dat" | "prn") => {
            if table::sniff_delimiter(header, b",;\t|").is_some() {
                Confidence::Extension
            } else {
                Confidence::None
            }
        }
        _ => by_extension(info, header, name),
    },
    open: |src, cx| {
        let header = src.header();
        let delimiter = match src.ext().as_deref() {
            Some("tsv" | "tab") => b'\t',
            Some("psv") => b'|',
            Some("ssv") => table::sniff_delimiter(header, b"; ").unwrap_or(b';'),
            Some("dat" | "prn") => {
                table::sniff_delimiter(header, b",;\t|").ok_or(ParseError::NotThisFormat)?
            }
            Some("dsv") => table::sniff_delimiter(header, b",;\t|:").unwrap_or(b','),
            _ => table::sniff_delimiter(header, b",;\t|").unwrap_or(b','),
        };
        table::delimited_document(src, delimiter, cx)
    },
};
