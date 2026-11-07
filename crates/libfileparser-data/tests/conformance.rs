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
        "log" => vec![
            small("app.log", b"2024-11-08 10:00:00 INFO started\n2024-11-08 10:00:01 ERROR failed\n  at main.rs:3\n"),
            small("access.log", b"1.2.3.4 - - [08/Nov/2024:10:00:00 +0000] \"GET / HTTP/1.1\" 200 512\n"),
            small("app.ltsv", b"time:2024-11-08\tstatus:200\n"),
            big("big.log", many("", "2024-11-08 10:00:00 INFO line {i}", "\n", "\n", 50)),
        ],
        "har" => vec![
            small("a.har", br#"{"log": {"entries": [{"request": {"method": "GET"}}]}}"#),
            big("big.har", many(r#"{"log": {"entries": ["#, r#"{"time": {i}}"#, ",", "]}}", 50)),
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

#[test]
fn logs_are_tables_of_entries_with_traces_kept_together() {
    let cx = ParseContext::new();
    let src = FileSource::from_bytes(
        "app.log",
        b"2024-11-08 10:00:00.120 INFO  server started\n2024-11-08 10:00:02.500 ERROR request failed\njava.lang.NullPointerException\n    at App.main(App.java:3)\n2024-11-08 10:00:03.000 WARN  slow\n".to_vec(),
    );
    let doc = libfileparser_data::open(&src, &cx).unwrap();
    assert_eq!((doc.format().reader, doc.format().name.as_str()), ("log", "Log"));
    let Ok(Content::Table(t)) = doc.facet(0, &cx) else { panic!() };
    assert_eq!(t.info().names(), ["Time", "Level", "Message"]);
    assert_eq!(t.info().rows, libfileparser_core::Count::Exact(3));
    let rows = t.rows(0..3).unwrap();
    assert_eq!(rows[1][0], Cell::Date("2024-11-08 10:00:02.500".into()));
    assert_eq!(rows[1][1], Cell::Text("ERROR".into()));
    assert_eq!(rows[1][2], Cell::Text("request failed\njava.lang.NullPointerException\n    at App.main(App.java:3)".into()));
    // The entry spans its trace's lines, and a line of the trace leads back to it.
    assert_eq!(doc.links(&Place::Row { facet: 0, row: 1 }, &cx).unwrap(), [Place::Lines(1..4)]);
    assert_eq!(doc.links(&Place::Lines(3..4), &cx).unwrap(), [Place::Row { facet: 0, row: 1 }]);
    // Prose named .log isn't a log: it opens as text elsewhere.
    assert!(libfileparser_data::detect(b"Dear diary,\ntoday I wrote a log.\n", "notes.log").is_empty());
}

#[test]
fn access_logs_logfmt_and_cef_have_their_columns() {
    let cx = ParseContext::new();
    let table = |name: &str, text: &str| {
        let doc = libfileparser_data::open(&FileSource::from_bytes(name, text.as_bytes().to_vec()), &cx).unwrap();
        let Ok(Content::Table(t)) = doc.facet(0, &cx) else { panic!() };
        (t.info(), t.rows(0..10).unwrap())
    };
    let (info, rows) = table("access.log", "10.0.0.1 - bob [08/Nov/2024:10:00:00 +0000] \"POST /api HTTP/2\" 201 - \"-\" \"curl/8\"\n");
    assert_eq!(info.names()[..4], ["Client", "User", "Time", "Method"]);
    assert_eq!(rows[0][6], Cell::Int(201));
    assert_eq!(rows[0][7], Cell::Null, "a size of `-` is no size");
    let (info, rows) = table("svc.log", "ts=2024-11-08T10:00:00Z level=info msg=\"hi there\" took=12\nts=2024-11-08T10:00:01Z level=warn msg=x took=7 retry\n");
    assert_eq!(info.names(), ["ts", "level", "msg", "took", "retry"]);
    assert_eq!(info.columns[0].kind, ValueKind::Date);
    assert_eq!(info.columns[3].kind, ValueKind::Number);
    assert_eq!(rows[1][3], Cell::Int(7));
    assert_eq!(rows[0][4], Cell::Null);
    let (info, rows) = table("ids.cef", "CEF:0|Vendor|IDS|1.0|42|Port scan|8|src=10.0.0.1 dpt=22\n");
    assert_eq!(info.names()[8..], ["src", "dpt"]);
    assert_eq!(rows[0][6], Cell::Text("Port scan".into()));
}

#[test]
fn har_requests_link_to_their_entries() {
    let cx = ParseContext::new();
    let har = br#"{"log": {"version": "1.2", "creator": {"name": "Firefox"}, "entries": [
  {"startedDateTime": "2024-11-08T10:00:00Z", "time": 12.5, "request": {"method": "GET", "url": "https://example.com/"},
   "response": {"status": 200, "bodySize": 900, "content": {"size": 1024, "mimeType": "text/html"}}},
  {"startedDateTime": "2024-11-08T10:00:01Z", "time": 3, "request": {"method": "POST", "url": "https://example.com/api"},
   "response": {"status": 404, "bodySize": 20, "content": {"size": -1, "mimeType": "application/json"}}}
]}}"#;
    let doc = libfileparser_data::open(&FileSource::from_bytes("session.har", har.to_vec()), &cx).unwrap();
    assert_eq!(facets(&doc)[..2], [(Role::Primary, "Requests"), (Role::Structure, "Tree")]);
    let Ok(Content::Table(t)) = doc.facet(0, &cx) else { panic!() };
    let rows = t.rows(0..2).unwrap();
    assert_eq!(rows[0][1..6], [Cell::Text("GET".into()), Cell::Text("https://example.com/".into()), Cell::Int(200), Cell::Text("text/html".into()), Cell::Int(1024)]);
    assert_eq!(rows[1][5], Cell::Int(20), "no content size: the body size");
    assert_eq!(rows[0][6], Cell::Number("12.5".into()));
    let links = doc.links(&Place::Row { facet: 0, row: 1 }, &cx).unwrap();
    let Some(Place::Node { facet: 1, node }) = links.iter().find(|p| matches!(p, Place::Node { .. })).cloned() else { panic!("{links:?}") };
    let Ok(Content::Tree(tree)) = doc.facet(1, &cx) else { panic!() };
    assert_eq!(tree.path(node).unwrap().as_deref(), Some("/log/entries/1"));
    assert!(links.contains(&Place::Lines(3..5)), "{links:?}");
}

#[test]
fn a_live_log_is_read_by_range() {
    let dir = std::env::temp_dir().join(format!("lfp-data-live-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("server.log");
    let text: String = (0..50_000).map(|i| format!("2024-11-08T10:00:00Z INFO request {i}\n")).collect();
    std::fs::write(&path, text).unwrap();
    let cx = ParseContext::new();
    let src = FileSource::open_live(&path).unwrap();
    let doc = libfileparser_data::open(&src, &cx).unwrap();
    let Ok(Content::Table(t)) = doc.facet(0, &cx) else { panic!() };
    assert_eq!(t.rows(40_000..40_001).unwrap()[0][2], Cell::Text("request 40000".into()));
    assert_eq!(t.memory().mapped, 0, "the log isn't held");
    std::fs::remove_dir_all(&dir).unwrap();
}
