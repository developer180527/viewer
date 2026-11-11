//! XLSX workbooks calamine refuses whole for something small and valid, or harmless:
//!
//! - a sheet whose `state` isn't one of the three calamine knows (`state="show"`);
//! - an Excel 4 macro sheet, whose relationship type calamine doesn't accept though the
//!   part holds rows and cells as a worksheet's does;
//! - a sheet listed with no relationship (a Visual Basic module's entry);
//! - a sheet whose `<dimension>` is a range of whole rows or columns (`1:5`, `A:C`),
//!   which calamine can't parse and doesn't need.
//!
//! calamine gives no way round them, so a workbook that failed is mended and tried
//! again: the few XML parts concerned are rewritten, every other entry of the zip copied
//! as it is, compressed. A workbook that opens is never touched.

use std::collections::HashMap;
use std::io::{Cursor, Read, Write};
use std::ops::Range;

use libfileparser_core::Limits;

/// A mended workbook, and what was changed, for people.
pub struct Mended {
    pub bytes: Vec<u8>,
    pub notes: Vec<String>,
}

const WORKBOOK: &str = "xl/workbook.xml";
const RELS: &str = "xl/_rels/workbook.xml.rels";
const WORKSHEET: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet";

/// One attribute of a tag: where it is (with the space before it), its name, its value.
struct Attr {
    whole: Range<usize>,
    key: Range<usize>,
    value: Range<usize>,
}

/// The tags named `local` (with any prefix) in `xml`: start and empty-element tags, each
/// from its `<` to its `>`. A `>` inside a quoted value doesn't end one.
fn tags(xml: &str, local: &str) -> Vec<Range<usize>> {
    let b = xml.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(lt) = b[i..].iter().position(|c| *c == b'<').map(|p| i + p) {
        let name_end = b[lt + 1..].iter().position(|c| c.is_ascii_whitespace() || matches!(c, b'>' | b'/')).map_or(b.len(), |p| lt + 1 + p);
        let name = &xml[lt + 1..name_end];
        let mut end = name_end;
        let mut quote = None;
        while end < b.len() {
            match (quote, b[end]) {
                (None, b'>') => break,
                (None, q @ (b'"' | b'\'')) => quote = Some(q),
                (Some(q), c) if c == q => quote = None,
                _ => {}
            }
            end += 1;
        }
        let end = (end + 1).min(b.len());
        if name.rsplit(':').next() == Some(local) && !name.starts_with('/') {
            out.push(lt..end);
        }
        i = end.max(lt + 1);
    }
    out
}

/// A tag's attributes, as offsets into `xml`.
fn attrs(xml: &str, tag: Range<usize>) -> Vec<Attr> {
    let b = xml.as_bytes();
    let mut out = Vec::new();
    let mut i = tag.start;
    while i < tag.end && !b[i].is_ascii_whitespace() {
        i += 1;
    }
    loop {
        let whole = i;
        while i < tag.end && b[i].is_ascii_whitespace() {
            i += 1;
        }
        let key = i;
        while i < tag.end && !b[i].is_ascii_whitespace() && !matches!(b[i], b'=' | b'>' | b'/') {
            i += 1;
        }
        let key = key..i;
        while i < tag.end && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if key.is_empty() || i >= tag.end || b[i] != b'=' {
            return out;
        }
        i += 1;
        while i < tag.end && b[i].is_ascii_whitespace() {
            i += 1;
        }
        let Some(&q) = b.get(i).filter(|q| matches!(q, b'"' | b'\'')) else { return out };
        let Some(close) = b[i + 1..tag.end].iter().position(|c| *c == q).map(|p| i + 1 + p) else { return out };
        out.push(Attr { whole: whole..close + 1, key, value: i + 1..close });
        i = close + 1;
    }
}

/// `xml` without these ranges (in order, not overlapping).
fn without(xml: &str, cuts: &[Range<usize>]) -> String {
    let mut out = String::with_capacity(xml.len());
    let mut at = 0;
    for c in cuts {
        out.push_str(&xml[at..c.start]);
        at = c.end;
    }
    out.push_str(&xml[at..]);
    out
}

/// Where a `<sheet>` tag to be dropped ends: after its own closing tag when it isn't an
/// empty element.
fn element_end(xml: &str, tag: &Range<usize>) -> usize {
    if xml[tag.clone()].ends_with("/>") {
        return tag.end;
    }
    let rest = &xml[tag.end..];
    match rest.trim_start().strip_prefix("</") {
        Some(close) => close.find('>').map_or(tag.end, |p| xml.len() - close.len() + p + 1),
        None => tag.end,
    }
}

/// Whether a `<dimension ref>` names cells at both ends (`A1`, `A1:C5`): calamine wants a
/// column and a row in each.
fn cells(reference: &str) -> bool {
    reference.split(':').all(|c| c.bytes().any(|b| b.is_ascii_alphabetic()) && c.bytes().any(|b| b.is_ascii_digit()))
}

/// The relationships part: each id's type, and the part with macro sheets called
/// worksheets when it has any.
fn relationships(xml: &str, notes: &mut Vec<String>) -> (HashMap<String, (String, String)>, Option<String>) {
    let mut map = HashMap::new();
    let mut out = xml.to_string();
    let mut changed = false;
    // Back to front, so earlier offsets stay good as types are replaced.
    for tag in tags(xml, "Relationship").into_iter().rev() {
        let a = attrs(xml, tag);
        let get = |k: &str| a.iter().find(|a| &xml[a.key.clone()] == k);
        let (Some(id), Some(typ)) = (get("Id"), get("Type")) else { continue };
        let mut kind = xml[typ.value.clone()].to_string();
        if kind.ends_with("/xlMacrosheet") || kind.ends_with("/xlIntlMacrosheet") {
            out.replace_range(typ.value.clone(), WORKSHEET);
            kind = WORKSHEET.to_string();
            changed = true;
        }
        let target = get("Target").map(|t| xml[t.value.clone()].to_string()).unwrap_or_default();
        map.insert(xml[id.value.clone()].to_string(), (kind, target));
    }
    if changed {
        notes.push("Its macro sheets are read as worksheets.".into());
    }
    (map, changed.then_some(out))
}

/// The workbook part with what calamine refuses in its sheet list taken out: a `state` it
/// doesn't know, a sheet with no relationship.
fn workbook(xml: &str, rels: &HashMap<String, (String, String)>, notes: &mut Vec<String>) -> Option<String> {
    let mut cuts = Vec::new();
    let (mut states, mut dropped) = (0, Vec::new());
    for tag in tags(xml, "sheet") {
        let a = attrs(xml, tag.clone());
        let text = |r: &Range<usize>| &xml[r.clone()];
        let id = a.iter().find(|a| text(&a.key) == "id" || text(&a.key).ends_with(":id")).map(|a| text(&a.value)).unwrap_or("");
        if !rels.contains_key(id) {
            dropped.push(a.iter().find(|a| text(&a.key) == "name").map_or("?", |a| text(&a.value)).to_string());
            cuts.push(tag.start..element_end(xml, &tag));
            continue;
        }
        if let Some(state) = a.iter().find(|a| text(&a.key) == "state")
            && !matches!(text(&state.value), "visible" | "hidden" | "veryHidden")
        {
            states += 1;
            cuts.push(state.whole.clone());
        }
    }
    if states > 0 {
        notes.push(format!("{states} sheet(s) had a visibility Excel doesn't define; they're shown."));
    }
    if !dropped.is_empty() {
        notes.push(format!("Listed with nothing to read (a code module, usually), so left out: {}.", dropped.join(", ")));
    }
    (!cuts.is_empty()).then(|| without(xml, &cuts))
}

/// A worksheet part without a `<dimension>` calamine can't parse.
fn worksheet(xml: &str) -> Option<String> {
    // It comes before the cells; a part's first tags are enough to look through.
    let mut cut = xml.len().min(1 << 16);
    while !xml.is_char_boundary(cut) {
        cut -= 1;
    }
    let head = &xml[..cut];
    let tag = tags(head, "dimension").into_iter().next()?;
    let a = attrs(xml, tag.clone());
    let reference = a.iter().find(|a| &xml[a.key.clone()] == "ref").map(|a| &xml[a.value.clone()])?;
    (!cells(reference)).then(|| without(xml, std::slice::from_ref(&(tag.start..element_end(xml, &tag)))))
}

/// The workbook mended, or `None` when nothing here is wrong with it (or it isn't a zip
/// with a workbook where one usually is).
pub fn mend(bytes: &[u8], limits: &Limits) -> Option<Mended> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).ok()?;
    let part = |zip: &mut zip::ZipArchive<Cursor<&[u8]>>, name: &str| -> Option<String> {
        let f = zip.by_name(name).ok()?;
        if f.size() > limits.allocation {
            return None;
        }
        let mut s = String::new();
        f.take(limits.allocation).read_to_string(&mut s).ok()?;
        Some(s)
    };
    let mut notes = Vec::new();
    let mut new: HashMap<String, String> = HashMap::new();
    let rels_xml = part(&mut zip, RELS)?;
    let (rels, mended_rels) = relationships(&rels_xml, &mut notes);
    new.extend(mended_rels.map(|x| (RELS.to_string(), x)));
    let book = part(&mut zip, WORKBOOK)?;
    new.extend(workbook(&book, &rels, &mut notes).map(|x| (WORKBOOK.to_string(), x)));
    let mut ranges = 0;
    for (kind, target) in rels.values() {
        if kind != WORKSHEET {
            continue;
        }
        let name = target.strip_prefix('/').map_or_else(|| format!("xl/{target}"), str::to_string);
        if let Some(mended) = part(&mut zip, &name).and_then(|x| worksheet(&x)) {
            ranges += 1;
            new.insert(name, mended);
        }
    }
    if ranges > 0 {
        notes.push(format!("{ranges} sheet(s) gave their size as whole rows or columns; it's worked out from their cells."));
    }
    if new.is_empty() {
        return None;
    }
    let mut out = zip::ZipWriter::new(Cursor::new(Vec::with_capacity(bytes.len())));
    for i in 0..zip.len() {
        let entry = zip.by_index_raw(i).ok()?;
        let name = entry.name().ok()?.to_string();
        match new.get(&name) {
            Some(xml) => {
                drop(entry);
                out.start_file(name, zip::write::SimpleFileOptions::default()).ok()?;
                out.write_all(xml.as_bytes()).ok()?;
            }
            None => out.raw_copy_file(entry).ok()?,
        }
    }
    Some(Mended { bytes: out.finish().ok()?.into_inner(), notes })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_and_their_attributes() {
        let xml = r#"<a><x:sheet name="a>b" r:id = 'r1' state="show"/><sheets/><sheet/></a>"#;
        let found = tags(xml, "sheet");
        assert_eq!(found.iter().map(|t| &xml[t.clone()]).collect::<Vec<_>>(), [r#"<x:sheet name="a>b" r:id = 'r1' state="show"/>"#, "<sheet/>"]);
        let a = attrs(xml, found[0].clone());
        let got: Vec<_> = a.iter().map(|a| (&xml[a.key.clone()], &xml[a.value.clone()], &xml[a.whole.clone()])).collect();
        assert_eq!(got, [("name", "a>b", r#" name="a>b""#), ("r:id", "r1", " r:id = 'r1'"), ("state", "show", r#" state="show""#)]);
        assert!(attrs(xml, found[1].clone()).is_empty());
        assert!(tags("<sheet name=\"unclosed", "sheet").len() == 1 && tags("no tags", "sheet").is_empty());
    }

    #[test]
    fn the_sheet_list_is_mended() {
        let rels_xml = format!(
            r#"<Relationships><Relationship Id="r1" Type="{WORKSHEET}" Target="worksheets/sheet1.xml"/><Relationship Target="macrosheets/sheet1.xml" Type="http://schemas.microsoft.com/office/2006/relationships/xlMacrosheet" Id="r2"/><Relationship Id="r3" Type="http://x/theme" Target="theme/theme1.xml"/></Relationships>"#
        );
        let mut notes = Vec::new();
        let (rels, mended) = relationships(&rels_xml, &mut notes);
        assert_eq!(rels["r2"], (WORKSHEET.to_string(), "macrosheets/sheet1.xml".to_string()));
        assert_eq!(rels["r3"].0, "http://x/theme");
        let mended = mended.unwrap();
        assert!(!mended.contains("xlMacrosheet") && mended.matches(WORKSHEET).count() == 2 && mended.contains(r#"Id="r2""#), "{mended}");
        assert_eq!(notes.len(), 1);

        let book = r#"<workbook><sheets><sheet name="A" sheetId="1" state="show" r:id="r1"/><sheet name="Module" sheetId="2" r:id=""></sheet><sheet name="M" sheetId="3" state="hidden" r:id="r2"/></sheets></workbook>"#;
        let mut notes = Vec::new();
        let mended = workbook(book, &rels, &mut notes).unwrap();
        assert_eq!(mended, r#"<workbook><sheets><sheet name="A" sheetId="1" r:id="r1"/><sheet name="M" sheetId="3" state="hidden" r:id="r2"/></sheets></workbook>"#);
        assert!(notes.len() == 2 && notes[1].contains("Module"), "{notes:?}");
        // Nothing wrong: nothing done.
        let mut notes = Vec::new();
        assert!(workbook(r#"<sheets><sheet name="A" state="veryHidden" id="r1"/></sheets>"#, &rels, &mut notes).is_none() && notes.is_empty());
        let (_, same) = relationships(r#"<Relationships><Relationship Id="r1" Type="t" Target="x"/></Relationships>"#, &mut notes);
        assert!(same.is_none() && notes.is_empty());
    }

    #[test]
    fn a_dimension_of_whole_rows_or_columns_is_dropped() {
        assert!(cells("A1") && cells("A1:XFD1048576") && !cells("1:5") && !cells("A:C") && !cells("A1:5") && !cells(""));
        assert_eq!(worksheet(r#"<worksheet><dimension ref="1:5"/><sheetData/></worksheet>"#).as_deref(), Some("<worksheet><sheetData/></worksheet>"));
        assert_eq!(worksheet(r#"<worksheet><dimension ref="A:C"></dimension><sheetData/></worksheet>"#).as_deref(), Some("<worksheet><sheetData/></worksheet>"));
        assert!(worksheet(r#"<worksheet><dimension ref="A1:B2"/></worksheet>"#).is_none());
        assert!(worksheet("<worksheet><sheetData/></worksheet>").is_none());
        assert!(worksheet("<worksheet><dimension/></worksheet>").is_none());
    }
}
