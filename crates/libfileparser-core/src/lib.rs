//! The contracts every libfileparser library shares (docs/design/libfileparser.md §3–4,
//! docs/design/document-model.md).
//!
//! - A [`Reader`] reads one format. A category library is a list of readers plus
//!   [`detect`], [`open`] and [`open_with`] over that list.
//! - Opening gives a [`Document`]: a format, ordered facets (the first is primary),
//!   metadata, and the files it refers to ([`Dependencies`]).
//! - A facet's data is a [`Content`]; anything large is a source read by range.
//! - Readers use the hooks in [`ParseContext`] for other files, background work,
//!   progress, cancellation and [`Limits`], and nothing else.
//!
//! This crate has no format code and no heavy dependencies.

mod bytes;
mod content;
mod dependency;
mod document;
mod error;
mod flow;
mod generic;
mod hooks;
mod limits;
mod pages;
mod link;
mod reader;
mod source;
mod value;

pub use bytes::Bytes;
pub use content::{
    Band, BandSource, Bands, Container, Content, Count, Entry, EntryKind, LineText, MemoryUse, Model, NodeId, OwnedTable, OwnedTree, Pixels, Raster, TableInfo,
    TableSource, TextInfo, TextSource, TreeNode, TreeSource,
};
pub use dependency::{Dependencies, Dependency, DependencyStatus, DependencyTable, Unreadable};
pub use document::{Document, FacetInfo, FormatInfo, Role, Warning};
pub use flow::{Block, FlowSource, Inline, OwnedFlow};
pub use error::{LimitExceeded, ParseError, Position, Result};
pub use generic::{FacetProvider, FileParts, is_text};
pub use hooks::{AnyFiles, Cancel, FileResolver, FolderFiles, InlineSpawn, NoFiles, NoNotify, Notify, ParseContext, Spawn};
pub use limits::{Limit, Limits};
pub use pages::{MAX_RENDER_SIDE, PageRect, PageSize, PageSource, render_size};
pub use link::{Links, Place, SceneItem};
pub use reader::{Category, Confidence, Detection, Reader, ReaderInfo, detect, open, open_guarded, open_with};
pub use source::{COPY_BELOW, FileSource};
#[cfg(feature = "scene")]
pub use content::SceneDoc;
pub use value::{Cell, Column, ValueKind};
