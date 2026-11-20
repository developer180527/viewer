#!/bin/sh
# Builds viewer-ffi and the SwiftUI shell for iPhone and iPad, and runs it.
#   apple/run-ios.sh                     the simulator: the first booted one
#   apple/run-ios.sh sim "Viewer iPad"   a simulator by name
#   apple/run-ios.sh device              the connected iPhone or iPad (debug)
#   apple/run-ios.sh device release      the same, optimised: what to measure
#   apple/run-ios.sh device release <ID>  a given device (xcrun devicectl list devices)
# A device must trust the developer profile once (Settings › General › VPN & Device
# Management) and have Developer Mode on.
set -eu
cd "$(dirname "$0")/.."
where=${1:-sim}
if [ "$where" = device ]; then
  profile=${2:-debug}
  config=Debug
  [ "$profile" = release ] && config=Release
  scripts/apple.sh ios "$profile"
  xcodegen --quiet --spec apple/project.yml --project apple
  # Again if need be, after a pause: right after the xcframework changes platforms,
  # Xcode's build service can still see the old ones for a few seconds ("Supported
  # platforms … is empty").
  build() {
    nice xcodebuild -quiet -project apple/ViewerShell.xcodeproj -scheme ViewerMobile -configuration "$config" \
      -destination 'generic/platform=iOS' -derivedDataPath target/apple/xcode -jobs 2 -allowProvisioningUpdates build
  }
  build || { sleep 5; build; } || { sleep 10; build; }
  app="target/apple/xcode/Build/Products/$config-iphoneos/Viewer.app"
  # The device named, or the first connected, paired iPhone or iPad.
  id=${3:-}
  [ -n "$id" ] || id=$(xcrun devicectl list devices 2>/dev/null | awk '/available \(paired\)/ && /physical/ { for (i = 1; i <= NF; i++) if ($i ~ /^[0-9A-F]{8}-/) { print $i; exit } }')
  [ -n "$id" ] || { echo "no paired device connected" >&2; exit 1; }
  xcrun devicectl device install app --device "$id" "$app" >/dev/null
  xcrun devicectl device process launch --device "$id" dev.venugopal.viewer.mobile
  exit 0
fi
scripts/apple.sh all
xcodegen --quiet --spec apple/project.yml --project apple
nice xcodebuild -quiet -project apple/ViewerShell.xcodeproj -scheme ViewerMobile \
  -configuration Debug -sdk iphonesimulator -derivedDataPath target/apple/xcode -jobs 2 build
app=target/apple/xcode/Build/Products/Debug-iphonesimulator/Viewer.app
device=${2:-booted}
[ "$device" = booted ] || xcrun simctl boot "$device" 2>/dev/null || true
xcrun simctl install "$device" "$app"
xcrun simctl launch "$device" dev.venugopal.viewer.mobile
