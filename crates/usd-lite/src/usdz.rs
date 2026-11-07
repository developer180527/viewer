//! USDZ: a zip archive whose entries are stored uncompressed (and 64-byte aligned),
//! the first being the root layer. Entries are read in place, never inflated.

use std::io::Cursor;

use crate::{Error, Layer, Limits, Result};

/// An opened USDZ package: its root layer and the bytes of its other files
/// (textures, sublayers).
pub struct Package<'a> {
    bytes: &'a [u8],
    /// (name, byte range in `bytes`) of every stored file.
    files: Vec<(String, std::ops::Range<usize>)>,
    pub layer: Layer,
    /// The root layer's name inside the package.
    pub root: String,
}

impl<'a> Package<'a> {
    pub fn open(bytes: &'a [u8], limits: &Limits) -> Result<Self> {
        let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| Error::Format(format!("zip: {e}")))?;
        let mut files = Vec::new();
        for i in 0..zip.len() {
            let entry = zip.by_index_raw(i).map_err(|e| Error::Format(format!("zip: {e}")))?;
            if entry.is_dir() {
                continue;
            }
            let name = entry.name().map_err(|e| Error::Format(format!("zip entry name: {e}")))?.into_owned();
            if entry.compression() != zip::CompressionMethod::Stored {
                return Err(Error::Unsupported(format!("{name}: compressed USDZ entries")));
            }
            let start = entry.data_start().ok_or_else(|| Error::Format("zip entry without data".into()))? as usize;
            let end = start.checked_add(entry.size() as usize).filter(|&e| e <= bytes.len()).ok_or_else(|| Error::Format("zip entry runs past the file".into()))?;
            files.push((name, start..end));
        }
        // The first USD file is the root layer.
        let (root, range) = files
            .iter()
            .find(|(n, _)| [".usd", ".usda", ".usdc"].iter().any(|e| n.to_ascii_lowercase().ends_with(e)))
            .cloned()
            .ok_or_else(|| Error::Format("USDZ without a layer".into()))?;
        let layer = crate::read_layer(&bytes[range], limits)?;
        Ok(Self { bytes, files, layer, root })
    }

    /// A file in the package by its asset path (`textures/wood.png`, `./x.png`),
    /// relative to the root layer.
    pub fn file(&self, asset: &str) -> Option<&'a [u8]> {
        let name = asset.trim_start_matches("./").replace('\\', "/");
        let dir = self.root.rsplit_once('/').map_or("", |(d, _)| d);
        let joined = if dir.is_empty() { name.clone() } else { format!("{dir}/{name}") };
        self.files.iter().find(|(n, _)| *n == joined || *n == name).map(|(_, r)| &self.bytes[r.clone()])
    }

    /// Every stored file: its name, and where its bytes are in the package.
    pub fn files(&self) -> impl Iterator<Item = (&str, std::ops::Range<usize>)> {
        self.files.iter().map(|(n, r)| (n.as_str(), r.clone()))
    }

    pub fn file_names(&self) -> impl Iterator<Item = &str> {
        self.files.iter().map(|(n, _)| n.as_str())
    }
}
