//! Zip-based documents (DOCX, ODT): reading their parts within the limits, and formatted
//! text whose images are read from the package when asked for.

use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::ops::Range;

use libfileparser_core::{Block, Bytes, Count, FileSource, FlowSource, Limit, MemoryUse, OwnedFlow, ParseContext, ParseError, Result};

/// A zip package: its bytes and where each part is.
pub(crate) struct Package {
    bytes: Bytes,
    names: HashMap<String, usize>,
    cx: ParseContext,
}

impl Package {
    pub fn open(src: &FileSource, cx: &ParseContext) -> Result<Self> {
        let bytes = crate::whole(src, cx)?;
        let z = zip::ZipArchive::new(Cursor::new(bytes.as_slice())).map_err(|e| ParseError::corrupt(format!("zip: {e}")))?;
        cx.limits.check(Limit::Entries, z.len() as u64)?;
        let names = (0..z.len()).filter_map(|i| z.name_for_index(i).and_then(|n| n.ok()).map(|n| (n.to_string(), i))).collect();
        Ok(Self { bytes, names, cx: cx.clone() })
    }

    /// Part `name`'s bytes (a leading `/` ignored), within the unpack limits; `None`
    /// when there's no such part.
    pub fn read(&self, name: &str) -> Result<Option<Vec<u8>>> {
        let name = name.trim_start_matches('/');
        let Some(&i) = self.names.get(name) else { return Ok(None) };
        let mut z = zip::ZipArchive::new(Cursor::new(self.bytes.as_slice())).map_err(|e| ParseError::corrupt(format!("zip: {e}")))?;
        let f = z.by_index(i).map_err(|e| ParseError::corrupt(format!("{name}: {e}")))?;
        let (packed, size) = (f.compressed_size(), f.size());
        self.cx.limits.check(Limit::Allocation, size)?;
        self.cx.limits.check_unpack(packed, size)?;
        let mut data = Vec::with_capacity(size as usize);
        f.take(size).read_to_end(&mut data).map_err(|e| ParseError::corrupt(format!("{name}: {e}")))?;
        Ok(Some(data))
    }

    pub fn memory(&self) -> u64 {
        self.bytes.len() as u64
    }

}

/// `path` relative to the folder of part `base`, `..` and `.` resolved: "word/" +
/// "media/a.png", "OEBPS/text/c1.xhtml" + "../images/x.jpg". URL escapes (`%20`) are
/// undone; a fragment (`#…`) is dropped.
pub(crate) fn resolve(base: &str, path: &str) -> String {
    let path = path.split('#').next().unwrap_or("");
    let path = percent_decode(path);
    let mut parts: Vec<&str> = if path.starts_with('/') { Vec::new() } else { base.rsplit_once('/').map_or(Vec::new(), |(d, _)| d.split('/').collect()) };
    for p in path.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    parts.join("/")
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(v) = b.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Formatted text whose images are parts of a package, read when a viewer asks.
pub(crate) struct PackagedFlow {
    flow: OwnedFlow,
    package: Package,
}

impl PackagedFlow {
    pub fn new(blocks: Vec<Block>, package: Package) -> Self {
        Self { flow: OwnedFlow::new(blocks), package }
    }
}

impl FlowSource for PackagedFlow {
    fn block_count(&self) -> Count {
        self.flow.block_count()
    }

    fn blocks(&self, range: Range<u64>) -> Result<Vec<Block>> {
        self.flow.blocks(range)
    }

    fn resource(&self, reference: &str) -> Result<FileSource> {
        let data = self.package.read(reference)?.ok_or_else(|| ParseError::invalid(format!("no part {reference:?}")))?;
        Ok(FileSource::from_bytes(reference.rsplit('/').next().unwrap_or(reference).to_string(), data))
    }

    fn memory(&self) -> MemoryUse {
        self.flow.memory() + MemoryUse::mapped(self.package.memory())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert_eq!(resolve("word/document.xml", "media/image1.png"), "word/media/image1.png");
        assert_eq!(resolve("OEBPS/text/c1.xhtml", "../images/a%20b.jpg#x"), "OEBPS/images/a b.jpg");
        assert_eq!(resolve("a/b.xml", "/root.png"), "root.png");
    }
}
