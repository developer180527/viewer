//! glTF's own facets (docs/design/libfileparser-3d.md §1): the **glTF** tree (the
//! file's JSON, an index shown with the name of what it points to), and for GLB the
//! **JSON chunk** as text and the **Chunks** (JSON, BIN and each image stored in the
//! BIN chunk) as files the front door can open; then **Extensions used**.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Mutex;

use gltf_lite::{ExtensionSupport, GlbChunk};
use libfileparser_core::{
    Container, Count, Entry, FileSource, LineText, MemoryUse, NodeId, OwnedTable, ParseError, Result, TreeNode, TreeSource,
};
use serde_json::Value;

/// The JSON of a `.gltf` file, or a GLB's JSON chunk.
pub fn json_range(bytes: &[u8]) -> Result<Range<usize>> {
    match gltf_lite::glb_chunks(bytes).map_err(ParseError::corrupt)? {
        None => Ok(0..bytes.len()),
        Some(chunks) => chunks.into_iter().find(|c| c.kind == GlbChunk::JSON).map(|c| c.range).ok_or_else(|| ParseError::corrupt("GLB has no JSON chunk")),
    }
}

pub fn parse(src: &FileSource) -> Result<Value> {
    let range = json_range(src.bytes())?;
    serde_json::from_slice(&src.bytes()[range]).map_err(ParseError::corrupt)
}

// ---------------------------------------------------------------- the glTF tree

#[derive(Clone, PartialEq, Eq, Hash)]
enum Seg {
    Key(String),
    Index(usize),
}

/// A JSON document read a level at a time. glTF objects refer to each other by index
/// (`"mesh": 0`), so an index is shown with the name of what it points to.
pub struct GltfTree {
    root: Value,
    /// Ids handed out, by path, so asking twice gives the same ones.
    ids: Mutex<Ids>,
}

/// Paths by id, and ids by path.
type Ids = (Vec<Vec<Seg>>, HashMap<Vec<Seg>, u64>);

/// Which top-level array a key's index points into.
fn points_to(key: &str, parent_key: Option<&str>) -> Option<&'static str> {
    Some(match (key, parent_key) {
        ("mesh", _) => "meshes",
        ("material", _) => "materials",
        ("skin", _) => "skins",
        ("camera", _) => "cameras",
        ("buffer", _) => "buffers",
        ("bufferView", _) => "bufferViews",
        ("sampler", _) => "samplers",
        ("source", _) => "images",
        ("indices" | "input" | "output" | "inverseBindMatrices", _) => "accessors",
        ("node" | "skeleton", _) => "nodes",
        ("scene", _) => "scenes",
        (_, Some("attributes")) => "accessors",
        (_, Some("children" | "nodes" | "joints")) => "nodes",
        _ => return None,
    })
}

impl GltfTree {
    pub fn new(root: Value) -> Self {
        Self { root, ids: Mutex::new((Vec::new(), HashMap::new())) }
    }

    fn id(&self, path: Vec<Seg>) -> NodeId {
        let mut ids = self.ids.lock().unwrap_or_else(|e| e.into_inner());
        let (list, index) = &mut *ids;
        let next = list.len() as u64;
        let id = *index.entry(path.clone()).or_insert(next);
        if id == next {
            list.push(path);
        }
        NodeId(id)
    }

    fn segments(&self, id: Option<NodeId>) -> Result<Vec<Seg>> {
        let Some(id) = id else { return Ok(Vec::new()) };
        let ids = self.ids.lock().unwrap_or_else(|e| e.into_inner());
        ids.0.get(id.0 as usize).cloned().ok_or_else(|| ParseError::invalid(format!("no node {}", id.0)))
    }

    fn at(&self, path: &[Seg]) -> Option<&Value> {
        path.iter().try_fold(&self.root, |v, seg| match seg {
            Seg::Key(k) => v.get(k),
            Seg::Index(i) => v.get(i),
        })
    }

    /// The name of item `index` of the top-level array `collection`, if it has one.
    fn name_of(&self, collection: &str, index: &Value) -> Option<&str> {
        let i = index.as_u64()? as usize;
        self.root.get(collection)?.get(i)?.get("name")?.as_str()
    }

    fn node(&self, path: Vec<Seg>, value: &Value) -> TreeNode {
        fn key(seg: Option<&Seg>) -> Option<&str> {
            match seg {
                Some(Seg::Key(k)) => Some(k.as_str()),
                _ => None,
            }
        }
        let label = match path.last() {
            Some(Seg::Key(k)) => k.clone(),
            Some(Seg::Index(i)) => format!("[{i}]"),
            None => String::new(),
        };
        let mut text = summary(value);
        // An index into another array: "0 · Cube". An array item takes its array's key.
        let (own, parent) = match path.last() {
            Some(Seg::Key(k)) => (Some(k.as_str()), key(path.len().checked_sub(2).and_then(|i| path.get(i)))),
            _ => (None, key(path.len().checked_sub(2).and_then(|i| path.get(i)))),
        };
        // An animation's channels name its own samplers, not the file's.
        let in_animation = path.first() == Some(&Seg::Key("animations".into()));
        let target = match own {
            Some("sampler") if in_animation => None,
            Some(k) => points_to(k, parent),
            None => parent.and_then(|p| points_to("", Some(p))),
        };
        if let Some(name) = target.and_then(|c| self.name_of(c, value)) {
            text = format!("{text} · {name}");
        }
        let has_children = matches!(value, Value::Object(o) if !o.is_empty()) || matches!(value, Value::Array(a) if !a.is_empty());
        TreeNode::new(self.id(path), label, text, has_children)
    }
}

impl TreeSource for GltfTree {
    fn child_count(&self, parent: Option<NodeId>) -> Result<Count> {
        let path = self.segments(parent)?;
        let n = match self.at(&path) {
            Some(Value::Object(o)) => o.len(),
            Some(Value::Array(a)) => a.len(),
            _ => 0,
        };
        Ok(Count::Exact(n as u64))
    }

    fn children(&self, parent: Option<NodeId>, range: Range<u64>) -> Result<Vec<TreeNode>> {
        let path = self.segments(parent)?;
        let child = |seg: Seg| {
            let mut p = path.clone();
            p.push(seg);
            p
        };
        let (start, end) = (range.start as usize, range.end.min(usize::MAX as u64) as usize);
        Ok(match self.at(&path) {
            Some(Value::Object(o)) => o.iter().skip(start).take(end.saturating_sub(start)).map(|(k, v)| self.node(child(Seg::Key(k.clone())), v)).collect(),
            Some(Value::Array(a)) => a.iter().enumerate().skip(start).take(end.saturating_sub(start)).map(|(i, v)| self.node(child(Seg::Index(i)), v)).collect(),
            _ => Vec::new(),
        })
    }

    fn memory(&self) -> MemoryUse {
        // Roughly: JSON values are a few words each, plus their text.
        fn size(v: &Value) -> u64 {
            48 + match v {
                Value::String(s) => s.len() as u64,
                Value::Array(a) => a.iter().map(size).sum(),
                Value::Object(o) => o.iter().map(|(k, v)| k.len() as u64 + size(v)).sum(),
                _ => 0,
            }
        }
        MemoryUse::heap(size(&self.root))
    }

    /// A JSON pointer (RFC 6901): `/nodes/3`, `/meshes/0/primitives/1`.
    fn find(&self, path: &str) -> Result<Option<NodeId>> {
        let Some(rest) = path.strip_prefix('/') else { return Ok(None) };
        let mut segs = Vec::new();
        let mut at = &self.root;
        for raw in rest.split('/') {
            let key = raw.replace("~1", "/").replace("~0", "~");
            let (seg, next) = match at {
                Value::Object(o) => match o.get(&key) {
                    Some(v) => (Seg::Key(key), v),
                    None => return Ok(None),
                },
                Value::Array(a) => match key.parse::<usize>().ok().and_then(|i| a.get(i).map(|v| (i, v))) {
                    Some((i, v)) => (Seg::Index(i), v),
                    None => return Ok(None),
                },
                _ => return Ok(None),
            };
            segs.push(seg);
            at = next;
        }
        Ok(Some(self.id(segs)))
    }

    fn path(&self, node: NodeId) -> Result<Option<String>> {
        let segs = self.segments(Some(node))?;
        Ok(Some(pointer(&segs)))
    }
}

/// A path as a JSON pointer.
fn pointer(segs: &[Seg]) -> String {
    segs.iter()
        .map(|s| match s {
            Seg::Key(k) => format!("/{}", k.replace('~', "~0").replace('/', "~1")),
            Seg::Index(i) => format!("/{i}"),
        })
        .collect()
}

/// A value as one short line: a branch by its name, or its size; long strings (a
/// `data:` URI) cut short.
fn summary(v: &Value) -> String {
    match v {
        Value::Object(o) => match o.get("name").and_then(Value::as_str) {
            Some(name) => format!("\"{name}\""),
            None => {
                let keys: Vec<&str> = o.keys().take(4).map(String::as_str).collect();
                let more = if o.len() > 4 { ", …" } else { "" };
                format!("{{{}{more}}}", keys.join(", "))
            }
        },
        Value::Array(a) => format!("[{}]", a.len()),
        Value::String(s) if s.chars().count() > 80 => {
            let head: String = s.chars().take(60).collect();
            format!("\"{head}…\" ({} characters)", s.chars().count())
        }
        Value::String(s) => format!("\"{s}\""),
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------- GLB

/// A GLB's JSON chunk as text. Exporters write it on one line, so a short one is
/// shown as it is and a long one laid out, an object or array per line.
pub fn json_text(src: &FileSource) -> Result<LineText> {
    let range = json_range(src.bytes())?;
    let json = src.part(format!("{}.json", stem(src.name())), range);
    let lines = json.bytes().iter().filter(|&&b| b == b'\n').count();
    if lines >= 2 || json.len() <= 200 {
        return Ok(LineText::new(json, Some("json")));
    }
    let value: Value = serde_json::from_slice(json.bytes()).map_err(ParseError::corrupt)?;
    let pretty = serde_json::to_vec_pretty(&value).map_err(ParseError::corrupt)?;
    Ok(LineText::new(FileSource::from_bytes(json.name(), pretty), Some("json")))
}

fn stem(name: &str) -> &str {
    name.rsplit_once('.').map_or(name, |(s, _)| s)
}

/// A GLB's chunks, and the images stored in its BIN chunk, as files.
pub struct GlbChunks {
    src: FileSource,
    entries: Vec<(Entry, Range<usize>)>,
}

impl GlbChunks {
    pub fn new(src: &FileSource, json: Option<&Value>) -> Result<Self> {
        let chunks = gltf_lite::glb_chunks(src.bytes()).map_err(ParseError::corrupt)?.unwrap_or_default();
        let mut entries = Vec::new();
        let mut bin = None;
        for c in &chunks {
            let path = match c.kind {
                GlbChunk::JSON => "JSON.json".to_string(),
                GlbChunk::BIN => {
                    bin.get_or_insert(c.range.clone());
                    "BIN.bin".to_string()
                }
                _ => format!("{}.chunk", c.name()),
            };
            entries.push((Entry::file(path, c.range.len() as u64), c.range.clone()));
        }
        // Images in a buffer view of the BIN chunk (buffer 0, with no URI).
        if let (Some(bin), Some(json)) = (bin, json) {
            let empty = Vec::new();
            let list = |key: &str| json.get(key).and_then(Value::as_array).unwrap_or(&empty);
            let (images, views) = (list("images"), list("bufferViews"));
            let mut used = std::collections::HashSet::new();
            for (i, image) in images.iter().enumerate() {
                let Some(view) = image.get("bufferView").and_then(Value::as_u64).and_then(|v| views.get(v as usize)) else { continue };
                if view.get("buffer").and_then(Value::as_u64) != Some(0) {
                    continue;
                }
                let at = |k: &str| view.get(k).and_then(Value::as_u64).unwrap_or(0) as usize;
                let (offset, len) = (at("byteOffset"), at("byteLength"));
                let Some(start) = bin.start.checked_add(offset) else { continue };
                let Some(end) = start.checked_add(len).filter(|&e| e <= bin.end) else { continue };
                let ext = match image.get("mimeType").and_then(Value::as_str) {
                    Some("image/png") => "png",
                    Some("image/jpeg") => "jpg",
                    Some("image/webp") => "webp",
                    Some("image/ktx2") => "ktx2",
                    Some("image/avif") => "avif",
                    _ => "bin",
                };
                let name = image.get("name").and_then(Value::as_str).filter(|n| !n.is_empty()).map_or_else(|| format!("image {i}"), |n| n.replace(['/', '\\'], "_"));
                // Two images may share a name.
                let mut path = format!("images/{name}.{ext}");
                if !used.insert(path.clone()) {
                    path = format!("images/{name} ({i}).{ext}");
                    used.insert(path.clone());
                }
                entries.push((Entry::file(path, len as u64), start..end));
            }
        }
        Ok(Self { src: src.clone(), entries })
    }
}

impl Container for GlbChunks {
    fn entry_count(&self) -> Count {
        Count::Exact(self.entries.len() as u64)
    }

    fn entries(&self, range: Range<u64>) -> Result<Vec<Entry>> {
        let end = range.end.min(self.entries.len() as u64) as usize;
        let start = (range.start as usize).min(end);
        Ok(self.entries[start..end].iter().map(|(e, _)| e.clone()).collect())
    }

    fn open(&self, index: u64) -> Result<FileSource> {
        let (entry, range) = self.entries.get(index as usize).ok_or_else(|| ParseError::invalid(format!("no entry {index}")))?;
        let name = entry.path.rsplit('/').next().unwrap_or(&entry.path);
        Ok(self.src.part(name, range.clone()))
    }

    fn memory(&self) -> MemoryUse {
        MemoryUse::heap((self.entries.len() * 96) as u64)
    }
}

// ---------------------------------------------------------------- extensions

/// The extensions the file uses: whether it requires each, and whether it's read here.
pub fn extensions_table(used: &[String], required: &[String]) -> Option<OwnedTable> {
    let mut names: Vec<&String> = used.iter().collect();
    names.extend(required.iter().filter(|r| !used.contains(r)));
    if names.is_empty() {
        return None;
    }
    let rows = names
        .into_iter()
        .map(|name| {
            let required = required.contains(name);
            let support = match (gltf_lite::extension_support(name), required) {
                (ExtensionSupport::Read, _) => "read",
                (ExtensionSupport::Unsupported, _) => "not supported",
                (ExtensionSupport::Ignored, true) => "not read: the model may look different",
                (ExtensionSupport::Ignored, false) => "not read",
            };
            vec![name.clone(), if required { "yes" } else { "no" }.into(), support.into()]
        })
        .collect();
    Some(OwnedTable::new(vec!["Extension".into(), "Required".into(), "Here".into()], rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indices_are_shown_with_what_they_point_to() {
        let json = serde_json::json!({
            "nodes": [{ "name": "Root", "children": [1] }, { "name": "Wheel", "mesh": 0 }],
            "meshes": [{ "name": "Tyre", "primitives": [{ "attributes": { "POSITION": 0 }, "material": 0 }] }],
            "accessors": [{ "name": "positions", "count": 3 }],
            "materials": [{ "name": "Rubber" }],
            "images": [{ "uri": format!("data:image/png;base64,{}", "A".repeat(200)) }]
        });
        let tree = GltfTree::new(json);
        let all = |parent| tree.children(parent, 0..100).unwrap();
        let top = all(None);
        let labels: Vec<_> = top.iter().map(|n| (n.label.as_str(), n.value.as_str())).collect();
        assert_eq!(labels[..2], [("nodes", "[2]"), ("meshes", "[1]")]);
        let nodes = all(Some(top[0].id));
        assert_eq!((nodes[1].label.as_str(), nodes[1].value.as_str()), ("[1]", "\"Wheel\""));
        let wheel = all(Some(nodes[1].id));
        assert_eq!(wheel[1].value, "0 · Tyre");
        let children = all(Some(all(Some(nodes[0].id))[1].id));
        assert_eq!(children[0].value, "1 · Wheel");
        let primitive = all(Some(all(Some(all(Some(top[1].id))[0].id))[1].id))[0].id;
        let primitive = all(Some(primitive));
        assert_eq!(primitive[1].value, "0 · Rubber");
        assert_eq!(all(Some(primitive[0].id))[0].value, "0 · positions");
        let image = all(Some(all(Some(top[4].id))[0].id));
        assert!(image[0].value.ends_with("…\" (222 characters)"), "{}", image[0].value);
        // The same node asked for twice has the same id; an unknown one is an error.
        assert_eq!(all(None)[0].id, top[0].id);
        assert!(tree.children(Some(NodeId(999)), 0..1).is_err());
    }
}
