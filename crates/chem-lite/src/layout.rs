//! Positions for a molecule that has none (SMILES): every bond its usual length, the
//! atoms around each one spread apart, as a spring model settles them. Heavy atoms are
//! kept near one plane, as chemists draw a structure, so rings come out flat and
//! readable; hydrogens stand out of it. A drawing, not a computed conformation.

use crate::{Molecule, elements};

/// Gives `m`'s atoms positions from its bonds.
pub fn layout(m: &mut Molecule) {
    let n = m.atoms.len();
    if n == 0 {
        return;
    }
    let mut next: Vec<Vec<usize>> = vec![Vec::new(); n];
    for b in &m.bonds {
        let (a, b) = (b.a as usize, b.b as usize);
        if a < n && b < n && a != b {
            next[a].push(b);
            next[b].push(a);
        }
    }
    let length = |a: usize, b: usize| elements::covalent_radius(m.atoms[a].element) + elements::covalent_radius(m.atoms[b].element);
    // A start: each part of the molecule grown from its first atom, a neighbour placed
    // a bond away along a direction of its own, so no two start in one place; parts
    // that aren't joined (salts, mixtures) side by side.
    let mut pos = vec![[f64::NAN; 3]; n];
    let mut offset = 0.0;
    for root in 0..n {
        if pos[root][0].is_finite() {
            continue;
        }
        pos[root] = [offset, 0.0, 0.0];
        let mut queue = std::collections::VecDeque::from([root]);
        let mut extent = offset;
        while let Some(a) = queue.pop_front() {
            for (k, &b) in next[a].iter().enumerate() {
                if pos[b][0].is_finite() {
                    continue;
                }
                // Heavy atoms start in the plane, hydrogens out of it.
                let mut d = direction(b * 7 + k);
                if m.atoms[b].element != 1 {
                    let flat = (d[0] * d[0] + d[1] * d[1]).sqrt().max(1e-6);
                    d = [d[0] / flat, d[1] / flat, 0.0];
                }
                let l = length(a, b);
                pos[b] = [pos[a][0] + d[0] * l, pos[a][1] + d[1] * l, pos[a][2] + d[2] * l];
                extent = extent.max(pos[b][0]);
                queue.push_back(b);
            }
        }
        offset = extent + 4.0;
    }
    // Pairs two bonds apart, kept at the distance of a tetrahedral angle between them.
    let mut angles = Vec::new();
    for (centre, around) in next.iter().enumerate() {
        for (i, &a) in around.iter().enumerate() {
            for &b in &around[i + 1..] {
                let (la, lb) = (length(centre, a), length(centre, b));
                // The law of cosines, at 109.5° (120° for three neighbours or fewer would
                // be flatter; the difference doesn't show at this scale).
                let cos = if around.len() <= 3 { -0.5 } else { -0.3338 };
                angles.push((a, b, (la * la + lb * lb - 2.0 * la * lb * cos).sqrt()));
            }
        }
    }
    let bonds: Vec<(usize, usize, f64)> = m.bonds.iter().filter(|b| (b.a as usize) < n && (b.b as usize) < n && b.a != b.b).map(|b| (b.a as usize, b.b as usize, length(b.a as usize, b.b as usize))).collect();
    // Every pair repels within a few Å; for big molecules only near ones are compared.
    let all_pairs = n <= 400;
    for step in 0..400 {
        let mut force = vec![[0.0f64; 3]; n];
        let pull = |a: usize, b: usize, want: f64, k: f64, force: &mut Vec<[f64; 3]>| {
            let d = [pos[b][0] - pos[a][0], pos[b][1] - pos[a][1], pos[b][2] - pos[a][2]];
            let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt().max(1e-6);
            let f = k * (len - want) / len;
            for c in 0..3 {
                force[a][c] += f * d[c];
                force[b][c] -= f * d[c];
            }
        };
        for &(a, b, l) in &bonds {
            pull(a, b, l, 0.5, &mut force);
        }
        for &(a, b, l) in &angles {
            pull(a, b, l, 0.2, &mut force);
        }
        if all_pairs {
            for a in 0..n {
                for b in a + 1..n {
                    let d2: f64 = (0..3).map(|c| (pos[b][c] - pos[a][c]).powi(2)).sum();
                    if d2 < 9.0 && !next[a].contains(&b) {
                        pull(a, b, 3.0, 0.05, &mut force);
                    }
                }
            }
        }
        // The plane: heavy atoms drawn back to it.
        for (a, f) in force.iter_mut().enumerate() {
            if m.atoms[a].element != 1 {
                f[2] -= 1.5 * pos[a][2];
            }
        }
        // Cooling: big steps at first, small ones to settle.
        let step_size = 0.5 * (1.0 - step as f64 / 400.0) + 0.05;
        for (p, f) in pos.iter_mut().zip(&force) {
            for c in 0..3 {
                p[c] += (f[c] * step_size).clamp(-0.5, 0.5);
            }
        }
    }
    // Heavy atoms on the plane exactly, where the springs left them a little off it: the
    // drawing is flat, whatever the hydrogens push.
    for (atom, mut p) in m.atoms.iter_mut().zip(pos) {
        if atom.element != 1 {
            p[2] = 0.0;
        }
        atom.position = p;
    }
}

/// A direction spread over the sphere, the same for the same seed (a golden-angle
/// spiral): no randomness, so a file always lays out the same way.
fn direction(seed: usize) -> [f64; 3] {
    let i = (seed % 97) as f64 + 0.5;
    let z = 1.0 - 2.0 * i / 97.0;
    let r = (1.0 - z * z).sqrt();
    let t = i * 2.399_963_229_728_653;
    [r * t.cos(), r * t.sin(), z]
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use crate::{Atom, Bond};

    fn dist(m: &Molecule, a: usize, b: usize) -> f64 {
        (0..3).map(|c| (m.atoms[a].position[c] - m.atoms[b].position[c]).powi(2)).sum::<f64>().sqrt()
    }

    #[test]
    fn bonds_come_out_their_length_and_atoms_apart() {
        // Ethanol's heavy atoms and its hydrogens.
        let mut m = Molecule::default();
        let elements = [6, 6, 8, 1, 1, 1, 1, 1, 1];
        m.atoms = elements.iter().map(|z| Atom::new(*z, "", [0.0; 3])).collect();
        m.bonds = [(0, 1), (1, 2), (0, 3), (0, 4), (0, 5), (1, 6), (1, 7), (2, 8)].iter().map(|(a, b)| Bond::new(*a, *b, 1)).collect();
        layout(&mut m);
        for b in m.bonds.clone() {
            let want = elements::covalent_radius(m.atoms[b.a as usize].element) + elements::covalent_radius(m.atoms[b.b as usize].element);
            let got = dist(&m, b.a as usize, b.b as usize);
            assert!((got - want).abs() < 0.15, "{b:?}: {got} for {want}");
        }
        for a in 0..m.atoms.len() {
            for b in a + 1..m.atoms.len() {
                assert!(dist(&m, a, b) > 0.9, "{a} and {b} on top of each other");
            }
        }
        // The same every time.
        let mut again = m.clone();
        layout(&mut again);
        assert_eq!(again, m);
    }

    #[test]
    fn rings_lie_flat() {
        // Naphthalene's carbons: two fused rings.
        let mut m = crate::smiles::parse("c1ccc2ccccc2c1").unwrap();
        layout(&mut m);
        for a in m.atoms.iter().filter(|a| a.element == 6) {
            assert_eq!(a.position[2], 0.0, "{:?} out of the plane", a.position);
        }
        // Every ring bond about its length, so neither ring is crushed.
        for b in m.bonds.iter().filter(|b| b.order == 4) {
            let d = dist(&m, b.a as usize, b.b as usize);
            assert!((d - 1.52).abs() < 0.12, "{d}");
        }
    }

    #[test]
    fn parts_side_by_side_and_nothing_to_lay_out() {
        let mut salt = Molecule::default();
        salt.atoms = vec![Atom::new(11, "", [0.0; 3]), Atom::new(17, "", [0.0; 3])];
        layout(&mut salt);
        assert!(dist(&salt, 0, 1) > 3.0);
        let mut empty = Molecule::default();
        layout(&mut empty);
        assert!(empty.atoms.is_empty());
        // A bond to an atom that isn't there is ignored.
        let mut odd = Molecule::default();
        odd.atoms = vec![Atom::new(6, "", [0.0; 3])];
        odd.bonds = vec![Bond::new(0, 5, 1), Bond::new(0, 0, 1)];
        layout(&mut odd);
        assert!(odd.atoms[0].position.iter().all(|c| c.is_finite()));
    }
}
