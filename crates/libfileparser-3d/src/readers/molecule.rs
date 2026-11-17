//! Molecules and crystals in 3D, ball and stick: a sphere per atom in its element's
//! colour, a stick per bond (two or three for double and triple bonds), each half the
//! colour of its atom's end, and a crystal's cell as lines. The files are read by
//! `chem-lite`; `-data` owns them, as tables of atoms (docs/design/libfileparser.md
//! §2.1), and this is the Scene the front door adds to them.

use std::f32::consts::TAU;

use chem_lite::{Format, Molecule, elements};
use libfileparser_core::{Category, Confidence, Detection, Document, FileSource, Limit, Model, ParseContext, ParseError, Reader, ReaderInfo, Result};
use scene_data::glam::{Mat4, Vec3, Vec4};
use scene_data::{Indices, MeshData, Node, Topology};

use crate::build::{self, SceneDocument};

/// The extensions `-data` owns that this reads too; and the whole names some are known
/// by (`POSCAR`, `coord`).
pub const EXTENSIONS: &[&str] = &[
    "xyz", "extxyz", "mol", "sdf", "sd", "mdl", "smi", "smiles", "ism", "can", "poscar", "contcar", "vasp", "coord", "tmol", "cjson", "qcjson", "molden", "mold", "pdb", "ent", "cif", "mmcif",
    "mcif",
];

static MOLECULE: ReaderInfo = ReaderInfo::new("molecule", "Molecule", Category::ThreeD).also_reads(EXTENSIONS, Model::Scene);

pub struct MoleculeReader;

/// The format a file is, by its name first (most of these have nothing in them that
/// says), then its content.
fn format(header: &[u8], name: &str) -> Option<Format> {
    chem_lite::by_extension(name).or_else(|| chem_lite::detect(header))
}

/// How finely spheres and sticks are drawn: fewer triangles the more there are.
struct Detail {
    sphere: u32,
    sides: usize,
    multiple: bool,
}

fn detail(atoms: usize) -> Detail {
    match atoms {
        0..=2_000 => Detail { sphere: 2, sides: 12, multiple: true },
        2_001..=20_000 => Detail { sphere: 1, sides: 8, multiple: true },
        _ => Detail { sphere: 0, sides: 6, multiple: false },
    }
}

/// A unit sphere: an icosahedron, its triangles split `level` times.
fn icosphere(level: u32) -> (Vec<Vec3>, Vec<u32>) {
    let t = (1.0 + 5f32.sqrt()) / 2.0;
    let mut v: Vec<Vec3> = [[-1.0, t, 0.0], [1.0, t, 0.0], [-1.0, -t, 0.0], [1.0, -t, 0.0], [0.0, -1.0, t], [0.0, 1.0, t], [0.0, -1.0, -t], [0.0, 1.0, -t], [t, 0.0, -1.0], [t, 0.0, 1.0], [-t, 0.0, -1.0], [-t, 0.0, 1.0]]
        .iter()
        .map(|p| Vec3::from_array(*p).normalize())
        .collect();
    let mut f: Vec<[u32; 3]> = vec![
        [0, 11, 5], [0, 5, 1], [0, 1, 7], [0, 7, 10], [0, 10, 11], [1, 5, 9], [5, 11, 4], [11, 10, 2], [10, 7, 6], [7, 1, 8],
        [3, 9, 4], [3, 4, 2], [3, 2, 6], [3, 6, 8], [3, 8, 9], [4, 9, 5], [2, 4, 11], [6, 2, 10], [8, 6, 7], [9, 8, 1],
    ];
    for _ in 0..level {
        let mut mid = std::collections::HashMap::new();
        let mut half = |a: u32, b: u32, v: &mut Vec<Vec3>| *mid.entry((a.min(b), a.max(b))).or_insert_with(|| {
            v.push(((v[a as usize] + v[b as usize]) / 2.0).normalize());
            v.len() as u32 - 1
        });
        f = f.iter().flat_map(|&[a, b, c]| {
            let (ab, bc, ca) = (half(a, b, &mut v), half(b, c, &mut v), half(c, a, &mut v));
            [[a, ab, ca], [b, bc, ab], [c, ca, bc], [ab, bc, ca]]
        }).collect();
    }
    (v, f.into_iter().flatten().collect())
}

/// Triangles with normals and colours, built up piece by piece.
#[derive(Default)]
struct Mesh {
    positions: Vec<Vec3>,
    normals: Vec<Vec3>,
    colours: Vec<Vec4>,
    indices: Vec<u32>,
}

impl Mesh {
    fn sphere(&mut self, unit: &(Vec<Vec3>, Vec<u32>), centre: Vec3, radius: f32, colour: Vec4) {
        let base = self.positions.len() as u32;
        for n in &unit.0 {
            self.positions.push(centre + *n * radius);
            self.normals.push(*n);
            self.colours.push(colour);
        }
        self.indices.extend(unit.1.iter().map(|i| base + i));
    }

    /// An open tube from `a` to `b`.
    fn tube(&mut self, a: Vec3, b: Vec3, radius: f32, sides: usize, colour: Vec4) {
        let axis = (b - a).normalize_or_zero();
        if axis == Vec3::ZERO {
            return;
        }
        let (u, v) = axis.any_orthonormal_pair();
        let base = self.positions.len() as u32;
        for k in 0..sides {
            let t = k as f32 / sides as f32 * TAU;
            let n = u * t.cos() + v * t.sin();
            for end in [a, b] {
                self.positions.push(end + n * radius);
                self.normals.push(n);
                self.colours.push(colour);
            }
        }
        for k in 0..sides as u32 {
            let (i, j) = (base + 2 * k, base + 2 * ((k + 1) % sides as u32));
            self.indices.extend([i, j, i + 1, j, j + 1, i + 1]);
        }
    }

    fn into_data(self) -> Option<MeshData> {
        if self.positions.is_empty() {
            return None;
        }
        let mut m = MeshData::new(self.positions).with_indices(Indices::U32(self.indices.into()));
        m.normals = Some(self.normals.into());
        m.colors = Some(self.colours.into());
        Some(m)
    }
}

fn colour(z: u8) -> Vec4 {
    let [r, g, b] = elements::colour(z);
    Vec4::new(r, g, b, 1.0)
}

/// How large an atom's ball is: a quarter of its covalent radius over a small floor, so
/// hydrogens show and metals don't swallow their neighbours.
fn ball(z: u8) -> f32 {
    (0.2 + 0.25 * elements::covalent_radius(z)) as f32
}

const STICK: f32 = 0.1;

/// The meshes for a molecule: its atoms, its bonds, and its cell's edges.
fn meshes(m: &Molecule) -> (Option<MeshData>, Option<MeshData>, Option<MeshData>) {
    let d = detail(m.atoms.len());
    let unit = icosphere(d.sphere);
    let at = |i: usize| Vec3::from_array(m.atoms[i].position.map(|c| c as f32));
    let (mut atoms, mut bonds) = (Mesh::default(), Mesh::default());
    for a in &m.atoms {
        let p = Vec3::from_array(a.position.map(|c| c as f32));
        if p.is_finite() {
            atoms.sphere(&unit, p, ball(a.element), colour(a.element));
        }
    }
    for b in &m.bonds {
        let (i, j) = (b.a as usize, b.b as usize);
        if i >= m.atoms.len() || j >= m.atoms.len() || !at(i).is_finite() || !at(j).is_finite() {
            continue;
        }
        let (pa, pb) = (at(i), at(j));
        let mid = (pa + pb) / 2.0;
        let axis = (pb - pa).normalize_or_zero();
        // Sticks side by side for a double or triple bond, in the plane of a neighbour
        // when there is one, so a double bond in a ring lies in the ring.
        let neighbour = m.bonds.iter().find_map(|o| match (o.a as usize, o.b as usize) {
            (x, y) if x == i && y != j => Some(y),
            (y, x) if x == i && y != j => Some(y),
            _ => None,
        });
        let side = neighbour.map(|n| at(n) - pa).map(|v| (v - axis * v.dot(axis)).normalize_or_zero()).filter(|v| *v != Vec3::ZERO).unwrap_or_else(|| axis.any_orthonormal_vector());
        let sticks: Vec<(f32, f32)> = match (b.order, d.multiple) {
            (2, true) => vec![(-0.09, 0.07), (0.09, 0.07)],
            (3, true) => vec![(-0.13, 0.055), (0.0, 0.055), (0.13, 0.055)],
            (4, true) => vec![(0.0, STICK), (0.16, 0.04)],
            _ => vec![(0.0, STICK)],
        };
        for (offset, r) in sticks {
            let o = side * offset;
            bonds.tube(pa + o, mid + o, r, d.sides, colour(m.atoms[i].element));
            bonds.tube(mid + o, pb + o, r, d.sides, colour(m.atoms[j].element));
        }
    }
    let cell = m.cell.map(|c| {
        let [a, b, c] = c.map(|v| Vec3::from_array(v.map(|x| x as f32)));
        let corners = [Vec3::ZERO, a, b, c, a + b, a + c, b + c, a + b + c];
        let edges: [u32; 24] = [0, 1, 0, 2, 0, 3, 1, 4, 1, 5, 2, 4, 2, 6, 3, 5, 3, 6, 4, 7, 5, 7, 6, 7];
        let mut mesh = MeshData::new(corners.to_vec()).with_indices(Indices::U32(edges.to_vec().into()));
        mesh.topology = Topology::Lines;
        mesh.colors = Some(vec![Vec4::new(0.55, 0.55, 0.6, 1.0); 8].into());
        mesh
    });
    (atoms.into_data(), bonds.into_data(), cell)
}

impl Reader for MoleculeReader {
    fn info(&self) -> &ReaderInfo {
        &MOLECULE
    }

    /// By name or by content, never more than an extension's worth: `-data` owns these
    /// files, and a reader that only also reads them mustn't outrank it (§2.1, item 6).
    fn detect(&self, header: &[u8], name: &str) -> Detection {
        if format(header, name).is_some() { Detection::new(Confidence::Extension) } else { Detection::NONE }
    }

    fn open(&self, src: &FileSource, cx: &ParseContext) -> Result<Document> {
        cx.cancel.check()?;
        let format = format(src.header(), src.name()).ok_or(ParseError::NotThisFormat)?;
        // A sphere is a few hundred vertices: what geometry may hold bounds the atoms.
        let limits = chem_lite::Limits::default().with_atoms(cx.limits.geometry / 12).with_molecules(cx.limits.rows);
        let file = chem_lite::read(src.bytes(), format, &limits).map_err(|e| match e {
            chem_lite::Error::NotThisFormat => ParseError::NotThisFormat,
            chem_lite::Error::TooLarge { value, max, .. } => ParseError::too_large(Limit::Geometry, value, max),
            e => ParseError::corrupt(e),
        })?;
        let count = file.molecules.len();
        let mut molecule = file.molecules.into_iter().next().ok_or(ParseError::NotThisFormat)?;
        if molecule.laid_out {
            chem_lite::layout(&mut molecule);
        }
        cx.cancel.check()?;
        let (atoms, bonds, cell) = meshes(&molecule);
        let (scene, mut builder) = build::new_scene(cx);
        for mesh in [atoms, bonds, cell].into_iter().flatten() {
            let id = builder.add_mesh(mesh).map_err(|e| build::scene_error(&e))?;
            builder.add_node(Node::mesh(id, None, Mat4::IDENTITY, None)).map_err(|e| build::scene_error(&e))?;
        }
        builder.finish();
        let mut warnings = Vec::new();
        if count > 1 {
            warnings.push(format!("The first of {count} molecules is shown."));
        }
        if molecule.laid_out {
            warnings.push("The file has no coordinates: the atoms are laid out to show the bonds, not where they really are.".to_string());
        }
        let name = match format {
            Format::Xyz => "XYZ",
            Format::Mol => "MDL molfile",
            Format::Smiles => "SMILES",
            Format::Poscar => "VASP POSCAR",
            Format::Turbomole => "Turbomole coordinates",
            Format::ChemicalJson => "Chemical JSON",
            Format::QcSchema => "QCSchema",
            Format::Molden => "Molden",
            Format::Pdb => "Protein Data Bank",
            _ => "Crystallographic Information File",
        };
        let (atoms, bonds) = (molecule.atoms.len(), molecule.bonds.len());
        SceneDocument::new(self, src, format!("Molecule ({name})"), scene).warnings(warnings).facets(move |doc| doc.with_meta("Atoms", atoms).with_meta("Bonds", bonds)).build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chem_lite::{Atom, Bond};

    #[test]
    fn spheres_and_sticks() {
        for (level, vertices, triangles) in [(0, 12, 20), (1, 42, 80), (2, 162, 320)] {
            let (v, i) = icosphere(level);
            assert_eq!((v.len(), i.len() / 3), (vertices, triangles));
            assert!(v.iter().all(|p| (p.length() - 1.0).abs() < 1e-5));
        }
        let mut m = Molecule::default();
        m.atoms = vec![Atom::new(6, "C", [0.0; 3]), Atom::new(8, "O", [1.2, 0.0, 0.0]), Atom::new(1, "H", [-0.5, 0.9, 0.0])];
        m.bonds = vec![Bond::new(0, 1, 2), Bond::new(0, 2, 1), Bond::new(0, 9, 1)];
        m.cell = Some([[5.0, 0.0, 0.0], [0.0, 5.0, 0.0], [0.0, 0.0, 5.0]]);
        let (atoms, bonds, cell) = meshes(&m);
        let atoms = atoms.unwrap();
        assert_eq!(atoms.vertex_count(), 3 * 162);
        // A double bond is two sticks, each in two halves; a single one is one; one to
        // an atom that isn't there, none. Twelve sides, two rings of vertices.
        assert_eq!(bonds.unwrap().vertex_count(), (2 * 2 + 2) * 12 * 2);
        let cell = cell.unwrap();
        assert_eq!((cell.topology, cell.vertex_count()), (Topology::Lines, 8));
        // Oxygen's ball is red, and the first half of the C=O stick the carbon's grey.
        let red = atoms.colors.as_ref().unwrap().as_slice()[162];
        assert!(red.x > 0.9 && red.y < 0.1);
        assert!(ball(1) < ball(6) && ball(6) < ball(26));
    }

    #[test]
    fn big_molecules_are_drawn_more_coarsely() {
        assert_eq!((detail(10).sphere, detail(5_000).sphere, detail(50_000).sphere), (2, 1, 0));
        assert!(!detail(50_000).multiple);
        assert!(meshes(&Molecule::default()).0.is_none());
        let mut m = Molecule::default();
        m.atoms = vec![Atom::new(6, "C", [f64::NAN, 0.0, 0.0]), Atom::new(6, "C", [0.0; 3])];
        m.bonds = vec![Bond::new(0, 1, 1)];
        let (atoms, bonds, _) = meshes(&m);
        assert_eq!(atoms.unwrap().vertex_count(), 162);
        assert!(bonds.is_none());
    }
}
