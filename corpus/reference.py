#!/usr/bin/env python3
"""Documents checked against a reference renderer: is what we show what the format's
own tools show?

For each document, the reference tool for its format reads it (Pandoc for most
markup, LibreOffice for Office files, Calibre for e-books, mandoc for man pages…), and
its result becomes the same outline crates/libfileparser/tests/outline.rs writes for
ours: headings with their levels, paragraphs, list items, code, tables' cells and
images. The two are compared on what any renderer agrees on:

  text      the words, in order: how many of the reference's we show (recall), and how
            many of ours it has (precision)
  headings  found, and at the same level relative to the document's top level
  tables, list items, code blocks, images: how many

Layout isn't compared: a flowed view never matches LibreOffice's pages, but the words,
headings and table cells should. Pages formats (PDF, XPS, DjVu) are left out: their
engine is the reference.

    LFP_OUTLINE_IN=$PWD/corpus/real/docs LFP_OUTLINE_OUT=$PWD/target/reference/ours/real \\
        cargo test --release -p libfileparser --test outline -- --ignored --nocapture
    python3 corpus/reference.py corpus/real/docs target/reference/ours/real target/reference/real

writes target/reference/real/report.md (per format, then the worst files) and
report.tsv (every file). References are cached in target/reference/real/refs/; a
missing tool is reported, and its formats skipped. --only=docx,rtf limits the formats.

Tools: pandoc, LibreOffice (soffice), Calibre (ebook-convert), asciidoctor, makeinfo
(texinfo), mandoc, pod2html; `brew install pandoc asciidoctor texinfo` and
`brew install --cask libreoffice calibre`.
"""

import email
import email.policy
import html
import html.parser
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import unicodedata
from difflib import SequenceMatcher
from pathlib import Path

TIMEOUT = 120

# --- The reference for each format ------------------------------------------------------

# Our reader folder (corpus/real/docs/<reader>) or extension, to Pandoc's reader.
PANDOC = {
    "markdown": "gfm", "md": "gfm", "rst": "rst", "org": "org", "mediawiki": "mediawiki", "textile": "textile",
    "dokuwiki": "dokuwiki", "tikiwiki": "tikiwiki", "twiki": "twiki", "vimwiki": "vimwiki", "jira": "jira",
    "muse": "muse", "creole": "creole", "haddock": "haddock", "docx": "docx", "odt": "odt", "epub": "epub",
    "fb2": "fb2", "rtf": "rtf", "ipynb": "ipynb", "latex": "latex", "tex": "latex", "docbook": "docbook",
    "typst": "typst", "typ": "typst", "djot": "djot", "dj": "djot", "txt2tags": "t2t", "t2t": "t2t",
    "html": "html", "htm": "html",
}
# Formats whose reference turns them into HTML first.
LIBREOFFICE = {"doc", "ppt", "pptx", "odp", "hwp", "hwpx"}
CALIBRE = {"mobi", "azw", "azw3", "prc"}
PAGES = {"pdf", "xps", "oxps", "djvu", "djv", "cbz", "cbr", "cb7", "cbt"}
# Extensions under corpus/files/docs, to our reader folders' names.
BY_EXTENSION = {
    "adoc": "asciidoc", "asciidoc": "asciidoc", "texi": "texinfo", "texinfo": "texinfo", "pod": "pod", "pm": "pod",
    "eml": "eml", "mbox": "mbox", "msg": "msg", "mht": "mht", "mhtml": "mht", "mediawiki": "mediawiki", "wiki": "mediawiki",
    "1": "man", "2": "man", "3": "man", "5": "man", "7": "man", "8": "man", "man": "man", "gmi": "gemtext",
    "fountain": "fountain", "xml": "docbook", "zim": "zimwiki", "xwiki": "xwiki",
}


def kind_of(path: Path, root: Path) -> str:
    """The reader a file is for: its folder under corpus/real/docs, else its extension."""
    rel = path.relative_to(root)
    if len(rel.parts) > 1:
        return rel.parts[0]
    ext = path.suffix.lower().lstrip(".")
    if path.name.endswith("-docbook.xml"):
        return "docbook"
    if path.name.endswith("-tei.xml"):
        return "tei"
    return BY_EXTENSION.get(ext, ext)


def run(cmd, **kw):
    return subprocess.run(cmd, capture_output=True, timeout=TIMEOUT, **kw)


def have(tool):
    return shutil.which(tool) is not None


def soffice():
    for c in ["soffice", "/Applications/LibreOffice.app/Contents/MacOS/soffice"]:
        if shutil.which(c) or Path(c).exists():
            return c
    return None


def ebook_convert():
    for c in ["ebook-convert", "/Applications/calibre.app/Contents/MacOS/ebook-convert"]:
        if shutil.which(c) or Path(c).exists():
            return c
    return None


class Missing(Exception):
    """The reference tool isn't installed."""


def reference(path: Path, kind: str):
    """The reference's outline of a document: a list of blocks, as outline.rs writes."""
    ext = path.suffix.lower().lstrip(".")
    if kind in ("man",):
        if not have("mandoc"):
            raise Missing("mandoc")
        r = run(["mandoc", "-T", "html", "-O", "fragment", str(path)])
        page = r.stdout.decode("utf-8", "replace")
        page = re.sub(r'<(table|div) class="(head|foot)".*?</\1>', "", page, flags=re.S)
        # A literal display (.Dl, .Bd -literal) is code, which mandoc writes as a div.
        page = re.sub(r'<div class="Bd[^"]*">\s*<code class="Li">(.*?)</code>\s*</div>', r"<pre>\1</pre>", page, flags=re.S)
        # The synopsis is laid out in a table; it isn't one.
        page = re.sub(r'<table class="Nm">(.*?)</table>', lambda m: re.sub(r"</?(tr|td|tbody)[^>]*>", " ", m.group(1)), page, flags=re.S)
        return html_outline(page)
    if kind == "pod":
        with tempfile.TemporaryDirectory() as t:
            r = run(["pod2html", f"--infile={path}", "--quiet", "--noindex", f"--cachedir={t}"])
        return html_outline(r.stdout.decode("utf-8", "replace"))
    if kind == "asciidoc":
        if not have("asciidoctor"):
            raise Missing("asciidoctor")
        r = run(["asciidoctor", "-s", "-o", "-", str(path)])
        return html_outline(r.stdout.decode("utf-8", "replace"))
    if kind == "texinfo":
        if not have("makeinfo"):
            raise Missing("makeinfo (texinfo)")
        r = run(["makeinfo", "--html", "--no-split", "--no-headers", "-o", "-", str(path)])
        return html_outline(r.stdout.decode("utf-8", "replace"))
    if kind == "mbox":
        return None  # Shown as a list of its messages, which no reference has.
    if kind == "mht":
        # A saved web page shows as the page, as a browser shows it.
        m = email.message_from_bytes(path.read_bytes(), policy=email.policy.default)
        page = next((p for p in m.walk() if p.get_content_type() == "text/html"), None)
        return html_outline(page.get_content()) if page is not None else None
    if kind == "eml":
        return mail_outline(path, kind)
    if kind in LIBREOFFICE or ext in LIBREOFFICE:
        office = soffice()
        if not office:
            raise Missing("LibreOffice")
        with tempfile.TemporaryDirectory() as t:
            # Presentations through Impress's HTML export are one page per slide; its
            # text export keeps the words, which is what's compared.
            impress = kind in ("ppt", "pptx", "odp") or ext in ("ppt", "pptx", "odp")
            target = "html" if not impress else "odp"
            run([office, f"-env:UserInstallation=file://{t}/profile", "--headless", "--convert-to", target, "--outdir", t, str(path)])
            out = list(Path(t).glob(f"*.{target}"))
            if not out:
                return None
            if impress:
                return odp_outline(out[0])
            return html_outline(out[0].read_text("utf-8", "replace"))
    if kind in CALIBRE or ext in CALIBRE:
        convert = ebook_convert()
        if not convert:
            raise Missing("Calibre")
        if not have("pandoc"):
            raise Missing("pandoc")
        with tempfile.TemporaryDirectory() as t:
            epub = Path(t) / "book.epub"
            run([convert, str(path), str(epub)])
            if not epub.exists():
                return None
            return pandoc_outline(epub, "epub")
    if kind == "html" or ext in ("html", "htm", "xhtml"):
        return html_outline(path.read_bytes().decode("utf-8", "replace"))
    fmt = PANDOC.get(kind) or PANDOC.get(ext)
    if fmt:
        if not have("pandoc"):
            raise Missing("pandoc")
        return pandoc_outline(path, fmt)
    return None


# --- Pandoc's tree, as an outline -------------------------------------------------------


def pandoc_outline(path: Path, fmt: str):
    r = run(["pandoc", "-f", fmt, "-t", "json", str(path)])
    if r.returncode != 0:
        return None
    doc = json.loads(r.stdout)
    out = []
    pandoc_blocks(doc.get("blocks", []), out)
    return out


def squash(s: str) -> str:
    return " ".join(s.split())


def pandoc_inlines(xs) -> str:
    parts = []

    def walk(xs):
        for x in xs:
            t, c = x.get("t"), x.get("c")
            if t == "Str":
                parts.append(c)
            elif t in ("Space", "SoftBreak", "LineBreak"):
                parts.append(" ")
            elif t in ("Emph", "Strong", "Underline", "Strikeout", "Superscript", "Subscript", "SmallCaps"):
                walk(c)
            elif t == "Quoted":
                walk(c[1])
            elif t == "Cite":
                walk(c[1])
            elif t == "Code":
                parts.append(c[1])
            elif t == "Math":
                parts.append(c[1])
            elif t in ("Link", "Image"):
                walk(c[1])
            elif t == "Span":
                walk(c[1])
            elif t == "Note":
                pass  # Notes are at the end of ours; their markers differ.
    walk(xs)
    return squash("".join(parts))


def pandoc_blocks(blocks, out):
    for b in blocks:
        t, c = b.get("t"), b.get("c")
        if t == "Header":
            out.append(["h", c[0], pandoc_inlines(c[2])])
        elif t in ("Para", "Plain"):
            # An image on its own is a figure, not a paragraph.
            if len(c) == 1 and c[0].get("t") == "Image":
                out.append(["img", pandoc_inlines(c[0]["c"][1])])
            else:
                s = pandoc_inlines(c)
                if s:
                    out.append(["p", s])
        elif t == "LineBlock":
            s = squash(" ".join(pandoc_inlines(l) for l in c))
            if s:
                out.append(["p", s])
        elif t in ("BulletList", "OrderedList"):
            items = c if t == "BulletList" else c[1]
            for item in items:
                inner = []
                pandoc_blocks(item, inner)
                if inner and inner[0][0] == "p":
                    out.append(["li", inner[0][1]])
                    out.extend(inner[1:])
                else:
                    out.append(["li", ""])
                    out.extend(inner)
        elif t == "DefinitionList":
            for term, defs in c:
                out.append(["p", pandoc_inlines(term)])
                for d in defs:
                    pandoc_blocks(d, out)
        elif t == "BlockQuote":
            pandoc_blocks(c, out)
        elif t == "Div":
            pandoc_blocks(c[1], out)
        elif t == "Figure":
            pandoc_blocks(c[2], out)
        elif t == "CodeBlock":
            out.append(["code", squash(c[1])])
        elif t == "RawBlock":
            out.append(["raw", squash(c[1])])
        elif t == "Table":
            rows = []
            _, _caption, _specs, head, bodies, foot = c
            for row in head[1]:
                rows.append(pandoc_row(row))
            for body in bodies:
                for row in body[2] + body[3]:
                    rows.append(pandoc_row(row))
            for row in foot[1]:
                rows.append(pandoc_row(row))
            out.append(["table", rows])


def pandoc_row(row):
    cells = []
    for cell in row[1]:
        inner = []
        pandoc_blocks(cell[4], inner)
        cells.append(squash(" ".join(text_of(b) for b in inner)))
    return cells


# --- HTML, as an outline (what LibreOffice, mandoc, asciidoctor… write) ------------------

BLOCKS = {"p", "div", "li", "pre", "h1", "h2", "h3", "h4", "h5", "h6", "td", "th", "tr", "table", "blockquote",
          "dt", "dd", "ul", "ol", "dl", "section", "article", "body", "header", "footer", "figure", "figcaption",
          "caption", "address", "center", "br", "hr", "main", "aside", "nav", "details", "summary"}
SKIP = {"script", "style", "head", "title", "noscript", "template", "svg", "math"}


class Outliner(html.parser.HTMLParser):
    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.out = []
        self.buf = []
        self.stack = []  # open block tags
        self.li_used = []  # per open <li>: its text was written
        self.skip = 0
        self.table = []  # open tables: lists of rows
        self.cell = None

    def kind(self):
        for tag in reversed(self.stack):
            if tag in ("h1", "h2", "h3", "h4", "h5", "h6"):
                return ["h", int(tag[1])]
            if tag == "pre":
                return ["code"]
            if tag == "li":
                if not self.li_used[-1]:
                    self.li_used[-1] = True
                    return ["li"]
                return ["p"]
            if tag in ("p", "dt", "dd", "div", "blockquote", "figcaption", "caption", "body", "section", "td", "th"):
                return ["p"]
        return ["p"]

    def flush(self):
        text = squash("".join(self.buf))
        self.buf = []
        if self.table and self.cell is not None:
            if text:
                self.cell.append(text)
            return
        if not text:
            return
        k = self.kind()
        self.out.append(k + [text])

    def handle_starttag(self, tag, attrs):
        if tag in SKIP:
            self.skip += 1
            return
        if self.skip:
            return
        a = dict(attrs)
        if tag == "img":
            alt = squash(a.get("alt") or "")
            if self.table and self.cell is not None:
                self.cell.append(alt)
            else:
                self.flush()
                self.out.append(["img", alt])
            return
        if tag == "br" and "pre" in self.stack:
            self.buf.append("\n")  # A line of the code, not a new block.
            return
        if tag in BLOCKS:
            self.flush()
            if tag == "br":
                return
            self.stack.append(tag)
            if tag == "li":
                self.li_used.append(False)
            if tag == "table":
                self.table.append([])
            if tag == "tr" and self.table:
                self.table[-1].append([])
            if tag in ("td", "th") and self.table:
                if not self.table[-1]:
                    self.table[-1].append([])
                self.cell = []

    def handle_endtag(self, tag):
        if tag in SKIP:
            self.skip = max(0, self.skip - 1)
            return
        if self.skip or tag not in BLOCKS or tag in ("br", "hr"):
            return
        self.flush()
        if tag in ("td", "th") and self.table and self.cell is not None:
            self.table[-1][-1].append(squash(" ".join(self.cell)))
            self.cell = None
        if tag == "table" and self.table:
            rows = [r for r in self.table.pop() if r]
            if self.table:
                # A table in a cell: its text joins the cell.
                if self.cell is not None:
                    self.cell.append(" ".join(" ".join(r) for r in rows))
            else:
                self.out.append(["table", rows])
        if tag in self.stack:
            while self.stack:
                t = self.stack.pop()
                if t == "li":
                    self.li_used.pop()
                if t == tag:
                    break

    def handle_data(self, data):
        if self.skip:
            return
        if "pre" in self.stack:
            self.buf.append(data)
        else:
            self.buf.append(data)


def html_outline(text: str):
    p = Outliner()
    p.feed(text)
    p.close()
    p.flush()
    return p.out


def odp_outline(path: Path):
    """A presentation's words, slide by slide, from its content.xml."""
    import zipfile
    import xml.etree.ElementTree as ET
    ns = {"text": "urn:oasis:names:tc:opendocument:xmlns:text:1.0", "draw": "urn:oasis:names:tc:opendocument:xmlns:drawing:1.0"}
    with zipfile.ZipFile(path) as z:
        root = ET.fromstring(z.read("content.xml"))
    out = []
    for page in root.iter(f"{{{ns['draw']}}}page"):
        for el in page.iter():
            if el.tag in (f"{{{ns['text']}}}p", f"{{{ns['text']}}}h"):
                s = squash("".join(el.itertext()))
                if s:
                    out.append(["p", s])
    return out


def mail_outline(path: Path, kind: str):
    """A message as a mail client shows it: its headers, labelled; every text part
    shown inline (the best of each set of alternatives); then its attachments' names."""
    data = path.read_bytes()
    if kind == "mbox":
        import mailbox
        messages = [email.message_from_bytes(bytes(m), policy=email.policy.default) for m in mailbox.mbox(str(path))]
    else:
        messages = [email.message_from_bytes(data, policy=email.policy.default)]
    out = []

    def body(part):
        ctype = part.get_content_type()
        if part.is_multipart():
            subs = list(part.iter_parts())
            if ctype == "multipart/alternative":
                # The last is the richest (RFC 2046), and what a client shows.
                pick = next((p for p in reversed(subs) if p.is_multipart() or p.get_content_type() in ("text/plain", "text/html")), None)
                if pick is not None:
                    body(pick)
                return
            for p in subs:
                body(p)
            return
        if ctype == "message/rfc822" and not part.get_filename():
            # A forwarded message, shown inline as clients show it.
            try:
                inner = part.get_content()
            except Exception:
                return
            out.append(["p", "Forwarded message"])
            headers(inner)
            body(inner)
            return
        if part.get_filename() or part.get_content_disposition() == "attachment" or not ctype.startswith("text/"):
            return
        try:
            content = part.get_content()
        except Exception:
            return
        if ctype == "text/html":
            out.extend(html_outline(content))
        else:
            for para in re.split(r"\n\s*\n", content):
                s = squash(para)
                if s:
                    out.append(["p", s])

    def headers(m):
        for h in ("From", "To", "Cc", "Date", "Subject"):
            try:
                v = m.get(h)
            except Exception:
                v = None
            if v:
                out.append(["p", f"{h} {squash(str(v))}"])

    for m in messages:
        headers(m)
        body(m)
        names = [squash(p.get_filename() or "") for p in m.walk() if not p.is_multipart() and (p.get_filename() or p.get_content_disposition() == "attachment")]
        if names:
            out.append(["p", "Attachments " + " ".join(names)])
    return out


# --- Comparing ---------------------------------------------------------------------------


def text_of(b) -> str:
    if b[0] == "h":
        return b[2]
    if b[0] == "table":
        return " ".join(" ".join(r) for r in b[1])
    return b[1] if len(b) > 1 else ""


WORD = re.compile(r"\w+", re.UNICODE)


def words(blocks):
    s = unicodedata.normalize("NFKC", " ".join(text_of(b) for b in blocks)).lower()
    return WORD.findall(s)


def norm(s: str) -> str:
    return " ".join(WORD.findall(unicodedata.normalize("NFKC", s).lower()))


MAX_WORDS = 40000


def compare(ours, ref):
    a, b = words(ours)[:MAX_WORDS], words(ref)[:MAX_WORDS]
    if not b:
        recall = 1.0 if not a else 0.0
        precision = 1.0 if not a else 0.0
    else:
        sm = SequenceMatcher(None, a, b, autojunk=False)
        matched = sum(m.size for m in sm.get_matching_blocks())
        recall = matched / len(b)
        precision = matched / len(a) if a else 0.0

    def heads(bl):
        return [(x[1], norm(x[2])) for x in bl if x[0] == "h" and norm(x[2])]

    ho, hr = heads(ours), heads(ref)
    ours_by_text = {}
    for l, t in ho:
        ours_by_text.setdefault(t, l)
    found = [t for _, t in hr if t in ours_by_text]
    # Levels relative to each other: a viewer that adds the title as a level-1 heading
    # moves every level down one, and that's fine; one heading moved alone isn't.
    shifts = [ours_by_text[t] - l for l, t in hr if t in ours_by_text]
    shift = max(set(shifts), key=shifts.count) if shifts else 0
    same_level = sum(1 for s in shifts if s == shift)
    count = lambda bl, k: sum(1 for x in bl if x[0] == k)

    def cells(bl):
        return {norm(c) for x in bl if x[0] == "table" for r in x[1] for c in r if norm(c)}

    co, cr = cells(ours), cells(ref)
    return {
        "recall": recall,
        "precision": precision,
        "words": (len(a), len(b)),
        "headings": (len(ho), len(hr), len(found), same_level),
        "tables": (count(ours, "table"), count(ref, "table"), len(co & cr), len(cr)),
        "items": (count(ours, "li"), count(ref, "li")),
        "code": (count(ours, "code"), count(ref, "code")),
        "images": (count(ours, "img"), count(ref, "img")),
    }


def problems(r, kind=""):
    """What a reader would notice, worst first."""
    out = []
    if r["recall"] < 0.9:
        out.append(f"shows {r['recall']:.0%} of the text")
    if r["precision"] < 0.9 and r["words"][0] > 20:
        out.append(f"{1 - r['precision']:.0%} of ours isn't in the reference")
    ho, hr, found, same = r["headings"]
    if hr and found < hr:
        out.append(f"{hr - found} of {hr} headings not headings")
    if found and same < found:
        out.append(f"{found - same} headings at another level")
    if ho > hr + 2 and ho > 1.5 * max(hr, 1):
        out.append(f"{ho - hr} headings the reference hasn't")
    to, tr, cm, ct = r["tables"]
    if tr != to and kind != "eml":  # A message's headers are a table of ours.
        out.append(f"{to} tables, reference {tr}")
    if ct and cm < 0.9 * ct:
        out.append(f"table cells {cm}/{ct}")
    io, ir = r["items"]
    if ir and abs(io - ir) > max(2, 0.1 * ir):
        out.append(f"{io} list items, reference {ir}")
    co, cr = r["code"]
    if abs(co - cr) > max(1, 0.2 * cr):
        out.append(f"{co} code blocks, reference {cr}")
    mo, mr = r["images"]
    if abs(mo - mr) > max(1, 0.2 * mr):
        out.append(f"{mo} images, reference {mr}")
    return out


# --- The run -----------------------------------------------------------------------------


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    only = next((a.split("=", 1)[1].split(",") for a in sys.argv[1:] if a.startswith("--only=")), None)
    if len(args) != 3:
        print(__doc__)
        sys.exit(2)
    docs, ours_dir, out_dir = (Path(a).resolve() for a in args)
    refs = out_dir / "refs"
    files = sorted(p for p in docs.rglob("*") if p.is_file() and not any(part.startswith(".") or part.endswith("-invalid") for part in p.relative_to(docs).parts))
    rows, missing, skipped = [], {}, {}
    for path in files:
        rel = path.relative_to(docs)
        kind = kind_of(path, docs)
        if only and kind not in only:
            continue
        if kind in PAGES or path.suffix.lower().lstrip(".") in PAGES:
            continue
        ours_path = ours_dir / f"{rel}.json"
        if not ours_path.exists():
            continue
        ours = json.loads(ours_path.read_text())
        cache = refs / f"{rel}.json"
        ref = None
        if cache.exists():
            ref = json.loads(cache.read_text())
        else:
            try:
                ref = reference(path, kind)
            except Missing as m:
                missing.setdefault(str(m), set()).add(kind)
                continue
            except subprocess.TimeoutExpired:
                ref = None
            except Exception as e:  # A reference that can't read it isn't ours to judge.
                skipped[kind] = skipped.get(kind, 0) + 1
                continue
            if ref is not None:
                cache.parent.mkdir(parents=True, exist_ok=True)
                cache.write_text(json.dumps(ref))
        if ref is None or not words(ref):
            skipped[kind] = skipped.get(kind, 0) + 1
            continue
        if "error" in ours:
            rows.append((kind, str(rel), None, [f"we didn't open it: {ours['error'][:100]}"], ours))
            continue
        if ours.get("model") == "pages":
            continue
        r = compare(ours.get("blocks", []), ref)
        rows.append((kind, str(rel), r, problems(r, kind), ours))

    out_dir.mkdir(parents=True, exist_ok=True)
    tsv = ["format\tfile\treader\trecall\tprecision\twords ours/ref\theadings ours/ref/found/same level\ttables\titems\tcode\timages\tproblems"]
    for kind, rel, r, probs, ours in rows:
        if r is None:
            tsv.append(f"{kind}\t{rel}\t-\t\t\t\t\t\t\t\t\t{'; '.join(probs)}")
            continue
        tsv.append("\t".join([kind, rel, ours.get("reader", "?"), f"{r['recall']:.3f}", f"{r['precision']:.3f}", "/".join(map(str, r["words"])),
                              "/".join(map(str, r["headings"])), "/".join(map(str, r["tables"])), "/".join(map(str, r["items"])),
                              "/".join(map(str, r["code"])), "/".join(map(str, r["images"])), "; ".join(probs)]))
    (out_dir / "report.tsv").write_text("\n".join(tsv) + "\n")

    by_kind = {}
    for kind, rel, r, probs, ours in rows:
        by_kind.setdefault(kind, []).append((rel, r, probs, ours))
    md = ["# Documents against their reference renderers", "",
          "Per format: files compared, how many read as the reference reads them (no problem below), the median share of the reference's words we show (recall) and of ours it has (precision), and headings found at the right level. Then each format's worst files.", "",
          "| format | files | clean | recall | precision | headings at level | reader |", "|---|---:|---:|---:|---:|---:|---|"]
    med = lambda xs: sorted(xs)[len(xs) // 2] if xs else float("nan")
    for kind in sorted(by_kind):
        xs = by_kind[kind]
        rs = [r for _, r, _, _ in xs if r]
        clean = sum(1 for _, r, p, _ in xs if r and not p)
        hr = sum(r["headings"][1] for r in rs)
        same = sum(r["headings"][3] for r in rs)
        readers = sorted({o.get("reader", "?") for _, _, _, o in xs})
        md.append(f"| {kind} | {len(xs)} | {clean} | {med([r['recall'] for r in rs]):.0%} | {med([r['precision'] for r in rs]):.0%} | {f'{same}/{hr}' if hr else '–'} | {', '.join(readers)} |")
    if missing:
        md += ["", "Not compared, tools missing: " + "; ".join(f"{t} ({', '.join(sorted(k))})" for t, k in sorted(missing.items()))]
    if skipped:
        md += ["", "The reference couldn't read: " + ", ".join(f"{k} {n}" for k, n in sorted(skipped.items()))]
    for kind in sorted(by_kind):
        bad = [(rel, r, p) for rel, r, p, _ in by_kind[kind] if p]
        if not bad:
            continue
        bad.sort(key=lambda x: (x[1]["recall"] if x[1] else -1, -len(x[2])))
        md += ["", f"## {kind}: {len(bad)} of {len(by_kind[kind])} differ", ""]
        for rel, r, p in bad[:8]:
            md.append(f"- `{rel}`: {'; '.join(p)}")
    (out_dir / "report.md").write_text("\n".join(md) + "\n")
    print("\n".join(md[: 6 + len(by_kind) + 4]))
    print(f"\nwrote {out_dir / 'report.md'} and report.tsv")


if __name__ == "__main__":
    main()
