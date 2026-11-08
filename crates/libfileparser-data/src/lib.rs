//! Any data file written as text into a document (docs/design/libfileparser.md §2.1):
//! structured formats as **trees**, delimited ones as **tables**, every node and row
//! linked to the lines it was written on, so an app selecting one shows the other.
//!
//! | Reader | Formats | Primary facet |
//! |---|---|---|
//! | `json` | JSON, and JSON with comments and trailing commas (`.jsonc`, or a `.json` that has them) | Tree |
//! | `json-lines` | JSON Lines (`.jsonl`, `.ndjson`; GELF, `.gelf`) | Table of the lines' keys, else Tree; the Tree too |
//! | `json5`, `hjson` | JSON5, Hjson | Tree |
//! | `yaml` | YAML 1.2, every document | Tree |
//! | `toml` | TOML 1.1 | Tree |
//! | `xml` | XML, and the formats written in it that no other reader here owns yet; also SVG and XHTML, which other categories own | Tree |
//! | `plist` | Property lists, XML and binary | Tree |
//! | `ini`, `properties`, `env` | INI-like config, Java properties, `.env` (feature `config`) | Tree |
//! | `log` | Logs (feature `logs`): syslog (RFC 5424 and BSD), Common and Combined access logs, logfmt, LTSV, CEF, and timestamped lines; a stack trace stays with its entry | Table of entries |
//! | `har` | HTTP Archives (feature `logs`) | Table of requests; the Tree too |
//! | `geojson`, `topojson`, `kml`, `kmz`, `gpx`, `gml`, `osm`, `wkt`, `nmea` | Geographic data (feature `geo`): features with their geometry, points and properties, and the bounds of all of them | Table of features; the Tree too |
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
mod configs;
#[cfg(feature = "feeds")]
mod feeds;
#[cfg(feature = "geo")]
mod geo;
#[cfg(feature = "graphs")]
mod graphs;
#[cfg(feature = "l10n")]
mod l10n;
#[cfg(all(feature = "rdf", not(target_arch = "wasm32")))]
mod rdf;
#[cfg(feature = "records")]
mod records;
#[cfg(feature = "schemas")]
mod schemas;
#[cfg(feature = "science")]
mod science;
#[cfg(feature = "sql")]
mod sql;
#[cfg(feature = "logs")]
mod har;
#[cfg(feature = "logs")]
mod logs;
mod blocks;
#[cfg(feature = "business")]
mod business;
mod json;
mod lines;
mod markup;
mod nodes;
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
    // Specific readers before generic ones (docs/design/libfileparser.md §2.1): a
    // HAR is JSON too, and ties go to the reader listed first.
    static READERS: &[&dyn Reader] = &[
        &PLIST,
        #[cfg(feature = "logs")]
        &HAR,
        #[cfg(feature = "geo")]
        &GEOJSON,
        #[cfg(feature = "geo")]
        &TOPOJSON,
        #[cfg(feature = "geo")]
        &KML,
        #[cfg(feature = "geo")]
        &KMZ,
        #[cfg(feature = "geo")]
        &GPX,
        #[cfg(feature = "geo")]
        &GML,
        #[cfg(feature = "geo")]
        &OSM,
        #[cfg(feature = "geo")]
        &WKT,
        #[cfg(feature = "geo")]
        &NMEA,
        #[cfg(feature = "feeds")]
        &RSS,
        #[cfg(feature = "feeds")]
        &ATOM,
        #[cfg(feature = "feeds")]
        &M3U,
        #[cfg(feature = "feeds")]
        &PLS,
        #[cfg(feature = "feeds")]
        &XSPF,
        #[cfg(feature = "l10n")]
        &PO,
        #[cfg(feature = "l10n")]
        &MO,
        #[cfg(feature = "l10n")]
        &XLIFF,
        #[cfg(feature = "l10n")]
        &TMX,
        #[cfg(feature = "l10n")]
        &RESX,
        #[cfg(feature = "l10n")]
        &STRINGS,
        #[cfg(feature = "l10n")]
        &ARB,
        #[cfg(feature = "records")]
        &VCARD,
        #[cfg(feature = "records")]
        &ICALENDAR,
        #[cfg(feature = "records")]
        &LDIF,
        #[cfg(feature = "sql")]
        &SQL,
        #[cfg(feature = "schemas")]
        &PROTOBUF,
        #[cfg(feature = "schemas")]
        &THRIFT,
        #[cfg(feature = "schemas")]
        &FLATBUFFERS,
        #[cfg(feature = "schemas")]
        &CAPNP,
        #[cfg(feature = "schemas")]
        &SMITHY,
        #[cfg(feature = "schemas")]
        &GRAPHQL,
        #[cfg(feature = "schemas")]
        &PRISMA,
        #[cfg(feature = "schemas")]
        &DBML,
        #[cfg(feature = "schemas")]
        &AVRO,
        #[cfg(feature = "schemas")]
        &OPENAPI,
        #[cfg(feature = "schemas")]
        &XSD,
        #[cfg(all(feature = "rdf", not(target_arch = "wasm32")))]
        &TURTLE,
        #[cfg(all(feature = "rdf", not(target_arch = "wasm32")))]
        &RDFXML,
        #[cfg(all(feature = "rdf", not(target_arch = "wasm32")))]
        &TRIX,
        #[cfg(all(feature = "rdf", not(target_arch = "wasm32")))]
        &JSONLD,
        #[cfg(all(feature = "rdf", not(target_arch = "wasm32")))]
        &SPARQL,
        #[cfg(feature = "graphs")]
        &DOT,
        #[cfg(feature = "graphs")]
        &GRAPHML,
        #[cfg(feature = "graphs")]
        &GEXF,
        #[cfg(feature = "graphs")]
        &GXL,
        #[cfg(feature = "graphs")]
        &GRAPH_GML,
        #[cfg(feature = "science")]
        &FASTA,
        #[cfg(feature = "science")]
        &FASTQ,
        #[cfg(feature = "science")]
        &GENBANK,
        #[cfg(feature = "science")]
        &GFF,
        #[cfg(feature = "science")]
        &BED,
        #[cfg(feature = "science")]
        &VCF,
        #[cfg(feature = "science")]
        &SAM,
        #[cfg(feature = "science")]
        &PDB,
        #[cfg(feature = "science")]
        &CIF,
        #[cfg(feature = "science")]
        &ARFF,
        #[cfg(feature = "science")]
        &DIMACS,
        #[cfg(feature = "business")]
        &X12,
        #[cfg(feature = "business")]
        &EDIFACT,
        #[cfg(feature = "business")]
        &HL7,
        #[cfg(feature = "business")]
        &OFX,
        #[cfg(feature = "business")]
        &QIF,
        #[cfg(feature = "business")]
        &FIX,
        #[cfg(feature = "business")]
        &SWIFT,
        #[cfg(feature = "business")]
        &XBRL,
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
        #[cfg(feature = "logs")]
        &LOG,
        #[cfg(feature = "config")]
        &HCL,
        #[cfg(feature = "config")]
        &CUE,
        #[cfg(feature = "config")]
        &DHALL,
        #[cfg(feature = "config")]
        &NICKEL,
        #[cfg(feature = "config")]
        &KDL,
        #[cfg(feature = "config")]
        &CRON,
        &SGML,
        &DELIMITED,
        // After delimited: a `.prn` with a delimiter is that.
        &FIXED_WIDTH,
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
            "ipynb",
            "webmanifest",
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
        .extensions(&["jsonl", "ndjson", "gelf"])
        .mime_types(&["application/x-ndjson"]),
    language: "json",
    detect: by_extension,
    open: |src, cx| {
        let (tree, warnings) = json::parse_lines(src.bytes(), cx)?;
        Ok(json::lines_document(tree, warnings, src))
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
            "xml", "xsl", "xslt", "rng", "sch", "wsdl", 
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
        .extensions(&["plist", "stringsdict"])
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

// ---------------------------------------------------------------- logs

/// The format a log's extension suggests.
#[cfg(feature = "logs")]
fn log_hint(name: &str) -> Option<logs::Format> {
    match name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).as_deref() {
        Some("ltsv") => Some(logs::Format::Ltsv),
        Some("cef") => Some(logs::Format::Cef),
        _ => None,
    }
}

#[cfg(feature = "logs")]
static LOG: Data = Data {
    info: ReaderInfo::new("log", "Log", Category::Data).extensions(&["log", "syslog", "ltsv", "cef"]),
    language: "log",
    // A `.log` of another shape (or of prose) isn't claimed: it opens as text.
    detect: |info, header, name| {
        if by_extension(info, header, name) == Confidence::None {
            return Confidence::None;
        }
        let head = String::from_utf8_lossy(header);
        let mut sample: Vec<&str> = head.lines().collect();
        if header.len() >= 8192 {
            sample.pop();
        }
        if logs::detect(&sample, log_hint(name)).is_some() { Confidence::Extension } else { Confidence::None }
    },
    open: |src, cx| logs::document(src, log_hint(src.name()), cx),
};

#[cfg(feature = "logs")]
static HAR: Data = Data {
    info: ReaderInfo::new("har", "HTTP Archive", Category::Data).extensions(&["har"]),
    language: "json",
    detect: by_extension,
    open: har::document,
};

// ---------------------------------------------------------------- geo

/// XML or not, by its first character.
fn starts_xml(header: &[u8]) -> bool {
    header.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(header).iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'<')
}

/// The extension, else (no stronger than another reader's extension, or `Content`
/// when `strong`) what the content says.
fn ext_or(info: &ReaderInfo, header: &[u8], name: &str, sniff: bool) -> Confidence {
    match by_extension(info, header, name) {
        Confidence::None if sniff => Confidence::Content,
        c => c,
    }
}

#[cfg(feature = "geo")]
static GEOJSON: Data = Data {
    info: ReaderInfo::new("geojson", "GeoJSON", Category::Data).extensions(&["geojson"]).mime_types(&["application/geo+json"]),
    language: "json",
    detect: |info, header, name| ext_or(info, header, name, geo::looks_like_geojson(header)),
    open: geo::geojson,
};

#[cfg(feature = "geo")]
static TOPOJSON: Data = Data {
    info: ReaderInfo::new("topojson", "TopoJSON", Category::Data).extensions(&["topojson"]),
    language: "json",
    detect: |info, header, name| ext_or(info, header, name, geo::looks_like_topojson(header)),
    open: geo::topojson,
};

#[cfg(feature = "geo")]
static KML: Data = Data {
    info: ReaderInfo::new("kml", "KML", Category::Data).extensions(&["kml"]).mime_types(&["application/vnd.google-earth.kml+xml"]),
    language: "xml",
    detect: |info, header, name| ext_or(info, header, name, starts_xml(header) && geo::looks_like_kml(header)),
    open: geo::kml,
};

#[cfg(feature = "geo")]
static KMZ: Data = Data {
    info: ReaderInfo::new("kmz", "KMZ", Category::Data).extensions(&["kmz"]).mime_types(&["application/vnd.google-earth.kmz"]),
    language: "xml",
    // A zip named .kmz: it outranks the zip reader.
    detect: |info, header, name| {
        if header.starts_with(b"PK\x03\x04") && info.matches_extension(name) { Confidence::Certain } else { Confidence::None }
    },
    open: geo::kmz,
};

#[cfg(feature = "geo")]
static GPX: Data = Data {
    info: ReaderInfo::new("gpx", "GPX", Category::Data).extensions(&["gpx"]).mime_types(&["application/gpx+xml"]),
    language: "xml",
    detect: |info, header, name| ext_or(info, header, name, starts_xml(header) && geo::looks_like_gpx(header)),
    open: geo::gpx,
};

#[cfg(feature = "geo")]
static GML: Data = Data {
    info: ReaderInfo::new("gml", "GML", Category::Data).extensions(&["gml"]).mime_types(&["application/gml+xml"]),
    language: "xml",
    // `.gml` is also Graph Modelling Language: only XML that names GML is this.
    detect: |_, header, _| if geo::looks_like_gml(header) { Confidence::Content } else { Confidence::None },
    open: geo::gml,
};

#[cfg(feature = "geo")]
static OSM: Data = Data {
    info: ReaderInfo::new("osm", "OpenStreetMap XML", Category::Data).extensions(&["osm"]),
    language: "xml",
    detect: |info, header, name| {
        if !starts_xml(header) {
            return Confidence::None;
        }
        ext_or(info, header, name, geo::looks_like_osm(header))
    },
    open: geo::osm,
};

#[cfg(feature = "geo")]
static WKT: Data = Data {
    info: ReaderInfo::new("wkt", "WKT", Category::Data).extensions(&["wkt"]),
    language: "wkt",
    detect: by_extension,
    open: geo::wkt_document,
};

#[cfg(feature = "geo")]
static NMEA: Data = Data {
    info: ReaderInfo::new("nmea", "NMEA 0183", Category::Data).extensions(&["nmea"]),
    language: "nmea",
    detect: |info, header, name| if geo::looks_like_nmea(header) { ext_or(info, header, name, true) } else { Confidence::None },
    open: geo::nmea_document,
};

// ---------------------------------------------------------------- feeds

/// Whether the header holds `needle`.
fn has(header: &[u8], needle: &[u8]) -> bool {
    memchr::memmem::find(header, needle).is_some()
}

#[cfg(feature = "feeds")]
static RSS: Data = Data {
    info: ReaderInfo::new("rss", "RSS", Category::Data).extensions(&["rss"]).mime_types(&["application/rss+xml"]),
    language: "xml",
    detect: |info, header, name| ext_or(info, header, name, starts_xml(header) && feeds::looks_like_rss(header)),
    open: feeds::rss,
};

#[cfg(feature = "feeds")]
static ATOM: Data = Data {
    info: ReaderInfo::new("atom", "Atom", Category::Data).extensions(&["atom"]).mime_types(&["application/atom+xml"]),
    language: "xml",
    detect: |info, header, name| ext_or(info, header, name, starts_xml(header) && feeds::looks_like_atom(header)),
    open: feeds::atom,
};

#[cfg(feature = "feeds")]
static M3U: Data = Data {
    info: ReaderInfo::new("m3u", "M3U playlist", Category::Data).extensions(&["m3u", "m3u8"]).mime_types(&["audio/x-mpegurl", "application/vnd.apple.mpegurl"]),
    language: "m3u",
    detect: |info, header, name| ext_or(info, header, name, header.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(header).starts_with(b"#EXTM3U")),
    open: feeds::m3u,
};

#[cfg(feature = "feeds")]
static PLS: Data = Data {
    info: ReaderInfo::new("pls", "PLS playlist", Category::Data).extensions(&["pls"]).mime_types(&["audio/x-scpls"]),
    language: "ini",
    detect: |info, header, name| ext_or(info, header, name, header.len() >= 10 && header[..10].eq_ignore_ascii_case(b"[playlist]")),
    open: |src, _| feeds::pls(src),
};

#[cfg(feature = "feeds")]
static XSPF: Data = Data {
    info: ReaderInfo::new("xspf", "XSPF playlist", Category::Data).extensions(&["xspf"]).mime_types(&["application/xspf+xml"]),
    language: "xml",
    detect: |info, header, name| ext_or(info, header, name, starts_xml(header) && has(header, b"xspf.org/ns")),
    open: feeds::xspf,
};

// ---------------------------------------------------------------- l10n

#[cfg(feature = "l10n")]
static PO: Data = Data {
    info: ReaderInfo::new("po", "gettext catalog", Category::Data).extensions(&["po", "pot"]).mime_types(&["text/x-gettext-translation"]),
    language: "po",
    detect: by_extension,
    open: l10n::po,
};

#[cfg(feature = "l10n")]
static MO: Data = Data {
    info: ReaderInfo::new("mo", "gettext compiled catalog", Category::Data).extensions(&["mo", "gmo"]),
    language: "po",
    detect: |_, header, _| if l10n::looks_like_mo(header) { Confidence::Magic } else { Confidence::None },
    open: l10n::mo,
};

#[cfg(feature = "l10n")]
static XLIFF: Data = Data {
    info: ReaderInfo::new("xliff", "XLIFF", Category::Data).extensions(&["xlf", "xliff"]).mime_types(&["application/xliff+xml"]),
    language: "xml",
    detect: |info, header, name| ext_or(info, header, name, starts_xml(header) && l10n::looks_like_xliff(header)),
    open: l10n::xliff,
};

#[cfg(feature = "l10n")]
static TMX: Data = Data {
    info: ReaderInfo::new("tmx", "Translation memory", Category::Data).extensions(&["tmx"]),
    language: "xml",
    detect: |info, header, name| ext_or(info, header, name, starts_xml(header) && has(header, b"<tmx")),
    open: l10n::tmx,
};

#[cfg(feature = "l10n")]
static RESX: Data = Data {
    info: ReaderInfo::new("resx", ".NET resources", Category::Data).extensions(&["resx", "resw"]),
    language: "xml",
    detect: by_extension,
    open: l10n::resx,
};

#[cfg(feature = "l10n")]
static STRINGS: Data = Data {
    info: ReaderInfo::new("strings", "Strings file", Category::Data).extensions(&["strings"]),
    language: "strings",
    // A binary plist named .strings is the plist reader's.
    detect: |info, header, name| if header.starts_with(b"bplist") { Confidence::None } else { by_extension(info, header, name) },
    open: |src, _| l10n::strings(src),
};

#[cfg(feature = "l10n")]
static ARB: Data = Data {
    info: ReaderInfo::new("arb", "Application Resource Bundle", Category::Data).extensions(&["arb"]),
    language: "json",
    detect: by_extension,
    open: l10n::arb,
};

// ---------------------------------------------------------------- records

#[cfg(feature = "records")]
static VCARD: Data = Data {
    info: ReaderInfo::new("vcard", "vCard", Category::Data).extensions(&["vcf", "vcard"]).mime_types(&["text/vcard"]),
    language: "vcard",
    // `.vcf` is also variant calls (science): only a file that begins a card is this.
    detect: |_, header, _| if has(&header[..header.len().min(1024)], b"BEGIN:VCARD") { Confidence::Content } else { Confidence::None },
    open: records::vcard,
};

#[cfg(feature = "records")]
static ICALENDAR: Data = Data {
    info: ReaderInfo::new("icalendar", "iCalendar", Category::Data).extensions(&["ics", "ical", "ifb", "icalendar"]).mime_types(&["text/calendar"]),
    language: "ics",
    detect: |info, header, name| ext_or(info, header, name, header.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(header).starts_with(b"BEGIN:VCALENDAR")),
    open: records::icalendar,
};

#[cfg(feature = "records")]
static LDIF: Data = Data {
    info: ReaderInfo::new("ldif", "LDIF", Category::Data).extensions(&["ldif"]).mime_types(&["text/x-ldif"]),
    language: "ldif",
    detect: |info, header, name| if records::looks_like_ldif(header) { ext_or(info, header, name, false) } else { Confidence::None },
    open: records::ldif,
};

// ---------------------------------------------------------------- sql

#[cfg(feature = "sql")]
static SQL: Data = Data {
    info: ReaderInfo::new("sql", "SQL", Category::Data).extensions(&["sql", "cql", "dump", "ddl"]).mime_types(&["application/sql"]),
    language: "sql",
    // `.dump` is anything: SQL only when it reads as SQL.
    detect: |info, header, name| match name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).as_deref() {
        Some("dump") => if sql::looks_like_sql(header) { Confidence::Extension } else { Confidence::None },
        _ => by_extension(info, header, name),
    },
    open: sql::document,
};

// ---------------------------------------------------------------- schemas

#[cfg(feature = "schemas")]
static PROTOBUF: Data = Data {
    info: ReaderInfo::new("protobuf", "Protocol Buffers", Category::Data).extensions(&["proto"]),
    language: "protobuf",
    detect: by_extension,
    open: |src, cx| schemas::block_document("protobuf", src, cx),
};

#[cfg(feature = "schemas")]
static THRIFT: Data = Data {
    info: ReaderInfo::new("thrift", "Thrift", Category::Data).extensions(&["thrift"]),
    language: "thrift",
    detect: by_extension,
    open: |src, cx| schemas::block_document("thrift", src, cx),
};

#[cfg(feature = "schemas")]
static FLATBUFFERS: Data = Data {
    info: ReaderInfo::new("flatbuffers", "FlatBuffers schema", Category::Data).extensions(&["fbs"]),
    language: "flatbuffers",
    detect: by_extension,
    open: |src, cx| schemas::block_document("flatbuffers", src, cx),
};

#[cfg(feature = "schemas")]
static CAPNP: Data = Data {
    info: ReaderInfo::new("capnp", "Cap'n Proto schema", Category::Data).extensions(&["capnp"]),
    language: "capnp",
    detect: by_extension,
    open: |src, cx| schemas::block_document("capnp", src, cx),
};

#[cfg(feature = "schemas")]
static SMITHY: Data = Data {
    info: ReaderInfo::new("smithy", "Smithy model", Category::Data).extensions(&["smithy"]),
    language: "smithy",
    detect: by_extension,
    open: |src, cx| schemas::block_document("smithy", src, cx),
};

#[cfg(feature = "schemas")]
static GRAPHQL: Data = Data {
    info: ReaderInfo::new("graphql", "GraphQL schema", Category::Data).extensions(&["graphql", "gql", "graphqls"]),
    language: "graphql",
    detect: by_extension,
    open: |src, cx| schemas::block_document("graphql", src, cx),
};

#[cfg(feature = "schemas")]
static PRISMA: Data = Data {
    info: ReaderInfo::new("prisma", "Prisma schema", Category::Data).extensions(&["prisma"]),
    language: "prisma",
    detect: by_extension,
    open: |src, cx| schemas::block_document("prisma", src, cx),
};

#[cfg(feature = "schemas")]
static DBML: Data = Data {
    info: ReaderInfo::new("dbml", "DBML", Category::Data).extensions(&["dbml"]),
    language: "dbml",
    detect: by_extension,
    open: |src, cx| schemas::block_document("dbml", src, cx),
};

#[cfg(feature = "schemas")]
static AVRO: Data = Data {
    info: ReaderInfo::new("avro", "Avro schema", Category::Data).extensions(&["avsc"]),
    language: "json",
    detect: by_extension,
    open: schemas::avro,
};

#[cfg(feature = "schemas")]
static OPENAPI: Data = Data {
    info: ReaderInfo::new("openapi", "OpenAPI", Category::Data).extensions(&["openapi", "asyncapi"]),
    language: "yaml",
    // An `openapi.yaml` or `api.json`: told by its top-level key, so it outranks YAML and JSON.
    detect: |info, header, name| {
        let api = schemas::looks_like_api(header);
        match by_extension(info, header, name) {
            Confidence::None if api => Confidence::Content,
            c => c,
        }
    },
    open: schemas::api,
};

#[cfg(feature = "schemas")]
static XSD: Data = Data {
    info: ReaderInfo::new("xsd", "XML Schema", Category::Data).extensions(&["xsd"]).mime_types(&["application/xml-schema"]),
    language: "xml",
    detect: |info, header, name| ext_or(info, header, name, starts_xml(header) && (has(header, b"<xs:schema") || has(header, b"<xsd:schema"))),
    open: schemas::xsd,
};

// ---------------------------------------------------------------- rdf

#[cfg(all(feature = "rdf", not(target_arch = "wasm32")))]
static TURTLE: Data = Data {
    info: ReaderInfo::new("turtle", "Turtle", Category::Data)
        .extensions(&["ttl", "trig", "nt", "nq", "ntriples", "nquads"])
        .mime_types(&["text/turtle", "application/trig", "application/n-triples", "application/n-quads"]),
    language: "turtle",
    detect: by_extension,
    open: rdf::turtle,
};

#[cfg(all(feature = "rdf", not(target_arch = "wasm32")))]
static RDFXML: Data = Data {
    info: ReaderInfo::new("rdfxml", "RDF/XML", Category::Data).extensions(&["rdf", "owl", "rdfs"]).mime_types(&["application/rdf+xml"]),
    language: "xml",
    detect: |info, header, name| ext_or(info, header, name, starts_xml(header) && has(header, b"<rdf:RDF")),
    open: rdf::rdfxml,
};

#[cfg(all(feature = "rdf", not(target_arch = "wasm32")))]
static TRIX: Data = Data {
    info: ReaderInfo::new("trix", "TriX", Category::Data).extensions(&["trix"]),
    language: "xml",
    detect: |info, header, name| ext_or(info, header, name, starts_xml(header) && has(header, b"<TriX")),
    open: rdf::trix,
};

#[cfg(all(feature = "rdf", not(target_arch = "wasm32")))]
static JSONLD: Data = Data {
    info: ReaderInfo::new("jsonld", "JSON-LD", Category::Data).extensions(&["jsonld"]).mime_types(&["application/ld+json"]),
    language: "json",
    detect: by_extension,
    open: rdf::jsonld,
};

#[cfg(all(feature = "rdf", not(target_arch = "wasm32")))]
static SPARQL: Data = Data {
    info: ReaderInfo::new("sparql", "SPARQL query", Category::Data).extensions(&["rq", "sparql", "ru"]).mime_types(&["application/sparql-query"]),
    language: "sparql",
    detect: by_extension,
    open: rdf::sparql,
};

// ---------------------------------------------------------------- graphs

#[cfg(feature = "graphs")]
static DOT: Data = Data {
    info: ReaderInfo::new("dot", "Graphviz DOT", Category::Data).extensions(&["dot", "gv"]).mime_types(&["text/vnd.graphviz"]),
    language: "dot",
    detect: |info, header, name| ext_or(info, header, name, graphs::looks_like_dot(header)),
    open: graphs::dot_document,
};

#[cfg(feature = "graphs")]
static GRAPHML: Data = Data {
    info: ReaderInfo::new("graphml", "GraphML", Category::Data).extensions(&["graphml"]).mime_types(&["application/graphml+xml"]),
    language: "xml",
    detect: |info, header, name| ext_or(info, header, name, starts_xml(header) && has(header, b"<graphml")),
    open: graphs::graphml,
};

#[cfg(feature = "graphs")]
static GEXF: Data = Data {
    info: ReaderInfo::new("gexf", "GEXF", Category::Data).extensions(&["gexf"]),
    language: "xml",
    detect: |info, header, name| ext_or(info, header, name, starts_xml(header) && has(header, b"<gexf")),
    open: graphs::gexf,
};

#[cfg(feature = "graphs")]
static GXL: Data = Data {
    info: ReaderInfo::new("gxl", "GXL", Category::Data).extensions(&["gxl"]),
    language: "xml",
    detect: |info, header, name| ext_or(info, header, name, starts_xml(header) && has(header, b"<gxl")),
    open: graphs::gxl,
};

#[cfg(feature = "graphs")]
static GRAPH_GML: Data = Data {
    info: ReaderInfo::new("graph-gml", "Graph Modelling Language", Category::Data).extensions(&["gml"]),
    language: "gml",
    // `.gml` is also Geography Markup (XML): only `graph [` is this.
    detect: |_, header, _| if !starts_xml(header) && graphs::looks_like_graph_gml(header) { Confidence::Content } else { Confidence::None },
    open: graphs::graph_gml,
};

// ---------------------------------------------------------------- science

/// PDB's record names on its first lines (`.ent` is also an XML entity file).
#[cfg(feature = "science")]
fn science_pdb_like(header: &[u8]) -> bool {
    const RECORDS: [&[u8]; 9] = [b"HEADER", b"TITLE ", b"COMPND", b"REMARK", b"CRYST1", b"ATOM  ", b"HETATM", b"MODEL ", b"SEQRES"];
    header.split(|&c| c == b'\n').find(|l| !l.is_empty()).is_some_and(|l| RECORDS.iter().any(|r| l.starts_with(r)))
}

#[cfg(feature = "science")]
static FASTA: Data = Data {
    info: ReaderInfo::new("fasta", "FASTA", Category::Data).extensions(&["fasta", "fa", "fna", "faa", "ffn", "frn", "fas", "mpfa"]),
    language: "fasta",
    detect: by_extension,
    open: science::fasta,
};

#[cfg(feature = "science")]
static FASTQ: Data = Data {
    info: ReaderInfo::new("fastq", "FASTQ", Category::Data).extensions(&["fastq", "fq"]),
    language: "fastq",
    detect: by_extension,
    open: science::fastq,
};

#[cfg(feature = "science")]
static GENBANK: Data = Data {
    info: ReaderInfo::new("genbank", "GenBank", Category::Data).extensions(&["gb", "gbk", "genbank", "gbff", "gp", "gpff", "gbwithparts"]),
    language: "genbank",
    detect: |info, header, name| ext_or(info, header, name, header.starts_with(b"LOCUS ")),
    open: science::genbank,
};

#[cfg(feature = "science")]
static GFF: Data = Data {
    info: ReaderInfo::new("gff", "GFF3", Category::Data).extensions(&["gff", "gff3", "gtf"]),
    language: "gff",
    detect: |info, header, name| ext_or(info, header, name, header.starts_with(b"##gff-version")),
    open: science::gff,
};

#[cfg(feature = "science")]
static BED: Data = Data {
    info: ReaderInfo::new("bed", "BED", Category::Data).extensions(&["bed"]),
    language: "bed",
    detect: by_extension,
    open: science::bed,
};

#[cfg(feature = "science")]
static VCF: Data = Data {
    info: ReaderInfo::new("vcf", "Variant Call Format", Category::Data).extensions(&["vcf"]),
    language: "vcf",
    detect: |_, header, _| if header.starts_with(b"##fileformat=VCF") { Confidence::Content } else { Confidence::None },
    open: science::vcf,
};

#[cfg(feature = "science")]
static SAM: Data = Data {
    info: ReaderInfo::new("sam", "SAM", Category::Data).extensions(&["sam"]),
    language: "sam",
    detect: |info, header, name| ext_or(info, header, name, header.starts_with(b"@HD\t")),
    open: science::sam,
};

#[cfg(feature = "science")]
static PDB: Data = Data {
    info: ReaderInfo::new("pdb", "Protein Data Bank", Category::Data).extensions(&["pdb", "ent"]),
    language: "pdb",
    detect: |info, header, name| if science_pdb_like(header) { ext_or(info, header, name, true) } else { Confidence::None },
    open: science::pdb,
};

#[cfg(feature = "science")]
static CIF: Data = Data {
    info: ReaderInfo::new("cif", "Crystallographic Information File", Category::Data).extensions(&["cif", "mmcif", "mcif"]),
    language: "cif",
    detect: |info, header, name| ext_or(info, header, name, header.starts_with(b"data_")),
    open: science::cif,
};

#[cfg(feature = "science")]
static ARFF: Data = Data {
    info: ReaderInfo::new("arff", "ARFF", Category::Data).extensions(&["arff"]),
    language: "arff",
    detect: by_extension,
    open: science::arff,
};

#[cfg(feature = "science")]
static DIMACS: Data = Data {
    info: ReaderInfo::new("dimacs", "DIMACS", Category::Data).extensions(&["dimacs", "cnf", "wcnf"]),
    language: "dimacs",
    detect: by_extension,
    open: science::dimacs,
};

// ---------------------------------------------------------------- business

/// The header after a byte-order mark and white space.
#[cfg(feature = "business")]
fn lead(header: &[u8]) -> &[u8] {
    let h = header.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(header);
    &h[h.iter().position(|b| !b.is_ascii_whitespace()).unwrap_or(h.len())..]
}

#[cfg(feature = "business")]
static X12: Data = Data {
    info: ReaderInfo::new("x12", "ANSI X12", Category::Data).extensions(&["x12", "edi"]),
    language: "edi",
    // `.edi` is X12 or EDIFACT: its first segment says which.
    detect: |_, header, _| if lead(header).starts_with(b"ISA") { Confidence::Content } else { Confidence::None },
    open: business::x12,
};

#[cfg(feature = "business")]
static EDIFACT: Data = Data {
    info: ReaderInfo::new("edifact", "UN/EDIFACT", Category::Data).extensions(&["edi", "edifact"]),
    language: "edi",
    detect: |_, header, _| if lead(header).starts_with(b"UNA") || lead(header).starts_with(b"UNB+") { Confidence::Content } else { Confidence::None },
    open: business::edifact,
};

#[cfg(feature = "business")]
static HL7: Data = Data {
    info: ReaderInfo::new("hl7", "HL7 v2", Category::Data).extensions(&["hl7"]),
    language: "hl7",
    detect: |info, header, name| ext_or(info, header, name, lead(header).starts_with(b"MSH|")),
    open: business::hl7,
};

#[cfg(feature = "business")]
static OFX: Data = Data {
    info: ReaderInfo::new("ofx", "OFX", Category::Data).extensions(&["ofx", "qfx"]).mime_types(&["application/x-ofx"]),
    language: "sgml",
    detect: |info, header, name| ext_or(info, header, name, lead(header).starts_with(b"OFXHEADER") || has(header, b"<?OFX")),
    open: business::ofx,
};

#[cfg(feature = "business")]
static QIF: Data = Data {
    info: ReaderInfo::new("qif", "Quicken Interchange Format", Category::Data).extensions(&["qif"]),
    language: "qif",
    detect: |info, header, name| ext_or(info, header, name, lead(header).starts_with(b"!Type:")),
    open: business::qif,
};

#[cfg(feature = "business")]
static FIX: Data = Data {
    info: ReaderInfo::new("fix", "FIX messages", Category::Data).extensions(&["fix"]),
    language: "fix",
    detect: |info, header, name| ext_or(info, header, name, lead(header).starts_with(b"8=FIX")),
    open: business::fix,
};

#[cfg(feature = "business")]
static SWIFT: Data = Data {
    info: ReaderInfo::new("swift", "SWIFT MT", Category::Data).extensions(&["fin", "swift", "mt940", "940", "sta"]),
    language: "swift",
    // `.fin` is anything: only a `{1:` basic header is this.
    // `{1:F01…`: the basic header, a letter after the colon (not JSON's `{1:2}`); or,
    // named as a statement, its fields without the envelope.
    detect: |info, header, name| {
        let l = lead(header);
        if l.starts_with(b"{1:") && l.get(3).is_some_and(u8::is_ascii_alphabetic) {
            ext_or(info, header, name, true)
        } else if info.matches_extension(name) && business::looks_like_bare_mt(header) {
            Confidence::Extension
        } else {
            Confidence::None
        }
    },
    open: business::swift,
};

#[cfg(feature = "business")]
static XBRL: Data = Data {
    info: ReaderInfo::new("xbrl", "XBRL", Category::Data).extensions(&["xbrl"]),
    language: "xml",
    detect: |info, header, name| ext_or(info, header, name, starts_xml(header) && has(header, b"<xbrli:xbrl")),
    open: business::xbrl,
};

// ---------------------------------------------------------------- more config, SGML, fixed-width

#[cfg(feature = "config")]
static HCL: Data = Data {
    info: ReaderInfo::new("hcl", "HCL", Category::Data).extensions(&["hcl", "tf", "tfvars", "nomad"]),
    language: "hcl",
    detect: by_extension,
    open: configs::hcl,
};

#[cfg(feature = "config")]
static CUE: Data = Data {
    info: ReaderInfo::new("cue", "CUE", Category::Data).extensions(&["cue"]),
    language: "cue",
    detect: by_extension,
    open: |src, cx| configs::fields("cue", src, cx),
};

#[cfg(feature = "config")]
static DHALL: Data = Data {
    info: ReaderInfo::new("dhall", "Dhall", Category::Data).extensions(&["dhall"]),
    language: "dhall",
    detect: by_extension,
    open: |src, cx| configs::fields("dhall", src, cx),
};

#[cfg(feature = "config")]
static NICKEL: Data = Data {
    info: ReaderInfo::new("nickel", "Nickel", Category::Data).extensions(&["ncl"]),
    language: "nickel",
    detect: by_extension,
    open: |src, cx| configs::fields("nickel", src, cx),
};

#[cfg(feature = "config")]
static KDL: Data = Data {
    info: ReaderInfo::new("kdl", "KDL", Category::Data).extensions(&["kdl"]),
    language: "kdl",
    detect: by_extension,
    open: configs::kdl,
};

#[cfg(feature = "config")]
static CRON: Data = Data {
    info: ReaderInfo::new("cron", "Crontab", Category::Data).extensions(&["cron", "crontab"]),
    language: "crontab",
    detect: |info, header, name| if base(name) == "crontab" { Confidence::Extension } else { by_extension(info, header, name) },
    open: configs::cron,
};

static SGML: Data = Data {
    info: ReaderInfo::new("sgml", "SGML", Category::Data).extensions(&["sgml", "sgm"]),
    language: "sgml",
    detect: by_extension,
    open: configs::sgml,
};

static FIXED_WIDTH: Data = Data {
    info: ReaderInfo::new("fixed-width", "Fixed-width table", Category::Data).extensions(&["fwf", "prn"]),
    language: "text",
    detect: by_extension,
    open: configs::fixed_width,
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
