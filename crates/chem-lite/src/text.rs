//! Two line-based formats from quantum chemistry: Turbomole's `coord` and Molden.

use crate::{Atom, BOHR, Budget, File, Molecule, Result, damaged, elements, number};

/// Turbomole: `$coord` (in bohr, or `$coord angs`), then `x y z element` lines until the
/// next `$` keyword. `$periodic` and `$cell` or `$lattice` give a crystal's cell.
pub fn turbomole(text: &str, budget: &mut Budget) -> Result<File> {
    let mut m = Molecule { name: "Turbomole coordinates".into(), ..Molecule::default() };
    let mut section = String::new();
    let mut scale = BOHR;
    let mut lattice = Vec::new();
    let mut cell_line: Option<Vec<f64>> = None;
    let mut found = false;
    for line in text.lines() {
        let t = line.trim();
        if let Some(keyword) = t.strip_prefix('$') {
            let mut words = keyword.split_whitespace();
            section = words.next().unwrap_or("").to_string();
            if section == "coord" {
                found = true;
                scale = if words.any(|w| w.starts_with("ang")) { 1.0 } else { BOHR };
            }
            if section == "end" {
                break;
            }
            continue;
        }
        let f: Vec<&str> = t.split_whitespace().collect();
        match section.as_str() {
            "coord" if f.len() >= 4 => {
                let p: Vec<f64> = f[..3].iter().filter_map(|v| number(v)).collect();
                if p.len() != 3 {
                    return Err(damaged(format!("Turbomole: atom {} isn't three numbers and an element", m.atoms.len() + 1)));
                }
                budget.atoms(1)?;
                m.atoms.push(Atom::new(elements::parse(f[3]).unwrap_or(0), f[3], [p[0] * scale, p[1] * scale, p[2] * scale]));
            }
            "lattice" => lattice.extend(f.iter().filter_map(|v| number(v))),
            "cell" => cell_line = Some(f.iter().filter_map(|v| number(v)).collect()),
            _ => {}
        }
    }
    if !found {
        return Err(crate::Error::NotThisFormat);
    }
    if lattice.len() == 9 {
        m.cell = Some([[lattice[0], lattice[1], lattice[2]], [lattice[3], lattice[4], lattice[5]], [lattice[6], lattice[7], lattice[8]]].map(|r| r.map(|v| v * BOHR)));
    } else if let Some(c) = cell_line.filter(|c| c.len() >= 6) {
        m.cell = Some(cell_vectors([c[0] * BOHR, c[1] * BOHR, c[2] * BOHR], [c[3], c[4], c[5]]));
    }
    budget.molecule()?;
    Ok(File { molecules: vec![m], notes: Vec::new() })
}

/// A cell from its lengths and angles (degrees), `a` along x and `b` in the xy plane.
pub fn cell_vectors(len: [f64; 3], angles: [f64; 3]) -> [[f64; 3]; 3] {
    let [al, be, ga] = angles.map(f64::to_radians);
    let (cg, sg) = (ga.cos(), ga.sin().max(1e-12));
    let cx = be.cos();
    let cy = (al.cos() - be.cos() * cg) / sg;
    let cz = (1.0 - cx * cx - cy * cy).max(0.0).sqrt();
    [[len[0], 0.0, 0.0], [len[1] * cg, len[1] * sg, 0.0], [len[2] * cx, len[2] * cy, len[2] * cz]]
}

/// Molden: `[Atoms] Angs` (or `AU`) with `name number Z x y z` lines, or the first of
/// `[GEOMETRIES] XYZ`; `[MO]` orbitals and `[FREQ]` frequencies are counted.
pub fn molden(text: &str, budget: &mut Budget) -> Result<File> {
    let mut file = File::default();
    let mut m = Molecule { name: "Molden".into(), ..Molecule::default() };
    let mut section = String::new();
    let mut scale = 1.0;
    let (mut orbitals, mut frequencies, mut geometry_lines) = (0usize, 0usize, Vec::new());
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            let close = t.find(']').unwrap_or(t.len());
            section = t[1..close].to_ascii_lowercase();
            let unit = t[close.min(t.len())..].to_ascii_lowercase();
            if section == "atoms" {
                scale = if unit.contains("au") || unit.contains("bohr") { BOHR } else { 1.0 };
            }
            if section == "title" {
                continue;
            }
            continue;
        }
        let f: Vec<&str> = t.split_whitespace().collect();
        match section.as_str() {
            "title" if !t.is_empty() && m.name == "Molden" => m.name = t.to_string(),
            "atoms" if f.len() >= 6 => {
                let p: Vec<f64> = f[3..6].iter().filter_map(|v| number(v)).collect();
                if p.len() != 3 {
                    return Err(damaged(format!("Molden: atom {} has no three coordinates", m.atoms.len() + 1)));
                }
                let z = f[2].parse::<u8>().ok().filter(|z| (1..=elements::LAST).contains(z)).or_else(|| elements::parse(f[0])).unwrap_or(0);
                budget.atoms(1)?;
                m.atoms.push(Atom::new(z, f[0], [p[0] * scale, p[1] * scale, p[2] * scale]));
            }
            "geometries" => geometry_lines.push(line),
            "mo" if t.to_ascii_lowercase().starts_with("ene") => orbitals += 1,
            "freq" if number(t).is_some() => frequencies += 1,
            _ => {}
        }
    }
    if m.atoms.is_empty() && !geometry_lines.is_empty() {
        let xyz = geometry_lines.join("\n");
        let first = crate::xyz::read(&xyz, budget)?.molecules.into_iter().next();
        m.atoms = first.map(|f| f.atoms).unwrap_or_default();
    }
    if m.atoms.is_empty() {
        return Err(damaged("Molden: no atoms"));
    }
    budget.molecule()?;
    if orbitals > 0 {
        file.notes.push(("Molecular orbitals".into(), orbitals.to_string()));
    }
    if frequencies > 0 {
        file.notes.push(("Vibrational frequencies".into(), frequencies.to_string()));
    }
    file.molecules.push(m);
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Format, Limits, detect, read};

    #[test]
    fn turbomole_in_bohr_and_angstroms() {
        let text = "$coord\n 0.0 0.0 0.0 o\n 1.8 0.0 0.0 h\n$periodic 3\n$lattice\n 10 0 0\n 0 10 0\n 0 0 10\n$end\n";
        assert_eq!(detect(text.as_bytes()), Some(Format::Turbomole));
        let m = &read(text.as_bytes(), Format::Turbomole, &Limits::default()).unwrap().molecules[0];
        assert_eq!(m.atoms.iter().map(|a| a.element).collect::<Vec<_>>(), [8, 1]);
        assert!((m.atoms[1].position[0] - 1.8 * BOHR).abs() < 1e-9);
        assert!((m.cell.unwrap()[0][0] - 10.0 * BOHR).abs() < 1e-9);
        let angs = "$coord angs\n0 0 0 c\n1.5 0 0 c\n$cell\n 10 10 10 90 90 90\n$end";
        let m = &read(angs.as_bytes(), Format::Turbomole, &Limits::default()).unwrap().molecules[0];
        assert_eq!((m.atoms[1].position[0], m.bonds.len()), (1.5, 1));
        let c = m.cell.unwrap();
        assert!((c[1][1] - 10.0 * BOHR).abs() < 1e-9 && c[1][0].abs() < 1e-9 && (c[2][2] - 10.0 * BOHR).abs() < 1e-9);
        assert_eq!(read(b"no coords\n", Format::Turbomole, &Limits::default()), Err(crate::Error::NotThisFormat));
        assert!(matches!(read(b"$coord\n x 0 0 c\n", Format::Turbomole, &Limits::default()), Err(crate::Error::Damaged(_))));
    }

    #[test]
    fn molden_atoms_and_geometries() {
        let text = "[Molden Format]\n[Title]\n water\n[Atoms] AU\nO 1 8 0 0 0\nH 2 1 1.8 0 0\nH 3 1 0 1.8 0\n[MO]\n Ene= -20.5\n Spin= Alpha\n Ene= -1.3\n[FREQ]\n1600.1\n3700.2\n";
        assert_eq!(detect(text.as_bytes()), Some(Format::Molden));
        let f = read(text.as_bytes(), Format::Molden, &Limits::default()).unwrap();
        let m = &f.molecules[0];
        assert_eq!((m.name.as_str(), m.atoms.len(), m.bonds.len()), ("water", 3, 2));
        assert!((m.atoms[1].position[0] - 1.8 * BOHR).abs() < 1e-9);
        assert_eq!(f.notes, [("Molecular orbitals".to_string(), "2".to_string()), ("Vibrational frequencies".to_string(), "2".to_string())]);
        let geo = "[Molden Format]\n[GEOMETRIES] XYZ\n2\n\nC 0 0 0\nO 1.2 0 0\n2\n\nC 0 0 0\nO 1.3 0 0\n";
        assert_eq!(read(geo.as_bytes(), Format::Molden, &Limits::default()).unwrap().molecules[0].atoms[1].position[0], 1.2);
        // Angstroms by default; an element from its name when its number isn't one.
        let m = &read(b"[Molden Format]\n[Atoms] Angs\nFe1 1 0 0 0 0\n", Format::Molden, &Limits::default()).unwrap().molecules[0];
        assert_eq!(m.atoms[0].element, 26);
        assert!(matches!(read(b"[Molden Format]\n", Format::Molden, &Limits::default()), Err(crate::Error::Damaged(_))));
        assert!(matches!(read(b"[Atoms] Angs\nC 1 6 0 x 0\n", Format::Molden, &Limits::default()), Err(crate::Error::Damaged(_))));
    }

    #[test]
    fn cells_from_angles() {
        let c = cell_vectors([2.0, 3.0, 4.0], [90.0, 90.0, 120.0]);
        assert!((c[1][0] + 1.5).abs() < 1e-9 && (c[1][1] - 3.0 * 0.75f64.sqrt()).abs() < 1e-9 && (c[2][2] - 4.0).abs() < 1e-9);
    }
}
