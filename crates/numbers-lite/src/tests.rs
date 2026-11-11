use std::io::Write;

use super::proto::tests::Message;
use super::snappy::tests::chunk;
use super::*;

/// An archive: its info (identifier, one message's type and length), then the message.
fn archive(id: u64, kind: u32, body: &Message) -> Vec<u8> {
    let info = Message::default().number(1, id).bytes(2, &Message::default().number(1, kind as u64).number(3, body.0.len() as u64).0).0;
    [&Message::default().number(0, info.len() as u64).0[1..], &info, &body.0].concat()
}

fn zip_of(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(Cursor::new(&mut out));
        for (name, body) in entries {
            zip.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(body).unwrap();
        }
    }
    out
}

/// A decimal128 as Numbers lays it out.
fn decimal_bytes(whole: u128, exponent: i32, negative: bool) -> [u8; 16] {
    let mut b = [0u8; 16];
    b[..14].copy_from_slice(&whole.to_le_bytes()[..14]);
    let e = (exponent + 0x1820) as u32;
    b[14] = (whole >> 112) as u8 & 1 | ((e & 0x7f) as u8) << 1;
    b[15] = (e >> 7) as u8 | if negative { 0x80 } else { 0 };
    b
}

/// A version 5 cell: its type, then the fields its flags name, in order.
fn cell_bytes(kind: u8, fields: &[(u32, &[u8])]) -> Vec<u8> {
    let flags = fields.iter().fold(0, |a, f| a | f.0);
    let mut out = vec![5, kind, 0, 0, 0, 0, 0, 0];
    out.extend(flags.to_le_bytes());
    fields.iter().for_each(|f| out.extend_from_slice(f.1));
    out
}

/// A tile row: its cells one after another, and each column's offset.
fn row(index: u64, cells: &[(usize, Vec<u8>)], wide: bool) -> Message {
    let mut offsets = vec![0xffu8; 2 * 6];
    let mut buffer = Vec::new();
    for (col, cell) in cells {
        let at = if wide { buffer.len() / 4 } else { buffer.len() } as u16;
        offsets[2 * col..2 * col + 2].copy_from_slice(&at.to_le_bytes());
        buffer.extend_from_slice(cell);
        while wide && buffer.len() % 4 != 0 {
            buffer.push(0);
        }
    }
    Message::default().number(1, index).bytes(6, &buffer).bytes(7, &offsets).number(8, wide as u64)
}

/// A document of two sheets. "First" holds a chart (not a table) and "Totals", whose
/// rows are in two tiles, one of them wide; "Second" holds an empty table.
fn document() -> Vec<(&'static str, Vec<u8>)> {
    let number = |n: u128, e: i32| cell_bytes(2, &[(1, &decimal_bytes(n, e, false))]);
    let text = |key: u32| cell_bytes(3, &[(8, &key.to_le_bytes())]);
    let tile0 = Message::default()
        .bytes(5, &row(0, &[(0, text(1)), (2, text(2))], false).0)
        .bytes(5, &row(1, &[(0, number(15, -1)), (1, cell_bytes(6, &[(2, &1f64.to_le_bytes())])), (2, cell_bytes(9, &[(16, &7u32.to_le_bytes())]))], false).0)
        // A cell that is only a style, one pointing past the buffer, and a row kept
        // only in the old layout.
        .bytes(5, &row(2, &[(0, cell_bytes(0, &[(32, &[0; 4])]))], false).0)
        .bytes(5, &Message::default().number(1, 3).bytes(6, &[5]).bytes(7, &[9, 0]).0)
        .bytes(5, &Message::default().number(1, 4).bytes(3, &[1, 2, 3]).bytes(4, &[0, 0]).0);
    let tile1 = Message::default().bytes(5, &row(2, &[(1, cell_bytes(5, &[(4, &0f64.to_le_bytes())])), (3, cell_bytes(7, &[(2, &90f64.to_le_bytes())])), (5, cell_bytes(8, &[]))], true).0);
    let strings = Message::default().bytes(3, &Message::default().number(1, 1).bytes(3, b"Item").0).bytes(3, &Message::default().number(1, 2).bytes(3, "Caf\u{e9}".as_bytes()).0);
    let rich = Message::default().bytes(3, &Message::default().number(1, 7).reference(9, 50).0).bytes(3, &Message::default().number(1, 8).reference(9, 999).0);
    let tiles = Message::default().bytes(1, &Message::default().number(1, 0).reference(2, 40).0).bytes(1, &Message::default().number(1, 1).reference(2, 41).0).bytes(1, &Message::default().number(1, 2).reference(2, 999).0).number(2, 256);
    let store = Message::default().bytes(3, &tiles.0).reference(4, 30).reference(17, 31);
    let model = Message::default().bytes(4, &store.0).number(6, 259).number(7, 6).bytes(8, b"Totals").number(10, 1).number(11, 2);
    let main = [
        archive(1, DOCUMENT, &Message::default().reference(1, 2).reference(1, 3).reference(1, 999)),
        archive(2, SHEET, &Message::default().bytes(1, b"First").reference(2, 9).reference(2, 10)),
        archive(3, SHEET, &Message::default().bytes(1, b"Second").reference(2, 12)),
        // Drawables: a chart (not kept), a table, and a table whose model has no store.
        archive(9, 5021, &Message::default().bytes(1, b"chart")),
        archive(10, TABLE_INFO, &Message::default().reference(2, 11)),
        archive(11, TABLE_MODEL, &model),
        archive(12, TABLE_INFO, &Message::default().reference(2, 13)),
        archive(13, TABLE_MODEL, &Message::default().bytes(8, b"Empty")),
        // A second message for an identifier is a patch, not the object.
        archive(13, TABLE_MODEL, &Message::default().bytes(8, b"Patched")),
    ]
    .concat();
    let tables = [
        archive(30, DATA_LIST, &strings),
        archive(31, DATA_LIST, &rich),
        archive(40, TILE, &tile0),
        archive(41, TILE, &tile1),
        archive(50, RICH_TEXT, &Message::default().reference(1, 51)),
        archive(51, STORAGE, &Message::default().bytes(3, b"two\n").bytes(3, b"lines")),
    ]
    .concat();
    vec![("Index/Document.iwa", chunk(&main)), ("Index/Tables/Tile.iwa", chunk(&tables)), ("preview.jpg", vec![1, 2, 3])]
}

#[test]
fn a_document_of_sheets_tables_and_cells() {
    let book = read(&zip_of(&document()), &Limits::default()).unwrap();
    assert_eq!(book.unread_parts, 0);
    assert_eq!(book.sheets.iter().map(|s| (s.name.as_str(), s.tables.len())).collect::<Vec<_>>(), [("First", 1), ("Second", 1)]);
    let t = &book.sheets[0].tables[0];
    assert_eq!((t.name.as_str(), t.rows, t.columns, t.header_rows, t.header_columns, t.unread_rows), ("Totals", 259, 6, 1, 2, 1));
    let cells: Vec<_> = t.cells.iter().map(|c| (c.row, c.col, c.value.clone())).collect();
    assert_eq!(cells, [
        (0, 0, Value::Text("Item".into())),
        (0, 2, Value::Text("Caf\u{e9}".into())),
        (1, 0, Value::Number(1.5)),
        (1, 1, Value::Bool(true)),
        (1, 2, Value::Text("two\nlines".into())),
        // The second tile's rows follow the first's 256.
        (258, 1, Value::Date("2001-01-01T00:00:00".into())),
        (258, 3, Value::Duration(90.0)),
        (258, 5, Value::Error),
    ]);
    let empty = &book.sheets[1].tables[0];
    assert_eq!((empty.name.as_str(), empty.cells.len(), empty.rows), ("Empty", 0, 0));
}

/// Tiles needn't be 256 rows: a table says how many, and a size it couldn't mean is
/// taken as the usual one.
#[test]
fn rows_follow_the_tables_tile_size() {
    let build = |tile_rows: Option<u64>| {
        let cell = cell_bytes(6, &[(2, &1f64.to_le_bytes())]);
        let tile = Message::default().bytes(5, &row(1, &[(0, cell)], false).0);
        let mut tiles = Message::default().bytes(1, &Message::default().number(1, 3).reference(2, 40).0);
        if let Some(n) = tile_rows {
            tiles = tiles.number(2, n);
        }
        let model = Message::default().bytes(4, &Message::default().bytes(3, &tiles.0).0).bytes(8, b"T");
        let parts = [
            archive(1, DOCUMENT, &Message::default().reference(1, 2)),
            archive(2, SHEET, &Message::default().bytes(1, b"S").reference(2, 10)),
            archive(10, TABLE_INFO, &Message::default().reference(2, 11)),
            archive(11, TABLE_MODEL, &model),
            archive(40, TILE, &tile),
        ]
        .concat();
        let book = read(&zip_of(&[("Index/Document.iwa", chunk(&parts))]), &Limits::default()).unwrap();
        book.sheets[0].tables[0].cells[0].row
    };
    assert_eq!(build(Some(4)), 3 * 4 + 1);
    assert_eq!(build(Some(1 << 16)), 3 * 65_536 + 1);
    assert_eq!(build(Some(1)), 3 + 1);
    for odd in [None, Some(0), Some((1 << 16) + 1)] {
        assert_eq!(build(odd), 3 * 256 + 1, "{odd:?}");
    }
}

#[test]
fn the_ways_a_document_is_packed() {
    let parts = document();
    // Under a folder, as a zipped package is.
    let under: Vec<(String, Vec<u8>)> = parts.iter().map(|(n, b)| (format!("my.numbers/{n}"), b.clone())).collect();
    let under: Vec<(&str, Vec<u8>)> = under.iter().map(|(n, b)| (n.as_str(), b.clone())).collect();
    assert_eq!(read(&zip_of(&under), &Limits::default()).unwrap().sheets.len(), 2);
    // In an Index.zip of its own, as Numbers 3 wrote it; at the top or under a folder.
    let inner = zip_of(&parts);
    for name in ["Index.zip", "my.numbers/Index.zip"] {
        assert_eq!(read(&zip_of(&[(name, inner.clone()), ("preview.jpg", vec![1])]), &Limits::default()).unwrap().sheets.len(), 2, "{name}");
    }
    // An Index.zip inside an Index.zip isn't followed.
    assert_eq!(read(&zip_of(&[("Index.zip", zip_of(&[("Index.zip", inner)]))]), &Limits::default()), Err(Error::NotNumbers));
    // A part named like one but elsewhere isn't a part.
    assert_eq!(read(&zip_of(&[("Other/Document.iwa", parts[0].1.clone()), ("NotIndex/x.iwa", vec![])]), &Limits::default()), Err(Error::NotNumbers));
}

#[test]
fn what_isnt_read() {
    let limits = Limits::default();
    assert_eq!(read(b"not a zip", &limits), Err(Error::NotNumbers));
    assert!(matches!(read(b"PK\x03\x04 but cut short", &limits), Err(Error::Zip(_))));
    assert_eq!(read(&zip_of(&[("mimetype", b"x".to_vec())]), &limits), Err(Error::NotNumbers));
    for mark in [".iwpv2", "my.numbers/.iwpv2"] {
        let mut parts = document();
        parts.push((mark, vec![0; 8]));
        assert_eq!(read(&zip_of(&parts), &limits), Err(Error::Encrypted), "{mark}");
    }
    // A part that isn't Snappy is passed over and counted; with no document left,
    // that's damage.
    let mut parts = document();
    parts[1].1 = vec![9, 9, 9];
    let book = read(&zip_of(&parts), &limits).unwrap();
    assert_eq!((book.unread_parts, book.sheets[0].tables[0].cells.len()), (1, 0));
    parts[0].1 = vec![9, 9, 9];
    assert_eq!(read(&zip_of(&parts), &limits), Err(Error::Damaged("no document among its parts")));
}

#[test]
fn limits_hold() {
    let bytes = zip_of(&document());
    assert!(read(&bytes, &Limits::default().with_cells(8)).is_ok());
    assert_eq!(read(&bytes, &Limits::default().with_cells(7)), Err(Error::TooLarge { what: "cells", value: 8, max: 7 }));
    // "Item", "Café" (5 bytes), "two\nlines" and a date of 19.
    assert!(read(&bytes, &Limits::default().with_text(37)).is_ok());
    assert_eq!(read(&bytes, &Limits::default().with_text(36)), Err(Error::TooLarge { what: "bytes of cell text", value: 37, max: 36 }));
    // What unpacks is counted: each zipped part, then what it decompresses to. Every
    // cap short of the whole is refused as too large, never passed over as damage.
    let tight = |bytes: &[u8], cap: u64| read(bytes, &Limits::default().with_unpack(0, cap));
    let need = (1..4096).find(|cap| tight(&bytes, *cap).is_ok()).unwrap();
    let parts: u64 = document()[..2].iter().map(|p| p.1.len() as u64).sum();
    let unpacked: u64 = document()[..2].iter().map(|p| snappy::iwa(&p.1, 4096).unwrap().len() as u64).sum();
    assert_eq!(need, parts + unpacked);
    for cap in [1, parts / 2, parts, parts + 10, need - 1] {
        assert_eq!(tight(&bytes, cap), Err(Error::TooLarge { what: "bytes unpacked", value: cap + 1, max: cap }), "{cap}");
    }
    // Exactly enough is enough, a part of nothing after it too.
    let mut more = document();
    more.push(("Index/Zero.iwa", Vec::new()));
    assert!(tight(&zip_of(&more), need).is_ok());
    assert!(matches!(read(&bytes, &Limits::default().with_part_bytes(10)), Err(Error::TooLarge { .. })));
}

#[test]
fn numbers_as_decimals() {
    assert_eq!(decimal(&decimal_bytes(20215, -1, false)), 2021.5);
    assert_eq!(decimal(&decimal_bytes(1, -1, true)), -0.1);
    assert_eq!(decimal(&decimal_bytes(3, -2, false)), 0.03);
    assert_eq!(decimal(&decimal_bytes(7, 3, false)), 7000.0);
    // Where dividing by ten and multiplying by a tenth differ, and ten to the first.
    assert_eq!(decimal(&decimal_bytes(3, -1, false)), 0.3);
    assert_eq!(decimal(&decimal_bytes(3, 1, false)), 30.0);
    assert_eq!(decimal(&decimal_bytes(123, 0, false)), 123.0);
    assert_eq!(decimal(&decimal_bytes(0, 0, false)), 0.0);
    // All 113 bits of the whole number count.
    assert_eq!(decimal(&decimal_bytes(1 << 112, 0, false)), 2f64.powi(112));
    assert_eq!(decimal(&decimal_bytes((1 << 104) + (1 << 8), 0, false)), 2f64.powi(104) + 256.0);
}

#[test]
fn dates_from_seconds_since_2001() {
    assert_eq!(date(0.0).as_deref(), Some("2001-01-01T00:00:00"));
    assert_eq!(date(609_984_000.0).as_deref(), Some("2020-05-01T00:00:00"));
    assert_eq!(date(-1.0).as_deref(), Some("2000-12-31T23:59:59"));
    assert_eq!(date(86_399.9).as_deref(), Some("2001-01-01T23:59:59"));
    // A leap day, the days round a century that isn't one, and noon.
    assert_eq!(date(730_857_600.0 + 12.0 * 3600.0 + 5.0 * 60.0 + 9.0).as_deref(), Some("2024-02-29T12:05:09"));
    assert_eq!(date(-3_187_296_000.0).as_deref(), Some("1900-01-01T00:00:00"));
    assert_eq!(date(-3_182_198_400.0).as_deref(), Some("1900-03-01T00:00:00"));
    assert_eq!(date(-978_307_200.0).as_deref(), Some("1970-01-01T00:00:00"));
    // The last day of a 400-year cycle, and the day after.
    assert_eq!(date(-307.0 * 86_400.0).as_deref(), Some("2000-02-29T00:00:00"));
    assert_eq!(date(-306.0 * 86_400.0).as_deref(), Some("2000-03-01T00:00:00"));
    assert_eq!(date(-63_113_904_000.0).as_deref(), Some("0001-01-01T00:00:00"));
    assert_eq!(date(252_423_993_599.0).as_deref(), Some("9999-12-31T23:59:59"));
    assert_eq!(date(252_423_993_600.0), None);
    assert_eq!(date(-63_113_904_001.0), None);
    assert_eq!(date(f64::NAN), None);
}

#[test]
fn cells_field_by_field() {
    let mut texts = Texts::default();
    texts.strings.insert(3, "s".into());
    texts.rich.insert(4, "r".into());
    let d = decimal_bytes(25, -1, false);
    // Every field at once: each is found after the ones before it.
    let all = |kind| cell_bytes(kind, &[(1, &d), (2, &1f64.to_le_bytes()), (4, &86_400f64.to_le_bytes()), (8, &3u32.to_le_bytes()), (16, &4u32.to_le_bytes())]);
    assert_eq!(cell(&all(2), &texts), Some(Value::Number(2.5)));
    assert_eq!(cell(&all(10), &texts), Some(Value::Number(2.5)));
    assert_eq!(cell(&all(3), &texts), Some(Value::Text("s".into())));
    assert_eq!(cell(&all(5), &texts), Some(Value::Date("2001-01-02T00:00:00".into())));
    assert_eq!(cell(&all(6), &texts), Some(Value::Bool(true)));
    assert_eq!(cell(&all(7), &texts), Some(Value::Duration(1.0)));
    assert_eq!(cell(&all(8), &texts), Some(Value::Error));
    assert_eq!(cell(&all(9), &texts), Some(Value::Text("r".into())));
    assert_eq!(cell(&all(0), &texts), None);
    assert_eq!(cell(&all(4), &texts), None);
    // A number kept only as a double; false; a key with no text; a date out of range.
    assert_eq!(cell(&cell_bytes(2, &[(2, &4f64.to_le_bytes())]), &texts), Some(Value::Number(4.0)));
    assert_eq!(cell(&cell_bytes(6, &[(2, &0f64.to_le_bytes())]), &texts), Some(Value::Bool(false)));
    assert_eq!(cell(&cell_bytes(3, &[(8, &9u32.to_le_bytes())]), &texts), None);
    assert_eq!(cell(&cell_bytes(9, &[(16, &9u32.to_le_bytes())]), &texts), None);
    assert_eq!(cell(&cell_bytes(5, &[(4, &1e300f64.to_le_bytes())]), &texts), None);
    assert_eq!(cell(&cell_bytes(2, &[]), &texts), None);
    // Another version, and cells cut short at each point.
    let mut old = all(2);
    old[0] = 4;
    assert_eq!(cell(&old, &texts), None);
    assert_eq!(cell(&[], &texts), None);
    assert_eq!(cell(&[5], &texts), None);
    assert_eq!(cell(&all(2)[..11], &texts), None);
    assert_eq!(cell(&all(2)[..27], &texts), None);
    assert_eq!(cell(&all(3)[..43], &texts), None);
}

#[test]
fn archives_cut_short_keep_what_came_before() {
    let whole = [archive(1, SHEET, &Message::default().bytes(1, b"a")), archive(2, SHEET, &Message::default().bytes(1, b"b"))].concat();
    for cut in [whole.len() - 1, whole.len() / 2 + 1, whole.len() / 2 - 1, 1] {
        let mut objects = Objects::new();
        archives(&whole[..cut], &mut objects);
        assert_eq!(objects.len(), usize::from(cut > whole.len() / 2), "{cut}");
    }
    // An archive with no identifier, and one whose message has no length.
    let mut objects = Objects::new();
    let nameless = Message::default().bytes(2, &Message::default().number(1, SHEET as u64).number(3, 0).0).0;
    archives(&[&[nameless.len() as u8][..], &nameless].concat(), &mut objects);
    let no_length = Message::default().number(1, 5).bytes(2, &Message::default().number(1, SHEET as u64).0).0;
    archives(&[&[no_length.len() as u8][..], &no_length].concat(), &mut objects);
    assert!(objects.is_empty());
}

#[test]
fn errors_say_what_they_are() {
    assert_eq!(Error::NotNumbers.to_string(), "not a Numbers spreadsheet");
    assert_eq!(Error::Encrypted.to_string(), "protected by a password");
    assert_eq!(Error::Zip("x".into()).to_string(), "zip: x");
    assert_eq!(Error::Damaged("y").to_string(), "damaged: y");
    assert_eq!(Error::TooLarge { what: "cells", value: 3, max: 2 }.to_string(), "cells: 3, more than the 2 allowed");
}
