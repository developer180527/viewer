//! USD's own facets (docs/design/libfileparser-3d.md §1): the composed **Stage** (prims,
//! their types, metadata and properties), the **Layers** that went into it (the layer
//! stack and the references and payloads, each with its status), and the **Variants**
//! (each variant set, its variants, the one selected).

use std::collections::HashMap;
use std::ops::Range;
use std::sync::{Arc, Mutex};

use libfileparser_core::{
    Content, Count, Dependencies, Document, FileSource, Links, MemoryUse, NodeId, OwnedTree, ParseContext, ParseError, Place, Result, TreeNode,
    TreeSource,
};
use usd_lite::{ArcKind, Layer, LayerArc, Spec, Stage, Value, VariantSet, property_path};

/// The composed stage, composed again the first time the Stage facet or a link asks
/// for it (rather than kept beside the scene from the start), then shared.
pub struct Composed {
    src: FileSource,
    deps: Dependencies,
    stage: Mutex<Option<Arc<Stage>>>,
}

impl Composed {
    pub fn new(src: &FileSource, deps: &Dependencies) -> Self {
        Self { src: src.clone(), deps: deps.clone(), stage: Mutex::new(None) }
    }

    pub fn get(&self, cx: &ParseContext) -> Result<Arc<Stage>> {
        let mut stage = self.stage.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(s) = &*stage {
            return Ok(s.clone());
        }
        let s = Arc::new(super::compose_stage(&self.src, cx, &self.deps).map_err(crate::readers::parse_error)?);
        *stage = Some(s.clone());
        Ok(s)
    }
}

// ---------------------------------------------------------------- stage

/// What a Stage node stands for.
#[derive(Clone, PartialEq, Eq, Hash)]
enum Key {
    /// The stage's own metadata (`upAxis`, `defaultPrim`…), a group at the top.
    StageMetadata,
    Prim(String),
    /// A field of a prim, or of the stage (`/`).
    Field(String, String),
    Property(String),
}

/// The composed stage, read a level at a time: at the top the stage's metadata, then
/// the root prims; under a prim its metadata, its properties, then its children.
pub struct StageTree {
    stage: Arc<Stage>,
    /// Ids handed out, so asking twice gives the same ones.
    ids: Mutex<(Vec<Key>, HashMap<Key, u64>)>,
}

/// Fields that are the tree's shape, not something to show.
const STRUCTURE: &[&str] = &["primChildren", "properties", "typeName", "specifier"];

impl StageTree {
    pub fn new(stage: Arc<Stage>) -> Self {
        Self { stage, ids: Mutex::new((Vec::new(), HashMap::new())) }
    }

    fn layer(&self) -> &Layer {
        &self.stage.layer
    }

    fn id(&self, key: Key) -> NodeId {
        let mut ids = self.ids.lock().unwrap_or_else(|e| e.into_inner());
        let (list, index) = &mut *ids;
        let next = list.len() as u64;
        let id = *index.entry(key.clone()).or_insert(next);
        if id == next {
            list.push(key);
        }
        NodeId(id)
    }

    fn key(&self, id: NodeId) -> Result<Key> {
        let ids = self.ids.lock().unwrap_or_else(|e| e.into_inner());
        ids.0.get(id.0 as usize).cloned().ok_or_else(|| ParseError::invalid(format!("no node {}", id.0)))
    }

    fn fields<'a>(&'a self, path: &str) -> impl Iterator<Item = &'a (String, Value)> {
        self.layer().spec(path).into_iter().flat_map(|s| s.fields.iter()).filter(|(n, _)| !STRUCTURE.contains(&n.as_str()))
    }

    fn properties(&self, prim: &str) -> Vec<String> {
        self.layer().field(prim, "properties").and_then(Value::as_tokens).unwrap_or_default().to_vec()
    }

    /// What `parent` holds, as keys.
    fn keys(&self, parent: Option<&Key>) -> Vec<Key> {
        match parent {
            None => {
                let mut keys = Vec::new();
                if self.fields("/").next().is_some() {
                    keys.push(Key::StageMetadata);
                }
                keys.extend(self.layer().root_prims().into_iter().map(Key::Prim));
                keys
            }
            Some(Key::StageMetadata) => self.fields("/").map(|(n, _)| Key::Field("/".into(), n.clone())).collect(),
            Some(Key::Prim(p)) => {
                let mut keys: Vec<Key> = self.fields(p).map(|(n, _)| Key::Field(p.clone(), n.clone())).collect();
                keys.extend(self.properties(p).into_iter().map(|n| Key::Property(property_path(p, &n))));
                keys.extend(self.layer().children(p).into_iter().map(Key::Prim));
                keys
            }
            Some(Key::Field(..) | Key::Property(_)) => Vec::new(),
        }
    }

    fn node(&self, key: Key) -> TreeNode {
        let (label, value, has_children) = match &key {
            Key::StageMetadata => ("Stage metadata".to_string(), format!("{} fields", self.fields("/").count()), true),
            Key::Prim(p) => {
                let name = p.rsplit('/').next().unwrap_or(p).to_string();
                let ty = self.layer().type_name(p).unwrap_or("");
                let value = match self.layer().field(p, "specifier") {
                    Some(Value::Specifier(usd_lite::Specifier::Class)) => format!("class {ty}").trim_end().to_string(),
                    Some(Value::Specifier(usd_lite::Specifier::Over)) => format!("over {ty}").trim_end().to_string(),
                    _ => ty.to_string(),
                };
                let has = self.fields(p).next().is_some() || !self.properties(p).is_empty() || !self.layer().children(p).is_empty();
                (name, value, has)
            }
            Key::Field(path, name) => (name.clone(), self.layer().field(path, name).map(summary).unwrap_or_default(), false),
            Key::Property(path) => {
                let name = path.rsplit_once('.').map_or(path.as_str(), |(_, n)| n).to_string();
                (name, self.layer().spec(path).map(property_summary).unwrap_or_default(), false)
            }
        };
        TreeNode::new(self.id(key), label, value, has_children)
    }
}

impl TreeSource for StageTree {
    fn child_count(&self, parent: Option<NodeId>) -> Result<Count> {
        let parent = parent.map(|p| self.key(p)).transpose()?;
        Ok(Count::Exact(self.keys(parent.as_ref()).len() as u64))
    }

    fn children(&self, parent: Option<NodeId>, range: Range<u64>) -> Result<Vec<TreeNode>> {
        let parent = parent.map(|p| self.key(p)).transpose()?;
        let keys = self.keys(parent.as_ref());
        let end = range.end.min(keys.len() as u64) as usize;
        let start = (range.start as usize).min(end);
        Ok(keys[start..end].iter().cloned().map(|k| self.node(k)).collect())
    }

    fn memory(&self) -> MemoryUse {
        // The arrays are counted where they're shared from (the scene's source); this is
        // the specs' own size, roughly.
        MemoryUse::heap((self.layer().specs.len() * 128) as u64)
    }

    /// A prim path (`/World/Car`) or a property path (`/World/Car.points`).
    fn find(&self, path: &str) -> Result<Option<NodeId>> {
        let key = match self.layer().spec(path) {
            Some(spec) if spec.spec_type == usd_lite::SpecType::Prim => Key::Prim(path.to_string()),
            Some(_) if path.contains('.') => Key::Property(path.to_string()),
            _ => return Ok(None),
        };
        Ok(Some(self.id(key)))
    }

    fn path(&self, node: NodeId) -> Result<Option<String>> {
        Ok(match self.key(node)? {
            Key::Prim(p) | Key::Property(p) => Some(p),
            Key::StageMetadata | Key::Field(..) => None,
        })
    }
}

/// USD's own links: a Stage prim (or property) and the line it starts on in the USDA
/// text, when the root layer defines it; and the layer that defines it, in Layers.
/// (A prim and its Scene nodes are linked through the scene's origins.)
pub struct UsdLinks {
    pub composed: Arc<Composed>,
    /// Whether the file is USDA text, so its lines are the Source's lines.
    pub text: bool,
}

impl Links for UsdLinks {
    fn links(&self, doc: &Document, from: &Place, cx: &ParseContext) -> Result<Vec<Place>> {
        let Some(stage_facet) = doc.facet_index("Stage") else { return Ok(Vec::new()) };
        let tree = || -> Result<Option<Arc<dyn TreeSource>>> {
            Ok(match doc.facet(stage_facet, cx)? {
                Content::Tree(t) => Some(t),
                _ => None,
            })
        };
        match from {
            Place::Node { facet, node } if *facet == stage_facet => {
                let Some(tree) = tree()? else { return Ok(Vec::new()) };
                let Some(path) = tree.path(*node)? else { return Ok(Vec::new()) };
                let prim = path.split_once('.').map_or(path.as_str(), |(p, _)| p);
                let stage = self.composed.get(cx)?;
                let Some(source) = stage.sources.get(prim) else { return Ok(Vec::new()) };
                let mut out = Vec::new();
                if self.text
                    && source.layer == 0
                    && let Some(line) = source.line
                {
                    let line = u64::from(line.saturating_sub(1));
                    out.push(Place::Lines(line..line + 1));
                }
                if let (Some(layers), Some(id)) = (doc.facet_index("Layers"), stage.layers.get(source.layer))
                    && let Content::Tree(t) = doc.facet(layers, cx)?
                    && let Some(node) = t.find(id)?
                {
                    out.push(Place::Node { facet: layers, node });
                }
                Ok(out)
            }
            // A line of the USDA text: the prim whose spec starts nearest above it.
            Place::Lines(lines) if self.text => {
                let stage = self.composed.get(cx)?;
                let line = lines.start + 1;
                let best = stage
                    .sources
                    .iter()
                    .filter_map(|(prim, s)| Some((prim, s, u64::from(s.line?))))
                    .filter(|(_, s, l)| s.layer == 0 && *l <= line)
                    // The latest start; among prims sharing it, the one at its own path.
                    .max_by_key(|(prim, s, l)| (*l, **prim == s.path, std::cmp::Reverse(prim.len())));
                let Some((prim, ..)) = best else { return Ok(Vec::new()) };
                let Some(tree) = tree()? else { return Ok(Vec::new()) };
                Ok(tree.find(prim)?.map(|node| Place::Node { facet: stage_facet, node }).into_iter().collect())
            }
            _ => Ok(Vec::new()),
        }
    }
}

/// An attribute's value (its default, else how many time samples), a connection's
/// source or a relationship's targets.
fn property_summary(spec: &Spec) -> String {
    let ty = spec.field("typeName").and_then(Value::as_str).unwrap_or("");
    if let Some(p) = spec.field("connectionPaths").and_then(Value::as_paths).filter(|p| !p.is_empty()) {
        return format!("{ty} → {}", p.join(", ")).trim_start().to_string();
    }
    if let Some(p) = spec.field("targetPaths").and_then(Value::as_paths) {
        return format!("rel → {}", if p.is_empty() { "(none)".to_string() } else { p.join(", ") });
    }
    let value = match (spec.field("default"), spec.field("timeSamples")) {
        (Some(v), _) => summary(v),
        (None, Some(Value::TimeSamples(s))) => format!("{} time samples", s.len()),
        _ => String::new(),
    };
    match (ty.strip_suffix("[]"), value.strip_prefix('[')) {
        // `point3f[8]` rather than `point3f[] [8]`.
        (Some(base), Some(rest)) if value.ends_with(" items]") => format!("{base}[{}]", rest.trim_end_matches(" items]")),
        _ => format!("{ty} {value}").trim().to_string(),
    }
}

/// A value as one short line.
fn summary(v: &Value) -> String {
    let num = |x: f64| {
        let s = format!("{x:.4}");
        let s = s.trim_end_matches('0').trim_end_matches('.');
        if s == "-0" { "0".to_string() } else { s.to_string() }
    };
    let list = |xs: &[f64]| format!("({})", xs.iter().map(|&x| num(x)).collect::<Vec<_>>().join(", "));
    let items = |n: usize| format!("[{n} items]");
    let names = |xs: &[String]| {
        let shown: Vec<&str> = xs.iter().take(8).map(String::as_str).collect();
        let more = if xs.len() > 8 { format!(", …{} more", xs.len() - 8) } else { String::new() };
        format!("[{}{more}]", shown.join(", "))
    };
    match v {
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => num(*f),
        Value::String(s) => format!("\"{s}\""),
        Value::Token(s) => s.clone(),
        Value::Asset(a) => format!("@{a}@"),
        Value::Vec2(x) => list(x),
        Value::Vec3(x) => list(x),
        Value::Vec4(x) | Value::Quat(x) => list(x),
        Value::Matrix4(m) => format!("matrix, translate {}", list(&m[12..15])),
        Value::BoolArray(a) => items(a.len()),
        Value::IntArray(a) => items(a.len()),
        Value::Int64Array(a) => items(a.len()),
        Value::FloatArray(a) => items(a.len()),
        Value::DoubleArray(a) => items(a.len()),
        Value::Vec2Array(a) => items(a.len()),
        Value::Vec3Array(a) => items(a.len()),
        Value::Vec4Array(a) | Value::QuatArray(a) => items(a.len()),
        Value::Matrix4Array(a) => items(a.len()),
        Value::TokenArray(a) | Value::StringArray(a) | Value::Paths(a) => names(a),
        Value::AssetArray(a) => names(&a.iter().map(|a| format!("@{a}@")).collect::<Vec<_>>()),
        Value::References(r) => {
            let r: Vec<String> = r.iter().map(|r| format!("{}{}", if r.asset.is_empty() { String::new() } else { format!("@{}@", r.asset) }, r.prim.as_deref().map(|p| format!("<{p}>")).unwrap_or_default())).collect();
            names(&r)
        }
        Value::Dictionary(d) => format!("{{{}}}", d.iter().take(8).map(|(k, v)| format!("{k}: {}", summary(v))).collect::<Vec<_>>().join(", ")),
        Value::TimeSamples(s) => format!("{} time samples", s.len()),
        Value::Specifier(s) => format!("{s:?}").to_lowercase(),
        Value::Blocked => "None (blocked)".into(),
        Value::Other(name) => format!("({name})"),
    }
}

// ---------------------------------------------------------------- layers

/// The layers composed into the stage: the root, then under each layer the arcs it
/// authors (sublayers, references, payloads), each with what it brought in and its
/// status, and under each the arcs of the layer it read. A layer reached twice is
/// expanded the first time only.
pub fn layers_tree(root: &str, arcs: &[LayerArc], deps: Option<&Dependencies>) -> OwnedTree {
    let mut tree = OwnedTree::new();
    let top = tree.add_at(None, file_name(root), "root layer", root);
    let mut shown = vec![root.to_string()];
    // (layer, its node), breadth first so a layer is expanded where it's met first.
    let mut queue = std::collections::VecDeque::from([(root.to_string(), top)]);
    while let Some((layer, node)) = queue.pop_front() {
        for arc in arcs.iter().filter(|a| a.from == layer) {
            let status = match &arc.layer {
                Some(_) => "read".to_string(),
                None => {
                    let reference = usd_lite::anchored(&arc.from, &arc.asset);
                    deps.and_then(|d| d.list().into_iter().find(|d| d.reference == reference)).map_or("not read".into(), |d| d.status.label().to_string())
                }
            };
            let into = match (arc.kind, arc.prims.as_slice()) {
                (ArcKind::Sublayer, _) | (_, []) => String::new(),
                (_, [one]) if arc.prim_count == 1 => format!(" · into {one}"),
                (_, [first, ..]) => format!(" · into {} prims, first {first}", arc.prim_count),
            };
            let label = arc.asset.clone();
            let value = format!("{} · {status}{into}", arc.kind.label());
            // A layer's node is where it's first met; its path is the layer's identifier.
            let child = match &arc.layer {
                Some(read) if !shown.contains(read) => tree.add_at(Some(node), label, value, read.clone()),
                _ => tree.add(Some(node), label, value),
            };
            if let Some(read) = &arc.layer
                && !shown.contains(read)
            {
                shown.push(read.clone());
                queue.push_back((read.clone(), child));
            }
        }
    }
    tree
}

fn file_name(id: &str) -> String {
    id.rsplit(['/', '\\']).next().unwrap_or(id).to_string()
}

// ---------------------------------------------------------------- variants

/// Every variant set on the stage, by prim: the set, then its variants, the selected
/// one marked.
pub fn variants_tree(sets: &[VariantSet]) -> OwnedTree {
    let mut tree = OwnedTree::new();
    let mut prims: Vec<(&str, NodeId)> = Vec::new();
    for set in sets {
        let prim = match prims.iter().find(|(p, _)| *p == set.prim) {
            Some(&(_, node)) => node,
            None => {
                let node = tree.add(None, set.prim.clone(), "");
                prims.push((&set.prim, node));
                node
            }
        };
        let selected = set.selection.as_deref().unwrap_or("none selected");
        let node = tree.add(Some(prim), set.name.clone(), selected);
        for v in &set.variants {
            tree.add(Some(node), v.clone(), if Some(v.as_str()) == set.selection.as_deref() { "selected" } else { "" });
        }
        // A selection naming a variant the set doesn't have selects nothing.
        if let Some(sel) = &set.selection
            && !set.variants.contains(sel)
        {
            tree.add(Some(node), sel.clone(), "selected, but not in the set");
        }
    }
    tree
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn summaries_are_short() {
        assert_eq!(summary(&Value::Float(1.0)), "1");
        assert_eq!(summary(&Value::Vec3([0.5, -0.0, 2.25])), "(0.5, 0, 2.25)");
        assert_eq!(summary(&Value::TokenArray(vec!["a".into(), "b".into()])), "[a, b]");
        let spec = Spec { spec_type: usd_lite::SpecType::Attribute, fields: vec![("typeName".into(), Value::Token("point3f[]".into())), ("default".into(), Value::Vec3Array(Arc::from(vec![[0.0f32; 3]; 8])))] };
        assert_eq!(property_summary(&spec), "point3f[8]");
    }
}
