//! Comic book archives: CBZ (zip) and CBT (tar) as pages, one per image, in the
//! order their names sort naturally ("page2" before "page10"). Their ComicInfo.xml
//! (title, series, number, writer…) is metadata. CB7 and CBR are recognised and refused
//! until 7-Zip and RAR can be read.

use std::io::{Cursor, Read};
use std::sync::{Arc, Mutex, OnceLock};

use libfileparser_core::{
    Cancel, Category, Confidence, Content, Detection, Document, FileSource, Limit, MemoryUse, PageRect, PageSize, PageSource, ParseContext, ParseError, Reader, ReaderInfo,
    Result, render_size,
};

static INFO: ReaderInfo = ReaderInfo::new("comic", "Comic book", Category::Docs)
    .extensions(&["cbz", "cbt", "cb7", "cbr"])
    .mime_types(&["application/vnd.comicbook+zip", "application/vnd.comicbook+tar", "application/vnd.comicbook-rar"]);

const IMAGES: &[&str] = &["jpg", "jpeg", "png", "gif", "webp", "bmp", "tif", "tiff", "avif", "jxl"];

pub struct ComicReader;

fn is_zip(h: &[u8]) -> bool {
    h.starts_with(b"PK\x03\x04") || h.starts_with(b"PK\x05\x06")
}

fn is_tar(h: &[u8]) -> bool {
    h.get(257..262) == Some(b"ustar")
}

fn ext(name: &str) -> String {
    name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default()
}

impl Reader for ComicReader {
    fn info(&self) -> &ReaderInfo {
        &INFO
    }

    /// Certain when the container's signature and the name agree: a CBZ outranks the
    /// zip reader.
    fn detect(&self, header: &[u8], name: &str) -> Detection {
        let ok = match ext(name).as_str() {
            "cbz" => is_zip(header),
            "cbt" => is_tar(header),
            "cb7" => header.starts_with(b"7z\xBC\xAF\x27\x1C"),
            "cbr" => header.starts_with(b"Rar!"),
            _ => false,
        };
        if ok { Detection::new(Confidence::Certain) } else { Detection::NONE }
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        cx.cancel.check()?;
        let header = src.header();
        let pages = if is_zip(header) {
            Comic::zip(src, cx)?
        } else if is_tar(header) {
            Comic::tar(src, cx)?
        } else if header.starts_with(b"7z\xBC\xAF\x27\x1C") || header.starts_with(b"Rar!") {
            return Err(ParseError::unsupported("CB7 and CBR comics (7-Zip and RAR) can't be read yet"));
        } else {
            return Err(ParseError::NotThisFormat);
        };
        if pages.pages.is_empty() {
            return Err(ParseError::corrupt("no images in this comic"));
        }
        let meta = pages.meta.clone();
        crate::pages::document("comic", "Comic book", src, Arc::new(pages), meta, cx)
    }
}

/// A page decoded: its index, width, height and pixels.
type Decoded = (u64, u32, u32, Arc<Vec<u8>>);

/// A comic's pages: images inside the archive, decoded when a page is drawn.
struct Comic {
    pages: Vec<FileSource>,
    sizes: Vec<OnceLock<std::result::Result<PageSize, String>>>,
    meta: Vec<(String, String)>,
    /// The last pages decoded, so tiles of one page don't decode it again.
    cache: Mutex<Vec<Decoded>>,
    cx: ParseContext,
}

impl Comic {
    fn new(mut entries: Vec<(String, FileSource)>, info: Option<String>, cx: &ParseContext) -> Self {
        entries.sort_by(|a, b| natural(&a.0, &b.0));
        let pages: Vec<FileSource> = entries.into_iter().map(|(_, f)| f).collect();
        Self { sizes: (0..pages.len()).map(|_| OnceLock::new()).collect(), pages, meta: info.map(|x| comic_info(&x)).unwrap_or_default(), cache: Mutex::new(Vec::new()), cx: cx.clone() }
    }

    fn zip(src: &FileSource, cx: &ParseContext) -> Result<Self> {
        let bytes = crate::whole(src, cx)?;
        let mut z = zip::ZipArchive::new(Cursor::new(bytes.as_slice())).map_err(|e| ParseError::corrupt(format!("zip: {e}")))?;
        cx.limits.check(Limit::Entries, z.len() as u64)?;
        let mut entries = Vec::new();
        let mut info = None;
        for i in 0..z.len() {
            cx.cancel.check()?;
            let mut f = z.by_index(i).map_err(|e| ParseError::corrupt(format!("zip: {e}")))?;
            let name = f.name().map(|n| n.to_string()).unwrap_or_default();
            let wanted = IMAGES.contains(&ext(&name).as_str()) && !name.starts_with("__MACOSX/") && !name.rsplit('/').next().unwrap_or("").starts_with('.');
            let is_info = name.eq_ignore_ascii_case("ComicInfo.xml");
            if !(wanted || is_info) || f.is_dir() {
                continue;
            }
            let (packed, size) = (f.compressed_size(), f.size());
            let data = read_entry(&mut f, packed, size, cx)?;
            if is_info {
                info = Some(String::from_utf8_lossy(&data).into_owned());
            } else {
                entries.push((name.clone(), FileSource::from_bytes(name.rsplit('/').next().unwrap_or(&name).to_string(), data)));
            }
        }
        Ok(Self::new(entries, info, cx))
    }

    fn tar(src: &FileSource, cx: &ParseContext) -> Result<Self> {
        let bytes = crate::whole(src, cx)?;
        let mut a = tar::Archive::new(Cursor::new(bytes.as_slice()));
        let mut entries = Vec::new();
        let mut info = None;
        for (n, e) in a.entries().map_err(|e| ParseError::corrupt(format!("tar: {e}")))?.enumerate() {
            cx.cancel.check()?;
            cx.limits.check(Limit::Entries, n as u64 + 1)?;
            let mut e = e.map_err(|e| ParseError::corrupt(format!("tar: {e}")))?;
            let name = e.path().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
            let is_info = name.eq_ignore_ascii_case("ComicInfo.xml");
            if !(IMAGES.contains(&ext(&name).as_str()) || is_info) {
                continue;
            }
            let size = e.size();
            let data = read_entry(&mut e, size, size, cx)?;
            if is_info {
                info = Some(String::from_utf8_lossy(&data).into_owned());
            } else {
                entries.push((name.clone(), FileSource::from_bytes(name.rsplit('/').next().unwrap_or(&name).to_string(), data)));
            }
        }
        Ok(Self::new(entries, info, cx))
    }

    /// Page `index` decoded, from the cache or now.
    fn decoded(&self, index: u64) -> Result<(u32, u32, Arc<Vec<u8>>)> {
        if let Some(&(_, w, h, ref p)) = self.cache.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|c| c.0 == index) {
            return Ok((w, h, p.clone()));
        }
        let doc = libfileparser_images::open(&self.pages[index as usize], &self.cx)?;
        let Content::Raster(r) = doc.facet(0, &self.cx)? else { return Err(ParseError::corrupt("a page isn't an image")) };
        let (w, h, rgba) = (r.width(), r.height(), Arc::new(r.decode()?));
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.insert(0, (index, w, h, rgba.clone()));
        cache.truncate(2);
        Ok((w, h, rgba))
    }

    fn check(&self, index: u64) -> Result<()> {
        if index < self.pages.len() as u64 { Ok(()) } else { Err(ParseError::invalid(format!("no page {}", index + 1))) }
    }
}

/// An archive entry's bytes, within the limits on one allocation and on unpacking.
fn read_entry(r: &mut impl Read, packed: u64, size: u64, cx: &ParseContext) -> Result<Vec<u8>> {
    cx.limits.check(Limit::Allocation, size)?;
    cx.limits.check_unpack(packed, size)?;
    let mut data = Vec::with_capacity(size as usize);
    r.take(size).read_to_end(&mut data).map_err(|e| ParseError::corrupt(format!("an entry: {e}")))?;
    Ok(data)
}

impl PageSource for Comic {
    fn page_count(&self) -> u64 {
        self.pages.len() as u64
    }

    /// The image's size, one point a pixel, read from its header alone.
    fn page_size(&self, index: u64) -> Result<PageSize> {
        self.check(index)?;
        self.sizes[index as usize]
            .get_or_init(|| {
                let doc = libfileparser_images::open(&self.pages[index as usize], &self.cx).map_err(|e| e.to_string())?;
                match doc.facet(0, &self.cx).map_err(|e| e.to_string())? {
                    Content::Raster(r) => Ok(PageSize::new(r.width() as f32, r.height() as f32)),
                    _ => Err("a page isn't an image".to_string()),
                }
            })
            .clone()
            .map_err(ParseError::corrupt)
    }

    fn render(&self, index: u64, scale: f32, region: PageRect, rgba: &mut Vec<u8>, cancel: &Cancel) -> Result<(u32, u32)> {
        self.page_size(index)?;
        let (w, h) = render_size(scale, region)?;
        cancel.check()?;
        let (pw, ph, pixels) = self.decoded(index)?;
        crate::pages::resample(crate::pages::Image { rgba: &pixels, width: pw, height: ph }, region, scale, (w, h), rgba);
        Ok((w, h))
    }

    fn engine(&self) -> &str {
        "images"
    }

    fn memory(&self) -> MemoryUse {
        let packed: u64 = self.pages.iter().map(FileSource::len).sum();
        let decoded: u64 = self.cache.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|c| c.3.len() as u64).sum();
        MemoryUse::heap(packed + decoded)
    }
}

/// "page2" before "page10": runs of digits compare as numbers.
fn natural(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut a, mut b) = (a.as_bytes(), b.as_bytes());
    loop {
        match (a.first(), b.first()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, _) => return std::cmp::Ordering::Less,
            (_, None) => return std::cmp::Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let na = a.iter().take_while(|c| c.is_ascii_digit()).count();
                let nb = b.iter().take_while(|c| c.is_ascii_digit()).count();
                let (da, db) = (std::str::from_utf8(&a[..na]).unwrap_or("0"), std::str::from_utf8(&b[..nb]).unwrap_or("0"));
                let (ta, tb) = (da.trim_start_matches('0'), db.trim_start_matches('0'));
                let o = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if o != std::cmp::Ordering::Equal {
                    return o;
                }
                a = &a[na..];
                b = &b[nb..];
            }
            (Some(x), Some(y)) => {
                let o = x.to_ascii_lowercase().cmp(&y.to_ascii_lowercase());
                if o != std::cmp::Ordering::Equal {
                    return o;
                }
                a = &a[1..];
                b = &b[1..];
            }
        }
    }
}

/// The fields of ComicInfo.xml worth showing.
fn comic_info(xml: &str) -> Vec<(String, String)> {
    let field = |tag: &str| {
        let open = format!("<{tag}>");
        let start = xml.find(&open)? + open.len();
        let end = xml[start..].find(&format!("</{tag}>"))? + start;
        let v = xml[start..end].trim();
        (!v.is_empty()).then(|| v.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">"))
    };
    [("Title", "Title"), ("Series", "Series"), ("Number", "Number"), ("Volume", "Volume"), ("Writer", "Writer"), ("Penciller", "Artist"), ("Publisher", "Publisher"), ("Year", "Year")]
        .into_iter()
        .filter_map(|(tag, label)| field(tag).map(|v| (label.to_string(), v)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order() {
        let mut v = vec!["p10.png", "p2.png", "P1.png", "cover.jpg"];
        v.sort_by(|a, b| natural(a, b));
        assert_eq!(v, ["cover.jpg", "P1.png", "p2.png", "p10.png"]);
    }

    #[test]
    fn comic_info_fields() {
        let m = comic_info("<ComicInfo><Title>One &amp; Two</Title><Series>S</Series><Number>3</Number></ComicInfo>");
        assert_eq!(m, [("Title".to_string(), "One & Two".to_string()), ("Series".into(), "S".into()), ("Number".into(), "3".into())]);
    }
}
