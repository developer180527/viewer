//! XYZ: a count, a comment, then a line per atom (`C 0.0 1.2 -0.5`), repeated for
//! every frame. Extended XYZ puts `key=value` pairs in the comment: `Lattice="…"` is the
//! cell, and `Properties=species:S:1:pos:R:3:…` says which columns hold what.

use crate::{Atom, Budget, File, Molecule, Result, damaged, elements, number};

/// A count alone on the first line, and an element and three numbers on the third.
pub fn looks_like(text: &str) -> bool {
    let mut lines = text.lines();
    let count = lines.next().map(str::trim).and_then(|l| l.parse::<u64>().ok());
    let atom = lines.nth(1).map(|l| l.split_whitespace().collect::<Vec<_>>());
    matches!((count, atom), (Some(n), Some(a)) if n > 0 && a.len() >= 4 && elements::parse(a[0]).is_some() && a[1..4].iter().all(|v| number(v).is_some()))
}

/// The comment's `key=value` pairs; values may be quoted.
fn pairs(comment: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut chars = comment.trim().chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        let key: String = chars.by_ref().take_while(|c| *c != '=' && !c.is_whitespace()).collect();
        if key.is_empty() {
            return out;
        }
        let value: String = match chars.peek() {
            Some('"') => {
                chars.next();
                chars.by_ref().take_while(|c| *c != '"').collect()
            }
            _ => chars.by_ref().take_while(|c| !c.is_whitespace()).collect(),
        };
        out.push((key, value));
    }
}

pub fn read(text: &str, budget: &mut Budget) -> Result<File> {
    let mut file = File::default();
    let mut lines = text.lines().peekable();
    while let Some(first) = lines.next() {
        if first.trim().is_empty() {
            continue;
        }
        let Ok(count) = first.trim().parse::<u64>() else {
            if file.molecules.is_empty() {
                return Err(crate::Error::NotThisFormat);
            }
            // Something after the last frame that isn't one.
            break;
        };
        budget.atoms(count)?;
        budget.molecule()?;
        let comment = lines.next().unwrap_or("").trim().to_string();
        let props = pairs(&comment);
        let get = |k: &str| props.iter().find(|p| p.0.eq_ignore_ascii_case(k)).map(|p| p.1.as_str());
        // Which columns: species and position, from Properties when it's there.
        let (mut species, mut position) = (0usize, 1usize);
        if let Some(spec) = get("Properties") {
            let parts: Vec<&str> = spec.split(':').collect();
            let mut col = 0;
            for p in parts.chunks(3) {
                let width = p.get(2).and_then(|w| w.parse::<usize>().ok()).unwrap_or(1);
                match p[0] {
                    "species" => species = col,
                    "pos" => position = col,
                    _ => {}
                }
                col += width;
            }
        }
        let mut m = Molecule { name: format!("Frame {}", file.molecules.len() + 1), ..Molecule::default() };
        if let Some(l) = get("Lattice") {
            let v: Vec<f64> = l.split_whitespace().filter_map(number).collect();
            if v.len() == 9 {
                m.cell = Some([[v[0], v[1], v[2]], [v[3], v[4], v[5]], [v[6], v[7], v[8]]]);
            }
        }
        if !comment.is_empty() {
            m.properties.push(("Comment".into(), comment.clone()));
        }
        for i in 0..count {
            let line = lines.next().ok_or_else(|| damaged(format!("XYZ: frame {} ends after {i} of its {count} atoms", file.molecules.len() + 1)))?;
            let f: Vec<&str> = line.split_whitespace().collect();
            let label = f.get(species).copied().unwrap_or("");
            let p: Vec<f64> = f.get(position..position + 3).unwrap_or(&[]).iter().filter_map(|v| number(v)).collect();
            if p.len() != 3 {
                return Err(damaged(format!("XYZ: atom {} has no three coordinates", i + 1)));
            }
            m.atoms.push(Atom::new(elements::parse(label).unwrap_or(0), label, [p[0], p[1], p[2]]));
        }
        file.molecules.push(m);
    }
    match file.molecules.len() {
        // One frame has no name of its own.
        1 => file.molecules[0].name.clear(),
        n => file.notes.push(("Frames".into(), n.to_string())),
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Limits, by_extension, read as read_any, Format};

    #[test]
    fn frames_and_extended_comments() {
        let text = "3\nwater\nO 0 0 0\nH 0.96 0 0\nH -0.24 0.93 0\n2\nLattice=\"5 0 0 0 5 0 0 0 5\" Properties=id:I:1:pos:R:3:species:S:1 energy=-1.5\n1 0.0 0.0 0.0 Na\n2 2.8 0.0 0.0 Cl\n\n";
        assert!(looks_like(text) && !looks_like("x\n\nC 0 0 0") && !looks_like("2\n\nC 0 0") && !looks_like("0\n\nC 0 0 0"));
        let f = read_any(text.as_bytes(), Format::Xyz, &Limits::default()).unwrap();
        assert_eq!(f.molecules.len(), 2);
        assert_eq!(f.notes, [("Frames".to_string(), "2".to_string())]);
        let w = &f.molecules[0];
        assert_eq!((w.name.as_str(), w.atoms.len(), w.bonds.len(), w.bonds_found), ("Frame 1", 3, 2, true));
        assert_eq!(read_any(b"1\n\nC 0 0 0\n", Format::Xyz, &Limits::default()).unwrap().molecules[0].name, "");
        assert_eq!(w.properties, [("Comment".to_string(), "water".to_string())]);
        let s = &f.molecules[1];
        assert_eq!(s.atoms.iter().map(|a| (a.element, a.position[0])).collect::<Vec<_>>(), [(11, 0.0), (17, 2.8)]);
        assert_eq!(s.cell, Some([[5.0, 0.0, 0.0], [0.0, 5.0, 0.0], [0.0, 0.0, 5.0]]));
        assert_eq!(by_extension("traj.extxyz"), Some(Format::Xyz));
    }

    #[test]
    fn numbers_for_elements_and_damage() {
        let f = read_any(b"2\n\n6 0 0 0\n8 1.2 0 0\n", Format::Xyz, &Limits::default()).unwrap();
        assert_eq!(f.molecules[0].atoms.iter().map(|a| a.element).collect::<Vec<_>>(), [6, 8]);
        assert!(matches!(read_any(b"3\n\nC 0 0 0\n", Format::Xyz, &Limits::default()), Err(crate::Error::Damaged(_))));
        assert!(matches!(read_any(b"1\n\nC 0 x 0\n", Format::Xyz, &Limits::default()), Err(crate::Error::Damaged(_))));
        assert_eq!(read_any(b"not xyz\n", Format::Xyz, &Limits::default()), Err(crate::Error::NotThisFormat));
        // Trailing text that isn't a frame ends the file.
        assert_eq!(read_any(b"1\n\nC 0 0 0\nend\n", Format::Xyz, &Limits::default()).unwrap().molecules.len(), 1);
        assert!(matches!(read_any(b"9999\n\nC 0 0 0\n", Format::Xyz, &Limits::default().with_atoms(10)), Err(crate::Error::TooLarge { .. })));
        assert!(matches!(read_any(b"1\n\nC 0 0 0\n1\n\nC 0 0 0\n", Format::Xyz, &Limits::default().with_molecules(1)), Err(crate::Error::TooLarge { what: "molecules", .. })));
        assert_eq!(pairs(r#" a=1  b="x y"  c"#), [("a".into(), "1".into()), ("b".into(), "x y".into()), ("c".into(), String::new())]);
    }
}
