//! BIFF, Excel's binary format before Excel 2007, read bare: the record stream with no
//! compound file around it. That is how Excel 2, 3 and 4 saved every file (BIFF2–4),
//! and how some programs write Excel 5 and 97 streams (BIFF5, BIFF8). An `.xls` from
//! Excel 97 or later is a compound file whose "Workbook" stream is this; open that with
//! a compound-file reader and hand its stream here, or use calamine.
//!
//! Every record is `id: u16, length: u16, body`. A substream runs from a BOF record to
//! its EOF; a worksheet's holds its cells. Only cell values are read: not formulas
//! (their results are), formats, widths or charts.

/// What a cell holds.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Value {
    Number(f64),
    Text(String),
    Bool(bool),
    /// `#N/A`, `#DIV/0!`…
    Error(String),
}

/// A cell, at its 0-based row and column.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Cell {
    pub row: u32,
    pub col: u32,
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Sheet {
    pub name: String,
    pub cells: Vec<Cell>,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Workbook {
    /// 2, 3, 4, 5 or 8.
    pub biff: u8,
    /// Worksheets and Excel 4 macro sheets, in the file's order.
    pub sheets: Vec<Sheet>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    NotBiff,
    /// A FILEPASS record: what follows it is encrypted.
    Encrypted,
    TooManyCells(u64),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotBiff => write!(f, "not a BIFF stream"),
            Error::Encrypted => write!(f, "the workbook is encrypted"),
            Error::TooManyCells(max) => write!(f, "more than {max} cells"),
        }
    }
}

impl std::error::Error for Error {}

const BOF2: u16 = 0x0009;
const BOF3: u16 = 0x0209;
const BOF4: u16 = 0x0409;
const BOF5: u16 = 0x0809;
const EOF: u16 = 0x000a;

/// Whether these are a bare BIFF stream's first bytes: a BOF record of a plausible size.
pub fn detect(header: &[u8]) -> bool {
    let Some(h) = header.get(..4) else { return false };
    let (id, len) = (u16::from_le_bytes([h[0], h[1]]), u16::from_le_bytes([h[2], h[3]]));
    matches!(id, BOF2 | BOF3 | BOF4 | BOF5) && (4..=20).contains(&len)
}

/// What a substream is: from its BOF record's type.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Globals,
    Sheet,
    /// A chart, a Visual Basic module, anything else: its records are passed over.
    Other,
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    b.get(at..at + 2).map(|s| u16::from_le_bytes([s[0], s[1]]))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn f64_at(b: &[u8], at: usize) -> Option<f64> {
    b.get(at..at + 8).map(|s| f64::from_le_bytes(s.try_into().unwrap()))
}

/// An RK number: 30 bits of an integer or of a double's top, perhaps hundredths.
fn rk(v: u32) -> f64 {
    let n = if v & 2 != 0 { ((v as i32) >> 2) as f64 } else { f64::from_bits(((v & !3) as u64) << 32) };
    if v & 1 != 0 { n / 100.0 } else { n }
}

fn error(code: u8) -> Value {
    Value::Error(
        match code {
            0x00 => "#NULL!",
            0x07 => "#DIV/0!",
            0x0f => "#VALUE!",
            0x17 => "#REF!",
            0x1d => "#NAME?",
            0x24 => "#NUM!",
            0x2a => "#N/A",
            _ => "#ERROR!",
        }
        .to_string(),
    )
}

fn bool_or_error(b: &[u8], at: usize) -> Option<Value> {
    let (v, is_error) = (*b.get(at)?, *b.get(at + 1)?);
    Some(if is_error != 0 { error(v) } else { Value::Bool(v != 0) })
}

/// Text in the workbook's code page.
struct Text {
    page: u16,
}

impl Text {
    fn bytes(&self, b: &[u8]) -> String {
        decode(b, match self.page {
            // BIFF's own numbers for Mac Roman and Windows-1252.
            0x8000 => 10000,
            0x8001 | 367 | 1200 => 1252,
            p => p as u32,
        })
    }

    /// A BIFF8 string's characters, `n` of them from `at`: one byte each, or with bit 0
    /// of `flags` two (UTF-16). Returns the text and where it ended.
    fn chars(&self, b: &[u8], at: usize, n: usize, flags: u8) -> (String, usize) {
        if flags & 1 != 0 {
            let at = at.min(b.len());
            let end = at + (2 * n).min(b.len() - at) / 2 * 2;
            let units = b[at..end].as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c));
            (char::decode_utf16(units).map(|c| c.unwrap_or('\u{fffd}')).collect(), end)
        } else {
            let at = at.min(b.len());
            let end = at + n.min(b.len() - at);
            // Compressed UTF-16: each byte its own character, Latin-1.
            (b[at..end].iter().map(|&c| c as char).collect(), end)
        }
    }

    /// A string at `at`: a length of `wide` (1 or 2) bytes, then the characters: bytes
    /// in the code page before BIFF8, a BIFF8 string after.
    fn string(&self, b: &[u8], at: usize, wide: usize, biff: u8) -> Option<String> {
        let n = if wide == 1 { *b.get(at)? as usize } else { u16_at(b, at)? as usize };
        let at = at + wide;
        if biff < 8 {
            let end = (at + n).min(b.len());
            return Some(self.bytes(b.get(at..end)?));
        }
        let flags = *b.get(at)?;
        let mut at = at + 1;
        if flags & 8 != 0 {
            at += 2;
        }
        if flags & 4 != 0 {
            at += 4;
        }
        Some(self.chars(b, at, n, flags).0)
    }
}

/// The shared strings of a BIFF8 stream (SST): those wholly in its first record. A
/// string carried on into a CONTINUE record is cut there.
fn shared_strings(b: &[u8], text: &Text) -> Vec<String> {
    let mut out = Vec::new();
    let Some(unique) = u32_at(b, 4) else { return out };
    let mut at = 8;
    while out.len() < unique as usize {
        let (Some(n), Some(&flags)) = (u16_at(b, at), b.get(at + 2)) else { break };
        at += 3;
        let runs = if flags & 8 != 0 { u16_at(b, at).inspect(|_| at += 2).unwrap_or(0) as usize } else { 0 };
        let ext = if flags & 4 != 0 { u32_at(b, at).inspect(|_| at += 4).unwrap_or(0) as usize } else { 0 };
        let (s, end) = text.chars(b, at, n as usize, flags);
        out.push(s);
        at = end.saturating_add(4 * runs).saturating_add(ext);
        if at >= b.len() {
            break;
        }
    }
    out
}

/// Reads a bare BIFF stream's sheets, at most `max_cells` cells in all.
pub fn read(bytes: &[u8], max_cells: u64) -> Result<Workbook, Error> {
    if !detect(bytes) {
        return Err(Error::NotBiff);
    }
    let biff = match u16_at(bytes, 0) {
        Some(BOF2) => 2,
        Some(BOF3) => 3,
        Some(BOF4) => 4,
        // BIFF5 and BIFF8 share a BOF id; its version says which.
        _ => {
            if u16_at(bytes, 4) == Some(0x0600) {
                8
            } else {
                5
            }
        }
    };
    let mut text = Text { page: 1252 };
    let (mut names, mut sst) = (Vec::<String>::new(), Vec::<String>::new());
    let (mut sheets, mut stack, mut sheet, mut substreams) = (Vec::new(), Vec::<Kind>::new(), None::<Sheet>, 0usize);
    let mut count = 0u64;
    // A formula whose result is text: the STRING record after it holds the text.
    let mut pending: Option<(u32, u32)> = None;
    let mut at = 0usize;
    while let Some(head) = bytes.get(at..at + 4) {
        let (id, len) = (u16::from_le_bytes([head[0], head[1]]), u16::from_le_bytes([head[2], head[3]]) as usize);
        let body = &bytes[at + 4..(at + 4 + len).min(bytes.len())];
        let first = at == 0;
        at += 4 + len;
        let in_sheet = stack.last() == Some(&Kind::Sheet);
        let mut cells: Vec<(u32, u32, Value)> = Vec::new();
        // The fixed part before a cell's value: row, column, and a format (three bytes
        // of attributes in BIFF2, an index after).
        let fixed = if biff == 2 { 7 } else { 6 };
        let rc = |b: &[u8]| Some((u16_at(b, 0)? as u32, u16_at(b, 2)? as u32));
        match id {
            BOF2 | BOF3 | BOF4 | BOF5 => {
                let kind = match u16_at(body, 2) {
                    Some(0x0005 | 0x0100) => Kind::Globals,
                    Some(0x0010 | 0x0040) => Kind::Sheet,
                    // A chart, a Visual Basic module.
                    Some(0x0006 | 0x0020) => Kind::Other,
                    // A stream's first BOF of a type no writer should use (some write
                    // 0x1000 for 0x0010), or none: a bare stream is a sheet.
                    _ if first => Kind::Sheet,
                    _ => Kind::Other,
                };
                // BOUNDSHEET names the top-level substreams in order, charts too; a chart
                // inside a sheet isn't one.
                if kind != Kind::Globals && stack.iter().all(|k| *k == Kind::Globals) {
                    substreams += 1;
                }
                if kind == Kind::Sheet && sheet.is_none() {
                    let name = names.get(substreams - 1).cloned().unwrap_or_else(|| format!("Sheet{}", sheets.len() + 1));
                    sheet = Some(Sheet { name, cells: Vec::new() });
                }
                stack.push(kind);
            }
            EOF => {
                if stack.pop() == Some(Kind::Sheet) && !stack.contains(&Kind::Sheet) {
                    sheets.extend(sheet.take());
                }
            }
            0x002f => return Err(Error::Encrypted),
            0x0042 => text.page = u16_at(body, 0).unwrap_or(1252),
            // BOUNDSHEET (BIFF5, 8): position, visibility, type, then the name.
            0x0085 => names.extend(text.string(body, 6, 1, biff)),
            // BUNDLESHEET (BIFF4 workbooks): position, flags, then the name.
            0x008f => names.extend(text.string(body, 6, 1, 4)),
            0x00fc => sst = shared_strings(body, &text),
            _ if !in_sheet => {}
            // Numbers: INTEGER (BIFF2), NUMBER, RK, MULRK.
            0x0002 => cells.extend(rc(body).zip(u16_at(body, 7)).map(|((r, c), v)| (r, c, Value::Number(v as f64)))),
            0x0003 | 0x0203 => cells.extend(rc(body).zip(f64_at(body, fixed)).map(|((r, c), v)| (r, c, Value::Number(v)))),
            0x027e => cells.extend(rc(body).zip(u32_at(body, 6)).map(|((r, c), v)| (r, c, Value::Number(rk(v))))),
            0x00bd => {
                // Row, first column, then a format and an RK number for each, then the
                // last column.
                if let (Some((r, from)), Some(runs)) = (rc(body), body.get(4..body.len().saturating_sub(2))) {
                    for (i, run) in runs.as_chunks::<6>().0.iter().enumerate() {
                        cells.push((r, from + i as u32, Value::Number(rk(u32::from_le_bytes([run[2], run[3], run[4], run[5]])))));
                    }
                }
            }
            // Text: LABEL, RSTRING, and LABELSST's index into the shared strings.
            0x0004 => cells.extend(rc(body).zip(text.string(body, 7, 1, biff)).map(|((r, c), s)| (r, c, Value::Text(s)))),
            0x0204 | 0x00d6 => cells.extend(rc(body).zip(text.string(body, 6, 2, biff)).map(|((r, c), s)| (r, c, Value::Text(s)))),
            0x00fd => cells.extend(rc(body).zip(u32_at(body, 6).and_then(|i| sst.get(i as usize).cloned())).map(|((r, c), s)| (r, c, Value::Text(s)))),
            // BOOLERR.
            0x0005 | 0x0205 => cells.extend(rc(body).zip(bool_or_error(body, fixed)).map(|((r, c), v)| (r, c, v))),
            // FORMULA (BIFF2 and 5/8, 3, 4): its last result. Bytes 6 and 7 of all ones
            // mean not a number: byte 0 says what.
            0x0006 | 0x0206 | 0x0406 => {
                if let (Some((r, c)), Some(res)) = (rc(body), body.get(fixed..fixed + 8)) {
                    if res[6..8] == [0xff, 0xff] {
                        match res[0] {
                            0 => pending = Some((r, c)),
                            1 => cells.push((r, c, Value::Bool(res[2] != 0))),
                            2 => cells.push((r, c, error(res[2]))),
                            _ => {}
                        }
                    } else {
                        cells.push((r, c, Value::Number(f64::from_le_bytes(res.try_into().unwrap()))));
                    }
                }
            }
            // STRING: the text a formula just before came to.
            0x0007 | 0x0207 => {
                if let Some((r, c)) = pending.take() {
                    let s = if id == 0x0007 { text.string(body, 0, 1, 2) } else { text.string(body, 0, 2, biff) };
                    cells.extend(s.map(|s| (r, c, Value::Text(s))));
                }
            }
            _ => {}
        }
        // Shared and array formulas (SHRFMLA, ARRAY, TABLE) may come between a formula
        // and its STRING.
        if !matches!(id, 0x0006 | 0x0206 | 0x0406 | 0x0007 | 0x0207 | 0x04bc | 0x00bc | 0x0021 | 0x0221 | 0x0036 | 0x0236) {
            pending = None;
        }
        if let Some(s) = sheet.as_mut() {
            for (row, col, value) in cells {
                count += 1;
                if count > max_cells {
                    return Err(Error::TooManyCells(max_cells));
                }
                s.cells.push(Cell { row, col, value });
            }
        }
    }
    // A stream cut short of its EOF: its sheet as far as it went.
    sheets.extend(sheet);
    Ok(Workbook { biff, sheets })
}

/// Bytes in a Windows code page. With the `encodings` feature every common one is read;
/// without it, Windows-1252, and others as Latin-1.
fn decode(bytes: &[u8], page: u32) -> String {
    #[cfg(feature = "encodings")]
    if let Some(e) = encoding(page) {
        return e.decode_without_bom_handling(bytes).0.into_owned();
    }
    bytes.iter().map(|&b| decode_byte(b, page)).collect()
}

#[cfg(feature = "encodings")]
fn encoding(page: u32) -> Option<&'static encoding_rs::Encoding> {
    use encoding_rs::*;
    // Windows-1252 is decode_byte's.
    Some(match page {
        866 => IBM866,
        874 => WINDOWS_874,
        932 => SHIFT_JIS,
        936 => GBK,
        949 => EUC_KR,
        950 => BIG5,
        1250 => WINDOWS_1250,
        1251 => WINDOWS_1251,
        1253 => WINDOWS_1253,
        1254 => WINDOWS_1254,
        1255 => WINDOWS_1255,
        1256 => WINDOWS_1256,
        1257 => WINDOWS_1257,
        1258 => WINDOWS_1258,
        10000 => MACINTOSH,
        65001 => UTF_8,
        _ => return None,
    })
}

/// A byte in Windows-1252; in other code pages, as Latin-1.
fn decode_byte(b: u8, page: u32) -> char {
    const CP1252: [char; 32] = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8d}', 'Ž', '\u{8f}', '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›',
        'œ', '\u{9d}', 'ž', 'Ÿ',
    ];
    if page == 1252 && (0x80..0xa0).contains(&b) { CP1252[(b - 0x80) as usize] } else { b as char }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(id: u16, body: &[u8]) -> Vec<u8> {
        [&id.to_le_bytes()[..], &(body.len() as u16).to_le_bytes(), body].concat()
    }

    /// Row and column, then `fixed - 4` bytes of format.
    fn at(r: u16, c: u16, fixed: usize) -> Vec<u8> {
        [&r.to_le_bytes()[..], &c.to_le_bytes(), &vec![0; fixed - 4]].concat()
    }

    fn values(sheet: &Sheet) -> Vec<(u32, u32, Value)> {
        sheet.cells.iter().map(|c| (c.row, c.col, c.value.clone())).collect()
    }

    #[test]
    fn an_excel_2_sheet() {
        let s = [
            rec(BOF2, &[2, 0, 0x10, 0]),
            rec(0x0042, &1252u16.to_le_bytes()),
            rec(0x0002, &[at(0, 0, 7), 7u16.to_le_bytes().to_vec()].concat()),
            rec(0x0003, &[at(0, 1, 7), 2.5f64.to_le_bytes().to_vec()].concat()),
            rec(0x0004, &[at(1, 0, 7), vec![3], b"a\x80b".to_vec()].concat()),
            rec(0x0005, &[at(1, 1, 7), vec![1, 0]].concat()),
            rec(0x0005, &[at(1, 2, 7), vec![0x07, 1]].concat()),
            // A formula whose result is text, then the text.
            rec(0x0006, &[at(2, 0, 7), vec![0, 0, 0, 0, 0, 0, 0xff, 0xff], vec![0; 3]].concat()),
            rec(0x0007, &[&[2][..], b"hi"].concat()),
            rec(0x0006, &[at(2, 1, 7), 4.0f64.to_le_bytes().to_vec(), vec![0; 3]].concat()),
            rec(0x0006, &[at(2, 2, 7), vec![1, 0, 1, 0, 0, 0, 0xff, 0xff]].concat()),
            rec(0x0006, &[at(2, 3, 7), vec![2, 0, 0x2a, 0, 0, 0, 0xff, 0xff]].concat()),
            rec(EOF, &[]),
        ]
        .concat();
        assert!(detect(&s));
        let w = read(&s, 100).unwrap();
        assert_eq!((w.biff, w.sheets.len(), w.sheets[0].name.as_str()), (2, 1, "Sheet1"));
        assert_eq!(values(&w.sheets[0]), [
            (0, 0, Value::Number(7.0)),
            (0, 1, Value::Number(2.5)),
            (1, 0, Value::Text("a€b".into())),
            (1, 1, Value::Bool(true)),
            (1, 2, Value::Error("#DIV/0!".into())),
            (2, 0, Value::Text("hi".into())),
            (2, 1, Value::Number(4.0)),
            (2, 2, Value::Bool(true)),
            (2, 3, Value::Error("#N/A".into())),
        ]);
    }

    #[test]
    fn rk_numbers() {
        assert_eq!(rk(5 << 2 | 2), 5.0);
        assert_eq!(rk(((-3i32) << 2) as u32 | 2), -3.0);
        assert_eq!(rk(1234 << 2 | 3), 12.34);
        let top = (1.5f64.to_bits() >> 32) as u32;
        assert_eq!(rk(top), 1.5);
        assert_eq!(rk(top | 1), 0.015);
    }

    #[test]
    fn an_excel_4_sheet_with_runs_of_numbers() {
        let mulrk = [at(3, 1, 4), vec![0, 0], (1u32 << 2 | 2).to_le_bytes().to_vec(), vec![0, 0], (2u32 << 2 | 2).to_le_bytes().to_vec(), 2u16.to_le_bytes().to_vec()].concat();
        let s = [
            rec(BOF4, &[0, 0, 0x10, 0, 0, 0]),
            rec(0x027e, &[at(0, 0, 6), (9u32 << 2 | 2).to_le_bytes().to_vec()].concat()),
            rec(0x0204, &[at(0, 1, 6), 2u16.to_le_bytes().to_vec(), b"ok".to_vec()].concat()),
            rec(0x0203, &[at(0, 2, 6), 0.5f64.to_le_bytes().to_vec()].concat()),
            rec(0x0205, &[at(0, 3, 6), vec![0, 0]].concat()),
            rec(0x00bd, &mulrk),
            // Its text after a shared formula.
            rec(0x0406, &[at(4, 0, 6), vec![0, 0, 0, 0, 0, 0, 0xff, 0xff]].concat()),
            rec(0x04bc, &[0; 8]),
            rec(0x0207, &[&2u16.to_le_bytes()[..], b"sf"].concat()),
            // A STRING with no formula before it is nobody's.
            rec(0x0207, &[&2u16.to_le_bytes()[..], b"no"].concat()),
            rec(EOF, &[]),
        ]
        .concat();
        let w = read(&s, 100).unwrap();
        assert_eq!(w.biff, 4);
        assert_eq!(values(&w.sheets[0]), [
            (0, 0, Value::Number(9.0)),
            (0, 1, Value::Text("ok".into())),
            (0, 2, Value::Number(0.5)),
            (0, 3, Value::Bool(false)),
            (3, 1, Value::Number(1.0)),
            (3, 2, Value::Number(2.0)),
            (4, 0, Value::Text("sf".into())),
        ]);
    }

    #[test]
    fn an_excel_5_workbook_names_its_sheets_and_passes_over_charts() {
        let bound = |name: &str| [&[0u8; 6][..], &[name.len() as u8], name.as_bytes()].concat();
        let cell = |v: f64| rec(0x0203, &[at(0, 0, 6), v.to_le_bytes().to_vec()].concat());
        let s = [
            rec(BOF5, &[0, 5, 0x05, 0, 0, 0, 0, 0]),
            rec(0x0085, &bound("Data")),
            rec(0x0085, &bound("Chart1")),
            rec(0x0085, &bound("More")),
            rec(EOF, &[]),
            rec(BOF5, &[0, 5, 0x10, 0, 0, 0, 0, 0]),
            cell(1.0),
            // A chart in the sheet: its records aren't the sheet's.
            rec(BOF5, &[0, 5, 0x20, 0, 0, 0, 0, 0]),
            cell(99.0),
            rec(EOF, &[]),
            rec(EOF, &[]),
            rec(BOF5, &[0, 5, 0x20, 0, 0, 0, 0, 0]),
            cell(98.0),
            rec(EOF, &[]),
            rec(BOF5, &[0, 5, 0x10, 0, 0, 0, 0, 0]),
            cell(2.0),
            // Cut short of its EOF.
        ]
        .concat();
        let w = read(&s, 100).unwrap();
        assert_eq!(w.biff, 5);
        let got: Vec<_> = w.sheets.iter().map(|s| (s.name.as_str(), values(s))).collect();
        assert_eq!(got, [("Data", vec![(0, 0, Value::Number(1.0))]), ("More", vec![(0, 0, Value::Number(2.0))])]);
    }

    #[test]
    fn an_excel_97_stream_with_shared_strings() {
        let utf16: Vec<u8> = "Ω!".encode_utf16().flat_map(u16::to_le_bytes).collect();
        let sst = [&3u32.to_le_bytes()[..], &2u32.to_le_bytes(), &[3, 0, 0], b"abc", &[2, 0, 1], &utf16].concat();
        let s = [
            rec(BOF5, &[0, 6, 0x10, 0, 0, 0, 0, 0]),
            rec(0x00fc, &sst),
            rec(0x00fd, &[at(0, 0, 6), 1u32.to_le_bytes().to_vec()].concat()),
            rec(0x00fd, &[at(0, 1, 6), 0u32.to_le_bytes().to_vec()].concat()),
            // An index past the table: no cell.
            rec(0x00fd, &[at(0, 2, 6), 7u32.to_le_bytes().to_vec()].concat()),
            rec(0x0204, &[at(1, 0, 6), 2u16.to_le_bytes().to_vec(), vec![1], utf16.clone()].concat()),
            rec(EOF, &[]),
        ]
        .concat();
        let w = read(&s, 100).unwrap();
        assert_eq!(w.biff, 8);
        assert_eq!(values(&w.sheets[0]), [(0, 0, Value::Text("Ω!".into())), (0, 1, Value::Text("abc".into())), (1, 0, Value::Text("Ω!".into()))]);
    }

    #[test]
    fn a_first_bof_of_an_unknown_type_is_a_sheet() {
        let cell = rec(0x0203, &[at(0, 0, 6), 1.0f64.to_le_bytes().to_vec()].concat());
        let s = [rec(BOF5, &[0, 0, 0, 0x10, 0, 0, 0, 0]), cell.clone(), rec(EOF, &[])].concat();
        assert_eq!(read(&s, 10).unwrap().sheets[0].cells.len(), 1);
        // A chart on its own is not.
        let chart = [rec(BOF5, &[0, 5, 0x20, 0, 0, 0, 0, 0]), cell, rec(EOF, &[])].concat();
        assert!(read(&chart, 10).unwrap().sheets.is_empty());
    }

    #[test]
    fn an_excel_4_workbook_with_its_sheet_names_and_code_page() {
        let bundle = |name: &str| [&[0u8; 6][..], &[name.len() as u8], name.as_bytes()].concat();
        let label = |c: u16, text: &[u8]| rec(0x0204, &[at(0, c, 6), (text.len() as u16).to_le_bytes().to_vec(), text.to_vec()].concat());
        let s = [
            rec(BOF4, &[0, 0, 0, 1, 0, 0]),
            rec(0x0042, &1251u16.to_le_bytes()),
            rec(0x008f, &bundle("First")),
            rec(0x008f, &bundle("Second")),
            rec(BOF4, &[0, 0, 0x10, 0, 0, 0]),
            label(0, b"\xc0"),
            rec(EOF, &[]),
            rec(BOF4, &[0, 0, 0x40, 0, 0, 0]),
            label(1, b"b"),
            rec(EOF, &[]),
            // A substream of a type no one uses, after the first: not a sheet.
            rec(BOF4, &[0, 0, 0x34, 0x12, 0, 0]),
            label(2, b"c"),
            rec(EOF, &[]),
            rec(EOF, &[]),
        ]
        .concat();
        let w = read(&s, 10).unwrap();
        assert_eq!(w.biff, 4);
        let got: Vec<_> = w.sheets.iter().map(|s| (s.name.as_str(), values(s))).collect();
        #[cfg(feature = "encodings")]
        let first = "А";
        #[cfg(not(feature = "encodings"))]
        let first = "À";
        assert_eq!(got, [("First", vec![(0, 0, Value::Text(first.into()))]), ("Second", vec![(0, 1, Value::Text("b".into()))])]);
        assert_eq!(read(&rec(BOF3, &[0, 0, 0x10, 0, 0, 0]), 1).unwrap().biff, 3);
    }

    #[test]
    fn strings_as_each_version_writes_them() {
        let t = Text { page: 1252 };
        // A one-byte length, then a two-byte one, each with a byte after the text.
        assert_eq!(t.string(&[2, b'a', b'b', b'z'], 0, 1, 5).as_deref(), Some("ab"));
        assert_eq!(t.string(&[2, 0, b'a', b'b', b'z'], 0, 2, 5).as_deref(), Some("ab"));
        // BIFF8: rich (a run count) and extended (a size) strings, the text after them.
        assert_eq!(t.string(&[2, 0, 0x08, 1, 0, b'h', b'i', 9, 9, 9, 9], 0, 2, 8).as_deref(), Some("hi"));
        assert_eq!(t.string(&[2, 0, 0x04, 4, 0, 0, 0, b'h', b'i', 9, 9, 9, 9], 0, 2, 8).as_deref(), Some("hi"));
        assert_eq!(t.string(&[2, 0, 0x0c, 1, 0, 4, 0, 0, 0, b'h', b'i'], 0, 2, 8).as_deref(), Some("hi"));
        // Cut short: what there is, from wherever it starts.
        assert_eq!(t.chars(&[9, b'O', 0, b'K', 0, b'!'], 1, 9, 1), ("OK".to_string(), 5));
        assert_eq!(t.chars(&[9, b'O', 0, b'K', 0], 1, 1, 1), ("O".to_string(), 3));
        assert_eq!(t.chars(&[9, b'a', b'b'], 1, 9, 0), ("ab".to_string(), 3));
        assert_eq!(t.chars(&[9, b'a', b'b', b'c'], 1, 2, 0), ("ab".to_string(), 3));
        assert_eq!(t.chars(&[1], 5, 2, 1), (String::new(), 1));
        assert_eq!(t.chars(&[b'a', 0, b'b', 0, b'c', 0, b'd', 0], 0, 3, 1), ("abc".to_string(), 6));
    }

    #[test]
    fn shared_strings_with_their_runs_and_extensions() {
        let sst = [
            &3u32.to_le_bytes()[..],
            &3u32.to_le_bytes(),
            // Rich: two runs of four bytes after the text.
            &[1, 0, 0x08, 2, 0],
            b"a",
            &[0; 8],
            // Extended: four bytes after the text.
            &[1, 0, 0x04, 4, 0, 0, 0],
            b"b",
            &[0; 4],
            &[1, 0, 0],
            b"c",
            // Past the count: not one of them.
            &[1, 0, 0],
            b"d",
        ]
        .concat();
        assert_eq!(shared_strings(&sst, &Text { page: 1252 }), ["a", "b", "c"]);
        assert!(shared_strings(&[0; 3], &Text { page: 1252 }).is_empty());
    }

    #[test]
    fn sheets_one_after_another() {
        let cell = |v: f64| rec(0x0203, &[at(0, 0, 6), v.to_le_bytes().to_vec()].concat());
        let sheet = |v: f64| [rec(BOF5, &[0, 6, 0x10, 0, 0, 0, 0, 0]), cell(v), rec(EOF, &[])].concat();
        let s = [rec(BOF5, &[0, 6, 0x05, 0, 0, 0, 0, 0]), rec(EOF, &[]), sheet(1.0), sheet(2.0)].concat();
        let w = read(&s, 10).unwrap();
        assert_eq!(w.sheets.iter().map(|s| s.cells.len()).collect::<Vec<_>>(), [1, 1]);
        assert_eq!(w.sheets[1].name, "Sheet2");
        // A sheet's BOF inside a sheet: still the one sheet, to its own EOF.
        let nested = [rec(BOF5, &[0, 6, 0x10, 0, 0, 0, 0, 0]), sheet(1.0), cell(2.0), rec(EOF, &[])].concat();
        let w = read(&nested, 10).unwrap();
        assert_eq!(w.sheets.iter().map(|s| s.cells.len()).collect::<Vec<_>>(), [2]);
    }

    #[cfg(feature = "encodings")]
    #[test]
    fn every_code_page_named() {
        let pages = [
            (866, "IBM866"),
            (874, "windows-874"),
            (932, "Shift_JIS"),
            (936, "GBK"),
            (949, "EUC-KR"),
            (950, "Big5"),
            (1250, "windows-1250"),
            (1251, "windows-1251"),
            (1253, "windows-1253"),
            (1254, "windows-1254"),
            (1255, "windows-1255"),
            (1256, "windows-1256"),
            (1257, "windows-1257"),
            (1258, "windows-1258"),
            (10000, "macintosh"),
            (65001, "UTF-8"),
        ];
        for (page, name) in pages {
            assert_eq!(encoding(page).map(|e| e.name()), Some(name), "{page}");
        }
        assert!(encoding(1252).is_none() && encoding(437).is_none());
    }

    #[test]
    fn what_isnt_read() {
        let bof = rec(BOF3, &[0, 0, 0x10, 0, 0, 0]);
        assert_eq!(read(b"nope", 10), Err(Error::NotBiff));
        assert!(!detect(&rec(BOF3, &[0; 40])));
        assert_eq!(read(&[bof.clone(), rec(0x002f, &[0; 6])].concat(), 10), Err(Error::Encrypted));
        let two = [bof, rec(0x0203, &[at(0, 0, 6), vec![0; 8]].concat()), rec(0x0203, &[at(0, 1, 6), vec![0; 8]].concat())].concat();
        assert_eq!(read(&two, 1), Err(Error::TooManyCells(1)));
        assert_eq!(read(&two, 2).unwrap().sheets[0].cells.len(), 2);
        assert_eq!(Error::NotBiff.to_string(), "not a BIFF stream");
        assert_eq!(Error::Encrypted.to_string(), "the workbook is encrypted");
        assert_eq!(Error::TooManyCells(3).to_string(), "more than 3 cells");
    }

    #[test]
    fn every_error_code_is_named() {
        let names: Vec<_> = [0x00, 0x07, 0x0f, 0x17, 0x1d, 0x24, 0x2a, 0x99].into_iter().map(error).collect();
        let want = ["#NULL!", "#DIV/0!", "#VALUE!", "#REF!", "#NAME?", "#NUM!", "#N/A", "#ERROR!"];
        assert_eq!(names, want.map(|s| Value::Error(s.into())));
    }

    #[test]
    fn text_in_the_workbooks_code_page() {
        let t = |page: u16| Text { page }.bytes(&[0xc7, 0x80]);
        assert_eq!(t(0x8001), "Ç€");
        #[cfg(feature = "encodings")]
        {
            assert_eq!(t(1256), "ا€");
            assert_eq!(Text { page: 0x8000 }.bytes(&[0x80]), "Ä");
        }
        #[cfg(not(feature = "encodings"))]
        assert_eq!(t(1256), "Ç\u{80}");
    }
}
