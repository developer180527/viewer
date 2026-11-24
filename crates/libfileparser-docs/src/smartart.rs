//! SmartArt in PowerPoint (PPTX): the text of a slide's diagrams, which office_oxide
//! doesn't read. A diagram's data part (`ppt/diagrams/dataN.xml`) holds its points and
//! which is whose parent; its text, in that tree, becomes a nested list. A data part
//! with no text of its own (only "[Text]" placeholders) leaves the text to the drawing
//! PowerPoint saved with it (`drawingN.xml`, its shapes as laid out), read in order, as
//! LibreOffice does.

use std::collections::HashMap;
use std::io::{Cursor, Read};

use libfileparser_core::{Block, Inline};
use quick_xml::events::Event;

/// Most bytes read of one part, and points of one diagram: past these it's not a
/// diagram anyone drew.
const PART: u64 = 16 << 20;
const POINTS: usize = 20_000;
const DEPTH: usize = 32;

/// Each slide's diagrams, in the deck's order, as blocks to follow its text.
pub(crate) fn diagrams(bytes: &[u8]) -> Vec<Vec<Block>> {
    let Ok(mut zip) = zip::ZipArchive::new(Cursor::new(bytes)) else { return Vec::new() };
    let mut part = |name: &str| -> Option<String> {
        let f = zip.by_name(name.trim_start_matches('/')).ok()?;
        let mut s = String::new();
        f.take(PART).read_to_string(&mut s).ok()?;
        Some(s)
    };
    let Some(presentation) = part("ppt/presentation.xml") else { return Vec::new() };
    let rels = part("ppt/_rels/presentation.xml.rels").map(|r| relationships(&r)).unwrap_or_default();
    let slides: Vec<String> = attributes(&presentation, "sldId", "id").into_iter().filter_map(|id| rels.iter().find(|r| r.id == id)).map(|r| resolve("ppt/presentation.xml", &r.target)).collect();
    slides
        .iter()
        .map(|slide| {
            let (dir, file) = slide.rsplit_once('/').unwrap_or(("", slide));
            let Some(rels) = part(&format!("{dir}/_rels/{file}.rels")).map(|r| relationships(&r)) else { return Vec::new() };
            let drawings: Vec<&Rel> = rels.iter().filter(|r| r.kind.ends_with("/diagramDrawing")).collect();
            rels.iter()
                .filter(|r| r.kind.ends_with("/diagramData"))
                .enumerate()
                .filter_map(|(n, data)| {
                    let from_data = part(&resolve(slide, &data.target)).and_then(|x| data_list(&x));
                    let list = from_data.or_else(|| drawings.get(n).and_then(|d| part(&resolve(slide, &d.target))).and_then(|x| drawing_list(&x)))?;
                    Some(list)
                })
                .collect()
        })
        .collect()
}

struct Rel {
    id: String,
    kind: String,
    target: String,
}

fn relationships(xml: &str) -> Vec<Rel> {
    let mut out = Vec::new();
    let mut r = quick_xml::Reader::from_str(xml);
    loop {
        match r.read_event() {
            Ok(Event::Start(e) | Event::Empty(e)) if e.local_name().as_ref() == b"Relationship" => {
                let get = |k: &[u8]| e.attributes().flatten().find(|a| a.key.local_name().as_ref() == k).map(|a| String::from_utf8_lossy(&a.value).into_owned()).unwrap_or_default();
                out.push(Rel { id: get(b"Id"), kind: get(b"Type"), target: get(b"Target") });
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

/// Every `attr` (by local name, so `r:id` is `id`) of elements named `tag`, in order.
fn attributes(xml: &str, tag: &str, attr: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut r = quick_xml::Reader::from_str(xml);
    loop {
        match r.read_event() {
            Ok(Event::Start(e) | Event::Empty(e)) if e.local_name().as_ref() == tag.as_bytes() => {
                // `r:id`, not the element's own numeric `id`.
                if let Some(a) = e.attributes().flatten().find(|a| a.key.local_name().as_ref() == attr.as_bytes() && a.key.prefix().is_some()) {
                    out.push(String::from_utf8_lossy(&a.value).into_owned());
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

/// A relationship's target as a path in the package, from the part that names it.
fn resolve(from: &str, target: &str) -> String {
    if let Some(abs) = target.strip_prefix('/') {
        return abs.to_string();
    }
    let mut parts: Vec<&str> = from.split('/').collect();
    parts.pop();
    for seg in target.split('/') {
        match seg {
            ".." => {
                parts.pop();
            }
            "." | "" => {}
            s => parts.push(s),
        }
    }
    parts.join("/")
}

struct Point {
    kind: String,
    text: Vec<String>,
}

/// The data model's text as a nested list, by its parent-of connections; `None` when
/// no point has text.
fn data_list(xml: &str) -> Option<Block> {
    let mut points: HashMap<String, Point> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    // parent → (order, child)
    let mut children: HashMap<String, Vec<(i64, String)>> = HashMap::new();
    let mut r = quick_xml::Reader::from_str(xml);
    let mut current: Option<String> = None;
    let (mut in_t, mut in_run, mut para) = (false, false, String::new());
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) if points.len() < POINTS => {
                let get = |k: &[u8]| e.attributes().flatten().find(|a| a.key.local_name().as_ref() == k).map(|a| String::from_utf8_lossy(&a.value).into_owned());
                match e.local_name().as_ref() {
                    b"pt" => {
                        let id = get(b"modelId").unwrap_or_default();
                        points.insert(id.clone(), Point { kind: get(b"type").unwrap_or_else(|| "node".into()), text: Vec::new() });
                        order.push(id.clone());
                        current = Some(id);
                    }
                    b"cxn" => {
                        if get(b"type").is_none_or(|t| t == "parOf") {
                            let ord = get(b"srcOrd").and_then(|o| o.parse().ok()).unwrap_or(0);
                            if let (Some(s), Some(d)) = (get(b"srcId"), get(b"destId")) {
                                children.entry(s).or_default().push((ord, d));
                            }
                        }
                    }
                    b"t" if current.is_some() && !in_t => in_t = true,
                    b"t" if in_t => in_run = true,
                    _ => {}
                }
            }
            Ok(Event::Text(t)) if in_run => para.push_str(&t.xml10_content().map(|c| c.into_owned()).unwrap_or_default()),
            Ok(Event::GeneralRef(g)) if in_run => para.push_str(&xml_entity(&g)),
            Ok(Event::End(e)) => match e.local_name().as_ref() {
                b"t" if in_run => in_run = false,
                b"p" if in_t => {
                    if let Some(p) = current.as_ref().and_then(|c| points.get_mut(c))
                        && !para.trim().is_empty()
                    {
                        p.text.push(para.trim().to_string());
                    }
                    para.clear();
                }
                b"t" if in_t => in_t = false,
                b"pt" => current = None,
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    let shown = |id: &str| points.get(id).is_some_and(|p| matches!(p.kind.as_str(), "node" | "asst"));
    if !order.iter().any(|id| shown(id) && points[id].text.iter().any(|t| !t.is_empty())) {
        return None;
    }
    for c in children.values_mut() {
        c.sort_by_key(|(o, _)| *o);
    }
    fn items(id: &str, points: &HashMap<String, Point>, children: &HashMap<String, Vec<(i64, String)>>, depth: usize, out: &mut Vec<Vec<Block>>) {
        if depth > DEPTH {
            return;
        }
        for (_, child) in children.get(id).map(Vec::as_slice).unwrap_or(&[]) {
            let Some(p) = points.get(child) else { continue };
            if !matches!(p.kind.as_str(), "node" | "asst") {
                continue;
            }
            let mut below = Vec::new();
            items(child, points, children, depth + 1, &mut below);
            if p.text.is_empty() {
                // A point with no text of its own: its children stand in its place.
                out.extend(below);
                continue;
            }
            let mut item: Vec<Block> = p.text.iter().map(|t| Block::Paragraph(vec![Inline::Text(t.clone())])).collect();
            if !below.is_empty() {
                item.push(Block::List { ordered: false, start: 1, items: below });
            }
            out.push(item);
        }
    }
    let root = order.iter().find(|id| points[*id].kind == "doc")?;
    let mut out = Vec::new();
    items(root, &points, &children, 0, &mut out);
    (!out.is_empty()).then_some(Block::List { ordered: false, start: 1, items: out })
}

/// The drawing's shapes' text, in order, as a list.
fn drawing_list(xml: &str) -> Option<Block> {
    let mut items = Vec::new();
    let mut r = quick_xml::Reader::from_str(xml);
    let (mut in_body, mut in_run) = (false, false);
    let (mut para, mut shape) = (String::new(), Vec::<String>::new());
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) => match e.local_name().as_ref() {
                b"txBody" => in_body = true,
                b"t" if in_body => in_run = true,
                _ => {}
            },
            Ok(Event::Text(t)) if in_run => para.push_str(&t.xml10_content().map(|c| c.into_owned()).unwrap_or_default()),
            Ok(Event::GeneralRef(g)) if in_run => para.push_str(&xml_entity(&g)),
            Ok(Event::End(e)) => match e.local_name().as_ref() {
                b"t" => in_run = false,
                b"p" if in_body => {
                    if !para.trim().is_empty() {
                        shape.push(para.trim().to_string());
                    }
                    para.clear();
                }
                b"txBody" => {
                    in_body = false;
                    if !shape.is_empty() && items.len() < POINTS {
                        items.push(std::mem::take(&mut shape).into_iter().map(|t| Block::Paragraph(vec![Inline::Text(t)])).collect());
                    }
                }
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    (!items.is_empty()).then_some(Block::List { ordered: false, start: 1, items })
}

/// `&amp;` and the like, and character references, as their characters.
fn xml_entity(g: &quick_xml::events::BytesRef) -> String {
    if let Ok(Some(c)) = g.resolve_char_ref() {
        return c.to_string();
    }
    let name: &[u8] = g.as_ref();
    match name {
        b"amp" => "&",
        b"lt" => "<",
        b"gt" => ">",
        b"quot" => "\"",
        b"apos" => "'",
        _ => "",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hierarchy_as_a_nested_list() {
        let xml = r#"<dgm:dataModel xmlns:dgm="d" xmlns:a="a"><dgm:ptLst>
            <dgm:pt modelId="0" type="doc"/>
            <dgm:pt modelId="1"><dgm:t><a:p><a:r><a:t>Parent &amp; one</a:t></a:r></a:p></dgm:t></dgm:pt>
            <dgm:pt modelId="2"><dgm:t><a:p><a:r><a:t>Child</a:t></a:r></a:p></dgm:t></dgm:pt>
            <dgm:pt modelId="3"><dgm:t><a:p><a:r><a:t>Second</a:t></a:r></a:p></dgm:t></dgm:pt>
            <dgm:pt modelId="9" type="pres"><dgm:t><a:p><a:r><a:t>layout</a:t></a:r></a:p></dgm:t></dgm:pt>
            </dgm:ptLst><dgm:cxnLst>
            <dgm:cxn modelId="c3" srcId="0" destId="3" srcOrd="1"/>
            <dgm:cxn modelId="c1" srcId="0" destId="1" srcOrd="0"/>
            <dgm:cxn modelId="c2" srcId="1" destId="2" srcOrd="0"/>
            <dgm:cxn modelId="c9" type="presOf" srcId="1" destId="9"/>
            </dgm:cxnLst></dgm:dataModel>"#;
        let s = format!("{:?}", data_list(xml).unwrap());
        let p = |t: &str| format!("Paragraph([Text(\"{t}\")])");
        assert_eq!(s, format!("List {{ ordered: false, start: 1, items: [[{}, List {{ ordered: false, start: 1, items: [[{}]] }}], [{}]] }}", p("Parent & one"), p("Child"), p("Second")));
    }

    #[test]
    fn placeholders_leave_the_text_to_the_drawing() {
        let data = r#"<dgm:dataModel xmlns:dgm="d"><dgm:ptLst><dgm:pt modelId="0" type="doc"/><dgm:pt modelId="1"><dgm:prSet phldrT="[Text]"/></dgm:pt></dgm:ptLst><dgm:cxnLst><dgm:cxn modelId="c" srcId="0" destId="1"/></dgm:cxnLst></dgm:dataModel>"#;
        assert!(data_list(data).is_none());
        let drawing = r#"<dsp:drawing xmlns:dsp="s" xmlns:a="a"><dsp:spTree><dsp:sp><dsp:txBody><a:p><a:r><a:t>one</a:t></a:r></a:p></dsp:txBody></dsp:sp><dsp:sp><dsp:txBody><a:p><a:r><a:t>two</a:t></a:r></a:p></dsp:txBody></dsp:sp><dsp:sp><dsp:spPr/></dsp:sp></dsp:spTree></dsp:drawing>"#;
        let s = format!("{:?}", drawing_list(drawing).unwrap());
        assert!(s.contains("\"one\"") && s.contains("\"two\"") && s.matches("Paragraph").count() == 2, "{s}");
    }

    #[test]
    fn paths_from_relationships() {
        assert_eq!(resolve("ppt/slides/slide1.xml", "../diagrams/data1.xml"), "ppt/diagrams/data1.xml");
        assert_eq!(resolve("ppt/presentation.xml", "slides/slide2.xml"), "ppt/slides/slide2.xml");
        assert_eq!(resolve("ppt/slides/slide1.xml", "/ppt/diagrams/d.xml"), "ppt/diagrams/d.xml");
    }
}
