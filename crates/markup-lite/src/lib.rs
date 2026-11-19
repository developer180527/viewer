//! Plain-text markups that no maintained Rust crate reads, as HTML: a small,
//! embeddable reader of each, with no dependencies.
//!
//! - **Wikis:** [`mediawiki`], [`creole`], [`dokuwiki`], [`jira`], [`txt2tags`],
//!   [`muse`], [`haddock`], [`xwiki`], [`tikiwiki`], [`twiki`], [`zimwiki`],
//!   [`vimwiki`] and [`gemtext`], on one block writer and inline engine.
//! - **Textile** ([`textile`]).
//! - **Manual pages** ([`man`]): roff with the `man` or BSD `mdoc` macros.
//! - **LaTeX and Texinfo** ([`latex`], [`texinfo`]).
//! - **POD and Fountain** ([`pod`], [`fountain`]): Perl's documentation, screenplays.
//!
//! Each reads what documents commonly use (headings, paragraphs, lists, tables, code,
//! quotes, notes, links, images and the usual marks) and writes it as HTML, which any
//! HTML parser turns into the caller's document model. Nothing a document contains is
//! run, expanded or fetched: a template, macro, `\input` or `@include` is skipped. The
//! HTML is the reader's own: text is escaped, and only the elements it writes appear.
//!
//! ```
//! let page = markup_lite::mediawiki("== Hello ==\n'''bold''' and [https://example.org a link]\n");
//! assert_eq!(page.html, "<h2>Hello</h2><p><strong>bold</strong> and <a href=\"https://example.org\">a link</a></p>");
//! ```
//!
//! Every reader takes the whole text, so the caller bounds its size; each is linear in
//! it (a mark's close is looked for only so far, and nesting is capped), which the
//! fuzzing of its user, libfileparser-docs, checks.

mod latex;
mod pod;
mod roff;
mod textile;
mod wiki;

pub use latex::{latex, texinfo};
pub use pod::{fountain, pod};
pub use roff::{man, starts_like_a_manual};
pub use textile::textile;
pub use wiki::{creole, dokuwiki, gemtext, haddock, jira, mediawiki, muse, tikiwiki, twiki, txt2tags, vimwiki, xwiki, zimwiki};

/// A document as HTML, its title (from its first top-level heading, or what the
/// format names as its title), and what was shown differently from how it's written.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Html {
    pub html: String,
    pub title: Option<String>,
    pub warnings: Vec<String>,
}

/// One line of plain text as HTML: escaped, its bare `http(s)://` addresses linked.
pub fn linkify(line: &str) -> String {
    wiki::inline(line, &[])
}

/// `s` with HTML's special characters escaped.
pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

/// The text of `html`: its tags removed, the entities [`esc`] writes undone.
pub fn strip_tags(html: &str) -> String {
    let mut out = String::new();
    let mut tag = false;
    for c in html.chars() {
        match c {
            '<' => tag = true,
            '>' => tag = false,
            c if !tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&amp;", "&")
}
