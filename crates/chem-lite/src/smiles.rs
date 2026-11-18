//! SMILES: a molecule as a line of text, `CC(=O)O` acetic acid, `c1ccccc1` benzene. A
//! `.smi` file is one a line, a name after it. Atoms are the organic subset (`B C N O
//! P S F Cl Br I`, lower case when aromatic) or anything in brackets (`[NH4+]`,
//! `[13CH3]`, `[Fe@@]`); bonds `- = # $ :` (and `/ \`, single), a dot none; branches in
//! parentheses; rings closed by digits (`%12` past nine). The hydrogens a formula
//! implies are added as atoms. There are no coordinates: [`crate::layout`] makes some.

use std::collections::HashMap;

use crate::{Atom, Bond, Budget, File, Molecule, Result, damaged, elements};

/// An atom as written: element, aromatic, hydrogens (`None`: implied, for the organic
/// subset), charge.
struct Parsed {
    element: u8,
    aromatic: bool,
    hydrogens: Option<u8>,
    charge: i8,
    label: String,
}

/// The valences the organic subset may have, lowest first.
fn valences(z: u8) -> &'static [u8] {
    match z {
        5 => &[3],
        6 => &[4],
        7 => &[3, 5],
        8 => &[2],
        15 => &[3, 5],
        16 => &[2, 4, 6],
        9 | 17 | 35 | 53 => &[1],
        _ => &[],
    }
}

fn bracket(inner: &str) -> Result<Parsed> {
    let b = inner.as_bytes();
    let mut i = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    let isotope = &inner[..i];
    // The symbol: `*`, an aromatic one (`c`, `se`, `as`), or an element.
    let rest = &inner[i..];
    let (element, aromatic, len) = if rest.starts_with('*') {
        (0, false, 1)
    } else if let Some(z) = ["se", "as", "te"].iter().find(|s| rest.starts_with(**s)).and_then(|s| elements::by_symbol(s)) {
        (z, true, 2)
    } else if rest.starts_with(|c: char| c.is_ascii_lowercase()) {
        let z = elements::by_symbol(&rest[..1]).ok_or_else(|| damaged(format!("SMILES: no element `{}`", &rest[..1])))?;
        (z, true, 1)
    } else {
        let two = rest.get(..2).filter(|s| s.as_bytes()[1].is_ascii_lowercase()).and_then(elements::by_symbol);
        match two {
            Some(z) => (z, false, 2),
            None => (elements::by_symbol(rest.get(..1).unwrap_or("")).ok_or_else(|| damaged(format!("SMILES: no element in `[{inner}]`")))?, false, 1),
        }
    };
    i += len;
    while i < b.len() && b[i] == b'@' {
        i += 1;
    }
    // Chirality classes like `@TH1`, `@OH12`.
    if i < b.len() && b[i].is_ascii_uppercase() && b[i] != b'H' {
        while i < b.len() && b[i].is_ascii_alphanumeric() && b[i] != b'H' {
            i += 1;
        }
    }
    let mut hydrogens = 0u8;
    if i < b.len() && b[i] == b'H' {
        i += 1;
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        hydrogens = inner[start..i].parse().unwrap_or(1);
    }
    let mut charge = 0i32;
    while i < b.len() && matches!(b[i], b'+' | b'-') {
        let sign = if b[i] == b'+' { 1 } else { -1 };
        i += 1;
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        charge += sign * inner[start..i].parse::<i32>().unwrap_or(1);
    }
    let label = format!("{isotope}{}", elements::symbol(element));
    Ok(Parsed { element, aromatic, hydrogens: Some(hydrogens), charge: charge.clamp(-128, 127) as i8, label })
}

/// One SMILES string as a molecule, its hydrogens added, without positions.
pub fn parse(smiles: &str) -> Result<Molecule> {
    let mut atoms: Vec<Parsed> = Vec::new();
    let mut bonds: Vec<Bond> = Vec::new();
    let mut prev: Option<usize> = None;
    let mut stack: Vec<Option<usize>> = Vec::new();
    let mut order: Option<u8> = None;
    let mut rings: HashMap<u32, (usize, Option<u8>)> = HashMap::new();
    let s = smiles.as_bytes();
    let mut i = 0;
    let bond_order = |a: &Parsed, b: &Parsed, given: Option<u8>| given.unwrap_or(if a.aromatic && b.aromatic { 4 } else { 1 });
    while i < s.len() {
        let c = s[i] as char;
        let mut atom = None;
        match c {
            '(' => {
                stack.push(prev);
                i += 1;
            }
            ')' => {
                prev = stack.pop().ok_or_else(|| damaged("SMILES: a `)` with no `(`"))?;
                i += 1;
            }
            '-' | '/' | '\\' => (order, i) = (Some(1), i + 1),
            '=' => (order, i) = (Some(2), i + 1),
            '#' => (order, i) = (Some(3), i + 1),
            '$' => (order, i) = (Some(3), i + 1),
            ':' => (order, i) = (Some(4), i + 1),
            '.' => (prev, i) = (None, i + 1),
            '[' => {
                let end = smiles[i..].find(']').ok_or_else(|| damaged("SMILES: a `[` with no `]`"))? + i;
                atom = Some(bracket(&smiles[i + 1..end])?);
                i = end + 1;
            }
            '%' | '0'..='9' => {
                let (n, len) = if c == '%' && s.get(i + 1) == Some(&b'(') {
                    // `%(123)`: OpenSMILES's ring numbers past 99.
                    let close = smiles[i..].find(')').ok_or_else(|| damaged("SMILES: `%(` with no `)`"))?;
                    let n = smiles[i + 2..i + close].parse::<u32>().map_err(|_| damaged("SMILES: `%(…)` without a number"))?;
                    (n, close + 1)
                } else if c == '%' {
                    let digits = smiles.get(i + 1..i + 3).filter(|d| d.bytes().all(|b| b.is_ascii_digit())).ok_or_else(|| damaged("SMILES: `%` without two digits"))?;
                    (digits.parse::<u32>().unwrap(), 3)
                } else {
                    (c.to_digit(10).unwrap(), 1)
                };
                i += len;
                let here = prev.ok_or_else(|| damaged("SMILES: a ring bond before any atom"))?;
                match rings.remove(&n) {
                    Some((other, first)) => {
                        let given = order.or(first);
                        bonds.push(Bond::new(other as u32, here as u32, bond_order(&atoms[other], &atoms[here], given)));
                    }
                    None => {
                        rings.insert(n, (here, order));
                    }
                }
                order = None;
            }
            '*' => {
                atom = Some(Parsed { element: 0, aromatic: false, hydrogens: Some(0), charge: 0, label: "*".into() });
                i += 1;
            }
            _ if c.is_ascii_alphabetic() => {
                let two = smiles.get(i..i + 2).filter(|t| *t == "Cl" || *t == "Br");
                let (symbol, aromatic) = match two {
                    Some(t) => (t, false),
                    None if "bcnops".contains(c) => (&smiles[i..i + 1], true),
                    None if "BCNOPSFI".contains(c) => (&smiles[i..i + 1], false),
                    None => return Err(damaged(format!("SMILES: `{c}` isn't an atom outside brackets"))),
                };
                i += symbol.len();
                let element = elements::by_symbol(symbol).unwrap();
                atom = Some(Parsed { element, aromatic, hydrogens: None, charge: 0, label: elements::symbol(element).into() });
            }
            _ if c.is_whitespace() => break,
            _ => return Err(damaged(format!("SMILES: `{c}` isn't understood"))),
        }
        if let Some(a) = atom {
            let here = atoms.len();
            if let Some(p) = prev {
                bonds.push(Bond::new(p as u32, here as u32, bond_order(&atoms[p], &a, order)));
            }
            atoms.push(a);
            prev = Some(here);
            order = None;
        }
    }
    if !stack.is_empty() {
        return Err(damaged("SMILES: a `(` with no `)`"));
    }
    if let Some(n) = rings.keys().next() {
        return Err(damaged(format!("SMILES: ring {n} isn't closed")));
    }
    // The hydrogens: each organic-subset atom's lowest valence its bonds don't pass.
    let mut used = vec![0u32; atoms.len()];
    let mut aromatic_bonds = vec![0u32; atoms.len()];
    for b in &bonds {
        for end in [b.a, b.b] {
            if b.order == 4 {
                aromatic_bonds[end as usize] += 1;
            } else {
                used[end as usize] += b.order as u32;
            }
        }
    }
    let mut m = Molecule { laid_out: true, ..Molecule::default() };
    let mut hydrogens = Vec::new();
    for (k, a) in atoms.iter().enumerate() {
        let h = a.hydrogens.unwrap_or_else(|| {
            // An aromatic atom's ring bonds count one each, and one more between them.
            let total = used[k] + aromatic_bonds[k] + u32::from(a.aromatic && aromatic_bonds[k] > 0);
            valences(a.element).iter().find(|v| **v as u32 >= total).map_or(0, |v| (*v as u32 - total) as u8)
        });
        let mut atom = Atom::new(a.element, a.label.clone(), [0.0; 3]);
        atom.charge = a.charge;
        m.atoms.push(atom);
        hydrogens.push(h);
    }
    for (k, h) in hydrogens.into_iter().enumerate() {
        for _ in 0..h {
            let at = m.atoms.len() as u32;
            m.atoms.push(Atom::new(1, "H", [0.0; 3]));
            bonds.push(Bond::new(k as u32, at, 1));
        }
    }
    m.bonds = bonds;
    m.properties.push(("SMILES".into(), smiles.to_string()));
    Ok(m)
}

/// A file of SMILES, one a line with a name after it. A line that isn't SMILES (a
/// header, a formula, a typo) is skipped and counted, as long as some lines are; a
/// file of none is damaged, with the first line's error.
pub fn read_file(text: &str, budget: &mut Budget) -> Result<File> {
    let mut file = File::default();
    let (mut skipped, mut first_error) = (0usize, None);
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let (smiles, name) = line.split_once(char::is_whitespace).map_or((line, ""), |(s, n)| (s, n.trim()));
        // A header line (`SMILES Name`) isn't a molecule.
        if smiles.eq_ignore_ascii_case("smiles") {
            continue;
        }
        let mut m = match parse(smiles) {
            Ok(m) => m,
            Err(e) => {
                skipped += 1;
                first_error.get_or_insert(e);
                continue;
            }
        };
        budget.molecule()?;
        budget.atoms(m.atoms.len() as u64)?;
        m.name = if name.is_empty() { format!("Molecule {}", file.molecules.len() + 1) } else { name.to_string() };
        file.molecules.push(m);
    }
    if file.molecules.is_empty()
        && let Some(e) = first_error
    {
        return Err(e);
    }
    if file.molecules.len() > 1 {
        file.notes.push(("Molecules".into(), file.molecules.len().to_string()));
    }
    if skipped > 0 {
        file.notes.push(("Lines that aren't SMILES".into(), skipped.to_string()));
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Format, Limits, read};

    fn formula(m: &Molecule) -> String {
        let mut count: Vec<(u8, usize)> = Vec::new();
        for a in &m.atoms {
            match count.iter_mut().find(|c| c.0 == a.element) {
                Some(c) => c.1 += 1,
                None => count.push((a.element, 1)),
            }
        }
        count.sort();
        count.iter().map(|(z, n)| format!("{}{n}", if *z == 0 { "*" } else { elements::symbol(*z) })).collect()
    }

    #[test]
    fn hydrogens_as_a_chemist_counts_them() {
        for (smiles, want) in [
            ("C", "H4C1"),
            ("CC(=O)O", "H4C2O2"),
            ("c1ccccc1", "H6C6"),
            ("c1ccncc1", "H5C5N1"),
            ("c1cc[nH]c1", "H5C4N1"),
            ("C#N", "H1C1N1"),
            ("[NH4+]", "H4N1"),
            ("[Na+].[Cl-]", "Na1Cl1"),
            ("ClC(Cl)(Cl)Cl", "C1Cl4"),
            ("CS(=O)(=O)C", "H6C2O2S1"),
            ("C1CC2CCC1C2", "H12C7"),
            ("C%10CC%10", "H6C3"),
            ("C%(100)CC%(100)", "H6C3"),
            ("[13CH3]C", "H6C2"),
            ("F/C=C/F", "H2C2F2"),
            ("[Fe@@]", "Fe1"),
            ("C[C@H](N)O", "H7C2N1O1"),
            ("*C", "*1H3C1"),
            ("[se]1cccc1", "H4C4Se1"),
            ("C=1CC1", "H4C3"),
            ("[C@TH1H](F)(Cl)Br", "H1C1F1Cl1Br1"),
        ] {
            let m = parse(smiles).unwrap();
            assert_eq!(formula(&m), want, "{smiles}");
        }
        let acid = parse("CC(=O)O").unwrap();
        assert_eq!(acid.bonds.iter().filter(|b| b.order == 2).count(), 1);
        assert_eq!(parse("c1ccccc1").unwrap().bonds.iter().filter(|b| b.order == 4).count(), 6);
        let ions = parse("[NH4+].[O-2]").unwrap();
        assert_eq!((ions.atoms[0].charge, ions.atoms[1].charge), (1, -2));
        assert_eq!(parse("[Cu++]").unwrap().atoms[0].charge, 2);
        assert_eq!(parse("C=1CC1").unwrap().bonds.iter().filter(|b| b.order == 2).count(), 1);
    }

    #[test]
    fn mistakes_are_damage() {
        for bad in ["C(C", "CC)", "C1CC", "[Na", "Xx", "C%1", "1CC", "[Qq]", "C^C", "[]", "C%(12", "C%()C"] {
            assert!(matches!(parse(bad), Err(crate::Error::Damaged(_))), "{bad}");
        }
    }

    #[test]
    fn a_file_of_them_laid_out() {
        let f = read(b"# comment\nSMILES Name\nCCO ethanol\n\nc1ccccc1\n", Format::Smiles, &Limits::default()).unwrap();
        assert_eq!(f.molecules.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), ["ethanol", "Molecule 2"]);
        assert_eq!(f.notes, [("Molecules".to_string(), "2".to_string())]);
        let mut e = f.molecules[0].clone();
        // No positions until a drawing asks for them.
        assert!(e.laid_out && !e.bonds_found && e.atoms.iter().all(|a| a.position == [0.0; 3]));
        crate::layout(&mut e);
        assert!(e.atoms.iter().all(|a| a.position.iter().all(|c| c.is_finite())));
        // The C–O bond came out about its length.
        let d: f64 = (0..3).map(|c| (e.atoms[1].position[c] - e.atoms[2].position[c]).powi(2)).sum::<f64>().sqrt();
        assert!((d - 1.42).abs() < 0.2, "{d}");
        assert_eq!(e.properties[0], ("SMILES".to_string(), "CCO".to_string()));
        assert!(matches!(read(b"CCO\nCCO\n", Format::Smiles, &Limits::default().with_molecules(1)), Err(crate::Error::TooLarge { .. })));
        // Lines that aren't SMILES are skipped and counted; a file of nothing else isn't one.
        let f = read(b"CCO\nC(C bad\nUx formula\nCC\n", Format::Smiles, &Limits::default()).unwrap();
        assert_eq!(f.molecules.len(), 2);
        assert!(f.notes.contains(&("Lines that aren't SMILES".to_string(), "2".to_string())));
        assert!(matches!(read(b"C(C\nXx\n", Format::Smiles, &Limits::default()), Err(crate::Error::Damaged(e)) if e.contains("`(`")));
        assert!(matches!(read(b"CCCCCC\n", Format::Smiles, &Limits::default().with_atoms(5)), Err(crate::Error::TooLarge { .. })));
    }
}

#[cfg(test)]
mod edges {
    use super::*;

    fn hydrogens(smiles: &str) -> usize {
        parse(smiles).unwrap().atoms.iter().filter(|a| a.element == 1).count()
    }

    #[test]
    fn every_valence_in_the_table() {
        for (smiles, h) in [("B", 3), ("P", 3), ("S", 2), ("F", 1), ("Cl", 1), ("Br", 1), ("I", 1), ("O=P(O)(O)O", 3), ("CS(=O)C", 6)] {
            assert_eq!(hydrogens(smiles), h, "{smiles}");
        }
    }

    #[test]
    fn bonds_written_out() {
        // `$` (quadruple, drawn as triple) and `:` (aromatic) between any atoms.
        let m = parse("[C]$[C]").unwrap();
        assert_eq!(m.bonds[0].order, 3);
        assert_eq!(parse("C:C").unwrap().bonds[0].order, 4);
        // An aromatic bond between aliphatic atoms adds no extra valence.
        assert_eq!(hydrogens("C:C"), 6);
        // Toluene: the ring's bonds aromatic, the methyl's single.
        let t = parse("c1ccccc1C").unwrap();
        assert_eq!(t.bonds.iter().filter(|b| b.order == 4).count(), 6);
        // An aromatic atom with no ring bonds is counted as written.
        assert_eq!(hydrogens("c"), 4);
    }

    #[test]
    fn where_a_smiles_ends_and_what_isnt_one() {
        assert_eq!(parse("CC O").unwrap().atoms.iter().filter(|a| a.element == 6).count(), 2);
        assert!(matches!(parse("C^C"), Err(crate::Error::Damaged(e)) if e.contains("isn't understood")));
        // A chirality class closing its bracket.
        assert_eq!(parse("[C@TH1]").unwrap().atoms[0].element, 6);
        assert_eq!(parse("[Pt@SP1]").unwrap().atoms[0].element, 78);
        let f = read_file("CCO\n", &mut crate::Budget::new(&crate::Limits::default())).unwrap();
        assert!(f.notes.is_empty());
    }
}
