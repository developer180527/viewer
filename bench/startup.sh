#!/usr/bin/env bash
# Startup benchmark: time from launch to the first frame showing each sample.
# Uses hyperfine when installed; otherwise prints the app's own --benchmark trace.
set -euo pipefail
cd "$(dirname "$0")/.."
# The libgui frontend: the one the app is moving to (PLAN.md §5).
cargo build --release -q --no-default-features --features libgui
bin=target/release/viewer
files=(samples/gradient.png samples/main.rs samples/data.json samples/table.csv samples/bundle.zip)
[[ -d bench/out ]] && files+=(bench/out/big.log bench/out/big.png)

if command -v hyperfine >/dev/null; then
    hyperfine --warmup 2 --runs 10 "${files[@]/#/$bin --benchmark }"
else
    for f in "${files[@]}"; do
        printf '%-28s ' "$f"
        "$bin" --benchmark "$f" 2>&1 | grep 'first frame showing' | awk '{print $2, $3}'
    done
fi
