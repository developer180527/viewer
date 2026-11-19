//! Jupyter notebooks (`.ipynb`, nbformat 2 to 4) as a document: Markdown cells
//! through the Markdown reader, code cells as code in the kernel's language, and
//! their outputs after them. An output's HTML (a pandas table, say) becomes formatted
//! text through `crate::html`; its plain text is shown as written; an image is named,
//! not shown yet. Nothing in a notebook is run.

use std::sync::Arc;

use libfileparser_core::{Block, Category, Confidence, Content, Detection, Document, FileSource, FormatInfo, Inline, OwnedFlow, ParseContext, ParseError, Reader, ReaderInfo, Result, Role, Warning, is_text};
use serde_json::Value;

use crate::build::Builder;

static INFO: ReaderInfo = ReaderInfo::new("ipynb", "Jupyter notebook", Category::Docs).extensions(&["ipynb"]).mime_types(&["application/x-ipynb+json"]);

pub struct NotebookReader;

impl Reader for NotebookReader {
    fn info(&self) -> &ReaderInfo {
        &INFO
    }

    /// By extension, with a JSON object's opening brace; content alone when the
    /// header names nbformat.
    fn detect(&self, header: &[u8], name: &str) -> Detection {
        let head = String::from_utf8_lossy(&header[..header.len().min(4096)]);
        let object = head.trim_start_matches('\u{feff}').trim_start().starts_with('{');
        if !object || !is_text(header) {
            return Detection::NONE;
        }
        if INFO.matches_extension(name) {
            Detection::new(Confidence::Content).with_language("json")
        } else if head.contains("\"nbformat\"") && (head.contains("\"cells\"") || head.contains("\"worksheets\"")) {
            Detection::new(Confidence::Content).with_language("json")
        } else {
            Detection::NONE
        }
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        cx.cancel.check()?;
        let bytes = crate::whole(src, cx)?;
        let nb: Value = serde_json::from_slice(&bytes).map_err(|e| ParseError::corrupt(format!("not JSON: {e}")))?;
        let Some(top) = nb.as_object() else { return Err(ParseError::NotThisFormat) };
        if !top.contains_key("nbformat") && !top.contains_key("cells") && !top.contains_key("worksheets") {
            return Err(ParseError::NotThisFormat);
        }
        let meta = nb.get("metadata");
        let language = meta
            .and_then(|m| m.pointer("/kernelspec/language").or_else(|| m.pointer("/language_info/name")).or_else(|| m.get("language")))
            .and_then(Value::as_str)
            .map(str::to_lowercase)
            .or_else(|| meta.and_then(|m| m.pointer("/kernelspec/name")).and_then(Value::as_str).map(|k| if k.starts_with("python") { "python".into() } else { k.to_lowercase() }));
        // nbformat 4 has `cells`; 2 and 3 have `worksheets[].cells`.
        let cells: Vec<&Value> = match nb.get("cells").and_then(Value::as_array) {
            Some(c) => c.iter().collect(),
            None => nb.get("worksheets").and_then(Value::as_array).into_iter().flatten().filter_map(|w| w.get("cells").and_then(Value::as_array)).flatten().collect(),
        };
        let mut b = Builder::new(cx);
        let (mut code_cells, mut images) = (0usize, 0usize);
        for (n, cell) in cells.iter().enumerate() {
            if n % 64 == 0 {
                cx.cancel.check()?;
            }
            let kind = cell.get("cell_type").and_then(Value::as_str).unwrap_or("");
            // v4 `source`, v3 code cells' `input`; a string or a list of lines.
            let source = text(cell.get("source").or_else(|| cell.get("input")));
            match kind {
                "markdown" => {
                    for block in crate::markdown::parse(&source, cx)? {
                        b.block(block)?;
                    }
                }
                "heading" => {
                    // nbformat 3's heading cells.
                    let level = cell.get("level").and_then(Value::as_u64).unwrap_or(1).clamp(1, 6) as u8;
                    b.block(Block::Heading { level, text: vec![Inline::Text(source.trim().to_string())] })?;
                }
                "code" => {
                    code_cells += 1;
                    let lang = cell.get("language").and_then(Value::as_str).map(str::to_lowercase).or_else(|| language.clone());
                    let count = cell.get("execution_count").or_else(|| cell.get("prompt_number")).and_then(Value::as_u64);
                    if let Some(c) = count {
                        b.block(Block::Paragraph(vec![Inline::Code(format!("In [{c}]:"))]))?;
                    }
                    b.block(Block::Code { language: lang, text: source.trim_end_matches('\n').to_string() })?;
                    for output in cell.get("outputs").and_then(Value::as_array).into_iter().flatten() {
                        images += out(output, src.name(), &mut b)?;
                    }
                }
                _ => {
                    // Raw cells, and kinds a later nbformat may add: as written.
                    if !source.trim().is_empty() {
                        b.block(Block::Code { language: None, text: source.trim_end_matches('\n').to_string() })?;
                    }
                }
            }
        }
        let mut doc = Document::new(FormatInfo::new("ipynb", "Jupyter notebook", Category::Docs))
            .with_facet(Role::Primary, "Notebook", Content::Flow(Arc::new(OwnedFlow::new(b.finish()?))))
            .with_meta("Cells", cells.len())
            .with_meta("Code cells", code_cells);
        if let Some(l) = &language {
            doc = doc.with_meta("Language", l);
        }
        if let Some(v) = nb.get("nbformat").and_then(Value::as_u64) {
            let minor = nb.get("nbformat_minor").and_then(Value::as_u64).map_or(String::new(), |m| format!(".{m}"));
            doc = doc.with_meta("Format", format!("nbformat {v}{minor}"));
        }
        if let Some(k) = meta.and_then(|m| m.pointer("/kernelspec/display_name")).and_then(Value::as_str) {
            doc = doc.with_meta("Kernel", k);
        }
        if let Some(t) = meta.and_then(|m| m.get("title")).and_then(Value::as_str) {
            doc = doc.with_meta("Title", t);
        }
        if images > 0 {
            doc = doc.with_warning(Warning::new(format!("{images} image output(s) named but not shown yet.")));
        }
        Ok(doc.with_file_facets(src, Some("json")))
    }
}

/// A notebook string: a string, or a list of lines to join.
fn text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).collect(),
        _ => String::new(),
    }
}

/// One output of a code cell; how many images it held.
fn out(o: &Value, base: &str, b: &mut Builder) -> Result<usize> {
    let kind = o.get("output_type").and_then(Value::as_str).unwrap_or("");
    match kind {
        "stream" => {
            let t = text(o.get("text"));
            if !t.trim().is_empty() {
                b.block(Block::Code { language: None, text: strip_ansi(t.trim_end_matches('\n')) })?;
            }
            Ok(0)
        }
        "error" | "pyerr" => {
            let trace: Vec<String> = o.get("traceback").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).map(strip_ansi).collect();
            let head = format!("{}: {}", o.get("ename").and_then(Value::as_str).unwrap_or("Error"), o.get("evalue").and_then(Value::as_str).unwrap_or(""));
            b.block(Block::Code { language: None, text: if trace.is_empty() { head } else { trace.join("\n") } })?;
            Ok(0)
        }
        _ => {
            // execute_result, display_data (v4); pyout, display_data (v3, whose
            // types sit on the output itself, by short name).
            let data = o.get("data").unwrap_or(o);
            let get = |keys: &[&str]| keys.iter().find_map(|k| data.get(*k)).map(|v| text(Some(v)));
            if let Some(html) = get(&["text/html", "html"]) {
                crate::html::parse(&html, base, b)?;
                return Ok(0);
            }
            if let Some(md) = get(&["text/markdown"]) {
                for block in crate::markdown::parse(&md, b.context())? {
                    b.block(block)?;
                }
                return Ok(0);
            }
            let image = ["image/png", "image/jpeg", "image/svg+xml", "image/gif", "png", "jpeg", "svg"].into_iter().find(|k| data.get(*k).is_some());
            if let Some(kind) = image {
                b.block(Block::Paragraph(vec![Inline::Emphasis(vec![Inline::Text(format!("[Image output: {kind}]"))])]))?;
                return Ok(1);
            }
            if let Some(t) = get(&["text/plain", "text", "text/latex", "latex"]) {
                if !t.trim().is_empty() {
                    b.block(Block::Code { language: None, text: strip_ansi(t.trim_end_matches('\n')) })?;
                }
            }
            Ok(0)
        }
    }
}

/// Text without terminal colour codes (`ESC [ … m`), which tracebacks carry.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for d in chars.by_ref() {
                if d.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const NB: &str = r##"{"nbformat": 4, "nbformat_minor": 5, "metadata": {"kernelspec": {"display_name": "Python 3", "language": "python", "name": "python3"}},
      "cells": [
        {"cell_type": "markdown", "metadata": {}, "source": ["# Analysis\n", "Some *text*."]},
        {"cell_type": "code", "execution_count": 1, "metadata": {}, "source": "import pandas as pd\ndf", "outputs": [
          {"output_type": "stream", "name": "stdout", "text": ["hello\n"]},
          {"output_type": "execute_result", "execution_count": 1, "metadata": {}, "data": {"text/plain": ["   a\n0  1"], "text/html": ["<table><tr><th>a</th></tr><tr><td>1</td></tr></table>"]}},
          {"output_type": "display_data", "metadata": {}, "data": {"image/png": "iVBORw0KGgo=", "text/plain": ["<Figure>"]}},
          {"output_type": "error", "ename": "ValueError", "evalue": "bad", "traceback": ["\u001b[0;31mValueError\u001b[0m: bad"]}
        ]},
        {"cell_type": "raw", "metadata": {}, "source": "raw text"}
      ]}"##;

    #[test]
    fn a_notebook_reads_as_a_document() {
        let src = FileSource::from_bytes("a.ipynb", NB.as_bytes().to_vec());
        assert!(NotebookReader.detect(src.header(), "a.ipynb").confidence >= Confidence::Content);
        let doc = NotebookReader.open(&src, &ParseContext::new()).unwrap();
        let Content::Flow(f) = doc.facet(0, &ParseContext::new()).unwrap() else { panic!() };
        let s = format!("{:?}", f.blocks(0..100).unwrap());
        for want in ["Heading { level: 1", "Emphasis", "In [1]:", "language: Some(\"python\")", "import pandas", "hello", "Table", "[Image output: image/png]", "ValueError: bad", "raw text"] {
            assert!(s.contains(want), "{want} in {s}");
        }
        assert!(!s.contains("\u{1b}") && !s.contains("<Figure>"), "{s}");
        assert!(doc.metadata().iter().any(|(k, v)| k == "Kernel" && v == "Python 3"));
        assert_eq!(doc.warnings().len(), 1);
    }

    #[test]
    fn version_3_worksheets() {
        let nb = r#"{"nbformat": 3, "metadata": {"name": "old"}, "worksheets": [{"cells": [
            {"cell_type": "heading", "level": 2, "source": ["Old"]},
            {"cell_type": "code", "language": "python", "input": ["x = 1"], "prompt_number": 3, "outputs": [{"output_type": "pyout", "text": ["1"]}]}]}]}"#;
        let doc = NotebookReader.open(&FileSource::from_bytes("o.ipynb", nb.as_bytes().to_vec()), &ParseContext::new()).unwrap();
        let Content::Flow(f) = doc.facet(0, &ParseContext::new()).unwrap() else { panic!() };
        let s = format!("{:?}", f.blocks(0..100).unwrap());
        for want in ["Heading { level: 2", "In [3]:", "x = 1", "text: \"1\""] {
            assert!(s.contains(want), "{want} in {s}");
        }
    }

    #[test]
    fn other_json_isnt_a_notebook() {
        let src = FileSource::from_bytes("a.ipynb", br#"{"a": 1}"#.to_vec());
        assert!(matches!(NotebookReader.open(&src, &ParseContext::new()), Err(ParseError::NotThisFormat)));
        assert_eq!(NotebookReader.detect(br#"{"nbformat": 4, "cells": []}"#, "x.json").confidence, Confidence::Content);
        assert_eq!(NotebookReader.detect(br#"{"a": 1}"#, "x.json"), Detection::NONE);
    }
}
