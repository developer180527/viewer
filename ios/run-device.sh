#!/usr/bin/env bash
# Builds Viewer for a real iPhone or iPad, signs it with your Apple development
# team (Xcode makes the provisioning profile) and launches it with a sample.
#
#     ios/run-device.sh "Device name" TEAM_ID [SAMPLE]
#
# The device must be paired, with Developer Mode on.
set -euo pipefail
cd "$(dirname "$0")/.."
device=$1 team=$2 sample=${3:-main.rs}
id=dev.venugopal.viewer

cargo build -q --target aarch64-apple-ios -p viewer
xcodegen -q --spec ios/project.yml --project ios
# Xcode doesn't see the Rust binary as an input to signing: an incremental build
# copies the new binary in and leaves the old signature, which the device rejects.
rm -rf target/ios-device/Build/Products/Debug-iphoneos/Viewer.app
xcodebuild -quiet -project ios/Viewer.xcodeproj -scheme Viewer -configuration Debug \
    -destination generic/platform=iOS -derivedDataPath target/ios-device \
    -allowProvisioningUpdates DEVELOPMENT_TEAM="$team" build
app=target/ios-device/Build/Products/Debug-iphoneos/Viewer.app

xcrun devicectl device install app --device "$device" "$app"
# The samples ship inside the app; the app reads argv as on the desktop.
bundle=$(xcrun devicectl device info apps --device "$device" --bundle-id "$id" --json-output /dev/stdout 2>/dev/null \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["result"]["apps"][0]["url"].removeprefix("file://").rstrip("/"))')
xcrun devicectl device process launch --terminate-existing --device "$device" "$id" "$bundle/samples/$sample"
