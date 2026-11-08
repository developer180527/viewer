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
  - `3d/`, `images/`, `pdf/`: models (with and without their textures), images, a PDF, written by `make.py` (standard library only).
  - `data/structured/`, `delimited/`, `config/`, `logs/`, `geo/`, `feeds/`, `l10n/`, `records/`, `sql/`, `schemas/`, `rdf/`, `graphs/`, `business/`, `science/`: a file or more per reader.
  - `data/science/genbank/`: real GenBank, GenPept and FASTA files from Biopython's test suite (its README says so).
  - `hostile/`: files that must fail cleanly.
- Real-world files marked `optional`, kept out of git (`test_3d_files/`). They're skipped where absent, as on CI.

## Real-world files

`fetch-real.sh` shallow-clones public test suites (toml-test, JSONTestSuite, the W3C RDF tests, htslib, pydot, kdl, hcl, x12-parser, pydifact, ofxparse, mt940, Arelle and more: about 4,000 files, ~50 MB) into `real/`, sorted by the reader each file should open with; `<reader>-invalid/` holds test suites' negative cases, which must fail or fall back cleanly. They're kept out of git (each has its own licence) and checked by an opt-in test that fails on any crash or hang and reports, per folder, what opened as what:

```bash
corpus/fetch-real.sh
LFP_REAL_CORPUS=corpus/real cargo test -p libfileparser --test real_world -- --ignored --nocapture
```

## Fuzzing

Every reader, and usd-lite and gltf-lite through their own APIs, is fuzzed by each crate's tests (`libfileparser-testkit`'s fuzzer): its samples and the corpus files it reads, damaged thousands of ways, opened, every facet made and read. Nothing may panic or hang. `LFP_FUZZ_ITERATIONS` (default 64) and `LFP_FUZZ_SEED` go further; failing inputs are saved under `target/fuzz-failures/`.

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
