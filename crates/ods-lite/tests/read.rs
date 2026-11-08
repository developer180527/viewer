use ods_lite::{Error, Limits, Value, read_flat};

fn book(body: &str) -> String {
    format!(r#"<office:document xmlns:office="o" xmlns:table="t" xmlns:text="x"><office:body><office:spreadsheet>{body}</office:spreadsheet></office:body></office:document>"#)
}

#[test]
fn types_formulas_and_repeats() {
    let xml = book(concat!(
        r#"<table:table table:name="A"><table:table-row table:number-rows-repeated="2">"#,
        r#"<table:table-cell office:value-type="float" office:value="1.5" table:formula="of:=1+0.5"><text:p>1.5</text:p></table:table-cell>"#,
        r#"<table:table-cell table:number-columns-repeated="3"/>"#,
        r#"<table:table-cell office:value-type="string"><text:p>a &amp; b</text:p><text:p>c<text:s text:c="2"/>d</text:p></table:table-cell>"#,
        r#"</table:table-row><table:table-row table:number-rows-repeated="1048570"><table:table-cell/></table:table-row>"#,
        r#"<table:table-row><table:table-cell office:value-type="boolean" office:boolean-value="true"/>"#,
        r#"<table:table-cell office:value-type="date" office:date-value="2020-01-02"/></table:table-row></table:table>"#,
        r#"<table:table table:name="B"/>"#,
    ));
    let b = read_flat(xml.as_bytes(), &Limits::default()).unwrap();
    assert_eq!(b.sheets.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["A", "B"]);
    let c = &b.sheets[0].cells;
    assert_eq!(c.len(), 6);
    assert_eq!((c[0].row, c[0].col, c[0].value.clone(), c[0].formula.as_deref()), (0, 0, Some(Value::Number(1.5)), Some("of:=1+0.5")));
    assert_eq!((c[1].row, c[1].col), (0, 4), "the empty run still moves along");
    assert_eq!(c[1].value, Some(Value::Text("a & b\nc  d".into())));
    assert_eq!((c[2].row, c[3].row), (1, 1), "repeated rows are repeated");
    assert_eq!((c[4].row, c[4].value.clone()), (1048572, Some(Value::Bool(true))));
    assert_eq!(c[5].value, Some(Value::Date("2020-01-02".into())));
}

#[test]
fn a_small_file_cant_claim_a_huge_sheet() {
    let xml = book(concat!(
        r#"<table:table table:name="A"><table:table-row table:number-rows-repeated="1048576">"#,
        r#"<table:table-cell table:number-columns-repeated="16384" office:value-type="float" office:value="1"/></table:table-row></table:table>"#,
    ));
    assert!(matches!(read_flat(xml.as_bytes(), &Limits::default().with_cells(1000)), Err(Error::TooLarge { .. })));
}

#[test]
fn comments_and_nesting_arent_the_cells_text() {
    let xml = book(concat!(
        r#"<table:table table:name="A"><table:table-row><table:table-cell office:value-type="string">"#,
        r#"<office:annotation><text:p>a note</text:p></office:annotation><text:p>shown</text:p></table:table-cell></table:table-row></table:table>"#,
    ));
    let b = read_flat(xml.as_bytes(), &Limits::default()).unwrap();
    assert_eq!(b.sheets[0].cells[0].value, Some(Value::Text("shown".into())));
    assert_eq!(read_flat(b"<a/>", &Limits::default()).err(), Some(Error::NotOds));
}

#[test]
fn a_few_bytes_cant_spell_gigabytes_of_spaces() {
    // Each element is 26 bytes and 64 KiB of spaces; 2,000 of them, 128 MiB.
    let spaces = r#"<text:s text:c="65536"/>"#.repeat(2_000);
    let xml = book(&format!(r#"<table:table table:name="A"><table:table-row><table:table-cell office:value-type="string"><text:p>{spaces}</text:p></table:table-cell></table:table-row></table:table>"#));
    let e = read_flat(xml.as_bytes(), &Limits::default().with_text(16 << 20)).unwrap_err();
    assert!(matches!(e, Error::TooLarge { what: "bytes of cell text", .. }), "{e}");
    // And repeated, the same text counts each time it's laid out.
    let xml = book(r#"<table:table table:name="A"><table:table-row table:number-rows-repeated="1000"><table:table-cell table:number-columns-repeated="1000" office:value-type="string"><text:p>0123456789</text:p></table:table-cell></table:table-row></table:table>"#);
    let e = read_flat(xml.as_bytes(), &Limits::default().with_text(1 << 20)).unwrap_err();
    assert!(matches!(e, Error::TooLarge { what: "bytes of cell text", .. }), "{e}");
}
