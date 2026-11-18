//! VASP's POSCAR (and CONTCAR): a comment, a scale, the cell's three vectors, the
//! elements (VASP 5; VASP 4 has none, the comment often naming them), how many atoms
//! of each, then positions, `Direct` (fractions of the cell) or `Cartesian`.

use crate::{Atom, Budget, File, Molecule, Result, damaged, elements, number};

/// A comment, a scale (one number or three), three cell vectors of three numbers, then
/// counts of whole numbers, perhaps after a line of element names: a POSCAR whatever
/// it's called (`POSCAR.relax`, `structure_7`, a renamed copy).
pub fn looks_like(text: &str) -> bool {
    let lines: Vec<&str> = text.lines().take(7).collect();
    if lines.len() < 6 {
        return false;
    }
    let scale = numbers(lines[1]).len();
    let whole = |l: &str| {
        let w: Vec<&str> = l.split_whitespace().collect();
        !w.is_empty() && w.iter().all(|x| x.parse::<u64>().is_ok())
    };
    let counts = whole(lines[5]) || (lines.get(6).is_some_and(|l| whole(l)) && lines[5].split_whitespace().all(|w| w.starts_with(|c: char| c.is_ascii_alphabetic())));
    (scale == 1 || scale == 3) && lines[1].split_whitespace().count() == scale && lines[2..5].iter().all(|l| numbers(l).len() == 3 && l.split_whitespace().count() == 3) && counts
}

fn numbers(l: &str) -> Vec<f64> {
    l.split_whitespace().map_while(number).collect()
}

pub fn read(text: &str, budget: &mut Budget) -> Result<File> {
    let lines: Vec<&str> = text.lines().collect();
    let line = |i: usize| lines.get(i).copied().ok_or_else(|| damaged("POSCAR: the file ends before its atoms"));
    let comment = line(0)?.trim();
    let scale = numbers(line(1)?);
    let mut cell = [[0.0; 3]; 3];
    for (k, row) in cell.iter_mut().enumerate() {
        let v = numbers(line(2 + k)?);
        if v.len() < 3 {
            return Err(damaged(format!("POSCAR: cell vector {} isn't three numbers", k + 1)));
        }
        *row = [v[0], v[1], v[2]];
    }
    // A negative scale is the cell's volume.
    let volume = |c: &[[f64; 3]; 3]| (c[0][0] * (c[1][1] * c[2][2] - c[1][2] * c[2][1]) - c[0][1] * (c[1][0] * c[2][2] - c[1][2] * c[2][0]) + c[0][2] * (c[1][0] * c[2][1] - c[1][1] * c[2][0])).abs();
    let factors: [f64; 3] = match scale.as_slice() {
        [s] if *s < 0.0 => [(-s / volume(&cell).max(1e-12)).cbrt(); 3],
        [s] => [*s; 3],
        [a, b, c, ..] => [*a, *b, *c],
        _ => return Err(damaged("POSCAR: no scale")),
    };
    for row in &mut cell {
        for (k, v) in row.iter_mut().enumerate() {
            *v *= factors[k];
        }
    }
    // VASP 5 names the elements on a line of their own; VASP 4 goes straight to counts.
    let mut at = 5;
    let mut names: Vec<String> = Vec::new();
    if numbers(line(at)?).is_empty() {
        names = line(at)?.split_whitespace().map(|s| s.split(['/', '_']).next().unwrap_or(s).to_string()).collect();
        at += 1;
    }
    let counts: Vec<u64> = line(at)?.split_whitespace().map_while(|c| c.parse().ok()).collect();
    if counts.is_empty() {
        return Err(damaged("POSCAR: no atom counts"));
    }
    if names.is_empty() {
        names = comment.split_whitespace().filter(|w| elements::by_symbol(w).is_some()).map(str::to_string).collect();
    }
    let total: u64 = counts.iter().sum();
    budget.atoms(total)?;
    budget.molecule()?;
    at += 1;
    if line(at)?.trim_start().to_ascii_lowercase().starts_with('s') {
        at += 1;
    }
    let mode = line(at)?.trim_start().to_ascii_lowercase();
    let cartesian = mode.starts_with('c') || mode.starts_with('k');
    at += 1;
    let mut m = Molecule { name: comment.to_string(), cell: Some(cell), ..Molecule::default() };
    for (kind, &n) in counts.iter().enumerate() {
        let name = names.get(kind).map_or_else(|| format!("Type {}", kind + 1), Clone::clone);
        let z = elements::by_symbol(&name).unwrap_or(0);
        for _ in 0..n {
            let v = numbers(line(at)?);
            if v.len() < 3 {
                return Err(damaged(format!("POSCAR: atom {} isn't three numbers", m.atoms.len() + 1)));
            }
            let p = if cartesian {
                [v[0] * factors[0], v[1] * factors[1], v[2] * factors[2]]
            } else {
                let f = [v[0], v[1], v[2]];
                [0, 1, 2].map(|c| f[0] * cell[0][c] + f[1] * cell[1][c] + f[2] * cell[2][c])
            };
            m.atoms.push(Atom::new(z, name.clone(), p));
            at += 1;
        }
    }
    Ok(File { molecules: vec![m], notes: Vec::new() })
}

#[cfg(test)]
mod tests {
    use crate::{Format, Limits, by_extension, read};

    const NACL: &str = "rock salt\n1.0\n5.64 0 0\n0 5.64 0\n0 0 5.64\nNa Cl\n1 1\nDirect\n0 0 0\n0.5 0.5 0.5\n";

    #[test]
    fn direct_and_cartesian() {
        let m = &read(NACL.as_bytes(), Format::Poscar, &Limits::default()).unwrap().molecules[0];
        assert_eq!(m.atoms.iter().map(|a| (a.element, a.position)).collect::<Vec<_>>(), [(11, [0.0; 3]), (17, [2.82; 3])]);
        assert_eq!(m.cell.unwrap()[1], [0.0, 5.64, 0.0]);
        // VASP 4: no element line, the comment naming them; a scale of 2; selective
        // dynamics; Cartesian.
        let v4 = "Si O\n2.0\n1 0 0\n0 1 0\n0 0 1\n1 1\nSelective dynamics\nCartesian\n0 0 0 T T T\n0.5 0 0 F F F\n";
        let m = &read(v4.as_bytes(), Format::Poscar, &Limits::default()).unwrap().molecules[0];
        assert_eq!(m.atoms.iter().map(|a| (a.element, a.position[0])).collect::<Vec<_>>(), [(14, 0.0), (8, 1.0)]);
        assert_eq!(m.cell.unwrap()[0], [2.0, 0.0, 0.0]);
        // A negative scale is the volume; three scales are one per axis.
        let vol = NACL.replace("\n1.0\n", "\n-8.0\n").replace("5.64", "1");
        assert!((read(vol.as_bytes(), Format::Poscar, &Limits::default()).unwrap().molecules[0].cell.unwrap()[0][0] - 2.0).abs() < 1e-9);
        let three = NACL.replace("\n1.0\n", "\n1 2 3\n");
        assert!((read(three.as_bytes(), Format::Poscar, &Limits::default()).unwrap().molecules[0].cell.unwrap()[2][2] - 16.92).abs() < 1e-9);
        // No names anywhere: types by number.
        let unnamed = NACL.replace("rock salt", "x").replace("Na Cl\n", "");
        assert_eq!(read(unnamed.as_bytes(), Format::Poscar, &Limits::default()).unwrap().molecules[0].atoms[1].label, "Type 2");
        for name in ["POSCAR", "CONTCAR", "x/POSCAR_relaxed", "a.vasp", "b.poscar"] {
            assert_eq!(by_extension(name), Some(Format::Poscar), "{name}");
        }
    }

    #[test]
    fn known_by_content() {
        use super::looks_like;
        assert!(looks_like(NACL));
        assert!(looks_like(&NACL.replace("Na Cl\n", "")));
        assert!(looks_like(&NACL.replace("\n1.0\n", "\n1 1 1\n")));
        assert_eq!(crate::detect(NACL.as_bytes()), Some(Format::Poscar));
        for not in ["a\nb\nc\n", "x\n1.0\n1 0 0\n0 1 0\n0 0\nNa\n1\n", "x\n1.0\n1 0 0\n0 1 0\n0 0 1\nNa\nmany\n", "x\n1.0 2.0\n1 0 0\n0 1 0\n0 0 1\n1\n", "x\n1.0\n1 0 0 4\n0 1 0\n0 0 1\n1\n", "x\nnope\n1 0 0\n0 1 0\n0 0 1\n1\n"] {
            assert!(!looks_like(not), "{not:?}");
        }
    }

    #[test]
    fn damage() {
        for bad in ["x\n1\n1 0\n", "x\n\n1 0 0\n0 1 0\n0 0 1\nNa\n1\nD\n0 0 0\n", "x\n1\n1 0 0\n0 1 0\n0 0 1\nNa\nx\n", "x\n1\n1 0 0\n0 1 0\n0 0 1\nNa\n2\nD\n0 0 0\n", "x\n1\n1 0 0\n0 1 0\n0 0 1\nNa\n1\nD\n0 0\n"] {
            assert!(matches!(read(bad.as_bytes(), Format::Poscar, &Limits::default()), Err(crate::Error::Damaged(_))), "{bad:?}");
        }
        assert!(matches!(read(NACL.as_bytes(), Format::Poscar, &Limits::default().with_atoms(1)), Err(crate::Error::TooLarge { .. })));
    }
}

#[cfg(test)]
mod edges {
    use super::*;
    use crate::{Format, Limits, read as read_any};

    fn det(c: [[f64; 3]; 3]) -> f64 {
        c[0][0] * (c[1][1] * c[2][2] - c[1][2] * c[2][1]) - c[0][1] * (c[1][0] * c[2][2] - c[1][2] * c[2][0]) + c[0][2] * (c[1][0] * c[2][1] - c[1][1] * c[2][0])
    }

    #[test]
    fn a_negative_scale_is_the_volume_of_a_skewed_cell() {
        let text = "skewed\n-60.0\n2 0.5 0.3\n0.4 3 0.2\n0.1 0.6 4\nC\n1\nDirect\n0 0 0\n";
        let m = &read_any(text.as_bytes(), Format::Poscar, &Limits::default()).unwrap().molecules[0];
        assert!((det(m.cell.unwrap()).abs() - 60.0).abs() < 1e-9, "{}", det(m.cell.unwrap()));
    }

    #[test]
    fn cartesian_names_and_scales() {
        // `K` is Cartesian too; three scales, one per axis; the comment is its name.
        // (A cell that isn't the identity, so Cartesian and direct differ.)
        let text = "my crystal\n1 2 3\n2 0 0\n0 2 0\n0 0 2\nC\n1\nKartesian\n1 1 1\n";
        let m = &read_any(text.as_bytes(), Format::Poscar, &Limits::default()).unwrap().molecules[0];
        assert_eq!((m.name.as_str(), m.atoms[0].position), ("my crystal", [1.0, 2.0, 3.0]));
    }

    #[test]
    fn known_by_content_at_its_shortest() {
        assert!(looks_like("x\n1\n1 0 0\n0 1 0\n0 0 1\n2"));
        assert!(!looks_like("x\n1\n1 0 0 x\n0 1 0\n0 0 1\n2"));
        assert!(!looks_like("x\n1\n1 0 0\n0 1 0\n0 0 1"));
    }
}
