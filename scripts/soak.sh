#!/usr/bin/env bash
# Overnight soak: every cargo-fuzz target (coverage-guided, libFuzzer) for HOURS, one
# core each, plus the testkit's mutation fuzzer over thousands of seeds on another.
# Crashes, timeouts and out-of-memory inputs are kept and the run goes on.
# At the end soak/report.md says what ran, how far it got, and what it found.
#
#   scripts/soak.sh            # 8 hours
#   HOURS=2 scripts/soak.sh
#   SECS=120 scripts/soak.sh  # a smoke test
#   scripts/soak.sh report     # the report again, from what's there
#
# Needs nightly Rust and cargo-fuzz (cargo install cargo-fuzz). Runs niced.
set -uo pipefail
cd "$(dirname "$0")/.."
ROOT=$PWD
HOURS=${HOURS:-8}
SECS=${SECS:-$((HOURS * 3600))}
OUT=$ROOT/soak
TARGETS=(data sheets docs threed text archives images rtf ods_flat usd gltf)
# Memory each target may hold before libFuzzer calls it an out-of-memory finding.
RSS=${RSS:-1200}

report() {
  local r=$OUT/report.md
  {
    echo "# Soak report"
    echo
    echo "Host: $(hostname), $(nproc) cores. Finished $(date -u +%Y-%m-%dT%H:%MZ). Commit: $(cat "$OUT/commit" 2>/dev/null || echo unknown)."
    echo
    echo "| Target | Runs | Coverage | Corpus | Crashes | Timeouts | Out of memory |"
    echo "|---|---|---|---|---|---|---|"
    for t in "${TARGETS[@]}"; do
      local log=$OUT/logs/$t.log a=$ROOT/fuzz/artifacts/$t
      local runs cov
      runs=$(grep -oE '^#[0-9]+' "$log" 2>/dev/null | tail -1 | tr -d '#')
      cov=$(grep -oE 'cov: [0-9]+' "$log" 2>/dev/null | tail -1 | grep -oE '[0-9]+')
      echo "| $t | ${runs:-?} | ${cov:-?} | $(ls "$ROOT/fuzz/corpus/$t" 2>/dev/null | wc -l) | $(ls "$a" 2>/dev/null | grep -c '^crash-') | $(ls "$a" 2>/dev/null | grep -c '^timeout-') | $(ls "$a" 2>/dev/null | grep -c '^oom-') |"
    done
    echo
    echo "## Findings"
    echo
    local found
    found=$(find "$ROOT/fuzz/artifacts" -type f 2>/dev/null | wc -l)
    for t in "${TARGETS[@]}"; do
      local a=$ROOT/fuzz/artifacts/$t bin
      bin=$(ls -d "$ROOT"/fuzz/target/*/release 2>/dev/null | head -1)/$t
      [ -d "$a" ] || continue
      # One line per distinct place: where it panicked, or what libFuzzer said.
      for f in "$a"/*; do
        [ -f "$f" ] || continue
        local what
        # The input again, alone: where it panicked, or what libFuzzer or ASan said.
        what=$({ timeout 60 "$bin" -runs=1 -rss_limit_mb=$RSS -malloc_limit_mb=$RSS -timeout=20 "$f" 2>&1 || true; } | grep -m1 -oE "panicked at [^ ]+|ERROR: libFuzzer: [a-z -]+|ERROR: AddressSanitizer: [a-z -]+" | head -1)
        [ -n "$what" ] || what="no longer reproduces"
        echo "$t|$what|$(basename "$f")"
      done
    done | sort | awk -F'|' '{k=$1"|"$2; n[k]++; if(!(k in ex)) ex[k]=$3} END {for (k in n) {split(k,p,"|"); printf "- **%s** %s: %d input(s), e.g. `fuzz/artifacts/%s/%s`\n", p[1], p[2], n[k], p[1], ex[k]}}' | sort
    [ "$found" -gt 0 ] || echo "None."
    echo
    echo "## Testkit mutation fuzzer"
    echo
    echo '```'
    echo "$(grep -c '^== ' "$OUT/logs/testkit.log" 2>/dev/null) crate runs; failures, if any:"
    grep -E "FAILED|fuzz failures|  [a-z0-9-]+ on " "$OUT/logs/testkit.log" 2>/dev/null | sort -u | head -80
    echo '```'
  } > "$r"
  echo "wrote $r"
}

if [ "${1:-}" = report ]; then report; exit 0; fi

mkdir -p "$OUT/logs"
echo "${SOAK_COMMIT:-$(git rev-parse --short HEAD 2>/dev/null || date -u +%F)}" > "$OUT/commit"
export CARGO_BUILD_JOBS=$(nproc)
echo "building the fuzz targets..."
(cd fuzz && nice -n 10 cargo +nightly fuzz build -O) > "$OUT/logs/build.log" 2>&1 || { echo "build failed: $OUT/logs/build.log"; exit 1; }
if [ -z "$(ls fuzz/corpus 2>/dev/null)" ]; then
  echo "seeding from corpus/..."
  "$(ls -d fuzz/target/*/release | head -1)/seed" fuzz/corpus corpus/files corpus/real >> "$OUT/logs/build.log" 2>&1
fi

echo "fuzzing ${#TARGETS[@]} targets for $HOURS h..."
for t in "${TARGETS[@]}"; do
  mkdir -p "fuzz/corpus/$t" "fuzz/artifacts/$t"
  (cd fuzz && nice -n 10 cargo +nightly fuzz run -O "$t" "corpus/$t" -- \
      -max_total_time=$SECS -fork=1 -ignore_crashes=1 -ignore_timeouts=1 -ignore_ooms=1 \
      -rss_limit_mb=$RSS -malloc_limit_mb=$RSS -timeout=20 -max_len=1048576 \
      -artifact_prefix="artifacts/$t/" -print_final_stats=1) > "$OUT/logs/$t.log" 2>&1 &
done

# The testkit's fuzzer on the last core: every category's test, seed after seed.
(
  export CARGO_BUILD_JOBS=1 LFP_FUZZ_ITERATIONS=2000
  end=$((SECONDS + SECS))
  seed=1
  while [ $SECONDS -lt $end ]; do
    for p in libfileparser-data libfileparser-sheets libfileparser-docs libfileparser-3d libfileparser-text libfileparser-archives libfileparser-images usd-lite gltf-lite; do
      [ $SECONDS -lt $end ] || break
      echo "== $p seed $seed"
      LFP_FUZZ_SEED=$seed nice -n 15 timeout 3600 cargo test -q -p $p fuzz 2>&1 | grep -E "test result|fuzz failures|  [a-z0-9-]+ on "
    done
    seed=$((seed + 1))
  done
) > "$OUT/logs/testkit.log" 2>&1 &

wait
report
