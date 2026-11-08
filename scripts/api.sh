#!/usr/bin/env bash
# Public API snapshots (docs/design/libfileparser.md §5): each library's public API,
# as `cargo public-api` lists it, in `crates/<crate>/api/public-api.txt`.
#
#   scripts/api.sh           check: fail, with the diff, when an API differs from its snapshot
#   scripts/api.sh --update  write the snapshots (after an intended change; review the diff)
#
# Needs `cargo install cargo-public-api` and a nightly toolchain (it reads rustdoc's JSON).
set -euo pipefail
cd "$(dirname "$0")/.."

crates=(libfileparser-core libfileparser libfileparser-3d libfileparser-images libfileparser-text
        libfileparser-archives libfileparser-data libfileparser-docs libfileparser-testkit usd-lite gltf-lite rtf-lite docx-lite odt-lite scene-data libviewer)
status=0
diff_out=$(mktemp)
trap 'rm -f "$diff_out"' EXIT
for c in "${crates[@]}"; do
    snapshot="crates/$c/api/public-api.txt"
    # Simplified: no blanket or auto-trait impls, which change with the compiler, not with us.
    api=$(cargo public-api -p "$c" -ss --color never 2>/dev/null)
    if [[ "${1:-}" == --update ]]; then
        mkdir -p "$(dirname "$snapshot")"
        printf '%s\n' "$api" > "$snapshot"
        echo "wrote $snapshot ($(wc -l < "$snapshot" | tr -d ' ') items)"
    elif ! diff -u "$snapshot" <(printf '%s\n' "$api") > "$diff_out" 2>&1; then
        echo "::error::$c: public API differs from $snapshot (run scripts/api.sh --update and review):"
        cat "$diff_out"
        status=1
    fi
done
exit $status
