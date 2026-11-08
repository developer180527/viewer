//! Geographic data as a table of its features over the file's tree: GeoJSON,
//! TopoJSON, KML and KMZ, GPX, GML, OpenStreetMap XML, WKT and NMEA. Each row is a
//! feature (a placemark, a track, a way), linked to its node in the tree and its
//! lines; the document's metadata gives the bounds of everything in it.
//!
//! **Columns** (contract, docs/design/libfileparser.md §5): fixed per format (below),
//! then, where a format has them, the properties of its first features, in the order
//! first seen.

use std::ops::Range;
use std::sync::Arc;

use libfileparser_core::{
    Category, Cell, Column, Content, Count, Document, FileSource, FormatInfo, LineText, MemoryUse, OwnedTable, ParseContext, ParseError,
    Result, Role, TableInfo, TableSource, TextSource, ValueKind,
};

use crate::json::{self, Dialect};
use crate::lines::RowLines;
use crate::nodes::{MAX_PROPERTIES, NodeTable, SAMPLE};
use crate::tree::{DataTree, TOP, TreeLinks, cell};


// ---------------------------------------------------------------- shared

/// The box around a set of points, longitude and latitude.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Bounds {
    min: (f64, f64),
    max: (f64, f64),
    n: u64,
}

impl Bounds {
    fn add(&mut self, lon: f64, lat: f64) {
        if !lon.is_finite() || !lat.is_finite() {
            return;
        }
        if self.n == 0 {
            self.min = (lon, lat);
            self.max = (lon, lat);
        } else {
            self.min = (self.min.0.min(lon), self.min.1.min(lat));
            self.max = (self.max.0.max(lon), self.max.1.max(lat));
        }
        self.n += 1;
    }

    /// "west, south to east, north", or `None` without points.
    fn text(&self) -> Option<String> {
        let f = |v: f64| {
            let s = format!("{v:.6}");
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        };
        (self.n > 0).then(|| format!("{}, {} to {}, {}", f(self.min.0), f(self.min.1), f(self.max.0), f(self.max.1)))
    }
}

/// A document of features over a tree: Features, Tree, then the file's own facets.
fn features_document(format: FormatInfo, table: NodeTable, bounds: Bounds, src: &FileSource, language: Option<&str>, lines: bool) -> Document {
    let (tree, rows) = (table.tree.clone(), table.rows.clone());
    let nodes = tree.len();
    let mut doc = Document::new(format)
        .with_meta("Features", rows.len())
        .with_facet(Role::Primary, "Features", Content::Table(Arc::new(table)))
        .with_facet(Role::Structure, "Tree", Content::Tree(tree.clone()))
        .with_meta("Nodes", nodes);
    if let Some(b) = bounds.text() {
        doc = doc.with_meta("Bounds", b);
    }
    // A feature that is the whole file has no node of its own to link to.
    let table = (!rows.contains(&TOP)).then_some((0, rows));
    doc.with_links(TreeLinks { tree, tree_facet: 1, table, lines }).with_file_facets(src, language)
}

fn text(t: &DataTree, id: Option<u32>) -> Cell {
    match id {
        Some(id) if !t.value(id).is_empty() => cell(t.kind(id), t.value(id)),
        _ => Cell::Null,
    }
}

fn number(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok().filter(|v| v.is_finite())
}

/// The part of an XML name after its prefix: `gml:pos` → `pos`.
fn local(label: &str) -> &str {
    label.rsplit(':').next().unwrap_or(label)
}

// ---------------------------------------------------------------- GeoJSON

const GEOMETRIES: [&str; 7] = ["Point", "MultiPoint", "LineString", "MultiLineString", "Polygon", "MultiPolygon", "GeometryCollection"];

/// Whether a JSON header looks like GeoJSON: its `type` is a feature or geometry.
pub(crate) fn looks_like_geojson(header: &[u8]) -> bool {
    let head = String::from_utf8_lossy(&header[..header.len().min(4096)]);
    let compact: String = head.chars().filter(|c| !c.is_whitespace()).collect();
    // TopoJSON holds GeoJSON geometry types too.
    head.trim_start().starts_with('{')
        && !compact.contains("\"type\":\"Topology\"")
        && ["FeatureCollection", "Feature"].iter().chain(GEOMETRIES.iter()).any(|t| compact.contains(&format!("\"type\":\"{t}\"")))
}

/// The positions below `id` (arrays of two or more numbers), as (x, y).
fn positions(t: &DataTree, id: u32) -> Vec<(f64, f64)> {
    t.subtree(id)
        .into_iter()
        .filter(|&n| t.kind(n) == ValueKind::List)
        .filter_map(|n| {
            let c = t.child_ids(n);
            if c.len() < 2 || !c.iter().all(|&k| t.kind(k) == ValueKind::Number) {
                return None;
            }
            Some((number(t.value(c[0]))?, number(t.value(c[1]))?))
        })
        .collect()
}

pub(crate) fn geojson(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let tree = json::parse(src.bytes(), Dialect::Json, cx)?;
    let kind = tree.child(TOP, "type").map(|t| tree.value(t).to_string()).ok_or_else(|| ParseError::corrupt("GeoJSON: no `type`"))?;
    let rows: Arc<[u32]> = match kind.as_str() {
        "FeatureCollection" => tree.child(TOP, "features").map(|f| tree.child_ids(f).into()).unwrap_or_else(|| Arc::from([])),
        "Feature" => Arc::from([TOP]),
        k if GEOMETRIES.contains(&k) => Arc::from([TOP]),
        other => return Err(ParseError::corrupt(format!("GeoJSON: `type` is {other:?}"))),
    };
    let geometry = |t: &DataTree, row: u32| if t.child(row, "geometry").is_some() { t.child(row, "geometry") } else if t.child(row, "coordinates").is_some() || t.child(row, "geometries").is_some() { Some(row) } else { None };
    let mut bounds = Bounds::default();
    for &row in rows.iter() {
        if let Some(g) = geometry(&tree, row) {
            for (x, y) in positions(&tree, g) {
                bounds.add(x, y);
            }
        }
    }
    let tree = Arc::new(tree);
    let has_id = rows.iter().take(SAMPLE).any(|&r| tree.child(r, "id").is_some());
    let mut table = NodeTable::new(tree.clone(), rows);
    if has_id {
        table = table.column("ID", ValueKind::Mixed, |t, row| text(t, t.child(row, "id")));
    }
    let table = table
        .column("Geometry", ValueKind::Text, move |t, row| text(t, geometry(t, row).and_then(|g| t.child(g, "type"))))
        .column("Points", ValueKind::Number, move |t, row| match geometry(t, row) {
            Some(g) => Cell::Int(positions(t, g).len() as i64),
            None => Cell::Null,
        })
        .properties(|t, row| t.child(row, "properties").map(|p| t.child_ids(p).to_vec()).unwrap_or_default());
    Ok(features_document(FormatInfo::new("geojson", "GeoJSON", Category::Data), table, bounds, src, Some("json"), true))
}

// ---------------------------------------------------------------- TopoJSON

pub(crate) fn looks_like_topojson(header: &[u8]) -> bool {
    let compact: String = String::from_utf8_lossy(&header[..header.len().min(4096)]).chars().filter(|c| !c.is_whitespace()).collect();
    compact.starts_with('{') && compact.contains("\"type\":\"Topology\"")
}

pub(crate) fn topojson(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let tree = json::parse(src.bytes(), Dialect::Json, cx)?;
    if tree.child(TOP, "type").is_none_or(|t| tree.value(t) != "Topology") {
        return Err(ParseError::corrupt("TopoJSON: `type` isn't Topology"));
    }
    let objects = tree.child(TOP, "objects").ok_or_else(|| ParseError::corrupt("TopoJSON: no `objects`"))?;
    // Each object's geometries, or the object itself when it is one.
    let rows: Arc<[u32]> = tree
        .child_ids(objects)
        .iter()
        .flat_map(|&o| match tree.child(o, "geometries") {
            Some(g) => tree.child_ids(g).to_vec(),
            None => vec![o],
        })
        .collect();
    let mut bounds = Bounds::default();
    if let Some(b) = tree.child(TOP, "bbox") {
        let v: Vec<f64> = tree.child_ids(b).iter().filter_map(|&n| number(tree.value(n))).collect();
        if v.len() >= 4 {
            bounds.add(v[0], v[1]);
            bounds.add(v[2], v[3]);
        }
    }
    let arcs = tree.child(TOP, "arcs").map_or(0, |a| tree.child_ids(a).len());
    let tree = Arc::new(tree);
    let table = NodeTable::new(tree.clone(), rows)
        .column("Object", ValueKind::Text, move |t, row| {
            // The object a geometry is in: its parent's parent, or itself.
            let o = t.parent(row).filter(|&p| t.label(p) == "geometries").and_then(|p| t.parent(p)).unwrap_or(row);
            Cell::Text(t.label(o).to_string())
        })
        .column("ID", ValueKind::Mixed, |t, row| text(t, t.child(row, "id")))
        .column("Geometry", ValueKind::Text, |t, row| text(t, t.child(row, "type")))
        .properties(|t, row| t.child(row, "properties").map(|p| t.child_ids(p).to_vec()).unwrap_or_default());
    let doc = features_document(FormatInfo::new("topojson", "TopoJSON", Category::Data), table, bounds, src, Some("json"), true);
    Ok(doc.with_meta("Arcs", arcs))
}

// ---------------------------------------------------------------- KML

const KML_GEOMETRIES: [&str; 7] = ["Point", "LineString", "LinearRing", "Polygon", "MultiGeometry", "Track", "Model"];

/// The `lon,lat[,alt]` tuples of KML's `<coordinates>`, and `gx:coord`'s `lon lat alt`.
fn kml_points(t: &DataTree, id: u32) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    for n in t.subtree(id) {
        match local(t.label(n)) {
            "coordinates" => out.extend(t.value(n).split_whitespace().filter_map(|tuple| {
                let mut p = tuple.split(',');
                Some((number(p.next()?)?, number(p.next()?)?))
            })),
            "coord" => {
                let mut p = t.value(n).split_whitespace();
                if let (Some(x), Some(y)) = (p.next().and_then(number), p.next().and_then(number)) {
                    out.push((x, y));
                }
            }
            _ => {}
        }
    }
    out
}

pub(crate) fn looks_like_kml(header: &[u8]) -> bool {
    memchr::memmem::find(header, b"<kml").is_some()
}

fn kml_document(tree: DataTree, format: FormatInfo, src: &FileSource, lines: bool) -> Result<Document> {
    let rows: Arc<[u32]> = tree.all().into_iter().filter(|&n| tree.label(n) == "Placemark").collect();
    let mut bounds = Bounds::default();
    for &row in rows.iter() {
        for (x, y) in kml_points(&tree, row) {
            bounds.add(x, y);
        }
    }
    let tree = Arc::new(tree);
    let table = NodeTable::new(tree.clone(), rows)
        .column("Name", ValueKind::Text, |t, row| text(t, t.child(row, "name")))
        .column("Geometry", ValueKind::Text, |t, row| {
            match t.subtree(row).into_iter().skip(1).find(|&n| KML_GEOMETRIES.contains(&local(t.label(n)))) {
                Some(g) => Cell::Text(local(t.label(g)).to_string()),
                None => Cell::Null,
            }
        })
        .column("Points", ValueKind::Number, |t, row| Cell::Int(kml_points(t, row).len() as i64))
        .column("Folder", ValueKind::Text, |t, row| {
            let mut at = t.parent(row);
            while let Some(p) = at {
                if matches!(t.label(p), "Folder" | "Document") {
                    return text(t, t.child(p, "name"));
                }
                at = t.parent(p);
            }
            Cell::Null
        })
        .column("Description", ValueKind::Text, |t, row| text(t, t.child(row, "description")));
    // A column per ExtendedData name of the first placemarks.
    let mut names: Vec<String> = Vec::new();
    for &row in table.rows.iter().take(SAMPLE) {
        for (name, _) in kml_data(&table.tree, row) {
            if names.len() < MAX_PROPERTIES && !names.contains(&name) {
                names.push(name);
            }
        }
    }
    let mut table = table;
    for name in names {
        let key = name.clone();
        table = table.column(&name, ValueKind::Text, move |t, row| {
            kml_data(t, row).into_iter().find(|(k, _)| *k == key).map_or(Cell::Null, |(_, v)| Cell::Text(v))
        });
    }
    Ok(features_document(format, table, bounds, src, Some("xml"), lines))
}

/// A placemark's ExtendedData: `<Data name="k"><value>v</value></Data>` and
/// `<SimpleData name="k">v</SimpleData>`.
fn kml_data(t: &DataTree, row: u32) -> Vec<(String, String)> {
    let Some(ext) = t.child(row, "ExtendedData") else { return Vec::new() };
    t.subtree(ext)
        .into_iter()
        .filter_map(|el| {
            let name = t.child(el, "@name")?;
            let value = match t.label(el) {
                "Data" => t.child(el, "value").map(|v| t.value(v)).unwrap_or(""),
                "SimpleData" => t.value(el),
                _ => return None,
            };
            Some((t.value(name).to_string(), value.to_string()))
        })
        .collect()
}

pub(crate) fn kml(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let tree = crate::xml::parse(src.bytes(), cx)?;
    kml_document(tree, FormatInfo::new("kml", "KML", Category::Data), src, true)
}

/// KMZ: a zip holding `doc.kml` (or another `.kml`) and the files it uses.
pub(crate) fn kmz(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    use std::io::Read;
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(src.bytes())).map_err(|e| ParseError::corrupt(format!("KMZ: {e}")))?;
    let names: Vec<String> = zip.file_names().filter_map(|n| n.ok().map(|n| n.into_owned())).collect();
    let name = names
        .iter()
        .find(|n| n.eq_ignore_ascii_case("doc.kml"))
        .or_else(|| names.iter().find(|n| n.to_ascii_lowercase().ends_with(".kml")))
        .ok_or_else(|| ParseError::corrupt("KMZ: no .kml inside"))?
        .clone();
    let mut entry = zip.by_name(&name).map_err(|e| ParseError::corrupt(format!("KMZ: {e}")))?;
    let (packed, size) = (entry.compressed_size(), entry.size());
    cx.limits.check_unpack(packed, size)?;
    // The size a zip claims may lie: read no more than the limit allows.
    let mut kml = Vec::with_capacity(size.min(cx.limits.unpack_cap(packed)) as usize);
    entry.by_ref().take(cx.limits.unpack_cap(packed) + 1).read_to_end(&mut kml).map_err(|e| ParseError::corrupt(format!("KMZ: {e}")))?;
    cx.limits.check_unpack(packed, kml.len() as u64)?;
    let tree = crate::xml::parse(&kml, cx)?;
    // The KML's lines aren't the file's: no line links.
    Ok(kml_document(tree, FormatInfo::new("kmz", "KMZ (zipped KML)", Category::Data), src, false)?.with_meta("KML", name))
}

// ---------------------------------------------------------------- GPX

pub(crate) fn looks_like_gpx(header: &[u8]) -> bool {
    memchr::memmem::find(header, b"<gpx").is_some()
}

/// The points of a GPX waypoint, route or track: `lat`/`lon` attributes.
fn gpx_points(t: &DataTree, row: u32) -> Vec<u32> {
    t.subtree(row).into_iter().filter(|&n| matches!(t.label(n), "wpt" | "rtept" | "trkpt") && t.child(n, "@lat").is_some()).collect()
}

fn lat_lon(t: &DataTree, pt: u32) -> Option<(f64, f64)> {
    Some((number(t.value(t.child(pt, "@lon")?))?, number(t.value(t.child(pt, "@lat")?))?))
}

/// Great-circle distance in metres along `points` (lon, lat), over each track
/// segment separately.
fn distance(points: &[(f64, f64)]) -> f64 {
    const R: f64 = 6_371_008.8;
    points
        .windows(2)
        .map(|w| {
            let ((lon1, lat1), (lon2, lat2)) = (w[0], w[1]);
            let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
            let (dp, dl) = ((lat2 - lat1).to_radians(), (lon2 - lon1).to_radians());
            let a = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
            2.0 * R * a.sqrt().asin()
        })
        .sum()
}

pub(crate) fn gpx(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let tree = crate::xml::parse(src.bytes(), cx)?;
    let root = tree.roots().iter().copied().find(|&r| tree.label(r) == "gpx").ok_or_else(|| ParseError::corrupt("GPX: no <gpx>"))?;
    let rows: Arc<[u32]> = tree.child_ids(root).iter().copied().filter(|&n| matches!(tree.label(n), "wpt" | "rte" | "trk")).collect();
    let mut bounds = Bounds::default();
    for &row in rows.iter() {
        for p in gpx_points(&tree, row) {
            if let Some((x, y)) = lat_lon(&tree, p) {
                bounds.add(x, y);
            }
        }
    }
    let creator = tree.child(root, "@creator").map(|c| tree.value(c).to_string());
    let tree = Arc::new(tree);
    let time = |t: &DataTree, pts: &[u32], last: bool| {
        let pt = if last { pts.iter().rev().find(|&&p| t.child(p, "time").is_some()) } else { pts.iter().find(|&&p| t.child(p, "time").is_some()) };
        match pt.and_then(|&p| t.child(p, "time")) {
            Some(n) => Cell::Date(t.value(n).to_string()),
            None => Cell::Null,
        }
    };
    let table = NodeTable::new(tree.clone(), rows)
        .column("Type", ValueKind::Text, |t, row| {
            Cell::Text(match t.label(row) {
                "wpt" => "Waypoint",
                "rte" => "Route",
                _ => "Track",
            }
            .into())
        })
        .column("Name", ValueKind::Text, |t, row| text(t, t.child(row, "name")))
        .column("Points", ValueKind::Number, |t, row| Cell::Int(gpx_points(t, row).len() as i64))
        .column("Start", ValueKind::Date, move |t, row| time(t, &gpx_points(t, row), false))
        .column("End", ValueKind::Date, move |t, row| time(t, &gpx_points(t, row), true))
        .column("Distance (km)", ValueKind::Number, |t, row| {
            if t.label(row) == "wpt" {
                return Cell::Null;
            }
            // Each track segment on its own: the gap between segments isn't travelled.
            let segments: Vec<u32> = match t.label(row) {
                "trk" => t.child_ids(row).iter().copied().filter(|&s| t.label(s) == "trkseg").collect(),
                _ => vec![row],
            };
            let metres: f64 = segments.iter().map(|&s| distance(&gpx_points(t, s).iter().filter_map(|&p| lat_lon(t, p)).collect::<Vec<_>>())).sum();
            Cell::Float((metres / 10.0).round() / 100.0)
        })
        .column("Elevation (m)", ValueKind::Number, |t, row| match t.child(row, "ele") {
            Some(e) => number(t.value(e)).map_or(Cell::Null, Cell::Float),
            None => Cell::Null,
        })
        .column("Description", ValueKind::Text, |t, row| text(t, t.child(row, "desc")));
    let mut doc = features_document(FormatInfo::new("gpx", "GPX", Category::Data), table, bounds, src, Some("xml"), true);
    if let Some(c) = creator {
        doc = doc.with_meta("Recorded by", c);
    }
    Ok(doc)
}

// ---------------------------------------------------------------- GML

const GML_GEOMETRIES: [&str; 12] = [
    "Point",
    "LineString",
    "LinearRing",
    "Polygon",
    "MultiPoint",
    "MultiLineString",
    "MultiCurve",
    "MultiPolygon",
    "MultiSurface",
    "Curve",
    "Surface",
    "MultiGeometry",
];

/// Geography Markup Language: XML naming the GML namespace. (Graph Modelling
/// Language `.gml` files, `graph [ … ]`, are not this.)
pub(crate) fn looks_like_gml(header: &[u8]) -> bool {
    let head = header.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(header);
    head.iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'<') && memchr::memmem::find(head, b"opengis.net/gml").is_some()
}

/// How many positions a GML geometry holds: `pos`, `posList` (by its dimension),
/// `coordinates` tuples, `coord`.
fn gml_points(t: &DataTree, id: u32) -> usize {
    t.subtree(id)
        .into_iter()
        .map(|n| match local(t.label(n)) {
            "pos" | "coord" => 1,
            "posList" => {
                let dim = t.child(n, "@srsDimension").and_then(|d| t.value(d).parse::<usize>().ok()).unwrap_or(2).max(1);
                t.value(n).split_whitespace().count() / dim
            }
            "coordinates" => t.value(n).split_whitespace().count(),
            _ => 0,
        })
        .sum()
}

pub(crate) fn gml(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let tree = crate::xml::parse(src.bytes(), cx)?;
    // Features: what `featureMember` holds, or each child of `featureMembers`.
    let rows: Arc<[u32]> = tree
        .all()
        .into_iter()
        .filter(|&n| matches!(local(tree.label(n)), "featureMember" | "featureMembers" | "member"))
        .flat_map(|m| tree.child_ids(m).iter().copied().filter(|&c| !tree.label(c).starts_with(['@', '#'])).collect::<Vec<_>>())
        .collect();
    let tree = Arc::new(tree);
    let geometry = |t: &DataTree, row: u32| t.subtree(row).into_iter().skip(1).find(|&n| GML_GEOMETRIES.contains(&local(t.label(n))));
    let table = NodeTable::new(tree.clone(), rows)
        .column("Feature", ValueKind::Text, |t, row| Cell::Text(t.label(row).to_string()))
        .column("ID", ValueKind::Text, |t, row| text(t, t.child(row, "@gml:id").or_else(|| t.child(row, "@fid"))))
        .column("Geometry", ValueKind::Text, move |t, row| match geometry(t, row) {
            Some(g) => Cell::Text(local(t.label(g)).to_string()),
            None => Cell::Null,
        })
        .column("Points", ValueKind::Number, move |t, row| match geometry(t, row) {
            Some(g) => Cell::Int(gml_points(t, g) as i64),
            None => Cell::Null,
        })
        // Properties: the feature's own elements that hold only text.
        .properties(|t, row| {
            t.child_ids(row)
                .iter()
                .copied()
                .filter(|&c| !t.label(c).starts_with(['@', '#']) && !t.label(c).starts_with("gml:") && t.child_ids(c).is_empty())
                .collect()
        });
    // Axis order differs between GML versions and reference systems: no bounds.
    Ok(features_document(FormatInfo::new("gml", "GML", Category::Data), table, Bounds::default(), src, Some("xml"), true))
}

// ---------------------------------------------------------------- OpenStreetMap

pub(crate) fn looks_like_osm(header: &[u8]) -> bool {
    memchr::memmem::find(header, b"<osm").is_some()
}

/// Tag keys most worth a column: the commonest among the first elements.
const OSM_TAG_COLUMNS: usize = 24;

fn osm_tag(t: &DataTree, row: u32, key: &str) -> Option<u32> {
    t.child_ids(row).iter().copied().filter(|&c| t.label(c) == "tag").find(|&tag| t.child(tag, "@k").is_some_and(|k| t.value(k) == key)).and_then(|tag| t.child(tag, "@v"))
}

pub(crate) fn osm(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let tree = crate::xml::parse(src.bytes(), cx)?;
    let root = tree.roots().iter().copied().find(|&r| tree.label(r) == "osm").ok_or_else(|| ParseError::corrupt("OSM: no <osm>"))?;
    let rows: Arc<[u32]> = tree.child_ids(root).iter().copied().filter(|&n| matches!(tree.label(n), "node" | "way" | "relation")).collect();
    let mut bounds = Bounds::default();
    if let Some(b) = tree.child(root, "bounds") {
        let v = |k: &str| tree.child(b, k).and_then(|n| number(tree.value(n)));
        if let (Some(w), Some(s), Some(e), Some(n)) = (v("@minlon"), v("@minlat"), v("@maxlon"), v("@maxlat")) {
            bounds.add(w, s);
            bounds.add(e, n);
        }
    }
    if bounds.n == 0 {
        for &r in rows.iter() {
            if let Some((x, y)) = lat_lon(&tree, r) {
                bounds.add(x, y);
            }
        }
    }
    // The commonest tag keys of the first elements, name first.
    let mut counts: Vec<(String, usize)> = Vec::new();
    for &r in rows.iter().take(SAMPLE * 5) {
        for &tag in tree.child_ids(r).iter().filter(|&&c| tree.label(c) == "tag") {
            if let Some(k) = tree.child(tag, "@k").map(|k| tree.value(k)).filter(|k| *k != "name") {
                match counts.iter_mut().find(|(c, _)| c == k) {
                    Some((_, n)) => *n += 1,
                    None => counts.push((k.to_string(), 1)),
                }
            }
        }
    }
    counts.sort_by_key(|c| std::cmp::Reverse(c.1));
    counts.truncate(OSM_TAG_COLUMNS);
    let generator = tree.child(root, "@generator").map(|g| tree.value(g).to_string());
    let tree = Arc::new(tree);
    let mut table = NodeTable::new(tree.clone(), rows)
        .column("Kind", ValueKind::Text, |t, row| Cell::Text(t.label(row).to_string()))
        .column("ID", ValueKind::Number, |t, row| t.child(row, "@id").map_or(Cell::Null, |id| Cell::number(t.value(id))))
        .column("Name", ValueKind::Text, |t, row| text(t, osm_tag(t, row, "name")))
        .column("Latitude", ValueKind::Number, |t, row| t.child(row, "@lat").and_then(|v| number(t.value(v))).map_or(Cell::Null, Cell::Float))
        .column("Longitude", ValueKind::Number, |t, row| t.child(row, "@lon").and_then(|v| number(t.value(v))).map_or(Cell::Null, Cell::Float))
        .column("Members", ValueKind::Number, |t, row| match t.label(row) {
            "node" => Cell::Null,
            _ => Cell::Int(t.child_ids(row).iter().filter(|&&c| matches!(t.label(c), "nd" | "member")).count() as i64),
        })
        .column("Tags", ValueKind::Number, |t, row| Cell::Int(t.child_ids(row).iter().filter(|&&c| t.label(c) == "tag").count() as i64));
    for (key, _) in counts {
        let k = key.clone();
        table = table.column(&key, ValueKind::Text, move |t, row| text(t, osm_tag(t, row, &k)));
    }
    let mut doc = features_document(FormatInfo::new("osm", "OpenStreetMap XML", Category::Data), table, bounds, src, Some("xml"), true);
    if let Some(g) = generator {
        doc = doc.with_meta("Generator", g);
    }
    Ok(doc)
}

// ---------------------------------------------------------------- WKT

/// One geometry of a WKT file: its type, its points, the lines it is on.
struct Wkt {
    kind: String,
    points: Vec<(f64, f64)>,
    lines: Range<u64>,
}

/// Every geometry in WKT (or EWKT, `SRID=4326;…`) text, one after another.
fn wkt(text: &str, cx: &ParseContext) -> Result<Vec<Wkt>> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let line_of = |at: usize| memchr::memchr_iter(b'\n', &b[..at]).count() as u64;
    loop {
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b';' || b[i] == b',') {
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        let start = i;
        if text[i..].len() >= 5 && text[i..i + 5].eq_ignore_ascii_case("SRID=") {
            i = text[i..].find(';').map_or(b.len(), |p| i + p + 1);
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
        }
        let word_start = i;
        while i < b.len() && b[i].is_ascii_alphabetic() {
            i += 1;
        }
        let kind = text[word_start..i].to_ascii_uppercase();
        let known = ["POINT", "MULTIPOINT", "LINESTRING", "MULTILINESTRING", "POLYGON", "MULTIPOLYGON", "GEOMETRYCOLLECTION", "TRIANGLE", "TIN", "POLYHEDRALSURFACE", "CIRCULARSTRING", "COMPOUNDCURVE", "CURVEPOLYGON", "MULTICURVE", "MULTISURFACE"];
        if !known.contains(&kind.as_str()) {
            return Err(ParseError::corrupt_at(format!("WKT: expected a geometry, found {:?}", &text[word_start..i.max(word_start + 1).min(text.len())]), libfileparser_core::Position::in_text(b, word_start)));
        }
        // ` Z`, ` M`, ` ZM`, then `EMPTY` or a parenthesised list, nested.
        let mut points = Vec::new();
        let rest = text[i..].trim_start();
        i = b.len() - rest.len();
        let modifier = rest.split(|c: char| !c.is_ascii_alphabetic()).next().unwrap_or("");
        if matches!(modifier.to_ascii_uppercase().as_str(), "Z" | "M" | "ZM") {
            i += modifier.len();
        }
        let rest = text[i..].trim_start();
        i = b.len() - rest.len();
        if rest.len() >= 5 && rest[..5].eq_ignore_ascii_case("EMPTY") {
            i += 5;
        } else if rest.starts_with('(') {
            let mut depth = 0usize;
            let mut tuple = String::new();
            while i < b.len() {
                let c = b[i];
                i += 1;
                match c {
                    b'(' => {
                        depth += 1;
                        cx.limits.check(libfileparser_core::Limit::Depth, depth as u64)?;
                        tuple.clear();
                    }
                    b')' | b',' => {
                        let mut p = tuple.split_whitespace();
                        if let (Some(x), Some(y)) = (p.next().and_then(number), p.next().and_then(number)) {
                            points.push((x, y));
                            cx.limits.check(libfileparser_core::Limit::Geometry, points.len() as u64)?;
                        }
                        tuple.clear();
                        if c == b')' {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                    }
                    // A nested geometry's name in a collection.
                    c if c.is_ascii_alphabetic() => {}
                    c => tuple.push(c as char),
                }
            }
            if depth != 0 {
                return Err(ParseError::corrupt_at("WKT: a geometry that never closes", libfileparser_core::Position::in_text(b, start)));
            }
        } else {
            return Err(ParseError::corrupt_at("WKT: expected `(` or EMPTY", libfileparser_core::Position::in_text(b, i)));
        }
        out.push(Wkt { kind, points, lines: line_of(start)..line_of(i.saturating_sub(1)) + 1 });
        cx.limits.check(libfileparser_core::Limit::Rows, out.len() as u64)?;
    }
    if out.is_empty() {
        return Err(ParseError::corrupt("WKT: no geometry"));
    }
    Ok(out)
}

pub(crate) fn wkt_document(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let text = std::str::from_utf8(src.bytes()).map_err(|_| ParseError::corrupt("WKT: not UTF-8 text"))?;
    let geometries = wkt(text, cx)?;
    let mut bounds = Bounds::default();
    for g in &geometries {
        for &(x, y) in &g.points {
            bounds.add(x, y);
        }
    }
    let rows = geometries.iter().map(|g| vec![Cell::Text(g.kind.clone()), Cell::Int(g.points.len() as i64)]).collect();
    let table = OwnedTable::new(vec![Column::new("Geometry").with_kind(ValueKind::Text), Column::new("Points").with_kind(ValueKind::Number)], rows);
    let mut doc = Document::new(FormatInfo::new("wkt", "WKT", Category::Data))
        .with_facet(Role::Primary, "Features", Content::Table(Arc::new(table)))
        .with_meta("Features", geometries.len());
    if let Some(b) = bounds.text() {
        doc = doc.with_meta("Bounds", b);
    }
    Ok(doc.with_links(RowLines(geometries.into_iter().map(|g| g.lines).collect())).with_file_facets(src, Some("wkt")))
}

// ---------------------------------------------------------------- NMEA

/// An NMEA 0183 sentence's talker and type (`GPGGA`) and fields, and whether its
/// checksum holds (`None` without one).
fn nmea(line: &str) -> Option<(&str, Vec<&str>, Option<bool>)> {
    let body = line.trim().strip_prefix(['$', '!'])?;
    let (body, sum) = match body.rsplit_once('*') {
        Some((b, s)) if s.len() == 2 => (b, u8::from_str_radix(s, 16).ok()),
        _ => (body, None),
    };
    let mut fields = body.split(',');
    let id = fields.next()?;
    if id.len() < 5 || !id.bytes().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    let ok = sum.map(|s| body.bytes().fold(0u8, |a, c| a ^ c) == s);
    Some((id, fields.collect(), ok))
}

pub(crate) fn looks_like_nmea(header: &[u8]) -> bool {
    let text = String::from_utf8_lossy(header);
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).take(20).collect();
    !lines.is_empty() && lines.iter().filter(|l| nmea(l).is_some()).count() * 10 >= lines.len() * 8
}

/// `ddmm.mmmm` with its hemisphere, as signed degrees.
fn degrees(v: &str, hemisphere: &str, degree_digits: usize) -> Option<f64> {
    if v.len() < degree_digits + 2 || !v.is_ascii() {
        return None;
    }
    let d: f64 = v[..degree_digits].parse().ok()?;
    let m: f64 = v[degree_digits..].parse().ok()?;
    let x = d + m / 60.0;
    Some(if matches!(hemisphere, "S" | "W") { -x } else { x })
}

/// `hhmmss.ss` as `hh:mm:ss.ss`.
fn clock(v: &str) -> Option<String> {
    (v.len() >= 6 && v.is_ascii() && v[..6].bytes().all(|c| c.is_ascii_digit())).then(|| format!("{}:{}:{}", &v[..2], &v[2..4], &v[4..]))
}

/// What one sentence says: (time, date, lat, lon, altitude, speed, course, satellites, fix).
type Fix = (Option<String>, Option<String>, Option<f64>, Option<f64>, Option<f64>, Option<f64>, Option<f64>, Option<i64>, Option<String>);

fn fix(kind: &str, f: &[&str]) -> Fix {
    let get = |i: usize| f.get(i).copied().filter(|s| !s.is_empty());
    let num = |i: usize| get(i).and_then(number);
    match kind {
        // GGA: time, lat, N/S, lon, E/W, quality, satellites, hdop, altitude, M, …
        "GGA" => (
            get(0).and_then(clock),
            None,
            get(1).and_then(|v| degrees(v, get(2)?, 2)),
            get(3).and_then(|v| degrees(v, get(4)?, 3)),
            num(8),
            None,
            None,
            get(6).and_then(|s| s.parse().ok()),
            get(5).map(|q| match q {
                "0" => "none".to_string(),
                "1" => "GPS".to_string(),
                "2" => "DGPS".to_string(),
                "4" => "RTK fixed".to_string(),
                "5" => "RTK float".to_string(),
                other => other.to_string(),
            }),
        ),
        // RMC: time, status, lat, N/S, lon, E/W, speed (knots), course, date, …
        "RMC" => (
            get(0).and_then(clock),
            get(8).filter(|d| d.len() == 6).map(|d| format!("20{}-{}-{}", &d[4..6], &d[2..4], &d[..2])),
            get(2).and_then(|v| degrees(v, get(3)?, 2)),
            get(4).and_then(|v| degrees(v, get(5)?, 3)),
            None,
            num(6),
            num(7),
            None,
            get(1).map(|s| if s == "A" { "valid" } else { "void" }.to_string()),
        ),
        // GLL: lat, N/S, lon, E/W, time, status
        "GLL" => (
            get(4).and_then(clock),
            None,
            get(0).and_then(|v| degrees(v, get(1)?, 2)),
            get(2).and_then(|v| degrees(v, get(3)?, 3)),
            None,
            None,
            None,
            None,
            get(5).map(|s| if s == "A" { "valid" } else { "void" }.to_string()),
        ),
        _ => (None, None, None, None, None, None, None, None, None),
    }
}

/// An NMEA log as a table of sentences, read through the line index (a GPS log may
/// be growing).
struct NmeaTable {
    text: Arc<LineText>,
    /// The line of each sentence.
    sentences: Vec<u32>,
}

impl NmeaTable {
    const COLUMNS: [(&str, ValueKind); 11] = [
        ("Sentence", ValueKind::Text),
        ("Time", ValueKind::Date),
        ("Date", ValueKind::Date),
        ("Latitude", ValueKind::Number),
        ("Longitude", ValueKind::Number),
        ("Altitude (m)", ValueKind::Number),
        ("Speed (knots)", ValueKind::Number),
        ("Course", ValueKind::Number),
        ("Satellites", ValueKind::Number),
        ("Fix", ValueKind::Text),
        ("Checksum", ValueKind::Text),
    ];
}

impl TableSource for NmeaTable {
    fn info(&self) -> TableInfo {
        TableInfo::new(Self::COLUMNS.iter().map(|&(n, k)| Column::new(n).with_kind(k)).collect(), Count::Exact(self.sentences.len() as u64))
    }

    fn rows(&self, range: Range<u64>) -> Result<Vec<Vec<Cell>>> {
        let end = range.end.min(self.sentences.len() as u64) as usize;
        let start = (range.start as usize).min(end);
        let mut out = Vec::with_capacity(end - start);
        for &line in &self.sentences[start..end] {
            let text = self.text.lines(line as u64..line as u64 + 1)?;
            let text = text.first().map(|l| String::from_utf8_lossy(l).into_owned()).unwrap_or_default();
            let Some((id, fields, ok)) = nmea(&text) else { continue };
            let (time, date, lat, lon, alt, speed, course, sats, fix_) = fix(&id[id.len() - 3..], &fields);
            let f = |v: Option<f64>| v.map_or(Cell::Null, Cell::Float);
            out.push(vec![
                Cell::Text(id.to_string()),
                time.map_or(Cell::Null, Cell::Date),
                date.map_or(Cell::Null, Cell::Date),
                f(lat.map(|v| (v * 1e6).round() / 1e6)),
                f(lon.map(|v| (v * 1e6).round() / 1e6)),
                f(alt),
                f(speed),
                f(course),
                sats.map_or(Cell::Null, Cell::Int),
                fix_.map_or(Cell::Null, Cell::Text),
                match ok {
                    Some(true) => Cell::Text("ok".into()),
                    Some(false) => Cell::Error("checksum doesn't match".into()),
                    None => Cell::Null,
                },
            ]);
        }
        Ok(out)
    }

    fn memory(&self) -> MemoryUse {
        MemoryUse::heap(self.sentences.len() as u64 * 4) + self.text.memory()
    }
}

pub(crate) fn nmea_document(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let text = Arc::new(LineText::within(src.clone(), Some("nmea"), &cx.limits)?);
    let lines = text.info().lines.known();
    let mut sentences = Vec::new();
    let mut bounds = Bounds::default();
    let mut at = 0;
    while at < lines {
        cx.cancel.check()?;
        for (i, line) in text.lines(at..(at + 4096).min(lines))?.iter().enumerate() {
            let line = String::from_utf8_lossy(line);
            if let Some((id, fields, _)) = nmea(&line) {
                sentences.push((at + i as u64) as u32);
                if id.len() >= 5 {
                    let (_, _, lat, lon, ..) = fix(&id[id.len() - 3..], &fields);
                    if let (Some(lat), Some(lon)) = (lat, lon) {
                        bounds.add(lon, lat);
                    }
                }
            }
        }
        cx.limits.check(libfileparser_core::Limit::Rows, sentences.len() as u64)?;
        at += 4096;
    }
    if sentences.is_empty() {
        return Err(ParseError::NotThisFormat);
    }
    let row_lines: Vec<Range<u64>> = sentences.iter().map(|&l| l as u64..l as u64 + 1).collect();
    let count = sentences.len();
    let mut doc = Document::new(FormatInfo::new("nmea", "NMEA 0183", Category::Data))
        .with_facet(Role::Primary, "Sentences", Content::Table(Arc::new(NmeaTable { text, sentences })))
        .with_meta("Sentences", count);
    if let Some(b) = bounds.text() {
        doc = doc.with_meta("Bounds", b);
    }
    Ok(doc.with_links(RowLines(row_lines)).with_file_facets(src, Some("nmea")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nmea_sentences() {
        let (id, f, ok) = nmea("$GPGGA,123519,4807.038,N,01131.000,E,1,08,0.9,545.4,M,46.9,M,,*47").unwrap();
        assert_eq!((id, ok), ("GPGGA", Some(true)));
        let (time, _, lat, lon, alt, _, _, sats, q) = fix("GGA", &f);
        assert_eq!(time.as_deref(), Some("12:35:19"));
        assert!((lat.unwrap() - 48.1173).abs() < 1e-4 && (lon.unwrap() - 11.516_666).abs() < 1e-4);
        assert_eq!((alt, sats, q.as_deref()), (Some(545.4), Some(8), Some("GPS")));
        let (_, f, _) = nmea("$GPRMC,123519,A,4807.038,S,01131.000,W,022.4,084.4,230394,003.1,W*6A").unwrap();
        let (_, date, lat, lon, ..) = fix("RMC", &f);
        assert_eq!(date.as_deref(), Some("2094-03-23"));
        assert!(lat.unwrap() < 0.0 && lon.unwrap() < 0.0);
        assert_eq!(nmea("$GPGGA,1*00").unwrap().2, Some(false));
    }

    #[test]
    fn wkt_geometries() {
        let cx = ParseContext::new();
        let g = wkt("POINT (30 10)\nLINESTRING Z (30 10 1, 10 30 2, 40 40 3)\nSRID=4326;MULTIPOLYGON (((30 20, 45 40, 10 40, 30 20)), ((15 5, 40 10, 10 20, 5 10, 15 5)))\nPOINT EMPTY\nGEOMETRYCOLLECTION (POINT (4 6), LINESTRING (4 6, 7 10))", &cx).unwrap();
        let kinds: Vec<(&str, usize)> = g.iter().map(|g| (g.kind.as_str(), g.points.len())).collect();
        assert_eq!(kinds, [("POINT", 1), ("LINESTRING", 3), ("MULTIPOLYGON", 9), ("POINT", 0), ("GEOMETRYCOLLECTION", 3)]);
        assert_eq!(g[2].lines, 2..3);
        assert!(wkt("POINT (1 2", &cx).is_err());
        assert!(wkt("hello", &cx).is_err());
    }

    #[test]
    fn distances_and_bounds() {
        // One degree of latitude is about 111.2 km.
        let d = distance(&[(0.0, 0.0), (0.0, 1.0)]);
        assert!((d - 111_195.0).abs() < 50.0, "{d}");
        let mut b = Bounds::default();
        b.add(10.0, 50.5);
        b.add(-3.25, 40.0);
        assert_eq!(b.text().as_deref(), Some("-3.25, 40 to 10, 50.5"));
        assert!(looks_like_geojson(br#"{ "type" : "FeatureCollection", "features": [] }"#));
        assert!(!looks_like_geojson(br#"{"type": "module"}"#));
        assert!(looks_like_gml(br#"<?xml version="1.0"?><wfs:FeatureCollection xmlns:gml="http://www.opengis.net/gml">"#));
        assert!(!looks_like_gml(b"graph [ node [ id 1 ] ]"));
    }
}
