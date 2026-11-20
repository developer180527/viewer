#!/bin/sh
# Stress and profile the SwiftUI Mac app (Release, with symbols):
#   1. a clean run of every scenario, its metrics in metrics.jsonl;
#   2. each scenario again under Instruments' Time Profiler, one trace each.
# Then scripts/stress/summarize.py tabulates both. Keep the window visible and leave
# the Mac alone while it runs (about 15 minutes): a covered window draws nothing.
#   scripts/stress/run-mac.sh [scenario,scenario]   default: all, then each traced
set -eu
cd "$(dirname "$0")/../.."
only=${1:-}
[ -d target/stress/files ] || scripts/stress/files.sh
app=target/apple/xcode/Build/Products/Release/Viewer.app
[ -d "$app" ] || { echo "build it first: scripts/apple.sh mac release, then the Release ViewerShell scheme" >&2; exit 1; }
bin="$app/Contents/MacOS/Viewer"

# The app is sandboxed: its Documents are in its container.
docs="$HOME/Library/Containers/dev.venugopal.viewer.shell/Data/Documents"
mkdir -p "$docs"
rsync -a --delete target/stress/files/ "$docs/"

out=target/stress/results/mac-$(date +%Y%m%d-%H%M%S)
mkdir -p "$out/traces"
names=${only:-all}
echo "metrics run ($names) → $out/metrics.jsonl"
"$bin" -stress "$names" 2>/dev/null | grep '^viewer-' > "$out/metrics.jsonl" || true

scenarios=$(grep -o '"scenarios":\[[^]]*\]' "$out/metrics.jsonl" | head -1 | tr -d '"[]' | sed 's/scenarios://' | tr ',' ' ')
[ "$scenarios" = all ] && scenarios=$(grep -o '"scenario":"[^"]*"' "$out/metrics.jsonl" | cut -d'"' -f4 | sort -u | tr '\n' ' ')
# Launched here and attached to by pid: `xctrace --launch` would start whichever
# copy of the app Launch Services has registered (a Debug build, say).
for s in $scenarios; do
  echo "profiling $s"
  "$bin" -stress "$s" -stress-delay 4 2>/dev/null | grep '^viewer-' > "$out/traces/$s.jsonl" &
  sleep 1
  pid=$(pgrep -n -f "$bin -stress $s") || { echo "  (didn't start)"; continue; }
  xcrun xctrace record --quiet --no-prompt --template 'Time Profiler' --output "$out/traces/$s.trace" \
    --time-limit 180s --attach "$pid" >/dev/null 2>&1 || echo "  (no trace for $s)"
  wait
done
python3 -I scripts/stress/summarize.py "$out"
