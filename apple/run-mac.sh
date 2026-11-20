#!/bin/sh
# Builds viewer-ffi and the SwiftUI shell for macOS, and launches it.
#   apple/run-mac.sh            debug
#   apple/run-mac.sh release
set -eu
cd "$(dirname "$0")/.."
profile=${1:-debug}
scripts/apple.sh mac "$profile"
xcodegen --quiet --spec apple/project.yml --project apple
config=Debug
[ "$profile" = release ] && config=Release
nice xcodebuild -quiet -project apple/ViewerShell.xcodeproj -scheme ViewerShell \
  -configuration "$config" -derivedDataPath target/apple/xcode -jobs 2 build
open "target/apple/xcode/Build/Products/$config/Viewer.app"
