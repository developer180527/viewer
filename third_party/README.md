# Third-party code

Submodules, each at a release tag, built by their crate's `build.rs`:

- `mupdf/`: MuPDF, for `libfileparser-mupdf` (AGPL, opt-in).
- `ffmpeg/`, `dav1d/`: FFmpeg and dav1d, for `libfileparser-media` (LGPL and BSD; docs/design/media.md).

## Patched copies

A crate whose fix we need before its next release lives here as a copy, used in place of the crates.io one through `[patch.crates-io]` in the workspace `Cargo.toml`. It stays out of the workspace (`exclude`), so our lints don't apply to it; `cargo test -p <crate>` from the root still runs its own tests. Each patch is offered upstream; when a release carries it, the copy goes.

### `office_oxide/` — 0.1.17 (MIT OR Apache-2.0), PowerPoint and Word 97–2003

Word 97–2003 (`.doc`) shows its automatic heading numbers ("3.1 Historique"), as Word and LibreOffice do. Measured on POI's and LibreOffice's 147 real DOC files: 527 numbered headings read as LibreOffice reads them, none of LibreOffice's left unnumbered; where the two differ, ours follow the document's own titles ("Header 2.99" numbered 2.99).

- `src/doc/list_format.rs`: **a bug fix.** `LSTF`'s flags are at offset 26, not 24 (offset 24 is `rgistdPara`'s last entry, usually `0x0FFF`), so nearly every multilevel list was read as a one-level list and every list definition after it misread. Each level now keeps its number text (`LVL.xst`), and an `LFOLVL` that replaces a level whole is kept too. `ListFormatting::number` counts paragraphs per list as Word does (deeper levels restart; an `LFO`'s start-at override applies at its first paragraph and the list continues from it) and writes the number in the level's format (decimal, Roman, letters, ordinal, zero-padded). Tests at the end of the file.
- `src/doc/styles.rs`, `src/doc/papx.rs`: a style's own list (`sprmPIlfo`, `sprmPIlvl`) is kept and inherited through `istdBase`, and a paragraph that states no list takes its style's, as in Word; a heading in a list with no level is at its outline level.
- `src/convert_doc.rs`: the paragraph walk counts list paragraphs and puts a numbered heading's number before its text.
