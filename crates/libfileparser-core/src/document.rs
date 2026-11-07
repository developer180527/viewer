//! Documents and their facets (docs/design/document-model.md).

use std::sync::OnceLock;

use crate::{Category, Content, Dependencies, MemoryUse, Model, ParseContext, Result};

/// What a facet is for, the same for every format, so an app finds "the hierarchy" or
/// "the source" the same way everywhere (docs/design/document-model.md §3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Role {
    /// What the format is for: a scene, pages, an image, a table.
    Primary,
    /// How the format organises itself: USD prims, glTF nodes, FBX records.
    Structure,
    /// The text the file is written in.
    Source,
    /// The files packed inside.
    Package,
    /// Files outside it that it uses.
    Dependencies,
    /// Format-specific lists: materials, textures, variant sets.
    Inspector,
    /// The bytes.
    Raw,
}

/// A facet, described before it is made.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct FacetInfo {
    pub role: Role,
    /// The format's own word: "Scene", "Stage", "Layers", "Accessors".
    pub name: String,
    pub model: Model,
}

impl FacetInfo {
    pub fn new(role: Role, name: impl Into<String>, model: Model) -> Self {
        Self { role, name: name.into(), model }
    }
}

/// Which format a document was read as.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct FormatInfo {
    /// The reader's id ("gltf", "usd").
    pub reader: &'static str,
    /// For people: "glTF binary (GLB)", "USD (text)".
    pub name: String,
    pub category: Category,
}

impl FormatInfo {
    pub fn new(reader: &'static str, name: impl Into<String>, category: Category) -> Self {
        Self { reader, name: name.into(), category }
    }
}

type Make = Box<dyn Fn(&ParseContext) -> Result<Content> + Send + Sync>;

/// How a facet not yet made is made.
enum Maker {
    Fn(Make),
    Provider(std::sync::Arc<dyn crate::FacetProvider>),
}

struct Facet {
    info: FacetInfo,
    made: OnceLock<Content>,
    make: Option<Maker>,
}

/// What one reading of a file produced: its format, its facets (the first is the
/// primary one, shown by default), metadata, and the files it refers to.
pub struct Document {
    format: FormatInfo,
    facets: Vec<Facet>,
    metadata: Vec<(String, String)>,
    dependencies: Dependencies,
    links: Vec<std::sync::Arc<dyn crate::Links>>,
}

impl Document {
    pub fn new(format: FormatInfo) -> Self {
        Self { format, facets: Vec::new(), metadata: Vec::new(), dependencies: Dependencies::new(), links: Vec::new() }
    }

    /// Adds a facet whose content is already made.
    pub fn with_facet(mut self, role: Role, name: impl Into<String>, content: Content) -> Self {
        let info = FacetInfo::new(role, name, content.model());
        let made = OnceLock::new();
        let _ = made.set(content);
        self.facets.push(Facet { info, made, make: None });
        self
    }

    /// Adds a facet made the first time it is asked for. `make` is called again if it
    /// failed, so a facet that fails doesn't stay broken (or take the document with it).
    pub fn with_lazy_facet(mut self, info: FacetInfo, make: impl Fn(&ParseContext) -> Result<Content> + Send + Sync + 'static) -> Self {
        self.facets.push(Facet { info, made: OnceLock::new(), make: Some(Maker::Fn(Box::new(make))) });
        self
    }

    /// Adds the facets of the context's facet providers that name this document's
    /// reader (`-core`'s `open` does this for every document it opens), before the
    /// file's own Source and Raw, which stay last. Facets a provider already added
    /// aren't added twice.
    pub fn with_provided_facets(mut self, cx: &ParseContext) -> Self {
        let tail = self.facets.iter().rev().take_while(|f| matches!(f.info.role, Role::Source | Role::Raw)).count();
        let mut at = self.facets.len() - tail;
        for provider in &cx.facet_providers {
            let formats = provider.formats();
            if !formats.is_empty() && !formats.contains(&self.format.reader) {
                continue;
            }
            let infos = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| provider.facets(&self))).unwrap_or_default();
            for info in infos {
                if self.facets.iter().any(|f| f.info == info) {
                    continue;
                }
                self.facets.insert(at, Facet { info, made: OnceLock::new(), make: Some(Maker::Provider(provider.clone())) });
                at += 1;
            }
        }
        self
    }

    pub fn with_meta(mut self, key: impl Into<String>, value: impl ToString) -> Self {
        self.metadata.push((key.into(), value.to_string()));
        self
    }

    /// Uses `dependencies` as this document's list: the one the reader resolved its
    /// files through, which its background work may still add to.
    pub fn with_dependencies(mut self, dependencies: Dependencies) -> Self {
        self.dependencies = dependencies;
        self
    }

    /// Adds a Dependencies facet: [`Self::dependencies`] as a table that stays current.
    /// Call it after [`Self::with_dependencies`].
    pub fn with_dependencies_facet(self) -> Self {
        let table = std::sync::Arc::new(self.dependencies.table());
        self.with_facet(Role::Dependencies, "Dependencies", Content::Table(table))
    }

    /// Adds a source of links between this document's facets ([`Self::links`]).
    pub fn with_links(mut self, links: impl crate::Links + 'static) -> Self {
        self.links.push(std::sync::Arc::new(links));
        self
    }

    /// Where `from` also is in this document's facets, or in the file. Each source's
    /// answers are joined; a source that fails is left out, so one broken link doesn't
    /// hide the others, and the error is returned only when every source failed.
    pub fn links(&self, from: &crate::Place, cx: &ParseContext) -> Result<Vec<crate::Place>> {
        let mut out = Vec::new();
        let mut error = None;
        let mut ok = false;
        for source in &self.links {
            match crate::reader::guard("links", || source.links(self, from, cx)) {
                Ok(places) => {
                    ok = true;
                    for p in places {
                        if p != *from && !out.contains(&p) {
                            out.push(p);
                        }
                    }
                }
                Err(e) => {
                    error.get_or_insert(e);
                }
            }
        }
        match error {
            Some(e) if !ok => Err(e),
            _ => Ok(out),
        }
    }

    /// The index of the facet called `name`.
    pub fn facet_index(&self, name: &str) -> Option<usize> {
        self.facets.iter().position(|f| f.info.name == name)
    }

    pub fn format(&self) -> &FormatInfo {
        &self.format
    }

    /// Every facet, made or not, in order: the first is the primary one.
    pub fn facets(&self) -> impl ExactSizeIterator<Item = &FacetInfo> {
        self.facets.iter().map(|f| &f.info)
    }

    /// The content of facet `index`, made now if it hasn't been.
    pub fn facet(&self, index: usize, cx: &ParseContext) -> Result<Content> {
        let facet = self.facets.get(index).ok_or_else(|| crate::ParseError::invalid(format!("no facet {index}")))?;
        if let Some(content) = facet.made.get() {
            return Ok(content.clone());
        }
        let make = facet.make.as_ref().expect("a facet without content has a maker");
        cx.cancel.check()?;
        // A panic while making a facet fails that facet alone (docs/design/document-model.md §8).
        let content = crate::reader::guard(&facet.info.name, || match make {
            Maker::Fn(make) => make(cx),
            Maker::Provider(provider) => provider.make(self, &facet.info, cx),
        })?;
        Ok(facet.made.get_or_init(|| content).clone())
    }

    /// Whether facet `index` has been made.
    pub fn is_made(&self, index: usize) -> bool {
        self.facets.get(index).is_some_and(|f| f.made.get().is_some())
    }

    pub fn metadata(&self) -> &[(String, String)] {
        &self.metadata
    }

    pub fn dependencies(&self) -> &Dependencies {
        &self.dependencies
    }

    /// What the facets made so far hold. Facets that share data count it once each,
    /// so this is an upper bound.
    pub fn memory(&self) -> MemoryUse {
        self.facets.iter().filter_map(|f| f.made.get()).map(Content::memory).fold(MemoryUse::default(), |a, b| a + b)
    }
}
