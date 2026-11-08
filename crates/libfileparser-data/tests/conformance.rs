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

const GEOJSON: &[u8] = br#"{"type": "FeatureCollection", "features": [
  {"type": "Feature", "id": "a", "geometry": {"type": "Point", "coordinates": [13.4, 52.5]}, "properties": {"name": "Berlin", "pop": 3645000}},
  {"type": "Feature", "geometry": {"type": "LineString", "coordinates": [[2.35, 48.85], [-0.12, 51.5]]}, "properties": {"name": "Paris to London"}}
]}"#;

const X12: &[u8] = b"ISA*00*          *00*          *ZZ*SENDER         *ZZ*RECEIVER       *241108*1200*U*00401*000000001*0*P*>~GS*PO*S*R*20241108*1200*1*X*004010~ST*850*0001~BEG*00*SA*PO1**20241108~SE*3*0001~GE*1*1~IEA*1*000000001~";

const NMEA: &[u8] = b"$GPGGA,123519,4807.038,N,01131.000,E,1,08,0.9,545.4,M,46.9,M,,*47\n$GPRMC,123519,A,4807.038,N,01131.000,E,022.4,084.4,230394,003.1,W*6A\n";

/// A little-endian MO file of `pairs`.
fn mo(pairs: &[(&str, &str)]) -> Vec<u8> {
    let n = pairs.len() as u32;
    let (orig_at, tr_at) = (28u32, 28 + 8 * n);
    let mut strings_at = tr_at + 8 * n;
    let mut head = Vec::new();
    for v in [0x950412deu32, 0, n, orig_at, tr_at, 0, 0] {
        head.extend(v.to_le_bytes());
    }
    let (mut origs, mut trs, mut data) = (Vec::new(), Vec::new(), Vec::new());
    for (o, _) in pairs {
        origs.extend((o.len() as u32).to_le_bytes());
        origs.extend(strings_at.to_le_bytes());
        data.extend(o.as_bytes());
        data.push(0);
        strings_at += o.len() as u32 + 1;
    }
    for (_, t) in pairs {
        trs.extend((t.len() as u32).to_le_bytes());
        trs.extend(strings_at.to_le_bytes());
        data.extend(t.as_bytes());
        data.push(0);
        strings_at += t.len() as u32 + 1;
    }
    [head, origs, trs, data].concat()
}

/// A KMZ: the KML zipped as `doc.kml`.
fn kmz(kml: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut w = zip::ZipWriter::new(&mut buf);
    w.start_file("doc.kml", zip::write::SimpleFileOptions::default()).unwrap();
    w.write_all(kml).unwrap();
    w.finish().unwrap();
    buf.into_inner()
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
            small("events.jsonl", &corpus("data/structured/events.jsonl")),
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
            small("people.tsv", &corpus("data/delimited/people.tsv")),
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
        "geojson" => vec![
            small("a.geojson", GEOJSON),
            big("big.geojson", many(r#"{"type": "FeatureCollection", "features": ["#, r#"{"type": "Feature", "geometry": {"type": "Point", "coordinates": [{i}, 1]}, "properties": {}}"#, ",", "]}", 30)),
        ],
        "topojson" => vec![small("a.topojson", br#"{"type": "Topology", "objects": {"o": {"type": "Point", "coordinates": [1, 2]}}, "arcs": []}"#)
            , big("big.topojson", many(r#"{"type": "Topology", "arcs": [], "objects": {"o": {"type": "GeometryCollection", "geometries": ["#, r#"{"type": "Point", "coordinates": [{i}, 2]}"#, ",", "]}}}", 30))],
        "kml" => vec![small("a.kml", br#"<kml><Placemark><name>A</name><Point><coordinates>1,2</coordinates></Point></Placemark></kml>"#),
            big("big.kml", many("<kml><Document>", "<Placemark><name>{i}</name></Placemark>", "", "</Document></kml>", 30))],
        "kmz" => vec![
            Sample::new("a.kmz", kmz(br#"<kml><Placemark><name>A</name><Point><coordinates>1,2</coordinates></Point></Placemark></kml>"#)).fits_tiny_limits(),
            Sample::new("big.kmz", kmz(&many("<kml><Document>", "<Placemark><name>{i}</name></Placemark>", "", "</Document></kml>", 30))),
        ],
        "gpx" => vec![small("a.gpx", br#"<gpx><wpt lat="1" lon="2"><name>W</name></wpt></gpx>"#),
            big("big.gpx", many("<gpx><trk><trkseg>", r#"<trkpt lat="{i}" lon="1"/>"#, "", "</trkseg></trk></gpx>", 30))],
        "gml" => vec![small("a.gml", br#"<f xmlns:gml="http://www.opengis.net/gml"><gml:featureMember><r><gml:Point><gml:pos>1 2</gml:pos></gml:Point></r></gml:featureMember></f>"#),
            big("big.gml", many(r#"<f xmlns:gml="http://www.opengis.net/gml">"#, "<gml:featureMember><r>{i}</r></gml:featureMember>", "", "</f>", 30))],
        "osm" => vec![small("a.osm", br#"<osm><node id="1" lat="1" lon="2"/></osm>"#),
            big("big.osm", many("<osm>", r#"<node id="{i}" lat="1" lon="2"/>"#, "", "</osm>", 30))],
        "wkt" => vec![small("a.wkt", b"POINT (1 2)\nLINESTRING (1 2, 3 4)\n"), big("big.wkt", many("", "POINT ({i} 1)", "\n", "\n", 30))],
        "nmea" => vec![small("a.nmea", NMEA), big("big.nmea", many("", "$GPGLL,4916.45,N,12311.12,W,2254{i}.00,A,A", "\n", "\n", 30))],
        "rss" => vec![small("a.rss", br#"<rss version="2.0"><channel><title>T</title><item><title>A</title></item></channel></rss>"#),
            big("big.rss", many("<rss><channel>", "<item><title>{i}</title></item>", "", "</channel></rss>", 30))],
        "atom" => vec![small("a.atom", br#"<feed xmlns="http://www.w3.org/2005/Atom"><entry><title>A</title></entry></feed>"#),
            big("big.atom", many(r#"<feed xmlns="http://www.w3.org/2005/Atom">"#, "<entry><title>{i}</title></entry>", "", "</feed>", 30))],
        "m3u" => vec![small("a.m3u", b"#EXTM3U\n#EXTINF:10,One\none.mp3\n"), big("big.m3u8", many("#EXTM3U\n", "#EXTINF:10,T{i}\n{i}.ts", "\n", "\n", 30))],
        "pls" => vec![small("a.pls", b"[playlist]\nFile1=a.mp3\nTitle1=A\nNumberOfEntries=1\n"),
            big("big.pls", many("[playlist]\n", "File{i}=a{i}.mp3", "\n", "\n", 300)).fits_tiny_limits()],
        "xspf" => vec![small("a.xspf", br#"<playlist xmlns="http://xspf.org/ns/0/"><trackList><track><title>A</title></track></trackList></playlist>"#),
            big("big.xspf", many(r#"<playlist xmlns="http://xspf.org/ns/0/"><trackList>"#, "<track><title>{i}</title></track>", "", "</trackList></playlist>", 30))],
        "po" => vec![small("a.po", b"msgid \"Hi\"\nmsgstr \"Hallo\"\n"), big("big.po", many("", "msgid \"m{i}\"\nmsgstr \"t{i}\"", "\n\n", "\n", 30))],
        "mo" => vec![Sample::new("a.mo", mo(&[("Hi", "Hallo")])).fits_tiny_limits(), Sample::new("big.mo", mo(&(0..30).map(|i| (Box::leak(format!("m{i}").into_boxed_str()) as &str, "t")).collect::<Vec<_>>()))],
        "xliff" => vec![small("a.xlf", br#"<xliff version="1.2"><file><body><trans-unit id="1"><source>Hi</source><target>Hallo</target></trans-unit></body></file></xliff>"#),
            big("big.xlf", many(r#"<xliff version="1.2"><file><body>"#, r#"<trans-unit id="{i}"><source>s</source></trans-unit>"#, "", "</body></file></xliff>", 30))],
        "tmx" => vec![small("a.tmx", br#"<tmx><body><tu><tuv xml:lang="en"><seg>Hi</seg></tuv></tu></body></tmx>"#),
            big("big.tmx", many("<tmx><body>", r#"<tu><tuv xml:lang="en"><seg>{i}</seg></tuv></tu>"#, "", "</body></tmx>", 30))],
        "resx" => vec![small("a.resx", br#"<root><data name="A"><value>B</value></data></root>"#),
            big("big.resx", many("<root>", r#"<data name="k{i}"><value>v</value></data>"#, "", "</root>", 30))],
        "strings" => vec![small("a.strings", b"\"a\" = \"b\";\n"), big("big.strings", many("", "\"k{i}\" = \"v\";", "\n", "\n", 300)).fits_tiny_limits()],
        "arb" => vec![small("a.arb", br#"{"@@locale": "en", "hi": "Hi"}"#), big("big.arb", many("{", r#""k{i}": "v""#, ",", "}", 30))],
        "vcard" => vec![small("a.vcf", b"BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Ann\r\nEND:VCARD\r\n"),
            big("big.vcf", many("", "BEGIN:VCARD\r\nFN:P{i}\r\nEND:VCARD", "\r\n", "\r\n", 30))],
        "icalendar" => vec![small("a.ics", b"BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nSUMMARY:Lunch\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"),
            big("big.ics", many("BEGIN:VCALENDAR\r\n", "BEGIN:VEVENT\r\nSUMMARY:E{i}\r\nEND:VEVENT", "\r\n", "\r\nEND:VCALENDAR\r\n", 30))],
        "ldif" => vec![small("a.ldif", b"version: 1\ndn: cn=a,dc=x\ncn: a\n"), big("big.ldif", many("", "dn: cn={i},dc=x\ncn: {i}", "\n\n", "\n", 30))],
        "sql" => vec![small("a.sql", b"CREATE TABLE t (a int);\nINSERT INTO t VALUES (1);\n"), big("big.sql", many("", "INSERT INTO t VALUES ({i});", "\n", "\n", 30))],
        "protobuf" => vec![small("a.proto", b"message A { string b = 1; }"), big("big.proto", many("message A {", "string f{i} = {i};", "\n", "}", 30))],
        "thrift" => vec![small("a.thrift", b"struct A { 1: string b }"), big("big.thrift", many("struct A {", "{i}: string f{i},", "\n", "}", 30))],
        "flatbuffers" => vec![small("a.fbs", b"table A { b:int; }"), big("big.fbs", many("table A {", "f{i}:int;", "\n", "}", 30))],
        "capnp" => vec![small("a.capnp", b"struct A { b @0 :Text; }"), big("big.capnp", many("struct A {", "f{i} @{i} :Text;", "\n", "}", 30))],
        "smithy" => vec![small("a.smithy", b"structure A { b: String }"), big("big.smithy", many("structure A {", "f{i}: String", "\n", "}", 30))],
        "graphql" => vec![small("a.graphql", b"type A { b: Int }"), big("big.graphql", many("type A {", "f{i}: Int", "\n", "}", 30))],
        "prisma" => vec![small("a.prisma", b"model A {\n  id Int @id\n}\n"), big("big.prisma", many("model A {\n", "f{i} Int", "\n", "\n}", 30))],
        "dbml" => vec![small("a.dbml", b"Table a {\n  id int\n}\n"), big("big.dbml", many("Table a {\n", "f{i} int", "\n", "\n}", 30))],
        "avro" => vec![small("a.avsc", br#"{"type": "record", "name": "A", "fields": [{"name": "b", "type": "int"}]}"#),
            big("big.avsc", many(r#"{"type": "record", "name": "A", "fields": ["#, r#"{"name": "f{i}", "type": "int"}"#, ",", "]}", 30))],
        "openapi" => vec![small("api.yaml", b"openapi: 3.0.0\npaths:\n  /a:\n    get: {}\n"), big("big.yaml", many("openapi: 3.0.0\npaths:\n", "  /p{i}:\n    get: {}", "\n", "\n", 30))],
        "xsd" => vec![small("a.xsd", br#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:element name="a" type="xs:string"/></xs:schema>"#),
            big("big.xsd", many(r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">"#, r#"<xs:element name="e{i}"/>"#, "", "</xs:schema>", 30))],
        "turtle" => vec![small("a.ttl", b"@prefix ex: <http://e.com/> .\nex:a ex:b \"c\" .\n"), big("big.nt", many("", "<http://e.com/{i}> <http://e.com/p> \"v\" .", "\n", "\n", 30))],
        "rdfxml" => vec![small("a.rdf", br#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:ex="http://e.com/"><rdf:Description rdf:about="http://e.com/a"><ex:b>c</ex:b></rdf:Description></rdf:RDF>"#),
            big("big.rdf", many(r#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:ex="http://e.com/">"#, r#"<rdf:Description rdf:about="http://e.com/{i}"><ex:b>c</ex:b></rdf:Description>"#, "", "</rdf:RDF>", 30))],
        "trix" => vec![small("a.trix", b"<TriX><graph><triple><uri>http://e.com/a</uri><uri>http://e.com/b</uri><plainLiteral>c</plainLiteral></triple></graph></TriX>"),
            big("big.trix", many("<TriX><graph>", "<triple><uri>http://e.com/{i}</uri><uri>http://e.com/b</uri><plainLiteral>c</plainLiteral></triple>", "", "</graph></TriX>", 30))],
        "jsonld" => vec![small("a.jsonld", br#"{"@context": "https://schema.org", "@id": "x", "name": "A"}"#), big("big.jsonld", many(r#"{"@graph": ["#, r#"{"@id": "n{i}"}"#, ",", "]}", 30))],
        "sparql" => vec![small("q.rq", b"SELECT ?s WHERE { ?s ?p ?o . }"), big("big.rq", many("SELECT * WHERE {\n", "?s{i} ?p ?o .", "\n", "\n}", 30))],
        "dot" => vec![small("a.dot", b"digraph { a -> b }"), big("big.dot", many("digraph {\n", "n{i} -> m{i};", "\n", "\n}", 30))],
        "graphml" => vec![small("a.graphml", br#"<graphml><graph edgedefault="directed"><node id="a"/><edge source="a" target="a"/></graph></graphml>"#),
            big("big.graphml", many("<graphml><graph>", r#"<node id="n{i}"/>"#, "", "</graph></graphml>", 30))],
        "gexf" => vec![small("a.gexf", br#"<gexf><graph><nodes><node id="0" label="A"/></nodes><edges/></graph></gexf>"#),
            big("big.gexf", many("<gexf><graph><nodes>", r#"<node id="{i}"/>"#, "", "</nodes></graph></gexf>", 30))],
        "gxl" => vec![small("a.gxl", br#"<gxl><graph id="g"><node id="a"/></graph></gxl>"#), big("big.gxl", many(r#"<gxl><graph id="g">"#, r#"<node id="n{i}"/>"#, "", "</graph></gxl>", 30))],
        "graph-gml" => vec![small("a.gml", b"graph [ node [ id 1 ] ]"), big("big.gml", many("graph [\n", "node [ id {i} ]", "\n", "\n]", 30))],
        "fasta" => vec![small("a.fa", b">s1 one\nACGT\n"), big("big.fa", many("", ">s{i}\nACGT", "\n", "\n", 30))],
        "fastq" => vec![small("a.fq", b"@r1\nACGT\n+\nIIII\n"), big("big.fq", many("", "@r{i}\nACGT\n+\nIIII", "\n", "\n", 30))],
        "genbank" => vec![small("a.gb", b"LOCUS       X 4 bp DNA linear SYN 01-JAN-2024\nORIGIN\n        1 acgt\n//\n"),
            big("big.gb", many("", "LOCUS       X{i} 4 bp DNA linear SYN 01-JAN-2024\n//", "\n", "\n", 30))],
        "gff" => vec![small("a.gff3", b"##gff-version 3\nchr1\t.\tgene\t1\t9\t.\t+\t.\tID=g1\n"), big("big.gff3", many("##gff-version 3\n", "chr1\t.\tgene\t{i}\t9\t.\t+\t.\tID=g{i}", "\n", "\n", 30))],
        "bed" => vec![small("a.bed", b"chr1\t0\t10\tx\n"), big("big.bed", many("", "chr1\t{i}\t99", "\n", "\n", 30))],
        "vcf" => vec![small("a.vcf", b"##fileformat=VCFv4.2\n#CHROM\tPOS\tID\tREF\tALT\tQUAL\tFILTER\tINFO\n1\t10\t.\tA\tG\t50\tPASS\t.\n"),
            big("big.vcf", many("##fileformat=VCFv4.2\n#CHROM\tPOS\tID\tREF\tALT\tQUAL\tFILTER\tINFO\n", "1\t{i}\t.\tA\tG\t50\tPASS\t.", "\n", "\n", 30))],
        "sam" => vec![small("a.sam", b"@HD\tVN:1.6\nr1\t0\tchr1\t1\t60\t4M\t*\t0\t0\tACGT\tIIII\n"), big("big.sam", many("@HD\tVN:1.6\n", "r{i}\t0\tchr1\t1\t60\t4M\t*\t0\t0\tACGT\tIIII", "\n", "\n", 30))],
        "pdb" => vec![small("a.pdb", b"HEADER    TEST\nATOM      1  N   MET A   1      11.104   6.134  -6.504  1.00  0.00           N\n"),
            big("big.pdb", many("HEADER    TEST\n", "ATOM      1  N   MET A   1      11.104   6.134  -6.504  1.00  0.00           N", "\n", "\n", 30))],
        "cif" => vec![small("a.cif", b"data_x\n_cell.length_a 10\n"), big("big.cif", many("data_x\nloop_\n_atom_site.id\n", "{i}", "\n", "\n", 30))],
        "arff" => vec![small("a.arff", b"@relation r\n@attribute a numeric\n@data\n1\n"), big("big.arff", many("@relation r\n@attribute a numeric\n@data\n", "{i}", "\n", "\n", 30))],
        "dimacs" => vec![small("a.cnf", b"p cnf 2 1\n1 -2 0\n"), big("big.cnf", many("p cnf 99 30\n", "{i} 0", "\n", "\n", 30))],
        "x12" => vec![small("a.x12", X12), big("big.x12", [&X12[..106], &many("", "N1*ST*{i}", "~", "~", 30)[..]].concat())],
        "edifact" => vec![small("a.edi", b"UNA:+.? 'UNB+UNOC:3+A+B'UNZ+0'"), big("big.edi", many("UNA:+.? '", "FTX+AAA+++t{i}", "'", "'", 30))],
        "hl7" => vec![small("a.hl7", b"MSH|^~\\&|APP|FAC|||20241108||ADT^A01|1|P|2.5\r"), big("big.hl7", many("MSH|^~\\&|APP\r", "OBX|{i}|ST|x", "\r", "\r", 30))],
        "ofx" => vec![small("a.ofx", b"OFXHEADER:100\n<OFX><BANKMSGSRSV1></BANKMSGSRSV1></OFX>"), big("big.ofx", many("OFXHEADER:100\n<OFX>", "<STMTTRN><TRNAMT>{i}</STMTTRN>", "", "</OFX>", 30))],
        "qif" => vec![small("a.qif", b"!Type:Bank\nD01/02/2024\nT-5.00\n^\n"), big("big.qif", many("!Type:Bank\n", "D01/02/2024\nT-{i}\n^", "\n", "\n", 30))],
        "fix" => vec![small("a.fix", b"8=FIX.4.4|9=5|35=0|10=000|\n"), big("big.fix", many("", "8=FIX.4.4|35=0|34={i}|10=000|", "\n", "\n", 30))],
        "swift" => vec![small("a.fin", b"{1:F01BANKBEBBAXXX0000000000}{2:I103BANKDEFFXXXXN}{4:\n:20:REF\n-}"),
            big("big.fin", many("{1:F01BANKBEBBAXXX0000000000}{2:I103BANKDEFFXXXXN}{4:\n", ":20:R{i}", "\n", "\n-}", 30))],
        "xbrl" => vec![small("a.xbrl", br#"<xbrli:xbrl xmlns:xbrli="http://www.xbrl.org/2003/instance"><x contextRef="c">1</x></xbrli:xbrl>"#),
            big("big.xbrl", many(r#"<xbrli:xbrl xmlns:xbrli="http://www.xbrl.org/2003/instance">"#, r#"<x contextRef="c">{i}</x>"#, "", "</xbrli:xbrl>", 30))],
        "hcl" => vec![small("a.tf", b"resource \"x\" \"y\" {\n  a = 1\n}\n"), big("big.tf", many("", "variable \"v{i}\" { default = {i} }", "\n", "\n", 30))],
        "cue" => vec![small("a.cue", b"a: 1\nb: {\n  c: \"x\"\n}\n"), big("big.cue", many("", "f{i}: {i}", "\n", "\n", 30))],
        "dhall" => vec![small("a.dhall", b"{ a = 1, b = \"x\" }\n"), big("big.dhall", many("{ ", "f{i} = {i}", ", ", " }", 30))],
        "nickel" => vec![small("a.ncl", b"{ a = 1 }\n"), big("big.ncl", many("{ ", "f{i} = {i}", ", ", " }", 30))],
        "kdl" => vec![small("a.kdl", b"package { name \"x\" }\n"), big("big.kdl", many("", "node{i} {i}", "\n", "\n", 30))],
        "cron" => vec![small("a.cron", b"0 2 * * * /backup.sh\n"), big("big.cron", many("", "{i} * * * * job{i}", "\n", "\n", 30))],
        "sgml" => vec![small("a.sgml", b"<doc><p>hi</doc>"), big("big.sgml", many("<doc>", "<p>{i}</p>", "", "</doc>", 30))],
        "fixed-width" => vec![small("a.fwf", b"NAME  AGE\nAnn    42\n"), big("big.fwf", many("NAME  AGE\n", "Ann    {i}", "\n", "\n", 30))],
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

fn table(doc: &libfileparser_core::Document, cx: &ParseContext) -> (libfileparser_core::TableInfo, Vec<Vec<Cell>>) {
    let Ok(Content::Table(t)) = doc.facet(0, cx) else { panic!() };
    (t.info(), t.rows(0..100).unwrap())
}

fn meta<'a>(doc: &'a libfileparser_core::Document, key: &str) -> Option<&'a str> {
    doc.metadata().iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

#[test]
fn geojson_features_with_their_properties_and_bounds() {
    let cx = ParseContext::new();
    let doc = libfileparser_data::open(&FileSource::from_bytes("cities.json", GEOJSON.to_vec()), &cx).unwrap();
    assert_eq!(doc.format().reader, "geojson", "GeoJSON in a .json is GeoJSON");
    assert_eq!(facets(&doc)[..2], [(Role::Primary, "Features"), (Role::Structure, "Tree")]);
    let (info, rows) = table(&doc, &cx);
    assert_eq!(info.names(), ["ID", "Geometry", "Points", "name", "pop"]);
    assert_eq!(info.columns[4].kind, ValueKind::Number);
    assert_eq!(rows[0], [Cell::Text("a".into()), Cell::Text("Point".into()), Cell::Int(1), Cell::Text("Berlin".into()), Cell::Int(3645000)]);
    assert_eq!(rows[1][2], Cell::Int(2));
    assert_eq!(rows[1][4], Cell::Null);
    assert_eq!(meta(&doc, "Bounds"), Some("-0.12, 48.85 to 13.4, 52.5"));
    let links = doc.links(&Place::Row { facet: 0, row: 1 }, &cx).unwrap();
    assert!(links.contains(&Place::Lines(2..3)), "{links:?}");
    // A bare geometry is one feature.
    let one = libfileparser_data::open(&FileSource::from_bytes("p.geojson", br#"{"type": "Point", "coordinates": [1, 2]}"#.to_vec()), &cx).unwrap();
    assert_eq!(table(&one, &cx).1, [vec![Cell::Text("Point".into()), Cell::Int(1)]]);
}

#[test]
fn kml_kmz_and_gpx() {
    let cx = ParseContext::new();
    let kml = br#"<?xml version="1.0"?>
<kml xmlns="http://www.opengis.net/kml/2.2"><Document><name>Trip</name><Folder><name>Stops</name>
  <Placemark><name>Home</name><Point><coordinates>-122.08,37.42,0</coordinates></Point>
    <ExtendedData><Data name="rating"><value>5</value></Data></ExtendedData></Placemark>
  <Placemark><name>Road</name><LineString><coordinates>-122.08,37.42 -122.09,37.43 -122.10,37.44</coordinates></LineString></Placemark>
</Folder></Document></kml>"#;
    let doc = libfileparser_data::open(&FileSource::from_bytes("trip.kml", kml.to_vec()), &cx).unwrap();
    let (info, rows) = table(&doc, &cx);
    assert_eq!(info.names(), ["Name", "Geometry", "Points", "Folder", "Description", "rating"]);
    assert_eq!(rows[0][..4], [Cell::Text("Home".into()), Cell::Text("Point".into()), Cell::Int(1), Cell::Text("Stops".into())]);
    assert_eq!(rows[0][5], Cell::Text("5".into()));
    assert_eq!(rows[1][2], Cell::Int(3));
    // Zipped: the same, without line links (the lines are the KML's, not the file's).
    let zipped = libfileparser_data::open(&FileSource::from_bytes("trip.kmz", kmz(kml)), &cx).unwrap();
    assert_eq!((zipped.format().reader, table(&zipped, &cx).1.len()), ("kmz", 2));
    assert!(zipped.links(&Place::Row { facet: 0, row: 0 }, &cx).unwrap().iter().all(|p| !matches!(p, Place::Lines(_))));

    let gpx = br#"<?xml version="1.0"?>
<gpx version="1.1" creator="Watch"><wpt lat="47.0" lon="8.0"><ele>440</ele><name>Start</name></wpt>
<trk><name>Run</name><trkseg>
  <trkpt lat="47.0" lon="8.0"><time>2024-11-08T07:00:00Z</time></trkpt>
  <trkpt lat="47.01" lon="8.0"><time>2024-11-08T07:05:00Z</time></trkpt>
</trkseg></trk></gpx>"#;
    let doc = libfileparser_data::open(&FileSource::from_bytes("run.gpx", gpx.to_vec()), &cx).unwrap();
    let (info, rows) = table(&doc, &cx);
    assert_eq!(info.names()[..6], ["Type", "Name", "Points", "Start", "End", "Distance (km)"]);
    assert_eq!(rows[0][..3], [Cell::Text("Waypoint".into()), Cell::Text("Start".into()), Cell::Int(1)]);
    assert_eq!(rows[0][6], Cell::Float(440.0));
    assert_eq!(rows[1][..5], [Cell::Text("Track".into()), Cell::Text("Run".into()), Cell::Int(2), Cell::Date("2024-11-08T07:00:00Z".into()), Cell::Date("2024-11-08T07:05:00Z".into())]);
    assert_eq!(rows[1][5], Cell::Float(1.11));
    assert_eq!(meta(&doc, "Recorded by"), Some("Watch"));
}

#[test]
fn osm_wkt_nmea_and_gml() {
    let cx = ParseContext::new();
    let osm = br#"<?xml version="1.0"?><osm version="0.6" generator="test">
<node id="1" lat="51.5" lon="-0.1"><tag k="name" v="Pub"/><tag k="amenity" v="pub"/></node>
<node id="2" lat="51.6" lon="-0.2"/>
<way id="10"><nd ref="1"/><nd ref="2"/><tag k="highway" v="residential"/><tag k="name" v="High St"/></way></osm>"#;
    let doc = libfileparser_data::open(&FileSource::from_bytes("map.osm", osm.to_vec()), &cx).unwrap();
    let (info, rows) = table(&doc, &cx);
    assert_eq!(info.names(), ["Kind", "ID", "Name", "Latitude", "Longitude", "Members", "Tags", "amenity", "highway"]);
    assert_eq!(rows[2][..3], [Cell::Text("way".into()), Cell::Int(10), Cell::Text("High St".into())]);
    assert_eq!(rows[2][5], Cell::Int(2));
    assert_eq!(meta(&doc, "Bounds"), Some("-0.2, 51.5 to -0.1, 51.6"));

    let doc = libfileparser_data::open(&FileSource::from_bytes("shapes.wkt", b"POINT (1 2)\nPOLYGON ((0 0, 4 0, 4 4, 0 0))\n".to_vec()), &cx).unwrap();
    assert_eq!(table(&doc, &cx).1[1], [Cell::Text("POLYGON".into()), Cell::Int(4)]);
    assert_eq!(doc.links(&Place::Row { facet: 0, row: 1 }, &cx).unwrap(), [Place::Lines(1..2)]);

    let doc = libfileparser_data::open(&FileSource::from_bytes("drive.txt", NMEA.to_vec()), &cx).unwrap();
    assert_eq!(doc.format().reader, "nmea", "NMEA is told by its content");
    let (_, rows) = table(&doc, &cx);
    assert_eq!(rows[1][0], Cell::Text("GPRMC".into()));
    assert_eq!(rows[1][2], Cell::Date("2094-03-23".into()));
    assert_eq!(rows[0][10], Cell::Text("ok".into()));

    let gml = br#"<?xml version="1.0"?><wfs:FeatureCollection xmlns:wfs="http://www.opengis.net/wfs" xmlns:gml="http://www.opengis.net/gml">
<gml:featureMember><river gml:id="r1"><name>Thames</name><gml:LineString><gml:posList>0 0 1 1 2 2</gml:posList></gml:LineString></river></gml:featureMember>
</wfs:FeatureCollection>"#;
    let doc = libfileparser_data::open(&FileSource::from_bytes("rivers.gml", gml.to_vec()), &cx).unwrap();
    let (info, rows) = table(&doc, &cx);
    assert_eq!(info.names(), ["Feature", "ID", "Geometry", "Points", "name"]);
    assert_eq!(rows[0], [Cell::Text("river".into()), Cell::Text("r1".into()), Cell::Text("LineString".into()), Cell::Int(3), Cell::Text("Thames".into())]);
    // Graph Modelling Language is another .gml: not this.
    assert!(libfileparser_data::detect(b"graph [ node [ id 1 ] ]", "g.gml").iter().all(|(r, _)| r.info().id != "gml"));
}

/// Every reader, its samples and the corpus files it reads damaged thousands of ways
/// (`LFP_FUZZ_ITERATIONS` for more): nothing panics or hangs.
#[test]
fn every_reader_survives_fuzzing() {
    let corpus_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/files");
    libfileparser_testkit::assert_fuzz_clean(libfileparser_data::readers(), |id| {
        let reader = libfileparser_data::readers().iter().find(|r| r.info().id == id).copied().expect("a reader");
        let mut s = samples(id);
        s.extend(libfileparser_testkit::corpus_samples(&corpus_dir, reader));
        s
    });
}
