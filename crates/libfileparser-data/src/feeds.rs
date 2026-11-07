//! Feeds and playlists: RSS (0.9x, 1.0 and 2.0) and Atom as tables of items; M3U and
//! M3U8 (HLS too), PLS and XSPF as tables of entries.

use std::sync::Arc;

use libfileparser_core::{Cell, Content, Document, FileSource, OwnedTable, ParseContext, ParseError, Result, Role, ValueKind};

use crate::lines::{self, Line, LineTable, RowLines, columns, num, text};
use crate::nodes::{self, NodeTable, child, format, local, named, value};
use crate::tree::DataTree;

pub(crate) fn looks_like_rss(header: &[u8]) -> bool {
    memchr::memmem::find(header, b"<rss").is_some() || (memchr::memmem::find(header, b"<rdf:RDF").is_some() && memchr::memmem::find(header, b"purl.org/rss").is_some())
}

pub(crate) fn looks_like_atom(header: &[u8]) -> bool {
    memchr::memmem::find(header, b"<feed").is_some() && memchr::memmem::find(header, b"http://www.w3.org/2005/Atom").is_some()
}

/// The first of `names` (local names) below `id` that has a value.
fn first(t: &DataTree, id: u32, names: &[&str]) -> Cell {
    names.iter().filter_map(|n| child(t, id, n)).map(|n| value(t, Some(n))).find(|c| !c.is_null()).unwrap_or(Cell::Null)
}

fn date(c: Cell) -> Cell {
    match c {
        Cell::Text(s) => Cell::Date(s),
        other => other,
    }
}

fn feed_document(tree: DataTree, rows: Vec<u32>, title: Cell, id: &'static str, name: &str, src: &FileSource, atom: bool) -> Document {
    let tree = Arc::new(tree);
    let table = NodeTable::new(tree, rows.into())
        .column("Title", ValueKind::Text, |t, row| first(t, row, &["title"]))
        .column("Published", ValueKind::Date, move |t, row| date(if atom { first(t, row, &["published", "updated"]) } else { first(t, row, &["pubDate", "date", "issued"]) }))
        .column("Link", ValueKind::Text, move |t, row| {
            if !atom {
                return first(t, row, &["link", "guid"]);
            }
            // Atom: the alternate link, else the first.
            let links: Vec<u32> = t.child_ids(row).iter().copied().filter(|&c| local(t.label(c)) == "link").collect();
            let pick = links.iter().find(|&&l| child(t, l, "@rel").is_none_or(|r| t.value(r) == "alternate")).or(links.first());
            pick.map_or(Cell::Null, |&l| value(t, child(t, l, "@href")))
        })
        .column("Author", ValueKind::Text, move |t, row| {
            if atom {
                child(t, row, "author").map_or(Cell::Null, |a| first(t, a, &["name", "email"]))
            } else {
                first(t, row, &["author", "creator"])
            }
        })
        .column("Summary", ValueKind::Text, |t, row| first(t, row, &["description", "summary", "content", "encoded"]));
    let mut doc = nodes::document(format(id, name), "Items", table, src, Some("xml"), true);
    if let Cell::Text(title) = title {
        doc = doc.with_meta("Title", title);
    }
    doc
}

pub(crate) fn rss(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let tree = crate::xml::parse(src.bytes(), cx)?;
    let root = tree.roots().first().copied().ok_or_else(|| ParseError::corrupt("RSS: empty"))?;
    // 2.0 and 0.9x: rss/channel/item; 1.0: rdf:RDF/item beside the channel.
    let channel = child(&tree, root, "channel").unwrap_or(root);
    let items: Vec<u32> = if local(tree.label(root)) == "RDF" { named(&tree, root, "item") } else { tree.child_ids(channel).iter().copied().filter(|&c| local(tree.label(c)) == "item").collect() };
    let title = first(&tree, channel, &["title"]);
    let version = child(&tree, root, "@version").map(|v| format!("RSS {}", tree.value(v))).unwrap_or_else(|| "RSS 1.0".into());
    Ok(feed_document(tree, items, title, "rss", &version, src, false))
}

pub(crate) fn atom(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let tree = crate::xml::parse(src.bytes(), cx)?;
    let root = tree.roots().iter().copied().find(|&r| local(tree.label(r)) == "feed").ok_or_else(|| ParseError::corrupt("Atom: no <feed>"))?;
    let entries: Vec<u32> = tree.child_ids(root).iter().copied().filter(|&c| local(tree.label(c)) == "entry").collect();
    let title = first(&tree, root, &["title"]);
    Ok(feed_document(tree, entries, title, "atom", "Atom", src, true))
}

// ---------------------------------------------------------------- M3U

/// `#EXTINF:123 tvg-id="x",Title` → (seconds, title).
fn extinf(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix("#EXTINF:")?;
    let (head, title) = rest.split_once(',').unwrap_or((rest, ""));
    Some((head.split_whitespace().next().unwrap_or(""), title.trim()))
}

pub(crate) fn m3u(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    // A record starts at an entry's first line (`#EXTINF`, `#EXT-X-STREAM-INF` or the
    // location itself) and ends with its location.
    let mut expecting = true;
    let mut hls = false;
    let index = lines::index(src, "m3u", cx, |_, line| {
        let line = line.trim();
        if line.starts_with("#EXT-X-") {
            hls = true;
        }
        if line.is_empty() {
            return Line::Within;
        }
        let entry_tag = line.starts_with("#EXTINF") || line.starts_with("#EXT-X-STREAM-INF");
        let location = !line.starts_with('#');
        let starts = expecting && (entry_tag || location);
        if starts {
            expecting = false;
        }
        if location {
            expecting = true;
        }
        if starts { Line::Start } else { Line::Within }
    })?;
    let table = LineTable::new(
        index,
        columns(&[("Title", ValueKind::Text), ("Duration (s)", ValueKind::Number), ("Location", ValueKind::Text), ("Bandwidth", ValueKind::Number), ("Resolution", ValueKind::Text), ("Codecs", ValueKind::Text)]),
        |_, lines| {
            let mut row = vec![Cell::Null; 6];
            for line in lines.iter().map(|l| l.trim()) {
                if let Some((secs, title)) = extinf(line) {
                    row[0] = text(title);
                    row[1] = num(secs);
                } else if let Some(attrs) = line.strip_prefix("#EXT-X-STREAM-INF:") {
                    for (k, v) in hls_attributes(attrs) {
                        match k.as_str() {
                            "BANDWIDTH" => row[3] = num(&v),
                            "RESOLUTION" => row[4] = text(&v),
                            "CODECS" => row[5] = text(&v),
                            _ => {}
                        }
                    }
                } else if !line.starts_with('#') && !line.is_empty() {
                    row[2] = text(line);
                }
            }
            row
        },
    );
    let name = if hls { "HLS playlist" } else { "M3U playlist" };
    Ok(lines::document("m3u", name, "Entries", table, src, "m3u"))
}

/// HLS attribute lists: `BANDWIDTH=1280000,CODECS="avc1,mp4a"`.
fn hls_attributes(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(eq) = rest.find('=') {
        let key = rest[..eq].trim().to_string();
        rest = &rest[eq + 1..];
        let value = if let Some(r) = rest.strip_prefix('"') {
            let end = r.find('"').unwrap_or(r.len());
            let v = &r[..end];
            rest = r.get(end + 1..).unwrap_or("");
            v.to_string()
        } else {
            let end = rest.find(',').unwrap_or(rest.len());
            let v = &rest[..end];
            rest = &rest[end..];
            v.to_string()
        };
        out.push((key, value));
        rest = rest.trim_start_matches(',');
    }
    out
}

// ---------------------------------------------------------------- PLS

pub(crate) fn pls(src: &FileSource) -> Result<Document> {
    let body = String::from_utf8_lossy(src.bytes());
    // `FileN=`, `TitleN=`, `LengthN=`, by N.
    let mut entries: Vec<(u32, [Option<String>; 3], u64)> = Vec::new();
    for (line_no, line) in body.lines().enumerate() {
        let Some((key, value)) = line.split_once('=') else { continue };
        let key = key.trim();
        let (field, n) = match key.find(|c: char| c.is_ascii_digit()) {
            Some(at) => (&key[..at], key[at..].parse::<u32>().ok()),
            None => continue,
        };
        let Some(n) = n else { continue };
        let slot = match field.to_ascii_lowercase().as_str() {
            "file" => 0,
            "title" => 1,
            "length" => 2,
            _ => continue,
        };
        let i = match entries.iter().position(|e| e.0 == n) {
            Some(i) => i,
            None => {
                entries.push((n, Default::default(), line_no as u64));
                entries.len() - 1
            }
        };
        entries[i].1[slot] = Some(value.trim().to_string());
    }
    if entries.is_empty() {
        return Err(ParseError::corrupt("PLS: no entries"));
    }
    entries.sort_by_key(|e| e.0);
    let lines: Vec<std::ops::Range<u64>> = entries.iter().map(|e| e.2..e.2 + 1).collect();
    let rows = entries
        .into_iter()
        .map(|(_, [file, title, length], _)| {
            // -1 is "unknown length".
            let length = length.filter(|l| l != "-1").map_or(Cell::Null, |l| num(&l));
            vec![title.map_or(Cell::Null, |t| text(&t)), length, file.map_or(Cell::Null, |f| text(&f))]
        })
        .collect::<Vec<_>>();
    let n = rows.len();
    let table = OwnedTable::new(columns(&[("Title", ValueKind::Text), ("Duration (s)", ValueKind::Number), ("Location", ValueKind::Text)]), rows);
    Ok(libfileparser_core::Document::new(format("pls", "PLS playlist"))
        .with_facet(Role::Primary, "Entries", Content::Table(Arc::new(table)))
        .with_meta("Entries", n)
        .with_links(RowLines(lines))
        .with_file_facets(src, Some("ini")))
}

// ---------------------------------------------------------------- XSPF

pub(crate) fn xspf(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let tree = crate::xml::parse(src.bytes(), cx)?;
    let root = tree.roots().iter().copied().find(|&r| local(tree.label(r)) == "playlist").ok_or_else(|| ParseError::corrupt("XSPF: no <playlist>"))?;
    let tracks: Vec<u32> = child(&tree, root, "trackList").map(|l| tree.child_ids(l).iter().copied().filter(|&c| local(tree.label(c)) == "track").collect()).unwrap_or_default();
    let title = first(&tree, root, &["title"]);
    let tree = Arc::new(tree);
    let table = NodeTable::new(tree, tracks.into())
        .column("Title", ValueKind::Text, |t, row| first(t, row, &["title"]))
        .column("Creator", ValueKind::Text, |t, row| first(t, row, &["creator"]))
        .column("Album", ValueKind::Text, |t, row| first(t, row, &["album"]))
        .column("Track", ValueKind::Number, |t, row| match first(t, row, &["trackNum"]) {
            Cell::Text(s) => num(&s),
            c => c,
        })
        .column("Duration (s)", ValueKind::Number, |t, row| match first(t, row, &["duration"]) {
            // Milliseconds.
            Cell::Text(s) => s.trim().parse::<f64>().map_or(Cell::Null, |ms| Cell::Float(ms / 1000.0)),
            c => c,
        })
        .column("Location", ValueKind::Text, |t, row| first(t, row, &["location"]));
    let mut doc = nodes::document(format("xspf", "XSPF playlist"), "Tracks", table, src, Some("xml"), true);
    if let Cell::Text(title) = title {
        doc = doc.with_meta("Title", title);
    }
    Ok(doc)
}
