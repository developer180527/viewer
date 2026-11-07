//! Links between facets (docs/design/document-model.md, "Linked facets"): the facets
//! of one document describe the same thing, so an item in one can say where it also
//! is in another. A prim in the Stage tree is a node in the Scene and a line of the
//! USDA text; a row of Materials is a material in the Scene. An app selects in one view
//! and highlights in the others.
//!
//! Links are asked for, not stored: [`crate::Document::links`] asks the document's
//! [`Links`] sources, which work them out from indices the reader kept.

use std::ops::Range;

use crate::{Document, NodeId, ParseContext, Result};

/// Something in a document: an item of one of its facets (by facet index), or a part
/// of the file itself.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Place {
    /// A node of a `Tree` facet.
    Node { facet: usize, node: NodeId },
    /// A row of a `Table` facet.
    Row { facet: usize, row: u64 },
    /// An entry of a `Container` facet.
    Entry { facet: usize, entry: u64 },
    /// An item of a `Scene` facet.
    Scene { facet: usize, item: SceneItem },
    /// Lines of the file's own text, from 0, end exclusive: where an app's Source view
    /// of the file (the front door's text facet) shows it.
    Lines(Range<u64>),
    /// Bytes of the file, end exclusive: for its Raw (hex) view.
    Bytes(Range<u64>),
}

/// An item of a scene (`scene-data`), by its index in the scene.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum SceneItem {
    Node(u32),
    Mesh(u32),
    Material(u32),
    Texture(u32),
}

/// Works out where a place also is. A document can have several sources (one for the
/// links every 3D document has, one for its format's own); their answers are joined.
pub trait Links: Send + Sync {
    /// The other places `from` is, in no particular order; empty when it has none.
    /// `doc` is the document these links belong to, to find facets by name and make
    /// the ones it needs.
    fn links(&self, doc: &Document, from: &Place, cx: &ParseContext) -> Result<Vec<Place>>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Category, FormatInfo, ParseError};

    struct Fixed(Vec<Place>);
    impl Links for Fixed {
        fn links(&self, _: &Document, _: &Place, _: &ParseContext) -> Result<Vec<Place>> {
            Ok(self.0.clone())
        }
    }
    struct Broken;
    impl Links for Broken {
        fn links(&self, _: &Document, _: &Place, _: &ParseContext) -> Result<Vec<Place>> {
            Err(ParseError::corrupt("broken"))
        }
    }
    struct Panics;
    impl Links for Panics {
        fn links(&self, _: &Document, _: &Place, _: &ParseContext) -> Result<Vec<Place>> {
            panic!("a bug in a link source")
        }
    }

    #[test]
    fn sources_are_joined_and_one_failing_hides_nothing() {
        let cx = ParseContext::new();
        let doc = || Document::new(FormatInfo::new("t", "test", Category::ThreeD));
        let from = Place::Lines(0..1);
        let a = Place::Bytes(0..4);
        // Joined, without duplicates or the place asked about.
        let d = doc().with_links(Fixed(vec![a.clone(), from.clone()])).with_links(Fixed(vec![a.clone()])).with_links(Broken).with_links(Panics);
        assert_eq!(d.links(&from, &cx).unwrap(), [a]);
        // Only when every source fails is it an error; none at all is no links.
        assert!(doc().with_links(Broken).links(&from, &cx).is_err());
        assert!(doc().links(&from, &cx).unwrap().is_empty());
    }
}
