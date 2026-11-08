#!/usr/bin/env python3
"""Writes the corpus's synthetic files into corpus/files/ (stdlib only).

Usage: python3 corpus/make.py

The output is committed: rerun this only to add or change a file, then refresh the
expected summaries with UPDATE_CORPUS=1 (corpus/README.md).
"""
import gzip, io, json, math, os, struct, zipfile, zlib

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

    # Binary FBX 7.4: the same triangle, and a Video record whose Content is a PNG
    # stored in the file (Embedded media).
    def rec(at, name, props, children=None):
        count = len(props)
        props = b"".join(props)
        start = at + 13 + len(name) + len(props)
        nested = b""
        if children is not None:
            for child in children:
                nested += rec(start + len(nested), *child)
            nested += bytes(13)
        end = start + len(nested)
        return struct.pack("<III", end, count, len(props)) + bytes([len(name)]) + name + props + nested
    def P(code, fmt, *v):
        return code + struct.pack("<" + fmt, *v)
    def S(text):
        return b"S" + struct.pack("<I", len(text)) + text
    def R(data):
        return b"R" + struct.pack("<I", len(data)) + data
    def arr(code, fmt, values):
        return code + struct.pack("<III", len(values), 0, len(values) * struct.calcsize(fmt)) + struct.pack("<%d%s" % (len(values), fmt), *values)
    texture = png(8, 8, checker)
    top = [
        (b"FBXHeaderExtension", [], [(b"FBXHeaderVersion", [P(b"I", "i", 1003)]), (b"FBXVersion", [P(b"I", "i", 7400)])]),
        (b"Objects", [], [
            (b"Geometry", [P(b"L", "q", 1000), S(b"Tri\x00\x01Geometry"), S(b"Mesh")], [
                (b"Vertices", [arr(b"d", "d", [0, 0, 0, 1, 0, 0, 0, 1, 0])]),
                (b"PolygonVertexIndex", [arr(b"i", "i", [0, 1, -3])]),
            ]),
            (b"Model", [P(b"L", "q", 2000), S(b"Tri\x00\x01Model"), S(b"Mesh")], [(b"Version", [P(b"I", "i", 232)])]),
            (b"Video", [P(b"L", "q", 3000), S(b"checker\x00\x01Video"), S(b"Clip")], [
                (b"Type", [S(b"Clip")]),
                (b"RelativeFilename", [S(b"textures\\checker.png")]),
                (b"Content", [R(texture)]),
            ]),
        ]),
        (b"Connections", [], [(b"C", [S(b"OO"), P(b"L", "q", 1000), P(b"L", "q", 2000)]), (b"C", [S(b"OO"), P(b"L", "q", 2000), P(b"L", "q", 0)])]),
    ]
    fbx = b"Kaydara FBX Binary  \x00\x1a\x00" + struct.pack("<I", 7400)
    for r in top:
        fbx += rec(len(fbx), *r)
    write("3d/tri-binary.fbx", fbx + bytes(13))

    # .blend with an SDNA (structs Link, ID, Object, Image, PackedFile, laid out as
    # Blender's are): an Object, an Image and the PNG packed into the file.
    names = [b"*next", b"*prev", b"name[66]", b"id", b"filepath[1024]", b"*packedfile", b"size", b"seek", b"*data"]
    types = [(b"char", 1), (b"int", 4), (b"void", 0), (b"Link", 16), (b"ID", 74), (b"Object", 74), (b"Image", 1106), (b"PackedFile", 16)]
    structs = [(3, [(2, 0), (2, 1)]), (4, [(2, 0), (0, 2)]), (5, [(4, 3)]), (6, [(4, 3), (0, 4), (2, 5)]), (7, [(1, 6), (1, 7), (2, 8)])]
    def pad(b):
        return b + bytes(-len(b) % 4)
    dna = pad(b"SDNANAME" + struct.pack("<I", len(names)) + b"".join(n + b"\0" for n in names))
    dna = pad(dna + b"TYPE" + struct.pack("<I", len(types)) + b"".join(t + b"\0" for t, _ in types))
    dna = pad(dna + b"TLEN" + b"".join(struct.pack("<H", n) for _, n in types))
    dna += b"STRC" + struct.pack("<I", len(structs)) + b"".join(struct.pack("<HH", t, len(f)) + b"".join(struct.pack("<HH", *x) for x in f) for t, f in structs)
    def block64(code, data, old=0, sdna=0):
        return code + struct.pack("<IQII", len(data), old, sdna, 1) + data
    def id_block(code, name, rest=b""):
        return bytes(8) + (code + name).ljust(66, b"\0") + rest
    image = id_block(b"IM", b"checker.png", b"//textures/checker.png".ljust(1024, b"\0") + struct.pack("<Q", 0x2000))
    blend = (b"BLENDER-v405" + block64(b"REND", bytes(72)) + block64(b"TEST", struct.pack("<II", 4, 4) + pixels)
        + block64(b"OB\0\0", id_block(b"OB", b"Tri"), 0x1000, 2)
        + block64(b"IM\0\0", image, 0x1800, 3)
        + block64(b"DATA", struct.pack("<iiQ", len(texture), 0, 0x3000), 0x2000, 4)
        + block64(b"DATA", texture, 0x3000, 0)
        + block64(b"DNA1", dna) + block64(b"ENDB", b""))
    write("3d/packed.blend", blend)

    # USDZ: the triangle layer and a texture, stored (never compressed), as the format
    # requires.
    usdz = io.BytesIO()
    with zipfile.ZipFile(usdz, "w", zipfile.ZIP_STORED) as z:
        # A fixed date, so the file is the same every time it's made.
        for name, data in [("tri.usda", open(os.path.join(OUT, "3d/tri.usda"), "rb").read()), ("textures/checker.png", png(8, 8, checker))]:
            z.writestr(zipfile.ZipInfo(name, date_time=(2026, 1, 1, 0, 0, 0)), data)
    write("3d/tri.usdz", usdz.getvalue())

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


def zip_of(parts, stored_first=None):
    """A zip with fixed dates (so the bytes don't change between runs); the first part
    stored uncompressed when named (EPUB's and ODF's `mimetype`)."""
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED) as z:
        for name, data in parts:
            method = zipfile.ZIP_STORED if name == stored_first else zipfile.ZIP_DEFLATED
            info = zipfile.ZipInfo(name, date_time=(2026, 1, 1, 0, 0, 0))
            info.compress_type = method
            z.writestr(info, data)
    return buf.getvalue()


W = 'xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"'


def docx():
    def run(text, props=""):
        return f'<w:r>{"<w:rPr>" + props + "</w:rPr>" if props else ""}<w:t xml:space="preserve">{text}</w:t></w:r>'
    body = (
        '<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr>' + run("Corpus document") + "</w:p>"
        + "<w:p>" + run("Plain, ") + run("bold", "<w:b/>") + run(", ") + run("italic", "<w:i/>") + run(" and ")
        + '<w:hyperlink r:id="rLink">' + run("a link") + "</w:hyperlink>" + run(".") + "</w:p>"
        + '<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr>' + run("First") + "</w:p>"
        + '<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr>' + run("Second") + "</w:p>"
        + "<w:tbl><w:tr><w:tc><w:p>" + run("A") + "</w:p></w:tc><w:tc><w:p>" + run("B") + "</w:p></w:tc></w:tr>"
        + "<w:tr><w:tc><w:p>" + run("1") + "</w:p></w:tc><w:tc><w:p>" + run("2") + "</w:p></w:tc></w:tr></w:tbl>"
    )
    return zip_of([
        ("[Content_Types].xml", '<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>'),
        ("_rels/.rels", '<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/></Relationships>'),
        ("word/document.xml", f'<?xml version="1.0"?><w:document {W}><w:body>{body}</w:body></w:document>'),
        ("word/_rels/document.xml.rels", '<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rLink" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.org" TargetMode="External"/></Relationships>'),
        ("word/numbering.xml", f'<?xml version="1.0"?><w:numbering {W}><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:numFmt w:val="decimal"/></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num></w:numbering>'),
        ("docProps/core.xml", '<?xml version="1.0"?><cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Corpus document</dc:title><dc:creator>Viewer</dc:creator></cp:coreProperties>'),
    ])


def odt():
    content = (
        '<?xml version="1.0"?><office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:xlink="http://www.w3.org/1999/xlink">'
        '<office:automatic-styles><style:style style:name="T1" style:family="text"><style:text-properties fo:font-weight="bold"/></style:style>'
        '<text:list-style style:name="L1"><text:list-level-style-number text:level="1"/></text:list-style></office:automatic-styles>'
        '<office:body><office:text><text:h text:outline-level="1">Corpus text</text:h>'
        '<text:p>Plain and <text:span text:style-name="T1">bold</text:span> and <text:a xlink:href="https://example.org">a link</text:a>.</text:p>'
        '<text:list text:style-name="L1"><text:list-item><text:p>One</text:p></text:list-item><text:list-item><text:p>Two</text:p></text:list-item></text:list>'
        '<table:table><table:table-row><table:table-cell><text:p>A</text:p></table:table-cell><table:table-cell><text:p>B</text:p></table:table-cell></table:table-row></table:table>'
        '</office:text></office:body></office:document-content>'
    )
    meta = '<?xml version="1.0"?><office:document-meta xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:meta="urn:oasis:names:tc:opendocument:xmlns:meta:1.0"><office:meta><dc:title>Corpus text</dc:title><meta:document-statistic meta:page-count="1" meta:word-count="12"/></office:meta></office:document-meta>'
    return zip_of([("mimetype", "application/vnd.oasis.opendocument.text"), ("content.xml", content), ("meta.xml", meta),
                   ("META-INF/manifest.xml", '<?xml version="1.0"?><manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0"/>')], stored_first="mimetype")


def epub():
    chapter = lambda title, body: f'<?xml version="1.0" encoding="utf-8"?><html xmlns="http://www.w3.org/1999/xhtml"><head><title>{title}</title></head><body><h1>{title}</h1>{body}</body></html>'
    opf = ('<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="id"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/">'
           '<dc:identifier id="id">corpus</dc:identifier><dc:title>Corpus Book</dc:title><dc:creator>Viewer</dc:creator><dc:language>en</dc:language></metadata>'
           '<manifest><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/><item id="c1" href="text/c1.xhtml" media-type="application/xhtml+xml"/>'
           '<item id="c2" href="text/c2.xhtml" media-type="application/xhtml+xml"/><item id="img" href="images/dot.png" media-type="image/png"/></manifest>'
           '<spine><itemref idref="c1"/><itemref idref="c2"/></spine></package>')
    nav = ('<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol>'
           '<li><a href="text/c1.xhtml">Chapter One</a></li><li><a href="text/c2.xhtml">Chapter Two</a></li></ol></nav></body></html>')
    return zip_of([
        ("mimetype", "application/epub+zip"),
        ("META-INF/container.xml", '<?xml version="1.0"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>'),
        ("OEBPS/content.opf", opf), ("OEBPS/nav.xhtml", nav),
        ("OEBPS/text/c1.xhtml", chapter("Chapter One", '<p>It <em>was</em> a <strong>dark</strong> night.</p><p><img src="../images/dot.png" alt="A dot"/></p>')),
        ("OEBPS/text/c2.xhtml", chapter("Chapter Two", "<ul><li>one</li><li>two</li></ul><blockquote><p>Quoted.</p></blockquote>")),
        ("OEBPS/images/dot.png", png(8, 8, checker)),
    ], stored_first="mimetype")


def comic(kind):
    pages = [(f"page{n}.png", png(48, 64, lambda x, y, n=n: (n * 60 % 256, x * 5, y * 4, 255))) for n in (1, 2, 10)]
    info = ("ComicInfo.xml", "<ComicInfo><Title>Corpus Comic</Title><Series>Samples</Series><Number>1</Number></ComicInfo>")
    if kind == "cbz":
        return zip_of(pages + [info])
    import tarfile
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w", format=tarfile.USTAR_FORMAT) as t:
        for name, data in pages + [(info[0], info[1].encode())]:
            ti = tarfile.TarInfo(name)
            ti.size, ti.mtime = len(data), 0
            t.addfile(ti, io.BytesIO(data))
    return buf.getvalue()


def xps():
    page = ('<FixedPage xmlns="http://schemas.microsoft.com/xps/2005/06" Width="816" Height="1056">'
            '<Path Data="M 96,96 L 720,96 720,192 96,192 Z" Fill="#FF2266CC"/></FixedPage>')
    return zip_of([
        ("[Content_Types].xml", '<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="fdseq" ContentType="application/vnd.ms-package.xps-fixeddocumentsequence+xml"/><Default Extension="fdoc" ContentType="application/vnd.ms-package.xps-fixeddocument+xml"/><Default Extension="fpage" ContentType="application/vnd.ms-package.xps-fixedpage+xml"/></Types>'),
        ("_rels/.rels", '<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="r1" Type="http://schemas.microsoft.com/xps/2005/06/fixedrepresentation" Target="/FixedDocSeq.fdseq"/></Relationships>'),
        ("FixedDocSeq.fdseq", '<FixedDocumentSequence xmlns="http://schemas.microsoft.com/xps/2005/06"><DocumentReference Source="/Documents/1/FixedDoc.fdoc"/></FixedDocumentSequence>'),
        ("Documents/1/FixedDoc.fdoc", '<FixedDocument xmlns="http://schemas.microsoft.com/xps/2005/06"><PageContent Source="/Documents/1/Pages/1.fpage"/></FixedDocument>'),
        ("Documents/1/Pages/1.fpage", page),
    ])


RTF = r"""{\rtf1\ansi\ansicpg1252{\fonttbl{\f0 Helvetica;}}{\stylesheet{\s1 heading 1;}}{\info{\title Corpus RTF}}
{\s1 Corpus RTF\par}
\pard Plain, {\b bold}, {\i italic} and caf\'e9 \u8212? all here.\par
{\field{\*\fldinst HYPERLINK "https://example.org"}{\fldrslt a link}}\par
\trowd\intbl A\cell B\cell\row
}
"""


def docs():
    write("pdf/two-pages.pdf", pdf([(612, 792, "Corpus page one"), (842, 595, "Page two, landscape")]))
    write("docs/sample.docx", docx())
    write("docs/sample.odt", odt())
    write("docs/sample.epub", epub())
    write("docs/sample.cbz", comic("cbz"))
    write("docs/sample.cbt", comic("cbt"))
    write("docs/sample.xps", xps())
    write("docs/sample.rtf", RTF)
    write("docs/sample.org", "#+TITLE: Corpus Org\n* Heading\nSome *bold*, /italic/ and =code=, with [[https://example.org][a link]].\n- one\n- two\n#+BEGIN_SRC sh\nls\n#+END_SRC\n| a | b |\n|---+---|\n| 1 | 2 |\n")


# ---------------------------------------------------------------- data

def data():
    write("data/delimited/people.tsv", "name\tage\tcity\nAda\t36\tLondon\nGrace\t85\tArlington\nLinus\t54\tPortland\n")
    write("data/structured/events.jsonl", "".join(json.dumps({"id": i, "kind": ["open", "close"][i % 2]}) + "\n" for i in range(5)))


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
