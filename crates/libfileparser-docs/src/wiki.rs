//! Wiki and lightweight markups with no maintained Rust parser, read by `markup-lite`
//! (our own: MediaWiki, Creole, DokuWiki, Jira, txt2tags, Muse, Haddock, XWiki,
//! TikiWiki, TWiki, Zim, Vimwiki, Gemtext) into HTML, which becomes formatted text
//! as every markup does (`crate::markup`).

use libfileparser_core::Result;

use crate::markup::{Rendered, markup};

markup!(
    /// MediaWiki's wikitext (Wikipedia's markup): headings, lists, tables, code,
    /// links, files and marks; templates are skipped.
    MediaWikiReader, MEDIAWIKI, "mediawiki", "MediaWiki", "MediaWiki page", ["mediawiki", "wiki", "wikitext", "mw"], ["text/x-wiki"], mediawiki
);

markup!(
    /// Creole 1.0, the common wiki markup.
    CreoleReader, CREOLE, "creole", "Creole", "Creole page", ["creole"], ["text/x-creole"], creole
);

markup!(
    /// DokuWiki's markup.
    DokuWikiReader, DOKUWIKI, "dokuwiki", "DokuWiki", "DokuWiki page", ["dokuwiki", "doku"], ["text/x-dokuwiki"], dokuwiki
);

markup!(
    /// Jira's (Atlassian's) text formatting.
    JiraReader, JIRA, "jira", "Jira", "Jira text", ["jira"], ["text/x-jira"], jira
);

markup!(
    /// txt2tags' markup.
    Txt2tagsReader, TXT2TAGS, "txt2tags", "txt2tags", "txt2tags document", ["t2t"], ["text/x-txt2tags"], txt2tags
);

markup!(
    /// Emacs Muse (and Amusewiki's) markup.
    MuseReader, MUSE, "muse", "Muse", "Muse document", ["muse"], ["text/x-muse"], muse
);

markup!(
    /// Haddock's markup (Haskell's documentation comments, in a file of its own).
    HaddockReader, HADDOCK, "haddock", "Haddock", "Haddock document", ["haddock"], ["text/x-haddock"], haddock
);

markup!(
    /// XWiki 2.x syntax.
    XWikiReader, XWIKI, "xwiki", "XWiki", "XWiki page", ["xwiki"], ["text/x-xwiki"], xwiki
);

markup!(
    /// TikiWiki's markup.
    TikiWikiReader, TIKIWIKI, "tikiwiki", "TikiWiki", "TikiWiki page", ["tikiwiki", "tiki"], ["text/x-tikiwiki"], tikiwiki
);

markup!(
    /// TWiki's (and Foswiki's) topic markup.
    TWikiReader, TWIKI, "twiki", "TWiki", "TWiki topic", ["twiki"], ["text/x-twiki"], twiki
);

markup!(
    /// Zim's wiki pages.
    ZimWikiReader, ZIMWIKI, "zimwiki", "Zim", "Zim page", ["zim", "zimwiki"], ["text/x-zim-wiki"], zimwiki
);

markup!(
    /// Vimwiki's default syntax.
    VimwikiReader, VIMWIKI, "vimwiki", "Vimwiki", "Vimwiki page", ["vimwiki"], ["text/x-vimwiki"], vimwiki, guess = vimwiki_by_content
);

/// A `.wiki` file (Vimwiki's default name, and MediaWiki's) whose lines read as Vimwiki:
/// its `%title` and `{{{` blocks, `*bold*` and `- [ ]` checkboxes, more than MediaWiki's
/// `'''bold'''`, `''italic''`, `{{templates}}` and `{|` tables.
fn vimwiki_by_content(header: &[u8], name: &str) -> bool {
    if !name.to_ascii_lowercase().ends_with(".wiki") {
        return false;
    }
    let text = String::from_utf8_lossy(header);
    let (mut vim, mut media) = (0, 0);
    for line in text.lines() {
        let t = line.trim_start();
        let block = t.starts_with('%') || t.starts_with("{{{") || t.starts_with("}}}");
        let task = t.contains("[ ]") || t.contains("[X]") || t.contains("[x]");
        let bold = t.split_whitespace().any(|w| w.len() > 2 && w.starts_with('*') && w.ends_with('*') && !w.starts_with("**")) || t.contains("_*");
        if block || task || bold {
            vim += 1;
        }
        if t.contains("'''") || t.contains("''") || t.starts_with("{{") || t.starts_with("{|") || t.contains("<ref") || t.contains("__TOC__") {
            media += 1;
        }
    }
    vim >= 2 && vim > media
}

markup!(
    /// Gemtext, the Gemini protocol's line-oriented markup.
    GemtextReader, GEMTEXT, "gemtext", "Gemtext", "Gemtext page", ["gmi", "gemini"], ["text/gemini"], gemtext
);

fn mediawiki(text: &str) -> Result<Rendered> {
    Ok(markup_lite::mediawiki(text).into())
}

fn creole(text: &str) -> Result<Rendered> {
    Ok(markup_lite::creole(text).into())
}

fn dokuwiki(text: &str) -> Result<Rendered> {
    Ok(markup_lite::dokuwiki(text).into())
}

fn jira(text: &str) -> Result<Rendered> {
    Ok(markup_lite::jira(text).into())
}

fn txt2tags(text: &str) -> Result<Rendered> {
    Ok(markup_lite::txt2tags(text).into())
}

fn muse(text: &str) -> Result<Rendered> {
    Ok(markup_lite::muse(text).into())
}

fn haddock(text: &str) -> Result<Rendered> {
    Ok(markup_lite::haddock(text).into())
}

fn xwiki(text: &str) -> Result<Rendered> {
    Ok(markup_lite::xwiki(text).into())
}

fn tikiwiki(text: &str) -> Result<Rendered> {
    Ok(markup_lite::tikiwiki(text).into())
}

fn twiki(text: &str) -> Result<Rendered> {
    Ok(markup_lite::twiki(text).into())
}

fn zimwiki(text: &str) -> Result<Rendered> {
    Ok(markup_lite::zimwiki(text).into())
}

fn vimwiki(text: &str) -> Result<Rendered> {
    Ok(markup_lite::vimwiki(text).into())
}

fn gemtext(text: &str) -> Result<Rendered> {
    Ok(markup_lite::gemtext(text).into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use libfileparser_core::{Content, Document, FileSource, ParseContext, Reader};

    fn blocks(reader: &dyn Reader, name: &str, text: &str) -> (Document, String) {
        let doc = reader.open(&FileSource::from_bytes(name, text.as_bytes().to_vec()), &ParseContext::new()).unwrap();
        let Content::Flow(f) = doc.facet(0, &ParseContext::new()).unwrap() else { panic!() };
        let s = format!("{:?}", f.blocks(0..1000).unwrap());
        (doc, s)
    }

    fn has(s: &str, wants: &[&str]) {
        for w in wants {
            assert!(s.contains(w), "{w} in {s}");
        }
    }

    fn lacks(s: &str, nots: &[&str]) {
        for w in nots {
            assert!(!s.contains(w), "{w} not in {s}");
        }
    }

    #[test]
    fn mediawiki() {
        let (doc, s) = blocks(&MediaWikiReader, "a.mediawiki", "{{Infobox|name=x\n|y={{nested}}}}\n== Head ==\n'''bold''' and ''it'' and [[Page|a page]] and [https://x.org site]<ref>a note</ref>.\n* one\n** nested\n# first\n<syntaxhighlight lang=\"python\">\nprint(1)\n</syntaxhighlight>\n{| class=\"wikitable\"\n! a !! b\n|-\n| 1 || 2\n|}\n[[File:Pic.png|thumb|Caption]]\n[[Category:Things]]\n");
        has(&s, &["Heading { level: 2", "Strong", "Emphasis", "Page", "https://x.org", "a note", "List { ordered: false", "nested", "List { ordered: true", "language: Some(\"python\")", "Table { header_rows: 1", "Pic.png"]);
        lacks(&s, &["Infobox", "nested}}", "Category", "wikitable"]);
        assert!(doc.warnings().is_empty());
    }

    #[test]
    fn creole() {
        let (_, s) = blocks(&CreoleReader, "a.creole", "= Top\n**bold** and //it// and [[https://x.org|link]] at http://y.org\n* one\n** two\n{{{\ncode\n}}}\n|=a|=b|\n|1|2|\n");
        has(&s, &["Heading { level: 1", "Strong", "Emphasis", "https://x.org", "http://y.org", "List", "Code { language: None, text: \"code\"", "Table { header_rows: 1"]);
    }

    #[test]
    fn dokuwiki() {
        let (_, s) = blocks(&DokuWikiReader, "a.dokuwiki", "====== Top ======\n===== Sub =====\n**bold** //it// ''mono'' [[ns:page|a page]]\n  * one\n    * nested\n  - first\n<code python>\nprint(1)\n</code>\n^ a ^ b ^\n| 1 | 2 |\n\n  indented code\n");
        has(&s, &["Heading { level: 1", "Heading { level: 2", "Strong", "Emphasis", "Code(\"mono\")", "ns:page", "nested", "List { ordered: true", "language: Some(\"python\")", "Table { header_rows: 1", "indented code"]);
    }

    #[test]
    fn jira() {
        let (_, s) = blocks(&JiraReader, "a.jira", "h1. Top\n*bold* _it_ {{mono}} [label|https://x.org]\n* one\n** two\n# first\n{code:java}\nint x;\n{code}\n||a||b||\n|1|2|\nbq. quoted\n");
        has(&s, &["Heading { level: 1", "Strong", "Emphasis", "Code(\"mono\")", "https://x.org", "List { ordered: false", "List { ordered: true", "language: Some(\"java\")", "Table { header_rows: 1", "Quote"]);
    }

    #[test]
    fn txt2tags() {
        let (doc, s) = blocks(&Txt2tagsReader, "a.t2t", "My Title\nAuthor\n2026\n\n= Section =\n**bold** //it// ``mono`` [label https://x.org]\n- one\n- two\n+ first\n```\ncode\n```\n|| a | b |\n| 1 | 2 |\n");
        has(&s, &["Heading { level: 1", "My Title", "Strong", "Emphasis", "Code(\"mono\")", "https://x.org", "List { ordered: false", "List { ordered: true", "Code { language: None, text: \"code\"", "Table { header_rows: 1"]);
        assert!(doc.metadata().iter().any(|(k, v)| k == "Title" && v == "My Title"));
    }

    #[test]
    fn muse() {
        let (doc, s) = blocks(&MuseReader, "a.muse", "#title Muse Doc\n\n* Top\n*em* **strong** =code= [[https://x.org][link]]\n\n - one\n - two\n 1. first\n\n<example>\ncode\n</example>\n\na || b\n1 | 2\n");
        has(&s, &["Heading { level: 1", "Emphasis", "Strong", "Code(\"code\")", "https://x.org", "List { ordered: false", "List { ordered: true", "text: \"code\"", "Table { header_rows: 1"]);
        assert!(doc.metadata().iter().any(|(k, v)| k == "Title" && v == "Muse Doc"));
    }

    #[test]
    fn haddock() {
        let (_, s) = blocks(&HaddockReader, "a.haddock", "= Top\n/em/ __strong__ @code@ <https://x.org label>\n\n* one\n* two\n\n(1) first\n\n> main = print 1\n");
        has(&s, &["Heading { level: 1", "Emphasis", "Strong", "Code(\"code\")", "https://x.org", "List { ordered: false", "List { ordered: true", "main = print 1"]);
    }

    #[test]
    fn xwiki() {
        let (_, s) = blocks(&XWikiReader, "a.xwiki", "{{toc/}}\n= Top =\n**bold** //it// ##mono## [[label>>https://x.org]]\n* one\n** two\n1. first\n{{code language=\"java\"}}\nint x;\n{{/code}}\n|=a|=b\n|1|2\n");
        has(&s, &["Heading { level: 1", "Strong", "Emphasis", "Code(\"mono\")", "https://x.org", "List { ordered: false", "List { ordered: true", "language: Some(\"java\")", "Table { header_rows: 1"]);
        lacks(&s, &["toc"]);
    }

    #[test]
    fn tikiwiki() {
        let (_, s) = blocks(&TikiWikiReader, "a.tikiwiki", "! Top\n__bold__ ''it'' -+mono+- [https://x.org|link]\n* one\n** two\n# first\n{CODE(colors=\"python\")}\nprint(1)\n{CODE}\n||a|b\n1|2||\n");
        has(&s, &["Heading { level: 1", "Strong", "Emphasis", "Code(\"mono\")", "https://x.org", "List { ordered: false", "List { ordered: true", "print(1)", "Table"]);
    }

    #[test]
    fn twiki() {
        let (_, s) = blocks(&TWikiReader, "a.twiki", "---+ Top\n*bold* _it_ =mono= [[https://x.org][link]] %TOC%\n   * one\n      * two\n   1 first\n<verbatim>\ncode\n</verbatim>\n| *a* | *b* |\n| 1 | 2 |\n");
        has(&s, &["Heading { level: 1", "Strong", "Emphasis", "Code(\"mono\")", "https://x.org", "List { ordered: false", "two", "List { ordered: true", "text: \"code\"", "Table { header_rows: 1"]);
        lacks(&s, &["TOC"]);
    }

    #[test]
    fn zimwiki() {
        let (_, s) = blocks(&ZimWikiReader, "a.zim", "Content-Type: text/x-zim-wiki\nWiki-Format: zim 0.4\n\n====== Top ======\n**bold** //it// ''mono'' [[Page|link]]\n* one\n\t* two\n1. first\n'''\ncode\n'''\n|a|b|\n|-|-|\n|1|2|\n");
        has(&s, &["Heading { level: 1", "Strong", "Emphasis", "Code(\"mono\")", "Page", "List { ordered: false", "two", "List { ordered: true", "text: \"code\"", "Table { header_rows: 1"]);
        lacks(&s, &["Content-Type"]);
    }

    #[test]
    fn vimwiki_takes_wiki_files_that_read_as_vimwiki() {
        let vim = b"%title Notes\n= Heading =\n* [ ] a task\nsome *bold* words\n{{{\ncode\n}}}\n";
        let media = b"== Heading ==\nSome '''bold''' and ''italic''.\n{{Infobox}}\n{| class=wikitable\n|}\n";
        assert!(super::vimwiki_by_content(vim, "notes.wiki"));
        assert!(!super::vimwiki_by_content(media, "Article.wiki"));
        assert!(!super::vimwiki_by_content(vim, "notes.txt"), "only .wiki is in question");
    }

    #[test]
    fn vimwiki() {
        let (_, s) = blocks(&VimwikiReader, "a.vimwiki", "%title Notes\n= Top =\n*bold* _it_ `mono` [[Page|link]]\n* one\n    * two\n1. first\n{{{python\nprint(1)\n}}}\n| a | b |\n|---|---|\n| 1 | 2 |\n");
        has(&s, &["Heading { level: 1", "Strong", "Emphasis", "Code(\"mono\")", "Page", "List { ordered: false", "two", "List { ordered: true", "language: Some(\"python\")", "Table { header_rows: 1"]);
    }

    #[test]
    fn gemtext() {
        let (doc, s) = blocks(&GemtextReader, "a.gmi", "# Capsule\nSome *text* stays plain.\n=> gemini://x.org/ A link\n* item\n> quoted\n```sh\nls\n```\n");
        has(&s, &["Heading { level: 1", "*text*", "gemini://x.org/", "A link", "List", "Quote", "language: Some(\"sh\")"]);
        assert!(doc.metadata().iter().any(|(k, v)| k == "Title" && v == "Capsule"));
    }
}
