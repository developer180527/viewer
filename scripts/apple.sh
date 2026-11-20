#!/bin/sh
# viewer-ffi for Apple: Swift bindings and an .xcframework of the static library, for
# the SwiftUI shell (docs/design/apple-ui.md §3).
#
#   scripts/apple.sh            macOS only, debug: what the Mac app needs day to day
#   scripts/apple.sh all        macOS, iOS and the iOS simulator
#   scripts/apple.sh all release
#
# Writes target/apple/ViewerFFI.xcframework and target/apple/swift/ViewerFFI.swift.
# Gentle on the machine: nice, -j2.
set -eu
cd "$(dirname "$0")/.."

platforms=${1:-mac}
profile=${2:-debug}
case $profile in
  release) flag=--release ;;
  debug) flag= ;;
  *) echo "profile is debug or release" >&2; exit 2 ;;
esac
case $platforms in
  mac) targets="aarch64-apple-darwin" ;;
  all) targets="aarch64-apple-darwin aarch64-apple-ios aarch64-apple-ios-sim" ;;
  *) echo "platforms is mac or all" >&2; exit 2 ;;
esac

# C dependencies (built by the cc crate) target what the apps support, not this Mac.
export MACOSX_DEPLOYMENT_TARGET=14.0 IPHONEOS_DEPLOYMENT_TARGET=15.0

out=target/apple
rm -rf "$PWD/$out/swift" "$PWD/$out/headers" "$PWD/$out/ViewerFFI.xcframework"
mkdir -p "$out/swift" "$out/headers"

# Line tables only, for these targets: backtraces keep file and line, and the static
# library is a fraction of full debug info's size (copied three times on the way to
# the app). The host's own build is untouched: it's a different target directory.
for t in $targets; do
  echo "building viewer-ffi for $t ($profile)"
  CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_RELEASE_DEBUG=line-tables-only \
    nice cargo build -q -j2 -p viewer-ffi --lib --target "$t" $flag
done

# The bindings come from the library's own metadata (library mode), read from the
# Mac target's dylib.
nice cargo run -q -j2 -p viewer-ffi --features cli --bin uniffi-bindgen -- \
  generate --library "target/aarch64-apple-darwin/$profile/libviewer_ffi.dylib" --language swift \
  --out-dir "$out/gen" >/dev/null

mv "$out/gen/"*.swift "$out/swift/ViewerFFI.swift"
mv "$out/gen/"*.h "$out/headers/"
# xcodebuild wants the module map named module.modulemap.
mv "$out/gen/"*.modulemap "$out/headers/module.modulemap"
rm -rf "$PWD/$out/gen"

args=
for t in $targets; do
  args="$args -library target/$t/$profile/libviewer_ffi.a -headers $out/headers"
done
# shellcheck disable=SC2086
xcodebuild -create-xcframework $args -output "$out/ViewerFFI.xcframework" >/dev/null
echo "wrote $out/ViewerFFI.xcframework and $out/swift/ViewerFFI.swift"
