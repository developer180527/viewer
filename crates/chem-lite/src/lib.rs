//! Molecules and crystals as atoms and bonds, from the files chemists keep them in:
//!
//! | Format | Read | Bonds |
//! |---|---|---|
//! | XYZ, extended XYZ (`.xyz`) | every frame; a `Lattice="…"` cell | found |
//! | MOL and SDF (`.mol`, `.sdf`, `.sd`), V2000 and V3000 | every record, its data fields | the file's |
//! | SMILES (`.smi`, `.smiles`) | every line; hydrogens added | the file's; atoms laid out |
//! | VASP POSCAR and CONTCAR | the cell, direct or Cartesian | found |
//! | Turbomole `coord` | the `$coord` block, in bohr | found |
//! | Chemical JSON (`.cjson`) | atoms, cell | the file's, or found |
//! | QCSchema (`.qcjson`, `.json`) | molecule, in bohr | the file's, or found |
//! | Molden (`.molden`, `.mold`) | `[Atoms]` or `[GEOMETRIES]`; orbitals and frequencies counted | found |
//! | PDB (`.pdb`, `.ent`) | the first model; `CONECT` | the file's and found |
//! | CIF and mmCIF (`.cif`, `.mmcif`) | `_atom_site`: Cartesian, or fractional in the cell | found |
//!
//! "Found" is by distance: two atoms closer than their covalent radii together, and a
//! little more, are bonded ([`find_bonds`]). SMILES has no coordinates: its atoms are at
//! the origin, [`Molecule::laid_out`] says so, and [`layout`] gives them a plausible
//! shape when they're to be drawn.

use std::fmt;

pub mod elements;

mod bonds;
mod cif;
mod json;
mod layout;
mod mol;
mod pdb;
mod poscar;
mod smiles;
mod text;
mod xyz;

pub use bonds::find_bonds;
pub use layout::layout;

/// An atom: its element (`0` when the file names none we know), the label the file
/// gives it, and where it is, in Å.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Atom {
    pub element: u8,
    pub label: String,
    pub position: [f64; 3],
    /// Formal charge, where the file gives one.
    pub charge: i8,
}

impl Atom {
    pub fn new(element: u8, label: impl Into<String>, position: [f64; 3]) -> Self {
        Self { element, label: label.into(), position, charge: 0 }
    }
}

/// A bond between two atoms, by index. `order` 1 to 3; 4 aromatic; 0 unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Bond {
    pub a: u32,
    pub b: u32,
    pub order: u8,
}

impl Bond {
    pub fn new(a: u32, b: u32, order: u8) -> Self {
        Self { a, b, order }
    }
}

/// One molecule, crystal or frame.
#[derive(Debug, Clone, PartialEq, Default)]
#[non_exhaustive]
pub struct Molecule {
    pub name: String,
    pub atoms: Vec<Atom>,
    pub bonds: Vec<Bond>,
    /// A crystal's cell: its three edge vectors, in Å.
    pub cell: Option<[[f64; 3]; 3]>,
    /// Bonds were found by distance, not given.
    pub bonds_found: bool,
    /// The file gives no positions: they're all zero until [`layout`] makes some up.
    pub laid_out: bool,
    /// What the file says about it besides atoms: an SDF record's fields, an XYZ
    /// comment, an energy.
    pub properties: Vec<(String, String)>,
}

/// What a file held: one or more molecules (an SDF's records, an XYZ's frames), and
/// what was counted without being read.
#[derive(Debug, Clone, PartialEq, Default)]
#[non_exhaustive]
pub struct File {
    pub molecules: Vec<Molecule>,
    pub notes: Vec<(String, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Format {
    Xyz,
    Mol,
    Smiles,
    Poscar,
    Turbomole,
    ChemicalJson,
    QcSchema,
    Molden,
    Pdb,
    Cif,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// Not this format.
    NotThisFormat,
    Damaged(String),
    TooLarge { what: &'static str, value: u64, max: u64 },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotThisFormat => write!(f, "not this format"),
            Error::Damaged(why) => write!(f, "damaged: {why}"),
            Error::TooLarge { what, value, max } => write!(f, "{what}: {value}, more than the {max} allowed"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn damaged(why: impl Into<String>) -> Error {
    Error::Damaged(why.into())
}

#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct Limits {
    /// Atoms in all, every molecule and frame counted.
    pub atoms: u64,
    /// Molecules (records, frames).
    pub molecules: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self { atoms: 1 << 24, molecules: 1 << 20 }
    }
}

impl Limits {
    pub fn with_atoms(mut self, max: u64) -> Self {
        self.atoms = max;
        self
    }

    pub fn with_molecules(mut self, max: u64) -> Self {
        self.molecules = max;
        self
    }
}

/// Counts what's read against the limits.
pub(crate) struct Budget {
    limits: Limits,
    atoms: u64,
    molecules: u64,
}

impl Budget {
    pub fn new(limits: &Limits) -> Self {
        Self { limits: *limits, atoms: 0, molecules: 0 }
    }

    pub fn atoms(&mut self, n: u64) -> Result<()> {
        self.atoms = self.atoms.saturating_add(n);
        if self.atoms > self.limits.atoms {
            return Err(Error::TooLarge { what: "atoms", value: self.atoms, max: self.limits.atoms });
        }
        Ok(())
    }

    pub fn molecule(&mut self) -> Result<()> {
        self.molecules += 1;
        if self.molecules > self.limits.molecules {
            return Err(Error::TooLarge { what: "molecules", value: self.molecules, max: self.limits.molecules });
        }
        Ok(())
    }
}

/// Which format these first bytes are, by content alone, where content can tell; the
/// rest only by name ([`by_extension`]).
pub fn detect(header: &[u8]) -> Option<Format> {
    let text = String::from_utf8_lossy(&header[..header.len().min(4096)]);
    let t = text.trim_start_matches('\u{feff}').trim_start();
    if t.starts_with("[Molden Format]") || t.to_ascii_lowercase().starts_with("[molden format]") {
        return Some(Format::Molden);
    }
    if t.starts_with("$coord") {
        return Some(Format::Turbomole);
    }
    if t.starts_with('{') {
        if t.contains("\"chemicalJson\"") || t.contains("\"chemical json\"") {
            return Some(Format::ChemicalJson);
        }
        if t.contains("\"qcschema_molecule\"") || t.contains("\"qc_schema_") || t.contains("\"qcschema_") {
            return Some(Format::QcSchema);
        }
    }
    if mol::looks_like(&text) {
        return Some(Format::Mol);
    }
    if poscar::looks_like(&text) {
        return Some(Format::Poscar);
    }
    if xyz::looks_like(&text) {
        return Some(Format::Xyz);
    }
    None
}

/// The format a file's name gives it, if any.
pub fn by_extension(name: &str) -> Option<Format> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name).to_ascii_lowercase();
    if matches!(base.as_str(), "poscar" | "contcar" | "chgcar" | "locpot") || base.starts_with("poscar") || base.starts_with("contcar") || base.ends_with(".vasp") || base.ends_with(".poscar") {
        return Some(Format::Poscar);
    }
    if base == "coord" || base.ends_with(".coord") || base.ends_with(".tmol") {
        return Some(Format::Turbomole);
    }
    Some(match base.rsplit_once('.')?.1 {
        "xyz" | "extxyz" => Format::Xyz,
        "mol" | "sdf" | "sd" | "mdl" => Format::Mol,
        "smi" | "smiles" | "ism" | "can" => Format::Smiles,
        "cjson" => Format::ChemicalJson,
        "qcjson" => Format::QcSchema,
        "molden" | "mold" => Format::Molden,
        "pdb" | "ent" => Format::Pdb,
        "cif" | "mmcif" | "mcif" => Format::Cif,
        _ => return None,
    })
}

/// Reads a file as `format`.
pub fn read(bytes: &[u8], format: Format, limits: &Limits) -> Result<File> {
    let text = std::str::from_utf8(bytes).map(std::borrow::Cow::Borrowed).unwrap_or_else(|_| String::from_utf8_lossy(bytes));
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let mut budget = Budget::new(limits);
    let mut file = match format {
        Format::Xyz => xyz::read(text, &mut budget)?,
        Format::Mol => mol::read(text, &mut budget)?,
        Format::Smiles => smiles::read_file(text, &mut budget)?,
        Format::Poscar => poscar::read(text, &mut budget)?,
        Format::Turbomole => text::turbomole(text, &mut budget)?,
        Format::ChemicalJson => json::chemical_json(text, &mut budget)?,
        Format::QcSchema => json::qcschema(text, &mut budget)?,
        Format::Molden => text::molden(text, &mut budget)?,
        Format::Pdb => pdb::read(text, &mut budget)?,
        Format::Cif => cif::read(text, &mut budget)?,
    };
    if file.molecules.is_empty() {
        return Err(damaged("no atoms"));
    }
    // A molecule without coordinates keeps them at zero: laying one out costs (a few
    // milliseconds a molecule, minutes for a file of thousands), and only a drawing of it
    // needs it, which calls [`layout`].
    for m in &mut file.molecules {
        if m.bonds.is_empty() && !m.laid_out {
            m.bonds = find_bonds(&m.atoms);
            m.bonds_found = !m.bonds.is_empty();
        }
    }
    Ok(file)
}

/// Numbers as Fortran writes them too: `1.5D-03`.
pub(crate) fn number(s: &str) -> Option<f64> {
    let s = s.trim();
    s.parse::<f64>().ok().or_else(|| s.replace(['D', 'd'], "E").parse().ok()).filter(|v| v.is_finite())
}

/// Bohr to Å.
pub(crate) const BOHR: f64 = 0.529_177_210_903;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_by_content_and_by_name() {
        assert_eq!(detect(b"3
water
O 0 0 0
"), Some(Format::Xyz));
        assert_eq!(detect(b"\xef\xbb\xbf  [Molden Format]\n"), Some(Format::Molden));
        assert_eq!(detect(b"[molden format]\n"), Some(Format::Molden));
        assert_eq!(detect(b"name\n\n\n  0  0  0  0  0  0  0  0  0  0999 V2000\n"), Some(Format::Mol));
        assert_eq!(detect(br#"{"schema_name": "qcschema_input"}"#), Some(Format::QcSchema));
        assert_eq!(detect(b"plain text\n"), None);
        assert_eq!(detect(b"{\"a\": 1}"), None);
        for (name, format) in [("a.xyz", Format::Xyz), ("b.SDF", Format::Mol), ("c.sd", Format::Mol), ("d.smi", Format::Smiles), ("e.cjson", Format::ChemicalJson), ("f.qcjson", Format::QcSchema), ("g.mold", Format::Molden), ("h.molden", Format::Molden), ("i.pdb", Format::Pdb), ("j.mmcif", Format::Cif), ("dir/coord", Format::Turbomole), ("x.coord", Format::Turbomole)] {
            assert_eq!(by_extension(name), Some(format), "{name}");
        }
        assert_eq!(by_extension("a.txt"), None);
        assert_eq!(by_extension("noextension"), None);
    }

    #[test]
    fn files_and_errors() {
        // Text that isn't UTF-8 is read all the same.
        let f = read(b"1\n\xff\nC 0 0 0\n", Format::Xyz, &Limits::default()).unwrap();
        assert_eq!(f.molecules[0].atoms.len(), 1);
        assert_eq!(read(b"", Format::Smiles, &Limits::default()), Err(damaged("no atoms")));
        assert_eq!((number("1.5D-03"), number("2d1"), number("inf"), number("x")), (Some(0.0015), Some(20.0), None, None));
        assert_eq!(Error::NotThisFormat.to_string(), "not this format");
        assert_eq!(damaged("x").to_string(), "damaged: x");
        assert_eq!(Error::TooLarge { what: "atoms", value: 2, max: 1 }.to_string(), "atoms: 2, more than the 1 allowed");
    }
}
