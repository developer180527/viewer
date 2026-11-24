//! Every reader, from outside the crate: what it writes for the markup documents use,
//! and that hostile input stays quick and never panics.

use markup_lite::{Html, creole, dokuwiki, fountain, gemtext, haddock, jira, latex, man, mediawiki, muse, pod, texinfo, textile, tikiwiki, twiki, txt2tags, vimwiki, xwiki, zimwiki};

fn has(out: &Html, wants: &[&str]) {
    for w in wants {
        assert!(out.html.contains(w), "{w} in {}", out.html);
    }
}

fn lacks(out: &Html, nots: &[&str]) {
    for w in nots {
        assert!(!out.html.contains(w), "{w} not in {}", out.html);
    }
}

#[test]
fn wikis() {
    let m = mediawiki("{{Infobox|x={{y}}}}\n== Head ==\n'''bold''' ''it'' [[Page|a page]] [https://x.org site]<ref>note</ref>\n* one\n** two\n# first\n{|\n! a !! b\n|-\n| 1 || 2\n|}\n[[File:P.png|thumb|Cap]]\n[[Category:C]]\n");
    has(&m, &["<h2>Head</h2>", "<strong>bold</strong>", "<em>it</em>", "<a href=\"Page\">a page</a>", "<a href=\"https://x.org\">site</a>", "<sup>[note]</sup>", "<ul><li>one<ul><li>two", "<ol><li>first", "<th>a</th>", "<td>2</td>", "<img src=\"P.png\" alt=\"Cap\">"]);
    lacks(&m, &["Infobox", "Category"]);
    has(&creole("= Top\n**b** //i// {{{c}}} [[https://x.org|l]]\n|=a|=b|\n|1|2|\n"), &["<h1>Top</h1>", "<strong>b</strong>", "<em>i</em>", "<code>c</code>", "<a href=\"https://x.org\">l</a>", "<th>a</th>"]);
    let d = dokuwiki("====== Top ======\n**b** //i// ''c''\n  * one\n  - first\n<code rust>\nfn x() {}\n</code>\n");
    has(&d, &["<h1>Top</h1>", "<code>c</code>", "<ul><li>one", "<ol><li>first", "language-rust"]);
    has(&jira("h2. Top\n*b* _i_ {{c}} [l|https://x.org]\n{code:java}\nint x;\n{code}\n"), &["<h2>Top</h2>", "<strong>b</strong>", "<code>c</code>", "language-java"]);
    // An anchor is a place to link to, not text.
    let a = jira("h1. {anchor:top}Headers\n");
    has(&a, &["<h1>Headers</h1>"]);
    lacks(&a, &["anchor"]);
    let t = txt2tags("My Title\n\n\n= S =\n**b** //i// ``c``\n+ first\n");
    has(&t, &["<h1>My Title</h1>", "<h1>S</h1>", "<ol><li>first"]);
    assert_eq!(t.title.as_deref(), Some("My Title"));
    let mu = muse("#title Doc\n\n* Top\n*em* **strong** =c=\n");
    has(&mu, &["<h1>Top</h1>", "<em>em</em>", "<strong>strong</strong>", "<code>c</code>"]);
    assert_eq!(mu.title.as_deref(), Some("Doc"));
    has(&haddock("= Top\n/em/ __strong__ @c@\n\n> main = 1\n"), &["<em>em</em>", "<strong>strong</strong>", "<code>c</code>", "main = 1"]);
    let x = xwiki("{{toc/}}\n= Top =\n##c## [[l>>https://x.org]]\n");
    has(&x, &["<code>c</code>", "https://x.org"]);
    lacks(&x, &["toc"]);
    has(&tikiwiki("!! Two\n__b__ ''i''\n"), &["<h2>Two</h2>", "<strong>b</strong>", "<em>i</em>"]);
    let tw = twiki("---++ Two\n*b* =c= %TOC%\n   * one\n");
    has(&tw, &["<h2>Two</h2>", "<strong>b</strong>", "<code>c</code>", "<li>one"]);
    lacks(&tw, &["TOC"]);
    let z = zimwiki("Content-Type: text/x-zim-wiki\n\n====== Top ======\n[ ] todo\n");
    has(&z, &["<h1>Top</h1>", "☐ todo"]);
    lacks(&z, &["Content-Type"]);
    has(&vimwiki("= Top =\n*b* `c`\n| a | b |\n|---|---|\n| 1 | 2 |\n"), &["<h1>Top</h1>", "<strong>b</strong>", "<code>c</code>", "<th>a</th>"]);
    let g = gemtext("# Cap\n*kept* <b>\n=> gemini://x.org/ Link\n```sh\nls\n```\n");
    has(&g, &["<h1>Cap</h1>", "*kept* &lt;b&gt;", "<a href=\"gemini://x.org/\">Link</a>", "language-sh"]);
}

#[test]
fn textile_and_text_marks_stay_text() {
    let t = textile("h1. Head\n\n*b* _i_ @c@ \"l\":https://x.org 2*3 a-b x_y\n\n|_. a |_. b |\n| 1 | 2 |\n");
    has(&t, &["<h1>Head</h1>", "<strong>b</strong>", "<em>i</em>", "<code>c</code>", "<a href=\"https://x.org\">l</a>", "2*3 a-b x_y", "<th>a</th>"]);
}

#[test]
fn manuals_and_sources() {
    let m = man(".TH LS 1\n.SH NAME\nls \\- list\n.SH OPTIONS\n.TP\n.B \\-a\nall\n.EX\nls \\-a\n.EE\n");
    has(&m, &["<h1>LS(1)</h1>", "<h2>NAME</h2>", "ls - list", "<strong>-a</strong>", "<blockquote>", "<pre>ls -a</pre>"]);
    assert!(markup_lite::starts_like_a_manual(b".\\\" comment\n.TH X 1\n"));
    assert!(!markup_lite::starts_like_a_manual(b"Some prose.\n"));
    let md = man(".Dd 2026\n.Dt CAT 1\n.Sh NAME\n.Nm cat\n.Nd join\n.Sh SYNOPSIS\n.Nm\n.Op Fl n\n.Ar file\n");
    has(&md, &["<h1>cat(1)</h1>", "<strong>cat</strong>", "— join", "[<strong>-n</strong>]", "<em>file</em>"]);
    // Bullet and numbered lists are lists, as mandoc shows them, in man and mdoc.
    let lists = man(".TH X 1\n.SH L\n.IP \\(bu 2\none\n.IP \\(bu 2\ntwo\n.PP\nAfter.\n.IP 1. 3\nfirst\n.IP 2. 3\nsecond\n.PP\nEnd.\n");
    has(&lists, &["<ul><li><p>one</p></li><li><p>two</p></li></ul><p>After.</p>", "<ol><li><p>first</p></li><li><p>second</p></li></ol><p>End.</p>"]);
    lacks(&lists, &["•"]);
    let bl = man(".Dd 2026\n.Dt X 1\n.Sh L\n.Bl -enum\n.It\nfirst\n.It\nsecond\n.El\n.Bl -bullet\n.It\nthree\n.El\n");
    has(&bl, &["<ol><li><p>first</p></li><li><p>second</p></li></ol>", "<ul><li><p>three</p></li></ul>"]);
    // A table's text blocks (T{ … T}) are cells over several lines.
    let tbl = man(".TH X 1\n.TS\ntab(@);\ncw(10n) lw(9n).\nT{\nCentered\nHeader\nT}@T{\nLeft\nT}\n_\nA@B\n.TE\n");
    has(&tbl, &["<tr><td>Centered Header</td><td>Left</td></tr><tr><td>A</td><td>B</td></tr>"]);
    let l = latex("\\documentclass{article}\\title{T}\\begin{document}\\maketitle\\section{S}A \\textbf{b} \\emph{i}, $x^2$.\\begin{itemize}\\item one\\end{itemize}\\end{document}");
    has(&l, &["<h1>T</h1>", "<h2>S</h2>", "<strong>b</strong>", "<em>i</em>", "<code>x^2</code>", "<li><p> one"]);
    lacks(&l, &["documentclass"]);
    let ti = texinfo("@settitle M\n@chapter C\n@code{x} @emph{y}\n@example\na @@ b\n@end example\n");
    has(&ti, &["<h1>M</h1>", "<h1>C</h1>", "<code>x</code>", "<em>y</em>", "a @ b"]);
    let p = pod("=head1 NAME\n\nFoo - bar\n\n=head2 Use\n\nB<b> I<< i >> C<c> E<lt>\n\n    verbatim\n\n=cut\nnot pod\n");
    has(&p, &["<h1>NAME</h1>", "<strong>b</strong>", "<em>i</em>", "<code>c</code>", "&lt;", "<pre>    verbatim</pre>"]);
    lacks(&p, &["not pod"]);
    assert_eq!(p.title.as_deref(), Some("Foo"));
    let f = fountain("Title: Play\n\nINT. ROOM - DAY\n\nBOB\n(quiet)\nHi.\n\nCUT TO:\n");
    has(&f, &["<h1>Play</h1>", "<h2>INT. ROOM - DAY</h2>", "<strong>BOB</strong>", "<em>(quiet)</em>", "Hi.", "<strong>CUT TO:</strong>"]);
}

/// Every reader on input built to be slow or deep: quick, and no panic.
#[test]
fn hostile_input_is_quick() {
    type Read = fn(&str) -> Html;
    let readers: &[(&str, Read)] = &[
        ("mediawiki", mediawiki), ("creole", creole), ("dokuwiki", dokuwiki), ("jira", jira), ("txt2tags", txt2tags), ("muse", muse), ("haddock", haddock), ("xwiki", xwiki),
        ("tikiwiki", tikiwiki), ("twiki", twiki), ("zimwiki", zimwiki), ("vimwiki", vimwiki), ("gemtext", gemtext), ("textile", textile), ("man", man), ("latex", latex),
        ("texinfo", texinfo), ("pod", pod), ("fountain", fountain),
    ];
    let inputs = [
        "*a _b /c '' ".repeat(5_000),
        "[[".repeat(10_000),
        "{{".repeat(10_000) + &"}}".repeat(3),
        "\\textbf{".repeat(5_000),
        "B<".repeat(5_000),
        "* ".repeat(2_000) + "deep",
        format!("{}\n", "x".repeat(200_000)),
        ".RS\n".repeat(5_000),
        "@code{".repeat(5_000),
        "=".repeat(10_000),
        String::from_utf8_lossy(&(0..=255u8).cycle().take(50_000).collect::<Vec<_>>()).into_owned(),
    ];
    for (name, read) in readers {
        for (n, input) in inputs.iter().enumerate() {
            let t = std::time::Instant::now();
            let out = std::panic::catch_unwind(|| read(input)).unwrap_or_else(|_| panic!("{name} panicked on input {n}"));
            assert!(t.elapsed().as_secs() < 5, "{name} took {:?} on input {n}", t.elapsed());
            if std::env::var("MARKUP_TIMES").is_ok() && t.elapsed().as_millis() > 300 {
                println!("SLOW {name} input {n}: {:?}", t.elapsed());
            }
            assert!(!out.html.contains("<script"), "{name}");
        }
    }
}
