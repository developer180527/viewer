//! The periodic table, as far as drawing a molecule needs it: symbols, covalent radii
//! (Cordero et al., 2008, Å) and the colours molecular viewers give each element (Jmol's).

const SYMBOLS: [&str; 119] = [
    "", "H", "He", "Li", "Be", "B", "C", "N", "O", "F", "Ne", "Na", "Mg", "Al", "Si", "P", "S", "Cl", "Ar", "K", "Ca", "Sc", "Ti", "V", "Cr", "Mn", "Fe", "Co", "Ni", "Cu", "Zn", "Ga", "Ge",
    "As", "Se", "Br", "Kr", "Rb", "Sr", "Y", "Zr", "Nb", "Mo", "Tc", "Ru", "Rh", "Pd", "Ag", "Cd", "In", "Sn", "Sb", "Te", "I", "Xe", "Cs", "Ba", "La", "Ce", "Pr", "Nd", "Pm", "Sm", "Eu",
    "Gd", "Tb", "Dy", "Ho", "Er", "Tm", "Yb", "Lu", "Hf", "Ta", "W", "Re", "Os", "Ir", "Pt", "Au", "Hg", "Tl", "Pb", "Bi", "Po", "At", "Rn", "Fr", "Ra", "Ac", "Th", "Pa", "U", "Np", "Pu",
    "Am", "Cm", "Bk", "Cf", "Es", "Fm", "Md", "No", "Lr", "Rf", "Db", "Sg", "Bh", "Hs", "Mt", "Ds", "Rg", "Cn", "Nh", "Fl", "Mc", "Lv", "Ts", "Og",
];

/// Covalent radii in hundredths of an ångström, to curium; heavier elements have none
/// measured and are given 1.50.
const RADII: [u16; 97] = [
    0, 31, 28, 128, 96, 84, 76, 71, 66, 57, 58, 166, 141, 121, 111, 107, 105, 102, 106, 203, 176, 170, 160, 153, 139, 139, 132, 126, 124, 132, 122, 122, 120, 119, 120, 120, 116, 220, 195,
    190, 175, 164, 154, 147, 146, 142, 139, 145, 144, 142, 139, 139, 138, 139, 140, 244, 215, 207, 204, 203, 201, 199, 198, 198, 196, 194, 192, 192, 189, 190, 187, 187, 175, 170, 162,
    151, 144, 141, 136, 136, 132, 145, 146, 148, 140, 150, 150, 260, 221, 215, 206, 200, 196, 190, 187, 180, 169,
];

/// Jmol's colours to xenon, as 0xRRGGBB.
const COLOURS: [u32; 55] = [
    0xFF1493, 0xFFFFFF, 0xD9FFFF, 0xCC80FF, 0xC2FF00, 0xFFB5B5, 0x909090, 0x3050F8, 0xFF0D0D, 0x90E050, 0xB3E3F5, 0xAB5CF2, 0x8AFF00, 0xBFA6A6, 0xF0C8A0, 0xFF8000, 0xFFFF30, 0x1FF01F,
    0x80D1E3, 0x8F40D4, 0x3DFF00, 0xE6E6E6, 0xBFC2C7, 0xA6A6AB, 0x8A99C7, 0x9C7AC7, 0xE06633, 0xF090A0, 0x50D050, 0xC88033, 0x7D80B0, 0xC28F8F, 0x668F8F, 0xBD80E3, 0xFFA100, 0xA62929,
    0x5CB8D1, 0x702EB0, 0x00FF00, 0x94FFFF, 0x94E0E0, 0x73C2C9, 0x54B5B5, 0x3B9E9E, 0x248F8F, 0x0A7D8C, 0x006985, 0xC0C0C0, 0xFFD98F, 0xA67573, 0x668080, 0x9E63B5, 0xD47A00, 0x940094,
    0x429EB0,
];

/// Heavier elements' colours, where Jmol gives one people know.
const HEAVY: [(u8, u32); 12] = [(55, 0x57178F), (56, 0x00C900), (57, 0x70D4FF), (74, 0x2194D6), (76, 0x266696), (77, 0x175487), (78, 0xD0D0E0), (79, 0xFFD123), (80, 0xB8B8D0), (82, 0x575961), (83, 0x9E4FB5), (92, 0x008FFF)];

/// The heaviest element there is.
pub const LAST: u8 = 118;

/// An element's symbol, `""` for none (0) or past oganesson.
pub fn symbol(z: u8) -> &'static str {
    SYMBOLS.get(z as usize).copied().unwrap_or("")
}

/// The element a symbol names, in any case (`CL`, `cl` and `Cl` alike, as PDB writes
/// them); deuterium and tritium are hydrogen.
pub fn by_symbol(s: &str) -> Option<u8> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("D") || s.eq_ignore_ascii_case("T") {
        return Some(1);
    }
    (1..=LAST).find(|z| SYMBOLS[*z as usize].eq_ignore_ascii_case(s))
}

/// A symbol or an atomic number, as XYZ and Turbomole files may give either; a label
/// like `C12` or `Fe_2` is its element.
pub fn parse(s: &str) -> Option<u8> {
    let s = s.trim();
    if let Ok(z) = s.parse::<u8>() {
        return (1..=LAST).contains(&z).then_some(z);
    }
    if let Some(z) = by_symbol(s) {
        return Some(z);
    }
    // The letters a label starts with: two if they name an element, else one.
    let letters: String = s.chars().take_while(char::is_ascii_alphabetic).take(2).collect();
    by_symbol(&letters).or_else(|| by_symbol(&letters[..letters.len().min(1)]))
}

/// Covalent radius in Å.
pub fn covalent_radius(z: u8) -> f64 {
    match RADII.get(z as usize) {
        Some(0) | None => 1.5,
        Some(r) => *r as f64 / 100.0,
    }
}

/// The colour to draw an element in, as linear-light RGB from 0 to 1.
pub fn colour(z: u8) -> [f32; 3] {
    let rgb = COLOURS.get(z as usize).copied().or_else(|| HEAVY.iter().find(|h| h.0 == z).map(|h| h.1)).unwrap_or(0xFF1493);
    let srgb = |c: u32| {
        let c = c as f32 / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    [srgb(rgb >> 16 & 0xff), srgb(rgb >> 8 & 0xff), srgb(rgb & 0xff)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbols_numbers_and_labels() {
        assert_eq!((symbol(6), symbol(118), symbol(0), symbol(200)), ("C", "Og", "", ""));
        assert_eq!((by_symbol("CL"), by_symbol("cl"), by_symbol(" Fe "), by_symbol("D"), by_symbol("t"), by_symbol("Xx")), (Some(17), Some(17), Some(26), Some(1), Some(1), None));
        for (s, z) in [("8", Some(8)), ("0", None), ("119", None), ("C12", Some(6)), ("Fe_2", Some(26)), ("Cl1", Some(17)), ("Ca", Some(20)), ("Cx", Some(6)), ("", None), ("12C", None)] {
            assert_eq!(parse(s), z, "{s}");
        }
        assert!(SYMBOLS.len() == LAST as usize + 1 && (1..=LAST).all(|z| by_symbol(symbol(z)) == Some(z)));
    }

    #[test]
    fn radii_and_colours() {
        assert_eq!((covalent_radius(1), covalent_radius(6), covalent_radius(96), covalent_radius(97), covalent_radius(0)), (0.31, 0.76, 1.69, 1.5, 1.5));
        assert_eq!(colour(1), [1.0, 1.0, 1.0]);
        let o = colour(8);
        assert!(o[0] == 1.0 && o[1] < 0.01 && o[2] < 0.01, "{o:?}");
        assert_eq!(colour(79), colour(79));
        assert_ne!(colour(79), colour(0));
        // Unknown and heavy elements without a colour of their own are the same pink.
        assert_eq!(colour(0), colour(110));
    }
}
