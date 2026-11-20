#!/bin/sh
# viewer-ffi from Swift, with no UI: compiles apple/Tests/main.swift against the Mac
# library and bindings (scripts/apple.sh builds them) and runs it on the samples.
set -eu
cd "$(dirname "$0")/.."
profile=${1:-debug}
[ -f target/apple/swift/ViewerFFI.swift ] || scripts/apple.sh mac "$profile"
out=target/apple/test
mkdir -p "$out"
nice swiftc -O -target arm64-apple-macos14.0 -module-name Smoke \
  -I target/apple/headers \
  -Xcc -fmodule-map-file=target/apple/headers/module.modulemap \
  target/apple/swift/ViewerFFI.swift apple/Tests/main.swift -o "$out/smoke" \
  -L "target/aarch64-apple-darwin/$profile" -Xlinker "target/aarch64-apple-darwin/$profile/libviewer_ffi.a" \
  -framework PDFKit -framework Foundation -framework CoreGraphics -framework CoreFoundation -lobjc -lc++ -liconv \
  -Xlinker -dead_strip 2>&1 | grep -v "built for newer" || true
"$out/smoke" "$PWD"
