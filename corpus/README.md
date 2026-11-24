# Test corpus

Every file listed in `manifest.toml` is opened through the registry the app uses (`viewer-formats`), and what it opened as is compared with `expected/`. "Supported" means passing this on every platform.

```bash
cargo test -p viewer-corpus
```

## What is checked

For each file, a summary in `expected/<path>.toml`:
- which loader won, and which failed before it;
- the content model and its numbers (image size, lines, rows and columns, entries, page count and sizes, meshes, vertices, textures decoded, bounds);
- the metadata the info panel shows;
- the files it couldn't read, as blocked or missing.

For entries marked `golden`, also a small render in `expected/<path>.png`: the image, or a document's first page, at most 128 px. It may differ slightly between platforms (1% of pixels).

Every open runs on its own thread with a time limit, so a hang fails the test instead of stalling it. A panic outside a loader fails too (inside one, the registry falls through to the next loader, and the summary shows that).

## Files

- `samples/`, the app's own sample files.
- `files/`, committed, by libfileparser category, and for data by family:
  - `3d/`, `images/`, `docs/`, `sheets/`: models (with and without their textures), images, documents (the PDF among them) and workbooks; most written by `make.py` (standard library only).
  - A file goes under the category its format belongs to by convention, not by what it is made of (docs/design/libfileparser.md §2.1): a PDF is under `docs/`, an XML spreadsheet under `sheets/`.
  - `data/structured/`, `delimited/`, `config/`, `logs/`, `geo/`, `feeds/`, `l10n/`, `records/`, `sql/`, `schemas/`, `rdf/`, `graphs/`, `business/`, `science/`: a file or more per reader.
  - `data/science/genbank/`: real GenBank, GenPept and FASTA files from Biopython's test suite (its README says so).
  - `hostile/`: files that must fail cleanly.
- Real-world files marked `optional`, kept out of git (`test_3d_files/`). They're skipped where absent, as on CI.

## Real-world files

`fetch-real.sh` clones public test suites one at a time into a throwaway folder (latest commit only, only the test folders), sorts their files into `real/`, and deletes the clone; it clones (toml-test, JSONTestSuite, the W3C RDF tests, htslib, pydot, kdl, hcl, x12-parser, pydifact, ofxparse, mt940, Arelle; for documents GitHub's markup tests, py-pdf's sample files, python-docx, Apache POI, odfpy, EPUBCheck; and more: about 4,000 files, ~230 MB) into `real/<category>/<reader>/` (`data/`, `docs/`, `sheets/`), by the reader each file should open with; `real/README.md` (written by `index-real.py`) lists every folder and where its files came from; `<reader>-invalid/` holds test suites' negative cases, which must fail or fall back cleanly. They're kept out of git (each has its own licence) and checked by an opt-in test that fails on any crash or hang and reports, per folder, what opened as what:

```bash
corpus/fetch-real.sh
LFP_REAL_CORPUS=$PWD/corpus/real cargo test -p libfileparser --test real_world -- --ignored --nocapture
```

### Comic archives (7-Zip and RAR)

The CB7 and CBR readers (sevenz-rust2, rars) were measured on libarchive's own test archives, which `fetch-real.sh` doesn't fetch because libarchive keeps them uuencoded: clone `libarchive/libarchive` sparsely with `/libarchive/test/test_read_format_rar*.uu` and `/libarchive/test/test_read_format_7z*.uu`, then `uudecode` each. Of the 100 RAR files, rars read every one genuine UnRAR read except the deliberately malformed ones and single volumes of a set, and never claimed one UnRAR refused; sevenz-rust2 read all 82 7-Zip files but the encrypted, malformed and self-extracting ones (docs/decisions.md, 2026-11-19).

## Fuzzing

Every reader, and usd-lite and gltf-lite through their own APIs, is fuzzed by each crate's tests (`libfileparser-testkit`'s fuzzer): its samples and the corpus files it reads, damaged thousands of ways, opened, every facet made and read. Nothing may panic or hang. `LFP_FUZZ_ITERATIONS` (default 64) and `LFP_FUZZ_SEED` go further; failing inputs are saved under `target/fuzz-failures/`.

## Every format, and what tested it

`docs/formats.md` lists every reader by category library with what tested it: a corpus file, the conformance suite, the testkit's fuzzer, real-world files, the last soak. Each tick is from evidence the tests record (`LFP_TESTED`), never written in. Regenerate it after adding a reader or a round of testing:

```bash
scripts/formats.sh
```

## After a change

When a change is meant to alter what a file opens as, rewrite the expected files and review the diff before committing:

```bash
UPDATE_CORPUS=1 cargo test -p viewer-corpus
```

A failing render leaves what was drawn and a diff in `target/tmp/corpus/`.

## Adding a format

1. Add a small file to `make.py` (or to `samples/`), then run `python3 corpus/make.py`.
2. List it in `manifest.toml`.
3. Run with `UPDATE_CORPUS=1`, and check the new summary is right before committing it.

`every_loader_has_a_corpus_file` fails while a loader opens no committed corpus file. `LOCAL_ONLY` in `crates/corpus/tests/corpus.rs` lists the loaders that so far have only optional files.

## Against reference renderers

Opening isn't showing: a DOCX can open with half its text gone and pass everything above. `scripts/reference.sh` checks each document against the tool that defines its format: Pandoc for Markdown, the wikis, reST, Org, DOCX, ODT, EPUB, RTF, LaTeX and the rest it reads; LibreOffice for DOC, PPT, PPTX, ODP and HWP; Calibre for MOBI; Asciidoctor, makeinfo, mandoc and pod2html for their own formats; a mail client's rules (Python's email package) for messages. Both sides become the same outline (`crates/libfileparser/tests/outline.rs` writes ours), compared on what any renderer agrees on, not on layout:

- **text**: the words in order, as recall (how much of the reference's text we show) and precision (how much of ours it has);
- **headings**: found, and at the same level as the others (a constant shift, like a title added as level 1, is fine);
- **tables** and their cells, **list items**, **code blocks**, **images**: how many.

```bash
scripts/reference.sh                 # target/reference/{files,real}/report.md
scripts/reference.sh --only=docx,rtf
```

Pages formats (PDF, XPS, DjVu, comics) aren't compared: their engine is the reference. An mbox is shown as a list of messages, which no reference has. A missing tool is reported and its formats skipped.

Results, 2026-11-25, every tool installed (`target/reference/real/report.md` has every format and its worst files):

| format | files | clean | text recall | precision | headings at their level |
|---|---:|---:|---:|---:|---:|
| Markdown | 1236 | 1224 | 100% | 100% | 1130/1133 |
| DOCX | 244 | 205 | 100% | 100% | 111/113 |
| PPTX | 459 | 250 | 100% | 92% | – |
| email | 182 | 128 | 100% | 99% | – |
| DOC | 147 | 76 | 100% | 100% | 586/769 |
| PPT | 146 | 66 | 100% | 79% | – |
| ODP | 93 | 74 | 100% | 75% | – |
| man | 67 | 62 | 100% | 99% | 888/889 |
| ODT | 64 | 61 | 100% | 100% | 12/12 |
| AsciiDoc | 63 | 37 | 100% | 100% | 211/221 |
| EPUB | 32 | 27 | 100% | 100% | 112/114 |
| reST | 11 | 6 | 100% | 100% | 112/114 |

Fixed from what it found: man and mdoc bullet and numbered lists; tbl text blocks (`T{`…`T}`); mail headers that aren't addresses or dates, and `text/enriched` parts; reST grid, simple and list tables, `.. table::`, `Text::` literal blocks, and a directive option the parser refuses no longer losing the document; SmartArt text in PPTX; Org's TODO keywords; Jira's `{anchor:}`; AsciiDoc's author and revision lines.

Still different, and why:
- DOC headings: Word's automatic numbering ("3.1 Historique") isn't shown; the headings themselves are found.
- PPT, ODP, PPTX precision: our "Slide 3" headings, slide-number and date fields LibreOffice fills in, and SmartArt in the diagram's order rather than its layout's.
- HWP isn't compared: LibreOffice for macOS has no HWP import.
- AsciiDoc fixtures whose includes name files the corpus stores under other names.

## Real-world 3D

`fetch-real-3d.sh` works the same way but keeps each repository's folders, since a model finds its buffers and textures by relative path. `real/3d/` (about 1.5 GB, ignored by git) holds Khronos's glTF samples, the USD Working Group's assets, Pixar's OpenUSD examples, Assimp's and ufbx's test models, the Blender glTF exporter's test scenes and more. An opt-in test goes beyond "did it open": it counts models that opened with nothing to draw, with broken bounds, with files they couldn't find, or under another reader, and says why:

```bash
LFP_REAL_3D=$PWD/corpus/real/3d cargo test --release -p libfileparser --features docs-mupdf --test real_3d -- --ignored --nocapture
```
