#!/usr/bin/env bash
# Builds Viewer for the iOS simulator, wraps it in an .app (with the samples, so
# there is something to open) and launches it on the booted simulator.
#
#     ios/run-sim.sh [SAMPLE]     # SAMPLE: a file in samples/, default main.rs
#
# A stopgap until the Xcode project of PLAN.md §5: no icon, no document picker.
set -euo pipefail
cd "$(dirname "$0")/.."
sample=${1:-main.rs}
target=aarch64-apple-ios-sim
id=dev.viewer.app

cargo build -q --target "$target" -p viewer
app=target/$target/debug/Viewer.app
rm -rf "$app"
mkdir -p "$app/samples"
cp "target/$target/debug/viewer" "$app/"
cp ios/Info.plist "$app/"
cp samples/* "$app/samples/"
codesign --force --sign - "$app" >/dev/null

xcrun simctl install booted "$app"
# The app reads argv like on the desktop; the samples are inside its bundle.
bundle=$(xcrun simctl get_app_container booted "$id" app)
xcrun simctl launch ${LOG:+--stderr="$LOG"} --terminate-running-process booted "$id" "$bundle/samples/$sample"
