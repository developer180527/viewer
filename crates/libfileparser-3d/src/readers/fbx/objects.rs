//! OBJ's **Objects** facet (docs/design/libfileparser-3d.md §1): the material
//! libraries it names, then each object (`o`) with its groups (`g`) and the materials
//! they use (`usemtl`), each with its face count and the line it starts on. Faces
//! before any `o` belong to an unnamed object; an object's faces outside any group have
//! their materials directly under it.

use std::sync::{Arc, Mutex};

use libfileparser_core::{Content, Document, FileSource, Links, NodeId, OwnedTree, ParseContext, ParseError, Place, Result, TreeSource};

#[derive(Default)]
struct Group {
    name: Option<String>,
    line: usize,
    faces: u64,
    /// (material, faces), in the order first used.
    materials: Vec<(String, u64)>,
}

struct Object {
    name: String,
    line: usize,
    groups: Vec<Group>,
}

impl Object {
    fn faces(&self) -> u64 {
        self.groups.iter().map(|g| g.faces).sum()
    }
}

/// The Objects tree, and the line (from 0) each object, group and `mtllib` starts on.
pub struct ObjIndex {
    pub tree: Arc<OwnedTree>,
    starts: Vec<(u64, NodeId)>,
}

#[cfg(test)]
fn objects_tree(text: &[u8], max_nodes: u64) -> Result<Arc<OwnedTree>> {
    Ok(index(text, max_nodes)?.tree)
}

/// Objects get paths by name (`Car`, `Car#1` for a second `o Car`), as the scene's
/// origins key their nodes; a material's first use is `usemtl Paint`.
pub fn index(text: &[u8], max_nodes: u64) -> Result<ObjIndex> {
    let mut libraries: Vec<(String, usize)> = Vec::new();
    let mut objects: Vec<Object> = Vec::new();
    let mut material: Option<String> = None;
    let mut nodes = 0u64;
    let mut count = |n: u64| -> Result<()> {
        nodes += n;
        if nodes > max_nodes { Err(ParseError::too_large(libfileparser_core::Limit::Items, nodes, max_nodes)) } else { Ok(()) }
    };
    for (i, line) in text.split(|&b| b == b'\n').enumerate() {
        let line = String::from_utf8_lossy(line);
        let line = line.trim();
        let (key, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let rest = rest.trim();
        let number = i + 1;
        match key {
            "mtllib" => {
                count(1)?;
                libraries.push((rest.to_string(), number));
            }
            "o" => {
                count(1)?;
                objects.push(Object { name: rest.to_string(), line: number, groups: Vec::new() });
            }
            "g" => {
                count(1)?;
                if objects.is_empty() {
                    objects.push(Object { name: String::new(), line: number, groups: Vec::new() });
                }
                let name = if rest.is_empty() { "default".to_string() } else { rest.to_string() };
                objects.last_mut().unwrap().groups.push(Group { name: Some(name), line: number, ..Group::default() });
            }
            "usemtl" => material = Some(rest.to_string()),
            "f" => {
                if objects.is_empty() {
                    objects.push(Object { name: String::new(), line: number, groups: Vec::new() });
                }
                let object = objects.last_mut().unwrap();
                if object.groups.is_empty() {
                    object.groups.push(Group { line: number, ..Group::default() });
                }
                let group = object.groups.last_mut().unwrap();
                group.faces += 1;
                let name = material.clone().unwrap_or_default();
                match group.materials.iter_mut().find(|(m, _)| *m == name) {
                    Some((_, n)) => *n += 1,
                    None => {
                        count(1)?;
                        group.materials.push((name, 1));
                    }
                }
            }
            _ => {}
        }
    }
    let mut tree = OwnedTree::new();
    let mut starts = Vec::new();
    for (lib, line) in libraries {
        let id = tree.add(None, "mtllib", format!("{lib} · line {line}"));
        starts.push((line as u64 - 1, id));
    }
    let faces = |n: u64| if n == 1 { "1 face".to_string() } else { format!("{n} faces") };
    let mut keys = super::Keys::default();
    for o in objects {
        let label = if o.name.is_empty() { "(unnamed object)".to_string() } else { o.name.clone() };
        let node = tree.add_at(None, label, format!("{} · line {}", faces(o.faces()), o.line), keys.next(o.name.clone()));
        starts.push((o.line as u64 - 1, node));
        for g in o.groups {
            let parent = match &g.name {
                Some(name) => {
                    let id = tree.add(Some(node), format!("g {name}"), format!("{} · line {}", faces(g.faces), g.line));
                    starts.push((g.line as u64 - 1, id));
                    id
                }
                None => node,
            };
            for (m, n) in g.materials {
                if m.is_empty() {
                    tree.add(Some(parent), "(no material)", faces(n));
                } else {
                    tree.add_at(Some(parent), format!("usemtl {m}"), faces(n), format!("usemtl {m}"));
                }
            }
        }
    }
    starts.sort();
    Ok(ObjIndex { tree: Arc::new(tree), starts })
}

/// The index, made the first time the Objects facet or a link asks, then shared.
pub struct Shared {
    src: FileSource,
    index: Mutex<Option<Arc<ObjIndex>>>,
}

impl Shared {
    pub fn new(src: &FileSource) -> Self {
        Self { src: src.clone(), index: Mutex::new(None) }
    }

    pub fn get(&self, cx: &ParseContext) -> Result<Arc<ObjIndex>> {
        let mut index = self.index.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(i) = &*index {
            return Ok(i.clone());
        }
        let i = Arc::new(self::index(self.src.bytes(), cx.limits.items)?);
        *index = Some(i.clone());
        Ok(i)
    }
}

/// OBJ's own links: an object, group or `mtllib` and the line it starts on, and a line
/// and the one starting nearest above it. (Objects and their Scene nodes are linked
/// through the scene's origins.)
pub struct ObjLinks(pub Arc<Shared>);

impl Links for ObjLinks {
    fn links(&self, doc: &Document, from: &Place, cx: &ParseContext) -> Result<Vec<Place>> {
        let Some(facet) = doc.facet_index("Objects") else { return Ok(Vec::new()) };
        let index = self.0.get(cx)?;
        Ok(match from {
            Place::Node { facet: f, node } if *f == facet => {
                index.starts.iter().filter(|(_, n)| n == node).map(|&(line, _)| Place::Lines(line..line + 1)).collect()
            }
            Place::Lines(lines) => {
                let above = index.starts.iter().filter(|(l, _)| *l <= lines.start).max_by_key(|(l, n)| (*l, std::cmp::Reverse(n.0)));
                above.map(|&(_, node)| Place::Node { facet, node }).into_iter().collect()
            }
            _ => Vec::new(),
        })
    }
}

impl ObjIndex {
    pub fn content(&self) -> Content {
        Content::Tree(self.tree.clone() as Arc<dyn TreeSource>)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libfileparser_core::TreeSource;

    fn lines(tree: &OwnedTree) -> Vec<String> {
        fn walk(t: &OwnedTree, p: Option<libfileparser_core::NodeId>, d: usize, out: &mut Vec<String>) {
            for n in t.children(p, 0..100).unwrap() {
                out.push(format!("{}{} = {}", "  ".repeat(d), n.label, n.value));
                walk(t, Some(n.id), d + 1, out);
            }
        }
        let mut out = Vec::new();
        walk(tree, None, 0, &mut out);
        out
    }

    #[test]
    fn objects_groups_and_materials() {
        let obj = b"mtllib a.mtl\nv 0 0 0\nf 1 1 1\no Car\ng body\nusemtl Paint\nf 1 1 1\nf 1 1 1\nusemtl Chrome\nf 1 1 1\ng wheels\nf 1 1 1\no Empty\n";
        assert_eq!(
            lines(&objects_tree(obj, 100).unwrap()),
            [
                "mtllib = a.mtl · line 1",
                "(unnamed object) = 1 face · line 3",
                "  (no material) = 1 face",
                "Car = 4 faces · line 4",
                "  g body = 3 faces · line 5",
                "    usemtl Paint = 2 faces",
                "    usemtl Chrome = 1 face",
                "  g wheels = 1 face · line 11",
                "    usemtl Chrome = 1 face",
                "Empty = 0 faces · line 13",
            ]
        );
        assert!(objects_tree(obj, 3).is_err());
    }
}
