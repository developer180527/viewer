//! XML Spreadsheet 2003 ("SpreadsheetML"): Excel 2002 and 2003's XML format, still
//! written by many reporting tools, often named `.xls`.
//!
//! ```xml
//! <Workbook xmlns="urn:schemas-microsoft-com:office:spreadsheet" xmlns:ss="…same…">
//!   <Worksheet ss:Name="Sales"><Table>
//!     <Row><Cell><Data ss:Type="String">Total</Data></Cell>
//!          <Cell ss:Index="3" ss:Formula="=SUM(R1C1:R1C2)"><Data ss:Type="Number">5</Data></Cell></Row>
//!   </Table></Worksheet>
//! </Workbook>
//! ```
//!
//! Rows and cells are placed one after another, or at an `ss:Index` (1-based); a cell
//! merged across `n` more columns moves the next one on by `n`. Names are matched
//! without their prefix, as writers vary in which they use. Styles are ignored.

use quick_xml::events::{BytesStart, Event};

/// What a cell holds.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Value {
    Number(f64),
    Text(String),
    Bool(bool),
    /// As written: `2024-03-01T00:00:00.000`.
    Date(String),
    /// `#N/A`, `#DIV/0!`…
    Error(String),
}

/// A cell, at its 0-based row and column.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Cell {
    pub row: u32,
    pub col: u32,
    pub value: Option<Value>,
    /// In R1C1 notation, as the format keeps it: `=SUM(R1C1:R1C2)`.
    pub formula: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default)]
#[non_exhaustive]
pub struct Sheet {
    pub name: String,
    pub cells: Vec<Cell>,
}

#[derive(Debug, Clone, PartialEq, Default)]
#[non_exhaustive]
pub struct Workbook {
    pub sheets: Vec<Sheet>,
    /// Named ranges: the name, and what it refers to.
    pub names: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    NotXmlss,
    Xml(String),
    TooManyCells(u64),
    /// More bytes of cell text than allowed.
    TooMuchText(u64),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotXmlss => write!(f, "not an XML spreadsheet"),
            Error::Xml(e) => write!(f, "XML: {e}"),
            Error::TooManyCells(max) => write!(f, "more than {max} cells"),
            Error::TooMuchText(max) => write!(f, "more than {max} bytes of cell text"),
        }
    }
}

impl std::error::Error for Error {}

const NAMESPACE: &[u8] = b"urn:schemas-microsoft-com:office:spreadsheet";

/// Whether these first bytes are an XML spreadsheet's: XML in its namespace with a
/// `Workbook` root.
pub fn detect(header: &[u8]) -> bool {
    let has = |s: &[u8]| header.windows(s.len()).any(|w| w == s);
    let xml = header.starts_with(b"<?xml") || header.starts_with(b"\xef\xbb\xbf<?xml");
    xml && has(NAMESPACE) && (has(b"<Workbook") || has(b":Workbook"))
}

/// What a cell's `ss:Type` says its text is.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Number,
    Text,
    Bool,
    Date,
    Error,
}

/// The cell being read.
struct Open {
    row: u32,
    col: u32,
    formula: Option<String>,
    kind: Option<Kind>,
    text: String,
    /// Rich text: HTML inside the Data, whose whitespace is HTML's, collapsed.
    rich: bool,
}

impl Open {
    fn value(&mut self) -> Option<Value> {
        let mut text = std::mem::take(&mut self.text);
        if self.rich {
            text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        }
        Some(match self.kind? {
            Kind::Number => text.trim().parse().map_or(Value::Text(text), Value::Number),
            Kind::Text => Value::Text(text),
            Kind::Bool => Value::Bool(matches!(text.trim(), "1" | "true" | "TRUE" | "True")),
            Kind::Date => Value::Date(text),
            Kind::Error => Value::Error(text),
        })
    }
}

/// An attribute by its name without a prefix (`ss:Index` and `Index` alike).
fn attr(e: &BytesStart<'_>, name: &[u8]) -> Option<String> {
    let a = e.attributes().flatten().find(|a| a.key.local_name().as_ref() == name)?;
    Some(a.normalized_value(quick_xml::XmlVersion::Implicit1_0).map_or_else(|_| String::from_utf8_lossy(&a.value).into_owned(), |v| v.into_owned()))
}

/// A 1-based position attribute, as 0-based.
fn index(e: &BytesStart<'_>, name: &[u8]) -> Option<u32> {
    attr(e, name)?.trim().parse::<u32>().ok().filter(|i| *i > 0).map(|i| i - 1)
}

fn number(e: &BytesStart<'_>, name: &[u8]) -> u32 {
    attr(e, name).and_then(|v| v.trim().parse().ok()).unwrap_or(0)
}

/// The text as UTF-8: a declared single-byte encoding (Latin-1, Windows-1252) or a
/// UTF-16 byte order mark converted, anything else taken as UTF-8.
fn utf8(xml: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    if let Some(rest) = xml.strip_prefix(b"\xff\xfe").or_else(|| xml.strip_prefix(b"\xfe\xff")) {
        let le = xml[0] == 0xff;
        let units = rest.as_chunks::<2>().0.iter().map(|c| if le { u16::from_le_bytes(*c) } else { u16::from_be_bytes(*c) });
        return char::decode_utf16(units).map(|c| c.unwrap_or('\u{fffd}')).collect::<String>().into_bytes().into();
    }
    let decl = &xml[..xml.len().min(200)];
    let decl = decl.split(|b| *b == b'>').next().unwrap_or(decl).to_ascii_lowercase();
    let says = |e: &[u8]| decl.windows(e.len()).any(|w| w == e);
    if !(says(b"iso-8859-1") || says(b"latin1") || says(b"windows-1252") || says(b"cp1252")) {
        return xml.into();
    }
    const CP1252: [char; 32] = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8d}', 'Ž', '\u{8f}', '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›',
        'œ', '\u{9d}', 'ž', 'Ÿ',
    ];
    // Latin-1's 0x80–0x9F are control characters no text has; read as Windows-1252.
    xml.iter().map(|&b| if (0x80..0xa0).contains(&b) { CP1252[(b - 0x80) as usize] } else { b as char }).collect::<String>().into_bytes().into()
}

/// Reads an XML spreadsheet's sheets: at most `max_cells` cells, `max_text` bytes of
/// their text.
pub fn read(xml: &[u8], max_cells: u64, max_text: u64) -> Result<Workbook, Error> {
    let xml = utf8(xml);
    let mut reader = quick_xml::Reader::from_reader(&*xml);
    let mut buf = Vec::new();
    let mut book = Workbook::default();
    let (mut cells, mut text) = (0u64, 0u64);
    let mut saw_root = false;
    let (mut row, mut next_row, mut next_col) = (0u32, 0u32, 0u32);
    let mut open: Option<Open> = None;
    // Inside a cell's Data; inside something whose text isn't the cell's (a comment).
    let (mut data, mut skip) = (0usize, 0usize);
    loop {
        let event = reader.read_event_into(&mut buf).map_err(|e| Error::Xml(e.to_string()))?;
        let empty = matches!(event, Event::Empty(_));
        match &event {
            Event::Start(e) | Event::Empty(e) => {
                let name = e.local_name();
                match name.as_ref() {
                    _ if skip > 0 => {
                        if !empty {
                            skip += 1;
                        }
                    }
                    b"Workbook" => saw_root = true,
                    b"Worksheet" if saw_root => {
                        book.sheets.push(Sheet { name: attr(e, b"Name").unwrap_or_else(|| format!("Sheet{}", book.sheets.len() + 1)), cells: Vec::new() });
                        (next_row, next_col) = (0, 0);
                    }
                    b"NamedRange" => {
                        if let (Some(n), Some(r)) = (attr(e, b"Name"), attr(e, b"RefersTo")) {
                            book.names.push((n, r));
                        }
                    }
                    b"Row" => {
                        row = index(e, b"Index").unwrap_or(next_row);
                        next_col = 0;
                        // A row repeated `Span` more times, empty.
                        next_row = row.saturating_add(1).saturating_add(number(e, b"Span"));
                    }
                    b"Cell" => {
                        let col = index(e, b"Index").unwrap_or(next_col);
                        next_col = col.saturating_add(1).saturating_add(number(e, b"MergeAcross"));
                        let c = Open { row, col, formula: attr(e, b"Formula"), kind: None, text: String::new(), rich: false };
                        if empty {
                            if c.formula.is_some() {
                                push(&mut book, c, None, &mut cells, max_cells)?;
                            }
                        } else {
                            open = Some(c);
                        }
                    }
                    b"Data" => {
                        if let Some(c) = &mut open {
                            c.kind = Some(match attr(e, b"Type").as_deref() {
                                Some("Number") => Kind::Number,
                                Some("Boolean") => Kind::Bool,
                                Some("DateTime") => Kind::Date,
                                Some("Error") => Kind::Error,
                                _ => Kind::Text,
                            });
                        }
                        if !empty {
                            data += 1;
                        }
                    }
                    // Rich text inside Data: its text is the cell's.
                    _ if data > 0 => {
                        if let Some(c) = &mut open {
                            c.rich = true;
                        }
                    }
                    // A comment's text, a nested element's, isn't the cell's.
                    _ if open.is_some() && !empty => skip = 1,
                    _ => {}
                }
            }
            Event::End(e) => {
                if skip > 0 {
                    skip -= 1;
                } else {
                    match e.local_name().as_ref() {
                        b"Data" => data = data.saturating_sub(1),
                        b"Cell" => {
                            if let Some(mut c) = open.take() {
                                let value = c.value();
                                if let Some(Value::Text(s) | Value::Date(s) | Value::Error(s)) = &value {
                                    text = text.saturating_add(s.len() as u64);
                                    if text > max_text {
                                        return Err(Error::TooMuchText(max_text));
                                    }
                                }
                                if value.is_some() || c.formula.is_some() {
                                    push(&mut book, c, value, &mut cells, max_cells)?;
                                }
                            }
                            data = 0;
                        }
                        _ => {}
                    }
                }
            }
            Event::Text(t) if data > 0 && skip == 0 => {
                if let Some(c) = &mut open {
                    c.text.push_str(&String::from_utf8_lossy(t.as_ref()));
                }
            }
            Event::CData(t) if data > 0 && skip == 0 => {
                if let Some(c) = &mut open {
                    c.text.push_str(&String::from_utf8_lossy(t.as_ref()));
                }
            }
            Event::GeneralRef(r) if data > 0 && skip == 0 => {
                if let Some(c) = &mut open {
                    match &*String::from_utf8_lossy(r.as_ref()) {
                        "amp" => c.text.push('&'),
                        "lt" => c.text.push('<'),
                        "gt" => c.text.push('>'),
                        "quot" => c.text.push('"'),
                        "apos" => c.text.push('\''),
                        n => {
                            let code = n.strip_prefix("#x").map(|h| u32::from_str_radix(h, 16)).or_else(|| n.strip_prefix('#').map(str::parse));
                            if let Some(ch) = code.and_then(|c| c.ok()).and_then(char::from_u32) {
                                c.text.push(ch);
                            }
                        }
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    if !saw_root {
        return Err(Error::NotXmlss);
    }
    Ok(book)
}

fn push(book: &mut Workbook, c: Open, value: Option<Value>, cells: &mut u64, max: u64) -> Result<(), Error> {
    *cells += 1;
    if *cells > max {
        return Err(Error::TooManyCells(max));
    }
    if let Some(sheet) = book.sheets.last_mut() {
        sheet.cells.push(Cell { row: c.row, col: c.col, value, formula: c.formula });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEAD: &str = r#"<?xml version="1.0"?><Workbook xmlns="urn:schemas-microsoft-com:office:spreadsheet" xmlns:ss="urn:schemas-microsoft-com:office:spreadsheet">"#;

    fn book(body: &str) -> Workbook {
        let xml = format!("{HEAD}{body}</Workbook>");
        assert!(detect(xml.as_bytes()));
        read(xml.as_bytes(), 100, 1000).unwrap()
    }

    fn values(s: &Sheet) -> Vec<(u32, u32, Option<Value>)> {
        s.cells.iter().map(|c| (c.row, c.col, c.value.clone())).collect()
    }

    #[test]
    fn cells_where_they_are_put() {
        let b = book(
            r#"<Names><NamedRange ss:Name="Top" ss:RefersTo="=Sales!R1C1"/></Names>
            <Worksheet ss:Name="Sales"><Table>
              <Row><Cell><Data ss:Type="String">a &amp; b</Data></Cell><Cell ss:MergeAcross="1"><Data ss:Type="Number">1.5</Data></Cell><Cell><Data ss:Type="Boolean">1</Data></Cell></Row>
              <Row ss:Index="4" ss:Span="1"><Cell ss:Index="3" ss:Formula="=R1C2*2"><Data ss:Type="Number">3</Data></Cell></Row>
              <Row><Cell><Data ss:Type="DateTime">2024-03-01T00:00:00.000</Data></Cell><Cell><Data ss:Type="Error">#N/A</Data></Cell><Cell ss:Formula="=NOW()"/><Cell/></Row>
            </Table></Worksheet>
            <Worksheet ss:Name="Two"><Table><Row><Cell><Data ss:Type="Number">x</Data></Cell></Row></Table></Worksheet>"#,
        );
        assert_eq!(b.names, [("Top".to_string(), "=Sales!R1C1".to_string())]);
        assert_eq!(b.sheets[0].name, "Sales");
        assert_eq!(values(&b.sheets[0]), [
            (0, 0, Some(Value::Text("a & b".into()))),
            (0, 1, Some(Value::Number(1.5))),
            (0, 3, Some(Value::Bool(true))),
            (3, 2, Some(Value::Number(3.0))),
            (5, 0, Some(Value::Date("2024-03-01T00:00:00.000".into()))),
            (5, 1, Some(Value::Error("#N/A".into()))),
            (5, 2, None),
        ]);
        assert_eq!(b.sheets[0].cells[3].formula.as_deref(), Some("=R1C2*2"));
        assert_eq!(b.sheets[0].cells[6].formula.as_deref(), Some("=NOW()"));
        // A number that isn't one is kept as text.
        assert_eq!(values(&b.sheets[1]), [(0, 0, Some(Value::Text("x".into())))]);
    }

    #[test]
    fn rich_text_is_the_cells_and_a_comment_isnt() {
        let b = book(
            r#"<Worksheet ss:Name="S"><Table><Row><Cell><ss:Data ss:Type="String" xmlns="http://www.w3.org/TR/REC-html40"><B>bo</B>ld&#x21;<![CDATA[<x>]]></ss:Data><Comment><ss:Data><B>note</B></ss:Data></Comment></Cell></Row></Table></Worksheet>"#,
        );
        assert_eq!(values(&b.sheets[0]), [(0, 0, Some(Value::Text("bold!<x>".into())))]);
        // Rich text's layout whitespace is HTML's; plain text's is the text's.
        let b = book("<Worksheet><Table><Row><Cell><Data ss:Type=\"String\">\n  <Font>Id\n   here</Font>\n</Data></Cell><Cell><Data ss:Type=\"String\">two\n  lines</Data></Cell></Row></Table></Worksheet>");
        assert_eq!(values(&b.sheets[0]), [(0, 0, Some(Value::Text("Id here".into()))), (0, 1, Some(Value::Text("two\n  lines".into())))]);
    }

    #[test]
    fn prefixed_and_latin_1() {
        let xml = b"<?xml version=\"1.0\" encoding=\"iso-8859-1\"?><ss:Workbook xmlns:ss=\"urn:schemas-microsoft-com:office:spreadsheet\"><ss:Worksheet ss:Name=\"S\"><ss:Table><ss:Row><ss:Cell><ss:Data ss:Type=\"String\">caf\xe9 \x80</ss:Data></ss:Cell></ss:Row></ss:Table></ss:Worksheet></ss:Workbook>";
        assert!(detect(xml));
        let b = read(xml, 10, 100).unwrap();
        assert_eq!(values(&b.sheets[0]), [(0, 0, Some(Value::Text("café €".into())))]);
        let utf16: Vec<u8> = [0xff, 0xfe].into_iter().chain(format!("{HEAD}<Worksheet/></Workbook>").encode_utf16().flat_map(u16::to_le_bytes)).collect();
        assert_eq!(read(&utf16, 10, 100).unwrap().sheets.len(), 1);
    }

    #[test]
    fn only_what_the_cells_data_holds() {
        let b = book(
            "<Worksheet ss:Name=\"S\"><Table>\n <Row>\n  <Cell>&amp;<![CDATA[c]]>\n   <Data ss:Type=\"String\">x</Data>\n  </Cell>\n  <Cell><Comment><Font><B>&amp;</B><Data>note</Data><I/><Data>more</Data></Font></Comment><Data ss:Type=\"String\">y</Data></Cell>\n  <Cell ss:Index=\"0\"><Data ss:Type=\"String\">z</Data></Cell>\n </Row>\n</Table></Worksheet>",
        );
        let got: Vec<_> = b.sheets.iter().map(|s| (s.name.as_str(), values(s))).collect();
        assert_eq!(got, [("S", vec![(0, 0, Some(Value::Text("x".into()))), (0, 1, Some(Value::Text("y".into()))), (0, 2, Some(Value::Text("z".into())))])]);
        // A worksheet before the root isn't the workbook's.
        let xml = format!("<?xml version=\"1.0\"?><x xmlns:ss=\"{}\"><ss:Worksheet ss:Name=\"no\"/><ss:Workbook><ss:Worksheet ss:Name=\"yes\"/></ss:Workbook></x>", std::str::from_utf8(NAMESPACE).unwrap());
        let names: Vec<_> = read(xml.as_bytes(), 10, 10).unwrap().sheets.into_iter().map(|s| s.name).collect();
        assert_eq!(names, ["yes"]);
    }

    #[test]
    fn detected_only_as_itself() {
        let long = |root: &str, ns: &str| format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><{root} xmlns=\"{ns}\"></{root}>");
        assert!(detect(long("Workbook", "urn:schemas-microsoft-com:office:spreadsheet").as_bytes()));
        assert!(detect(format!("\u{feff}{}", long("ss:Workbook", "urn:schemas-microsoft-com:office:spreadsheet")).as_bytes()));
        assert!(!detect(long("Styles", "urn:schemas-microsoft-com:office:spreadsheet").as_bytes()));
        assert!(!detect(long("Workbook", "urn:example:something-else-entirely-long-enough").as_bytes()));
        assert!(!detect(long("Workbook", "urn:schemas-microsoft-com:office:spreadsheet").trim_start_matches("<?xml").as_bytes()));
    }

    #[test]
    fn every_encoding_label() {
        let body = |enc: &str| format!("<?xml version=\"1.0\" encoding=\"{enc}\"?>{}<Worksheet><Table><Row><Cell><Data ss:Type=\"String\">\u{1}</Data></Cell></Row></Table></Worksheet></Workbook>", &HEAD[21..]).replace('\u{1}', "\u{fffd}");
        for enc in ["ISO-8859-1", "latin1", "windows-1252", "cp1252"] {
            let mut bytes = body(enc).into_bytes();
            let at = bytes.windows(3).position(|w| w == "\u{fffd}".as_bytes()).unwrap();
            bytes.splice(at..at + 3, [0xe9]);
            let b = read(&bytes, 10, 100).unwrap();
            assert_eq!(values(&b.sheets[0]), [(0, 0, Some(Value::Text("é".into())))], "{enc}");
        }
        // UTF-8, declared or not, is left as it is.
        let utf8 = format!(r#"{HEAD}<Worksheet><Table><Row><Cell><Data ss:Type="String">café</Data></Cell></Row></Table></Worksheet></Workbook>"#);
        assert_eq!(values(&read(utf8.as_bytes(), 10, 100).unwrap().sheets[0]), [(0, 0, Some(Value::Text("café".into())))]);
        let be: Vec<u8> = [0xfe, 0xff].into_iter().chain(format!("{HEAD}<Worksheet/></Workbook>").encode_utf16().flat_map(u16::to_be_bytes)).collect();
        assert_eq!(read(&be, 10, 100).unwrap().sheets.len(), 1);
    }

    #[test]
    fn limits_are_inclusive() {
        let two = format!(r#"{HEAD}<Worksheet><Table><Row><Cell><Data ss:Type="String">abc</Data></Cell><Cell><Data ss:Type="String">de</Data></Cell></Row></Table></Worksheet></Workbook>"#);
        assert!(read(two.as_bytes(), 2, 5).is_ok());
        assert_eq!(read(two.as_bytes(), 2, 4), Err(Error::TooMuchText(4)));
    }

    #[test]
    fn what_isnt_read() {
        assert!(!detect(b"<?xml version=\"1.0\"?><root/>"));
        assert_eq!(read(b"<?xml version=\"1.0\"?><root/>", 10, 10), Err(Error::NotXmlss));
        assert!(matches!(read(b"<?xml version=\"1.0\"?><Workbook><a></b></Workbook>", 10, 10), Err(Error::Xml(_))));
        let two = format!(r#"{HEAD}<Worksheet><Table><Row><Cell><Data ss:Type="Number">1</Data></Cell><Cell><Data ss:Type="String">abcdef</Data></Cell></Row></Table></Worksheet></Workbook>"#);
        assert_eq!(read(two.as_bytes(), 1, 100), Err(Error::TooManyCells(1)));
        assert_eq!(read(two.as_bytes(), 10, 3), Err(Error::TooMuchText(3)));
        assert_eq!(read(two.as_bytes(), 10, 100).unwrap().sheets[0].name, "Sheet1");
        assert_eq!(Error::NotXmlss.to_string(), "not an XML spreadsheet");
        assert_eq!(Error::TooManyCells(2).to_string(), "more than 2 cells");
        assert_eq!(Error::TooMuchText(2).to_string(), "more than 2 bytes of cell text");
        assert_eq!(Error::Xml("x".into()).to_string(), "XML: x");
    }
}
