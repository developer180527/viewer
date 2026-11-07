//! Every data reader passes the conformance suite (docs/design/libfileparser.md §9),
//! and its documents have the facets and links the crate promises.

use libfileparser_core::{Cell, Content, FileSource, NodeId, ParseContext, Place, Role, ValueKind};
use libfileparser_testkit::{Sample, assert_conforms};

fn corpus(path: &str) -> Vec<u8> {
    std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../corpus/files")
            .join(path),
    )
    .unwrap()
}

/// `n` repetitions of `item`, numbered, between `open` and `close`.
fn many(open: &str, item: &str, sep: &str, close: &str, n: usize) -> Vec<u8> {
    let items: Vec<String> = (0..n)
        .map(|i| item.replace("{i}", &i.to_string()))
        .collect();
    format!("{open}{}{close}", items.join(sep)).into_bytes()
}

fn samples(id: &str) -> Vec<Sample> {
    let small = |name: &str, bytes: &[u8]| Sample::new(name, bytes.to_vec()).fits_tiny_limits();
    let big = |name: &str, bytes: Vec<u8>| Sample::new(name, bytes);
    match id {
        "json" => vec![
            small("a.json", b"{\"a\": [1, 2], \"b\": {\"c\": null}}"),
            small("settings.json", b"{\n  // a comment\n  \"x\": 1,\n}"),
            big("big.json", many("[", "{\"id\": {i}}", ",\n", "]", 50)),
        ],
        "json-lines" => vec![
            small("events.jsonl", &corpus("data/events.jsonl")),
            small("one.ndjson", b"{\"a\":1}\n"),
            big("big.jsonl", many("", "{\"id\": {i}, \"ok\": true}", "\n", "\n", 50)),
        ],
        "json5" => vec![small("a.json5", b"{a: 'x', b: 0x10,}"), big("big.json5", many("[", "{i}", ",", ",]", 50))],
        "hjson" => vec![small("a.hjson", b"a: x\nb: 2\n"), big("big.hjson", many("", "k{i}: v", "\n", "\n", 50))],
        "yaml" => vec![small("a.yaml", b"a: [1, 2]\nb: x\n"), big("big.yml", many("", "- {i}", "\n", "\n", 50))],
        "toml" => vec![small("a.toml", b"[pkg]\nname = \"x\"\n"), big("big.toml", many("", "k{i} = {i}", "\n", "\n", 50))],
        "xml" => vec![
            small("a.xml", b"<?xml version=\"1.0\"?>\n<a x=\"1\"><b>t</b></a>\n"),
            small("logo.svg", b"<svg xmlns=\"http://www.w3.org/2000/svg\"><rect/></svg>"),
            big("big.xml", many("<r>", "<i n=\"{i}\"/>", "\n", "</r>", 50)),
        ],
        "plist" => vec![
            small("Info.plist", b"<?xml version=\"1.0\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"x\">\n<plist><dict><key>A</key><true/></dict></plist>"),
            big("big.plist", many("<?xml version=\"1.0\"?>\n<!DOCTYPE plist PUBLIC \"\" \"\">\n<plist><array>", "<integer>{i}</integer>", "", "</array></plist>", 50)),
        ],
        "ini" => vec![small("a.ini", b"[core]\nk = v\n"), small(".editorconfig", b"root = true\n[*]\nindent = 4\n"), big("big.ini", many("[s]\n", "k{i} = {i}", "\n", "\n", 50))],
        "properties" => vec![small("a.properties", b"a=1\nb: 2\n"), big("big.properties", many("", "k{i}={i}", "\n", "\n", 50))],
        "env" => vec![small(".env", b"A=1\nexport B='x'\n"), big("big.env", many("", "K{i}={i}", "\n", "\n", 50))],
        "delimited" => vec![
            small("people.tsv", &corpus("data/people.tsv")),
            small("a.csv", b"a;b\n1;2\n"),
            big("big.csv", many("a,b\n", "{i},x", "\n", "\n", 50)),
        ],
        other => panic!("no samples for {other}"),
    }
}

#[test]
fn every_reader_conforms() {
    for reader in libfileparser_data::readers() {
        assert_conforms(*reader, &samples(reader.info().id));
    }
}

fn facets(doc: &libfileparser_core::Document) -> Vec<(Role, &str)> {
    doc.facets().map(|f| (f.role, f.name.as_str())).collect()
}

#[test]
fn trees_link_to_their_lines_and_back() {
    let cx = ParseContext::new();
    let src = FileSource::from_bytes(
        "a.json",
        b"{\n  \"name\": \"x\",\n  \"list\": [\n    1\n  ]\n}\n".to_vec(),
    );
    let doc = libfileparser_data::open(&src, &cx).unwrap();
    assert_eq!(doc.format().reader, "json");
    assert_eq!(
        facets(&doc),
        [
            (Role::Primary, "Tree"),
            (Role::Source, "Source"),
            (Role::Raw, "Raw")
        ]
    );
    let Ok(Content::Tree(tree)) = doc.facet(0, &cx) else {
        panic!()
    };
    let list = tree.find("/list").unwrap().unwrap();
    assert_eq!(
        doc.links(
            &Place::Node {
                facet: 0,
                node: list
            },
            &cx
        )
        .unwrap(),
        [Place::Lines(2..5)]
    );
    let one = tree.find("/list/0").unwrap().unwrap();
    assert_eq!(
        doc.links(&Place::Lines(3..4), &cx).unwrap(),
        [Place::Node {
            facet: 0,
            node: one
        }]
    );
}

#[test]
fn json_lines_rows_link_to_lines_and_nodes() {
    let cx = ParseContext::new();
    let src = FileSource::from_bytes(
        "e.jsonl",
        b"{\"t\": 1, \"msg\": \"a\"}\n{\"t\": 2, \"level\": \"warn\"}\n".to_vec(),
    );
    let doc = libfileparser_data::open(&src, &cx).unwrap();
    assert_eq!(
        facets(&doc),
        [
            (Role::Primary, "Table"),
            (Role::Structure, "Tree"),
            (Role::Source, "Source"),
            (Role::Raw, "Raw")
        ]
    );
    let Ok(Content::Table(t)) = doc.facet(0, &cx) else {
        panic!()
    };
    assert_eq!(t.info().names(), ["t", "msg", "level"]);
    assert_eq!(t.info().columns[0].kind, ValueKind::Number);
    // A missing key is null, not empty text.
    assert_eq!(t.rows(1..2).unwrap(), [vec![Cell::Int(2), Cell::Null, Cell::Text("warn".into())]]);
    let links = doc.links(&Place::Row { facet: 0, row: 1 }, &cx).unwrap();
    assert!(
        links.contains(&Place::Lines(1..2))
            && links.contains(&Place::Node {
                facet: 1,
                node: NodeId(3)
            }),
        "{links:?}"
    );
}

#[test]
fn delimited_rows_link_to_lines() {
    let cx = ParseContext::new();
    let src = FileSource::from_bytes("a.csv", b"h1,h2\n1,\"two\nlines\"\n3,4\n".to_vec());
    let doc = libfileparser_data::open(&src, &cx).unwrap();
    assert_eq!(doc.format().name, "CSV");
    assert_eq!(
        doc.links(&Place::Row { facet: 0, row: 0 }, &cx).unwrap(),
        [Place::Lines(1..3)]
    );
    assert_eq!(
        doc.links(&Place::Lines(3..4), &cx).unwrap(),
        [Place::Row { facet: 0, row: 1 }]
    );
}

#[test]
fn detection_by_name_and_content() {
    let id = |header: &[u8], name: &str| {
        libfileparser_data::detect(header, name)
            .first()
            .map(|(r, _)| r.info().id)
    };
    assert_eq!(id(b"  {\"a\":1}", "noext"), Some("json"));
    assert_eq!(id(b"[a]\nk=v\n", "noext"), None);
    assert_eq!(id(b"<?xml version=\"1.0\"?><a/>", "feed"), Some("xml"));
    assert_eq!(id(b"bplist00", "x"), Some("plist"));
    assert_eq!(id(b"", "Cargo.lock"), Some("toml"));
    assert_eq!(id(b"A=1", ".env.local"), Some("env"));
    assert_eq!(id(b"[a]\nk=v\n", "app.conf"), Some("ini"));
    // nginx's syntax isn't INI; a .dat of prose isn't a table.
    assert_eq!(id(b"server {\n  listen 80;\n}\n", "nginx.conf"), None);
    assert_eq!(id(b"some words here\nand more\n", "notes.dat"), None);
    assert_eq!(id(b"a|b\n1|2\n", "x.dat"), Some("delimited"));
    // Broken JSON isn't JSON: the front door shows it as text.
    let cx = ParseContext::new();
    assert!(
        libfileparser_data::open(
            &FileSource::from_bytes("bad.json", b"{\"a\": }".to_vec()),
            &cx
        )
        .is_err()
    );
}

#[test]
fn deep_nesting_fails_cleanly_in_every_format() {
    let cx = ParseContext::new();
    let deep = 200_000;
    let cases: [(&str, Vec<u8>); 6] = [
        ("deep.json", "[".repeat(deep).into_bytes()),
        ("deep.json5", "{a:".repeat(deep).into_bytes()),
        ("deep.jsonl", "[".repeat(deep).into_bytes()),
        ("deep.yaml", "[".repeat(deep).into_bytes()),
        (
            "deep.toml",
            format!("a = {}", "[".repeat(deep)).into_bytes(),
        ),
        ("deep.xml", "<a>".repeat(deep).into_bytes()),
    ];
    for (name, bytes) in cases {
        let r = libfileparser_data::open(&FileSource::from_bytes(name, bytes), &cx);
        assert!(r.is_err(), "{name} opened");
    }
}

#[test]
fn a_bad_line_is_a_row_marked_as_an_error_not_a_failed_file() {
    let cx = ParseContext::new();
    let src = FileSource::from_bytes("e.jsonl", b"{\"t\": 1}\n{\"t\": 2,\n{\"t\": 3}\n".to_vec());
    let doc = libfileparser_data::open(&src, &cx).unwrap();
    let Ok(Content::Table(t)) = doc.facet(0, &cx) else { panic!() };
    let rows = t.rows(0..3).unwrap();
    assert!(matches!(rows[1][0], Cell::Error(_)), "{rows:?}");
    assert_eq!(rows[2], [Cell::Int(3)]);
    let w = &doc.warnings()[0];
    assert_eq!((w.at.and_then(|p| p.line), w.place.clone()), (Some(2), Some(Place::Row { facet: 0, row: 1 })));
}

#[test]
fn delimited_number_columns_are_numbers() {
    let cx = ParseContext::new();
    let src = FileSource::from_bytes("a.csv", b"id,name,score\n1,ann,9.5\n2,bob,\n007,cy,1e3\n".to_vec());
    let doc = libfileparser_data::open(&src, &cx).unwrap();
    let Ok(Content::Table(t)) = doc.facet(0, &cx) else { panic!() };
    let kinds: Vec<_> = t.info().columns.iter().map(|c| c.kind).collect();
    assert_eq!(kinds, [ValueKind::Number, ValueKind::Text, ValueKind::Number]);
    let rows = t.rows(0..3).unwrap();
    assert_eq!(rows[1], [Cell::Int(2), Cell::Text("bob".into()), Cell::Null]);
    // Kept as written: converting would lose the zeros.
    assert_eq!(rows[2][0], Cell::Number("007".into()));
}

#[test]
fn trees_give_whole_typed_values() {
    let cx = ParseContext::new();
    let long = "x".repeat(5000);
    let src = FileSource::from_bytes("a.json", format!("{{\"s\": \"{long}\", \"n\": 1.50, \"z\": null, \"q\": \"1\"}}").into_bytes());
    let doc = libfileparser_data::open(&src, &cx).unwrap();
    let Ok(Content::Tree(t)) = doc.facet(0, &cx) else { panic!() };
    let top = t.children(None, 0..4).unwrap();
    assert!(top[0].value.chars().count() <= libfileparser_core::TreeNode::PREVIEW_CHARS + 1);
    assert_eq!(t.value(top[0].id).unwrap(), Some(Cell::Text(long)));
    assert_eq!(t.value(top[1].id).unwrap(), Some(Cell::Number("1.50".into())));
    assert_eq!((top[2].kind, t.value(top[2].id).unwrap()), (ValueKind::Null, Some(Cell::Null)));
    assert_eq!((top[3].kind, t.value(top[3].id).unwrap()), (ValueKind::Text, Some(Cell::Text("1".into()))));
    let err = libfileparser_data::open(&FileSource::from_bytes("b.json", b"{\n  \"a\": ]\n}".to_vec()), &cx).err().unwrap();
    assert_eq!(err.position().and_then(|p| p.line), Some(2));
}
