//! Meshes, objects and materials. Blender has stored a mesh four ways, all still found
//! in saved files:
//!
//! - up to 2.62: `mvert` and `mface` (triangles and quads);
//! - 2.63 to 3.4: `mvert`, `mpoly` and `mloop` (n-gons);
//! - 3.5 on: positions as a `position` attribute layer in `vdata`;
//! - 3.6 on: faces as `poly_offset_indices`, corners as a `.corner_vert` layer;
//! - 5.0 on: those attributes in an `attribute_storage` list instead of layers.
//!
//! Each piece is looked for in its newest place, then the older ones.

use std::collections::HashMap;

use crate::{Block, Dna, Field, File, text};

/// What a file holds to draw.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct Scene {
    pub meshes: Vec<Mesh>,
    pub materials: Vec<Material>,
    pub objects: Vec<Object>,
    /// What was skipped, and why.
    pub warnings: Vec<String>,
}

/// A mesh as saved: faces of any size, corners indexing `positions`.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct Mesh {
    pub name: String,
    pub positions: Vec<[f32; 3]>,
    /// Corners per face.
    pub face_sizes: Vec<u32>,
    /// Every face's corners, one after another; each is in range of `positions`.
    pub corners: Vec<u32>,
    /// Per face, an index into `slots`; empty when every face uses the first.
    pub face_slots: Vec<u16>,
    /// The mesh's material slots, as indices into [`Scene::materials`].
    pub slots: Vec<Option<usize>>,
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Material {
    pub name: String,
    /// The viewport colour.
    pub color: [f32; 3],
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Object {
    pub name: String,
    pub mesh: usize,
    /// World matrix, column by column, in Blender's Z-up space.
    pub matrix: [f32; 16],
}

/// A struct in the file, read by field name.
#[derive(Clone, Copy)]
struct View<'f, 'a> {
    file: &'f File<'a>,
    dna: &'f Dna,
    sdna: usize,
    data: &'a [u8],
    /// The block index pointers here are resolved from.
    at: usize,
}

impl<'f, 'a> View<'f, 'a> {
    /// The first of `names` that this version of the struct has (Blender renames fields).
    fn field(&self, names: &[&str]) -> Option<Field> {
        names.iter().find_map(|n| self.dna.field(self.sdna, n))
    }

    fn int(&self, names: &[&str]) -> Option<i64> {
        let f = self.field(names)?;
        let h = &self.file.header;
        match f.size {
            1 => self.data.get(f.offset).map(|&v| v as i64),
            2 => h.u16_at(self.data, f.offset).map(|v| v as i16 as i64),
            4 => h.u32_at(self.data, f.offset).map(|v| v as i32 as i64),
            _ => None,
        }
    }

    fn float(&self, name: &str) -> Option<f32> {
        let f = self.field(&[name]).filter(|f| f.size == 4 && !f.pointer)?;
        self.file.header.f32_at(self.data, f.offset)
    }

    /// The non-null pointer in one of `names`.
    fn pointer(&self, names: &[&str]) -> Option<u64> {
        names.iter().filter_map(|n| self.dna.field(self.sdna, n)).find_map(|f| self.file.pointer(self.data, &f).filter(|&p| p != 0))
    }

    fn target(&self, names: &[&str]) -> Option<&'f Block> {
        self.file.block_at(self.at, self.pointer(names)?)
    }

    /// A struct embedded in this one.
    fn nested(&self, names: &[&str]) -> Option<View<'f, 'a>> {
        let f = self.field(names).filter(|f| !f.pointer)?;
        let sdna = self.dna.struct_of_type(f.ty)?;
        Some(View { sdna, data: self.data.get(f.offset..f.offset.checked_add(f.size)?)?, ..*self })
    }

    /// The structs in `block`, as its own SDNA says they are.
    fn array(&self, block: &'f Block) -> impl Iterator<Item = View<'f, 'a>> + use<'f, 'a> {
        let (file, dna, sdna, at) = (self.file, self.dna, block.sdna, self.at);
        let data = file.data(block);
        let size = dna.struct_size(sdna).filter(|&s| s > 0).unwrap_or(usize::MAX);
        data.chunks_exact(size.min(data.len().max(1) + 1)).map(move |data| View { file, dna, sdna, data, at })
    }

    /// The data of the custom-data layer called one of `names`, in the `CustomData`
    /// embedded here under one of `fields`.
    fn layer(&self, fields: &[&str], names: &[&str]) -> Option<&'f Block> {
        let custom = self.nested(fields)?;
        let layers = custom.target(&["layers"])?;
        let name = custom.array(layers).find(|l| {
            l.field(&["name"]).and_then(|f| text(l.data, &f)).is_some_and(|n| names.contains(&n.as_str()))
        })?;
        name.target(&["data"])
    }

    /// An attribute's array in the 5.0 `attribute_storage`.
    fn stored(&self, name: &str) -> Option<&'f Block> {
        const ARRAY: i64 = 0;
        let storage = self.nested(&["attribute_storage"])?;
        let list = storage.target(&["dna_attributes"])?;
        let file = self.file;
        let attribute = storage.array(list).find(|a| a.target(&["name"]).is_some_and(|n| file.data(n).split(|&c| c == 0).next() == Some(name.as_bytes())))?;
        if attribute.int(&["storage_type"])? != ARRAY {
            return None;
        }
        let array = attribute.target(&["data"])?;
        storage.array(array).next()?.target(&["data"])
    }

    /// A mesh attribute's array, in `attribute_storage` or else in a custom-data layer.
    fn attribute(&self, name: &str, layers: &[&str]) -> Option<&'f Block> {
        self.stored(name).or_else(|| self.layer(layers, &[name]))
    }
}

fn view_of<'f, 'a>(file: &'f File<'a>, dna: &'f Dna, b: &Block) -> View<'f, 'a> {
    let at = file.blocks.partition_point(|x| x.data.start < b.data.start);
    View { file, dna, sdna: b.sdna, data: file.data(b), at }
}

fn block_kind(b: &Block, code: &[u8; 2]) -> bool {
    b.code[..2] == *code && b.code[2..] == [0, 0]
}

/// Field `field` of each of `count` structs in `block`, by `read`.
fn column<T>(v: &View<'_, '_>, block: &Block, field: &[&str], count: usize, read: impl Fn(&View<'_, '_>, Field) -> Option<T>) -> Option<Vec<T>> {
    let mut out = Vec::with_capacity(count.min(block.data.len()));
    let mut items = v.array(block);
    let first = items.next()?;
    let f = first.field(field)?;
    out.push(read(&first, f)?);
    for item in items.take(count.saturating_sub(1)) {
        out.push(read(&item, f)?);
    }
    (out.len() == count).then_some(out)
}

fn ints(file: &File<'_>, block: &Block, count: usize) -> Option<Vec<u32>> {
    let d = file.data(block);
    (d.len() / 4 >= count).then(|| (0..count).filter_map(|i| file.header.u32_at(d, i * 4)).collect())
}

fn positions(file: &File<'_>, me: &View<'_, '_>, count: usize) -> Option<Vec<[f32; 3]>> {
    let h = &file.header;
    if let Some(layer) = me.attribute("position", &["vdata", "vert_data"]) {
        let d = file.data(layer);
        if d.len() / 12 >= count {
            return (0..count).map(|i| Some([h.f32_at(d, i * 12)?, h.f32_at(d, i * 12 + 4)?, h.f32_at(d, i * 12 + 8)?])).collect();
        }
    }
    let verts = me.target(&["mvert"])?;
    column(me, verts, &["co"], count, |v, f| Some([h.f32_at(v.data, f.offset)?, h.f32_at(v.data, f.offset + 4)?, h.f32_at(v.data, f.offset + 8)?]))
}

/// (face sizes, corners, material slot per face) of a mesh.
fn faces(file: &File<'_>, me: &View<'_, '_>) -> Option<(Vec<u32>, Vec<u32>, Vec<u16>)> {
    let polys = me.int(&["totpoly", "faces_num"]).unwrap_or(0).max(0) as usize;
    let loops = me.int(&["totloop", "corners_num"]).unwrap_or(0).max(0) as usize;
    if polys > 0 {
        let corners = match me.attribute(".corner_vert", &["ldata", "corner_data"]) {
            Some(layer) => ints(file, layer, loops)?,
            None => column(me, me.target(&["mloop"])?, &["v"], loops, |l, f| file.header.u32_at(l.data, f.offset))?,
        };
        let size = |start: i64, end: i64| u32::try_from(end.checked_sub(start)?).ok().filter(|_| start >= 0 && end as usize <= loops);
        if let Some(offsets) = me.target(&["poly_offset_indices", "face_offset_indices"]) {
            let o = ints(file, offsets, polys + 1)?;
            let sizes = o.windows(2).map(|w| size(w[0] as i32 as i64, w[1] as i32 as i64)).collect::<Option<Vec<_>>>()?;
            let slots = me
                .attribute("material_index", &["pdata", "face_data"])
                .and_then(|l| ints(file, l, polys))
                .map(|v| v.into_iter().map(|s| s.min(u16::MAX as u32) as u16).collect())
                .unwrap_or_default();
            return Some((sizes, corners, slots));
        }
        let block = me.target(&["mpoly"])?;
        // Counts are the file's word; a block can't hold more structs than bytes.
        let room = polys.min(block.data.len());
        let mut sizes = Vec::with_capacity(room);
        let mut slots = Vec::with_capacity(room);
        let mut ordered = Vec::with_capacity(loops.min(corners.len()));
        for p in me.array(block).take(polys) {
            let (start, n) = (p.int(&["loopstart"])?, p.int(&["totloop"])?);
            let n32 = size(start, start.checked_add(n)?)?;
            sizes.push(n32);
            ordered.extend_from_slice(corners.get(start as usize..(start + n) as usize)?);
            slots.push(p.int(&["mat_nr"]).unwrap_or(0).clamp(0, u16::MAX as i64) as u16);
        }
        return (sizes.len() == polys).then_some((sizes, ordered, slots));
    }
    // The oldest layout: triangles and quads, the fourth corner zero on a triangle.
    let count = me.int(&["totface", "totface_legacy"]).unwrap_or(0).max(0) as usize;
    if count == 0 {
        // Vertices with no faces.
        return Some(Default::default());
    }
    let block = me.target(&["mface"])?;
    let room = count.min(block.data.len());
    let (mut sizes, mut corners, mut slots) = (Vec::with_capacity(room), Vec::with_capacity(room * 4), Vec::with_capacity(room));
    for f in me.array(block).take(count) {
        let v = [f.int(&["v1"])?, f.int(&["v2"])?, f.int(&["v3"])?, f.int(&["v4"])?];
        let n = if v[3] == 0 { 3 } else { 4 };
        sizes.push(n as u32);
        corners.extend(v[..n].iter().map(|&c| c as u32));
        slots.push(f.int(&["mat_nr"]).unwrap_or(0).clamp(0, u16::MAX as i64) as u16);
    }
    (sizes.len() == count).then_some((sizes, corners, slots))
}

fn read_mesh(file: &File<'_>, me: &View<'_, '_>, name: String, materials: &HashMap<u64, usize>) -> Result<Mesh, &'static str> {
    let count = me.int(&["totvert", "verts_num"]).unwrap_or(0).max(0) as usize;
    if count == 0 {
        return Ok(Mesh { name, ..Mesh::default() });
    }
    let positions = positions(file, me, count).ok_or("no positions it can read")?;
    let (face_sizes, corners, face_slots) = faces(file, me).ok_or("no faces it can read")?;
    if corners.iter().any(|&c| c as usize >= count) {
        return Err("a face names a vertex that doesn't exist");
    }
    if face_sizes.iter().map(|&n| n as u64).sum::<u64>() > corners.len() as u64 {
        return Err("its faces need more corners than it has");
    }
    // `mat` is an array of pointers to materials.
    let mut slots = Vec::new();
    if let Some(block) = me.target(&["mat"]) {
        let d = file.data(block);
        let p = file.header.pointer;
        for i in 0..me.int(&["totcol"]).unwrap_or(0).clamp(0, 1 << 15) as usize {
            let at = i * p;
            let address = if p == 8 { file.header.u64_at(d, at) } else { file.header.u32_at(d, at).map(u64::from) };
            slots.push(address.and_then(|a| materials.get(&a).copied()));
        }
    }
    Ok(Mesh { name, positions, face_sizes, corners, face_slots, slots })
}

/// Reads what a file holds to draw. A mesh that can't be read is skipped with a warning.
/// Modifiers aren't applied: meshes are as saved, before them.
pub fn scene(file: &File<'_>) -> Scene {
    let mut out = Scene::default();
    let Some(dna) = file.dna.as_ref() else {
        out.warnings.push(format!("no readable SDNA ({})", file.dna_error.as_deref().unwrap_or("unknown")));
        return out;
    };
    let view = |b: &Block| view_of(file, dna, b);
    let mut materials = HashMap::new();
    for b in file.blocks.iter().filter(|b| block_kind(b, b"MA")) {
        let m = view(b);
        materials.insert(b.old, out.materials.len());
        let channel = |n| m.float(n).filter(|v| v.is_finite()).unwrap_or(0.8);
        out.materials.push(Material { name: file.id_name(b).unwrap_or_default(), color: [channel("r"), channel("g"), channel("b")] });
    }
    let mut meshes = HashMap::new();
    for b in file.blocks.iter().filter(|b| block_kind(b, b"ME")) {
        let name = file.id_name(b).unwrap_or_default();
        match read_mesh(file, &view(b), name.clone(), &materials) {
            Ok(mesh) => {
                meshes.insert(b.old, out.meshes.len());
                out.meshes.push(mesh);
            }
            Err(why) => out.warnings.push(format!("mesh {name}: {why}")),
        }
    }
    let objects: HashMap<u64, &Block> = file.blocks.iter().filter(|b| block_kind(b, b"OB")).map(|b| (b.old, b)).collect();
    let mut modified = 0usize;
    for b in file.blocks.iter().filter(|b| block_kind(b, b"OB")) {
        let o = view(b);
        const MESH: i64 = 1;
        if o.int(&["type"]) != Some(MESH) || o.int(&["restrictflag"]).is_some_and(|f| f & 1 != 0) {
            continue;
        }
        let Some(&mesh) = o.pointer(&["data"]).and_then(|p| meshes.get(&p)) else { continue };
        if out.meshes[mesh].face_sizes.is_empty() {
            continue;
        }
        let name = file.id_name(b).unwrap_or_default();
        if o.nested(&["modifiers"]).is_some_and(|l| l.pointer(&["first"]).is_some()) {
            modified += 1;
        }
        match world(file, dna, &objects, b, 0) {
            Some(matrix) => out.objects.push(Object { name, mesh, matrix }),
            None => out.warnings.push(format!("object {name}: no transform it can read")),
        }
    }
    if modified > 0 {
        out.warnings.push(format!("{modified} object(s) have modifiers, which aren't applied here"));
    }
    out
}

type Mat = [f32; 16];
type Rot = [[f32; 3]; 3];

fn mul(a: &Mat, b: &Mat) -> Mat {
    let mut out = [0.0; 16];
    for c in 0..4 {
        for r in 0..4 {
            out[c * 4 + r] = (0..4).map(|k| a[k * 4 + r] * b[c * 4 + k]).sum();
        }
    }
    out
}

/// Columns of a product of 3×3 rotations.
fn rot_mul(a: &Rot, b: &Rot) -> Rot {
    let mut out = [[0.0; 3]; 3];
    for c in 0..3 {
        for r in 0..3 {
            out[c][r] = (0..3).map(|k| a[k][r] * b[c][k]).sum();
        }
    }
    out
}

fn around(axis: usize, angle: f32) -> Rot {
    let (s, c) = angle.sin_cos();
    match axis {
        0 => [[1.0, 0.0, 0.0], [0.0, c, s], [0.0, -s, c]],
        1 => [[c, 0.0, -s], [0.0, 1.0, 0.0], [s, 0.0, c]],
        _ => [[c, s, 0.0], [-s, c, 0.0], [0.0, 0.0, 1.0]],
    }
}

/// Euler angles in Blender's `rotmode` order: its first letter turns first.
fn euler(e: [f32; 3], mode: i64) -> Rot {
    let order = match mode {
        2 => [0, 2, 1],
        3 => [1, 0, 2],
        4 => [1, 2, 0],
        5 => [2, 0, 1],
        6 => [2, 1, 0],
        _ => [0, 1, 2],
    };
    let turn = |i: usize| around(order[i], e[order[i]]);
    rot_mul(&turn(2), &rot_mul(&turn(1), &turn(0)))
}

fn quaternion([w, x, y, z]: [f32; 4]) -> Rot {
    let n = (w * w + x * x + y * y + z * z).sqrt();
    if n < 1e-9 || !n.is_finite() {
        return around(0, 0.0);
    }
    let (w, x, y, z) = (w / n, x / n, y / n, z / n);
    [
        [1.0 - 2.0 * (y * y + z * z), 2.0 * (x * y + w * z), 2.0 * (x * z - w * y)],
        [2.0 * (x * y - w * z), 1.0 - 2.0 * (x * x + z * z), 2.0 * (y * z + w * x)],
        [2.0 * (x * z + w * y), 2.0 * (y * z - w * x), 1.0 - 2.0 * (x * x + y * y)],
    ]
}

fn axis_angle([x, y, z]: [f32; 3], angle: f32) -> Rot {
    let n = (x * x + y * y + z * z).sqrt();
    if n < 1e-9 || !n.is_finite() {
        return around(0, 0.0);
    }
    let (half_s, half_c) = (angle / 2.0).sin_cos();
    quaternion([half_c, half_s * x / n, half_s * y / n, half_s * z / n])
}

fn floats<const N: usize>(o: &View<'_, '_>, name: &str) -> Option<[f32; N]> {
    let f = o.field(&[name]).filter(|f| f.size == N * 4 && !f.pointer)?;
    let mut out = [0.0; N];
    for (i, v) in out.iter_mut().enumerate() {
        *v = o.file.header.f32_at(o.data, f.offset + i * 4)?;
    }
    Some(out)
}

/// An object's own transform, from its location, rotation and scale with their deltas.
fn local(o: &View<'_, '_>) -> Option<Mat> {
    let v = |name: &str, default: [f32; 3]| floats::<3>(o, name).unwrap_or(default);
    let mode = o.int(&["rotmode"]).unwrap_or(0);
    const AXIS_ANGLE: i64 = -1;
    let (rotation, delta) = match mode {
        1..=6 => (euler(floats(o, "rot")?, mode), euler(v("drot", [0.0; 3]), mode)),
        AXIS_ANGLE => (
            axis_angle(floats(o, "rotAxis")?, o.float("rotAngle").unwrap_or(0.0)),
            axis_angle(v("drotAxis", [0.0, 1.0, 0.0]), o.float("drotAngle").unwrap_or(0.0)),
        ),
        _ => (quaternion(floats(o, "quat")?), quaternion(floats::<4>(o, "dquat").unwrap_or([1.0, 0.0, 0.0, 0.0]))),
    };
    let r = rot_mul(&delta, &rotation);
    let (size, dscale) = (v("size", [1.0; 3]), v("dscale", [1.0; 3]));
    let (loc, dloc) = (floats::<3>(o, "loc")?, v("dloc", [0.0; 3]));
    let mut m = [0.0; 16];
    for c in 0..3 {
        for row in 0..3 {
            m[c * 4 + row] = r[c][row] * size[c] * dscale[c];
        }
        m[12 + c] = loc[c] + dloc[c];
    }
    m[15] = 1.0;
    Some(m)
}

/// An object's world matrix: as saved (`obmat`, until about 4.2), or worked out from its
/// parents. Constraints, drivers and bone parents aren't evaluated.
fn world(file: &File<'_>, dna: &Dna, objects: &HashMap<u64, &Block>, b: &Block, depth: usize) -> Option<Mat> {
    let o = view_of(file, dna, b);
    if let Some(m) = floats::<16>(&o, "obmat").or_else(|| floats::<16>(&o, "object_to_world")) {
        return Some(m);
    }
    let own = local(&o)?;
    if depth >= 64 {
        return Some(own);
    }
    let Some(parent) = o.pointer(&["parent"]).and_then(|p| objects.get(&p)) else { return Some(own) };
    let above = world(file, dna, objects, parent, depth + 1)?;
    let inverse = floats::<16>(&o, "parentinv").unwrap_or([1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0]);
    Some(mul(&above, &mul(&inverse, &own)))
}
