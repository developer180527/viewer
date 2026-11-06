#!/usr/bin/env python3
"""Writes the corpus's synthetic files into corpus/files/ (stdlib only).

Usage: python3 corpus/make.py

The output is committed: rerun this only to add or change a file, then refresh the
expected summaries with UPDATE_CORPUS=1 (corpus/README.md).
"""
import gzip, json, math, os, struct, zlib

ROOT = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(ROOT, "files")


def write(name, data):
    path = os.path.join(OUT, name)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as f:
        f.write(data if isinstance(data, bytes) else data.encode())


def png(w, h, pixel):
    rows = b"".join(b"\0" + b"".join(bytes(pixel(x, y)) for x in range(w)) for y in range(h))
    return png_chunks(struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0), zlib.compress(rows, 9))


def png_chunks(ihdr, idat):
    def chunk(tag, data):
        return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", zlib.crc32(tag + data))
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) + chunk(b"IDAT", idat) + chunk(b"IEND", b"")


def checker(x, y):
    return (230, 120, 40, 255) if (x // 8 + y // 8) % 2 else (40, 60, 90, 255)


# ---------------------------------------------------------------- images

def images():
    w, h = 32, 16
    rows = []
    for y in range(h - 1, -1, -1):  # BMP rows go bottom up
        row = b"".join(bytes((x * 8, y * 16, 128)) for x in range(w))
        rows.append(row + b"\0" * ((4 - len(row) % 4) % 4))
    pixels = b"".join(rows)
    header = struct.pack("<2sIHHI", b"BM", 54 + len(pixels), 0, 0, 54)
    info = struct.pack("<IiiHHIIiiII", 40, w, h, 1, 24, 0, len(pixels), 2835, 2835, 0, 0)
    write("images/ramp.bmp", header + info + pixels)
    write("images/ramp.ppm", b"P6\n32 16\n255\n" + b"".join(bytes((x * 8, y * 16, 128)) for y in range(h) for x in range(w)))
    write("images/checker.png", png(64, 64, checker))
    write("images/ramp.pgm", b"P5\n32 16\n255\n" + bytes(x * 8 for y in range(h) for x in range(w)))


# ---------------------------------------------------------------- 3D

def cube():
    """A unit cube: 24 vertices (flat normals, UVs per face), 36 indices."""
    faces = [((1, 0, 0), (0, 1, 0)), ((-1, 0, 0), (0, 1, 0)), ((0, 1, 0), (0, 0, 1)),
             ((0, -1, 0), (0, 0, 1)), ((0, 0, 1), (0, 1, 0)), ((0, 0, -1), (0, 1, 0))]
    pos, nor, uv, idx = [], [], [], []
    for n, up in faces:
        right = (up[1] * n[2] - up[2] * n[1], up[2] * n[0] - up[0] * n[2], up[0] * n[1] - up[1] * n[0])
        base = len(pos)
        for su, sv in ((-1, -1), (1, -1), (1, 1), (-1, 1)):
            pos.append(tuple(0.5 * (n[i] + su * right[i] + sv * up[i]) for i in range(3)))
            nor.append(n)
            uv.append(((su + 1) / 2, (1 - sv) / 2))
        idx += [base, base + 1, base + 2, base, base + 2, base + 3]
    return pos, nor, uv, idx


def gltf_parts():
    pos, nor, uv, idx = cube()
    blob = b"".join(struct.pack("<3f", *p) for p in pos)
    blob += b"".join(struct.pack("<3f", *n) for n in nor)
    blob += b"".join(struct.pack("<2f", *t) for t in uv)
    blob += b"".join(struct.pack("<H", i) for i in idx)
    n = len(pos)
    views = [
        {"buffer": 0, "byteOffset": 0, "byteLength": n * 12},
        {"buffer": 0, "byteOffset": n * 12, "byteLength": n * 12},
        {"buffer": 0, "byteOffset": n * 24, "byteLength": n * 8},
        {"buffer": 0, "byteOffset": n * 32, "byteLength": len(idx) * 2},
    ]
    accessors = [
        {"bufferView": 0, "componentType": 5126, "count": n, "type": "VEC3", "min": [-0.5] * 3, "max": [0.5] * 3},
        {"bufferView": 1, "componentType": 5126, "count": n, "type": "VEC3"},
        {"bufferView": 2, "componentType": 5126, "count": n, "type": "VEC2"},
        {"bufferView": 3, "componentType": 5123, "count": len(idx), "type": "SCALAR"},
    ]
    doc = {
        "asset": {"version": "2.0"},
        "scene": 0,
        "scenes": [{"nodes": [0]}],
        "nodes": [{"mesh": 0, "name": "Cube"}],
        "meshes": [{"primitives": [{"attributes": {"POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2}, "indices": 3, "material": 0}]}],
        "materials": [{"name": "Checker", "pbrMetallicRoughness": {"baseColorTexture": {"index": 0}, "metallicFactor": 0.0}}],
        "textures": [{"source": 0}],
        "bufferViews": views,
        "accessors": accessors,
    }
    return doc, blob


def threed():
    texture = png(64, 64, checker)

    # glTF with its buffer and texture as separate files, the texture in a subfolder.
    doc, blob = gltf_parts()
    doc["buffers"] = [{"uri": "cube.bin", "byteLength": len(blob)}]
    doc["images"] = [{"uri": "textures/checker.png"}]
    write("3d/cube-gltf/cube.gltf", json.dumps(doc, indent=1))
    write("3d/cube-gltf/cube.bin", blob)
    write("3d/cube-gltf/textures/checker.png", texture)

    # The same glTF whose texture wasn't copied along: reported as missing.
    doc["images"] = [{"uri": "textures/not-here.png"}]
    write("3d/missing-texture/cube.gltf", json.dumps(doc, indent=1))
    write("3d/missing-texture/cube.bin", blob)

    # GLB: buffer and texture embedded.
    doc, blob = gltf_parts()
    pad = lambda b, c=b"\0": b + c * ((4 - len(b) % 4) % 4)
    blob = pad(blob)
    doc["bufferViews"].append({"buffer": 0, "byteOffset": len(blob), "byteLength": len(texture)})
    doc["images"] = [{"bufferView": len(doc["bufferViews"]) - 1, "mimeType": "image/png"}]
    blob = pad(blob + texture)
    doc["buffers"] = [{"byteLength": len(blob)}]
    js = pad(json.dumps(doc).encode(), b" ")
    body = struct.pack("<II", len(js), 0x4E4F534A) + js + struct.pack("<II", len(blob), 0x004E4942) + blob
    glb = struct.pack("<4sII", b"glTF", 2, 12 + len(body)) + body
    write("3d/cube.glb", glb)

    # OBJ + MTL whose texture is missing.
    write("3d/tri-obj/tri.obj", "mtllib tri.mtl\no Tri\nv 0 0 0\nv 1 0 0\nv 0 1 0\nvt 0 0\nvt 1 0\nvt 0 1\nusemtl Paint\nf 1/1 2/2 3/3\n")
    write("3d/tri-obj/tri.mtl", "newmtl Paint\nKd 0.8 0.2 0.2\nmap_Kd paint.png\n")

    # An mtllib name with spaces in it, as some exporters write: with its .mtl beside
    # it, and without (reported once, under its whole name, not word by word).
    obj = "mtllib My Paint.mtl\nv 0 0 0\nv 1 0 0\nv 0 1 0\nusemtl Paint\nf 1 2 3\n"
    write("3d/spaced-mtl/tri.obj", obj)
    write("3d/spaced-mtl/My Paint.mtl", "newmtl Paint\nKd 0.2 0.6 0.9\n")
    write("3d/spaced-mtl-missing/tri.obj", obj)

    # USD as text: one triangle, Y up.
    write("3d/tri.usda", """#usda 1.0
(
    upAxis = "Y"
)

def Mesh "Tri"
{
    int[] faceVertexCounts = [3]
    int[] faceVertexIndices = [0, 1, 2]
    point3f[] points = [(0, 0, 0), (1, 0, 0), (0, 1, 0)]
}
""")

    # STL, binary (the cube) and ASCII (a tetrahedron).
    pos, _, _, idx = cube()
    tris = [(pos[idx[i]], pos[idx[i + 1]], pos[idx[i + 2]]) for i in range(0, len(idx), 3)]
    stl = b"corpus cube".ljust(80, b"\0") + struct.pack("<I", len(tris))
    for a, b, c in tris:
        stl += struct.pack("<3f", 0, 0, 0) + struct.pack("<9f", *a, *b, *c) + b"\0\0"
    write("3d/cube.stl", stl)
    t = [(0, 0, 0), (1, 0, 0), (0, 1, 0), (0, 0, 1)]
    facets = "".join(
        "  facet normal 0 0 0\n    outer loop\n" + "".join(f"      vertex {x} {y} {z}\n" for x, y, z in (t[a], t[b], t[c])) + "    endloop\n  endfacet\n"
        for a, b, c in ((0, 2, 1), (0, 1, 3), (0, 3, 2), (1, 2, 3)))
    write("3d/tetra.stl", f"solid tetra\n{facets}endsolid tetra\n")

    # PLY point cloud with colours (ASCII), 100 points on a spiral.
    pts = []
    for i in range(100):
        a = i * 0.3
        pts.append(f"{0.01 * i * math.cos(a):.4f} {0.01 * i * math.sin(a):.4f} {0.01 * i:.4f} {i * 2} {255 - i * 2} 128")
    header = "ply\nformat ascii 1.0\nelement vertex 100\nproperty float x\nproperty float y\nproperty float z\nproperty uchar red\nproperty uchar green\nproperty uchar blue\nend_header\n"
    write("3d/spiral.ply", header + "\n".join(pts) + "\n")


    # FBX as text (ASCII 7.4): one triangle, with only what ufbx needs.
    write("3d/tri.fbx", """; FBX 7.4.0 project file
FBXHeaderExtension:  {
\tFBXHeaderVersion: 1003
\tFBXVersion: 7400
}
Objects:  {
\tGeometry: 1000, "Geometry::Tri", "Mesh" {
\t\tVertices: *9 {
\t\t\ta: 0,0,0,1,0,0,0,1,0
\t\t}
\t\tPolygonVertexIndex: *3 {
\t\t\ta: 0,1,-3
\t\t}
\t}
\tModel: 2000, "Model::Tri", "Mesh" {
\t}
}
Connections:  {
\tC: "OO",1000,2000
\tC: "OO",2000,0
}
""")

    # .blend: a legacy little-endian 64-bit header, a REND block, then the 4x4
    # preview Blender saves in a TEST block (rows bottom-up), then ENDB.
    def block(code, data):
        return code + struct.pack("<I", len(data)) + bytes(8 + 4 + 4) + data
    pixels = b"".join(bytes(checker(x * 8, y * 8)) for y in range(4) for x in range(4))
    blend = b"BLENDER-v283" + block(b"REND", bytes(72)) + block(b"TEST", struct.pack("<II", 4, 4) + pixels) + block(b"ENDB", b"")
    write("3d/preview.blend", blend)

# ---------------------------------------------------------------- PDF

def pdf(pages):
    """A small PDF with one line of Helvetica text per page; `pages` is a list of
    (width, height, text) in points."""
    objs = ["<< /Type /Catalog /Pages 2 0 R >>", None, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"]
    kids = []
    for w, h, text in pages:
        content = f"BT /F1 24 Tf 72 {h - 96} Td ({text}) Tj ET 0.9 0.4 0.1 rg 72 72 {w - 144} 48 re f".encode()
        objs.append(f"<< /Length {len(content)} >>\nstream\n" + content.decode() + "\nendstream")
        objs.append(f"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w} {h}] /Resources << /Font << /F1 3 0 R >> >> /Contents {len(objs)} 0 R >>")
        kids.append(f"{len(objs)} 0 R")
    objs[1] = f"<< /Type /Pages /Kids [{' '.join(kids)}] /Count {len(kids)} >>"
    out = b"%PDF-1.7\n"
    offsets = []
    for i, o in enumerate(objs, 1):
        offsets.append(len(out))
        out += f"{i} 0 obj\n{o}\nendobj\n".encode()
    xref = len(out)
    out += f"xref\n0 {len(objs) + 1}\n0000000000 65535 f \n".encode()
    out += "".join(f"{o:010d} 00000 n \n" for o in offsets).encode()
    out += f"trailer\n<< /Size {len(objs) + 1} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n".encode()
    return out


def docs():
    write("pdf/two-pages.pdf", pdf([(612, 792, "Corpus page one"), (842, 595, "Page two, landscape")]))


# ---------------------------------------------------------------- data

def data():
    write("data/people.tsv", "name\tage\tcity\nAda\t36\tLondon\nGrace\t85\tArlington\nLinus\t54\tPortland\n")
    write("data/events.jsonl", "".join(json.dumps({"id": i, "kind": ["open", "close"][i % 2]}) + "\n" for i in range(5)))


# ---------------------------------------------------------------- hostile

def hostile():
    """Files that must fail cleanly: no panic, no hang, no huge allocation. Each
    falls through to a loader that can show it (usually hex)."""
    good = png(64, 64, checker)
    write("hostile/truncated.png", good[:120])
    # Claims 100,000 x 100,000 pixels (40 GB of RGBA) with almost no data.
    write("hostile/huge-dimensions.png", png_chunks(struct.pack(">IIBBBBB", 100000, 100000, 8, 6, 0, 0, 0), zlib.compress(b"\0" * 64)))
    # 100,000 levels of nesting: a recursive parser without a depth limit overflows its stack.
    write("hostile/deep-nesting.json", "[" * 100000 + "]" * 100000)
    write("hostile/garbage.pdf", b"%PDF-1.7\n" + bytes((i * 151 + 7) % 256 for i in range(4096)))
    doc, blob = gltf_parts()
    write("hostile/truncated.glb", b"glTF" + struct.pack("<II", 2, 100000) + json.dumps(doc).encode()[:200])
    write("hostile/fake.zip", b"PK\x03\x04" + bytes((i * 37) % 256 for i in range(512)))
    write("hostile/empty.bin", b"")
    # 100 MB of zeros in ~100 KB: inflating it must stop at the cap, not run to the end.
    write("hostile/bomb.txt.gz", gzip.compress(b"\0" * (100 << 20), 9, mtime=0))
    # An OBJ face pointing at vertices that don't exist.
    write("hostile/bad-indices.obj", "v 0 0 0\nv 1 0 0\nf 1 2 99\nf -50 1 2\n")
    # A binary STL whose header promises far more triangles than it holds.
    write("hostile/short.stl", b"short".ljust(80, b"\0") + struct.pack("<I", 1_000_000_000) + b"\0" * 50)


def main():
    images()
    threed()
    docs()
    data()
    hostile()
    print("wrote", OUT)


if __name__ == "__main__":
    main()
