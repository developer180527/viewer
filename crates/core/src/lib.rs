//! Viewer core: everything a format loader or a renderer needs to agree on.
//!
//! Loaders turn a [`FileSource`] into a [`Document`] whose [`Content`] is one of a
//! small set of content models. Renderers only ever see content models, so adding
//! a format never means writing a renderer.

mod content;
mod hex;
pub mod limits;
mod loader;
mod pages;
mod source;

pub use content::*;
pub use hex::HexLoader;
pub use loader::{Confidence, FormatLoader, Registry, open_guarded};
pub use pages::*;
pub use source::FileSource;

pub type Result<T> = anyhow::Result<T>;
