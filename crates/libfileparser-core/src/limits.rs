//! What a file may cost. Every size a file claims is checked here before anything is
//! allocated, so a small hostile file can't take gigabytes (PLAN.md §8).

use std::fmt;

use crate::{LimitExceeded, ParseError, Result};

/// One kind of limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Limit {
    /// Pixels in one decoded image.
    ImagePixels,
    /// Bytes in one allocation a file asks for.
    Allocation,
    /// Bytes inflated from compressed data (see [`Limits::unpack_cap`]).
    Unpacked,
    /// Nesting: brackets, records, composition arcs.
    Depth,
    /// Geometry: vertices, indices, triangles, array elements of a scene.
    Geometry,
    /// Nodes of a tree, and a format's records (FBX records, `.blend` blocks).
    Nodes,
    /// Rows of a table.
    Rows,
    /// Entries of an archive or folder.
    Entries,
    /// Values reached through references rather than written out: YAML aliases, XML
    /// entities, instancing. Caps what a small file can expand to.
    Expansion,
    /// Everything one document holds.
    Memory,
    /// Other files one document may read.
    Dependencies,
    /// Pages of a paged document.
    Pages,
}

impl fmt::Display for Limit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Limit::ImagePixels => "image pixels",
            Limit::Allocation => "one allocation",
            Limit::Unpacked => "unpacked size",
            Limit::Depth => "nesting depth",
            Limit::Geometry => "geometry",
            Limit::Nodes => "node count",
            Limit::Rows => "row count",
            Limit::Entries => "entry count",
            Limit::Expansion => "expansion",
            Limit::Memory => "document memory",
            Limit::Dependencies => "files referenced",
            Limit::Pages => "page count",
        })
    }
}

/// The caps for one document. Start from a preset and change what you need:
///
/// ```
/// let mut limits = libfileparser_core::Limits::mobile();
/// limits.depth = 64;
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Limits {
    pub image_pixels: u64,
    pub allocation: u64,
    /// Inflating may reach this many times the packed size…
    pub unpack_ratio: u64,
    /// …or this many bytes, whichever is more.
    pub unpack_always: u64,
    pub depth: u64,
    /// Vertices, indices, triangles: [`Limit::Geometry`].
    pub geometry: u64,
    pub nodes: u64,
    pub rows: u64,
    pub entries: u64,
    pub expansion: u64,
    pub memory: u64,
    pub dependencies: u64,
    pub pages: u64,
}

impl Limits {
    /// A desktop app: 256 megapixel images, 2 GiB per allocation, 4 GiB per document.
    pub fn desktop() -> Self {
        Self {
            image_pixels: 1 << 28,
            allocation: 2 << 30,
            unpack_ratio: 200,
            unpack_always: 64 << 20,
            depth: 512,
            geometry: 1 << 28,
            nodes: 1 << 26,
            rows: 1 << 31,
            entries: 1 << 20,
            expansion: 1 << 20,
            memory: 4 << 30,
            dependencies: 10_000,
            pages: 1 << 20,
        }
    }

    /// A phone or tablet: a quarter of the desktop's memory-bound limits.
    pub fn mobile() -> Self {
        Self {
            image_pixels: 1 << 26,
            allocation: 512 << 20,
            geometry: 1 << 26,
            nodes: 1 << 24,
            rows: 1 << 29,
            entries: 1 << 18,
            expansion: 1 << 18,
            memory: 1 << 30,
            ..Self::desktop()
        }
    }

    /// The most allowed for `limit`. For [`Limit::Unpacked`] the cap depends on the
    /// packed size ([`Limits::unpack_cap`]); this is the amount always allowed.
    pub fn max(&self, limit: Limit) -> u64 {
        match limit {
            Limit::ImagePixels => self.image_pixels,
            Limit::Allocation => self.allocation,
            Limit::Unpacked => self.unpack_always,
            Limit::Depth => self.depth,
            Limit::Geometry => self.geometry,
            Limit::Nodes => self.nodes,
            Limit::Rows => self.rows,
            Limit::Entries => self.entries,
            Limit::Expansion => self.expansion,
            Limit::Memory => self.memory,
            Limit::Dependencies => self.dependencies,
            Limit::Pages => self.pages,
        }
    }

    /// `Ok` when `value` is within `limit`, else [`ParseError::TooLarge`].
    pub fn check(&self, limit: Limit, value: u64) -> Result<()> {
        let max = self.max(limit);
        if value <= max { Ok(()) } else { Err(ParseError::TooLarge(LimitExceeded { limit, value, max })) }
    }

    /// The most `packed` compressed bytes may inflate to: the larger of
    /// [`Limits::unpack_always`] and `unpack_ratio` times `packed`, never past
    /// [`Limits::allocation`].
    pub fn unpack_cap(&self, packed: u64) -> u64 {
        self.unpack_always.max(packed.saturating_mul(self.unpack_ratio)).min(self.allocation)
    }

    /// `Ok` when inflating `packed` bytes to `unpacked` stays within [`Limits::unpack_cap`].
    pub fn check_unpack(&self, packed: u64, unpacked: u64) -> Result<()> {
        let max = self.unpack_cap(packed);
        if unpacked <= max { Ok(()) } else { Err(ParseError::TooLarge(LimitExceeded { limit: Limit::Unpacked, value: unpacked, max })) }
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self::desktop()
    }
}
