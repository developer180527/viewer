//! Molecules and crystals (feature `science`), read by `chem-lite`: a table of atoms
//! (element, label, position in Å, charge), a table of bonds, and the file's own fields
//! (an SD record's data, an energy). `-3d` reads the same files as a ball-and-stick
//! Scene, which the front door adds after these.

use std::sync::Arc;

use chem_lite::{Format, elements};
use libfileparser_core::{Category, Cell, Content, Document, FileSource, FormatInfo, Limit, OwnedTable, ParseContext, ParseError, Result, Role, ValueKind, Warning};

use crate::lines::columns;

/// The formula in Hill order: carbon, hydrogen, then the rest alphabetically (all
/// alphabetically when there's no carbon).
pub(crate) fn formula(zs: impl Iterator<Item = u8>) -> String {
    let mut count = std::collections::BTreeMap::<&str, usize>::new();
    for z in zs {
        *count.entry(if z == 0 { "?" } else { elements::symbol(z) }).or_default() += 1;
    }
    let carbon = count.contains_key("C");
    let mut keys: Vec<&str> = count.keys().copied().collect();
    keys.sort_by_key(|k| (!(carbon && *k == "C"), !(carbon && *k == "H"), *k));
    keys.iter().map(|k| if count[k] == 1 { k.to_string() } else { format!("{k}{}", count[k]) }).collect()
}

fn open(src: &FileSource, cx: &ParseContext, format: Format, id: &'static str, name: &str, language: &str) -> Result<Document> {
    cx.cancel.check()?;
    let limits = chem_lite::Limits::default().with_atoms(cx.limits.rows).with_molecules(cx.limits.rows);
    let file = chem_lite::read(src.bytes(), format, &limits).map_err(|e| match e {
        chem_lite::Error::NotThisFormat => ParseError::NotThisFormat,
        chem_lite::Error::TooLarge { value, max, .. } => ParseError::too_large(Limit::Rows, value, max),
        e => ParseError::corrupt(e),
    })?;
    let many = file.molecules.len() > 1;
    let molecule_col = |c: &mut Vec<(&str, ValueKind)>| {
        if many {
            c.insert(0, ("Molecule", ValueKind::Text));
        }
    };
    let mut atom_cols = vec![("#", ValueKind::Number), ("Element", ValueKind::Text), ("Label", ValueKind::Text), ("X (Å)", ValueKind::Number), ("Y (Å)", ValueKind::Number), ("Z (Å)", ValueKind::Number), ("Charge", ValueKind::Number)];
    let mut bond_cols = vec![("Atom 1", ValueKind::Number), ("Atom 2", ValueKind::Number), ("Order", ValueKind::Text)];
    molecule_col(&mut atom_cols);
    molecule_col(&mut bond_cols);
    let (mut atoms, mut bonds, mut fields) = (Vec::new(), Vec::new(), Vec::new());
    for m in &file.molecules {
        cx.cancel.check()?;
        let tag = |row: &mut Vec<Cell>| {
            if many {
                row.insert(0, Cell::Text(m.name.clone()));
            }
        };
        for (i, a) in m.atoms.iter().enumerate() {
            let mut row = vec![Cell::Int(i as i64 + 1), Cell::Text(if a.element == 0 { "?".into() } else { elements::symbol(a.element).into() }), Cell::Text(a.label.clone())];
            row.extend(a.position.map(|c| if m.laid_out { Cell::Null } else { Cell::Float(c) }));
            row.push(if a.charge == 0 { Cell::Null } else { Cell::Int(a.charge as i64) });
            tag(&mut row);
            atoms.push(row);
        }
        for b in &m.bonds {
            let order = match b.order {
                1 => "single",
                2 => "double",
                3 => "triple",
                4 => "aromatic",
                _ => "",
            };
            let mut row = vec![Cell::Int(b.a as i64 + 1), Cell::Int(b.b as i64 + 1), Cell::Text(order.into())];
            tag(&mut row);
            bonds.push(row);
        }
        for (k, v) in &m.properties {
            let mut row = vec![Cell::Text(k.clone()), Cell::Text(v.clone())];
            tag(&mut row);
            fields.push(row);
        }
    }
    let first = &file.molecules[0];
    let total_atoms = atoms.len();
    let total_bonds = bonds.len();
    let table = |cols: &[(&str, ValueKind)], rows| Content::Table(Arc::new(OwnedTable::new(columns(cols), rows)));
    let mut doc = Document::new(FormatInfo::new(id, name, Category::Data)).with_facet(Role::Primary, "Atoms", table(&atom_cols, atoms));
    if total_bonds > 0 {
        doc = doc.with_facet(Role::Structure, "Bonds", table(&bond_cols, bonds));
    }
    if !fields.is_empty() {
        let mut cols = vec![("Field", ValueKind::Text), ("Value", ValueKind::Text)];
        molecule_col(&mut cols);
        doc = doc.with_facet(Role::Inspector, "Fields", table(&cols, fields));
    }
    if !first.name.is_empty() && !many {
        doc = doc.with_meta("Name", first.name.clone());
    }
    doc = doc.with_meta("Formula", formula(first.atoms.iter().map(|a| a.element))).with_meta("Atoms", total_atoms).with_meta("Bonds", total_bonds);
    if let Some(c) = first.cell {
        let len = |v: [f64; 3]| (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        doc = doc.with_meta("Cell (Å)", format!("{:.3} × {:.3} × {:.3}", len(c[0]), len(c[1]), len(c[2])));
    }
    for (k, v) in file.notes {
        doc = doc.with_meta(k, v);
    }
    if first.bonds_found {
        doc = doc.with_warning(Warning::new("The file gives no bonds: they're found by distance, as viewers find them, and may differ from the chemistry."));
    }
    if first.laid_out {
        doc = doc.with_warning(Warning::new("The file has no coordinates: positions are left out here, and the 3D view lays the atoms out from their bonds."));
    }
    Ok(doc.with_file_facets(src, Some(language)))
}

pub(crate) fn xyz(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    open(src, cx, Format::Xyz, "xyz", "XYZ coordinates", "xyz")
}

pub(crate) fn mol(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    let sdf = src.name().to_ascii_lowercase().ends_with(".sdf") || src.name().to_ascii_lowercase().ends_with(".sd") || src.bytes().windows(5).any(|w| w == b"$$$$\n" || w == b"$$$$\r");
    open(src, cx, Format::Mol, "mol", if sdf { "SD file" } else { "MDL molfile" }, "mol")
}

pub(crate) fn smiles(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    open(src, cx, Format::Smiles, "smiles", "SMILES", "smiles")
}

pub(crate) fn poscar(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    open(src, cx, Format::Poscar, "poscar", "VASP POSCAR", "poscar")
}

pub(crate) fn turbomole(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    open(src, cx, Format::Turbomole, "turbomole", "Turbomole coordinates", "turbomole")
}

pub(crate) fn cjson(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    open(src, cx, Format::ChemicalJson, "cjson", "Chemical JSON", "json")
}

pub(crate) fn qcschema(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    open(src, cx, Format::QcSchema, "qcschema", "QCSchema", "json")
}

pub(crate) fn molden(src: &FileSource, cx: &ParseContext) -> Result<Document> {
    open(src, cx, Format::Molden, "molden", "Molden", "molden")
}

#[cfg(test)]
mod tests {
    use super::formula;

    #[test]
    fn hill_order() {
        assert_eq!(formula([6, 1, 1, 1, 8, 1, 6, 1, 1].into_iter()), "C2H6O");
        assert_eq!(formula([8, 1, 1].into_iter()), "H2O");
        assert_eq!(formula([17, 11].into_iter()), "ClNa");
        assert_eq!(formula([6, 0].into_iter()), "C?");
        assert_eq!(formula(std::iter::empty()), "");
    }
}
