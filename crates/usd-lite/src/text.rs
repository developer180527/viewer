//! USDA, the text format, into the same [`Layer`] model the crate reader builds: a
//! spec per prim and property, with the fields Sdf would give them (`specifier`,
//! `typeName`, `primChildren`, `properties`, `default`, `timeSamples`,
//! `connectionPaths`, `targetPaths`, and authored metadata).

use std::sync::Arc;

use crate::layer::{Layer, Spec, SpecType, child_path, property_path};
use crate::{Error, Limits, Result, Specifier, Value};

pub fn read(text: &[u8], limits: &Limits) -> Result<Layer> {
    let mut p = Parser { lex: Lexer { b: text, pos: 0, line: 1 }, layer: Layer::default(), limits: *limits, depth: 0, values: 0 };
    p.parse_layer()?;
    Ok(p.layer)
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Str(String),
    Num(f64),
    Asset(String),
    Path(String),
    Punct(u8),
    Eof,
}

struct Lexer<'a> {
    b: &'a [u8],
    pos: usize,
    line: usize,
}

impl Lexer<'_> {
    fn skip_space(&mut self) {
        while let Some(&c) = self.b.get(self.pos) {
            match c {
                b'\n' => {
                    self.line += 1;
                    self.pos += 1;
                }
                b' ' | b'\t' | b'\r' => self.pos += 1,
                b'#' => {
                    while self.b.get(self.pos).is_some_and(|&c| c != b'\n') {
                        self.pos += 1;
                    }
                }
                _ => break,
            }
        }
    }

    fn err(&self, what: impl std::fmt::Display) -> Error {
        Error::Format(format!("line {}: {what}", self.line))
    }

    fn next(&mut self) -> Result<Tok> {
        self.skip_space();
        let Some(&c) = self.b.get(self.pos) else { return Ok(Tok::Eof) };
        match c {
            b'"' | b'\'' => self.string(c).map(Tok::Str),
            b'@' => {
                let triple = self.b[self.pos..].starts_with(b"@@@");
                let (open, close): (usize, &[u8]) = if triple { (3, b"@@@") } else { (1, b"@") };
                let start = self.pos + open;
                let end = find(&self.b[start..], close).ok_or_else(|| self.err("unterminated asset path"))? + start;
                self.pos = end + close.len();
                Ok(Tok::Asset(String::from_utf8_lossy(&self.b[start..end]).into_owned()))
            }
            b'<' => {
                let start = self.pos + 1;
                let end = self.b[start..].iter().position(|&c| c == b'>').ok_or_else(|| self.err("unterminated path"))? + start;
                self.pos = end + 1;
                Ok(Tok::Path(String::from_utf8_lossy(&self.b[start..end]).into_owned()))
            }
            b'0'..=b'9' | b'-' | b'+' | b'.' => {
                let start = self.pos;
                self.pos += 1;
                while self.b.get(self.pos).is_some_and(|&c| c.is_ascii_digit() || matches!(c, b'.' | b'e' | b'E' | b'-' | b'+')) {
                    self.pos += 1;
                }
                let s = std::str::from_utf8(&self.b[start..self.pos]).unwrap_or("");
                // `-inf`, `-nan`
                if (s == "-" || s == "+") && self.b.get(self.pos).is_some_and(u8::is_ascii_alphabetic) {
                    let Tok::Ident(word) = self.next()? else { return Err(self.err("bad number")) };
                    let v: f64 = word.parse().map_err(|_| self.err(format!("bad number {s}{word}")))?;
                    return Ok(Tok::Num(if s == "-" { -v } else { v }));
                }
                s.parse().map(Tok::Num).map_err(|_| self.err(format!("bad number {s:?}")))
            }
            c if c.is_ascii_alphabetic() || c == b'_' => {
                let start = self.pos;
                while self.b.get(self.pos).is_some_and(|&c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b':' | b'.')) {
                    self.pos += 1;
                }
                Ok(Tok::Ident(String::from_utf8_lossy(&self.b[start..self.pos]).into_owned()))
            }
            _ => {
                self.pos += 1;
                Ok(Tok::Punct(c))
            }
        }
    }

    fn peek(&mut self) -> Result<Tok> {
        let (pos, line) = (self.pos, self.line);
        let t = self.next();
        (self.pos, self.line) = (pos, line);
        t
    }

    fn string(&mut self, quote: u8) -> Result<String> {
        let triple = self.b[self.pos..].starts_with(&[quote; 3]);
        let open = if triple { 3 } else { 1 };
        self.pos += open;
        let mut out = Vec::new();
        loop {
            let Some(&c) = self.b.get(self.pos) else { return Err(self.err("unterminated string")) };
            if triple && self.b[self.pos..].starts_with(&[quote; 3]) {
                self.pos += 3;
                break;
            }
            if !triple && c == quote {
                self.pos += 1;
                break;
            }
            if c == b'\n' {
                self.line += 1;
            }
            if c == b'\\' {
                let e = *self.b.get(self.pos + 1).ok_or_else(|| self.err("unterminated string"))?;
                out.push(match e {
                    b'n' => b'\n',
                    b't' => b'\t',
                    b'r' => b'\r',
                    other => other,
                });
                self.pos += 2;
                continue;
            }
            out.push(c);
            self.pos += 1;
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Metadata fields Sdf stores as tokens though USDA writes them as strings.
const TOKEN_METADATA: &[&str] = &["interpolation", "kind", "upAxis", "defaultPrim", "colorSpace", "purpose"];

/// A value before it is typed by its declaration.
#[derive(Debug, Clone)]
enum Raw {
    Num(f64),
    Str(String),
    Ident(String),
    Asset(String),
    /// `@asset@</prim>`
    Ref(String, String),
    Path(String),
    Tuple(Vec<Raw>),
    List(Vec<Raw>),
    /// A list of numbers or same-sized numeric tuples, kept flat (big arrays).
    Numbers { dims: usize, data: Vec<f64> },
    Dict(Vec<(String, Raw)>),
    /// `{ time: value, … }`
    Samples(Vec<(f64, Raw)>),
}

struct Parser<'a> {
    lex: Lexer<'a>,
    layer: Layer,
    limits: Limits,
    depth: u32,
    /// Numbers read, against `limits.max_elements`.
    values: usize,
}

impl Parser<'_> {
    fn expect(&mut self, c: u8) -> Result<()> {
        match self.lex.next()? {
            Tok::Punct(p) if p == c => Ok(()),
            t => Err(self.lex.err(format!("expected '{}', found {t:?}", c as char))),
        }
    }

    fn ident(&mut self) -> Result<String> {
        match self.lex.next()? {
            Tok::Ident(s) => Ok(s),
            t => Err(self.lex.err(format!("expected a name, found {t:?}"))),
        }
    }

    fn deeper(&mut self) -> Result<()> {
        self.depth += 1;
        if self.depth > 256 {
            return Err(Error::Limit("nesting deeper than 256".into()));
        }
        Ok(())
    }

    fn spec_mut(&mut self, path: &str, spec_type: SpecType) -> &mut Spec {
        self.layer.specs.entry(path.to_string()).or_insert_with(|| Spec { spec_type, fields: Vec::new() })
    }

    fn set(&mut self, path: &str, spec_type: SpecType, name: &str, value: Value) {
        let spec = self.spec_mut(path, spec_type);
        match spec.fields.iter_mut().find(|(n, _)| n == name) {
            Some(f) => f.1 = value,
            None => spec.fields.push((name.to_string(), value)),
        }
    }

    fn push_name(&mut self, path: &str, field: &str, name: &str) {
        let spec = self.spec_mut(path, SpecType::Prim);
        match spec.fields.iter_mut().find(|(n, _)| n == field) {
            Some((_, Value::TokenArray(v))) => v.push(name.to_string()),
            _ => spec.fields.push((field.to_string(), Value::TokenArray(vec![name.to_string()]))),
        }
    }

    fn parse_layer(&mut self) -> Result<()> {
        if !self.lex.b.starts_with(b"#usda") {
            return Err(Error::Format("not a USDA file".into()));
        }
        self.spec_mut("/", SpecType::PseudoRoot);
        if self.lex.peek()? == Tok::Punct(b'(') {
            self.metadata("/", SpecType::PseudoRoot)?;
        }
        loop {
            match self.lex.next()? {
                Tok::Eof => return Ok(()),
                Tok::Ident(s) if matches!(s.as_str(), "def" | "over" | "class") => self.prim("/", &s)?,
                t => return Err(self.lex.err(format!("expected a prim, found {t:?}"))),
            }
        }
    }

    /// `( key = value … )` onto the spec at `path`. Also takes a bare string (`doc`)
    /// and list-op keywords (`prepend apiSchemas = [...]`).
    fn metadata(&mut self, path: &str, spec_type: SpecType) -> Result<()> {
        self.expect(b'(')?;
        loop {
            match self.lex.next()? {
                Tok::Punct(b')') => return Ok(()),
                Tok::Punct(b';') => {}
                Tok::Str(doc) => self.set(path, spec_type, "documentation", Value::String(doc)),
                Tok::Ident(mut key) => {
                    if matches!(key.as_str(), "prepend" | "append" | "add" | "delete" | "reorder") {
                        key = self.ident()?;
                    }
                    self.expect(b'=')?;
                    let raw = self.value()?;
                    // Sdf's field names, as binary files store them.
                    let key = match key.as_str() {
                        "doc" => "documentation".to_string(),
                        "inherits" => "inheritPaths".to_string(),
                        "variants" => "variantSelection".to_string(),
                        "variantSets" => "variantSetNames".to_string(),
                        _ => key,
                    };
                    let value = match key.as_str() {
                        "references" | "payload" => Value::References(references(raw)),
                        "inheritPaths" | "specializes" => Value::Paths(paths(raw)),
                        "variantSetNames" => Value::StringArray(match raw {
                            Raw::Str(s) => vec![s],
                            Raw::List(items) => items.into_iter().filter_map(|r| if let Raw::Str(s) = r { Some(s) } else { None }).collect(),
                            _ => Vec::new(),
                        }),
                        _ => match untyped(raw) {
                            // Metadata Sdf types as tokens, written as strings in text.
                            Value::String(s) if TOKEN_METADATA.contains(&key.as_str()) => Value::Token(s),
                            v => v,
                        },
                    };
                    self.set(path, spec_type, &key, value);
                }
                t => return Err(self.lex.err(format!("unexpected {t:?} in metadata"))),
            }
        }
    }

    fn prim(&mut self, parent: &str, specifier: &str) -> Result<()> {
        // The keyword was just read: this is its line.
        let line = self.lex.line as u32;
        self.deeper()?;
        let type_name = match self.lex.peek()? {
            Tok::Ident(_) => Some(self.ident()?),
            _ => None,
        };
        let name = match self.lex.next()? {
            Tok::Str(s) => s,
            t => return Err(self.lex.err(format!("expected a prim name, found {t:?}"))),
        };
        let path = child_path(parent, &name);
        self.layer.lines.entry(path.clone()).or_insert(line);
        self.push_name(parent, "primChildren", &name);
        let spec = match specifier {
            "over" => Specifier::Over,
            "class" => Specifier::Class,
            _ => Specifier::Def,
        };
        self.set(&path, SpecType::Prim, "specifier", Value::Specifier(spec));
        if let Some(t) = type_name {
            self.set(&path, SpecType::Prim, "typeName", Value::Token(t));
        }
        if self.lex.peek()? == Tok::Punct(b'(') {
            self.metadata(&path, SpecType::Prim)?;
        }
        self.expect(b'{')?;
        self.body(&path)?;
        self.depth -= 1;
        Ok(())
    }

    /// Prim contents up to and including the closing `}`.
    fn body(&mut self, path: &str) -> Result<()> {
        loop {
            match self.lex.peek()? {
                Tok::Punct(b'}') => {
                    self.lex.next()?;
                    return Ok(());
                }
                Tok::Punct(b';') => {
                    self.lex.next()?;
                }
                Tok::Eof => return Err(self.lex.err("unexpected end of file in a prim")),
                Tok::Ident(word) => match word.as_str() {
                    "def" | "over" | "class" => {
                        self.lex.next()?;
                        self.prim(path, &word)?;
                    }
                    "variantSet" => {
                        self.lex.next()?;
                        self.variant_set(path)?;
                    }
                    "reorder" => {
                        // `reorder nameChildren = [...]`: ordering only.
                        self.lex.next()?;
                        self.ident()?;
                        self.expect(b'=')?;
                        self.value()?;
                    }
                    _ => self.property(path)?,
                },
                t => return Err(self.lex.err(format!("unexpected {t:?} in a prim"))),
            }
        }
    }

    /// Variants are kept out of the prim tree (under `/Prim{set=name}`): nothing here
    /// composes them.
    fn variant_set(&mut self, path: &str) -> Result<()> {
        let set = match self.lex.next()? {
            Tok::Str(s) => s,
            t => return Err(self.lex.err(format!("expected a variant set name, found {t:?}"))),
        };
        self.expect(b'=')?;
        self.expect(b'{')?;
        // The set's own spec, as Sdf has it: `/Prim{set=}`, listing its variants.
        let set_path = format!("{path}{{{set}=}}");
        self.spec_mut(&set_path, SpecType::VariantSet);
        loop {
            match self.lex.next()? {
                Tok::Punct(b'}') => return Ok(()),
                Tok::Str(variant) => {
                    let spec = self.spec_mut(&set_path, SpecType::VariantSet);
                    match spec.fields.iter_mut().find(|(n, _)| n == "variantChildren") {
                        Some((_, Value::TokenArray(v))) => v.push(variant.clone()),
                        _ => spec.fields.push(("variantChildren".into(), Value::TokenArray(vec![variant.clone()]))),
                    }
                    let vpath = format!("{path}{{{set}={variant}}}");
                    self.set(&vpath, SpecType::Variant, "specifier", Value::Specifier(Specifier::Over));
                    if self.lex.peek()? == Tok::Punct(b'(') {
                        self.metadata(&vpath, SpecType::Variant)?;
                    }
                    self.expect(b'{')?;
                    self.deeper()?;
                    self.body(&vpath)?;
                    self.depth -= 1;
                }
                t => return Err(self.lex.err(format!("unexpected {t:?} in a variant set"))),
            }
        }
    }

    fn property(&mut self, prim: &str) -> Result<()> {
        let mut word = self.ident()?;
        let (mut custom, mut uniform) = (false, false);
        loop {
            match word.as_str() {
                "custom" => custom = true,
                "uniform" => uniform = true,
                "varying" | "config" | "prepend" | "append" | "add" | "delete" => {}
                _ => break,
            }
            word = self.ident()?;
        }
        if word == "rel" {
            let name = self.ident()?;
            let path = property_path(prim, &name);
            self.push_name(prim, "properties", &name);
            self.spec_mut(&path, SpecType::Relationship);
            if self.lex.peek()? == Tok::Punct(b'=') {
                self.lex.next()?;
                let targets = paths(self.value()?);
                self.set(&path, SpecType::Relationship, "targetPaths", Value::Paths(targets));
            }
            if self.lex.peek()? == Tok::Punct(b'(') {
                self.metadata(&path, SpecType::Relationship)?;
            }
            return Ok(());
        }

        let mut type_name = word;
        if self.lex.peek()? == Tok::Punct(b'[') {
            self.lex.next()?;
            self.expect(b']')?;
            type_name.push_str("[]");
        }
        let full = self.ident()?;
        let (name, suffix) = match full.rsplit_once('.') {
            Some((n, s @ ("connect" | "timeSamples" | "spline"))) => (n.to_string(), Some(s.to_string())),
            _ => (full, None),
        };
        let path = property_path(prim, &name);
        if !self.layer.specs.contains_key(&path) {
            self.push_name(prim, "properties", &name);
        }
        self.set(&path, SpecType::Attribute, "typeName", Value::Token(type_name.clone()));
        if custom {
            self.set(&path, SpecType::Attribute, "custom", Value::Bool(true));
        }
        if uniform {
            self.set(&path, SpecType::Attribute, "variability", Value::Token("uniform".into()));
        }
        if self.lex.peek()? == Tok::Punct(b'=') {
            self.lex.next()?;
            let raw = self.value()?;
            match suffix.as_deref() {
                Some("connect") => self.set(&path, SpecType::Attribute, "connectionPaths", Value::Paths(paths(raw))),
                Some("timeSamples") => {
                    let samples = match raw {
                        Raw::Samples(s) => s.into_iter().map(|(t, v)| (t, typed(&type_name, v))).collect(),
                        _ => Vec::new(),
                    };
                    self.set(&path, SpecType::Attribute, "timeSamples", Value::TimeSamples(samples));
                }
                Some(_) => {}
                None => self.set(&path, SpecType::Attribute, "default", typed(&type_name, raw)),
            }
        }
        if self.lex.peek()? == Tok::Punct(b'(') {
            self.metadata(&path, SpecType::Attribute)?;
        }
        Ok(())
    }

    fn count(&mut self, n: usize) -> Result<()> {
        self.values += n;
        if self.values > self.limits.max_elements {
            return Err(Error::Limit(format!("more than {} values", self.limits.max_elements)));
        }
        Ok(())
    }

    fn value(&mut self) -> Result<Raw> {
        self.deeper()?;
        let v = match self.lex.next()? {
            Tok::Num(n) => {
                self.count(1)?;
                Raw::Num(n)
            }
            Tok::Str(s) => Raw::Str(s),
            Tok::Ident(s) => match s.as_str() {
                "inf" => Raw::Num(f64::INFINITY),
                "nan" => Raw::Num(f64::NAN),
                _ => Raw::Ident(s),
            },
            Tok::Asset(a) => match self.lex.peek()? {
                // A reference or payload: `@file.usd@</Prim>`.
                Tok::Path(p) => {
                    self.lex.next()?;
                    Raw::Ref(a, p)
                }
                _ => Raw::Asset(a),
            },
            Tok::Path(p) => Raw::Path(p),
            Tok::Punct(b'(') => Raw::Tuple(self.items(b')')?),
            Tok::Punct(b'[') => self.list()?,
            Tok::Punct(b'{') => self.dict()?,
            t => return Err(self.lex.err(format!("expected a value, found {t:?}"))),
        };
        self.depth -= 1;
        Ok(v)
    }

    fn items(&mut self, close: u8) -> Result<Vec<Raw>> {
        let mut out = Vec::new();
        loop {
            if self.lex.peek()? == Tok::Punct(close) {
                self.lex.next()?;
                return Ok(out);
            }
            out.push(self.value()?);
            match self.lex.next()? {
                Tok::Punct(b',') => {}
                Tok::Punct(c) if c == close => return Ok(out),
                t => return Err(self.lex.err(format!("expected ',' or '{}', found {t:?}", close as char))),
            }
        }
    }

    /// `[ … ]`. Numbers and numeric tuples go straight into a flat buffer.
    fn list(&mut self) -> Result<Raw> {
        let numeric = matches!(self.lex.peek()?, Tok::Num(_) | Tok::Punct(b'('));
        if !numeric {
            return Ok(Raw::List(self.items(b']')?));
        }
        let start = (self.lex.pos, self.lex.line);
        match self.flat_numbers() {
            Ok(Some(raw)) => Ok(raw),
            // Not uniform numbers after all (tuples of strings…): reparse generally.
            Ok(None) | Err(_) => {
                (self.lex.pos, self.lex.line) = start;
                Ok(Raw::List(self.items(b']')?))
            }
        }
    }

    fn flat_numbers(&mut self) -> Result<Option<Raw>> {
        let mut data = Vec::new();
        let mut dims = None;
        loop {
            match self.lex.next()? {
                Tok::Punct(b']') => break,
                Tok::Num(n) => {
                    if *dims.get_or_insert(1) != 1 {
                        return Ok(None);
                    }
                    data.push(n);
                }
                Tok::Punct(b'(') => {
                    let before = data.len();
                    loop {
                        match self.lex.next()? {
                            Tok::Num(n) => data.push(n),
                            Tok::Ident(s) if s == "inf" || s == "nan" => data.push(if s == "inf" { f64::INFINITY } else { f64::NAN }),
                            _ => return Ok(None),
                        }
                        match self.lex.next()? {
                            Tok::Punct(b',') => {}
                            Tok::Punct(b')') => break,
                            _ => return Ok(None),
                        }
                    }
                    if *dims.get_or_insert(data.len() - before) != data.len() - before {
                        return Ok(None);
                    }
                }
                _ => return Ok(None),
            }
            self.count(1)?;
            match self.lex.next()? {
                Tok::Punct(b',') => {}
                Tok::Punct(b']') => break,
                _ => return Ok(None),
            }
        }
        Ok(Some(Raw::Numbers { dims: dims.unwrap_or(1), data }))
    }

    /// `{ type key = value … }` (dictionaries) or `{ time: value, … }` (time samples).
    fn dict(&mut self) -> Result<Raw> {
        if let Tok::Num(_) = self.lex.peek()? {
            let mut samples = Vec::new();
            loop {
                let t = match self.lex.next()? {
                    Tok::Num(t) => t,
                    Tok::Punct(b'}') => return Ok(Raw::Samples(samples)),
                    t => return Err(self.lex.err(format!("expected a time, found {t:?}"))),
                };
                self.expect(b':')?;
                samples.push((t, self.value()?));
                match self.lex.next()? {
                    Tok::Punct(b',') => {}
                    Tok::Punct(b'}') => return Ok(Raw::Samples(samples)),
                    t => return Err(self.lex.err(format!("unexpected {t:?} in time samples"))),
                }
            }
        }
        let mut entries = Vec::new();
        loop {
            match self.lex.next()? {
                Tok::Punct(b'}') => return Ok(Raw::Dict(entries)),
                Tok::Punct(b';' | b',') => {}
                // `type key = value`, `"key": value` is not USDA, but be lenient with
                // `key = value` (variant selections: `string shading = "red"`).
                Tok::Ident(first) => {
                    let key = match self.lex.peek()? {
                        Tok::Punct(b'=') => first,
                        Tok::Punct(b'[') => {
                            self.lex.next()?;
                            self.expect(b']')?;
                            self.key()?
                        }
                        _ => self.key()?,
                    };
                    self.expect(b'=')?;
                    entries.push((key, self.value()?));
                }
                t => return Err(self.lex.err(format!("unexpected {t:?} in a dictionary"))),
            }
        }
    }

    fn key(&mut self) -> Result<String> {
        match self.lex.next()? {
            Tok::Ident(s) | Tok::Str(s) => Ok(s),
            t => Err(self.lex.err(format!("expected a key, found {t:?}"))),
        }
    }
}

/// `@a.usd@</P>`, `@a.usd@`, `</Internal>`, or a list of them; `None` is empty.
fn references(raw: Raw) -> Vec<crate::Reference> {
    let one = |r: Raw| match r {
        Raw::Ref(asset, prim) => Some(crate::Reference { asset, prim: Some(prim) }),
        Raw::Asset(asset) => Some(crate::Reference { asset, prim: None }),
        Raw::Path(prim) => Some(crate::Reference { asset: String::new(), prim: Some(prim) }),
        _ => None,
    };
    match raw {
        Raw::List(items) => items.into_iter().filter_map(one).collect(),
        r => one(r).into_iter().collect(),
    }
}

fn paths(raw: Raw) -> Vec<String> {
    match raw {
        Raw::Path(p) => vec![p],
        Raw::List(items) => items.into_iter().filter_map(|r| if let Raw::Path(p) = r { Some(p) } else { None }).collect(),
        _ => Vec::new(),
    }
}

/// Metadata and dictionary values: typed by what they look like.
fn untyped(raw: Raw) -> Value {
    match raw {
        Raw::Num(n) => Value::Float(n),
        Raw::Str(s) => Value::String(s),
        Raw::Ident(s) => match s.as_str() {
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            "None" => Value::Blocked,
            _ => Value::Token(s),
        },
        Raw::Asset(a) => Value::Asset(a),
        Raw::Ref(asset, prim) => Value::References(vec![crate::Reference { asset, prim: Some(prim) }]),
        Raw::Path(p) => Value::Paths(vec![p]),
        Raw::Tuple(items) => {
            let nums: Vec<f64> = items.iter().filter_map(|r| if let Raw::Num(n) = r { Some(*n) } else { None }).collect();
            if nums.len() == items.len() && (2..=4).contains(&nums.len()) { vec_value(&nums) } else { Value::Other("tuple") }
        }
        Raw::List(items) => {
            if items.iter().all(|r| matches!(r, Raw::Path(_))) && !items.is_empty() {
                Value::Paths(paths(Raw::List(items)))
            } else if items.iter().all(|r| matches!(r, Raw::Asset(_))) && !items.is_empty() {
                // `subLayers = [@a.usda@, @b.usda@]`
                Value::AssetArray(items.into_iter().filter_map(|r| if let Raw::Asset(a) = r { Some(a) } else { None }).collect())
            } else {
                Value::StringArray(items.into_iter().filter_map(|r| if let Raw::Str(s) | Raw::Ident(s) = r { Some(s) } else { None }).collect())
            }
        }
        Raw::Numbers { dims: 1, data } => Value::DoubleArray(data.into()),
        Raw::Numbers { .. } => Value::Other("numeric array"),
        Raw::Dict(entries) => Value::Dictionary(entries.into_iter().map(|(k, v)| (k, untyped(v))).collect()),
        Raw::Samples(s) => Value::TimeSamples(s.into_iter().map(|(t, v)| (t, untyped(v))).collect()),
    }
}

fn vec_value(v: &[f64]) -> Value {
    match v.len() {
        2 => Value::Vec2([v[0], v[1]]),
        3 => Value::Vec3([v[0], v[1], v[2]]),
        _ => Value::Vec4([v[0], v[1], v[2], v[3]]),
    }
}

/// Components of a value type name: 1 for scalars, 2–4 for vectors, 0 for others.
fn dims(base: &str) -> usize {
    let last = base.trim_end_matches(['f', 'd', 'h', 'i']);
    match last.chars().last() {
        Some('2') => 2,
        Some('3') => 3,
        Some('4') => 4,
        _ => 1,
    }
}

/// An attribute value, typed by its declaration (`point3f[]`, `token`, `matrix4d`…).
fn typed(type_name: &str, raw: Raw) -> Value {
    if let Raw::Ident(s) = &raw
        && s == "None"
    {
        return Value::Blocked;
    }
    let (base, array) = match type_name.strip_suffix("[]") {
        Some(b) => (b, true),
        None => (type_name, false),
    };
    let is_float = matches!(base.chars().last(), Some('f' | 'd' | 'h')) || matches!(base, "float" | "double" | "half" | "timecode");
    match (base, array) {
        // Written as `1`/`0` or `true`/`false`.
        ("bool", false) => match raw {
            Raw::Num(n) => Value::Bool(n != 0.0),
            other => untyped(other).as_bool().map_or(Value::Other("bool"), Value::Bool),
        },
        ("bool", true) => match raw {
            Raw::Numbers { data, .. } => Value::BoolArray(data.iter().map(|&v| v != 0.0).collect()),
            Raw::List(items) => Value::BoolArray(items.iter().map(|r| matches!(r, Raw::Ident(s) if s == "true") || matches!(r, Raw::Num(n) if *n != 0.0)).collect()),
            _ => Value::Other("bool[]"),
        },
        ("string", false) => match raw {
            Raw::Str(s) => Value::String(s),
            _ => Value::Other("string"),
        },
        ("token", false) => match raw {
            Raw::Str(s) | Raw::Ident(s) => Value::Token(s),
            _ => Value::Other("token"),
        },
        ("asset", false) => match raw {
            Raw::Asset(a) => Value::Asset(a),
            _ => Value::Other("asset"),
        },
        ("string" | "token" | "asset", true) => {
            let items: Vec<String> = match raw {
                Raw::List(items) => items.into_iter().filter_map(|r| if let Raw::Str(s) | Raw::Asset(s) | Raw::Ident(s) = r { Some(s) } else { None }).collect(),
                _ => Vec::new(),
            };
            match base {
                "string" => Value::StringArray(items),
                "token" => Value::TokenArray(items),
                _ => Value::AssetArray(items),
            }
        }
        ("int" | "uint" | "uchar" | "int64" | "uint64", false) => match raw {
            Raw::Num(n) => Value::Int(n as i64),
            _ => Value::Other("int"),
        },
        ("int" | "uint" | "uchar", true) => match raw {
            Raw::Numbers { dims: 1, data } => Value::IntArray(data.iter().map(|&v| v as i32).collect()),
            Raw::List(l) if l.is_empty() => Value::IntArray(Arc::from([])),
            _ => Value::Other("int[]"),
        },
        ("int64" | "uint64", true) => match raw {
            Raw::Numbers { dims: 1, data } => Value::Int64Array(data.iter().map(|&v| v as i64).collect()),
            _ => Value::Int64Array(Arc::from([])),
        },
        ("matrix4d" | "matrix3d" | "matrix2d", false) => match raw {
            Raw::Tuple(rows) => {
                let n = rows.len();
                let mut m = [0.0; 16];
                for i in 0..4 {
                    m[i * 4 + i] = 1.0;
                }
                for (r, row) in rows.iter().enumerate().take(4) {
                    if let Raw::Tuple(cols) = row {
                        for (c, v) in cols.iter().enumerate().take(4) {
                            if let Raw::Num(x) = v {
                                m[r * 4 + c] = *x;
                            }
                        }
                    }
                }
                if n == 0 { Value::Other("matrix") } else { Value::Matrix4(m) }
            }
            _ => Value::Other("matrix"),
        },
        ("matrix4d", true) => match raw {
            Raw::List(items) => Value::Matrix4Array(items.into_iter().filter_map(|r| typed("matrix4d", r).as_matrix4()).collect()),
            _ => Value::Matrix4Array(Arc::from([])),
        },
        (q, false) if q.starts_with("quat") => match untyped(raw) {
            // Text order is (real, i, j, k).
            Value::Vec4([w, x, y, z]) => Value::Quat([x, y, z, w]),
            _ => Value::Other("quat"),
        },
        (q, true) if q.starts_with("quat") => match raw {
            Raw::Numbers { dims: 4, data } => Value::QuatArray(data.as_chunks::<4>().0.iter().map(|v| [v[1] as f32, v[2] as f32, v[3] as f32, v[0] as f32]).collect()),
            _ => Value::QuatArray(Arc::from([])),
        },
        (b, false) if dims(b) > 1 => match raw {
            Raw::Tuple(items) => {
                let nums: Vec<f64> = items.iter().filter_map(|r| if let Raw::Num(n) = r { Some(*n) } else { None }).collect();
                if nums.len() == dims(b) { vec_value(&nums) } else { Value::Other("vector") }
            }
            _ => Value::Other("vector"),
        },
        (b, true) if dims(b) > 1 => {
            let d = dims(b);
            let data = match raw {
                Raw::Numbers { dims, data } if dims == d => data,
                _ => Vec::new(),
            };
            match d {
                2 => Value::Vec2Array(data.as_chunks::<2>().0.iter().map(|v| [v[0] as f32, v[1] as f32]).collect()),
                3 => Value::Vec3Array(data.as_chunks::<3>().0.iter().map(|v| [v[0] as f32, v[1] as f32, v[2] as f32]).collect()),
                _ => Value::Vec4Array(data.as_chunks::<4>().0.iter().map(|v| [v[0] as f32, v[1] as f32, v[2] as f32, v[3] as f32]).collect()),
            }
        }
        (_, false) if is_float => match raw {
            Raw::Num(n) => Value::Float(n),
            _ => Value::Other("float"),
        },
        ("double", true) => match raw {
            Raw::Numbers { dims: 1, data } => Value::DoubleArray(data.into()),
            _ => Value::DoubleArray(Arc::from([])),
        },
        (_, true) if is_float => match raw {
            Raw::Numbers { dims: 1, data } => Value::FloatArray(data.into_iter().map(|v| v as f32).collect()),
            _ => Value::FloatArray(Arc::from([])),
        },
        _ => untyped(raw),
    }
}
