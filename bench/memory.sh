#!/usr/bin/env bash
# Memory benchmark: peak memory per case, in MB (PLAN.md §7, "Memory").
#
# On macOS this is the peak *memory footprint* (what Activity Monitor shows): memory
# the app owns, not clean pages of memory-mapped files, which the OS drops at will
# (a 156 MB log is ~220 MB of RSS but ~60 MB of footprint). Elsewhere it is peak RSS.
#
#   bench/memory.sh            measure and print a table
#   bench/memory.sh --check    also compare with bench/memory-budgets.txt; exit 1 on
#                              any case over its budget
#
# Columns:
#   headless  peak of --screenshot: the whole load, textures included, drawn on the CPU
#             (3D on a windowless GPU). The most repeatable number; budgets apply to it.
#   window    peak of --benchmark: a real window and GPU surface, three frames after the
#             file shows. Includes ~180 MB the Apple GPU driver takes for a process's
#             first render pass and marks reclaimable straight after (a bare wgpu clear
#             pass shows it too), so it is not the app's memory.
#   settled   footprint of the open window 6 s after launch (macOS only): what the app
#             holds while you look at the file, as Activity Monitor shows it.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --release -q --no-default-features --features libgui -p viewer
bin=target/release/viewer
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

peak_mb() { # command... → peak footprint (macOS) or RSS, in MB
    if [[ "$(uname)" == Darwin ]]; then
        /usr/bin/time -l "$@" 2>&1 >/dev/null | awk '/peak memory footprint/ {printf "%.0f", $1 / 1048576}'
    else
        /usr/bin/time -v "$@" 2>&1 >/dev/null | awk -F: '/Maximum resident set size/ {printf "%.0f", $2 / 1024}'
    fi
}

cases=("empty:")
for f in samples/gradient.png samples/main.rs samples/data.json samples/table.csv samples/bundle.zip; do cases+=("$(basename "$f"):$f"); done
for f in bench/out/big.log bench/out/big.png; do [[ -f $f ]] && cases+=("$(basename "$f"):$f"); done
# 3D models are not in git; measured when present.
for f in test_3d_files/long_life_food_4k.fbx/long_life_food_4k.fbx \
         test_3d_files/portable_generator_4k.usdc/portable_generator_4k.usdc \
         test_3d_files/namaqualand_boulder_02_4k.usdc/namaqualand_boulder_02_4k.usdc \
         "test_3d_files/32-mercedes-benz-gls-580-2020/uploads_files_2787791_Mercedes+Benz+GLS+580.fbx"; do
    [[ -f $f ]] && cases+=("$(basename "$f" | sed 's/uploads_files_2787791_//'):$f")
done

settled_mb() { # file → footprint of the running window after 6 s, in MB
    [[ "$(uname)" == Darwin ]] || { echo -; return; }
    "$bin" "$1" >/dev/null 2>&1 &
    local pid=$!
    sleep 6
    footprint -p "$pid" 2>/dev/null | awk '/Footprint:/ {
        for (i = 1; i < NF; i++) if ($i == "Footprint:") { v = $(i + 1); u = $(i + 2) }
        if (u == "GB") v *= 1024; if (u == "KB") v /= 1024
        printf "%.0f", v; exit
    }'
    kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null || true
}

budgets=bench/memory-budgets.txt
status=0
printf '%-40s %10s %10s %10s\n' case headless window settled
for c in "${cases[@]}"; do
    name=${c%%:*}; file=${c#*:}
    if [[ -n $file ]]; then
        h=$(peak_mb "$bin" --screenshot "$tmp/shot.png" "$file")
        w=$(peak_mb "$bin" --benchmark "$file")
        st=$(settled_mb "$file")
    else
        h=$(peak_mb "$bin" --screenshot "$tmp/shot.png")
        w=-; st=-
    fi
    printf '%-40s %10s %10s %10s' "$name" "$h" "$w" "$st"
    if [[ "${1:-}" == --check && -f $budgets ]]; then
        read -r hb sb < <(awk -v n="$name" '$1 == n {print $2, $3}' "$budgets")
        if [[ -n ${hb:-} && $h -gt $hb ]]; then
            printf '   OVER headless budget %s MB' "$hb"; status=1
        fi
        if [[ -n ${sb:-} && $sb != - && $st != - && ${st%.*} -gt $sb ]]; then
            printf '   OVER settled budget %s MB' "$sb"; status=1
        fi
    fi
    printf '\n'
done
exit $status
