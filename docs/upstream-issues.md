# Upstream issues

Found by running libraries over the real-world corpus (corpus/README.md), filed 2026-11-08. When one is fixed, measure that library on the corpus again: our own crate stays only while the numbers justify it (design/libfileparser.md §6).

| Library | Issue | Our crate |
|---|---|---|
| docx-rust 0.1.11 | [One unexpected field or value refuses the whole document](https://github.com/cstkingkey/docx-rs/issues/61) | `docx-lite` |
| rtf-parser 0.4.3 | [Panic on an unpaired surrogate](https://github.com/d0rianb/rtf-parser/issues/24) | `rtf-lite` |
| rtf-parser 0.4.3 | [`舒-` fails to parse](https://github.com/d0rianb/rtf-parser/issues/25) | `rtf-lite` |

## Drafts, not filed

Found measuring calamine 0.38 on 2,100 real spreadsheets.

**`Range::from_sparse` lays a sheet out densely, so a tiny file can end the process.** An XLSX of a few hundred bytes with cells at A1 and XFD1048576 asks for 2^34 cells, about 550 GB; the allocation aborts rather than returning an error. XLSX and XLSB have streaming cell readers that avoid it; XLS builds every sheet this way on open. POI's `60284.xls` and `51832.xls` ask for 105 and 134 GB. Suggestion: a sparse range, or a cell limit returning an error.

**ODS: `Expecting 'table-cell', found 'Text(...)` refuses whole files.** 46 of 399 LibreOffice test files fail with it.

**XLSB: a panic on a missing relationship.** `relationships[&*relid]` at `xlsb/mod.rs:312` panics on POI's `Simple.xlsb`. Suggestion: return an error.

**XLSX: `Unrecognized sheet:type` refuses a workbook with an Excel 4 macro sheet** (`xl/macrosheets/`), as in POI's `xlmmacro.xlsm` and `64420.xlsm`. Suggestion: list it as `SheetType::MacroSheet`.

**`open_workbook_auto_from_rs` reports `Cannot detect file format` for a file of a known format that failed to parse,** hiding the real error.
