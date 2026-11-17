//! PDB: fixed-column records. `ATOM` and `HETATM` are atoms (the first model's, when
//! there are several), `CONECT` bonds the file lists (mostly for ligands), `CRYST1` the
//! cell. A protein's own bonds aren't listed, so they're found as well.

use crate::{Atom, Bond, Budget, File, Molecule, Result, elements, find_bonds, number};

fn col(l: &str, a: usize, b: usize) -> &str {
    l.get(a - 1..b.min(l.len())).unwrap_or("").trim()
}

pub fn read(text: &str, budget: &mut Budget) -> Result<File> {
    let mut m = Molecule::default();
    let mut serials = std::collections::HashMap::new();
    let mut conect = Vec::new();
    let mut models = 0;
    for l in text.lines() {
        if l.starts_with("MODEL") {
            models += 1;
        }
        if l.starts_with("ENDMDL") && !m.atoms.is_empty() {
            // The first model only; the others' bonds would cross it.
            for rest in text.lines().skip_while(|x| !x.starts_with("ENDMDL")) {
                if rest.starts_with("MODEL") {
                    models += 1;
                }
            }
            break;
        }
        if l.starts_with("HEADER") && m.name.is_empty() {
            m.name = col(l, 63, 66).to_string();
        } else if l.starts_with("CRYST1") {
            let v = [col(l, 7, 15), col(l, 16, 24), col(l, 25, 33), col(l, 34, 40), col(l, 41, 47), col(l, 48, 54)].map(number);
            if let [Some(a), Some(b), Some(c), Some(al), Some(be), Some(ga)] = v
                && a > 1.0
            {
                m.cell = Some(crate::text::cell_vectors([a, b, c], [al, be, ga]));
            }
        } else if l.starts_with("ATOM  ") || l.starts_with("HETATM") {
            let (Some(x), Some(y), Some(z)) = (number(col(l, 31, 38)), number(col(l, 39, 46)), number(col(l, 47, 54))) else { continue };
            budget.atoms(1)?;
            let name = col(l, 13, 16);
            // The element's own columns; old files leave them out, and the name's first
            // letters (right-justified for two-letter elements) say it.
            // ` CA ` is an alpha carbon: a one-letter element sits in column 14, a
            // two-letter one (`FE  `) fills 13 and 14.
            let element = elements::by_symbol(col(l, 77, 78)).or_else(|| {
                let raw = l.get(12..14).unwrap_or("");
                match raw.as_bytes() {
                    [first, second] if first.is_ascii_alphabetic() && second.is_ascii_alphabetic() && l.starts_with("HETATM") => elements::by_symbol(raw),
                    [_, second] if second.is_ascii_alphabetic() => elements::by_symbol(&raw[1..]),
                    _ => None,
                }
                .or_else(|| elements::parse(name))
            });
            let mut a = Atom::new(element.unwrap_or(0), format!("{} {}{}", name, col(l, 18, 20), col(l, 23, 26)), [x, y, z]);
            let charge = col(l, 79, 80);
            a.charge = match (charge.get(..1).and_then(|d| d.parse::<i8>().ok()), charge.get(1..)) {
                (Some(n), Some("-")) => -n,
                (Some(n), _) => n,
                _ => 0,
            };
            if let Ok(serial) = col(l, 7, 11).parse::<u32>() {
                serials.insert(serial, m.atoms.len() as u32);
            }
            m.atoms.push(a);
        } else if l.starts_with("CONECT") {
            let ids: Vec<u32> = [(7, 11), (12, 16), (17, 21), (22, 26), (27, 31)].iter().filter_map(|(a, b)| col(l, *a, *b).parse().ok()).collect();
            if let Some((first, rest)) = ids.split_first() {
                conect.extend(rest.iter().map(|r| (*first, *r)));
            }
        }
    }
    if m.atoms.is_empty() {
        return Err(crate::damaged("PDB: no atoms"));
    }
    budget.molecule()?;
    let mut bonds = find_bonds(&m.atoms);
    let found = !bonds.is_empty();
    let mut listed = std::collections::HashSet::new();
    for (a, b) in conect {
        if let (Some(&a), Some(&b)) = (serials.get(&a), serials.get(&b))
            && a != b
        {
            let (a, b) = (a.min(b), a.max(b));
            // A bond CONECT lists twice (once from each end) is one.
            if listed.insert((a, b)) && !bonds.iter().any(|x| x.a == a && x.b == b) {
                bonds.push(Bond::new(a, b, 1));
            }
        }
    }
    m.bonds = bonds;
    m.bonds_found = found;
    let mut file = File { molecules: vec![m], notes: Vec::new() };
    if models > 1 {
        file.notes.push(("Models".into(), format!("{models} (the first is shown)")));
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use crate::{Format, Limits, read};

    const PDB: &str = "\
HEADER    WATER                                   01-JAN-00   1ABC
CRYST1   10.000   10.000   10.000  90.00  90.00  90.00 P 1           1
MODEL        1
HETATM    1  O   HOH A   1       0.000   0.000   0.000  1.00  0.00           O
HETATM    2  H1  HOH A   1       0.960   0.000   0.000  1.00  0.00           H
HETATM    3  H2  HOH A   1      -0.240   0.930   0.000  1.00  0.00           H
HETATM    4 FE   HEM A   2       5.000   5.000   5.000  1.00  0.00          FE2+
HETATM    5 CA   CA  A   3      20.000  20.000  20.000  1.00  0.00
ATOM      6  CA  ALA A   4       9.000   9.000   9.000
ATOM      7  HG1 THR A   4       9.900   9.000   9.000
CONECT    4    1
CONECT    1    4
ENDMDL
MODEL        2
HETATM    1  O   HOH A   1       0.000   0.000   0.000  1.00  0.00           O
ENDMDL
";

    #[test]
    fn atoms_bonds_cell_and_models() {
        let f = read(PDB.as_bytes(), Format::Pdb, &Limits::default()).unwrap();
        let m = &f.molecules[0];
        assert_eq!(m.name, "1ABC");
        assert_eq!(m.atoms.iter().map(|a| a.element).collect::<Vec<_>>(), [8, 1, 1, 26, 20, 6, 1]);
        assert_eq!((m.atoms[3].charge, m.atoms[0].label.as_str()), (2, "O HOH1"));
        // Water's two bonds found, the iron's listed, and listed twice it's one.
        assert_eq!(m.bonds.iter().map(|b| (b.a, b.b)).collect::<Vec<_>>(), [(0, 1), (0, 2), (5, 6), (0, 3)]);
        assert!(m.bonds_found);
        assert_eq!(m.cell.unwrap()[0][0], 10.0);
        assert_eq!(f.notes, [("Models".to_string(), "2 (the first is shown)".to_string())]);
        assert!(matches!(read(b"HEADER\n", Format::Pdb, &Limits::default()), Err(crate::Error::Damaged(_))));
        assert!(matches!(read(PDB.as_bytes(), Format::Pdb, &Limits::default().with_atoms(2)), Err(crate::Error::TooLarge { .. })));
        let m = &read(b"ATOM      1  N   ALA A   1       0.000   0.000   0.000  1.00  0.00           N1-\n", Format::Pdb, &Limits::default()).unwrap().molecules[0];
        assert_eq!(m.atoms[0].charge, -1);
    }
}
