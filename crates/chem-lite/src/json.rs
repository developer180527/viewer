//! Chemical JSON (Avogadro's `.cjson`) and MolSSI's QCSchema.

use serde_json::Value;

use crate::{Atom, BOHR, Bond, Budget, File, Molecule, Result, damaged, elements};

fn parse(text: &str) -> Result<Value> {
    serde_json::from_str(text).map_err(|e| damaged(format!("JSON: {e}")))
}

fn numbers(v: Option<&Value>) -> Vec<f64> {
    v.and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_f64).collect()).unwrap_or_default()
}

fn text(v: Option<&Value>) -> Option<String> {
    v.and_then(|v| match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    })
}

/// Chemical JSON: `atoms.elements.number` (or `.symbol`), `atoms.coords.3d` (Å) or
/// `.3dFractional` with `unitCell`; `bonds.connections.index` in pairs and
/// `bonds.order`. Its first version spelled some keys out (`"unit cell"`, `"3d
/// fractional"`), as Avogadro 1 wrote them: those are read too.
pub fn chemical_json(text_in: &str, budget: &mut Budget) -> Result<File> {
    let mut v = parse(text_in)?;
    respell(&mut v);
    let atoms = v.get("atoms").ok_or_else(|| damaged("Chemical JSON: no atoms"))?;
    let els = atoms.get("elements");
    let mut zs: Vec<u8> = numbers(els.and_then(|e| e.get("number"))).iter().map(|z| *z as u8).collect();
    if zs.is_empty() {
        zs = els.and_then(|e| e.get("symbol")).and_then(Value::as_array).map(|a| a.iter().map(|s| s.as_str().and_then(elements::by_symbol).unwrap_or(0)).collect()).unwrap_or_default();
    }
    budget.atoms(zs.len() as u64)?;
    budget.molecule()?;
    let cell = v.get("unitCell").and_then(|c| {
        let vectors = numbers(c.get("cellVectors"));
        if vectors.len() == 9 {
            return Some([[vectors[0], vectors[1], vectors[2]], [vectors[3], vectors[4], vectors[5]], [vectors[6], vectors[7], vectors[8]]]);
        }
        let get = |k: &str| c.get(k).and_then(Value::as_f64);
        Some(crate::text::cell_vectors([get("a")?, get("b")?, get("c")?], [get("alpha")?, get("beta")?, get("gamma")?]))
    });
    let coords = atoms.get("coords");
    let cartesian = numbers(coords.and_then(|c| c.get("3d")));
    let fractional = numbers(coords.and_then(|c| c.get("3dFractional")));
    let positions: Vec<[f64; 3]> = if cartesian.len() >= zs.len() * 3 {
        cartesian.as_chunks::<3>().0.to_vec()
    } else if let (Some(c), true) = (cell, fractional.len() >= zs.len() * 3) {
        fractional.as_chunks::<3>().0.iter().map(|f| [0, 1, 2].map(|k| f[0] * c[0][k] + f[1] * c[1][k] + f[2] * c[2][k])).collect()
    } else {
        return Err(damaged("Chemical JSON: no coordinates for every atom"));
    };
    let charges = numbers(atoms.get("formalCharges"));
    let labels = atoms.get("labels").and_then(Value::as_array);
    let mut m = Molecule { name: text(v.get("name")).unwrap_or_else(|| "Chemical JSON".into()), cell, ..Molecule::default() };
    for (i, z) in zs.iter().enumerate() {
        let label = labels.and_then(|l| l.get(i)).and_then(Value::as_str).map_or_else(|| elements::symbol(*z).to_string(), str::to_string);
        let mut a = Atom::new(*z, label, positions[i]);
        a.charge = charges.get(i).map_or(0, |c| *c as i8);
        m.atoms.push(a);
    }
    let pairs = numbers(v.pointer("/bonds/connections/index"));
    let orders = numbers(v.pointer("/bonds/order"));
    for (k, p) in pairs.chunks(2).enumerate() {
        if let [a, b] = p
            && (*a as usize) < zs.len()
            && (*b as usize) < zs.len()
        {
            m.bonds.push(Bond::new(*a as u32, *b as u32, orders.get(k).map_or(1, |o| *o as u8)));
        }
    }
    if let Some(props) = v.get("properties").and_then(Value::as_object) {
        m.properties.extend(props.iter().filter_map(|(k, v)| Some((k.clone(), text(Some(v))?))));
    }
    Ok(File { molecules: vec![m], notes: Vec::new() })
}

/// The first version's keys as the current ones: `"unit cell"` is `"unitCell"`.
fn respell(v: &mut Value) {
    match v {
        Value::Object(map) => {
            for (old, new) in [("unit cell", "unitCell"), ("3d fractional", "3dFractional"), ("cell vectors", "cellVectors"), ("formal charges", "formalCharges"), ("chemical json", "chemicalJson")] {
                if !map.contains_key(new)
                    && let Some(x) = map.remove(old)
                {
                    map.insert(new.into(), x);
                }
            }
            map.values_mut().for_each(respell);
        }
        Value::Array(items) => items.iter_mut().for_each(respell),
        _ => {}
    }
}

/// QCSchema: a molecule, at the top or under `molecule` (in an input or output): its
/// `symbols`, `geometry` (in bohr, flat) and `connectivity` (`[a, b, order]`).
pub fn qcschema(text_in: &str, budget: &mut Budget) -> Result<File> {
    let v = parse(text_in)?;
    let mol = if v.get("symbols").is_some() { &v } else { v.get("molecule").ok_or_else(|| damaged("QCSchema: no molecule"))? };
    let symbols: Vec<String> = mol.get("symbols").and_then(Value::as_array).map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect()).unwrap_or_default();
    let geometry = numbers(mol.get("geometry"));
    if symbols.is_empty() || geometry.len() < symbols.len() * 3 {
        return Err(damaged("QCSchema: no geometry for every atom"));
    }
    budget.atoms(symbols.len() as u64)?;
    budget.molecule()?;
    let mut m = Molecule { name: text(mol.get("name")).unwrap_or_else(|| "QCSchema molecule".into()), ..Molecule::default() };
    for (i, s) in symbols.iter().enumerate() {
        let p = &geometry[i * 3..i * 3 + 3];
        m.atoms.push(Atom::new(elements::by_symbol(s).unwrap_or(0), s.clone(), [p[0] * BOHR, p[1] * BOHR, p[2] * BOHR]));
    }
    for c in mol.get("connectivity").and_then(Value::as_array).into_iter().flatten() {
        let c = numbers(Some(c));
        if let [a, b, order, ..] = c[..]
            && (a as usize) < symbols.len()
            && (b as usize) < symbols.len()
        {
            // Orders may be fractional (1.5, aromatic).
            let order = if order.fract() != 0.0 { 4 } else { order as u8 };
            m.bonds.push(Bond::new(a as u32, b as u32, order));
        }
    }
    for (key, label) in [("molecular_charge", "Charge"), ("molecular_multiplicity", "Multiplicity")] {
        if let Some(t) = text(mol.get(key)) {
            m.properties.push((label.into(), t));
        }
    }
    for (key, label) in [("driver", "Driver"), ("return_energy", "Energy (hartree)")] {
        if let Some(t) = text(v.get(key)) {
            m.properties.push((label.into(), t));
        }
    }
    if let Some(method) = v.pointer("/model/method").and_then(Value::as_str) {
        m.properties.push(("Method".into(), method.into()));
    }
    Ok(File { molecules: vec![m], notes: Vec::new() })
}

#[cfg(test)]
mod tests {
    use crate::{BOHR, Format, Limits, detect, read};

    #[test]
    fn chemical_json() {
        let text = r#"{"chemicalJson": 1, "name": "water", "atoms": {"elements": {"number": [8, 1, 1]}, "coords": {"3d": [0, 0, 0, 0.96, 0, 0, -0.24, 0.93, 0]}, "formalCharges": [0, 1, 0]}, "bonds": {"connections": {"index": [0, 1, 0, 2, 0, 9]}, "order": [1, 2]}, "properties": {"totalCharge": 0, "x": [1]}}"#;
        assert_eq!(detect(text.as_bytes()), Some(Format::ChemicalJson));
        let m = &read(text.as_bytes(), Format::ChemicalJson, &Limits::default()).unwrap().molecules[0];
        assert_eq!((m.name.as_str(), m.atoms.len(), m.atoms[1].charge), ("water", 3, 1));
        assert_eq!(m.bonds.iter().map(|b| (b.a, b.b, b.order)).collect::<Vec<_>>(), [(0, 1, 1), (0, 2, 2)]);
        assert_eq!(m.properties, [("totalCharge".to_string(), "0".to_string())]);
        // Fractional coordinates in a cell given by lengths and angles; symbols.
        let crystal = r#"{"chemicalJson": 1, "atoms": {"elements": {"symbol": ["Na", "Cl"]}, "coords": {"3dFractional": [0, 0, 0, 0.5, 0.5, 0.5]}}, "unitCell": {"a": 5.64, "b": 5.64, "c": 5.64, "alpha": 90, "beta": 90, "gamma": 90}}"#;
        let m = &read(crystal.as_bytes(), Format::ChemicalJson, &Limits::default()).unwrap().molecules[0];
        assert!((m.atoms[1].position[2] - 2.82).abs() < 1e-9 && m.atoms[1].element == 17 && m.cell.is_some());
        // Coordinates that don't come in threes: whole triples only (found by fuzzing).
        let ragged = r#"{"atoms": {"elements": {"number": [6]}, "coords": {"3d": [1, 2, 3, 4]}}}"#;
        assert_eq!(read(ragged.as_bytes(), Format::ChemicalJson, &Limits::default()).unwrap().molecules[0].atoms[0].position, [1.0, 2.0, 3.0]);
        // The first version's spelling (Avogadro's rutile).
        let old = r#"{"chemical json": 0, "unit cell": {"a": 2, "b": 2, "c": 2, "alpha": 90, "beta": 90, "gamma": 90}, "atoms": {"elements": {"number": [22]}, "coords": {"3d fractional": [0.5, 0.5, 0.5]}}}"#;
        assert_eq!(detect(old.as_bytes()), Some(Format::ChemicalJson));
        let m = &read(old.as_bytes(), Format::ChemicalJson, &Limits::default()).unwrap().molecules[0];
        assert!((m.atoms[0].position[0] - 1.0).abs() < 1e-9 && m.cell.is_some());
        let vectors = r#"{"atoms": {"elements": {"number": [6]}, "coords": {"3dFractional": [0.5, 0, 0]}}, "unitCell": {"cellVectors": [2, 0, 0, 0, 2, 0, 0, 0, 2]}}"#;
        assert_eq!(read(vectors.as_bytes(), Format::ChemicalJson, &Limits::default()).unwrap().molecules[0].atoms[0].position, [1.0, 0.0, 0.0]);
        for bad in [r#"{"x": 1}"#, r#"{"atoms": {"elements": {"number": [6]}}}"#, "{", r#"{"atoms": {"elements": {"number": [6]}, "coords": {"3dFractional": [0, 0, 0]}}}"#] {
            assert!(matches!(read(bad.as_bytes(), Format::ChemicalJson, &Limits::default()), Err(crate::Error::Damaged(_))), "{bad}");
        }
    }

    #[test]
    fn qcschema() {
        let text = r#"{"schema_name": "qcschema_output", "driver": "energy", "model": {"method": "B3LYP"}, "return_energy": -76.4, "molecule": {"schema_name": "qcschema_molecule", "symbols": ["O", "H", "H"], "geometry": [0, 0, 0, 1.8, 0, 0, 0, 1.8, 0], "connectivity": [[0, 1, 1], [0, 2, 1.5], [0, 7, 1]], "molecular_charge": 0}}"#;
        assert_eq!(detect(text.as_bytes()), Some(Format::QcSchema));
        let m = &read(text.as_bytes(), Format::QcSchema, &Limits::default()).unwrap().molecules[0];
        assert!((m.atoms[1].position[0] - 1.8 * BOHR).abs() < 1e-9);
        assert_eq!(m.bonds.iter().map(|b| b.order).collect::<Vec<_>>(), [1, 4]);
        let props: Vec<&str> = m.properties.iter().map(|p| p.0.as_str()).collect();
        assert_eq!(props, ["Charge", "Driver", "Energy (hartree)", "Method"]);
        // At the top, with no bonds: found.
        let top = r#"{"symbols": ["H", "H"], "geometry": [0, 0, 0, 1.4, 0, 0], "name": "H2"}"#;
        let m = &read(top.as_bytes(), Format::QcSchema, &Limits::default()).unwrap().molecules[0];
        assert_eq!((m.name.as_str(), m.bonds.len(), m.bonds_found), ("H2", 1, true));
        for bad in [r#"{"x": 1}"#, r#"{"symbols": ["H"], "geometry": [0]}"#] {
            assert!(matches!(read(bad.as_bytes(), Format::QcSchema, &Limits::default()), Err(crate::Error::Damaged(_))), "{bad}");
        }
        assert!(matches!(read(top.as_bytes(), Format::QcSchema, &Limits::default().with_atoms(1)), Err(crate::Error::TooLarge { .. })));
    }
}

#[cfg(test)]
mod edges {
    use super::*;
    use crate::{BOHR, Format, Limits, read};

    #[test]
    fn chemical_json_edges() {
        let read_c = |t: &str| read(t.as_bytes(), Format::ChemicalJson, &Limits::default());
        // Too few coordinates, Cartesian or fractional, is damage.
        assert!(matches!(read_c(r#"{"atoms": {"elements": {"number": [6, 6, 6]}, "coords": {"3d": [0, 0, 0, 1, 0, 0]}}}"#), Err(crate::Error::Damaged(_))));
        assert!(matches!(
            read_c(r#"{"atoms": {"elements": {"number": [6, 6, 6]}, "coords": {"3dFractional": [0, 0, 0]}}, "unitCell": {"cellVectors": [2, 0, 0, 0, 3, 0, 0, 0, 4]}}"#),
            Err(crate::Error::Damaged(_))
        ));
        // Fractions in a cell that isn't cubic; a bond to an atom past the last; a flag.
        let m = &read_c(r#"{"atoms": {"elements": {"number": [6, 6, 6]}, "coords": {"3dFractional": [0.5, 0.5, 0.5, 0, 0, 0, 0.5, 0, 0]}}, "unitCell": {"cellVectors": [2, 0, 0, 0, 3, 0, 0, 0, 4]}, "bonds": {"connections": {"index": [0, 1, 0, 3, 3, 0]}}, "properties": {"flag": true}}"#).unwrap().molecules[0];
        assert_eq!(m.atoms[0].position, [1.0, 1.5, 2.0]);
        assert_eq!(m.bonds.len(), 1);
        assert_eq!(m.properties, [("flag".to_string(), "true".to_string())]);
    }

    #[test]
    fn old_keys_inside_arrays() {
        let mut v = serde_json::json!([{"unit cell": 1}]);
        respell(&mut v);
        assert_eq!(v[0]["unitCell"], 1);
    }

    #[test]
    fn qcschema_edges() {
        let read_q = |t: &str| read(t.as_bytes(), Format::QcSchema, &Limits::default());
        assert!(matches!(read_q(r#"{"symbols": ["H", "H"], "geometry": [0, 0, 0, 1, 1]}"#), Err(crate::Error::Damaged(_))));
        let m = &read_q(r#"{"symbols": ["H", "H"], "geometry": [0, 0, 0, 1, 2, 3], "connectivity": [[0, 1, 1], [0, 2, 1], [2, 0, 1]]}"#).unwrap().molecules[0];
        let p = m.atoms[1].position;
        assert!((p[1] - 2.0 * BOHR).abs() < 1e-9 && (p[2] - 3.0 * BOHR).abs() < 1e-9, "{p:?}");
        assert_eq!(m.bonds.len(), 1);
    }
}
