//! CIF and mmCIF: `data_` blocks of `_tag value` pairs and `loop_` tables. A block's
//! `_atom_site` loop is its atoms: Cartesian (`Cartn_x`, as mmCIF has them) or
//! fractional (`fract_x`, in the cell `_cell_length_a`… gives). Each block is a
//! structure; in mmCIF, the first model's atoms.

use crate::{Atom, Budget, File, Molecule, Result, damaged, elements, number};

/// Values, as tokens: bare words, quoted (closed by a quote before white space), and
/// `;`-fields to a line that starts with `;`.
fn tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        if let Some(first) = line.strip_prefix(';') {
            let mut v = first.to_string();
            for l in lines.by_ref() {
                if l.starts_with(';') {
                    break;
                }
                v.push('\n');
                v.push_str(l);
            }
            out.push(v.trim().to_string());
            continue;
        }
        let b = line.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if b[i].is_ascii_whitespace() {
                i += 1;
                continue;
            }
            if b[i] == b'#' {
                break;
            }
            if b[i] == b'\'' || b[i] == b'"' {
                let q = b[i];
                let mut j = i + 1;
                while j < b.len() && !(b[j] == q && (j + 1 == b.len() || b[j + 1].is_ascii_whitespace())) {
                    j += 1;
                }
                out.push(line[i + 1..j.min(line.len())].to_string());
                i = j + 1;
                continue;
            }
            let start = i;
            while i < b.len() && !b[i].is_ascii_whitespace() {
                i += 1;
            }
            out.push(line[start..i].to_string());
        }
    }
    out
}

/// A number with its uncertainty (`1.234(5)`) taken off; `?` and `.` are none.
fn value(s: &str) -> Option<f64> {
    number(s.split('(').next().unwrap_or(s))
}

/// One block: its single values and its loops (column names, then rows).
#[derive(Default)]
struct Block {
    name: String,
    items: Vec<(String, String)>,
    loops: Vec<(Vec<String>, Vec<Vec<String>>)>,
}

fn blocks(text: &str) -> Vec<Block> {
    let t = tokens(text);
    let mut out: Vec<Block> = Vec::new();
    let mut i = 0;
    while i < t.len() {
        let tok = &t[i];
        if let Some(name) = tok.strip_prefix("data_") {
            out.push(Block { name: name.to_string(), ..Block::default() });
            i += 1;
        } else if tok.eq_ignore_ascii_case("loop_") {
            i += 1;
            let mut cols = Vec::new();
            while i < t.len() && t[i].starts_with('_') {
                cols.push(t[i].to_ascii_lowercase());
                i += 1;
            }
            let mut values = Vec::new();
            while i < t.len() && !t[i].starts_with('_') && !t[i].eq_ignore_ascii_case("loop_") && !t[i].starts_with("data_") {
                values.push(t[i].clone());
                i += 1;
            }
            if !cols.is_empty() {
                let rows = values.chunks_exact(cols.len()).map(<[String]>::to_vec).collect();
                out.last_mut().get_or_insert_with(|| unreachable!()).loops.push((cols, rows));
            }
        } else if tok.starts_with('_') {
            let v = t.get(i + 1).cloned().unwrap_or_default();
            if let Some(b) = out.last_mut() {
                b.items.push((tok.to_ascii_lowercase(), v));
            }
            i += 2;
        } else {
            i += 1;
        }
    }
    out
}

pub fn read(text: &str, budget: &mut Budget) -> Result<File> {
    if !text.trim_start().starts_with("data_") && !text.lines().any(|l| l.starts_with("data_")) {
        return Err(damaged("CIF: no data block"));
    }
    let mut file = File::default();
    for block in blocks(text) {
        let item = |k: &str| block.items.iter().find(|i| i.0 == k).and_then(|i| value(&i.1));
        let cell = match ["_cell_length_a", "_cell_length_b", "_cell_length_c", "_cell_angle_alpha", "_cell_angle_beta", "_cell_angle_gamma"].map(item) {
            [Some(a), Some(b), Some(c), al, be, ga] if a > 1.0 => Some(crate::text::cell_vectors([a, b, c], [al.unwrap_or(90.0), be.unwrap_or(90.0), ga.unwrap_or(90.0)])),
            _ => None,
        };
        let Some((cols, rows)) = block.loops.iter().find(|(c, _)| c.iter().any(|c| c.starts_with("_atom_site.") || c.starts_with("_atom_site_"))) else { continue };
        let find = |names: &[&str]| names.iter().find_map(|n| cols.iter().position(|c| c == n));
        let x = (find(&["_atom_site.cartn_x", "_atom_site_cartn_x"]), find(&["_atom_site.cartn_y", "_atom_site_cartn_y"]), find(&["_atom_site.cartn_z", "_atom_site_cartn_z"]));
        let f = (find(&["_atom_site.fract_x", "_atom_site_fract_x"]), find(&["_atom_site.fract_y", "_atom_site_fract_y"]), find(&["_atom_site.fract_z", "_atom_site_fract_z"]));
        let symbol = find(&["_atom_site.type_symbol", "_atom_site_type_symbol"]);
        let label = find(&["_atom_site.label_atom_id", "_atom_site_label", "_atom_site.auth_atom_id"]);
        let model = find(&["_atom_site.pdbx_pdb_model_num"]);
        let first_model = model.and_then(|m| rows.first().map(|r| r[m].clone()));
        budget.molecule()?;
        let mut m = Molecule { name: block.name.clone(), cell, ..Molecule::default() };
        for row in rows {
            if let (Some(k), Some(first)) = (model, &first_model)
                && &row[k] != first
            {
                continue;
            }
            let get = |c: Option<usize>| c.and_then(|c| value(&row[c]));
            let position = match (x, f, cell) {
                ((Some(_), Some(_), Some(_)), _, _) => match (get(x.0), get(x.1), get(x.2)) {
                    (Some(a), Some(b), Some(c)) => [a, b, c],
                    _ => continue,
                },
                (_, (Some(_), Some(_), Some(_)), Some(cell)) => match (get(f.0), get(f.1), get(f.2)) {
                    (Some(a), Some(b), Some(c)) => [0, 1, 2].map(|k| a * cell[0][k] + b * cell[1][k] + c * cell[2][k]),
                    _ => continue,
                },
                _ => return Err(damaged(format!("CIF: block {} has atoms but no coordinates it can place", block.name))),
            };
            budget.atoms(1)?;
            let label = label.map_or("", |l| row[l].as_str());
            // `Fe2+` and `O2-` are elements with their charge.
            let sym = symbol.map(|s| row[s].trim_end_matches(['+', '-']).trim_end_matches(|c: char| c.is_ascii_digit()).to_string());
            let element = sym.as_deref().and_then(elements::by_symbol).or_else(|| elements::parse(label)).unwrap_or(0);
            m.atoms.push(Atom::new(element, label, position));
        }
        if !m.atoms.is_empty() {
            file.molecules.push(m);
        }
    }
    if file.molecules.len() > 1 {
        file.notes.push(("Structures".into(), file.molecules.len().to_string()));
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Format, Limits, read as read_any};

    #[test]
    fn a_small_molecule_crystal() {
        let text = "data_nacl\n_cell_length_a 5.64(1)\n_cell_length_b 5.64\n_cell_length_c 5.64\n_cell_angle_alpha 90\n_cell_angle_beta 90\n_cell_angle_gamma 90\n_journal 'a ''quoted'' title'\nloop_\n_atom_site_label\n_atom_site_type_symbol\n_atom_site_fract_x\n_atom_site_fract_y\n_atom_site_fract_z\nNa1 Na+ 0 0 0\nCl1 Cl- 0.5 0.5 0.5\nX1 ? ? ? ?\n";
        let f = read_any(text.as_bytes(), Format::Cif, &Limits::default()).unwrap();
        let m = &f.molecules[0];
        assert_eq!(m.name, "nacl");
        assert_eq!(m.atoms.iter().map(|a| (a.element, a.label.as_str())).collect::<Vec<_>>(), [(11, "Na1"), (17, "Cl1")]);
        assert!((m.atoms[1].position[1] - 2.82).abs() < 1e-9);
    }

    #[test]
    fn mmcif_first_model_and_several_blocks() {
        let text = "data_1ABC\nloop_\n_atom_site.group_PDB\n_atom_site.type_symbol\n_atom_site.label_atom_id\n_atom_site.Cartn_x\n_atom_site.Cartn_y\n_atom_site.Cartn_z\n_atom_site.pdbx_PDB_model_num\nATOM C CA 0 0 0 1\nATOM O O 1.2 0 0 1\nATOM C CA 9 9 9 2\n#\ndata_two\n;\nlong text\n;\nloop_\n_atom_site.type_symbol\n_atom_site.Cartn_x\n_atom_site.Cartn_y\n_atom_site.Cartn_z\nN 0 0 0\n";
        let f = read_any(text.as_bytes(), Format::Cif, &Limits::default()).unwrap();
        assert_eq!(f.molecules.iter().map(|m| (m.name.as_str(), m.atoms.len())).collect::<Vec<_>>(), [("1ABC", 2), ("two", 1)]);
        assert_eq!(f.molecules[0].bonds.len(), 1);
        assert_eq!(f.notes[0], ("Structures".to_string(), "2".to_string()));
        assert!(matches!(read_any(b"no data\n", Format::Cif, &Limits::default()), Err(crate::Error::Damaged(_))));
        assert!(matches!(read_any(b"data_x\nloop_\n_atom_site_label\n_atom_site_fract_x\nC 0\n", Format::Cif, &Limits::default()), Err(crate::Error::Damaged(_))));
        assert!(matches!(read_any(b"data_x\n_cell_length_a 2\n", Format::Cif, &Limits::default()), Err(crate::Error::Damaged(_))));
    }

    #[test]
    fn tokens_as_cif_writes_them() {
        assert_eq!(tokens("a 'b c' \"d'e\" # comment\n;\nx\ny\n;\nf"), ["a", "b c", "d'e", "x\ny", "f"]);
        assert_eq!((value("1.5(3)"), value("?"), value(".")), (Some(1.5), None, None));
    }
}
