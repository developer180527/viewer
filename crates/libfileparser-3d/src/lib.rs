//! Any 3D file into a document (docs/design/libfileparser-3d.md): glTF/GLB/VRM, FBX,
//! OBJ, STL, PLY and USD/USDZ as a CPU scene (`scene-data`), their textures decoded in
//! the background; `.blend` files as their saved preview image.
//!
//! The API is the shape every libfileparser category has: [`readers`], [`detect`],
//! [`open`] and [`open_with`]. Other files (buffers, textures, layers, `.mtl` files) are
//! read only through the context's files hook and recorded in the document's
//! dependencies; texture decoding runs as background jobs through its spawn hook, and
//! each texture that arrives is announced through its notify hook.
//!
//! **Adding a format** is a module in `readers/` and one line in [`readers`]. A reader
//! written outside this crate uses the same pieces through [`build`], and joins these
//! by passing its own list to `libfileparser_core::open`:
//!
//! ```ignore
//! let mut list = libfileparser_3d::readers().to_vec();
//! list.push(&MyFormatReader);
//! libfileparser_core::open(&list, &src, &cx)
//! ```

pub mod build;
mod readers;
mod textures;

use libfileparser_core::{Detection, Document, FileSource, ParseContext, Reader, Result};

pub use readers::{BlendReader, FbxReader, GltfReader, ObjReader, PlyReader, StlReader, UsdReader};

/// Every format this category reads, in the order ties are broken.
pub fn readers() -> &'static [&'static dyn Reader] {
    &[&GltfReader, &FbxReader, &ObjReader, &StlReader, &PlyReader, &UsdReader, &BlendReader]
}

/// The readers that fit a file, best first.
pub fn detect(header: &[u8], name: &str) -> Vec<(&'static dyn Reader, Detection)> {
    libfileparser_core::detect(readers(), header, name)
}

/// Opens `src` with the best reader that succeeds.
pub fn open(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    libfileparser_core::open(readers(), src, cx)
}

/// Opens `src` with the reader whose id is `id`.
pub fn open_with(id: &str, src: &FileSource, cx: &ParseContext) -> Result<Document> {
    libfileparser_core::open_with(readers(), id, src, cx)
}
