//! Bonds found by distance, as viewers find them where a file gives none.

use std::collections::HashMap;

use crate::{Atom, Bond, elements};

/// How much longer than the two covalent radii together a bond may be.
const TOLERANCE: f64 = 1.2;
/// No bond is shorter (two atoms on top of each other are an error in the file).
const SHORTEST: f64 = 0.4;
/// The longest bond any pair of elements could make: the cell size of the search.
const LONGEST: f64 = 2.0 * 2.6 * TOLERANCE;

/// Every pair of atoms close enough to be bonded, each once, in order. Found through a
/// grid of cells as large as the longest bond, so only neighbouring cells are compared:
/// the cost grows with the atoms, not their square.
pub fn find_bonds(atoms: &[Atom]) -> Vec<Bond> {
    let cell = |p: [f64; 3]| [p[0], p[1], p[2]].map(|c| (c / LONGEST).floor() as i64);
    let mut grid: HashMap<[i64; 3], Vec<u32>> = HashMap::new();
    for (i, a) in atoms.iter().enumerate() {
        if a.position.iter().all(|c| c.is_finite()) {
            grid.entry(cell(a.position)).or_default().push(i as u32);
        }
    }
    let mut out = Vec::new();
    for (i, a) in atoms.iter().enumerate() {
        if !a.position.iter().all(|c| c.is_finite()) {
            continue;
        }
        let [x, y, z] = cell(a.position);
        let ra = elements::covalent_radius(a.element);
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    for &j in grid.get(&[x + dx, y + dy, z + dz]).map_or(&[][..], Vec::as_slice) {
                        if j as usize <= i {
                            continue;
                        }
                        let b = &atoms[j as usize];
                        // Hydrogens don't bond to each other except in H₂ itself.
                        let d2: f64 = (0..3).map(|k| (a.position[k] - b.position[k]).powi(2)).sum();
                        let most = (ra + elements::covalent_radius(b.element)) * TOLERANCE;
                        if d2 >= SHORTEST * SHORTEST && d2 <= most * most && !(a.element == 1 && b.element == 1 && atoms.len() > 2) {
                            out.push(Bond::new(i as u32, j, 1));
                        }
                    }
                }
            }
        }
    }
    out.sort_by_key(|b| (b.a, b.b));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn atom(z: u8, p: [f64; 3]) -> Atom {
        Atom::new(z, "", p)
    }

    #[test]
    fn water_and_methane() {
        let water = [atom(8, [0.0, 0.0, 0.0]), atom(1, [0.96, 0.0, 0.0]), atom(1, [-0.24, 0.93, 0.0])];
        assert_eq!(find_bonds(&water), [Bond::new(0, 1, 1), Bond::new(0, 2, 1)]);
        // The two hydrogens, 1.5 Å apart, aren't bonded; nor are atoms in one place.
        let d = 1.09 / 3f64.sqrt();
        let methane = [atom(6, [0.0; 3]), atom(1, [d, d, d]), atom(1, [-d, -d, d]), atom(1, [-d, d, -d]), atom(1, [d, -d, -d]), atom(6, [0.0; 3])];
        assert_eq!(find_bonds(&methane).iter().map(|b| (b.a, b.b)).collect::<Vec<_>>(), [(0, 1), (0, 2), (0, 3), (0, 4), (1, 5), (2, 5), (3, 5), (4, 5)]);
        // H₂ is.
        assert_eq!(find_bonds(&[atom(1, [0.0; 3]), atom(1, [0.74, 0.0, 0.0])]).len(), 1);
    }

    #[test]
    fn across_cells_and_far_apart() {
        // Either side of a cell boundary, in negative space too.
        let edge = LONGEST;
        let pair = [atom(6, [edge - 0.7, -0.1, 0.0]), atom(6, [edge + 0.7, -0.1, 0.0])];
        assert_eq!(find_bonds(&pair).len(), 1);
        let far = [atom(6, [0.0; 3]), atom(6, [1.9, 0.0, 0.0]), atom(6, [100.0, 0.0, 0.0]), atom(6, [f64::NAN, 0.0, 0.0])];
        assert!(find_bonds(&far).is_empty());
        // A thousand atoms in a line, 1.5 Å apart: 999 bonds, found quickly.
        let line: Vec<Atom> = (0..1000).map(|i| atom(6, [i as f64 * 1.5, 0.0, 0.0])).collect();
        assert_eq!(find_bonds(&line).len(), 999);
    }
}
