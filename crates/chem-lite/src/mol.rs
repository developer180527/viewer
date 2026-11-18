//! MDL molfiles and SD files: a molecule's name and two lines about it, a counts line,
//! then its atoms and bonds (V2000, in fixed columns), or a `V30` block of them
//! (V3000). An SD file is molfiles one after another, each with data fields (`> <MW>`)
//! and ended by `$$$$`.

use crate::{Atom, Bond, Budget, File, Molecule, Result, damaged, elements, number};

fn is_counts(l: &str) -> bool {
    let l = l.trim_end();
    l.ends_with("V2000") || l.ends_with("V3000")
}

/// A counts line naming its version, fourth (or, where a writer left out the name line,
/// third).
pub fn looks_like(text: &str) -> bool {
    text.lines().take(4).skip(2).any(is_counts)
}

/// V2000's charge field, 1 to 7 for +3 to −3, 4 a radical.
fn charge(code: i32) -> i8 {
    match code {
        1..=3 => (4 - code) as i8,
        5..=7 => (4 - code) as i8,
        _ => 0,
    }
}

fn field(line: &str, from: usize, to: usize) -> &str {
    line.get(from..to.min(line.len())).unwrap_or("").trim()
}

/// One record's lines, from its name to `M  END` (or the record's end).
fn record(lines: &[&str], budget: &mut Budget) -> Result<Molecule> {
    // The counts line is fourth, after the name and two lines about the molecule; some
    // writers leave one of those out.
    let at = if lines.get(3).is_some_and(|l| is_counts(l)) || !lines.get(2).is_some_and(|l| is_counts(l)) { 3 } else { 2 };
    let mut m = Molecule { name: if at == 3 { lines.first().map_or("", |l| l.trim()).to_string() } else { String::new() }, ..Molecule::default() };
    // Without its name line, one is put back so the counts line is fourth as usual.
    let restored: Vec<&str>;
    let lines = if at == 2 {
        restored = std::iter::once("").chain(lines.iter().copied()).collect();
        &restored[..]
    } else {
        lines
    };
    let counts = lines.get(3).ok_or_else(|| damaged("MOL: no counts line"))?;
    if counts.trim_end().ends_with("V3000") {
        return v3000(lines, m, budget);
    }
    let (atoms, bonds) = (field(counts, 0, 3).parse::<usize>(), field(counts, 3, 6).parse::<usize>());
    let (Ok(atoms), Ok(bonds)) = (atoms, bonds) else { return Err(damaged("MOL: a counts line without counts")) };
    budget.atoms(atoms as u64)?;
    let body = &lines[4..];
    if body.len() < atoms + bonds {
        return Err(damaged(format!("MOL: {atoms} atoms and {bonds} bonds, but {} lines", body.len())));
    }
    for (i, l) in body[..atoms].iter().enumerate() {
        let f: Vec<&str> = l.split_whitespace().collect();
        let p: Vec<f64> = f.iter().take(3).filter_map(|v| number(v)).collect();
        if p.len() != 3 || f.len() < 4 {
            return Err(damaged(format!("MOL: atom {} isn't three coordinates and an element", i + 1)));
        }
        let mut a = Atom::new(elements::by_symbol(f[3]).unwrap_or(0), f[3], [p[0], p[1], p[2]]);
        a.charge = charge(field(l, 36, 39).parse().unwrap_or(0));
        m.atoms.push(a);
    }
    for l in &body[atoms..atoms + bonds] {
        let (a, b, t) = (field(l, 0, 3).parse::<u32>(), field(l, 3, 6).parse::<u32>(), field(l, 6, 9).parse::<u8>());
        if let (Ok(a), Ok(b)) = (a, b)
            && (1..=atoms as u32).contains(&a)
            && (1..=atoms as u32).contains(&b)
        {
            m.bonds.push(Bond::new(a - 1, b - 1, t.ok().filter(|t| *t <= 4).unwrap_or(0)));
        }
    }
    // `M  CHG n aaa vvv …` replaces the charges in the atom block.
    for l in &body[atoms + bonds..] {
        if let Some(rest) = l.strip_prefix("M  CHG") {
            let v: Vec<i64> = rest.split_whitespace().filter_map(|x| x.parse().ok()).collect();
            for pair in v.get(1..).unwrap_or(&[]).chunks(2) {
                if let [a, c] = pair
                    && let Some(atom) = (*a as usize).checked_sub(1).and_then(|i| m.atoms.get_mut(i))
                {
                    atom.charge = (*c).clamp(-128, 127) as i8;
                }
            }
        }
        if l.starts_with("M  END") {
            break;
        }
    }
    Ok(m)
}

/// A V3000 line's values: words, or `"…"` with spaces in it (`"NOT [O]"`, an atom that
/// is a query), `""` a quote within.
fn words(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = line.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else if c == '"' {
            chars.next();
            let mut w = String::new();
            while let Some(c) = chars.next() {
                if c == '"' {
                    if chars.peek() == Some(&'"') {
                        chars.next();
                        w.push('"');
                    } else {
                        break;
                    }
                } else {
                    w.push(c);
                }
            }
            out.push(w);
        } else {
            let mut w = String::new();
            while let Some(&c) = chars.peek().filter(|c| !c.is_whitespace()) {
                w.push(c);
                chars.next();
            }
            out.push(w);
        }
    }
    out
}

fn v3000(lines: &[&str], mut m: Molecule, budget: &mut Budget) -> Result<Molecule> {
    // `M  V30 ` lines, a trailing `-` joining one to the next.
    let mut joined: Vec<String> = Vec::new();
    let mut open = false;
    for l in lines {
        let Some(rest) = l.strip_prefix("M  V30 ") else { continue };
        let (text, more) = match rest.trim_end().strip_suffix('-') {
            Some(t) => (t, true),
            None => (rest.trim_end(), false),
        };
        match joined.last_mut() {
            Some(last) if open => last.push_str(text),
            _ => joined.push(text.to_string()),
        }
        open = more;
    }
    let mut section = String::new();
    for l in &joined {
        let f = words(l);
        let f: Vec<&str> = f.iter().map(String::as_str).collect();
        match f.as_slice() {
            ["BEGIN", s, ..] => section = s.to_string(),
            ["END", ..] => section.clear(),
            ["COUNTS", atoms, ..] => budget.atoms(atoms.parse().unwrap_or(0))?,
            _ if section == "ATOM" && f.len() >= 5 => {
                let p: Vec<f64> = f[2..5].iter().filter_map(|v| number(v)).collect();
                if p.len() != 3 {
                    return Err(damaged(format!("MOL V3000: atom {} has no three coordinates", f[0])));
                }
                let mut a = Atom::new(elements::by_symbol(f[1]).unwrap_or(0), f[1], [p[0], p[1], p[2]]);
                a.charge = f.iter().find_map(|x| x.strip_prefix("CHG=")).and_then(|c| c.parse::<i8>().ok()).unwrap_or(0);
                m.atoms.push(a);
            }
            _ if section == "BOND" && f.len() >= 4 => {
                let n = m.atoms.len() as u32;
                if let (Ok(t), Ok(a), Ok(b)) = (f[1].parse::<u8>(), f[2].parse::<u32>(), f[3].parse::<u32>())
                    && (1..=n).contains(&a)
                    && (1..=n).contains(&b)
                {
                    m.bonds.push(Bond::new(a - 1, b - 1, if t <= 4 { t } else { 0 }));
                }
            }
            _ => {}
        }
    }
    Ok(m)
}

/// A molfile, or an SD file's records. A record that's damaged is skipped and counted,
/// as long as others aren't; a file of nothing else is damaged, with the first error.
pub fn read(text: &str, budget: &mut Budget) -> Result<File> {
    let lines: Vec<&str> = text.lines().collect();
    let mut file = File::default();
    let (mut skipped, mut first_error) = (0usize, None);
    let mut start = 0;
    while start < lines.len() {
        let end = lines[start..].iter().position(|l| l.trim_end() == "$$$$").map_or(lines.len(), |p| start + p);
        let chunk = &lines[start..end];
        if chunk.iter().any(|l| !l.trim().is_empty()) {
            budget.molecule()?;
            let mut m = match record(chunk, budget) {
                Ok(m) => m,
                Err(e @ crate::Error::TooLarge { .. }) => return Err(e),
                Err(e) => {
                    skipped += 1;
                    first_error.get_or_insert(e);
                    start = end + 1;
                    continue;
                }
            };
            // An SD file's data fields: `> <NAME>` (or `>  <NAME>  (1)`), then lines to a
            // blank one.
            let after = chunk.iter().position(|l| l.starts_with("M  END")).map_or(chunk.len(), |p| p + 1);
            let mut i = after;
            while i < chunk.len() {
                if let Some(name) = chunk[i].strip_prefix('>').and_then(|h| Some(h.split_once('<')?.1.split_once('>')?.0)) {
                    let mut value = Vec::new();
                    i += 1;
                    while i < chunk.len() && !chunk[i].trim().is_empty() {
                        value.push(chunk[i].trim_end());
                        i += 1;
                    }
                    m.properties.push((name.to_string(), value.join("\n")));
                }
                i += 1;
            }
            file.molecules.push(m);
        }
        start = end + 1;
    }
    if file.molecules.is_empty()
        && let Some(e) = first_error
    {
        return Err(e);
    }
    if file.molecules.len() > 1 {
        file.notes.push(("Records".into(), file.molecules.len().to_string()));
    }
    if skipped > 0 {
        file.notes.push(("Damaged records, skipped".into(), skipped.to_string()));
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use crate::{Format, Limits, read};

    const ETHANOL: &str = "ethanol
  test 3D

  3  2  0  0  0  0  0  0  0  0999 V2000
   -0.0187    1.5258    0.0104 C   0  0  0  0  0  0  0  0  0  0  0  0
    0.0021   -0.0041    0.0020 C   0  0  0  0  0  0  0  0  0  0  0  0
    1.2540   -0.5100    0.0300 O   0  5  0  0  0  0  0  0  0  0  0  0
  1  2  1  0  0  0  0
  2  3  1  0  0  0  0
M  END
";

    #[test]
    fn a_molfile_and_an_sd_file() {
        assert!(super::looks_like(ETHANOL) && !super::looks_like("a\nb\nc\nd\n"));
        let f = read(ETHANOL.as_bytes(), Format::Mol, &Limits::default()).unwrap();
        let m = &f.molecules[0];
        assert_eq!((m.name.as_str(), m.atoms.len(), m.bonds.len(), m.bonds_found), ("ethanol", 3, 2, false));
        assert_eq!((m.atoms[2].element, m.atoms[2].charge, m.atoms[0].position[1]), (8, -1, 1.5258));
        let sdf = format!("{ETHANOL}> <MW>\n46.07\n\n>  <NOTE>  (1)\ntwo\nlines\n\n$$$$\n{}M  CHG  1   1   1\nM  END\n$$$$\n", ETHANOL.trim_end_matches("M  END\n"));
        let f = read(sdf.as_bytes(), Format::Mol, &Limits::default()).unwrap();
        assert_eq!(f.molecules.len(), 2);
        assert_eq!(f.notes, [("Records".to_string(), "2".to_string())]);
        assert_eq!(f.molecules[0].properties, [("MW".to_string(), "46.07".to_string()), ("NOTE".to_string(), "two\nlines".to_string())]);
        // M  CHG sets the first atom's charge, and others' from the atom block go.
        assert_eq!(f.molecules[1].atoms.iter().map(|a| a.charge).collect::<Vec<_>>(), [1, 0, -1]);
        // A damaged record among good ones is skipped and counted (RDKit's tests have one).
        let bad = format!("{ETHANOL}$$$$\n{}$$$$\n{ETHANOL}", ETHANOL.replace("  3  2  0", "foo  2  0"));
        let f = read(bad.as_bytes(), Format::Mol, &Limits::default()).unwrap();
        assert_eq!(f.molecules.len(), 2);
        assert!(f.notes.contains(&("Damaged records, skipped".to_string(), "1".to_string())));
        assert!(matches!(read(b"x\n\n\n  5  0\n", Format::Mol, &Limits::default()), Err(crate::Error::Damaged(_))));
        assert!(matches!(read(b"x\n\n\nnope\n", Format::Mol, &Limits::default()), Err(crate::Error::Damaged(_))));
        assert!(matches!(read(b"x\n\n", Format::Mol, &Limits::default()), Err(crate::Error::Damaged(_))));
    }

    #[test]
    fn v3000() {
        let text = "benzene-ish\n\n\n  0  0  0     0  0            999 V3000\nM  V30 BEGIN CTAB\nM  V30 COUNTS 3 2 0 0 0\nM  V30 BEGIN ATOM\nM  V30 1 C 0 0 0 0\nM  V30 2 C 1.4 0 0 0 -\nM  V30 CHG=1\nM  V30 3 N 2.8 0 0 0\nM  V30 END ATOM\nM  V30 BEGIN BOND\nM  V30 1 4 1 2\nM  V30 2 2 2 3\nM  V30 3 1 2 9\nM  V30 END BOND\nM  V30 END CTAB\nM  END\n";
        let m = &read(text.as_bytes(), Format::Mol, &Limits::default()).unwrap().molecules[0];
        assert_eq!(m.atoms.iter().map(|a| (a.element, a.charge)).collect::<Vec<_>>(), [(6, 0), (6, 1), (7, 0)]);
        assert_eq!(m.bonds.iter().map(|b| (b.a, b.b, b.order)).collect::<Vec<_>>(), [(0, 1, 4), (1, 2, 2)]);
        assert!(matches!(read(text.replace("COUNTS 3", "COUNTS 300").as_bytes(), Format::Mol, &Limits::default().with_atoms(10)), Err(crate::Error::TooLarge { .. })));
        assert!(matches!(read(text.replace("2 C 1.4", "2 C x").as_bytes(), Format::Mol, &Limits::default()), Err(crate::Error::Damaged(_))));
        // A query atom, quoted, with spaces in it (from RDKit's tests).
        let query = text.replace("M  V30 3 N 2.8", "M  V30 3 \"NOT [O]\" 2.8");
        let m = &read(query.as_bytes(), Format::Mol, &Limits::default()).unwrap().molecules[0];
        assert_eq!((m.atoms[2].element, m.atoms[2].label.as_str(), m.atoms[2].position[0]), (0, "NOT [O]", 2.8));
        assert_eq!(super::words(r#"a "b c" "say ""hi""" d"#), ["a", "b c", "say \"hi\"", "d"]);
    }

    #[test]
    fn a_molfile_without_its_name_line() {
        // Marvin writes this: the program line first.
        let headless = ETHANOL.split_once('\n').unwrap().1;
        assert!(super::looks_like(headless));
        let m = &read(headless.as_bytes(), Format::Mol, &Limits::default()).unwrap().molecules[0];
        assert_eq!((m.name.as_str(), m.atoms.len(), m.bonds.len()), ("", 3, 2));
    }
}

#[cfg(test)]
mod edges {
    use super::*;
    use crate::{Format, Limits, read as read_any};

    const ETHANOL: &str = "ethanol\n  test 3D\n\n  3  2  0  0  0  0  0  0  0  0999 V2000\n   -0.0187    1.5258    0.0104 C   0  0\n    0.0021   -0.0041    0.0020 C   0  0\n    1.2540   -0.5100    0.0300 O   0  0\n  1  2  1  0  0  0  0\n  2  3  2  0  0  0  0\nM  END\n";

    fn mol(text: &str) -> crate::Result<Molecule> {
        read_any(text.as_bytes(), Format::Mol, &Limits::default()).map(|f| f.molecules.into_iter().next().unwrap())
    }

    #[test]
    fn charge_codes() {
        assert_eq!([0, 1, 2, 3, 4, 5, 6, 7, 8].map(charge), [0, 3, 2, 1, 0, -1, -2, -3, 0]);
    }

    #[test]
    fn bonds_where_the_file_puts_them() {
        let m = mol(ETHANOL).unwrap();
        assert_eq!(m.bonds.iter().map(|b| (b.a, b.b, b.order)).collect::<Vec<_>>(), [(0, 1, 1), (1, 2, 2)]);
        // Two atoms and one bond; more bonds than atoms (tetrahedrane's carbons).
        let pair = "x\n\n\n  2  1  0  0  0  0  0  0  0  0999 V2000\n    0.0 0.0 0.0 C\n    1.5 0.0 0.0 C\n  1  2  1\nM  END\n";
        assert_eq!(mol(pair).unwrap().bonds.len(), 1);
        let tetra = "x\n\n\n  4  6  0  0  0  0  0  0  0  0999 V2000\n    0 0 0 C\n    1.5 0 0 C\n    0.75 1.3 0 C\n    0.75 0.43 1.2 C\n  1  2  1\n  1  3  1\n  1  4  1\n  2  3  1\n  2  4  1\n  3  4  1\nM  END\n";
        assert_eq!(mol(tetra).unwrap().bonds.len(), 6);
    }

    #[test]
    fn what_a_molfile_may_leave_out() {
        // No `V2000` (files before it was written), no `M  END`, atoms of four fields.
        let old = ETHANOL.replace(" V2000", "");
        assert_eq!(mol(&old).unwrap().atoms.len(), 3);
        let unended = ETHANOL.replace("M  END\n", "");
        assert_eq!(mol(&unended).unwrap().bonds.len(), 2);
        assert_eq!(mol("x\n\n\n  1  0\n    0.0 0.0 0.0 C\n").unwrap().atoms[0].element, 6);
        // But not an element, or the lines it counts.
        assert!(matches!(mol("x\n\n\n  1  0\n    0.0 0.0 0.0\n"), Err(crate::Error::Damaged(_))));
        assert!(matches!(mol("x\n\n\n  1  2\n    0.0 0.0 0.0 C\n"), Err(crate::Error::Damaged(_))));
        assert!(read_any(ETHANOL.as_bytes(), Format::Mol, &Limits::default()).unwrap().notes.is_empty());
    }

    #[test]
    fn sd_fields_and_endings() {
        // A field as the file's last lines, with no blank line or `$$$$` after it; blank
        // lines after the last record.
        let f = read_any(format!("{ETHANOL}> <MW>\n46").as_bytes(), Format::Mol, &Limits::default()).unwrap();
        assert_eq!(f.molecules[0].properties, [("MW".to_string(), "46".to_string())]);
        let f = read_any(format!("{ETHANOL}$$$$\n\n\n").as_bytes(), Format::Mol, &Limits::default()).unwrap();
        assert!(f.notes.is_empty() && f.molecules.len() == 1, "{:?}", f.notes);
    }

    #[test]
    fn v3000_blocks_besides_atoms_and_bonds() {
        let text = "x\n\n\n  0  0  0     0  0            999 V3000\nM  V30 BEGIN CTAB\nM  V30 COUNTS 2 1 0 0 0\nM  V30 BEGIN ATOM\nM  V30 1 C 0 0 0 0\nM  V30 2 C 1.5 0 0 0\nM  V30 END ATOM\nM  V30 7 C 9 9 9 0\nM  V30 BEGIN BOND\nM  V30 1 1 1 2 CFG=1\nM  V30 END BOND\nM  V30 BEGIN SGROUP\nM  V30 1 SUP 0\nM  V30 2 1 1 2\nM  V30 END SGROUP\nM  V30 END CTAB\nM  END\n";
        let m = mol(text).unwrap();
        assert_eq!((m.atoms.len(), m.bonds.len()), (2, 1));
    }
}
