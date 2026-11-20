#!/bin/sh
# The stress set: real files from corpus/real at their largest, and generated ones
# where the corpus has nothing big enough. Written to target/stress/files (not kept
# in git; regenerated in seconds). Copied into the apps' Documents by
# scripts/stress/run-mac.sh and run-ios.sh.
set -eu
cd "$(dirname "$0")/../.."
out=target/stress/files
rm -rf "$PWD/$out"
mkdir -p "$out"
real=corpus/real

# Real: big, and of each kind the app draws.
cp "$real/docs/pdf/sample-files__009-pdflatex-geotopo__GeoTopo.pdf" "$out/geotopo.pdf"
cp "$real/3d/alecjacobson_common-3d-test-models/data/xyzrgb_dragon.obj" "$out/dragon.obj"
cp "$real/3d/KhronosGroup_glTF-Sample-Models/2.0/MosquitoInAmber/glTF-Binary/MosquitoInAmber.glb" "$out/mosquito.glb"
cp "$real/3d/usd-wg_assets/test_assets/USDZ/DamagedHelmet/DamagedHelmet.usdz" "$out/helmet.usdz"
mkdir -p "$out/sponza" && cp -R "$real/3d/KhronosGroup_glTF-Sample-Models/2.0/Sponza/glTF/." "$out/sponza/"
cp "$real/docs/docx/poi__test-data__document__saut_page.docx" "$out/document.docx"
cp "$real/docs/pptx/poi__test-data__slideshow__KEY02.pptx" "$out/slides.pptx"
cp "$real/docs/markdown/pandoc__changelog.md" "$out/changelog.md"
cp "$real/sheets/xlsx/poi__test-data__spreadsheet__LIBRE_OFFICE-116306-0.xlsx" "$out/sheet.xlsx"
cp "$real/3d/KhronosGroup_glTF-Sample-Models/2.0/MosquitoInAmber/glTF/MosquitoInAmber0.jpg" "$out/photo.jpg"

# Generated: an 8k image (upscaled, so it decodes like a real one), and data far
# past what the corpus has.
sips -s format png -z 8192 8192 "$real/3d/KhronosGroup_glTF-Sample-Models/2.0/ClearcoatRing/glTF/Carbon_normal.png" --out "$out/huge.png" >/dev/null
python3 -I - "$out" <<'PY'
import json, random, sys, zipfile
out = sys.argv[1]
random.seed(7)
words = "alpha beta gamma delta render frame tile page mesh texture cache worker queue".split()
with open(f"{out}/big.log", "w") as f:  # ~30 MB of log lines
    for i in range(400_000):
        f.write(f"2026-11-20T12:{i // 60 % 60:02}:{i % 60:02}.{i % 1000:03}Z INFO [{random.choice(words)}] request {i} took {random.random() * 100:.2f} ms " + " ".join(random.choices(words, k=6)) + "\n")
with open(f"{out}/big.csv", "w") as f:  # 300k rows
    f.write("id,name,city,amount,when,note\n")
    for i in range(300_000):
        f.write(f"{i},{random.choice(words)}{i},{random.choice(words).title()},{random.random() * 1e4:.2f},2026-{1 + i % 12:02}-{1 + i % 28:02},{' '.join(random.choices(words, k=4))}\n")
def node(depth):  # ~10 MB of nested JSON
    if depth == 0:
        return {"id": random.randrange(1 << 30), "name": random.choice(words), "v": random.random()}
    return {"items": [node(depth - 1) for _ in range(9)], "meta": {"depth": depth, "tag": random.choice(words)}}
with open(f"{out}/big.json", "w") as f:
    json.dump(node(6), f)
with zipfile.ZipFile(f"{out}/many.zip", "w", zipfile.ZIP_DEFLATED) as z:  # 5000 entries
    for i in range(5000):
        z.writestr(f"dir{i // 100:02}/file{i:04}.txt", f"entry {i}\n" * (1 + i % 50))
PY
du -sh "$out"/* | sort -rh
