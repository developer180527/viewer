//! The families of data formats beyond the structured and delimited ones: each opens
//! as its format, with the rows and columns it promises.

use libfileparser_core::{Cell, Content, Document, FileSource, ParseContext, Place, TableInfo};

fn open(name: &str, bytes: &[u8]) -> Document {
    libfileparser_data::open(&FileSource::from_bytes(name, bytes.to_vec()), &ParseContext::new()).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn table(doc: &Document) -> (TableInfo, Vec<Vec<Cell>>) {
    let Ok(Content::Table(t)) = doc.facet(0, &ParseContext::new()) else { panic!("{} has no table first", doc.format().reader) };
    (t.info(), t.rows(0..1000).unwrap())
}

fn meta<'a>(doc: &'a Document, key: &str) -> Option<&'a str> {
    doc.metadata().iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

fn t(s: &str) -> Cell {
    Cell::Text(s.into())
}

#[test]
fn feeds_and_playlists() {
    let rss = open("news.xml", br#"<?xml version="1.0"?><rss version="2.0"><channel><title>News</title>
<item><title>One</title><link>https://e.com/1</link><pubDate>Fri, 08 Nov 2024 10:00:00 GMT</pubDate><description>First</description></item>
<item><title>Two</title><guid>https://e.com/2</guid></item></channel></rss>"#);
    assert_eq!((rss.format().reader, rss.format().name.as_str(), meta(&rss, "Title")), ("rss", "RSS 2.0", Some("News")));
    let (info, rows) = table(&rss);
    assert_eq!(info.names(), ["Title", "Published", "Link", "Author", "Summary"]);
    assert_eq!(rows[0], [t("One"), Cell::Date("Fri, 08 Nov 2024 10:00:00 GMT".into()), t("https://e.com/1"), Cell::Null, t("First")]);
    assert_eq!(rows[1][2], t("https://e.com/2"));

    let atom = open("feed.xml", br#"<feed xmlns="http://www.w3.org/2005/Atom"><title>Blog</title>
<entry><title>Post</title><link rel="self" href="https://e.com/self"/><link href="https://e.com/post"/><updated>2024-11-08T10:00:00Z</updated><author><name>Ann</name></author><summary>Hi</summary></entry></feed>"#);
    assert_eq!(atom.format().reader, "atom");
    assert_eq!(table(&atom).1[0], [t("Post"), Cell::Date("2024-11-08T10:00:00Z".into()), t("https://e.com/post"), t("Ann"), t("Hi")]);

    let hls = open("master.m3u8", b"#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-STREAM-INF:BANDWIDTH=1280000,RESOLUTION=1280x720,CODECS=\"avc1.4d401f,mp4a.40.2\"\nlow/index.m3u8\n#EXTINF:-1,Live\nhttp://radio/stream\n");
    assert_eq!(hls.format().name, "HLS playlist");
    let (_, rows) = table(&hls);
    assert_eq!(rows[0][2..6], [t("low/index.m3u8"), Cell::Int(1280000), t("1280x720"), t("avc1.4d401f,mp4a.40.2")]);
    assert_eq!(rows[1][..3], [t("Live"), Cell::Int(-1), t("http://radio/stream")]);
    assert_eq!(hls.links(&Place::Row { facet: 0, row: 0 }, &ParseContext::new()).unwrap(), [Place::Lines(2..4)]);

    let pls = open("radio.pls", b"[playlist]\nFile1=http://a\nTitle1=A\nLength1=-1\nFile2=b.mp3\nLength2=200\nNumberOfEntries=2\n");
    assert_eq!(table(&pls).1, [vec![t("A"), Cell::Null, t("http://a")], vec![Cell::Null, Cell::Int(200), t("b.mp3")]]);

    let xspf = open("mix.xspf", br#"<playlist version="1" xmlns="http://xspf.org/ns/0/"><title>Mix</title><trackList>
<track><title>Song</title><creator>Band</creator><duration>215000</duration><location>file:///song.ogg</location></track></trackList></playlist>"#);
    let (_, rows) = table(&xspf);
    assert_eq!(rows[0][4], Cell::Float(215.0));
    assert_eq!(meta(&xspf, "Title"), Some("Mix"));
}

#[test]
fn localization() {
    let po = open("de.po", b"msgid \"\"\nmsgstr \"\"\n\"Language: de\\n\"\n\"Plural-Forms: nplurals=2; plural=(n != 1);\\n\"\n\n#: main.c:3\nmsgid \"Hello\"\nmsgstr \"Hallo\"\n\n#, fuzzy\nmsgctxt \"menu\"\nmsgid \"File\"\nmsgstr \"Datei\"\n");
    assert_eq!(meta(&po, "Language"), Some("de"));
    let (info, rows) = table(&po);
    assert_eq!(info.names()[..4], ["Context", "Source", "Plural", "Translation"]);
    assert_eq!(rows.len(), 3, "the header is a row too");
    assert_eq!(rows[1][1..4], [t("Hello"), Cell::Null, t("Hallo")]);
    assert_eq!(rows[2][0], t("menu"));
    assert_eq!(rows[2][4], t("fuzzy"));

    let xlf = open("app.xlf", br#"<?xml version="1.0"?><xliff version="1.2"><file original="app.html" source-language="en" target-language="fr"><body>
<trans-unit id="greet"><source>Hello <g id="1">world</g></source><target state="translated">Bonjour <g id="1">monde</g></target><note>Home</note></trans-unit>
</body></file></xliff>"#);
    assert_eq!(meta(&xlf, "Target language"), Some("fr"));
    assert_eq!(table(&xlf).1[0], [t("greet"), t("Hello world"), t("Bonjour monde"), t("translated"), t("Home"), t("app.html")]);
    let xlf2 = open("app.xliff", br#"<xliff version="2.0" srcLang="en" trgLang="de"><file id="f1"><unit id="u1"><segment state="final"><source>Yes</source><target>Ja</target></segment></unit></file></xliff>"#);
    assert_eq!(table(&xlf2).1[0][..4], [t("u1"), t("Yes"), t("Ja"), t("final")]);

    let tmx = open("mem.tmx", br#"<tmx version="1.4"><body><tu tuid="1"><tuv xml:lang="en"><seg>cat</seg></tuv><tuv xml:lang="es"><seg>gato</seg></tuv></tu></body></tmx>"#);
    let (info, rows) = table(&tmx);
    assert_eq!(info.names(), ["ID", "en", "es", "Note"]);
    assert_eq!(rows[0][..3], [t("1"), t("cat"), t("gato")]);

    let resx = open("Strings.resx", br#"<root><resheader name="resmimetype"><value>text/microsoft-resx</value></resheader><data name="Title" xml:space="preserve"><value>Hello</value><comment>Window title</comment></data></root>"#);
    assert_eq!(table(&resx).1, [vec![t("Title"), t("Hello"), t("Window title"), Cell::Null]]);

    let arb = open("app_en.arb", br#"{"@@locale": "en", "hello": "Hello {name}", "@hello": {"description": "Greeting", "placeholders": {"name": {}}}}"#);
    assert_eq!(meta(&arb, "Locale"), Some("en"));
    assert_eq!(table(&arb).1, [vec![t("hello"), t("Hello {name}"), t("Greeting"), t("name")]]);

    let strings = open("Localizable.strings", b"/* Greeting */\n\"hello\" = \"Bonjour\";\n");
    assert_eq!(table(&strings).1, [vec![t("hello"), t("Bonjour"), t("Greeting")]]);
}

#[test]
fn contacts_calendars_directories() {
    let vcf = open("people.vcf", b"BEGIN:VCARD\r\nVERSION:3.0\r\nN:Lovelace;Ada;;;\r\nEMAIL;TYPE=work:ada@example.com\r\nTEL;TYPE=cell:+44 1\r\nORG:Analytical Engines;R&D\r\nBDAY:1815-12-10\r\nNOTE:First\\, and\\nforemost\r\nEND:VCARD\r\nBEGIN:VCARD\r\nFN:Alan Turing\r\nEND:VCARD\r\n");
    let (info, rows) = table(&vcf);
    assert_eq!(info.names(), ["Name", "Email", "Phone", "Organization", "Title", "Address", "Birthday", "Note"]);
    assert_eq!(rows[0][..4], [t("Ada Lovelace"), t("ada@example.com"), t("+44 1"), t("Analytical Engines, R&D")]);
    assert_eq!(rows[0][6], Cell::Date("1815-12-10".into()));
    assert_eq!(rows[0][7], t("First, and\nforemost"));
    assert_eq!(rows[1][0], t("Alan Turing"));
    // A variant-call .vcf isn't a vCard.
    assert!(libfileparser_data::detect(b"##fileformat=VCFv4.2\n", "calls.vcf").iter().all(|(r, _)| r.info().id != "vcard"));

    let ics = open("cal.ics", b"BEGIN:VCALENDAR\r\nPRODID:-//Test//EN\r\nX-WR-CALNAME:Work\r\nBEGIN:VEVENT\r\nSUMMARY:Standup\r\nDTSTART;TZID=Europe/Berlin:20241108T090000\r\nDTEND;TZID=Europe/Berlin:20241108T091500\r\nRRULE:FREQ=DAILY;BYDAY=MO,TU\r\nORGANIZER:mailto:boss@example.com\r\n LONG\r\nEND:VEVENT\r\nBEGIN:VTODO\r\nSUMMARY:Report\r\nDUE:20241110\r\nEND:VTODO\r\nEND:VCALENDAR\r\n");
    assert_eq!(meta(&ics, "Calendar"), Some("Work"));
    let (_, rows) = table(&ics);
    assert_eq!(rows[0][..4], [t("Event"), t("Standup"), Cell::Date("20241108T090000".into()), Cell::Date("20241108T091500".into())]);
    assert_eq!(rows[0][6], t("boss@example.comLONG"), "folded lines are unfolded");
    assert_eq!(rows[1][..4], [t("To-do"), t("Report"), Cell::Null, Cell::Date("20241110".into())]);

    let ldif = open("dir.ldif", b"version: 1\n\ndn: cn=Ann,dc=example,dc=com\nobjectClass: top\nobjectClass: person\ncn: Ann\nmail: ann@example.com\n\ndn: cn=Bob,dc=example,dc=com\ncn: Bob\ndescription:: aMOpbGxv\n");
    let (info, rows) = table(&ldif);
    assert_eq!(info.names(), ["DN", "objectClass", "cn", "mail", "description"]);
    assert_eq!(rows[0][..2], [t("cn=Ann,dc=example,dc=com"), t("top; person")]);
    assert_eq!(rows[1][4], t("h\u{e9}llo"));
}

#[test]
fn sql_statements_schema_and_data() {
    let sql = open("shop.sql", b"-- schema\nCREATE TABLE products (id integer PRIMARY KEY, name text, price numeric(10,2));\nCREATE INDEX products_name ON products (name);\nINSERT INTO products (id, name, price) VALUES (1, 'Pen', 1.50), (2, 'It''s', NULL);\nINSERT INTO products VALUES (3, 'Ink', 9);\nCOPY orders (id, product) FROM stdin;\n10\t1\n11\t3\n\\.\n");
    let cx = ParseContext::new();
    let names: Vec<&str> = sql.facets().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["Statements", "Schema", "Data: products", "Data: orders", "Source", "Raw"]);
    let (_, rows) = table(&sql);
    assert_eq!(rows.iter().map(|r| r[0].clone()).collect::<Vec<_>>(), [t("CREATE TABLE"), t("CREATE INDEX"), t("INSERT"), t("INSERT"), t("COPY")]);
    assert_eq!(rows[2][2], Cell::Int(2));
    assert_eq!(rows[4][2], Cell::Int(2));
    let Ok(Content::Table(products)) = sql.facet(2, &cx) else { panic!() };
    let info = products.info();
    assert_eq!(info.names(), ["id", "name", "price"]);
    assert_eq!(info.columns[0].kind, libfileparser_core::ValueKind::Number);
    assert_eq!(products.rows(0..3).unwrap(), [vec![Cell::Int(1), t("Pen"), Cell::Number("1.50".into())], vec![Cell::Int(2), t("It's"), Cell::Null], vec![Cell::Int(3), t("Ink"), Cell::Int(9)]]);
    let Ok(Content::Table(orders)) = sql.facet(3, &cx) else { panic!() };
    assert_eq!(orders.rows(0..2).unwrap(), [vec![Cell::Int(10), Cell::Int(1)], vec![Cell::Int(11), Cell::Int(3)]]);
    let Ok(Content::Tree(schema)) = sql.facet(1, &cx) else { panic!() };
    let price = schema.find("/products/price").unwrap().unwrap();
    assert_eq!(schema.value(price).unwrap(), Some(t("numeric(10,2)")));
    assert_eq!(sql.links(&Place::Row { facet: 0, row: 2 }, &cx).unwrap(), [Place::Lines(3..4)]);
}

#[test]
fn schemas() {
    let proto = open("people.proto", b"syntax = \"proto3\";\npackage demo;\n\nmessage Person {\n  string name = 1;\n  repeated string emails = 2;\n  message Address { string city = 1; }\n}\nenum Kind { HUMAN = 0; }\nservice People {\n  rpc Get(GetRequest) returns (Person);\n}\n");
    let (info, rows) = table(&proto);
    assert_eq!(info.names(), ["Name", "Kind", "Members", "In"]);
    assert_eq!(rows, [
        vec![t("Person"), t("message"), Cell::Int(2), Cell::Null],
        vec![t("Address"), t("message"), Cell::Int(1), t("Person")],
        vec![t("Kind"), t("enum"), Cell::Int(1), Cell::Null],
        vec![t("People"), t("service"), Cell::Int(1), Cell::Null],
    ]);
    assert_eq!(proto.links(&Place::Row { facet: 0, row: 0 }, &ParseContext::new()).unwrap().iter().find(|p| matches!(p, Place::Lines(_))), Some(&Place::Lines(3..8)));

    let gql = open("schema.graphql", b"# API\ntype Query {\n  user(id: ID!): User\n  users(first: Int, after: String): [User!]!\n}\n\"\"\"A person\"\"\"\ntype User implements Node { id: ID!, name: String }\nunion Result = User | Error\n");
    assert_eq!(table(&gql).1.iter().map(|r| (r[0].clone(), r[2].clone())).collect::<Vec<_>>(), [(t("Query"), Cell::Int(2)), (t("User"), Cell::Int(2))]);

    let avro = open("user.avsc", br#"{"type": "record", "name": "User", "namespace": "com.example", "fields": [
  {"name": "id", "type": "long", "doc": "Key"},
  {"name": "email", "type": ["null", "string"], "default": null},
  {"name": "tags", "type": {"type": "array", "items": "string"}},
  {"name": "address", "type": {"type": "record", "name": "Address", "fields": [{"name": "city", "type": "string"}]}}
]}"#);
    let (_, rows) = table(&avro);
    assert_eq!(rows[0], [t("com.example.User"), t("id"), t("long"), Cell::Null, t("Key")]);
    assert_eq!(rows[1][2..4], [t("null | string"), Cell::Null]);
    assert_eq!(rows[2][2], t("array<string>"));
    assert_eq!(rows[3][2], t("Address"));
    assert_eq!(rows[4][..2], [t("com.example.Address"), t("city")], "a nested record inherits the namespace");

    let api = open("openapi.yaml", b"openapi: 3.1.0\ninfo:\n  title: Pets\n  version: 1.2.0\npaths:\n  /pets:\n    get:\n      operationId: listPets\n      summary: List pets\n      tags: [pets]\n      responses:\n        '200': {description: ok}\n    post:\n      operationId: createPet\n  /pets/{id}:\n    parameters: []\n    delete: {}\n");
    assert_eq!((api.format().reader, api.format().name.as_str(), meta(&api, "Title")), ("openapi", "OpenAPI 3.1.0", Some("Pets")));
    let (info, rows) = table(&api);
    assert_eq!(info.names(), ["Method", "Path", "Operation ID", "Summary", "Tags", "Responses"]);
    assert_eq!(rows[0], [t("GET"), t("/pets"), t("listPets"), t("List pets"), t("pets"), t("200")]);
    assert_eq!(rows.iter().map(|r| r[0].clone()).collect::<Vec<_>>(), [t("GET"), t("POST"), t("DELETE")]);

    let xsd = open("order.xsd", br#"<?xml version="1.0"?><xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" targetNamespace="urn:orders">
<xs:element name="order" type="Order"/>
<xs:complexType name="Order"><xs:annotation><xs:documentation>An order</xs:documentation></xs:annotation><xs:sequence/></xs:complexType>
<xs:simpleType name="Sku"><xs:restriction base="xs:string"/></xs:simpleType></xs:schema>"#);
    assert_eq!(meta(&xsd, "Target namespace"), Some("urn:orders"));
    assert_eq!(table(&xsd).1, [vec![t("element"), t("order"), t("Order"), Cell::Null], vec![t("complexType"), t("Order"), Cell::Null, t("An order")], vec![t("simpleType"), t("Sku"), t("xs:string"), Cell::Null]]);

    for (name, src, first) in [
        ("a.thrift", "struct User {\n  1: required string name,\n  2: optional i32 age = 0\n}\n", "User"),
        ("a.fbs", "namespace demo;\ntable Monster { hp:short = 100; name:string; }\nroot_type Monster;\n", "Monster"),
        ("a.capnp", "@0xdbb9ad1f14bf0b36;\nstruct Person { name @0 :Text; }\n", "Person"),
        ("a.smithy", "$version: \"2\"\nnamespace example\n@readonly\nstructure Forecast { chance: Float }\n", "Forecast"),
        ("schema.prisma", "datasource db { provider = \"postgresql\" }\nmodel User {\n  id Int @id\n  posts Post[]\n}\n", "db"),
        ("db.dbml", "Table users {\n  id integer [pk]\n  name varchar\n}\nRef: posts.user_id > users.id\n", "users"),
    ] {
        let doc = open(name, src.as_bytes());
        assert_eq!(table(&doc).1[0][0], t(first), "{name}");
    }
}

#[test]
fn rdf() {
    let ttl = open("people.ttl", b"@prefix ex: <http://example.com/> .\n@prefix foaf: <http://xmlns.com/foaf/0.1/> .\nex:ann a foaf:Person ;\n  foaf:name \"Ann\"@en ;\n  foaf:age 42 ;\n  foaf:knows ex:bob .\n");
    let (info, rows) = table(&ttl);
    assert_eq!(info.names(), ["Subject", "Predicate", "Object", "Datatype", "Language"]);
    assert_eq!(rows[0][..3], [t("ex:ann"), t("rdf:type"), t("foaf:Person")]);
    assert_eq!(rows[1][2..], [t("Ann"), Cell::Null, t("en")]);
    assert_eq!(rows[2][2..4], [Cell::Int(42), t("xsd:integer")]);
    assert_eq!(meta(&ttl, "Subjects"), Some("1"));
    let err = libfileparser_data::open(&FileSource::from_bytes("bad.ttl", b"@prefix ex: <http://e.com/> .\nex:a ex:b .\n".to_vec()), &ParseContext::new()).err().unwrap();
    assert_eq!(err.position().and_then(|p| p.line), Some(2));

    let nq = open("data.nq", b"<http://e.com/a> <http://e.com/p> \"x\" <http://e.com/g1> .\n");
    assert_eq!(table(&nq).0.names().last(), Some(&"Graph"));

    let rdfxml = open("onto.owl", br#"<?xml version="1.0"?><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:owl="http://www.w3.org/2002/07/owl#" xmlns:rdfs="http://www.w3.org/2000/01/rdf-schema#">
<owl:Class rdf:about="http://example.com/Animal"><rdfs:label>Animal</rdfs:label></owl:Class></rdf:RDF>"#);
    assert_eq!(rdfxml.format().name, "OWL (RDF/XML)");
    assert_eq!(table(&rdfxml).1[0][..3], [t("<http://example.com/Animal>"), t("rdf:type"), t("owl:Class")]);

    let ld = open("org.jsonld", br##"{"@context": "https://schema.org", "@graph": [{"@id": "#org", "@type": "Organization", "name": "Acme"}, {"@id": "#ann", "@type": ["Person", "Employee"]}]}"##);
    assert_eq!(table(&ld).1, [vec![t("#org"), t("Organization"), Cell::Int(1), t("Acme")], vec![t("#ann"), t("Person, Employee"), Cell::Int(0), Cell::Null]]);

    let q = open("find.rq", b"PREFIX foaf: <http://xmlns.com/foaf/0.1/>\nSELECT ?name WHERE {\n  ?p foaf:name ?name .\n  OPTIONAL { ?p foaf:age ?age . }\n  FILTER(?age > 18.5)\n}\nORDER BY ?name\nLIMIT 10\n");
    let Ok(Content::Tree(tree)) = q.facet(0, &ParseContext::new()) else { panic!() };
    let top: Vec<String> = tree.children(None, 0..9).unwrap().into_iter().map(|n| n.label).collect();
    assert_eq!(top, ["PREFIX", "SELECT", "WHERE", "modifier", "modifier"]);
    let where_ = tree.find("/WHERE").unwrap().unwrap();
    let inside: Vec<(String, String)> = tree.children(Some(where_), 0..9).unwrap().into_iter().map(|n| (n.label, n.value)).collect();
    assert_eq!(inside[0], ("pattern".into(), "?p foaf:name ?name".into()));
    assert_eq!(inside[1].0, "OPTIONAL");
    assert_eq!(inside[2], ("pattern".into(), "FILTER(?age > 18.5)".into()));
}

#[test]
fn graphs() {
    let dot = open("flow.gv", b"digraph Flow {\n  start [label=\"Start\", shape=circle];\n  start -> check [label=\"go\"];\n  check -> done;\n}\n");
    assert_eq!((meta(&dot, "Directed"), meta(&dot, "Name")), (Some("yes"), Some("Flow")));
    let (info, rows) = table(&dot);
    assert_eq!(info.names(), ["ID", "Label", "shape"]);
    assert_eq!(rows[0], [t("start"), t("Start"), t("circle")]);
    assert_eq!(rows.len(), 3);
    let Ok(Content::Table(edges)) = dot.facet(1, &ParseContext::new()) else { panic!() };
    assert_eq!(edges.rows(0..2).unwrap(), [vec![t("start"), t("check"), t("go")], vec![t("check"), t("done"), Cell::Null]]);
    assert_eq!(dot.links(&Place::Row { facet: 1, row: 1 }, &ParseContext::new()).unwrap(), [Place::Lines(3..4)]);

    let graphml = open("net.graphml", br#"<?xml version="1.0"?><graphml xmlns="http://graphml.graphdrawing.org/xmlns">
<key id="d0" for="node" attr.name="color" attr.type="string"/><key id="d1" for="edge" attr.name="weight" attr.type="double"/>
<graph id="G" edgedefault="undirected"><node id="n0"><data key="d0">green</data></node><node id="n1"/>
<edge id="e0" source="n0" target="n1"><data key="d1">1.5</data></edge></graph></graphml>"#);
    assert_eq!(meta(&graphml, "Directed"), Some("no"));
    assert_eq!(table(&graphml).1, [vec![t("n0"), t("green")], vec![t("n1"), Cell::Null]]);
    let Ok(Content::Table(edges)) = graphml.facet(1, &ParseContext::new()) else { panic!() };
    assert_eq!(edges.rows(0..1).unwrap(), [vec![t("n0"), t("n1"), t("e0"), t("1.5")]]);

    let gexf = open("g.gexf", br#"<gexf xmlns="http://gexf.net/1.3"><graph defaultedgetype="directed"><attributes class="node"><attribute id="0" title="url" type="string"/></attributes>
<nodes><node id="0" label="Gephi"><attvalues><attvalue for="0" value="https://gephi.org"/></attvalues></node><node id="1" label="Web"/></nodes>
<edges><edge id="0" source="0" target="1" weight="2"/></edges></graph></gexf>"#);
    assert_eq!(table(&gexf).1[0], [t("0"), t("Gephi"), t("https://gephi.org")]);

    let gml = open("karate.gml", b"Creator \"test\"\ngraph [\n  directed 0\n  node [ id 1 label \"A\" club \"x\" ]\n  node [ id 2 ]\n  edge [ source 1 target 2 weight 3 ]\n]\n");
    assert_eq!(gml.format().reader, "graph-gml");
    assert_eq!(table(&gml).1[0], [Cell::Int(1), t("A"), t("x")]);
    let Ok(Content::Table(edges)) = gml.facet(1, &ParseContext::new()) else { panic!() };
    assert_eq!(edges.rows(0..1).unwrap(), [vec![Cell::Int(1), Cell::Int(2), Cell::Null, Cell::Int(3)]]);

    let gxl = open("g.gxl", br#"<gxl><graph id="g" edgemode="directed"><node id="a"><attr name="name"><string>Alpha</string></attr></node><node id="b"/><edge from="a" to="b"/></graph></gxl>"#);
    assert_eq!(table(&gxl).1[0], [t("a"), t("Alpha")]);
}

#[test]
fn science() {
    let fa = open("genes.fasta", b">sp|P01308|INS insulin\nMALWMRLLPLLALLALWGPDPAAA\nFVNQHLCGSHLVEALYLVCGERG\n>seq2\nACGT\n");
    let (info, rows) = table(&fa);
    assert_eq!(info.names(), ["ID", "Description", "Length", "Sequence"]);
    assert_eq!(rows[0][..3], [t("sp|P01308|INS"), t("insulin"), Cell::Int(47)]);
    assert_eq!(rows[1][2..], [Cell::Int(4), t("ACGT")]);
    assert_eq!(meta(&fa, "Residues"), Some("51"));

    let fq = open("reads.fq", b"@r1 lane1\nACGT\n+\nIIII\n@r2\nAC\n+\n!!\n");
    assert_eq!(table(&fq).1, [vec![t("r1"), t("lane1"), Cell::Int(4), Cell::Float(40.0), t("ACGT")], vec![t("r2"), Cell::Null, Cell::Int(2), Cell::Float(0.0), t("AC")]]);

    let gb = open("phage.gb", b"LOCUS       NC_001416              48502 bp    DNA     linear   PHG 28-APR-2023\nDEFINITION  Escherichia phage Lambda,\n            complete genome.\nACCESSION   NC_001416\n  ORGANISM  Escherichia phage Lambda\nFEATURES             Location/Qualifiers\n     source          1..48502\n     gene            191..736\n                     /gene=\"nu1\"\nORIGIN\n        1 gggcggcgac\n//\n");
    assert_eq!(table(&gb).1[0], [t("NC_001416"), Cell::Int(48502), t("DNA"), t("linear"), Cell::Date("28-APR-2023".into()), t("Escherichia phage Lambda, complete genome."), t("NC_001416"), t("Escherichia phage Lambda"), Cell::Int(2)]);

    let gff = open("genes.gff3", b"##gff-version 3\nchr1\tRefSeq\tgene\t1000\t9000\t.\t+\t.\tID=gene1;Name=EDEN\nchr1\tRefSeq\tmRNA\t1050\t9000\t.\t+\t.\tID=mRNA1;Parent=gene1\n##FASTA\n>chr1\nACGT\n");
    let (info, rows) = table(&gff);
    assert_eq!(rows.len(), 2, "the FASTA after ##FASTA isn't features");
    assert_eq!(info.names()[8..11], ["ID", "Name", "Parent"]);
    assert_eq!(rows[1][..5], [t("chr1"), t("RefSeq"), t("mRNA"), Cell::Int(1050), Cell::Int(9000)]);
    assert_eq!(rows[1][10], t("gene1"));

    let vcf = open("calls.vcf", b"##fileformat=VCFv4.2\n##reference=GRCh38\n#CHROM\tPOS\tID\tREF\tALT\tQUAL\tFILTER\tINFO\tFORMAT\tNA001\n20\t14370\trs6054257\tG\tA\t29\tPASS\tDP=14\tGT\t0|0\n");
    assert_eq!((vcf.format().reader, meta(&vcf, "Samples"), meta(&vcf, "reference")), ("vcf", Some("1"), Some("GRCh38")));
    assert_eq!(table(&vcf).1[0][..6], [t("20"), Cell::Int(14370), t("rs6054257"), t("G"), t("A"), Cell::Int(29)]);

    let sam = open("aln.sam", b"@HD\tVN:1.6\n@SQ\tSN:chr1\tLN:100\nr1\t99\tchr1\t7\t30\t8M\t=\t37\t39\tTTAGATAA\t*\tNM:i:0\n");
    assert_eq!(meta(&sam, "References"), Some("1"));
    let (_, rows) = table(&sam);
    assert_eq!(rows[0][..6], [t("r1"), Cell::Int(99), t("chr1"), Cell::Int(7), Cell::Int(30), t("8M")]);
    assert_eq!((rows[0][10].clone(), rows[0][11].clone()), (Cell::Null, t("NM:i:0")));

    let pdb = open("1abc.pdb", b"HEADER    HYDROLASE                               01-JAN-00   1ABC\nTITLE     A TEST\nATOM      1  N   MET A   1      11.104   6.134  -6.504  1.00 21.45           N\nHETATM    2  O   HOH A 101       1.000   2.000   3.000  0.50 10.00           O\nEND\n");
    let (_, rows) = table(&pdb);
    assert_eq!(rows[0], [t("ATOM"), Cell::Int(1), t("N"), t("MET"), t("A"), Cell::Int(1), Cell::Number("11.104".into()), Cell::Number("6.134".into()), Cell::Number("-6.504".into()), Cell::Number("1.00".into()), Cell::Number("21.45".into()), t("N")]);
    assert_eq!(meta(&pdb, "Title"), Some("A TEST"));

    let cif = open("1abc.cif", b"data_1ABC\n_entry.id 1ABC\nloop_\n_atom_site.group_PDB\n_atom_site.id\n_atom_site.Cartn_x\nATOM 1 11.104\nATOM 2 ?\nloop_\n_struct_ref.db_name\nUNP\n");
    let names: Vec<&str> = cif.facets().map(|f| f.name.as_str()).collect();
    assert_eq!(names[..3], ["atom_site", "struct_ref", "Items"]);
    let (info, rows) = table(&cif);
    assert_eq!(info.names(), ["group_PDB", "id", "Cartn_x"]);
    assert_eq!(rows[1], [t("ATOM"), Cell::Int(2), Cell::Null]);

    let arff = open("iris.arff", b"% iris\n@RELATION iris\n@ATTRIBUTE sepal REAL\n@ATTRIBUTE 'class name' {Iris-setosa,Iris-versicolor}\n@DATA\n5.1,Iris-setosa\n?,'Iris-versicolor'\n{0 4.9}\n");
    assert_eq!(meta(&arff, "Relation"), Some("iris"));
    let (info, rows) = table(&arff);
    assert_eq!(info.names(), ["sepal", "class name"]);
    assert_eq!(rows, [vec![Cell::Number("5.1".into()), t("Iris-setosa")], vec![Cell::Null, t("Iris-versicolor")], vec![Cell::Number("4.9".into()), Cell::Null]]);

    let cnf = open("p.cnf", b"c a problem\np cnf 3 2\n1 -3 0\n2 3 -1 0\n");
    assert_eq!(table(&cnf).1, [vec![t("1 -3"), Cell::Int(2)], vec![t("2 3 -1"), Cell::Int(3)]]);
    let graph = open("g.dimacs", b"p edge 3 2\ne 1 2\ne 2 3\n");
    assert_eq!(table(&graph).1[1], [t("edge"), Cell::Int(2), Cell::Int(3), Cell::Null]);
}

#[test]
fn business() {
    let x12 = open("po.edi", b"ISA*00*          *00*          *ZZ*SENDER         *ZZ*RECEIVER       *241108*1200*U*00401*000000001*0*P*>~\nGS*PO*S*R*20241108*1200*1*X*004010~\nST*850*0001~\nBEG*00*SA*PO1**20241108~\nSE*3*0001~\nGE*1*1~\nIEA*1*000000001~\n");
    assert_eq!((x12.format().reader, meta(&x12, "Sender"), meta(&x12, "Transaction sets")), ("x12", Some("SENDER"), Some("850")));
    let (info, rows) = table(&x12);
    assert_eq!(info.names()[..3], ["Segment", "01", "02"]);
    assert_eq!(rows[3][..4], [t("BEG"), t("00"), t("SA"), t("PO1")]);
    assert_eq!(x12.links(&Place::Row { facet: 0, row: 3 }, &ParseContext::new()).unwrap(), [Place::Lines(3..4)]);
    let Ok(Content::Tree(tree)) = x12.facet(1, &ParseContext::new()) else { panic!() };
    assert!(tree.find("/ISA/GS/ST/BEG").unwrap().is_some(), "segments sit in their transaction set");

    let edifact = open("order.edi", b"UNA:+.? 'UNB+UNOC:3+SENDER+RECEIVER+241108:1200+1'UNH+1+ORDERS:D:96A:UN'BGM+220+PO?+1+9'UNT+3+1'UNZ+1+1'");
    assert_eq!((edifact.format().reader, meta(&edifact, "Message types")), ("edifact", Some("ORDERS")));
    assert_eq!(table(&edifact).1[2][..3], [t("BGM"), t("220"), t("PO+1")]);

    let hl7 = open("adt.hl7", b"MSH|^~\\&|EPIC|HOSP|||20241108120000||ADT^A01|MSG1|P|2.5\rPID|1||123^^^HOSP||Doe^John\r");
    assert_eq!((meta(&hl7, "Message type"), meta(&hl7, "Version")), (Some("ADT A01"), Some("2.5")));
    let (_, rows) = table(&hl7);
    assert_eq!(rows[0][..3], [t("MSH"), t("|"), t("^~\\&")]);
    assert_eq!(rows[1][5], t("Doe^John"));

    let ofx = open("bank.ofx", b"OFXHEADER:100\nDATA:OFXSGML\n\n<OFX><BANKMSGSRSV1><STMTTRNRS><STMTRS><CURDEF>USD<BANKACCTFROM><ACCTID>12345</BANKACCTFROM><BANKTRANLIST>\n<STMTTRN><TRNTYPE>DEBIT<DTPOSTED>20241108<TRNAMT>-42.50<FITID>T1<NAME>Coffee</STMTTRN>\n</BANKTRANLIST><LEDGERBAL><BALAMT>100.00<DTASOF>20241108</LEDGERBAL></STMTRS></STMTTRNRS></BANKMSGSRSV1></OFX>\n");
    assert_eq!((meta(&ofx, "Currency"), meta(&ofx, "Balance")), (Some("USD"), Some("100.00")));
    assert_eq!(table(&ofx).1[0], [Cell::Date("2024-11-08".into()), Cell::Number("-42.50".into()), t("Coffee"), Cell::Null, t("DEBIT"), Cell::Null, t("T1"), t("12345")]);

    let qif = open("cash.qif", b"!Type:Cash\nD11/08/2024\nT-1,250.00\nPRent\nLHousing\n^\nD11/09/2024\nT20\nSFood\nSDrink\n^\n");
    let (_, rows) = table(&qif);
    assert_eq!(rows[0][..5], [Cell::Date("11/08/2024".into()), Cell::Number("-1250.00".into()), t("Rent"), Cell::Null, t("Housing")]);
    assert_eq!((rows[1][7].clone(), rows[1][8].clone()), (Cell::Int(2), t("Cash")));

    let fix = open("session.fix", b"20241108-12:00:00 8=FIX.4.4|9=70|35=D|34=2|49=CLIENT|56=BROKER|52=20241108-12:00:00|55=AAPL|54=1|38=100|44=180.5|10=123|\n");
    assert_eq!(table(&fix).1[0], [t("NewOrderSingle (D)"), Cell::Int(2), Cell::Date("20241108-12:00:00".into()), t("CLIENT"), t("BROKER"), t("AAPL"), t("Buy"), Cell::Int(100), Cell::Number("180.5".into())]);

    let mt = open("pay.fin", b"{1:F01BANKBEBBAXXX0000000000}{2:I103BANKDEFFXXXXN}{4:\n:20:REF123\n:32A:241108EUR1000,00\n:59:/DE89370400440532013000\nJOHN DOE\n:71A:SHA\n-}");
    assert_eq!(meta(&mt, "Message types"), Some("MT103"));
    let (_, rows) = table(&mt);
    assert_eq!(rows[1][1..], [t("32A"), t("Value date, currency, amount"), t("241108EUR1000,00")]);
    assert_eq!(rows[2][3], t("/DE89370400440532013000\nJOHN DOE"));

    let xbrl = open("report.xbrl", br#"<?xml version="1.0"?><xbrli:xbrl xmlns:xbrli="http://www.xbrl.org/2003/instance" xmlns:us-gaap="http://fasb.org/us-gaap/2024">
<xbrli:context id="FY24"><xbrli:entity><xbrli:identifier scheme="x">1</xbrli:identifier></xbrli:entity><xbrli:period><xbrli:startDate>2024-01-01</xbrli:startDate><xbrli:endDate>2024-12-31</xbrli:endDate></xbrli:period></xbrli:context>
<xbrli:unit id="USD"><xbrli:measure>iso4217:USD</xbrli:measure></xbrli:unit>
<us-gaap:Revenues contextRef="FY24" unitRef="USD" decimals="-6">1000000</us-gaap:Revenues></xbrli:xbrl>"#);
    assert_eq!(table(&xbrl).1, [vec![t("us-gaap:Revenues"), Cell::Int(1000000), t("2024-01-01 to 2024-12-31"), t("iso4217:USD"), t("-6"), t("FY24")]]);
}

fn tree_top(doc: &Document) -> Vec<(String, String)> {
    let Ok(Content::Tree(t)) = doc.facet(0, &ParseContext::new()) else { panic!("{} has no tree first", doc.format().reader) };
    t.children(None, 0..50).unwrap().into_iter().map(|n| (n.label, n.value)).collect()
}

#[test]
fn more_config_sgml_fixed_width() {
    let tf = open("main.tf", b"terraform {\n  required_version = \">= 1.5\"\n}\n\nresource \"aws_s3_bucket\" \"logs\" {\n  bucket = \"my-logs\"\n  versioning {\n    enabled = true\n  }\n}\n\nvariable \"region\" {\n  default = \"eu-west-1\"\n}\n");
    assert_eq!(tf.format().name, "Terraform");
    assert_eq!(table(&tf).1, [
        vec![t("terraform"), Cell::Null, Cell::Int(1), Cell::Int(0)],
        vec![t("resource"), t("aws_s3_bucket.logs"), Cell::Int(1), Cell::Int(1)],
        vec![t("variable"), t("region"), Cell::Int(1), Cell::Int(0)],
    ]);

    let cue = open("app.cue", b"package app\n\n// settings\n#Config: {\n  port: int & >0\n  host: string | *\"localhost\"\n}\nconfig: #Config & {\n  port: 8080\n}\n");
    let top = tree_top(&cue);
    assert_eq!(top.iter().map(|n| n.0.as_str()).collect::<Vec<_>>(), ["package", "#Config", "config"]);
    let dhall = open("app.dhall", b"-- config\nlet port = 8080\nin  { name = \"app\", port = port, tags = [ \"a\", \"b\" ] }\n");
    assert!(tree_top(&dhall).iter().any(|n| n.0 == "(record)" || n.0 == "in"), "{:?}", tree_top(&dhall));
    let kdl = open("pkg.kdl", b"package {\n  name \"my-pkg\"\n  version \"1.2.3\"\n  /-disabled true\n  dependencies {\n    lodash \"^3.2.1\" optional=#true\n  }\n}\n");
    let Ok(Content::Tree(tree)) = kdl.facet(0, &ParseContext::new()) else { panic!() };
    let pkg = tree.find("/package").unwrap().unwrap();
    let kids: Vec<(String, String)> = tree.children(Some(pkg), 0..9).unwrap().into_iter().map(|n| (n.label, n.value)).collect();
    assert_eq!(kids, [("name".to_string(), "my-pkg".to_string()), ("version".to_string(), "1.2.3".to_string()), ("dependencies".to_string(), String::new())]);
    let dep = tree.find("/package/dependencies/lodash").unwrap().unwrap();
    assert_eq!(tree.value(dep).unwrap(), Some(t("\"^3.2.1\" optional=#true")));

    let cron = open("crontab", b"SHELL=/bin/bash\n# backups\n30 2 * * * /usr/local/bin/backup --full\n*/15 * * * 1-5 /check.sh\n@reboot /start.sh\n");
    assert_eq!(table(&cron).1, [
        vec![t("30"), t("2"), t("*"), t("*"), t("*"), t("/usr/local/bin/backup --full"), t("at 02:30")],
        vec![t("*/15"), t("*"), t("*"), t("*"), t("1-5"), t("/check.sh"), t("every 15 minutes, weekday 1 to 5")],
        vec![Cell::Null, Cell::Null, Cell::Null, Cell::Null, Cell::Null, t("/start.sh"), t("at startup")],
    ]);

    let sgml = open("doc.sgml", b"<!DOCTYPE book PUBLIC \"-//OASIS//DTD DocBook V4.1//EN\">\n<book><title>Guide</title><para>One<para>Two</book>\n");
    assert_eq!(tree_top(&sgml)[0].0, "book");

    let fwf = open("people.fwf", b"NAME      AGE  CITY\nAnn        42  Paris\nBob Smith   7  New York\n");
    let (info, rows) = table(&fwf);
    assert_eq!(info.names(), ["NAME", "AGE", "CITY"]);
    assert_eq!(info.columns[1].kind, libfileparser_core::ValueKind::Number);
    assert_eq!(rows, [vec![t("Ann"), Cell::Int(42), t("Paris")], vec![t("Bob Smith"), Cell::Int(7), t("New York")]]);
}
