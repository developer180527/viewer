//! FBX's own facets (docs/design/libfileparser-3d.md §1): the **Records** (the file's
//! node tree, each record with its properties) and the **Embedded media** (textures
//! stored in the file, each as a file of its own).
//!
//! Binary records are read in place: a node's id is its record's offset in the file,
//! and a record's children are found when they are asked for, so a file with 100,000
//! records costs nothing until someone looks. ASCII FBX is parsed into a tree when the
//! facet is first opened.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::{Arc, Mutex};

use libfileparser_core::{Container, Count, Entry, FileSource, MemoryUse, NodeId, OwnedTree, ParseError, Result, TreeNode, TreeSource};

const MAGIC: &[u8] = b"Kaydara FBX Binary  \0";
/// Records start after the magic, two bytes and the version.
const FIRST: usize = 27;
/// The parent key for the top level.
const TOP: u64 = u64::MAX;

pub fn is_binary(bytes: &[u8]) -> bool {
    bytes.starts_with(MAGIC)
}

/// A record's header.
struct Rec {
    end: usize,
    props: usize,
    name: Range<usize>,
    props_at: usize,
    nested_at: usize,
}

/// A binary FBX's records, read from the file as they're asked for.
pub struct BinaryRecords {
    src: FileSource,
    wide: bool,
    seen: Mutex<Seen>,
}

/// Children by parent offset, and every offset handed out.
type Seen = (HashMap<u64, Arc<Vec<u64>>>, HashSet<u64>);

impl BinaryRecords {
    pub fn new(src: &FileSource) -> Result<Self> {
        let b = src.bytes();
        if !is_binary(b) {
            return Err(ParseError::corrupt("not a binary FBX"));
        }
        let version = b.get(23..27).map(|v| u32::from_le_bytes(v.try_into().unwrap())).ok_or_else(|| ParseError::corrupt("FBX header truncated"))?;
        Ok(Self { src: src.clone(), wide: version >= 7500, seen: Mutex::new((HashMap::new(), HashSet::new())) })
    }

    fn bytes(&self) -> &[u8] {
        self.src.bytes()
    }

    fn num(&self, at: usize) -> Option<usize> {
        let b = self.bytes();
        if self.wide {
            usize::try_from(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?)).ok()
        } else {
            Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?) as usize)
        }
    }

    /// The record at `at`; `None` for the null record that ends a list, or a damaged one.
    fn rec(&self, at: usize) -> Option<Rec> {
        let w = if self.wide { 8 } else { 4 };
        let end = self.num(at)?;
        if end == 0 {
            return None;
        }
        let props = self.num(at + w)?;
        let props_len = self.num(at + 2 * w)?;
        let name_len = *self.bytes().get(at + 3 * w)? as usize;
        let name_at = at + 3 * w + 1;
        let props_at = name_at + name_len;
        let nested_at = props_at.checked_add(props_len)?;
        (end > at && end <= self.bytes().len() && nested_at <= end).then_some(Rec { end, props, name: name_at..props_at, props_at, nested_at })
    }

    /// The offsets of the records in `start..end`, up to the null record.
    fn list(&self, start: usize, end: usize) -> Vec<u64> {
        let mut out = Vec::new();
        let mut at = start;
        while at < end {
            let Some(r) = self.rec(at) else { break };
            if r.end > end {
                break;
            }
            out.push(at as u64);
            at = r.end;
        }
        out
    }

    fn children_of(&self, parent: Option<NodeId>) -> Result<Arc<Vec<u64>>> {
        let key = parent.map_or(TOP, |p| p.0);
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(list) = seen.0.get(&key) {
            return Ok(list.clone());
        }
        let list = match parent {
            None => self.list(FIRST, self.bytes().len()),
            Some(p) => {
                if !seen.1.contains(&p.0) {
                    return Err(ParseError::invalid(format!("no node {}", p.0)));
                }
                match self.rec(p.0 as usize) {
                    Some(r) => self.list(r.nested_at, r.end),
                    None => Vec::new(),
                }
            }
        };
        let list = Arc::new(list);
        seen.1.extend(list.iter().copied());
        seen.0.insert(key, list.clone());
        Ok(list)
    }

    fn node(&self, at: u64) -> TreeNode {
        let Some(r) = self.rec(at as usize) else { return TreeNode::new(NodeId(at), "?", "", false) };
        let name = String::from_utf8_lossy(&self.bytes()[r.name.clone()]).into_owned();
        let has_children = r.nested_at < r.end && self.rec(r.nested_at).is_some_and(|c| c.end <= r.end);
        TreeNode::new(NodeId(at), name, self.properties(&r).0.join(", "), has_children)
    }

    /// The first properties of a record as text, and any `R` (raw) property's bytes.
    fn properties(&self, r: &Rec) -> (Vec<String>, Option<Range<usize>>) {
        let b = self.bytes();
        let mut at = r.props_at;
        let mut out = Vec::new();
        let mut raw = None;
        let u32_at = |at: usize| b.get(at..at + 4).map(|v| u32::from_le_bytes(v.try_into().unwrap()) as usize);
        for i in 0..r.props {
            if at >= r.nested_at || i >= 64 {
                break;
            }
            if out.len() == 6 {
                out.push(format!("…{} more", r.props - 6));
                break;
            }
            let ty = b[at];
            at += 1;
            let get = |n: usize| b.get(at..at + n).filter(|_| at + n <= r.nested_at);
            let (text, len) = match ty {
                b'Y' => (get(2).map(|v| i16::from_le_bytes(v.try_into().unwrap()).to_string()), 2),
                b'C' => (get(1).map(|v| if v[0] != 0 { "true" } else { "false" }.to_string()), 1),
                b'I' => (get(4).map(|v| i32::from_le_bytes(v.try_into().unwrap()).to_string()), 4),
                b'F' => (get(4).map(|v| number(f32::from_le_bytes(v.try_into().unwrap()) as f64)), 4),
                b'D' => (get(8).map(|v| number(f64::from_le_bytes(v.try_into().unwrap()))), 8),
                b'L' => (get(8).map(|v| i64::from_le_bytes(v.try_into().unwrap()).to_string()), 8),
                b'f' | b'd' | b'l' | b'i' | b'b' => {
                    let (Some(count), Some(encoding), Some(packed)) = (u32_at(at), u32_at(at + 4), u32_at(at + 8)) else { break };
                    let kind = match ty {
                        b'f' => "f32",
                        b'd' => "f64",
                        b'l' => "i64",
                        b'i' => "i32",
                        _ => "bool",
                    };
                    let note = if encoding == 1 { ", compressed" } else { "" };
                    (Some(format!("{kind}[{count}]{note}")), 12 + packed)
                }
                b'S' | b'R' => {
                    let Some(len) = u32_at(at) else { break };
                    let data = at + 4..at + 4 + len;
                    if data.end > r.nested_at {
                        break;
                    }
                    let text = if ty == b'S' {
                        format!("\"{}\"", fbx_name(&b[data.clone()]))
                    } else {
                        raw.get_or_insert(data.clone());
                        format!("raw, {len} bytes")
                    };
                    (Some(text), 4 + len)
                }
                _ => break,
            };
            let Some(text) = text else { break };
            out.push(text);
            at = match at.checked_add(len) {
                Some(a) => a,
                None => break,
            };
        }
        (out, raw)
    }

    /// The textures stored in the file: each `Objects/Video` with `Content`, named
    /// after its `RelativeFilename` (else `Filename`, else the video's name).
    pub fn media(&self) -> Vec<(String, Range<usize>)> {
        let mut out = Vec::new();
        let mut used = HashSet::new();
        let name_of = |at: u64| self.rec(at as usize).map(|r| self.bytes()[r.name].to_vec()).unwrap_or_default();
        let top = self.list(FIRST, self.bytes().len());
        for objects in top.into_iter().filter(|&o| name_of(o) == b"Objects") {
            let Some(o) = self.rec(objects as usize) else { continue };
            for video in self.list(o.nested_at, o.end).into_iter().filter(|&v| name_of(v) == b"Video") {
                let Some(v) = self.rec(video as usize) else { continue };
                let mut content = None;
                let (mut relative, mut absolute) = (None, None);
                for child in self.list(v.nested_at, v.end) {
                    let Some(c) = self.rec(child as usize) else { continue };
                    let (props, raw) = self.properties(&c);
                    let text = || props.first().map(|p| p.trim_matches('"').to_string()).filter(|p| !p.is_empty());
                    match &self.bytes()[c.name.clone()] {
                        b"Content" => content = raw.filter(|r| !r.is_empty()),
                        b"RelativeFilename" => relative = text(),
                        b"Filename" => absolute = text(),
                        _ => {}
                    }
                }
                let Some(range) = content else { continue };
                let own = self.properties(&v).0.get(1).map(|n| n.trim_matches('"').rsplit("::").next().unwrap_or("").to_string());
                let name = relative.or(absolute).map(|p| p.rsplit(['/', '\\']).next().unwrap_or(&p).to_string()).filter(|n| !n.is_empty()).or(own).unwrap_or_else(|| format!("video {}", out.len()));
                let mut path = format!("media/{name}");
                if !used.insert(path.clone()) {
                    path = format!("media/{} ({name})", out.len());
                    used.insert(path.clone());
                }
                out.push((path, range));
            }
        }
        out
    }
}

impl TreeSource for BinaryRecords {
    fn child_count(&self, parent: Option<NodeId>) -> Result<Count> {
        Ok(Count::Exact(self.children_of(parent)?.len() as u64))
    }

    fn children(&self, parent: Option<NodeId>, range: Range<u64>) -> Result<Vec<TreeNode>> {
        let list = self.children_of(parent)?;
        let end = range.end.min(list.len() as u64) as usize;
        let start = (range.start as usize).min(end);
        Ok(list[start..end].iter().map(|&at| self.node(at)).collect())
    }

    fn memory(&self) -> MemoryUse {
        let seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        MemoryUse::heap(seen.0.values().map(|l| l.len() as u64 * 8 + 48).sum::<u64>() + seen.1.len() as u64 * 16)
    }
}

/// `Cube\0\x01Model` is written `Model::Cube` in ASCII FBX, and shown that way.
fn fbx_name(b: &[u8]) -> String {
    let s = String::from_utf8_lossy(b);
    match s.split_once("\0\u{1}") {
        Some((name, class)) => format!("{class}::{name}"),
        None => s.into_owned(),
    }
}

fn number(v: f64) -> String {
    let s = format!("{v:.6}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.into() }
}

// ---------------------------------------------------------------- ASCII

/// An ASCII FBX's records: `Name: values {` … `}`, values shown as the first few.
pub fn ascii_records(text: &[u8], max_nodes: u64, max_depth: u64) -> Result<OwnedTree> {
    let mut tree = OwnedTree::new();
    let mut parents: Vec<NodeId> = Vec::new();
    // The record being read: its key, its first values and how many there are.
    let mut pending: Option<(String, Vec<String>, usize)> = None;
    let mut nodes = 0u64;
    let mut flush = |pending: &mut Option<(String, Vec<String>, usize)>, parents: &[NodeId], tree: &mut OwnedTree| -> Result<Option<NodeId>> {
        let Some((key, values, n)) = pending.take() else { return Ok(None) };
        nodes += 1;
        if nodes > max_nodes {
            return Err(ParseError::too_large(libfileparser_core::Limit::Items, nodes, max_nodes));
        }
        let mut value = values.join(", ");
        if n > values.len() {
            value = format!("{value}, … ({n} values)");
        }
        Ok(Some(tree.add(parents.last().copied(), key, value)))
    };
    let mut i = 0usize;
    let b = text;
    while i < b.len() {
        let c = b[i];
        match c {
            b';' => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'{' => {
                let node = match flush(&mut pending, &parents, &mut tree)? {
                    Some(n) => n,
                    // A brace with no record before it: keep the nesting balanced.
                    None => tree.add(parents.last().copied(), "{", ""),
                };
                parents.push(node);
                if parents.len() as u64 > max_depth {
                    return Err(ParseError::too_large(libfileparser_core::Limit::Depth, parents.len() as u64, max_depth));
                }
                i += 1;
            }
            b'}' => {
                flush(&mut pending, &parents, &mut tree)?;
                parents.pop();
                i += 1;
            }
            b'"' => {
                let start = i + 1;
                i = start;
                while i < b.len() && b[i] != b'"' {
                    i += 1;
                }
                let s = String::from_utf8_lossy(&b[start..i.min(b.len())]).into_owned();
                i += 1;
                if let Some((_, values, n)) = &mut pending {
                    *n += 1;
                    if values.len() < 4 {
                        values.push(format!("\"{s}\""));
                    }
                }
            }
            b',' | b' ' | b'\t' | b'\r' | b'\n' => i += 1,
            _ => {
                let start = i;
                while i < b.len() && !matches!(b[i], b',' | b' ' | b'\t' | b'\r' | b'\n' | b'{' | b'}' | b'"' | b';' | b':') {
                    i += 1;
                }
                let word = String::from_utf8_lossy(&b[start..i]).into_owned();
                if b.get(i) == Some(&b':') {
                    // A new record's key.
                    i += 1;
                    flush(&mut pending, &parents, &mut tree)?;
                    pending = Some((word, Vec::new(), 0));
                } else if let Some((_, values, n)) = &mut pending {
                    *n += 1;
                    if values.len() < 4 {
                        // `*9` before an array's braces: its length.
                        values.push(match word.strip_prefix('*') {
                            Some(len) => format!("[{len}]"),
                            None => word,
                        });
                    }
                } else if word.is_empty() {
                    i += 1;
                }
            }
        }
    }
    flush(&mut pending, &parents, &mut tree)?;
    Ok(tree)
}

// ---------------------------------------------------------------- media

/// The Embedded media facet.
pub struct Media {
    src: FileSource,
    entries: Vec<(String, Range<usize>)>,
}

impl Media {
    pub fn new(src: &FileSource, entries: Vec<(String, Range<usize>)>) -> Self {
        Self { src: src.clone(), entries }
    }
}

impl Container for Media {
    fn entry_count(&self) -> Count {
        Count::Exact(self.entries.len() as u64)
    }

    fn entries(&self, range: Range<u64>) -> Result<Vec<Entry>> {
        let end = range.end.min(self.entries.len() as u64) as usize;
        let start = (range.start as usize).min(end);
        Ok(self.entries[start..end].iter().map(|(p, r)| Entry::file(p.clone(), r.len() as u64)).collect())
    }

    fn open(&self, index: u64) -> Result<FileSource> {
        let (path, range) = self.entries.get(index as usize).ok_or_else(|| ParseError::invalid(format!("no entry {index}")))?;
        Ok(self.src.part(path.rsplit('/').next().unwrap_or(path), range.clone()))
    }

    fn memory(&self) -> MemoryUse {
        MemoryUse::heap(self.entries.iter().map(|(p, _)| p.len() as u64 + 40).sum())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_records_nest_and_summarise() {
        let text = b"; FBX 7.4.0 project file\nObjects:  {\n\tGeometry: 1000, \"Geometry::Tri\", \"Mesh\" {\n\t\tVertices: *9 {\n\t\t\ta: 0,0,0,1,0,0,0,1,0\n\t\t}\n\t}\n}\n";
        let tree = ascii_records(text, 100, 10).unwrap();
        let all = |p| tree.children(p, 0..10).unwrap();
        let top = all(None);
        assert_eq!((top[0].label.as_str(), top[0].value.as_str(), top[0].has_children), ("Objects", "", true));
        let geometry = all(Some(top[0].id));
        assert_eq!(geometry[0].value, "1000, \"Geometry::Tri\", \"Mesh\"");
        let vertices = all(Some(geometry[0].id));
        assert_eq!(vertices[0].value, "[9]");
        assert_eq!(all(Some(vertices[0].id))[0].value, "0, 0, 0, 1, … (9 values)");
        assert!(ascii_records(text, 2, 10).is_err());
        assert!(ascii_records(text, 100, 2).is_err());
        // Unbalanced or junk text doesn't panic.
        for junk in [&b"}}}{{{"[..], b"a: \"unterminated", b":::", b"{"] {
            let _ = ascii_records(junk, 100, 10);
        }
    }
}
