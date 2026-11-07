//! Reads OpenUSD layers into plain data, in pure Rust.
//!
//! - **USDC** (binary "crate" files, versions 0.4–0.10), **USDA** (text) and **USDZ**
//!   (packages) all produce the same [`Layer`]: specs by path with their fields, the
//!   way Pixar's Sdf models a layer.
//! - [`geom`] reads what viewers need from a layer: prim hierarchy and transforms,
//!   meshes with primvars and material subsets, and `UsdPreviewSurface` materials
//!   with their texture inputs.
//! - [`compose()`] follows sublayers, references, payloads, inherits, variants and
//!   specializes into one flattened [`Layer`], matching `usdcat --flatten` on what it
//!   covers. A single [`read_layer`] is uncomposed; [`Layer::composition_arcs`] says
//!   whether it uses any arcs.
//! - **Safe on hostile files.** Every count is checked against the bytes present,
//!   decompression and element counts are capped by [`Limits`], nesting is bounded,
//!   and nothing panics on bad input.

pub mod compose;
mod crate_file;
pub mod geom;
mod layer;
mod text;
#[cfg(feature = "usdz")]
mod usdz;
mod value;

pub use compose::{ArcKind, FsResolver, LayerArc, PrimSource, Resolver, Stage, VariantSet, anchored, compose};
pub use layer::{Layer, Spec, SpecType, child_path, property_path, split_property};
#[cfg(feature = "usdz")]
pub use usdz::Package;
pub use value::{Reference, Specifier, Value};

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("not a valid USD file: {0}")]
    Format(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("over a limit: {0}")]
    Limit(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Caps on what a file can make the reader allocate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Limits {
    /// Elements in any one array or table (points, indices, tokens…).
    pub max_elements: usize,
    /// Bytes any one compressed block may expand to.
    pub max_decompressed: usize,
}

impl Limits {
    /// Elements in any one array or table.
    pub fn with_max_elements(mut self, n: usize) -> Self {
        self.max_elements = n;
        self
    }

    /// Bytes any one compressed block may expand to.
    pub fn with_max_decompressed(mut self, n: usize) -> Self {
        self.max_decompressed = n;
        self
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self { max_elements: 1 << 27, max_decompressed: 1 << 30 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Format {
    Usda,
    Usdc,
    Usdz,
}

/// Which USD format `bytes` are, from their first bytes.
pub fn detect(bytes: &[u8]) -> Option<Format> {
    if bytes.starts_with(b"PXR-USDC") {
        Some(Format::Usdc)
    } else if bytes.starts_with(b"#usda") {
        Some(Format::Usda)
    } else if bytes.starts_with(b"PK\x03\x04") {
        Some(Format::Usdz)
    } else {
        None
    }
}

/// Reads a USDA or USDC layer. (A USDZ package is opened with [`Package::open`].)
pub fn read_layer(bytes: &[u8], limits: &Limits) -> Result<Layer> {
    match detect(bytes) {
        Some(Format::Usdc) => crate_file::read(bytes, limits),
        Some(Format::Usda) => text::read(bytes, limits),
        Some(Format::Usdz) => Err(Error::Format("a USDZ package, not a layer: use Package::open".into())),
        None => Err(Error::Format("not a USD file".into())),
    }
}

impl Layer {
    /// Composition arcs the layer authors (and this crate doesn't follow): references,
    /// payloads, sublayers, inherits, specializes, variant sets.
    pub fn composition_arcs(&self) -> Vec<&'static str> {
        let mut found = Vec::new();
        let names: [(&str, &'static str); 6] = [
            ("references", "references"),
            ("payload", "payloads"),
            ("subLayers", "sublayers"),
            ("inheritPaths", "inherits"),
            ("specializes", "specializes"),
            ("variantSetNames", "variant sets"),
        ];
        for (field, label) in names {
            if self.specs.values().any(|s| s.field(field).is_some()) {
                found.push(label);
            }
        }
        if self.specs.keys().any(|p| p.contains('{')) && !found.contains(&"variant sets") {
            found.push("variant sets");
        }
        found
    }
}
