//! The test corpus (corpus/README.md): every file in `corpus/manifest.toml` is opened
//! through the same registry the app uses, and what it opened as is written down as a
//! small summary, plus a small render for images and documents. `tests/corpus.rs`
//! compares those with `corpus/expected/`.
//!
//! Only what can be checked without a window or a GPU goes in a summary: which loader
//! won and which failed first, the content model and its numbers, the metadata, and
//! the files the document couldn't read.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use image::RgbaImage;
use toml::{Table, Value};
use viewer_core::{Content, Document, FileSource, PageRect, Unreadable};

/// The longest side of a golden render, in pixels: enough to catch a wrong decode or
/// a blank page, small enough to keep the goldens tiny.
pub const GOLDEN_SIDE: u32 = 128;
/// How long an open may take before it counts as hung, unless the manifest says.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(20);
/// Lists longer than this are cut, with the full count kept beside them.
const LIST_MAX: usize = 20;

/// One file in the manifest.
#[derive(Clone, Debug)]
pub struct Entry {
    /// From the repository root.
    pub path: String,
    pub golden: bool,
    pub optional: bool,
    pub timeout: Duration,
    /// Target OSes (`std::env::consts::OS`) the file isn't checked on, because what
    /// opens it isn't built there yet.
    pub skip_on: Vec<String>,
}

impl Entry {
    /// Whether this file is checked on the OS the tests run on.
    pub fn checked_here(&self) -> bool {
        !self.skip_on.iter().any(|os| os == std::env::consts::OS)
    }
}

/// The repository root.
pub fn root() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    root.canonicalize().unwrap_or(root)
}

/// Reads `corpus/manifest.toml`.
pub fn manifest() -> anyhow::Result<Vec<Entry>> {
    let text = std::fs::read_to_string(root().join("corpus/manifest.toml"))?;
    let table: Table = text.parse()?;
    let files = table.get("file").and_then(Value::as_array).ok_or_else(|| anyhow::anyhow!("no [[file]] entries"))?;
    files
        .iter()
        .map(|f| {
            let path = f.get("path").and_then(Value::as_str).ok_or_else(|| anyhow::anyhow!("a [[file]] without a path"))?;
            let flag = |k: &str| f.get(k).and_then(Value::as_bool).unwrap_or(false);
            let timeout = f.get("timeout").and_then(Value::as_integer).map_or(DEFAULT_TIMEOUT, |s| Duration::from_secs(s as u64));
            let skip_on = f.get("skip_on").and_then(Value::as_array).map_or(Vec::new(), |a| a.iter().filter_map(Value::as_str).map(String::from).collect());
            Ok(Entry { path: path.into(), golden: flag("golden"), optional: flag("optional"), timeout, skip_on })
        })
        .collect()
}

/// What a file opened as.
pub struct Opened {
    pub summary: Table,
    /// For `golden` entries: the image, or the first page, scaled to [`GOLDEN_SIDE`].
    pub render: Option<RgbaImage>,
}

/// Opens `entry` on its own thread, so a hang shows as a timeout and not a stuck test.
/// Panics inside loaders are already caught by the registry; one anywhere else is an
/// error here.
pub fn open(entry: &Entry) -> Result<Opened, String> {
    let path = root().join(&entry.path);
    let (golden, deadline) = (entry.golden, Instant::now() + entry.timeout);
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name(format!("corpus: {}", entry.path))
        .spawn(move || {
            let result = std::panic::catch_unwind(|| open_here(&path, golden, deadline))
                .unwrap_or_else(|_| Err("panicked outside a loader".into()));
            let _ = tx.send(result);
        })
        .map_err(|e| e.to_string())?;
    rx.recv_timeout(entry.timeout).unwrap_or_else(|_| Err(format!("still running after {:?}", entry.timeout)))
}

fn open_here(path: &Path, golden: bool, deadline: Instant) -> Result<Opened, String> {
    let mut summary = Table::new();
    let mut doc = if path.is_dir() {
        summary.insert("loader".into(), "folder".into());
        viewer_fmt_archive::open_folder(path).map_err(|e| format!("{e:#}"))?
    } else {
        let src = FileSource::open(path).map_err(|e| e.to_string())?;
        let (doc, loader, attempts) = viewer_formats::registry().open(&src);
        summary.insert("loader".into(), loader.into());
        if !attempts.is_empty() {
            summary.insert("failed".into(), attempts.iter().map(|(id, _)| Value::from(*id)).collect::<Vec<_>>().into());
        }
        doc
    };
    summary.insert("format".into(), doc.format.clone().into());
    summary.insert("model".into(), doc.content.model_name().into());
    let render = content(&mut doc, golden, deadline, &mut summary)?;
    if !doc.metadata.is_empty() {
        // Warnings can quote the file's path: keep it relative, so the summary reads
        // the same on every machine.
        let root = format!("{}/", root().display());
        let meta: Table = doc.metadata.iter().map(|(k, v)| (k.clone(), Value::from(v.replace(&root, "")))).collect();
        summary.insert("metadata".into(), meta.into());
    }
    unreadable(&doc, &mut summary);
    Ok(Opened { summary, render })
}

/// The content model's own numbers, under a table named after it, and the render.
fn content(doc: &mut Document, golden: bool, deadline: Instant, summary: &mut Table) -> Result<Option<RgbaImage>, String> {
    let mut t = Table::new();
    let mut render = None;
    let key = match &mut doc.content {
        Content::Raster(r) => {
            t.insert("size".into(), format!("{}x{}", r.width, r.height).into());
            if golden {
                r.make_full()?;
                let rgba = r.rgba().ok_or("no pixels after decoding")?.to_vec();
                let img = RgbaImage::from_raw(r.width, r.height, rgba).ok_or("pixel count doesn't match the size")?;
                render = Some(shrink(&img));
            }
            "raster"
        }
        Content::Text(text) => {
            t.insert("lines".into(), int(text.line_count()));
            if let Some(s) = &text.syntax {
                t.insert("syntax".into(), s.clone().into());
            }
            "text"
        }
        Content::Markdown(md) => {
            t.insert("chars".into(), int(md.chars().count()));
            "markdown"
        }
        Content::Tree(node) => {
            t.insert("nodes".into(), int(node.count()));
            t.insert("top".into(), list(node.children.iter().map(|c| c.key.clone()), node.children.len()));
            "tree"
        }
        Content::Table(table) => {
            t.insert("rows".into(), int(table.rows.len()));
            t.insert("columns".into(), int(table.columns));
            t.insert("headers".into(), list(table.headers.iter().cloned(), table.headers.len()));
            "table"
        }
        Content::Container(c) => {
            let entries = c.entries();
            t.insert("entries".into(), int(entries.len()));
            t.insert("paths".into(), list(entries.iter().map(|e| e.path.clone()), entries.len()));
            "container"
        }
        Content::Hex(src) => {
            t.insert("bytes".into(), int(src.len() as usize));
            "hex"
        }
        Content::Scene(scene) => {
            // Textures decode on worker threads after the open returns: wait for them,
            // so the count is what the user ends up seeing.
            while scene.textures_pending.load(Ordering::Acquire) > 0 {
                if Instant::now() > deadline {
                    return Err("textures still decoding at the timeout".into());
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            scene_summary(&scene.scene, &mut t);
            "scene"
        }
        Content::Pages(pages) => {
            t.insert("backend".into(), pages.backend().into());
            t.insert("count".into(), int(pages.page_count()));
            let sizes = (0..pages.page_count().min(LIST_MAX)).map(|i| {
                pages.page_size(i).map_or_else(|e| format!("error: {e:#}"), |s| format!("{}x{}", s.width, s.height))
            });
            t.insert("sizes".into(), list(sizes, pages.page_count()));
            if golden && pages.page_count() > 0 {
                let size = pages.page_size(0).map_err(|e| format!("{e:#}"))?;
                let scale = GOLDEN_SIDE as f32 / size.width.max(size.height);
                let page = pages.render(0, scale, PageRect::whole(size)).map_err(|e| format!("{e:#}"))?;
                render = Some(RgbaImage::from_raw(page.width, page.height, page.rgba).ok_or("page pixel count doesn't match")?);
            }
            "pages"
        }
    };
    summary.insert(key.into(), t.into());
    Ok(render)
}

fn scene_summary(scene: &scene_data::CpuScene, t: &mut Table) {
    let read = scene.read();
    t.insert("geometry".into(), format!("{:?}", read.geometry()).into());
    let meshes = read.meshes();
    t.insert("meshes".into(), int(meshes.len()));
    t.insert("vertices".into(), int(meshes.iter().map(|m| m.info.vertices).sum()));
    t.insert("elements".into(), int(meshes.iter().map(|m| m.info.elements).sum()));
    let mut topologies: Vec<String> = meshes.iter().map(|m| format!("{:?}", m.info.topology)).collect();
    topologies.sort();
    topologies.dedup();
    t.insert("topologies".into(), list(topologies.iter().cloned(), topologies.len()));
    t.insert("materials".into(), int(read.materials().len()));
    t.insert("nodes".into(), int(read.nodes().len()));
    let textures = read.textures();
    let decoded: Vec<String> = textures.iter().filter_map(|t| t.info.as_ref()).map(|i| format!("{}x{}", i.width, i.height)).collect();
    t.insert("textures".into(), int(textures.len()));
    t.insert("textures_decoded".into(), list(decoded.iter().cloned(), decoded.len()));
    if let Some(b) = read.bounds() {
        // Rounded, so float noise between platforms and compilers doesn't show.
        let v = |v: scene_data::glam::Vec3| format!("{:.3} {:.3} {:.3}", v.x, v.y, v.z).replace("-0.000", "0.000");
        t.insert("bounds".into(), Value::Array(vec![v(b.min).into(), v(b.max).into()]));
    }
}

fn unreadable(doc: &Document, summary: &mut Table) {
    let mut t = Table::new();
    for (key, reason) in [("blocked", Unreadable::Blocked), ("missing", Unreadable::Missing), ("outside", Unreadable::Outside)] {
        let mut names = doc.unreadable.names(reason);
        names.sort();
        if !names.is_empty() {
            t.insert(key.into(), list(names.iter().cloned(), names.len()));
        }
    }
    if !t.is_empty() {
        summary.insert("unreadable".into(), t.into());
    }
}

/// `img` scaled so its longer side is at most [`GOLDEN_SIDE`].
fn shrink(img: &RgbaImage) -> RgbaImage {
    let (w, h) = img.dimensions();
    if w.max(h) <= GOLDEN_SIDE {
        return img.clone();
    }
    let s = GOLDEN_SIDE as f32 / w.max(h) as f32;
    image::imageops::thumbnail(img, ((w as f32 * s).round() as u32).max(1), ((h as f32 * s).round() as u32).max(1))
}

fn int(n: usize) -> Value {
    Value::Integer(n as i64)
}

/// Up to [`LIST_MAX`] items, then "… and N more".
fn list(items: impl Iterator<Item = String>, total: usize) -> Value {
    let mut v: Vec<Value> = items.take(LIST_MAX).map(Value::from).collect();
    if total > LIST_MAX {
        v.push(format!("… and {} more", total - LIST_MAX).into());
    }
    Value::Array(v)
}
