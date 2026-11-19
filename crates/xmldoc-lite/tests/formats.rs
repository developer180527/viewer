//! Every vocabulary, from outside the crate: what it writes, what it detects, and that
//! hostile XML stays quick and never panics.

use xmldoc_lite::{Presentation, docbook, fictionbook, is_docbook, is_fictionbook, is_tei, presentation, tei};

fn has(html: &str, wants: &[&str]) {
    for w in wants {
        assert!(html.contains(w), "{w} in {html}");
    }
}

#[test]
fn docbook_in_xml_and_sgml() {
    let d = docbook(r#"<?xml version="1.0"?><article xmlns="http://docbook.org/ns/docbook" xmlns:xlink="http://www.w3.org/1999/xlink"><info><title>The Article</title></info>
<section><title>First</title><para><emphasis role="bold">b</emphasis> <command>ls</command> <link xlink:href="https://x.org">l</link><footnote><para>n</para></footnote></para>
<itemizedlist><listitem><para>one</para></listitem></itemizedlist><programlisting language="c">int x;</programlisting>
<variablelist><varlistentry><term>t</term><listitem><para>def</para></listitem></varlistentry></variablelist>
<informaltable><tgroup><thead><row><entry>a</entry></row></thead><tbody><row><entry>1</entry></row></tbody></tgroup></informaltable>
<section><title>Nested</title><para>deeper</para></section><indexterm><primary>hidden</primary></indexterm></section></article>"#);
    assert_eq!(d.title.as_deref(), Some("The Article"));
    has(&d.html, &["<h1>The Article</h1>", "<h2>First</h2>", "<strong>b</strong>", "<code>ls</code>", "<a href=\"https://x.org\">l</a>", "<sup>[n]</sup>", "<li>", "language-c", "<strong>t</strong>", "<blockquote>", "<thead><tr><td>a</td>", "<h3>Nested</h3>"]);
    assert!(!d.html.contains("hidden"));
    let sgml = "<!DOCTYPE book PUBLIC \"-//OASIS//DTD DocBook V4.1//EN\">\n<book><title>Guide</title><chapter><title>Start</title><para>One.\n<para>Two.\n</chapter></book>\n";
    assert!(is_docbook(sgml.as_bytes()));
    let g = docbook(sgml);
    assert_eq!(g.title.as_deref(), Some("Guide"));
    has(&g.html, &["<h1>Guide</h1>", "<h2>Start</h2>", "One.", "Two."]);
}

#[test]
fn tei_and_fictionbook() {
    let t = tei(r#"<TEI xmlns="http://www.tei-c.org/ns/1.0"><teiHeader><fileDesc><titleStmt><title>A Poem</title></titleStmt></fileDesc></teiHeader><text><body><div><head>One</head><p><hi rend="italic">i</hi><note>n</note><lb/>next</p><lg><l>line</l></lg></div></body></text></TEI>"#);
    assert_eq!(t.title.as_deref(), Some("A Poem"));
    has(&t.html, &["<h1>A Poem</h1>", "<h1>One</h1>", "<em>i</em>", "<sup>[n]</sup>", "<br>", "<blockquote><p>line</p></blockquote>"]);
    let f = fictionbook(r##"<FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0" xmlns:l="http://www.w3.org/1999/xlink"><description><title-info><author><first-name>A</first-name><last-name>B</last-name></author><book-title>Book</book-title></title-info></description><body><section><title><p>Ch</p></title><p><strong>s</strong><a l:href="#n1">1</a></p><image l:href="#p.jpg"/></section></body><binary id="p.jpg">AAAA</binary></FictionBook>"##);
    assert_eq!(f.title.as_deref(), Some("Book"));
    has(&f.html, &["<h1>Book</h1>", "<em>A B</em>", "<h1>Ch</h1>", "<strong>s</strong>", "<sup>1</sup>", "[Image: p.jpg]"]);
    assert!(!f.html.contains("AAAA"));
}

#[test]
fn a_presentations_slides() {
    let Presentation { html, slides } = presentation(r#"<office:document-content xmlns:office="o" xmlns:draw="d" xmlns:text="t" xmlns:presentation="p" xmlns:xlink="x"><office:automatic-styles><s>hidden</s></office:automatic-styles><office:body><office:presentation>
<draw:page draw:name="Intro"><draw:frame presentation:class="title"><draw:text-box><text:p>Welcome</text:p></draw:text-box></draw:frame><draw:frame><draw:image xlink:href="Pictures/a.png"/></draw:frame><presentation:notes><draw:frame><draw:text-box><text:p>Say hi.</text:p></draw:text-box></draw:frame></presentation:notes></draw:page>
<draw:page draw:name="page2"><presentation:notes><draw:page-thumbnail/></presentation:notes></draw:page></office:presentation></office:body></office:document-content>"#);
    assert_eq!(slides, 2);
    has(&html.html, &["<h2>1. Intro</h2>", "<strong>Welcome</strong>", "Pictures/a.png", "<em>Notes</em>", "Say hi.", "<h2>2.</h2>"]);
    assert_eq!(html.html.matches("Notes").count(), 1, "blank notes dropped: {}", html.html);
    assert!(!html.html.contains("hidden"));
    assert_eq!(xmldoc_lite::element_text("<m><dc:title>T &amp; U</dc:title></m>", "dc:title").as_deref(), Some("T & U"));
}

#[test]
fn detection() {
    assert!(is_tei(br#"<?xml version="1.0"?><TEI xmlns="http://www.tei-c.org/ns/1.0">"#));
    assert!(is_fictionbook(b"<?xml version=\"1.0\"?>\n<!-- c --><FictionBook>"));
    for other in [&b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>"[..], b"<book/>", b"<rss/>", b"not xml", b""] {
        assert!(!is_docbook(other) && !is_tei(other) && !is_fictionbook(other), "{:?}", String::from_utf8_lossy(other));
    }
}

/// Damage, depth and entity tricks: quick, no panic, and nothing expanded.
#[test]
fn hostile_xml() {
    let cut = docbook("<article xmlns=\"http://docbook.org/ns/docbook\"><para>kept</para><para>cut <");
    assert!(cut.html.contains("kept"));
    let deep = format!("<article xmlns=\"http://docbook.org/ns/docbook\">{}x{}</article>", "<section><para>".repeat(20_000), "</para></section>".repeat(20_000));
    let laughs = "<?xml version=\"1.0\"?><!DOCTYPE lolz [<!ENTITY lol \"lol\"><!ENTITY lol2 \"&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;\"><!ENTITY lol3 \"&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;\">]><article><para>&lol3;</para></article>";
    let noise = String::from_utf8_lossy(&(0..=255u8).cycle().take(100_000).collect::<Vec<_>>()).into_owned();
    let unclosed = "<p>".repeat(50_000);
    for input in [deep.as_str(), laughs, noise.as_str(), unclosed.as_str()] {
        let t = std::time::Instant::now();
        for read in [docbook, tei, fictionbook] {
            let out = read(input);
            assert!(out.html.len() < input.len() * 8 + 1024, "no expansion");
        }
        let _ = presentation(input);
        assert!(t.elapsed().as_secs() < 5, "{:?}", t.elapsed());
    }
    assert!(!docbook(laughs).html.contains("lollol"), "entities aren't expanded");
}
