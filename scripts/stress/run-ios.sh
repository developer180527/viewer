#!/bin/sh
# Stress and profile the iPhone/iPad app (Release, with symbols) on a connected
# device: the same two passes as run-mac.sh, plus one Power Profiler trace of all.
# Keep the device unlocked, screen on (Auto-Lock: Never), and connected; the script
# stops if it is locked rather than recording nothing.
#   scripts/stress/run-ios.sh <device UDID> [scenario,scenario]
set -eu
cd "$(dirname "$0")/../.."
id=${1:?device UDID (xcrun devicectl list devices)}
only=${2:-}
[ -d target/stress/files ] || scripts/stress/files.sh
app=target/apple/xcode/Build/Products/Release-iphoneos/Viewer.app
bundle=dev.venugopal.viewer.mobile
xcrun devicectl device install app --device "$id" "$app" >/dev/null

# The files, into the app's Documents (one at a time: devicectl copies files).
(cd target/stress/files && find . -type f) | while read -r f; do
  f=${f#./}
  xcrun devicectl device copy to --device "$id" --domain-type appDataContainer --domain-identifier "$bundle" \
    --source "target/stress/files/$f" --destination "Documents/$f" >/dev/null
done

if xcrun devicectl device info lockState --device "$id" 2>/dev/null | grep -qi "passcodeRequired: true"; then
  echo "the device is locked: unlock it (and set Auto-Lock to Never)" >&2
  exit 1
fi
out=target/stress/results/ios-$(date +%Y%m%d-%H%M%S)
mkdir -p "$out/traces"
names=${only:-all}
echo "metrics run ($names) → $out/metrics.jsonl"
# The scenarios go in the environment: devicectl would read `-stress` as its own flags.
xcrun devicectl device process launch --device "$id" --console --terminate-existing \
  --environment-variables "{\"VIEWER_STRESS\":\"$names\"}" "$bundle" 2>/dev/null \
  | grep '^viewer-' > "$out/metrics.jsonl" || true

scenarios=$(grep -o '"scenario":"[^"]*"' "$out/metrics.jsonl" | cut -d'"' -f4 | sort -u | tr '\n' ' ')
for s in $scenarios; do
  echo "profiling $s"
  xcrun xctrace record --quiet --no-prompt --device "$id" --template 'Time Profiler' --output "$out/traces/$s.trace" \
    --time-limit 180s --env VIEWER_STRESS="$s" --launch -- "$bundle" >/dev/null 2>&1 || echo "  (no trace for $s)"
done
echo "power profile (all)"
xcrun xctrace record --quiet --no-prompt --device "$id" --template 'Power Profiler' --output "$out/power.trace" \
  --time-limit 900s --env VIEWER_STRESS=all --launch -- "$bundle" >/dev/null 2>&1 || echo "  (no power trace)"
python3 -I scripts/stress/summarize.py "$out"
