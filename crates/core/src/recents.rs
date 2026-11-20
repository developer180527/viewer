//! Recently opened files, newest first, with where the reader was in each: what a
//! shell's Open Recent shows and what reopening a file goes back to. Kept in a small
//! text file the shell names (its app-support folder), one line per file.
//!
//! A sandboxed shell can't open a path again by itself: it saves the platform's token
//! for it (a security-scoped bookmark) with the entry, as bytes this doesn't read.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// How many files are remembered.
pub const LIMIT: usize = 20;

/// Where the reader was in a paged document (see libviewer's `PagesPlace`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Place {
    /// The page at the window's top, from 0.
    pub page: u32,
    /// How far into it, in page points.
    pub offset: f32,
    /// `None`: fit to width.
    pub zoom: Option<f32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Recent {
    pub path: PathBuf,
    /// Seconds since 1970.
    pub opened: u64,
    pub place: Option<Place>,
    /// The shell's token for reopening it; empty when it needs none.
    pub bookmark: Vec<u8>,
}

impl Recent {
    pub fn name(&self) -> String {
        self.path.file_name().map_or_else(|| self.path.display().to_string(), |n| n.to_string_lossy().into_owned())
    }
}

/// The list, and the file it's kept in.
#[derive(Debug, Default)]
pub struct Recents {
    file: Option<PathBuf>,
    items: Vec<Recent>,
    dirty: bool,
}

impl Recents {
    /// Only in memory: nothing is saved.
    pub fn in_memory() -> Self {
        Self::default()
    }

    /// The list kept in `file`. A missing or unreadable file is an empty list; lines it
    /// can't read are skipped.
    pub fn load(file: PathBuf) -> Self {
        let items = std::fs::read_to_string(&file).map(|s| s.lines().filter_map(parse).take(LIMIT).collect()).unwrap_or_default();
        Self { file: Some(file), items, dirty: false }
    }

    pub fn items(&self) -> &[Recent] {
        &self.items
    }

    pub fn get(&self, path: &Path) -> Option<&Recent> {
        self.items.iter().find(|r| r.path == path)
    }

    /// `path` was opened: it goes to the top, keeping its place, and with `bookmark`
    /// when the shell gave a new one.
    pub fn note(&mut self, path: &Path, bookmark: Option<Vec<u8>>) {
        if !storable(path) {
            return;
        }
        let old = self.items.iter().position(|r| r.path == path).map(|i| self.items.remove(i));
        let (place, old_bookmark) = old.map_or((None, Vec::new()), |r| (r.place, r.bookmark));
        self.items.insert(0, Recent { path: path.to_path_buf(), opened: now(), place, bookmark: bookmark.unwrap_or(old_bookmark) });
        self.items.truncate(LIMIT);
        self.dirty = true;
    }

    /// Where the reader is in `path`, if it's in the list.
    pub fn set_place(&mut self, path: &Path, place: Place) {
        if let Some(r) = self.items.iter_mut().find(|r| r.path == path)
            && r.place != Some(place)
        {
            r.place = Some(place);
            self.dirty = true;
        }
    }

    pub fn remove(&mut self, path: &Path) {
        let n = self.items.len();
        self.items.retain(|r| r.path != path);
        self.dirty |= self.items.len() != n;
    }

    pub fn clear(&mut self) {
        self.dirty |= !self.items.is_empty();
        self.items.clear();
    }

    /// Writes the list if it changed since it was loaded or saved: to a new file put in
    /// place of the old, so a crash leaves one or the other.
    pub fn save(&mut self) -> std::io::Result<()> {
        let Some(file) = &self.file else { return Ok(()) };
        if !self.dirty {
            return Ok(());
        }
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text: String = self.items.iter().map(|r| format!("{}\n", line(r))).collect();
        let tmp = file.with_extension("tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, file)?;
        self.dirty = false;
        Ok(())
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// A path the line format can hold: no tabs or line breaks (fields are tab-separated).
fn storable(path: &Path) -> bool {
    path.to_str().is_some_and(|s| !s.is_empty() && !s.contains(['\t', '\n', '\r']))
}

// One file per line: path, opened, place ("page,offset,zoom" with zoom "fit", or "-"),
// bookmark in hex.

fn line(r: &Recent) -> String {
    let place = r.place.map_or_else(|| "-".to_owned(), |p| format!("{},{},{}", p.page, p.offset, p.zoom.map_or_else(|| "fit".to_owned(), |z| z.to_string())));
    let hex: String = r.bookmark.iter().map(|b| format!("{b:02x}")).collect();
    format!("{}\t{}\t{place}\t{hex}", r.path.display(), r.opened)
}

fn parse(line: &str) -> Option<Recent> {
    let mut f = line.split('\t');
    let path = PathBuf::from(f.next().filter(|p| !p.is_empty())?);
    let opened = f.next()?.parse().ok()?;
    let place = match f.next()? {
        "-" => None,
        p => {
            let mut p = p.split(',');
            let page = p.next()?.parse().ok()?;
            let offset = p.next()?.parse().ok()?;
            let zoom = match p.next()? {
                "fit" => None,
                z => Some(z.parse().ok()?),
            };
            Some(Place { page, offset, zoom })
        }
    };
    let hex = f.next().unwrap_or("");
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    let bookmark = (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok()).collect::<Option<Vec<u8>>>()?;
    Some(Recent { path, opened, place, bookmark })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_first_keeping_place_and_bookmark() {
        let mut r = Recents::in_memory();
        r.note(Path::new("/a.pdf"), Some(vec![1, 2]));
        r.note(Path::new("/b.pdf"), None);
        r.set_place(Path::new("/a.pdf"), Place { page: 3, offset: 12.5, zoom: None });
        r.note(Path::new("/a.pdf"), None);
        let paths: Vec<_> = r.items().iter().map(|i| i.path.clone()).collect();
        assert_eq!(paths, [PathBuf::from("/a.pdf"), PathBuf::from("/b.pdf")]);
        assert_eq!(r.items()[0].bookmark, [1, 2]);
        assert_eq!(r.items()[0].place.map(|p| p.page), Some(3));
        for i in 0..30 {
            r.note(Path::new(&format!("/{i}")), None);
        }
        assert_eq!(r.items().len(), LIMIT);
        r.note(Path::new("/tab\there"), None);
        assert!(r.get(Path::new("/tab\there")).is_none());
    }

    #[test]
    fn saved_and_loaded() {
        let dir = std::env::temp_dir().join(format!("viewer-recents-{}", std::process::id()));
        let file = dir.join("recents.txt");
        let mut r = Recents::load(file.clone());
        assert!(r.items().is_empty());
        r.note(Path::new("/docs/a b.pdf"), Some(vec![0, 255, 16]));
        r.set_place(Path::new("/docs/a b.pdf"), Place { page: 7, offset: -12.0, zoom: Some(1.25) });
        r.note(Path::new("/c.json"), None);
        r.save().unwrap();
        let back = Recents::load(file.clone());
        assert_eq!(back.items(), r.items());
        // A damaged line is skipped, not the whole list.
        let text = std::fs::read_to_string(&file).unwrap();
        std::fs::write(&file, format!("garbage\n{text}/x\t1\t1,2\tzz\n")).unwrap();
        assert_eq!(Recents::load(file).items().len(), 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
