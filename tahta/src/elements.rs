//! Element data and derived chemistry for the periodic table panel
//! (`periodic.rs`): Turkish names, standard atomic masses, Pauling
//! electronegativities, table position, MEB-style classes, ground-state
//! electron configurations and shell (Bohr) distributions.
//!
//! Masses: IUPAC standard atomic weights, abridged; elements with no
//! stable isotope show the mass number of their longest-lived isotope in
//! brackets, as textbooks do. Electronegativity: Pauling scale (empty
//! when not defined — He, Ne, Ar, and the heaviest elements).

/// (symbol, Turkish name, atomic mass as printed, Pauling EN or "").
pub const ELEMENTS: [(&str, &str, &str, &str); 118] = [
    ("H", "Hidrojen", "1.008", "2.20"),
    ("He", "Helyum", "4.0026", ""),
    ("Li", "Lityum", "6.94", "0.98"),
    ("Be", "Berilyum", "9.0122", "1.57"),
    ("B", "Bor", "10.81", "2.04"),
    ("C", "Karbon", "12.011", "2.55"),
    ("N", "Azot", "14.007", "3.04"),
    ("O", "Oksijen", "15.999", "3.44"),
    ("F", "Flor", "18.998", "3.98"),
    ("Ne", "Neon", "20.180", ""),
    ("Na", "Sodyum", "22.990", "0.93"),
    ("Mg", "Magnezyum", "24.305", "1.31"),
    ("Al", "Alüminyum", "26.982", "1.61"),
    ("Si", "Silisyum", "28.085", "1.90"),
    ("P", "Fosfor", "30.974", "2.19"),
    ("S", "Kükürt", "32.06", "2.58"),
    ("Cl", "Klor", "35.45", "3.16"),
    ("Ar", "Argon", "39.948", ""),
    ("K", "Potasyum", "39.098", "0.82"),
    ("Ca", "Kalsiyum", "40.078", "1.00"),
    ("Sc", "Skandiyum", "44.956", "1.36"),
    ("Ti", "Titanyum", "47.867", "1.54"),
    ("V", "Vanadyum", "50.942", "1.63"),
    ("Cr", "Krom", "51.996", "1.66"),
    ("Mn", "Mangan", "54.938", "1.55"),
    ("Fe", "Demir", "55.845", "1.83"),
    ("Co", "Kobalt", "58.933", "1.88"),
    ("Ni", "Nikel", "58.693", "1.91"),
    ("Cu", "Bakır", "63.546", "1.90"),
    ("Zn", "Çinko", "65.38", "1.65"),
    ("Ga", "Galyum", "69.723", "1.81"),
    ("Ge", "Germanyum", "72.630", "2.01"),
    ("As", "Arsenik", "74.922", "2.18"),
    ("Se", "Selenyum", "78.971", "2.55"),
    ("Br", "Brom", "79.904", "2.96"),
    ("Kr", "Kripton", "83.798", "3.00"),
    ("Rb", "Rubidyum", "85.468", "0.82"),
    ("Sr", "Stronsiyum", "87.62", "0.95"),
    ("Y", "İtriyum", "88.906", "1.22"),
    ("Zr", "Zirkonyum", "91.224", "1.33"),
    ("Nb", "Niyobyum", "92.906", "1.6"),
    ("Mo", "Molibden", "95.95", "2.16"),
    ("Tc", "Teknesyum", "[98]", "1.9"),
    ("Ru", "Rutenyum", "101.07", "2.2"),
    ("Rh", "Rodyum", "102.91", "2.28"),
    ("Pd", "Paladyum", "106.42", "2.20"),
    ("Ag", "Gümüş", "107.87", "1.93"),
    ("Cd", "Kadmiyum", "112.41", "1.69"),
    ("In", "İndiyum", "114.82", "1.78"),
    ("Sn", "Kalay", "118.71", "1.96"),
    ("Sb", "Antimon", "121.76", "2.05"),
    ("Te", "Tellür", "127.60", "2.1"),
    ("I", "İyot", "126.90", "2.66"),
    ("Xe", "Ksenon", "131.29", "2.6"),
    ("Cs", "Sezyum", "132.91", "0.79"),
    ("Ba", "Baryum", "137.33", "0.89"),
    ("La", "Lantan", "138.91", "1.10"),
    ("Ce", "Seryum", "140.12", "1.12"),
    ("Pr", "Praseodim", "140.91", "1.13"),
    ("Nd", "Neodim", "144.24", "1.14"),
    ("Pm", "Prometyum", "[145]", "1.13"),
    ("Sm", "Samaryum", "150.36", "1.17"),
    ("Eu", "Evropiyum", "151.96", "1.2"),
    ("Gd", "Gadolinyum", "157.25", "1.20"),
    ("Tb", "Terbiyum", "158.93", "1.2"),
    ("Dy", "Disprosyum", "162.50", "1.22"),
    ("Ho", "Holmiyum", "164.93", "1.23"),
    ("Er", "Erbiyum", "167.26", "1.24"),
    ("Tm", "Tulyum", "168.93", "1.25"),
    ("Yb", "İterbiyum", "173.05", "1.1"),
    ("Lu", "Lutesyum", "174.97", "1.27"),
    ("Hf", "Hafniyum", "178.49", "1.3"),
    ("Ta", "Tantal", "180.95", "1.5"),
    ("W", "Tungsten", "183.84", "2.36"),
    ("Re", "Renyum", "186.21", "1.9"),
    ("Os", "Osmiyum", "190.23", "2.2"),
    ("Ir", "İridyum", "192.22", "2.20"),
    ("Pt", "Platin", "195.08", "2.28"),
    ("Au", "Altın", "196.97", "2.54"),
    ("Hg", "Cıva", "200.59", "2.00"),
    ("Tl", "Talyum", "204.38", "1.62"),
    ("Pb", "Kurşun", "207.2", "1.87"),
    ("Bi", "Bizmut", "208.98", "2.02"),
    ("Po", "Polonyum", "[209]", "2.0"),
    ("At", "Astatin", "[210]", "2.2"),
    ("Rn", "Radon", "[222]", "2.2"),
    ("Fr", "Fransiyum", "[223]", "0.7"),
    ("Ra", "Radyum", "[226]", "0.9"),
    ("Ac", "Aktinyum", "[227]", "1.1"),
    ("Th", "Toryum", "232.04", "1.3"),
    ("Pa", "Protaktinyum", "231.04", "1.5"),
    ("U", "Uranyum", "238.03", "1.38"),
    ("Np", "Neptünyum", "[237]", "1.36"),
    ("Pu", "Plütonyum", "[244]", "1.28"),
    ("Am", "Amerikyum", "[243]", "1.13"),
    ("Cm", "Küriyum", "[247]", "1.28"),
    ("Bk", "Berkelyum", "[247]", "1.3"),
    ("Cf", "Kaliforniyum", "[251]", "1.3"),
    ("Es", "Aynştaynyum", "[252]", "1.3"),
    ("Fm", "Fermiyum", "[257]", "1.3"),
    ("Md", "Mendelevyum", "[258]", "1.3"),
    ("No", "Nobelyum", "[259]", "1.3"),
    ("Lr", "Lavrensiyum", "[266]", ""),
    ("Rf", "Rutherfordiyum", "[267]", ""),
    ("Db", "Dubniyum", "[268]", ""),
    ("Sg", "Seaborgiyum", "[269]", ""),
    ("Bh", "Bohriyum", "[270]", ""),
    ("Hs", "Hassiyum", "[269]", ""),
    ("Mt", "Meitneriyum", "[278]", ""),
    ("Ds", "Darmstadtiyum", "[281]", ""),
    ("Rg", "Röntgenyum", "[282]", ""),
    ("Cn", "Kopernikyum", "[285]", ""),
    ("Nh", "Nihonyum", "[286]", ""),
    ("Fl", "Flerovyum", "[289]", ""),
    ("Mc", "Moskovyum", "[290]", ""),
    ("Lv", "Livermoryum", "[293]", ""),
    ("Ts", "Tennesin", "[294]", ""),
    ("Og", "Oganesson", "[294]", ""),
];

pub fn symbol(z: u32) -> &'static str {
    ELEMENTS[z as usize - 1].0
}

pub fn name(z: u32) -> &'static str {
    ELEMENTS[z as usize - 1].1
}

/// Atomic mass as printed in Turkish (decimal comma), "[98]" for
/// elements without a stable isotope.
pub fn mass_text(z: u32) -> String {
    ELEMENTS[z as usize - 1].2.replace('.', ",")
}

/// Mass number used for the nucleus drawing (rounded atomic mass, or
/// the bracketed isotope).
pub fn mass_number(z: u32) -> u32 {
    let m = ELEMENTS[z as usize - 1].2.trim_matches(|c| c == '[' || c == ']');
    m.parse::<f32>().map(|v| v.round() as u32).unwrap_or(2 * z)
}

pub fn electronegativity_text(z: u32) -> Option<String> {
    let en = ELEMENTS[z as usize - 1].3;
    (!en.is_empty()).then(|| en.replace('.', ","))
}

/// Last atomic number of each period.
const PERIOD_ENDS: [u32; 7] = [2, 10, 18, 36, 54, 86, 118];

pub fn period(z: u32) -> u32 {
    PERIOD_ENDS.iter().position(|&end| z <= end).unwrap() as u32 + 1
}

/// Where the element sits: `(row, col)` with rows 0..7 for the main
/// table and 7/8 for the lanthanide/actinide rows below it (starting
/// under group 3, col 2).
pub fn position(z: u32) -> (usize, usize) {
    let p = period(z);
    let start = if p == 1 { 1 } else { PERIOD_ENDS[p as usize - 2] + 1 };
    let offset = (z - start) as usize;
    let row = p as usize - 1;
    match p {
        1 => (0, if z == 1 { 0 } else { 17 }),
        2 | 3 => (row, if offset < 2 { offset } else { offset + 10 }),
        4 | 5 => (row, offset),
        _ => match offset {
            0 | 1 => (row, offset),
            2..=16 => (row + 2, offset),
            _ => (row, offset - 14),
        },
    }
}

/// Group 1-18, `None` for the lanthanide/actinide rows.
pub fn group(z: u32) -> Option<u32> {
    let (row, col) = position(z);
    (row < 7).then_some(col as u32 + 1)
}

/// The A/B group name Turkish textbooks use ("6A", "8B").
pub fn group_ab(g: u32) -> String {
    match g {
        1 | 2 => format!("{g}A"),
        3..=7 => format!("{g}B"),
        8..=10 => "8B".to_string(),
        11 | 12 => format!("{}B", g - 10),
        _ => format!("{}A", g - 10),
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Class {
    Alkali,
    AlkalineEarth,
    Transition,
    PostTransition,
    Metalloid,
    Nonmetal,
    Halogen,
    NobleGas,
    Lanthanide,
    Actinide,
}

pub const CLASSES: [(Class, &str); 10] = [
    (Class::Alkali, "Alkali metal"),
    (Class::AlkalineEarth, "Toprak alkali"),
    (Class::Transition, "Geçiş metali"),
    (Class::PostTransition, "Zayıf metal"),
    (Class::Metalloid, "Yarı metal"),
    (Class::Nonmetal, "Ametal"),
    (Class::Halogen, "Halojen"),
    (Class::NobleGas, "Soy gaz"),
    (Class::Lanthanide, "Lantanit"),
    (Class::Actinide, "Aktinit"),
];

pub fn class(z: u32) -> Class {
    match z {
        3 | 11 | 19 | 37 | 55 | 87 => Class::Alkali,
        4 | 12 | 20 | 38 | 56 | 88 => Class::AlkalineEarth,
        2 | 10 | 18 | 36 | 54 | 86 | 118 => Class::NobleGas,
        9 | 17 | 35 | 53 | 85 | 117 => Class::Halogen,
        5 | 14 | 32 | 33 | 51 | 52 => Class::Metalloid,
        1 | 6 | 7 | 8 | 15 | 16 | 34 => Class::Nonmetal,
        57..=71 => Class::Lanthanide,
        89..=103 => Class::Actinide,
        13 | 31 | 49 | 50 | 81..=84 | 113..=116 => Class::PostTransition,
        _ => Class::Transition,
    }
}

pub fn class_name(c: Class) -> &'static str {
    CLASSES.iter().find(|(k, _)| *k == c).unwrap().1
}

/// The four-way classification MEB's chemistry curriculum uses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Metal,
    Nonmetal,
    Metalloid,
    NobleGas,
}

pub const KINDS: [(Kind, &str); 4] =
    [(Kind::Metal, "Metal"), (Kind::Nonmetal, "Ametal"), (Kind::Metalloid, "Yarı metal"), (Kind::NobleGas, "Soy gaz")];

pub fn kind(z: u32) -> Kind {
    match class(z) {
        Class::Nonmetal | Class::Halogen => Kind::Nonmetal,
        Class::Metalloid => Kind::Metalloid,
        Class::NobleGas => Kind::NobleGas,
        _ => Kind::Metal,
    }
}

/// s/p/d/f block by table position (He with the s block).
pub fn block(z: u32) -> char {
    let (row, col) = position(z);
    match (row, col) {
        (7 | 8, _) => 'f',
        _ if z == 2 => 's',
        (_, 0 | 1) => 's',
        (_, 2..=11) => 'd',
        _ => 'p',
    }
}

/// State at 25 °C and 1 atm; `None` when unknown (superheavy elements).
pub fn phase(z: u32) -> Option<&'static str> {
    match z {
        1 | 2 | 7 | 8 | 9 | 10 | 17 | 18 | 36 | 54 | 86 => Some("Gaz"),
        35 | 80 => Some("Sıvı"),
        _ if z >= 104 => None,
        _ => Some("Katı"),
    }
}

/// Subshell filling (Madelung) order as (n, l).
const ORDER: [(u32, u32); 19] = [
    (1, 0),
    (2, 0),
    (2, 1),
    (3, 0),
    (3, 1),
    (4, 0),
    (3, 2),
    (4, 1),
    (5, 0),
    (4, 2),
    (5, 1),
    (6, 0),
    (4, 3),
    (5, 2),
    (6, 1),
    (7, 0),
    (5, 3),
    (6, 2),
    (7, 1),
];

/// Ground states that break the Madelung rule, as (n, l, change) moves.
fn exceptions(z: u32) -> &'static [(u32, u32, i32)] {
    match z {
        24 | 29 => &[(4, 0, -1), (3, 2, 1)],
        41 | 42 | 44 | 45 | 47 => &[(5, 0, -1), (4, 2, 1)],
        46 => &[(5, 0, -2), (4, 2, 2)],
        57 | 58 | 64 => &[(4, 3, -1), (5, 2, 1)],
        78 | 79 => &[(6, 0, -1), (5, 2, 1)],
        89 | 91 | 92 | 93 | 96 => &[(5, 3, -1), (6, 2, 1)],
        90 => &[(5, 3, -2), (6, 2, 2)],
        103 => &[(6, 2, -1), (7, 1, 1)],
        _ => &[],
    }
}

/// Ground-state configuration as (n, l, electrons), in filling order,
/// empty subshells left out.
pub fn configuration(z: u32) -> Vec<(u32, u32, u32)> {
    let mut left = z;
    let mut out: Vec<(u32, u32, i32)> = Vec::new();
    for &(n, l) in &ORDER {
        if left == 0 {
            break;
        }
        let k = left.min(4 * l + 2);
        out.push((n, l, k as i32));
        left -= k;
    }
    for &(n, l, d) in exceptions(z) {
        match out.iter_mut().find(|(a, b, _)| *a == n && *b == l) {
            Some(entry) => entry.2 += d,
            None => out.push((n, l, d)),
        }
    }
    // A moved electron may open a subshell later in the order than the
    // last one filled (Lr's 7p): keep filling order.
    out.sort_by_key(|&(n, l, _)| ORDER.iter().position(|&o| o == (n, l)));
    out.into_iter().filter(|e| e.2 > 0).map(|(n, l, k)| (n, l, k as u32)).collect()
}

pub fn subshell_letter(l: u32) -> char {
    ['s', 'p', 'd', 'f'][l as usize]
}

pub fn superscript(n: u32) -> String {
    n.to_string()
        .chars()
        .map(|c| ['⁰', '¹', '²', '³', '⁴', '⁵', '⁶', '⁷', '⁸', '⁹'][c.to_digit(10).unwrap() as usize])
        .collect()
}

fn format_subshells(subshells: &[(u32, u32, u32)]) -> String {
    subshells
        .iter()
        .map(|&(n, l, k)| format!("{n}{}{}", subshell_letter(l), superscript(k)))
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn configuration_text(z: u32) -> String {
    format_subshells(&configuration(z))
}

/// The noble-gas core and what's left after it ("[Ne]", 3s² 3p⁴); no
/// core for H and He.
pub fn short_configuration(z: u32) -> Option<(u32, Vec<(u32, u32, u32)>)> {
    let core = [2, 10, 18, 36, 54, 86].into_iter().rev().find(|&c| c < z)?;
    let core_config = configuration(core);
    let rest = configuration(z)
        .into_iter()
        .filter_map(|(n, l, k)| {
            let inner = core_config.iter().find(|e| e.0 == n && e.1 == l).map_or(0, |e| e.2);
            (k > inner).then_some((n, l, k - inner))
        })
        .collect();
    Some((core, rest))
}

pub fn short_configuration_text(z: u32) -> String {
    match short_configuration(z) {
        Some((core, rest)) => format!("[{}] {}", symbol(core), format_subshells(&rest)),
        None => configuration_text(z),
    }
}

/// Electrons per shell K, L, M… (Bohr model / "katman dağılımı").
pub fn shells(z: u32) -> Vec<u32> {
    let mut out = vec![0; 7];
    for (n, _, k) in configuration(z) {
        out[n as usize - 1] += k;
    }
    while out.last() == Some(&0) {
        out.pop();
    }
    out
}

pub const SHELL_NAMES: [&str; 7] = ["K", "L", "M", "N", "O", "P", "Q"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configurations_follow_madelung_and_its_exceptions() {
        assert_eq!(configuration_text(8), "1s² 2s² 2p⁴");
        assert_eq!(configuration_text(26), "1s² 2s² 2p⁶ 3s² 3p⁶ 4s² 3d⁶");
        assert_eq!(short_configuration_text(24), "[Ar] 4s¹ 3d⁵");
        assert_eq!(short_configuration_text(29), "[Ar] 4s¹ 3d¹⁰");
        assert_eq!(short_configuration_text(46), "[Kr] 4d¹⁰");
        assert_eq!(short_configuration_text(79), "[Xe] 6s¹ 4f¹⁴ 5d¹⁰");
        assert_eq!(short_configuration_text(90), "[Rn] 7s² 6d²");
        assert_eq!(short_configuration_text(103), "[Rn] 7s² 5f¹⁴ 7p¹");
        assert_eq!(short_configuration_text(2), "1s²");
        for z in 1..=118 {
            assert_eq!(configuration(z).iter().map(|e| e.2).sum::<u32>(), z, "{}", symbol(z));
            assert!(configuration(z).iter().all(|&(_, l, k)| k <= 4 * l + 2), "{}", symbol(z));
        }
    }

    #[test]
    fn shells_and_neutrons() {
        assert_eq!(shells(8), vec![2, 6]);
        assert_eq!(shells(20), vec![2, 8, 8, 2]);
        assert_eq!(shells(26), vec![2, 8, 14, 2]);
        assert_eq!(shells(118), vec![2, 8, 18, 32, 32, 18, 8]);
        assert_eq!(mass_number(17) - 17, 18);
        assert_eq!(mass_number(43), 98);
    }

    #[test]
    fn table_positions_and_groups() {
        assert_eq!(position(1), (0, 0));
        assert_eq!(position(2), (0, 17));
        assert_eq!(position(5), (1, 12));
        assert_eq!(position(26), (3, 7));
        assert_eq!(position(57), (7, 2));
        assert_eq!(position(71), (7, 16));
        assert_eq!(position(72), (5, 3));
        assert_eq!(position(103), (8, 16));
        assert_eq!(position(118), (6, 17));
        // No two elements share a cell.
        let mut cells: Vec<_> = (1..=118).map(position).collect();
        cells.sort();
        cells.dedup();
        assert_eq!(cells.len(), 118);
        assert_eq!(group(8).map(group_ab).as_deref(), Some("6A"));
        assert_eq!(group(26).map(group_ab).as_deref(), Some("8B"));
        assert_eq!(group(29).map(group_ab).as_deref(), Some("1B"));
        assert_eq!(group(60), None);
        assert_eq!((block(2), block(26), block(60), block(16)), ('s', 'd', 'f', 'p'));
        assert_eq!((kind(1), kind(14), kind(18), kind(26)), (Kind::Nonmetal, Kind::Metalloid, Kind::NobleGas, Kind::Metal));
    }

    #[test]
    fn printed_values_use_a_decimal_comma() {
        assert_eq!(mass_text(8), "15,999");
        assert_eq!(mass_text(43), "[98]");
        assert_eq!(electronegativity_text(9).as_deref(), Some("3,98"));
        assert_eq!(electronegativity_text(10), None);
    }
}
