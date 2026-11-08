//! Composition: a root layer and everything it brings in (sublayers, references,
//! payloads, inherits, variants, specializes) flattened into one [`Layer`], which the
//! [`geom`](crate::geom) helpers read like any other.
//!
//! A practical subset of Pixar's Pcp, matching `usdcat --flatten` on what it covers:
//!
//! - **Layer stacks.** A layer and its sublayers, strongest first.
//! - **Sites.** Each composed prim is backed by a list of (layer stack, path) sites in
//!   strength order: its own, then for each, what its arcs bring in, depth first in
//!   LIVRPS order (local, inherits, variants, references, payloads), with specializes
//!   weakest of all. A field's value comes from the strongest site that has it.
//! - **Namespace mapping.** A site carries the mapping from its namespace to the
//!   composed one, so a referenced asset's `</Wheel/Looks/Rubber>` becomes
//!   `</World/Car/Looks/Rubber>` in relationships and connections.
//! - **Assets anchored.** Relative asset paths (textures) are resolved against the layer
//!   that authored them, so a referenced asset's textures are found next to it.
//!
//! Not modelled: layer offsets (time), relocates, instancing proxies (instances are
//! simply composed, sharing their arrays), list-op deletes across layers, and
//! variant fallbacks.

use std::collections::{HashMap, HashSet};

use crate::layer::{Layer, Spec, SpecType, child_path, property_path};
use crate::{Limits, Reference, Specifier, Value};

/// Finds and reads the layers an asset path names.
pub trait Resolver {
    /// The layer `asset` names, as authored in the layer identified by `anchor`:
    /// (its identifier, its bytes). `None` when it can't be found or isn't allowed.
    fn resolve(&self, asset: &str, anchor: &str) -> Option<(String, Vec<u8>)>;
}

/// An asset path made relative to the layer that authored it: `textures/a.png` in
/// `/models/parts/wheel.usda` is `/models/parts/textures/a.png`. Absolute paths and
/// URLs are returned unchanged.
pub fn anchored(anchor: &str, asset: &str) -> String {
    if asset.is_empty() || asset.starts_with('/') || asset.contains("://") || asset.as_bytes().get(1) == Some(&b':') {
        return asset.to_string();
    }
    let dir = anchor.rsplit_once('/').map_or("", |(d, _)| d);
    let mut parts: Vec<&str> = if dir.is_empty() { Vec::new() } else { dir.split('/').collect() };
    for part in asset.split('/') {
        match part {
            "." | "" => {}
            ".." if parts.last().is_some_and(|p| !p.is_empty() && *p != "..") => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    let joined = parts.join("/");
    if anchor.starts_with('/') && !joined.starts_with('/') { format!("/{joined}") } else { joined }
}

/// A composed stage, flattened.
#[non_exhaustive]
pub struct Stage {
    pub layer: Layer,
    /// Arcs that couldn't be followed (missing files, cycles, limits).
    pub warnings: Vec<String>,
    /// Identifiers of every layer used, root first.
    pub layers: Vec<String>,
    /// Every arc to another layer, in the order composition met them.
    pub arcs: Vec<LayerArc>,
    /// Every variant set on a composed prim, in the order composition met them.
    pub variant_sets: Vec<VariantSet>,
    /// Where each composed prim is defined, by its path.
    pub sources: HashMap<String, PrimSource>,
}

/// The spec that defines a composed prim: the strongest `def` (else the strongest
/// opinion) among its sites.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct PrimSource {
    /// Index into [`Stage::layers`].
    pub layer: usize,
    /// The spec's path in that layer (a referenced asset's `/Wheel`, a variant's
    /// `/Switch{shape=tri}/Tri`).
    pub path: String,
    /// The line it starts on, in a text layer.
    pub line: Option<u32>,
}

/// What kind of arc brings a layer in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ArcKind {
    Sublayer,
    Reference,
    Payload,
}

impl ArcKind {
    pub fn label(self) -> &'static str {
        match self {
            ArcKind::Sublayer => "sublayer",
            ArcKind::Reference => "reference",
            ArcKind::Payload => "payload",
        }
    }
}

/// One arc from a layer to another, as authored: a sublayer, or a reference or payload
/// with an asset path (internal references aren't arcs to another layer).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct LayerArc {
    /// The layer that authors it.
    pub from: String,
    pub kind: ArcKind,
    /// The asset path as authored.
    pub asset: String,
    /// The layer it was read as; `None` when it couldn't be (the warnings say why).
    pub layer: Option<String>,
    /// For references and payloads, the composed prims it brings the layer into (the
    /// first [`MAX_ARC_PRIMS`]); `prim_count` counts them all.
    pub prims: Vec<String>,
    pub prim_count: usize,
}

/// How many prims a [`LayerArc`] lists.
pub const MAX_ARC_PRIMS: usize = 100;

/// A variant set on a composed prim: its variants (across every site that authors the
/// set) and the one selected, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct VariantSet {
    pub prim: String,
    pub name: String,
    pub variants: Vec<String>,
    pub selection: Option<String>,
}

/// Composes `root` (identified by `root_id`, used to anchor relative paths).
pub fn compose(root: Layer, root_id: &str, resolver: &dyn Resolver, limits: &Limits) -> Stage {
    let mut c = Composer {
        resolver,
        limits: *limits,
        layers: vec![(root_id.to_string(), root)],
        by_id: HashMap::new(),
        stacks: Vec::new(),
        stack_of: HashMap::new(),
        warnings: Vec::new(),
        warned: HashSet::new(),
        out: Layer::default(),
        prims: 0,
        work: 0,
        arcs: Vec::new(),
        arc_index: HashMap::new(),
        variant_sets: Vec::new(),
        set_index: HashMap::new(),
        sources: HashMap::new(),
    };
    c.by_id.insert(root_id.to_string(), 0);
    let stack = c.stack(0);
    c.compose_root(stack);
    Stage { layer: c.out, warnings: c.warnings, layers: c.layers.iter().map(|(id, _)| id.clone()).collect(), arcs: c.arcs, variant_sets: c.variant_sets, sources: c.sources }
}

#[derive(Clone)]
struct Site {
    stack: usize,
    /// The site's path in its layer stack (may hold variant selections).
    path: String,
    /// Paths under `src` (in the site's namespace) map to under `dst` (composed).
    src: String,
    dst: String,
}

/// The sites that composed a prim and each of its ancestors, innermost first: an arc
/// to one of them is a cycle (Pcp's "arc cycle"). A prim whose payload names the file's
/// own default prim would otherwise nest the whole file inside itself, level after
/// level, until the prim budget ran out, with paths growing all the way.
struct Ancestry {
    sites: Vec<(usize, String)>,
    up: Option<std::rc::Rc<Ancestry>>,
}

impl Ancestry {
    fn has(this: &Option<std::rc::Rc<Ancestry>>, stack: usize, path: &str) -> bool {
        let mut at = this.as_deref();
        while let Some(a) = at {
            if a.sites.iter().any(|(s, p)| *s == stack && p == path) {
                return true;
            }
            at = a.up.as_deref();
        }
        false
    }
}

/// Fields that describe composition: consumed here, not copied out.
const ARC_FIELDS: &[&str] = &["references", "payload", "inheritPaths", "specializes", "variantSetNames", "variantSelection", "primChildren", "properties", "subLayers", "subLayerOffsets"];

const MAX_LAYERS: usize = 256;
const MAX_DEPTH: u32 = 64;

struct Composer<'r> {
    resolver: &'r dyn Resolver,
    limits: Limits,
    /// (identifier, layer); index 0 is the root.
    layers: Vec<(String, Layer)>,
    by_id: HashMap<String, usize>,
    /// Layer stacks: layer indices, strongest first.
    stacks: Vec<Vec<usize>>,
    stack_of: HashMap<usize, usize>,
    warnings: Vec<String>,
    warned: HashSet<String>,
    out: Layer,
    prims: usize,
    /// Expansion steps taken.
    work: usize,
    arcs: Vec<LayerArc>,
    /// (from layer, kind, asset) → index in `arcs`.
    arc_index: HashMap<(usize, ArcKind, String), usize>,
    variant_sets: Vec<VariantSet>,
    /// (prim, set) → index in `variant_sets`.
    set_index: HashMap<(String, String), usize>,
    sources: HashMap<String, PrimSource>,
}

impl Composer<'_> {
    fn warn(&mut self, msg: String) {
        if self.warned.insert(msg.clone()) {
            self.warnings.push(msg);
        }
    }

    /// The layer `asset` names from the layer `anchor`, loaded once.
    fn load(&mut self, asset: &str, anchor: usize) -> Option<usize> {
        let anchor_id = self.layers[anchor].0.clone();
        let wanted = anchored(&anchor_id, asset);
        if let Some(&i) = self.by_id.get(&wanted) {
            return Some(i);
        }
        if self.layers.len() >= MAX_LAYERS {
            self.warn(format!("more than {MAX_LAYERS} layers; {asset} skipped"));
            return None;
        }
        let Some((id, bytes)) = self.resolver.resolve(asset, &anchor_id) else {
            self.warn(format!("{asset} (from {anchor_id}) not found"));
            return None;
        };
        if let Some(&i) = self.by_id.get(&id) {
            self.by_id.insert(wanted, i);
            return Some(i);
        }
        match crate::read_layer(&bytes, &self.limits) {
            Ok(layer) => {
                let i = self.layers.len();
                self.layers.push((id.clone(), layer));
                self.by_id.insert(id, i);
                self.by_id.insert(wanted, i);
                Some(i)
            }
            Err(e) => {
                self.warn(format!("{asset}: {e}"));
                None
            }
        }
    }

    /// Loads `asset` from `anchor` and records the arc; `prim` is the composed prim a
    /// reference or payload brings it into.
    fn load_arc(&mut self, asset: &str, anchor: usize, kind: ArcKind, prim: Option<&str>) -> Option<usize> {
        let loaded = self.load(asset, anchor);
        let key = (anchor, kind, asset.to_string());
        let i = match self.arc_index.get(&key) {
            Some(&i) => i,
            None => {
                let i = self.arcs.len();
                let layer = loaded.map(|l| self.layers[l].0.clone());
                self.arcs.push(LayerArc { from: self.layers[anchor].0.clone(), kind, asset: asset.to_string(), layer, prims: Vec::new(), prim_count: 0 });
                self.arc_index.insert(key, i);
                i
            }
        };
        if let Some(prim) = prim {
            let arc = &mut self.arcs[i];
            if !arc.prims.iter().any(|p| p == prim) {
                arc.prim_count += 1;
                if arc.prims.len() < MAX_ARC_PRIMS {
                    arc.prims.push(prim.to_string());
                }
            }
        }
        loaded
    }

    /// Records the variant set `set` on the composed prim `prim`, with the variants the
    /// site at `path` in `stack` authors for it.
    fn record_variant_set(&mut self, prim: &str, set: &str, stack: usize, path: &str, selection: Option<&String>) {
        let mut variants: Vec<String> = Vec::new();
        for (_, spec) in self.specs(stack, &format!("{path}{{{set}=}}")) {
            for v in spec.field("variantChildren").and_then(Value::as_tokens).unwrap_or_default() {
                if !variants.contains(v) {
                    variants.push(v.clone());
                }
            }
        }
        let key = (prim.to_string(), set.to_string());
        match self.set_index.get(&key) {
            Some(&i) => {
                let entry = &mut self.variant_sets[i];
                for v in variants {
                    if !entry.variants.contains(&v) {
                        entry.variants.push(v);
                    }
                }
                if entry.selection.is_none() {
                    entry.selection = selection.cloned();
                }
            }
            None => {
                self.set_index.insert(key, self.variant_sets.len());
                self.variant_sets.push(VariantSet { prim: prim.to_string(), name: set.to_string(), variants, selection: selection.cloned() });
            }
        }
    }

    /// The layer stack rooted at `layer`: it and its sublayers, strongest first.
    fn stack(&mut self, layer: usize) -> usize {
        if let Some(&s) = self.stack_of.get(&layer) {
            return s;
        }
        let mut order = Vec::new();
        let mut visiting = HashSet::new();
        self.collect_sublayers(layer, &mut order, &mut visiting, 0);
        let s = self.stacks.len();
        self.stacks.push(order);
        self.stack_of.insert(layer, s);
        s
    }

    fn collect_sublayers(&mut self, layer: usize, order: &mut Vec<usize>, visiting: &mut HashSet<usize>, depth: u32) {
        if depth > MAX_DEPTH || !visiting.insert(layer) || order.contains(&layer) {
            return;
        }
        order.push(layer);
        let subs: Vec<String> = match self.layers[layer].1.metadata("subLayers") {
            Some(Value::AssetArray(a) | Value::StringArray(a)) => a.clone(),
            _ => Vec::new(),
        };
        for sub in subs {
            if let Some(i) = self.load_arc(&sub, layer, ArcKind::Sublayer, None) {
                self.collect_sublayers(i, order, visiting, depth + 1);
            }
        }
        visiting.remove(&layer);
    }

    /// Specs at `path` in a stack, strongest first, with their layer.
    fn specs(&self, stack: usize, path: &str) -> Vec<(usize, &Spec)> {
        self.stacks[stack].iter().filter_map(|&l| self.layers[l].1.spec(path).map(|s| (l, s))).collect()
    }

    fn map(site: &Site, path: &str) -> String {
        let path = strip_variants(path);
        match path.strip_prefix(site.src.as_str()) {
            Some(rest) if site.src == "/" => format!("{}{}{rest}", site.dst.trim_end_matches('/'), if rest.starts_with(['/', '.']) || rest.is_empty() { "" } else { "/" }),
            Some(rest) if rest.is_empty() || rest.starts_with(['/', '.', '[']) => format!("{}{rest}", site.dst),
            _ => path,
        }
    }

    /// All sites backing the composed prim `dst`, strongest first, starting from the
    /// sites its parent maps down (`base`).
    fn prim_sites(&mut self, base: Vec<Site>, dst: &str) -> Vec<Site> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut selections: HashMap<String, String> = HashMap::new();
        let mut specializes = Vec::new();
        for site in base {
            self.expand(site, dst, &mut out, &mut seen, &mut selections, &mut specializes, 0);
        }
        for site in specializes {
            self.expand(site, dst, &mut out, &mut seen, &mut selections, &mut Vec::new(), 0);
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn expand(
        &mut self,
        site: Site,
        dst: &str,
        out: &mut Vec<Site>,
        seen: &mut HashSet<(usize, String)>,
        selections: &mut HashMap<String, String>,
        specializes: &mut Vec<Site>,
        depth: u32,
    ) {
        if depth > MAX_DEPTH {
            self.warn(format!("{dst}: composition nested too deeply"));
            return;
        }
        // All the work composition may do, so arcs that keep bringing in more stop
        // in time as well as in size.
        self.work += 1;
        if self.work > self.limits.max_prims.saturating_mul(4) {
            self.warn(format!("composition stopped after {} steps", self.work - 1));
            return;
        }
        if !seen.insert((site.stack, site.path.clone())) {
            return;
        }
        let specs = self.specs(site.stack, &site.path);
        if specs.is_empty() {
            return;
        }
        // Gather the arcs this site authors, across its layers (strongest first).
        let mut inherits = Vec::new();
        let mut set_names = Vec::new();
        let mut refs: Vec<(usize, Reference)> = Vec::new();
        let mut payloads: Vec<(usize, Reference)> = Vec::new();
        let mut special = Vec::new();
        for (layer, spec) in &specs {
            if let Some(Value::Dictionary(d)) = spec.field("variantSelection") {
                for (set, sel) in d {
                    if let Some(sel) = sel.as_str() {
                        selections.entry(set.clone()).or_insert_with(|| sel.to_string());
                    }
                }
            }
            if let Some(p) = spec.field("inheritPaths").and_then(Value::as_paths) {
                inherits.extend(p.iter().cloned());
            }
            if let Some(n) = spec.field("variantSetNames").and_then(Value::as_tokens) {
                set_names.extend(n.iter().cloned());
            }
            if let Some(Value::References(r)) = spec.field("references") {
                refs.extend(r.iter().map(|r| (*layer, r.clone())));
            }
            if let Some(Value::References(r)) = spec.field("payload") {
                payloads.extend(r.iter().map(|r| (*layer, r.clone())));
            }
            if let Some(p) = spec.field("specializes").and_then(Value::as_paths) {
                special.extend(p.iter().cloned());
            }
        }
        drop(specs);
        out.push(site.clone());

        for path in inherits {
            let s = Site { stack: site.stack, path: path.clone(), src: path, dst: dst.to_string() };
            self.expand(s, dst, out, seen, selections, specializes, depth + 1);
        }
        set_names.dedup();
        for set in set_names {
            self.record_variant_set(dst, &set, site.stack, &site.path, selections.get(&set));
            let Some(sel) = selections.get(&set).cloned() else { continue };
            let s = Site { path: format!("{}{{{set}={sel}}}", site.path), ..site.clone() };
            self.expand(s, dst, out, seen, selections, specializes, depth + 1);
        }
        let refs = refs.into_iter().map(|r| (ArcKind::Reference, r)).chain(payloads.into_iter().map(|r| (ArcKind::Payload, r)));
        for (kind, (layer, r)) in refs {
            let stack = if r.asset.is_empty() {
                site.stack
            } else {
                match self.load_arc(&r.asset, layer, kind, Some(dst)) {
                    Some(l) => self.stack(l),
                    None => continue,
                }
            };
            let target = match r.prim {
                Some(p) => p,
                None => {
                    let root = self.stacks[stack][0];
                    match self.layers[root].1.metadata("defaultPrim").and_then(Value::as_str) {
                        Some(name) => child_path("/", name.trim_start_matches('/')),
                        None => {
                            self.warn(format!("{dst}: {} has no defaultPrim to reference", self.layers[root].0));
                            continue;
                        }
                    }
                }
            };
            let s = Site { stack, path: target.clone(), src: target, dst: dst.to_string() };
            self.expand(s, dst, out, seen, selections, specializes, depth + 1);
        }
        for path in special {
            specializes.push(Site { stack: site.stack, path: path.clone(), src: path, dst: dst.to_string() });
        }
    }

    fn compose_root(&mut self, stack: usize) {
        let root = Site { stack, path: "/".into(), src: "/".into(), dst: "/".into() };
        // Layer metadata: strongest opinion per field across the root stack.
        let mut fields: Vec<(String, Value)> = Vec::new();
        for (_, spec) in self.specs(stack, "/") {
            for (name, value) in &spec.fields {
                if !ARC_FIELDS.contains(&name.as_str()) && !fields.iter().any(|(n, _)| n == name) {
                    fields.push((name.clone(), value.clone()));
                }
            }
        }
        let children = self.child_names(std::slice::from_ref(&root));
        fields.push(("primChildren".into(), Value::TokenArray(children.clone())));
        self.out.specs.insert("/".into(), Spec { spec_type: SpecType::PseudoRoot, fields });
        // Depth-first over the composed tree, without recursion.
        let mut work: Vec<(String, Vec<Site>, Option<std::rc::Rc<Ancestry>>)> =
            children.into_iter().rev().map(|n| (child_path("/", &n), vec![child_site(&root, &n)], None)).collect();
        while let Some((path, base, ancestry)) = work.pop() {
            self.prims += 1;
            if self.prims > self.limits.max_prims {
                self.warn(format!("more than {} prims; the rest are skipped", self.limits.max_prims));
                return;
            }
            let mut sites = self.prim_sites(base, &path);
            let before = sites.len();
            sites.retain(|s| !Ancestry::has(&ancestry, s.stack, &s.path));
            if sites.len() < before {
                self.warn(format!("{path}: an arc brings in one of its own ancestors (a cycle); not followed"));
            }
            let Some(children) = self.emit_prim(&path, &sites) else { continue };
            let ancestry = Some(std::rc::Rc::new(Ancestry { sites: sites.iter().map(|s| (s.stack, s.path.clone())).collect(), up: ancestry }));
            for name in children.into_iter().rev() {
                let base = sites.iter().map(|s| child_site(s, &name)).collect();
                work.push((child_path(&path, &name), base, ancestry.clone()));
            }
        }
    }

    /// Child names across `sites`: weakest first, each name where it first appears
    /// (as Pcp orders them).
    fn child_names(&self, sites: &[Site]) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for site in sites.iter().rev() {
            for (_, spec) in self.specs(site.stack, &site.path).into_iter().rev() {
                if let Some(c) = spec.field("primChildren").and_then(Value::as_tokens) {
                    for n in c {
                        if !names.contains(n) {
                            names.push(n.clone());
                        }
                    }
                }
            }
        }
        names
    }

    /// Writes the composed prim and its properties; returns its children, or `None`
    /// when no site defines it (only `over`s).
    fn emit_prim(&mut self, path: &str, sites: &[Site]) -> Option<Vec<String>> {
        let mut specifier = None;
        let mut fields: Vec<(String, Value)> = Vec::new();
        let mut properties: Vec<String> = Vec::new();
        // (layer, path): the strongest def, else the strongest opinion.
        let (mut def_at, mut first_at) = (None, None);
        for site in sites {
            for (layer, spec) in self.specs(site.stack, &site.path) {
                first_at.get_or_insert((layer, site.path.clone()));
                if def_at.is_none() && matches!(spec.field("specifier"), Some(Value::Specifier(Specifier::Def))) {
                    def_at = Some((layer, site.path.clone()));
                }
                match spec.field("specifier") {
                    Some(Value::Specifier(Specifier::Def)) => specifier = Some(Specifier::Def),
                    Some(Value::Specifier(Specifier::Class)) if specifier.is_none() => specifier = Some(Specifier::Class),
                    _ => {}
                }
                for (name, value) in &spec.fields {
                    if !ARC_FIELDS.contains(&name.as_str()) && name != "specifier" && !fields.iter().any(|(n, _)| n == name) {
                        fields.push((name.clone(), value.clone()));
                    }
                }
                if let Some(p) = spec.field("properties").and_then(Value::as_tokens) {
                    for n in p {
                        if !properties.contains(n) {
                            properties.push(n.clone());
                        }
                    }
                }
            }
        }
        let specifier = specifier?;
        if let Some((layer, at)) = def_at.or(first_at) {
            let line = self.layers[layer].1.lines.get(&at).copied();
            self.sources.insert(path.to_string(), PrimSource { layer, path: at, line });
        }
        let children = self.child_names(sites);
        fields.push(("specifier".into(), Value::Specifier(specifier)));
        fields.push(("primChildren".into(), Value::TokenArray(children.clone())));
        fields.push(("properties".into(), Value::TokenArray(properties.clone())));
        self.out.specs.insert(path.to_string(), Spec { spec_type: SpecType::Prim, fields });

        for name in properties {
            let mut spec_type = None;
            let mut fields: Vec<(String, Value)> = Vec::new();
            for site in sites {
                let prop = property_path(&site.path, &name);
                for (layer, spec) in self.specs(site.stack, &prop) {
                    spec_type.get_or_insert(spec.spec_type);
                    for (field, value) in &spec.fields {
                        if fields.iter().any(|(n, _)| n == field) {
                            continue;
                        }
                        let value = match value {
                            // Targets and connections into the composed namespace.
                            Value::Paths(p) => Value::Paths(p.iter().map(|p| Self::map(site, p)).collect()),
                            v => anchor_assets(v, &self.layers[layer].0),
                        };
                        fields.push((field.clone(), value));
                    }
                }
            }
            if let Some(spec_type) = spec_type {
                self.out.specs.insert(property_path(path, &name), Spec { spec_type, fields });
            }
        }
        Some(children)
    }
}

fn child_site(site: &Site, name: &str) -> Site {
    Site { path: child_path(&site.path, name), ..site.clone() }
}

/// `/A{v=x}/B{w=y}.c` → `/A/B.c`
fn strip_variants(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut depth = 0u32;
    for ch in path.chars() {
        match ch {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            c if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

fn anchor_assets(value: &Value, layer_id: &str) -> Value {
    match value {
        Value::Asset(a) => Value::Asset(anchored(layer_id, a)),
        Value::AssetArray(a) => Value::AssetArray(a.iter().map(|a| anchored(layer_id, a)).collect()),
        Value::TimeSamples(s) => Value::TimeSamples(s.iter().map(|(t, v)| (*t, anchor_assets(v, layer_id))).collect()),
        v => v.clone(),
    }
}

/// Reads layers from the filesystem: local paths only (no URLs), USD files only, up
/// to a size cap.
#[non_exhaustive]
pub struct FsResolver {
    pub max_bytes: u64,
}

impl Default for FsResolver {
    fn default() -> Self {
        Self { max_bytes: 1 << 30 }
    }
}

impl Resolver for FsResolver {
    fn resolve(&self, asset: &str, anchor: &str) -> Option<(String, Vec<u8>)> {
        if asset.contains("://") || asset.contains('<') {
            return None;
        }
        let path = std::path::PathBuf::from(anchored(anchor, asset));
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        if !matches!(ext.as_str(), "usd" | "usda" | "usdc") {
            return None;
        }
        let meta = std::fs::metadata(&path).ok()?;
        if !meta.is_file() || meta.len() > self.max_bytes {
            return None;
        }
        let id = path.canonicalize().ok()?.to_string_lossy().into_owned();
        Some((id, std::fs::read(&path).ok()?))
    }
}

#[cfg(feature = "usdz")]
impl Resolver for crate::Package<'_> {
    /// Layers inside the package, by their path relative to the referencing layer.
    fn resolve(&self, asset: &str, anchor: &str) -> Option<(String, Vec<u8>)> {
        let id = anchored(anchor, asset);
        self.file(&id).map(|b| (id, b.to_vec()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchoring() {
        assert_eq!(anchored("/m/parts/wheel.usda", "textures/a.png"), "/m/parts/textures/a.png");
        assert_eq!(anchored("/m/parts/wheel.usda", "./a.png"), "/m/parts/a.png");
        assert_eq!(anchored("/m/parts/wheel.usda", "../shared/a.usd"), "/m/shared/a.usd");
        assert_eq!(anchored("/m/root.usda", "/abs/x.png"), "/abs/x.png");
        assert_eq!(anchored("root.usdc", "parts/x.usdc"), "parts/x.usdc");
        assert_eq!(anchored("a/b.usda", "../../x"), "../x");
        assert_eq!(anchored("/m/r.usda", "https://x/y.png"), "https://x/y.png");
    }

    #[test]
    fn variant_selections_are_stripped_for_mapping() {
        assert_eq!(strip_variants("/A{v=x}/B{w=y}.c"), "/A/B.c");
        let site = Site { stack: 0, path: "/Wheel".into(), src: "/Wheel".into(), dst: "/World/Car".into() };
        assert_eq!(Composer::map(&site, "/Wheel{size=big}/Looks/Rubber"), "/World/Car/Looks/Rubber");
        assert_eq!(Composer::map(&site, "/Wheel.outputs:x"), "/World/Car.outputs:x");
        assert_eq!(Composer::map(&site, "/WheelHub"), "/WheelHub", "a different prim with a common prefix");
        assert_eq!(Composer::map(&site, "/Elsewhere"), "/Elsewhere");
    }
}
