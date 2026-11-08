#!/bin/sh
# Real-world 3D files from public repositories, for crates/libfileparser/tests/real_3d.rs.
# Kept out of git: corpus/real-3d/ is ignored, and each file keeps its repository's licence.
#
#   corpus/fetch-real-3d.sh
#   LFP_REAL_3D=$PWD/corpus/real-3d cargo test --release -p libfileparser --features docs-mupdf --test real_3d -- --ignored --nocapture
#
# Throwaway clones, as in fetch-real.sh, except that each repository's folders are kept:
# a model finds its buffers and textures by relative path, so they must stay beside it.
set -e
cd "$(dirname "$0")"
rm -rf real-3d
mkdir -p real-3d
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
keep='\.(gltf|glb|bin|fbx|obj|mtl|stl|ply|off|usd|usda|usdc|usdz|mtlx|blend|png|jpe?g|webp|ktx2?|dds|tga|bmp|tiff?|exr|hdr|gif)$'
fetch() {
  repo=$1; shift
  dir="$tmp/$(echo "$repo" | tr / _)"
  out="real-3d/$(echo "$repo" | tr / _)"
  if git clone -q --depth 1 --filter=blob:none --sparse "https://github.com/$repo.git" "$dir" \
     && git -C "$dir" sparse-checkout set --no-cone "$@"; then
    rm -rf "$dir/.git"
    mkdir -p "$out"
    (cd "$dir" && find . -type f | grep -iE "$keep" | while IFS= read -r f; do
      mkdir -p "$OLDPWD/$out/$(dirname "$f")" && cp "$f" "$OLDPWD/$out/$f"
    done)
    echo "$repo: $(find "$out" -type f | wc -l | tr -d ' ') files, $(du -sh "$out" | cut -f1)"
  else
    echo "skipped $repo: couldn't clone it" >&2
  fi
  rm -rf "$dir"
}
fetch KhronosGroup/glTF-Sample-Models '/2.0/*/glTF/' '/2.0/*/glTF-Binary/' '/2.0/*/glTF-Draco/' '/2.0/*/glTF-Embedded/'
fetch KhronosGroup/glTF-Asset-Generator '/Output/'
fetch usd-wg/assets '/test_assets/' '/full_assets/Teapot/' '/full_assets/StanfordBunny/'
fetch PixarAnimationStudios/OpenUSD '/extras/usd/tutorials/' '/extras/usd/examples/'
fetch assimp/assimp '/test/models/' '/test/models-nonbsd/'
fetch ufbx/ufbx '/data/'
fetch tinyobjloader/tinyobjloader '/models/'
fetch alecjacobson/common-3d-test-models '/data/'
fetch KhronosGroup/glTF-Blender-IO '/tests/scenes/**/*.blend'
fetch lukebitts/blend '/examples/blend_files/'
fetch mrdoob/three.js '/examples/models/fbx/' '/examples/models/obj/' '/examples/models/stl/' '/examples/models/ply/' '/examples/models/usdz/'
du -sh real-3d
