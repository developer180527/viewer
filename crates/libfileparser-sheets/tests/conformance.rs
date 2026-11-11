//! Every sheets reader passes the conformance suite (docs/design/libfileparser.md §9).

use std::io::Write;

use libfileparser_core::{Cell, Content, FileSource, Limits, ParseContext, ParseError, Reader, Result};
use libfileparser_sheets::{DifReader, FodsReader, NumbersReader, OdsReader, SylkReader, XlsReader, XlsbReader, XlsxReader, XmlssReader, readers};
use libfileparser_testkit::{Sample, assert_conforms};

fn sample(name: &str) -> Vec<u8> {
    std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/files/sheets").join(name)).unwrap()
}

#[test]
fn every_reader_conforms() {
    assert_conforms(&XlsxReader, &[Sample::new("sample.xlsx", sample("sample.xlsx")).fits_tiny_limits()]);
    assert_conforms(&XlsbReader, &[Sample::new("sample.xlsb", sample("sample.xlsb")).fits_tiny_limits()]);
    assert_conforms(&XlsReader, &[Sample::new("sample.xls", sample("sample.xls")).fits_tiny_limits(), Sample::new("sample-biff4.xls", sample("sample-biff4.xls")).fits_tiny_limits()]);
    assert_conforms(&OdsReader, &[Sample::new("sample.ods", sample("sample.ods")).fits_tiny_limits()]);
    assert_conforms(&FodsReader, &[Sample::new("sample.fods", sample("sample.fods")).fits_tiny_limits()]);
    assert_conforms(&SylkReader, &[Sample::new("sample.slk", sample("sample.slk")).fits_tiny_limits()]);
    assert_conforms(&DifReader, &[Sample::new("sample.dif", sample("sample.dif")).fits_tiny_limits()]);
    assert_conforms(&XmlssReader, &[Sample::new("sample-xmlss.xml", sample("sample-xmlss.xml")).fits_tiny_limits()]);
    assert_conforms(&NumbersReader, &[Sample::new("sample.numbers", sample("sample.numbers"))]);
    assert_eq!(readers().len(), 9);
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

#[test]
fn a_sheet_damaged_partway_keeps_the_cells_before_the_damage() {
    let cx = ParseContext::new();
    let bytes = xlsx(r#"<row r="1"><c r="A1"><v>1</v></c></row><row r="2"><c r="A2" t="b"><v>00</v></c></row>"#);
    let doc = XlsxReader.open(&FileSource::from_bytes("a.xlsx", bytes), &cx).unwrap();
    assert!(doc.warnings().iter().any(|w| w.message.contains("damaged")), "{:?}", doc.warnings());
    let Content::Table(t) = doc.facet(0, &cx).unwrap() else { panic!("a table") };
    assert_eq!(t.rows(0..1).unwrap()[0][0], Cell::Int(1));
}

#[test]
fn cells_past_the_last_column_of_a_workbook_are_left_out() {
    let cx = ParseContext::new();
    let bytes = xlsx(r#"<row r="1"><c r="A1"><v>1</v></c><c r="XFE1"><v>2</v></c></row>"#);
    let doc = XlsxReader.open(&FileSource::from_bytes("a.xlsx", bytes), &cx).unwrap();
    assert!(doc.warnings().iter().any(|w| w.message.contains("past the last column")), "{:?}", doc.warnings());
    let Content::Table(t) = doc.facet(0, &cx).unwrap() else { panic!("a table") };
    assert_eq!(t.info().columns.len(), 1);
}

#[test]
fn a_workbook_of_only_charts_says_so() {
    let mut out = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut out));
        let mut add = |name: &str, body: &str| {
            zip.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        };
        add("_rels/.rels", r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#);
        add("xl/workbook.xml", r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Chart" sheetId="1" r:id="rId1"/></sheets></workbook>"#);
        add("xl/_rels/workbook.xml.rels", r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/chartsheet" Target="chartsheets/sheet1.xml"/></Relationships>"#);
        add("xl/chartsheets/sheet1.xml", r#"<chartsheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"/>"#);
    }
    let e = XlsxReader.open(&FileSource::from_bytes("a.xlsx", out), &ParseContext::new()).err().unwrap();
    assert!(matches!(e, ParseError::Unsupported { .. }) && e.to_string().contains("charts"), "{e}");
}

#[test]
fn an_excel_4_worksheet_opens_as_xls() {
    let cx = ParseContext::new();
    let bytes = sample("sample-biff4.xls");
    assert_eq!(XlsReader.detect(&bytes, "old.xls").confidence, libfileparser_core::Confidence::Certain);
    let doc = XlsReader.open(&FileSource::from_bytes("old.xls", bytes), &cx).unwrap();
    assert_eq!(doc.format().name, "Excel 4 worksheet");
    let Content::Table(t) = doc.facet(0, &cx).unwrap() else { panic!("a table") };
    let rows = t.rows(0..4).unwrap();
    assert_eq!(rows[1][0], Cell::Text("Café €".into()));
    assert_eq!(rows[1][1], Cell::Float(2.5));
    assert_eq!((rows[2][1].clone(), rows[2][2].clone()), (Cell::Int(3), Cell::Bool(true)));
    assert_eq!(rows[3][1], Cell::Float(5.5));
}

#[test]
fn an_xml_spreadsheet_opens_with_its_sheets_formulas_and_names() {
    let cx = ParseContext::new();
    let bytes = sample("sample-xmlss.xml");
    assert_eq!(XmlssReader.detect(&bytes, "report.xls").confidence, libfileparser_core::Confidence::Certain);
    let doc = XmlssReader.open(&FileSource::from_bytes("report.xls", bytes), &cx).unwrap();
    let Content::Table(t) = doc.facet(0, &cx).unwrap() else { panic!("a table") };
    let rows = t.rows(0..4).unwrap();
    assert_eq!((rows[1][0].clone(), rows[1][1].clone()), (Cell::Text("Café".into()), Cell::Float(2.5)));
    assert_eq!(rows[3][2], Cell::Date("2024-03-01T00:00:00.000".into()));
    let facets: Vec<String> = doc.facets().map(|f| f.name.clone()).collect();
    assert_eq!(facets[..4], ["Sales", "Notes", "Formulas", "Defined names"]);
}

#[test]
fn a_numbers_document_opens_with_its_tables() {
    let cx = ParseContext::new();
    let bytes = sample("sample.numbers");
    assert_eq!(NumbersReader.detect(&bytes, "budget.numbers").confidence, libfileparser_core::Confidence::Certain);
    let doc = NumbersReader.open(&FileSource::from_bytes("budget.numbers", bytes), &cx).unwrap();
    assert_eq!(doc.format().name, "Numbers spreadsheet");
    assert_eq!(doc.facets().next().unwrap().name, "Table 1");
    let Content::Table(t) = doc.facet(0, &cx).unwrap() else { panic!("a table") };
    assert_eq!((t.info().rows.known(), t.info().columns.len()), (4, 5));
    let rows = t.rows(0..4).unwrap();
    assert_eq!((rows[0][0].clone(), rows[0][4].clone(), rows[2][1].clone()), (Cell::Text("SheetJS".into()), Cell::Text("E1".into()), Cell::Text("B3:D3".into())));
    assert!(doc.metadata().iter().any(|(k, v)| k == "Sheets" && v == "1") && doc.metadata().iter().any(|(k, v)| k == "Tables" && v == "1"));
    assert!(doc.warnings().is_empty());
}

/// Four things calamine refuses a whole workbook for, in one: a visibility Excel doesn't
/// define, a macro sheet, a sheet listed with nothing behind it, and a size given as
/// whole rows. Mended (mend.rs), it opens with every sheet that has cells.
#[test]
fn a_workbook_calamine_refuses_for_something_small_is_mended() {
    let mut out = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut out));
        let mut add = |name: &str, body: &str| {
            zip.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        };
        let rel = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
        add("_rels/.rels", &format!(r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{rel}/officeDocument" Target="xl/workbook.xml"/></Relationships>"#));
        add(
            "xl/workbook.xml",
            &format!(
                r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="{rel}"><sheets><sheet name="Data" sheetId="1" state="show" r:id="rId1"/><sheet name="Module" sheetId="2" r:id=""/><sheet name="Macros" sheetId="3" r:id="rId2"/></sheets></workbook>"#
            ),
        );
        add(
            "xl/_rels/workbook.xml.rels",
            &format!(
                r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{rel}/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.microsoft.com/office/2006/relationships/xlMacrosheet" Target="macrosheets/sheet1.xml"/></Relationships>"#
            ),
        );
        add("xl/worksheets/sheet1.xml", r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dimension ref="1:2"/><sheetData><row r="1"><c r="A1"><v>7</v></c></row></sheetData></worksheet>"#);
        add(
            "xl/macrosheets/sheet1.xml",
            r#"<xm:macrosheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:xm="http://schemas.microsoft.com/office/excel/2006/main"><sheetData><row r="1"><c r="A1"><v>9</v></c></row></sheetData></xm:macrosheet>"#,
        );
    }
    let cx = ParseContext::new();
    let doc = XlsxReader.open(&FileSource::from_bytes("a.xlsx", out), &cx).unwrap();
    let names: Vec<String> = doc.facets().map(|f| f.name.clone()).collect();
    assert_eq!(names[..2], ["Data", "Macros"], "{names:?}");
    for (facet, value) in [(0, 7), (1, 9)] {
        let Content::Table(t) = doc.facet(facet, &cx).unwrap() else { panic!("a table") };
        assert_eq!(t.rows(0..1).unwrap()[0][0], Cell::Int(value));
    }
    let said: Vec<&str> = doc.warnings().iter().map(|w| w.message.as_str()).collect();
    assert!(said.iter().any(|w| w.contains("macro")) && said.iter().any(|w| w.contains("Module")) && said.iter().any(|w| w.contains("visibility")) && said.iter().any(|w| w.contains("whole rows")), "{said:?}");
    // A workbook with nothing wrong isn't mended.
    let plain = XlsxReader.open(&FileSource::from_bytes("a.xlsx", xlsx(r#"<row r="1"><c r="A1"><v>1</v></c></row>"#)), &cx).unwrap();
    assert!(plain.warnings().is_empty());
}

/// Damaged thousands of ways (`LFP_FUZZ_ITERATIONS` for more), every workbook opens or
/// fails cleanly, every sheet and the formulas included: no panic, no hang, no abort.
#[test]
fn every_reader_survives_fuzzing() {
    let all = ["sample.xlsx", "sample.xlsb", "sample.xls", "sample-biff4.xls", "sample.ods", "sample.fods", "sample.slk", "sample.dif", "sample-xmlss.xml", "sample.numbers"];
    libfileparser_testkit::assert_fuzz_clean(readers(), |id| {
        let ext = match id {
            "sylk" => "slk",
            "xmlss" => "xml",
            id => id,
        };
        all.iter().filter(|f| f.ends_with(&format!(".{ext}"))).map(|f| Sample::new(*f, sample(f))).collect()
    });
}
