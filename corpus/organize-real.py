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
          # Documents.
          "pdf": "pdf", "md": "markdown", "markdown": "markdown", "rst": "rst", "asciidoc": "asciidoc", "adoc": "asciidoc",
          "org": "org", "textile": "textile", "docx": "docx", "odt": "odt", "rtf": "rtf", "epub": "epub", "doc": "doc",
          "pptx": "pptx", "html": "html", "htm": "html",
          # Spreadsheets.
          "xlsx": "xlsx", "xlsm": "xlsx", "xltx": "xlsx", "xltm": "xlsx", "xlam": "xlsx", "xlsb": "xlsb",
          "xls": "xls", "xla": "xls", "xlt": "xls", "ods": "ods", "ots": "ods", "fods": "fods", "slk": "sylk", "dif": "dif"}
# Category folders, as corpus/real is laid out: data/, docs/, sheets/ (3d/ is
# fetch-real-3d.sh's).
DOCS = {"asciidoc", "doc", "docx", "epub", "html", "markdown", "odt", "org", "pdf", "pptx", "rst", "rtf", "textile", "xps", "comic"}
SHEETS = {"dif", "fods", "ods", "sylk", "xls", "xlsb", "xlsx"}
counts = {}
for repo in [os.path.basename(clone)]:
    root = clone
    for dp, dirs, files in os.walk(root):
        dirs[:] = [d for d in dirs if d != ".git"]
        for f in files:
            path = os.path.join(dp, f)
            rel = os.path.relpath(path, root)
            ext = f.rsplit(".", 1)[-1].lower() if "." in f else ""
            reader = by_ext.get(ext)
            head = open(path, "rb").read(256)
            if ext == "vcf":
                reader = "vcard" if b"BEGIN:VCARD" in head else "vcf"
            if ext == "txt":
                if repo.startswith("imsweb_x12"): reader = "x12"
                elif repo.startswith("nerdocs_pydifact"): reader = "edifact"
                elif repo.startswith("Efferent-Health"): reader = "hl7"
                elif repo.startswith("WoLpH_mt940"): reader = "swift"
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
