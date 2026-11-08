# Upstream issues

Found by running libraries over the real-world corpus (corpus/README.md), filed 2026-11-08. When one is fixed, measure that library on the corpus again: our own crate stays only while the numbers justify it (design/libfileparser.md §6).

| Library | Issue | Our crate |
|---|---|---|
| docx-rust 0.1.11 | [One unexpected field or value refuses the whole document](https://github.com/cstkingkey/docx-rs/issues/61) | `docx-lite` |
| rtf-parser 0.4.3 | [Panic on an unpaired surrogate](https://github.com/d0rianb/rtf-parser/issues/24) | `rtf-lite` |
| rtf-parser 0.4.3 | [`舒-` fails to parse](https://github.com/d0rianb/rtf-parser/issues/25) | `rtf-lite` |
