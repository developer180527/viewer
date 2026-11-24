#!/usr/bin/env bash
# Documents against their reference renderers (corpus/reference.py): our outline of
# every document, then the reference tool's, compared. Reports in
# target/reference/<set>/report.md (per format, then the worst files) and report.tsv.
#
#   scripts/reference.sh            corpus/files/docs, and corpus/real/docs if fetched
#   scripts/reference.sh --only=docx,rtf
#
# Tools: brew install pandoc asciidoctor texinfo; brew install --cask libreoffice calibre
# (mandoc and pod2html come with macOS). Formats whose tool is missing are listed, not
# compared. References are cached in target/reference/<set>/refs/: delete it after
# updating a tool.
set -euo pipefail
cd "$(dirname "$0")/.."
sets=(files)
[[ -d corpus/real/docs ]] && sets+=(real)
for set in "${sets[@]}"; do
    LFP_OUTLINE_IN="$PWD/corpus/$set/docs" LFP_OUTLINE_OUT="$PWD/target/reference/ours/$set" \
        cargo test -q --release -p libfileparser --test outline -- --ignored --nocapture
    python3 corpus/reference.py "corpus/$set/docs" "target/reference/ours/$set" "target/reference/$set" "$@"
done
