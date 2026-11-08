//! Every sheets reader passes the conformance suite (docs/design/libfileparser.md §9).

use std::io::Write;

use libfileparser_core::{Cell, Content, FileSource, Limits, ParseContext, ParseError, Reader, Result};
use libfileparser_sheets::{DifReader, FodsReader, OdsReader, SylkReader, XlsReader, XlsbReader, XlsxReader, readers};
use libfileparser_testkit::{Sample, assert_conforms};

fn sample(name: &str) -> Vec<u8> {
    std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/files/sheets").join(name)).unwrap()
}

#[test]
fn every_reader_conforms() {
    assert_conforms(&XlsxReader, &[Sample::new("sample.xlsx", sample("sample.xlsx")).fits_tiny_limits()]);
    assert_conforms(&XlsbReader, &[Sample::new("sample.xlsb", sample("sample.xlsb")).fits_tiny_limits()]);
    assert_conforms(&XlsReader, &[Sample::new("sample.xls", sample("sample.xls")).fits_tiny_limits()]);
    assert_conforms(&OdsReader, &[Sample::new("sample.ods", sample("sample.ods")).fits_tiny_limits()]);
    assert_conforms(&FodsReader, &[Sample::new("sample.fods", sample("sample.fods")).fits_tiny_limits()]);
    assert_conforms(&SylkReader, &[Sample::new("sample.slk", sample("sample.slk")).fits_tiny_limits()]);
    assert_conforms(&DifReader, &[Sample::new("sample.dif", sample("sample.dif")).fits_tiny_limits()]);
    assert_eq!(readers().len(), 7);
}

/// A minimal XLSX with these rows of the first sheet's `<sheetData>`.
fn xlsx(sheet_data: &str) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut out));
        let mut add = |name: &str, body: &str| {
            zip.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        };
        add("[Content_Types].xml", r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/></Types>"#);
        add("_rels/.rels", r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#);
        add("xl/workbook.xml", r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets></workbook>"#);
        add("xl/_rels/workbook.xml.rels", r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#);
        add("xl/worksheets/sheet1.xml", &format!(r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>{sheet_data}</sheetData></worksheet>"#));
    }
    out
}

fn open(bytes: Vec<u8>) -> Result<(u64, usize, Vec<Vec<Cell>>)> {
    let cx = ParseContext::new();
    let doc = XlsxReader.open(&FileSource::from_bytes("a.xlsx", bytes), &cx)?;
    let Content::Table(t) = doc.facet(0, &cx)? else { panic!("a table") };
    let info = t.info();
    let (rows, width) = (info.rows.known(), info.columns.len());
    Ok((rows, width, t.rows(0..3)?))
}

#[test]
fn a_tiny_file_claiming_the_last_cell_stays_tiny() {
    // calamine's dense layout would ask for hundreds of gigabytes here.
    let (rows, width, first) = open(xlsx(r#"<row r="1"><c r="A1"><v>1.5</v></c></row><row r="1048576"><c r="XFD1048576"><v>2</v></c></row>"#)).unwrap();
    assert_eq!((rows, width), (1_048_576, 16_384));
    assert_eq!(first[0][0], Cell::Float(1.5));
    assert_eq!(first[1][0], Cell::Null);
}

#[test]
fn whole_numbers_are_integers_and_errors_are_errors() {
    let (_, _, rows) = open(xlsx(r#"<row r="1"><c r="A1"><v>3</v></c><c r="B1" t="e"><v>#DIV/0!</v></c><c r="C1" t="b"><v>1</v></c></row>"#)).unwrap();
    assert_eq!(rows[0][0], Cell::Int(3));
    assert!(matches!(&rows[0][1], Cell::Error(e) if e.contains("DIV")), "{:?}", rows[0][1]);
    assert_eq!(rows[0][2], Cell::Bool(true));
}

#[test]
fn a_zip_bomb_is_refused_before_it_unpacks() {
    let body = "<row>".to_string() + &" ".repeat(1 << 24) + "</row>";
    let mut limits = Limits::desktop();
    limits.unpack_always = 1 << 20;
    limits.unpack_ratio = 2;
    let cx = ParseContext::new().with_limits(limits);
    let e = XlsxReader.open(&FileSource::from_bytes("a.xlsx", xlsx(&body)), &cx).err().unwrap();
    assert!(matches!(e, ParseError::TooLarge { .. }), "{e}");
}

#[test]
fn a_cell_past_the_last_column_is_refused() {
    // One cell four billion columns right: laid out, every row would be gigabytes.
    let cx = ParseContext::new();
    let e = SylkReader.open(&FileSource::from_bytes("a.slk", b"ID;P\nC;Y1;X4000000000;K1\nE\n".to_vec()), &cx).err().unwrap();
    assert!(matches!(e, ParseError::Corrupt { .. }), "{e}");
    let ok = SylkReader.open(&FileSource::from_bytes("a.slk", b"ID;P\nC;Y1;X16384;K1\nE\n".to_vec()), &cx).unwrap();
    let Content::Table(t) = ok.facet(0, &cx).unwrap() else { panic!("a table") };
    assert_eq!(t.info().columns.len(), 16_384);
}

/// Damaged thousands of ways (`LFP_FUZZ_ITERATIONS` for more), every workbook opens or
/// fails cleanly, every sheet and the formulas included: no panic, no hang, no abort.
#[test]
fn every_reader_survives_fuzzing() {
    let all = ["sample.xlsx", "sample.xlsb", "sample.xls", "sample.ods", "sample.fods", "sample.slk", "sample.dif"];
    libfileparser_testkit::assert_fuzz_clean(readers(), |id| {
        let ext = match id {
            "sylk" => "slk",
            id => id,
        };
        all.iter().filter(|f| f.ends_with(&format!(".{ext}"))).map(|f| Sample::new(*f, sample(f))).collect()
    });
}
