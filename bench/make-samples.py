#!/usr/bin/env python3
"""Writes one small sample per supported format into samples/ (stdlib only).

Usage: python3 bench/make-samples.py [--big]
  --big also writes multi-MB stress files into bench/out/ (not committed).
"""
import io, json, math, os, struct, sys, tarfile, zipfile, zlib, gzip

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "samples")


def png(w, h, pixel):
    rows = b"".join(b"\0" + b"".join(bytes(pixel(x, y)) for x in range(w)) for y in range(h))
    def chunk(tag, data):
        return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", zlib.crc32(tag + data))
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(rows, 9)) + chunk(b"IEND", b""))


def gradient(x, y, w=640, h=400):
    r = int(255 * x / w)
    g = int(255 * y / h)
    b = int(128 + 127 * math.sin((x + y) / 40))
    return (r, g, b, 255)


def write(name, data):
    path = os.path.join(OUT, name)
    with open(path, "wb") as f:
        f.write(data if isinstance(data, bytes) else data.encode())
    return path


def main():
    os.makedirs(OUT, exist_ok=True)
    write("gradient.png", png(640, 400, gradient))
    write("logo.svg", """<svg xmlns="http://www.w3.org/2000/svg" width="240" height="160" viewBox="0 0 240 160">
  <rect width="240" height="160" rx="16" fill="#1e293b"/>
  <circle cx="70" cy="80" r="44" fill="#f97316"/>
  <path d="M130 40h70v20h-50v10h40v20h-40v30h-20z" fill="#38bdf8"/>
</svg>
""")
    write("README.md", """# Sample Markdown

Viewer renders **Markdown** with *emphasis*, `inline code`, lists and tables.

- Images, documents, 3D, media, archives, data
- Falls back to a hex view for anything unknown

| Format | Model  |
|--------|--------|
| PNG    | Raster |
| JSON   | Tree   |

```rust
fn main() {
    println!("hello from a code block");
}
```

> Quick to open, quick to close.
""")
    write("main.rs", """use std::collections::HashMap;

/// Counts words in a sentence.
fn count(text: &str) -> HashMap<&str, usize> {
    let mut counts = HashMap::new();
    for word in text.split_whitespace() {
        *counts.entry(word).or_insert(0) += 1;
    }
    counts
}

fn main() {
    let counts = count("the quick brown fox jumps over the lazy dog the end");
    println!("{counts:?}"); // 42
}
""")
    write("data.json", json.dumps({
        "name": "viewer",
        "version": "0.1.0",
        "features": ["images", "text", "markdown", "json", "csv", "archives", "hex"],
        "startup_ms": 87.5,
        "cross_platform": True,
        "license": None,
        "targets": [{"os": o, "gpu": g} for o, g in
                    [("macOS", "Metal"), ("Windows", "DX12"), ("Linux", "Vulkan"), ("Android", "Vulkan"), ("iOS", "Metal")]],
        "numbers": list(range(500)),
    }, indent=2))
    # Not "Cargo.toml": a package manifest inside the workspace breaks cargo in samples/.
    write("example.toml", """[package]
name = "example"
version = "1.2.3"
edition = "2024"

[dependencies]
serde = { version = "1", features = ["derive"] }
""")
    write("table.csv", "id,city,country,population\n" + "".join(
        f"{i},City {i},{['India','Japan','Brazil','Kenya','France'][i % 5]},{(i * 7919) % 900000 + 10000}\n"
        for i in range(1, 2001)))
    blob = bytes((i * 31 + (i >> 3)) & 0xFF for i in range(4096))
    write("blob.bin", b"VIEWERBIN\x00\x01\x02" + blob)

    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr("docs/README.md", open(os.path.join(OUT, "README.md")).read())
        z.writestr("img/gradient.png", open(os.path.join(OUT, "gradient.png"), "rb").read())
        z.writestr("data/data.json", open(os.path.join(OUT, "data.json")).read())
        z.writestr("src/main.rs", open(os.path.join(OUT, "main.rs")).read())
    write("bundle.zip", buf.getvalue())

    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w:gz") as t:
        for name in ["table.csv", "logo.svg", "bundle.zip"]:
            t.add(os.path.join(OUT, name), arcname=f"nested/{name}")
    write("archive.tar.gz", buf.getvalue())
    write("notes.txt.gz", gzip.compress(b"A gzipped text file.\nOpened through the gzip container.\n"))

    if "--big" in sys.argv:
        big = os.path.join(ROOT, "bench", "out")
        os.makedirs(big, exist_ok=True)
        with open(os.path.join(big, "big.log"), "w") as f:
            for i in range(2_000_000):
                f.write(f"2026-10-30T12:00:{i % 60:02d}Z INFO request id={i} path=/api/v1/items/{i % 977} status=200\n")
        with open(os.path.join(big, "big.png"), "wb") as f:
            f.write(png(9000, 5000, lambda x, y: ((x // 8) & 255, (y // 8) & 255, (x ^ y) & 255, 255)))
    print("samples written to", OUT)


if __name__ == "__main__":
    main()
