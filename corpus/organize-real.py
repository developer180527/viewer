"""Sorts one clone's real-world test files (corpus/fetch-real.sh) into
`<out>/<category>/<reader>/`, the reader each should open with; `<reader>-invalid/` for
test suites' negative cases.

    organize-real.py <clone> <out>     (adds to <out>; the clone is then deleted)
"""

import os, shutil, sys
clone, out = sys.argv[1].rstrip("/"), sys.argv[2]
by_ext = {"hcl": "hcl", "tf": "hcl", "kdl": "kdl", "dhall": "dhall", "ncl": "nickel", "ofx": "ofx", "qfx": "ofx",
          "dot": "dot", "gv": "dot", "ics": "icalendar", "vcf": None, "sam": "sam", "fa": "fasta", "fasta": "fasta",
          "fq": "fastq", "fastq": "fastq", "gff3": "gff", "gpx": "gpx", "toml": "toml", "json": "json", "csv": "delimited",
          "ttl": "turtle", "nt": "turtle", "nq": "turtle", "trig": "turtle", "rdf": "rdfxml", "hl7": "hl7", "qif": "qif",
          "po": "po", "pot": "po", "mo": "mo", "kml": "kml", "yaml": "yaml", "xbrl": "xbrl", "proto": "protobuf",
          "sta": "swift", "edi": "edifact",
          # Molecules and crystals, and signals (science).
          "xyz": "xyz", "extxyz": "xyz", "mol": "mol", "sdf": "mol", "sd": "mol", "smi": "smiles", "smiles": "smiles",
          "vasp": "poscar", "cjson": "cjson", "qcjson": "qcschema", "molden": "molden", "mold": "molden", "vcd": "vcd",
          "pdb": "pdb", "ent": "pdb", "cif": "cif", "mmcif": "cif",
          # Documents.
          "pdf": "pdf", "md": "markdown", "markdown": "markdown", "rst": "rst", "asciidoc": "asciidoc", "adoc": "asciidoc",
          "org": "org", "textile": "textile", "docx": "docx", "odt": "odt", "rtf": "rtf", "epub": "epub", "doc": "doc",
          "pptx": "pptx", "ppt": "ppt", "pps": "ppt", "odp": "odp", "html": "html", "htm": "html",
          # Markup, mail, books (2026-11-18).
          "adoc": "asciidoc", "dj": "djot", "djot": "djot", "typ": "typst", "ipynb": "ipynb", "man": "man", "mdoc": "man",
          "mediawiki": "mediawiki", "wiki": "mediawiki", "latex": "latex", "tex": "latex", "creole": "creole", "pod": "pod", "gmi": "gemtext",
          "fountain": "fountain", "dokuwiki": "dokuwiki", "jira": "jira", "t2t": "txt2tags", "muse": "muse", "haddock": "haddock",
          "docbook": "docbook", "dbk": "docbook", "tei": "tei", "texinfo": "texinfo", "texi": "texinfo", "xwiki": "xwiki", "tikiwiki": "tikiwiki",
          "twiki": "twiki", "zimwiki": "zimwiki", "vimwiki": "vimwiki", "eml": "eml", "mbox": "mbox", "mht": "mhtml", "mhtml": "mhtml",
          "msg": "msg", "mobi": "mobi", "azw": "mobi", "azw3": "mobi", "prc": "mobi", "fb2": "fb2", "djvu": "djvu", "djv": "djvu",
          "hwp": "hwp", "hwpx": "hwp", "cb7": "comic", "cbr": "comic", "opml": "opml",
          "1": "man", "2": "man", "3": "man", "4": "man", "5": "man", "6": "man", "7": "man", "8": "man", "9": "man",
          # Spreadsheets.
          "xlsx": "xlsx", "xlsm": "xlsx", "xltx": "xlsx", "xltm": "xlsx", "xlam": "xlsx", "xlsb": "xlsb",
          "xls": "xls", "xla": "xls", "xlt": "xls", "ods": "ods", "ots": "ods", "fods": "fods", "slk": "sylk", "dif": "dif", "numbers": "numbers"}
# Category folders, as corpus/real is laid out: data/, docs/, sheets/ (3d/ is
# fetch-real-3d.sh's).
DOCS = {"asciidoc", "doc", "docx", "epub", "html", "markdown", "odp", "odt", "org", "pdf", "ppt", "pptx", "rst", "rtf", "textile", "xps", "comic",
        "djot", "typst", "ipynb", "man", "mediawiki", "latex", "creole", "pod", "gemtext", "fountain", "dokuwiki", "jira", "txt2tags", "muse",
        "haddock", "docbook", "tei", "texinfo", "xwiki", "tikiwiki", "twiki", "zimwiki", "vimwiki", "eml", "mbox", "mhtml", "msg", "mobi", "fb2",
        "djvu", "hwp"}
SHEETS = {"dif", "fods", "numbers", "ods", "sylk", "xls", "xlsb", "xlsx", "xmlss"}
counts = {}
for repo in [os.path.basename(clone)]:
    root = clone
    for dp, dirs, files in os.walk(root):
        dirs[:] = [d for d in dirs if d != ".git"]
        for f in files:
            path = os.path.join(dp, f)
            if not os.path.isfile(path):
                continue  # a link to nothing
            rel = os.path.relpath(path, root)
            ext = f.rsplit(".", 1)[-1].lower() if "." in f else ""
            reader = by_ext.get(ext)
            head = open(path, "rb").read(1024)
            if ext == "vcf":
                reader = "vcard" if b"BEGIN:VCARD" in head else "vcf"
            if ext == "txt":
                if repo.startswith("imsweb_x12"): reader = "x12"
                elif repo.startswith("nerdocs_pydifact"): reader = "edifact"
                elif repo.startswith("Efferent-Health"): reader = "hl7"
                elif repo.startswith("WoLpH_mt940"): reader = "swift"
            # Excel 2003's XML, whatever it's named (often .xls).
            if ext in ("xls", "xml") and head.lstrip(b"\xef\xbb\xbf").startswith(b"<?xml") and b"urn:schemas-microsoft-com:office:spreadsheet" in head:
                reader = "xmlss"
            if reader == "mediawiki" and "vimwiki" in f.lower():
                reader = "vimwiki"
            if ext in "123456789" and len(ext) == 1 and not (head.startswith(b".") or head.startswith(b"'\\\"")):
                reader = None
            # Known by name: VASP's POSCAR and CONTCAR, Turbomole's coord.
            low = f.lower()
            # Whatever follows: `POSCAR.relax`, `CONTCAR.MD` (a dynamics run, not Markdown).
            if low.startswith(("poscar", "contcar")) and ext not in ("gz", "bz2", "xz"):
                reader = "poscar"
            elif low == "coord":
                reader = "turbomole"
            elif ext == "json" and (b'"qcschema_' in head or b'"qc_schema_' in head):
                reader = "qcschema"
            if repo.startswith("WoLpH_mt940") and ext not in ("sta", "txt"):
                reader = None
            if reader is None:
                continue
            # Invalid on purpose: they must fail (or fall back), not crash.
            name = f.lower()
            invalid = ("/invalid/" in "/" + rel + "/" or (repo.startswith("nst_") and f.startswith("n_"))
                       or ("-bad-" in name) or (reader == "rdfxml" and "error" in name) or "fail" in rel.lower())
            folder = reader + ("-invalid" if invalid else "")
            category = "docs" if reader in DOCS else "sheets" if reader in SHEETS else "data"
            dst = os.path.join(out, category, folder, (repo.split("_", 1)[1] + "__" + rel.replace("/", "__"))[-150:])
            os.makedirs(os.path.dirname(dst), exist_ok=True)
            shutil.copyfile(path, dst)
            counts[folder] = counts.get(folder, 0) + 1
print(f"{os.path.basename(clone)}: {sum(counts.values())} files", file=sys.stderr)
