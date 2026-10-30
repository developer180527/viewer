//! Structured data: JSON / JSON Lines / TOML become trees, CSV / TSV become tables.

use anyhow::Context;
use viewer_core::{Confidence, Content, Document, FileSource, FormatLoader, Result, Table, TreeNode};

pub static LOADERS: &[&dyn FormatLoader] = &[&JsonLoader, &TomlLoader, &CsvLoader];

/// Leaf values longer than this are cut in the tree view.
const MAX_VALUE_CHARS: usize = 300;

pub struct JsonLoader;

impl FormatLoader for JsonLoader {
    fn id(&self) -> &'static str {
        "json"
    }

    fn name(&self) -> &'static str {
        "JSON tree"
    }

    fn probe(&self, header: &[u8], ext: Option<&str>, _: &str) -> Confidence {
        match ext {
            Some("json" | "geojson" | "ipynb" | "jsonl" | "ndjson" | "har" | "webmanifest") => Confidence::EXTENSION,
            _ => match header.iter().find(|b| !b.is_ascii_whitespace()) {
                Some(b'{' | b'[') => Confidence::LIKELY,
                _ => Confidence::NONE,
            },
        }
    }

    fn open(&self, src: &FileSource) -> Result<Document> {
        let lines_format = matches!(src.ext().as_deref(), Some("jsonl" | "ndjson"));
        let value: serde_json::Value = if lines_format {
            let text = std::str::from_utf8(src.bytes())?;
            let items = text
                .lines()
                .enumerate()
                .filter(|(_, l)| !l.trim().is_empty())
                .map(|(i, l)| serde_json::from_str(l).with_context(|| format!("line {}", i + 1)))
                .collect::<Result<Vec<_>>>()?;
            serde_json::Value::Array(items)
        } else {
            serde_json::from_slice(src.bytes())?
        };
        let root = json_node("root".into(), &value);
        let nodes = root.count();
        let name = if lines_format { "JSON Lines" } else { "JSON" };
        Ok(Document::new(name, Content::Tree(root)).with_meta("Nodes", nodes))
    }
}

fn json_node(key: String, v: &serde_json::Value) -> TreeNode {
    use serde_json::Value;
    match v {
        Value::Object(map) => TreeNode {
            key,
            value: format!("{{{}}}", map.len()),
            children: map.iter().map(|(k, v)| json_node(k.clone(), v)).collect(),
        },
        Value::Array(items) => TreeNode {
            key,
            value: format!("[{}]", items.len()),
            children: items.iter().enumerate().map(|(i, v)| json_node(i.to_string(), v)).collect(),
        },
        Value::String(s) => leaf(key, format!("{s:?}")),
        other => leaf(key, other.to_string()),
    }
}

pub struct TomlLoader;

impl FormatLoader for TomlLoader {
    fn id(&self) -> &'static str {
        "toml"
    }

    fn name(&self) -> &'static str {
        "TOML tree"
    }

    fn probe(&self, _: &[u8], ext: Option<&str>, _: &str) -> Confidence {
        // Just above the text loader: a tree is the more useful default for config
        // files, and the highlighted source stays one click away under "View as".
        if ext == Some("toml") { Confidence(55) } else { Confidence::NONE }
    }

    fn open(&self, src: &FileSource) -> Result<Document> {
        let value: toml::Value = toml::from_str(std::str::from_utf8(src.bytes())?)?;
        let root = toml_node("root".into(), &value);
        let nodes = root.count();
        Ok(Document::new("TOML", Content::Tree(root)).with_meta("Nodes", nodes))
    }
}

fn toml_node(key: String, v: &toml::Value) -> TreeNode {
    use toml::Value;
    match v {
        Value::Table(t) => TreeNode {
            key,
            value: format!("{{{}}}", t.len()),
            children: t.iter().map(|(k, v)| toml_node(k.clone(), v)).collect(),
        },
        Value::Array(items) => TreeNode {
            key,
            value: format!("[{}]", items.len()),
            children: items.iter().enumerate().map(|(i, v)| toml_node(i.to_string(), v)).collect(),
        },
        Value::String(s) => leaf(key, format!("{s:?}")),
        other => leaf(key, other.to_string()),
    }
}

fn leaf(key: String, mut value: String) -> TreeNode {
    if let Some((cut, _)) = value.char_indices().nth(MAX_VALUE_CHARS) {
        value.truncate(cut);
        value.push('…');
    }
    TreeNode { key, value, children: Vec::new() }
}

pub struct CsvLoader;

impl FormatLoader for CsvLoader {
    fn id(&self) -> &'static str {
        "csv"
    }

    fn name(&self) -> &'static str {
        "Table"
    }

    fn probe(&self, _: &[u8], ext: Option<&str>, _: &str) -> Confidence {
        match ext {
            Some("csv" | "tsv" | "tab" | "psv") => Confidence::EXTENSION,
            _ => Confidence::NONE,
        }
    }

    fn open(&self, src: &FileSource) -> Result<Document> {
        let delimiter = match src.ext().as_deref() {
            Some("tsv" | "tab") => b'\t',
            Some("psv") => b'|',
            _ => sniff_delimiter(src.header()),
        };
        let mut reader = csv::ReaderBuilder::new()
            .delimiter(delimiter)
            .flexible(true)
            .from_reader(src.bytes());
        let headers: Vec<String> = reader.headers()?.iter().map(str::to_owned).collect();
        let rows = reader
            .records()
            .map(|r| r.map(|rec| rec.iter().map(str::to_owned).collect()))
            .collect::<std::result::Result<Vec<Vec<String>>, _>>()?;
        let table = Table::new(headers, rows);
        let (n_rows, n_cols) = (table.rows.len(), table.columns);
        Ok(Document::new("Delimited table", Content::Table(table))
            .with_meta("Rows", n_rows)
            .with_meta("Columns", n_cols)
            .with_meta("Delimiter", format!("{:?}", delimiter as char)))
    }
}

/// Picks whichever common delimiter appears most in the first line. Ties (including
/// a line with none of them) go to the earlier one in the list, so comma by default.
fn sniff_delimiter(header: &[u8]) -> u8 {
    let first = header.split(|&b| b == b'\n').next().unwrap_or_default();
    let mut best = (b',', 0);
    for d in *b",;\t|" {
        let n = first.iter().filter(|&&b| b == d).count();
        if n > best.1 {
            best = (d, n);
        }
    }
    best.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_tree_keeps_key_order() {
        let src = FileSource::from_bytes("a.json", br#"{"z":1,"a":[true,null]}"#.to_vec());
        let Content::Tree(root) = JsonLoader.open(&src).unwrap().content else { panic!() };
        assert_eq!(root.children[0].key, "z");
        assert_eq!(root.children[1].value, "[2]");
        assert_eq!(root.count(), 5);
    }

    #[test]
    fn csv_sniffs_semicolons() {
        let src = FileSource::from_bytes("a.csv", b"a;b\n1;2\n3;4\n".to_vec());
        let Content::Table(t) = CsvLoader.open(&src).unwrap().content else { panic!() };
        assert_eq!(t.headers, ["a", "b"]);
        assert_eq!(t.rows.len(), 2);
    }

    #[test]
    fn csv_without_delimiter_in_header_defaults_to_comma() {
        assert_eq!(sniff_delimiter(b"name\nx|y\n"), b',');
        assert_eq!(sniff_delimiter(b"a;b,c|d\n"), b',');
    }

    #[test]
    fn jsonl_errors_name_the_file_line() {
        let src = FileSource::from_bytes("a.jsonl", b"{}\n\n{bad\n".to_vec());
        let err = format!("{:#}", JsonLoader.open(&src).err().unwrap());
        assert!(err.starts_with("line 3:"), "{err}");
    }

    #[test]
    fn toml_tree() {
        let src = FileSource::from_bytes("a.toml", b"[pkg]\nname = \"x\"\n".to_vec());
        let Content::Tree(root) = TomlLoader.open(&src).unwrap().content else { panic!() };
        assert_eq!(root.children[0].children[0].value, "\"x\"");
    }
}
