#!/usr/bin/env bash
# docs/formats.md: every reader by category library, with what tested it, each tick
# from evidence (scripts/formats.py says which). Rewrite it after adding a reader or
# a round of testing, and review the diff:
#
#   scripts/formats.sh                 everything, the real-world corpus included if fetched
#   FORMATS_SKIP_REAL=1 scripts/formats.sh   without the real-world run (minutes)
#
# Runs niced on two jobs: it builds and runs every category's tests.
set -euo pipefail
cd "$(dirname "$0")/.."
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

nice cargo run -q -j2 -p libfileparser --example formats > "$tmp/readers.tsv"
# Conformance and fuzz: the testkit records each reader it checks (LFP_TESTED).
LFP_TESTED="$tmp/tested.tsv" nice cargo test -q -j2 \
  -p libfileparser-images -p libfileparser-3d -p libfileparser-data -p libfileparser-docs \
  -p libfileparser-sheets -p libfileparser-text -p libfileparser-archives > "$tmp/tests.log" 2>&1 || {
  echo "some category tests failed; their readers stay unticked (see below)" >&2
  grep -E "panicked|FAILED" "$tmp/tests.log" | head -20 >&2
}
real=""
if [[ -d corpus/real && -z "${FORMATS_SKIP_REAL:-}" ]]; then
  LFP_REAL_CORPUS="$PWD/corpus/real" nice cargo test -q -j2 -p libfileparser --test real_world -- --ignored --nocapture > "$tmp/real.log" 2>&1 || true
  real="$tmp/real.log"
fi
python3 -I scripts/formats.py "$tmp/readers.tsv" "$tmp/tested.tsv" "$real" > docs/formats.md
echo "wrote docs/formats.md"
