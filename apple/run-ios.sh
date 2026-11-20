#!/bin/sh
# Builds viewer-ffi and the SwiftUI shell for the iOS simulator, and runs it there.
#   apple/run-ios.sh                    on the first booted simulator
#   apple/run-ios.sh "iPad Pro 13-inch (M5)"
set -eu
cd "$(dirname "$0")/.."
scripts/apple.sh all
xcodegen --quiet --spec apple/project.yml --project apple
nice xcodebuild -quiet -project apple/ViewerShell.xcodeproj -scheme ViewerMobile \
  -configuration Debug -sdk iphonesimulator -derivedDataPath target/apple/xcode -jobs 2 build
app=target/apple/xcode/Build/Products/Debug-iphonesimulator/Viewer.app
device=${1:-booted}
[ "$device" = booted ] || xcrun simctl boot "$device" 2>/dev/null || true
xcrun simctl install "$device" "$app"
xcrun simctl launch "$device" dev.venugopal.viewer.mobile
