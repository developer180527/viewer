use crate::{Document, FileSource, HexLoader, Result};

/// How sure a loader is that it can open a file. Higher wins.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Confidence(pub u8);

impl Confidence {
    pub const NONE: Self = Self(0);
    /// Can always show the bytes; the last resort.
    pub const FALLBACK: Self = Self(1);
    /// Content looks plausible (e.g. valid UTF-8 text).
    pub const SNIFF: Self = Self(10);
    /// Content looks like this format but has no hard signature.
    pub const LIKELY: Self = Self(30);
    pub const EXTENSION: Self = Self(50);
    pub const MAGIC: Self = Self(100);
}

pub trait FormatLoader: Sync {
    /// Stable identifier, e.g. "image".
    fn id(&self) -> &'static str;
    /// Name shown in the "View as" menu.
    fn name(&self) -> &'static str;
    /// Must be cheap: no allocation, no parsing beyond the header.
    fn probe(&self, header: &[u8], ext: Option<&str>, name: &str) -> Confidence;
    fn open(&self, src: &FileSource) -> Result<Document>;
}

/// All known loaders. Built once from static tables; nothing runs at startup.
pub struct Registry {
    loaders: Vec<&'static dyn FormatLoader>,
}

/// What happened when a loader failed before the winning one succeeded.
pub type Attempts = Vec<(&'static str, String)>;

impl Registry {
    pub fn new(tables: &[&[&'static dyn FormatLoader]]) -> Self {
        let mut loaders: Vec<_> = tables.iter().flat_map(|t| t.iter().copied()).collect();
        loaders.push(&HexLoader);
        Self { loaders }
    }

    pub fn loaders(&self) -> &[&'static dyn FormatLoader] {
        &self.loaders
    }

    pub fn get(&self, id: &str) -> Option<&'static dyn FormatLoader> {
        self.loaders.iter().copied().find(|l| l.id() == id)
    }

    /// Loaders that claim the file, best first.
    pub fn candidates(&self, src: &FileSource) -> Vec<(Confidence, &'static dyn FormatLoader)> {
        let ext = src.ext();
        let header = src.header();
        let mut found: Vec<_> = self
            .loaders
            .iter()
            .map(|l| (l.probe(header, ext.as_deref(), src.name()), *l))
            .filter(|(c, _)| *c > Confidence::NONE)
            .collect();
        // Stable sort keeps registration order among equals.
        found.sort_by_key(|c| std::cmp::Reverse(c.0));
        found
    }

    /// Opens with the best loader, falling through to the next on failure.
    /// Never fails: the hex loader always succeeds.
    pub fn open(&self, src: &FileSource) -> (Document, &'static str, Attempts) {
        let mut attempts = Vec::new();
        for (_, loader) in self.candidates(src) {
            match open_guarded(loader, src) {
                Ok(doc) => return (doc, loader.id(), attempts),
                Err(e) => attempts.push((loader.id(), format!("{e:#}"))),
            }
        }
        unreachable!("hex loader accepts every file")
    }
}

/// Runs `loader.open`, turning a panic in a decoder into an ordinary error so a
/// malformed file falls through to the next loader instead of crashing the app.
pub fn open_guarded(loader: &dyn FormatLoader, src: &FileSource) -> Result<Document> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| loader.open(src))).unwrap_or_else(|payload| {
        let msg = payload
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".into());
        Err(anyhow::anyhow!("loader panicked: {msg}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Content;

    struct Picky;
    impl FormatLoader for Picky {
        fn id(&self) -> &'static str {
            "picky"
        }
        fn name(&self) -> &'static str {
            "Picky"
        }
        fn probe(&self, _: &[u8], ext: Option<&str>, _: &str) -> Confidence {
            if ext == Some("pk") { Confidence::EXTENSION } else { Confidence::NONE }
        }
        fn open(&self, _: &FileSource) -> Result<Document> {
            anyhow::bail!("always fails")
        }
    }

    struct Panicky;
    impl FormatLoader for Panicky {
        fn id(&self) -> &'static str {
            "panicky"
        }
        fn name(&self) -> &'static str {
            "Panicky"
        }
        fn probe(&self, _: &[u8], _: Option<&str>, _: &str) -> Confidence {
            Confidence::MAGIC
        }
        fn open(&self, _: &FileSource) -> Result<Document> {
            panic!("decoder bug")
        }
    }

    #[test]
    fn falls_back_to_hex_when_best_loader_panics() {
        let reg = Registry::new(&[&[&Panicky]]);
        let (_, id, attempts) = reg.open(&FileSource::from_bytes("x", vec![1]));
        assert_eq!(id, "hex");
        assert!(attempts[0].1.contains("decoder bug"));
    }

    #[test]
    fn falls_back_to_hex_when_best_loader_fails() {
        let reg = Registry::new(&[&[&Picky]]);
        let src = FileSource::from_bytes("x.pk", vec![1, 2, 3]);
        let (doc, id, attempts) = reg.open(&src);
        assert_eq!(id, "hex");
        assert!(matches!(doc.content, Content::Model(libfileparser_core::Content::Hex(_))));
        assert_eq!(attempts[0].0, "picky");
    }
}
