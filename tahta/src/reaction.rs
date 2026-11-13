//! Draggable "Tepkime" panel: balancing chemical equations and the mole
//! calculations that follow from them.
//!
//! - the equation is typed on a chemistry keypad (element keys, digits,
//!   parentheses, + and →) or picked from common reactions;
//! - "Denkleştir" finds the smallest whole-number coefficients (the
//!   null space of the element × species matrix, exact rational
//!   arithmetic) and lists every element's atom count on both sides;
//! - "Mol hesabı": molar masses of every species; tap one, type an
//!   amount in grams or moles, and every other species' moles and mass
//!   follow from the coefficients;
//! - "Tahtaya bas" stamps the balanced equation, with each species'
//!   moles and grams under it when an amount was given.

use std::collections::BTreeMap;

use glam::Vec2;

use crate::elements as el;
use crate::font_atlas::{FontAtlas, GlyphVertex};
use crate::ink::{densify, stroke_text, stroke_text_width, InkLine};
use crate::stroke::{push_circle, push_line, push_rect, Vertex};

const HEADER_H: f32 = 36.0;
const PADDING: f32 = 12.0;
const GAP: f32 = 6.0;
const EQ_H: f32 = 64.0;
const INFO_H: f32 = 30.0;
const KEY_W: f32 = 68.0;
const KEY_H: f32 = 46.0;
const KEY_COLS: usize = 9;
const RIGHT_W: f32 = 400.0;
const PRESET_H: f32 = 34.0;
const ROW_H: f32 = 32.0;

const COLOR_PANEL_BG: [f32; 4] = [0.12, 0.13, 0.17, 0.96];
const COLOR_HEADER: [f32; 4] = [0.18, 0.19, 0.24, 1.0];
const COLOR_FIELD: [f32; 4] = [0.06, 0.07, 0.09, 1.0];
const COLOR_FIELD_FOCUS: [f32; 4] = [0.23, 0.51, 0.96, 1.0];
const COLOR_TEXT: [f32; 4] = [0.95, 0.95, 0.97, 1.0];
const COLOR_TEXT_DIM: [f32; 4] = [0.95, 0.95, 0.97, 0.55];
const COLOR_COEF: [f32; 4] = [0.98, 0.75, 0.25, 1.0];
const COLOR_OK: [f32; 4] = [0.45, 0.85, 0.50, 1.0];
const COLOR_WARN: [f32; 4] = [0.98, 0.55, 0.40, 1.0];
const COLOR_KEY_IDLE: [f32; 4] = [0.22, 0.24, 0.30, 1.0];
const COLOR_KEY_ELEMENT: [f32; 4] = [0.20, 0.30, 0.42, 1.0];
const COLOR_KEY_OFF: [f32; 4] = [0.16, 0.17, 0.21, 1.0];
const COLOR_KEY_ACTIVE: [f32; 4] = [0.23, 0.51, 0.96, 1.0];
const COLOR_KEY_GO: [f32; 4] = [0.20, 0.55, 0.35, 1.0];
const COLOR_KEY_STAMP: [f32; 4] = [0.98, 0.55, 0.15, 1.0];
const COLOR_KEY_CLEAR: [f32; 4] = [0.55, 0.30, 0.30, 1.0];

const ELEMENT_KEYS: [&str; 36] = [
    "H", "He", "Li", "Be", "B", "C", "N", "O", "F", //
    "Ne", "Na", "Mg", "Al", "Si", "P", "S", "Cl", "Ar", //
    "K", "Ca", "Cr", "Mn", "Fe", "Co", "Ni", "Cu", "Zn", //
    "Br", "Ag", "Sn", "I", "Ba", "Pt", "Au", "Hg", "Pb",
];
const DIGIT_KEYS: [&str; 9] = ["1", "2", "3", "4", "5", "6", "7", "8", "9"];
const SIGN_KEYS: [&str; 9] = ["0", ",", "(", ")", "+", "→", "⌫", "Sil", ""];

const PRESETS: [&str; 12] = [
    "H2 + O2 → H2O",
    "N2 + H2 → NH3",
    "Fe + O2 → Fe2O3",
    "Na + H2O → NaOH + H2",
    "Al + HCl → AlCl3 + H2",
    "CH4 + O2 → CO2 + H2O",
    "C3H8 + O2 → CO2 + H2O",
    "KClO3 → KCl + O2",
    "CaCO3 → CaO + CO2",
    "Fe2O3 + CO → Fe + CO2",
    "C6H12O6 + O2 → CO2 + H2O",
    "Cu + HNO3 → Cu(NO3)2 + NO + H2O",
];

// --- Chemistry ---------------------------------------------------------------

/// Atom counts of a formula like "Cu(NO3)2"; `Err` names the problem.
pub fn parse_formula(f: &str) -> Result<BTreeMap<String, u32>, String> {
    fn group(chars: &[char], i: &mut usize, depth: u32) -> Result<BTreeMap<String, u32>, String> {
        let mut out: BTreeMap<String, u32> = BTreeMap::new();
        while *i < chars.len() {
            let c = chars[*i];
            let part = if c == '(' {
                *i += 1;
                let inner = group(chars, i, depth + 1)?;
                if *i >= chars.len() || chars[*i] != ')' {
                    return Err("Parantez kapanmamış".into());
                }
                *i += 1;
                inner
            } else if c == ')' {
                if depth == 0 {
                    return Err("Fazla kapanan parantez".into());
                }
                return Ok(out);
            } else if c.is_ascii_uppercase() {
                let mut sym = c.to_string();
                *i += 1;
                if *i < chars.len() && chars[*i].is_ascii_lowercase() {
                    sym.push(chars[*i]);
                    *i += 1;
                }
                if !el::ELEMENTS.iter().any(|e| e.0 == sym) {
                    return Err(format!("Bilinmeyen element: {sym}"));
                }
                BTreeMap::from([(sym, 1)])
            } else {
                return Err(format!("Formülde beklenmeyen işaret: {c}"));
            };
            let mut n = 0u32;
            while *i < chars.len() && chars[*i].is_ascii_digit() {
                n = n * 10 + chars[*i].to_digit(10).unwrap();
                *i += 1;
            }
            let n = if n == 0 { 1 } else { n };
            for (k, v) in part {
                *out.entry(k).or_default() += v * n;
            }
        }
        Ok(out)
    }
    let chars: Vec<char> = f.chars().collect();
    if chars.is_empty() {
        return Err("Boş formül".into());
    }
    let mut i = 0;
    let out = group(&chars, &mut i, 0)?;
    if i < chars.len() {
        return Err("Fazla kapanan parantez".into());
    }
    Ok(out)
}

/// Molar mass in g/mol.
pub fn molar_mass(counts: &BTreeMap<String, u32>) -> f64 {
    counts
        .iter()
        .map(|(sym, &n)| {
            let z = el::ELEMENTS.iter().position(|e| e.0 == sym).unwrap();
            let m: f64 = el::ELEMENTS[z].2.trim_matches(|c| c == '[' || c == ']').parse().unwrap_or(0.0);
            m * n as f64
        })
        .sum()
}

/// Splits "Fe + O2 → Fe2O3" into reactant and product formulas (leading
/// coefficients someone typed are dropped).
pub fn split_equation(eq: &str) -> Result<(Vec<String>, Vec<String>), String> {
    let mut sides = eq.split('→');
    let (Some(left), Some(right), None) = (sides.next(), sides.next(), sides.next()) else {
        return Err(if eq.contains('→') { "Birden çok ok (→) var".into() } else { "Ok (→) yok".into() });
    };
    let parse_side = |s: &str| -> Result<Vec<String>, String> {
        let items: Vec<String> = s
            .split('+')
            .map(|t| t.trim().trim_start_matches(|c: char| c.is_ascii_digit()).to_string())
            .collect();
        if items.iter().any(|t| t.is_empty()) {
            return Err("Eksik madde (+ ya da → yanında boşluk)".into());
        }
        Ok(items)
    };
    Ok((parse_side(left)?, parse_side(right)?))
}

fn gcd(a: i128, b: i128) -> i128 {
    if b == 0 { a.abs() } else { gcd(b, a % b) }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Frac(i128, i128);

impl Frac {
    fn new(n: i128, d: i128) -> Self {
        let g = gcd(n, d).max(1);
        let s = if d < 0 { -1 } else { 1 };
        Frac(s * n / g, s * d / g)
    }
    fn zero(self) -> bool {
        self.0 == 0
    }
    fn sub(self, o: Frac) -> Frac {
        Frac::new(self.0 * o.1 - o.0 * self.1, self.1 * o.1)
    }
    fn mul(self, o: Frac) -> Frac {
        Frac::new(self.0 * o.0, self.1 * o.1)
    }
    fn div(self, o: Frac) -> Frac {
        Frac::new(self.0 * o.1, self.1 * o.0)
    }
}

#[derive(Debug)]
pub struct Balanced {
    pub reactants: Vec<String>,
    pub products: Vec<String>,
    /// One per species, reactants first.
    pub coefficients: Vec<u32>,
    pub molar_masses: Vec<f64>,
    /// (element, left count, right count).
    pub atom_counts: Vec<(String, u32, u32)>,
}

impl Balanced {
    pub fn species(&self) -> impl Iterator<Item = &String> {
        self.reactants.iter().chain(self.products.iter())
    }
}

pub fn balance(eq: &str) -> Result<Balanced, String> {
    let (reactants, products) = split_equation(eq)?;
    let mut counts = Vec::new();
    for f in reactants.iter().chain(products.iter()) {
        counts.push(parse_formula(f).map_err(|e| format!("{f}: {e}"))?);
    }
    let n_left = reactants.len();
    let elements: Vec<String> = {
        let mut all: Vec<String> = counts.iter().flat_map(|c| c.keys().cloned()).collect();
        all.sort();
        all.dedup();
        all
    };
    for e in &elements {
        let left = counts[..n_left].iter().any(|c| c.contains_key(e));
        let right = counts[n_left..].iter().any(|c| c.contains_key(e));
        if left != right {
            return Err(format!("{e} yalnız bir tarafta var"));
        }
    }
    // Rows = elements, columns = species (products negative).
    let n = counts.len();
    let mut m: Vec<Vec<Frac>> = elements
        .iter()
        .map(|e| {
            (0..n)
                .map(|j| {
                    let v = *counts[j].get(e).unwrap_or(&0) as i128;
                    Frac::new(if j < n_left { v } else { -v }, 1)
                })
                .collect()
        })
        .collect();
    // Reduced row echelon form.
    let mut pivots = Vec::new();
    let mut row = 0;
    for col in 0..n {
        let Some(p) = (row..m.len()).find(|&r| !m[r][col].zero()) else { continue };
        m.swap(row, p);
        let pv = m[row][col];
        for c in 0..n {
            m[row][c] = m[row][c].div(pv);
        }
        for r in 0..m.len() {
            if r != row && !m[r][col].zero() {
                let f = m[r][col];
                for c in 0..n {
                    let v = m[row][c].mul(f);
                    m[r][c] = m[r][c].sub(v);
                }
            }
        }
        pivots.push(col);
        row += 1;
        if row == m.len() {
            break;
        }
    }
    let free: Vec<usize> = (0..n).filter(|c| !pivots.contains(c)).collect();
    if free.len() != 1 {
        return Err(if free.is_empty() { "Denkleştirilemedi".into() } else { "Tek bir denkleştirme yok (madde fazla)".into() });
    }
    let f = free[0];
    let mut x = vec![Frac(0, 1); n];
    x[f] = Frac(1, 1);
    for (r, &pc) in pivots.iter().enumerate() {
        x[pc] = Frac(0, 1).sub(m[r][f]);
    }
    let lcm = x.iter().fold(1i128, |acc, v| acc / gcd(acc, v.1) * v.1);
    let mut ints: Vec<i128> = x.iter().map(|v| v.0 * (lcm / v.1)).collect();
    if ints.iter().all(|&v| v <= 0) {
        ints.iter_mut().for_each(|v| *v = -*v);
    }
    if ints.iter().any(|&v| v <= 0) {
        return Err("Denkleştirilemedi".into());
    }
    let g = ints.iter().fold(0, |acc, &v| gcd(acc, v));
    let coefficients: Vec<u32> = ints.iter().map(|&v| (v / g) as u32).collect();
    let atom_counts = elements
        .iter()
        .map(|e| {
            let side = |range: std::ops::Range<usize>| -> u32 { range.map(|j| coefficients[j] * counts[j].get(e).unwrap_or(&0)).sum() };
            (e.clone(), side(0..n_left), side(n_left..n))
        })
        .collect();
    let molar_masses = counts.iter().map(molar_mass).collect();
    Ok(Balanced { reactants, products, coefficients, molar_masses, atom_counts })
}

/// Formula with Unicode subscripts: "Fe2O3" → "Fe₂O₃".
pub fn pretty(formula: &str) -> String {
    formula.chars().map(|c| c.to_digit(10).map_or(c, |d| ['₀', '₁', '₂', '₃', '₄', '₅', '₆', '₇', '₈', '₉'][d as usize])).collect()
}

/// Turkish number: decimal comma, a few significant decimals.
pub fn number(x: f64) -> String {
    let decimals = if x.abs() >= 100.0 { 1 } else if x.abs() >= 1.0 { 2 } else { 3 };
    let s = format!("{x:.decimals$}");
    let s = if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s };
    s.replace('.', ",")
}

// --- Panel -------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Focus {
    Equation,
    Amount,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Unit {
    Gram,
    Mol,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ReactionPress {
    Move,
    Close,
    Stamp,
    Other,
}

pub struct Reaction {
    pub position: Vec2,
    /// The typed equation as keypad tokens ("Fe", "2", "+", "→", …).
    tokens: Vec<String>,
    result: Option<Result<Balanced, String>>,
    focus: Focus,
    given: Option<usize>,
    amount: String,
    unit: Unit,
}

impl Reaction {
    pub fn new(screen_size: Vec2) -> Self {
        let mut r = Self {
            position: Vec2::ZERO,
            tokens: Vec::new(),
            result: None,
            focus: Focus::Equation,
            given: None,
            amount: String::new(),
            unit: Unit::Gram,
        };
        r.set_equation(PRESETS[2]);
        let size = Vec2::new(r.panel_width(), r.panel_height());
        r.position = Vec2::new((screen_size.x - size.x) / 2.0, ((screen_size.y - 120.0 - size.y) / 2.0).max(12.0));
        r
    }

    fn set_equation(&mut self, eq: &str) {
        self.tokens.clear();
        for word in eq.split(' ') {
            let chars: Vec<char> = word.chars().collect();
            let mut i = 0;
            while i < chars.len() {
                let mut t = chars[i].to_string();
                if chars[i].is_ascii_uppercase() && i + 1 < chars.len() && chars[i + 1].is_ascii_lowercase() {
                    t.push(chars[i + 1]);
                    i += 1;
                }
                self.tokens.push(t);
                i += 1;
            }
        }
        self.edited();
    }

    fn edited(&mut self) {
        self.result = None;
        self.given = None;
        self.amount.clear();
        self.focus = Focus::Equation;
    }

    fn equation(&self) -> String {
        self.tokens.concat().replace('+', " + ").replace('→', " → ")
    }

    pub fn balanced(&self) -> Option<&Balanced> {
        self.result.as_ref().and_then(|r| r.as_ref().ok())
    }

    fn panel_width(&self) -> f32 {
        PADDING * 2.0 + self.keypad_w() + 16.0 + RIGHT_W
    }

    fn keypad_w(&self) -> f32 {
        KEY_COLS as f32 * KEY_W + (KEY_COLS as f32 - 1.0) * GAP
    }

    fn body_top(&self) -> f32 {
        self.position.y + HEADER_H + PADDING + EQ_H + GAP + INFO_H + 8.0
    }

    fn panel_height(&self) -> f32 {
        self.body_top() - self.position.y + 6.0 * (PRESET_H + 4.0) + 12.0 + 36.0 + 6.0 * ROW_H + 8.0 + KEY_H + PADDING
    }

    fn header_rect(&self) -> (Vec2, Vec2) {
        (self.position, Vec2::new(self.panel_width(), HEADER_H))
    }

    fn close_rect(&self) -> (Vec2, Vec2) {
        (self.position + Vec2::new(self.panel_width() - HEADER_H, 0.0), Vec2::splat(HEADER_H))
    }

    fn eq_rect(&self) -> (Vec2, Vec2) {
        (self.position + Vec2::new(PADDING, HEADER_H + PADDING), Vec2::new(self.panel_width() - 2.0 * PADDING, EQ_H))
    }

    fn info_pos(&self) -> Vec2 {
        self.position + Vec2::new(PADDING + 4.0, HEADER_H + PADDING + EQ_H + GAP + 6.0)
    }

    /// Key `i` of row `row` (rows 0-3 elements, 4 digits, 5 signs).
    fn key_rect(&self, row: usize, col: usize) -> (Vec2, Vec2) {
        let x = self.position.x + PADDING + col as f32 * (KEY_W + GAP);
        let y = self.body_top() + row as f32 * (KEY_H + GAP);
        (Vec2::new(x, y), Vec2::new(KEY_W, KEY_H))
    }

    fn balance_rect(&self) -> (Vec2, Vec2) {
        let (pos, _) = self.key_rect(6, 0);
        (pos, Vec2::new(self.keypad_w(), KEY_H))
    }

    fn right_x(&self) -> f32 {
        self.position.x + PADDING + self.keypad_w() + 16.0
    }

    fn preset_rect(&self, i: usize) -> (Vec2, Vec2) {
        let w = (RIGHT_W - GAP) / 2.0;
        let (row, col) = (i / 2, i % 2);
        (Vec2::new(self.right_x() + col as f32 * (w + GAP), self.body_top() + row as f32 * (PRESET_H + 4.0)), Vec2::new(w, PRESET_H))
    }

    fn mol_top(&self) -> f32 {
        self.body_top() + 6.0 * (PRESET_H + 4.0) + 12.0
    }

    fn unit_rect(&self, unit: Unit) -> (Vec2, Vec2) {
        let x = self.right_x() + RIGHT_W - if unit == Unit::Gram { 2.0 * 56.0 + GAP } else { 56.0 };
        (Vec2::new(x, self.mol_top()), Vec2::new(56.0, 30.0))
    }

    fn row_rect(&self, i: usize) -> (Vec2, Vec2) {
        (Vec2::new(self.right_x(), self.mol_top() + 36.0 + 22.0 + i as f32 * ROW_H), Vec2::new(RIGHT_W, ROW_H - 2.0))
    }

    fn stamp_rect(&self) -> (Vec2, Vec2) {
        let bottom = self.position.y + self.panel_height() - PADDING;
        (Vec2::new(self.right_x(), bottom - KEY_H), Vec2::new(RIGHT_W, KEY_H))
    }

    pub fn contains(&self, p: Vec2) -> bool {
        rect_contains((self.position, Vec2::new(self.panel_width(), self.panel_height())), p)
    }

    pub fn drag_to(&mut self, p: Vec2, grab_offset: Vec2) {
        self.position = p - grab_offset;
    }

    pub fn press_at(&mut self, p: Vec2) -> ReactionPress {
        if rect_contains(self.close_rect(), p) {
            return ReactionPress::Close;
        }
        if rect_contains(self.header_rect(), p) {
            return ReactionPress::Move;
        }
        if rect_contains(self.stamp_rect(), p) {
            if self.result.is_none() {
                self.result = Some(balance(&self.equation()));
            }
            return ReactionPress::Stamp;
        }
        if rect_contains(self.eq_rect(), p) {
            self.focus = Focus::Equation;
            return ReactionPress::Other;
        }
        if rect_contains(self.balance_rect(), p) {
            self.result = Some(balance(&self.equation()));
            self.given = None;
            self.amount.clear();
            return ReactionPress::Other;
        }
        for (i, preset) in PRESETS.iter().enumerate() {
            if rect_contains(self.preset_rect(i), p) {
                self.set_equation(preset);
                return ReactionPress::Other;
            }
        }
        for unit in [Unit::Gram, Unit::Mol] {
            if rect_contains(self.unit_rect(unit), p) {
                self.unit = unit;
                return ReactionPress::Other;
            }
        }
        if let Some(n) = self.balanced().map(|b| b.coefficients.len()) {
            if let Some(i) = (0..n).find(|&i| rect_contains(self.row_rect(i), p)) {
                if self.given != Some(i) {
                    self.amount.clear();
                }
                self.given = Some(i);
                self.focus = Focus::Amount;
                return ReactionPress::Other;
            }
        }
        let rows: [&[&str]; 6] = [&ELEMENT_KEYS[0..9], &ELEMENT_KEYS[9..18], &ELEMENT_KEYS[18..27], &ELEMENT_KEYS[27..36], &DIGIT_KEYS, &SIGN_KEYS];
        for (row, keys) in rows.iter().enumerate() {
            for (col, key) in keys.iter().enumerate() {
                if !key.is_empty() && rect_contains(self.key_rect(row, col), p) {
                    self.key(key);
                    return ReactionPress::Other;
                }
            }
        }
        ReactionPress::Other
    }

    fn key(&mut self, key: &str) {
        match self.focus {
            Focus::Amount => match key {
                "⌫" => {
                    self.amount.pop();
                }
                "Sil" => self.amount.clear(),
                "," if !self.amount.contains(',') => self.amount.push(','),
                k if k.len() == 1 && k.chars().all(|c| c.is_ascii_digit()) && self.amount.len() < 9 => self.amount.push_str(k),
                _ => {}
            },
            Focus::Equation => {
                match key {
                    "⌫" => {
                        self.tokens.pop();
                    }
                    "Sil" => self.tokens.clear(),
                    "," => return,
                    k => self.tokens.push(k.to_string()),
                }
                self.edited();
            }
        }
    }

    fn amount_value(&self) -> Option<f64> {
        self.amount.replace(',', ".").parse::<f64>().ok().filter(|v| *v > 0.0)
    }

    /// (mol, gram) per species once an amount is given.
    fn amounts(&self) -> Option<Vec<(f64, f64)>> {
        let b = self.balanced()?;
        let g = self.given?;
        let value = self.amount_value()?;
        let n_given = match self.unit {
            Unit::Mol => value,
            Unit::Gram => value / b.molar_masses[g],
        };
        Some(
            b.coefficients
                .iter()
                .zip(&b.molar_masses)
                .map(|(&c, &m)| {
                    let n = n_given * c as f64 / b.coefficients[g] as f64;
                    (n, n * m)
                })
                .collect(),
        )
    }

    // --- Ink -------------------------------------------------------------

    pub fn stamp_lines(&self, screen_size: Vec2, ink: [f32; 4]) -> Option<Vec<InkLine>> {
        let b = self.balanced()?;
        let amounts = self.amounts();
        let h = 44.0;
        let sub = h * 0.6;
        // Lay out species widths first, then center the line.
        let species: Vec<(String, String)> = b
            .species()
            .zip(&b.coefficients)
            .map(|(f, &c)| (if c == 1 { String::new() } else { c.to_string() }, f.clone()))
            .collect();
        let formula_w = |f: &str| -> f32 {
            f.chars().map(|c| if c.is_ascii_digit() { stroke_text_width("0", sub) + 2.0 } else { stroke_text_width(&c.to_string(), h) + 0.2 * h }).sum()
        };
        let sep_w = 2.2 * h;
        let mut widths = Vec::new();
        for (coef, f) in &species {
            let cw = if coef.is_empty() { 0.0 } else { stroke_text_width(coef, h) + 0.25 * h };
            widths.push((cw, formula_w(f)));
        }
        let total: f32 = widths.iter().map(|(a, b)| a + b).sum::<f32>() + sep_w * (species.len() - 1) as f32;
        let x0 = ((screen_size.x - total) / 2.0).max(20.0);
        let y0 = (screen_size.y * 0.33).max(60.0);
        let mut lines = Vec::new();
        let mut add = |points: Vec<Vec2>, width: f32| {
            if points.len() >= 2 {
                lines.push(InkLine { points: densify(&points, 3.0), color: ink, width });
            }
        };
        let mut x = x0;
        let n_left = b.reactants.len();
        for (i, ((coef, f), (cw, fw))) in species.iter().zip(&widths).enumerate() {
            if i > 0 {
                let mid = Vec2::new(x + sep_w / 2.0, y0 + h * 0.5);
                if i == n_left {
                    let (a, e) = (mid - Vec2::new(h * 0.7, 0.0), mid + Vec2::new(h * 0.7, 0.0));
                    add(vec![a, e], 3.0);
                    add(vec![e - Vec2::new(h * 0.25, h * 0.2), e, e - Vec2::new(h * 0.25, -h * 0.2)], 3.0);
                } else {
                    for g in stroke_text("+", mid - Vec2::new(stroke_text_width("+", h) / 2.0, h * 0.55), h) {
                        add(g, 3.0);
                    }
                }
                x += sep_w;
            }
            let start = x;
            for g in stroke_text(coef, Vec2::new(x, y0), h) {
                add(g, 3.0);
            }
            x += cw;
            for c in f.chars() {
                let s = c.to_string();
                if c.is_ascii_digit() {
                    for g in stroke_text(&s, Vec2::new(x, y0 + h * 0.62), sub) {
                        add(g, 2.4);
                    }
                    x += stroke_text_width("0", sub) + 2.0;
                } else {
                    for g in stroke_text(&s, Vec2::new(x, y0), h) {
                        add(g, 3.0);
                    }
                    x += stroke_text_width(&s, h) + 0.2 * h;
                }
            }
            if let Some(am) = &amounts {
                let (n, m) = am[i];
                let center = start + (cw + fw) / 2.0;
                let th = 26.0;
                for (k, t) in [format!("{} mol", number(n)), format!("{} g", number(m))].iter().enumerate() {
                    let w = stroke_text_width(t, th);
                    for g in stroke_text(t, Vec2::new(center - w / 2.0, y0 + h * 1.8 + k as f32 * th * 1.6), th) {
                        add(g, 2.0);
                    }
                }
            }
        }
        Some(lines)
    }

    // --- Drawing ---------------------------------------------------------

    pub fn render(
        &self,
        font_atlas: &FontAtlas,
        out_vertices: &mut Vec<Vertex>,
        out_indices: &mut Vec<u32>,
        glyph_vertices: &mut Vec<GlyphVertex>,
        glyph_indices: &mut Vec<u32>,
    ) {
        push_rect(self.position, Vec2::new(self.panel_width(), self.panel_height()), COLOR_PANEL_BG, out_vertices, out_indices);
        let (header_pos, header_size) = self.header_rect();
        push_rect(header_pos, header_size, COLOR_HEADER, out_vertices, out_indices);
        font_atlas.push_text("Tepkime denkleştirme ve mol hesabı", header_pos + Vec2::new(PADDING, 10.0), 16.0, COLOR_TEXT, glyph_vertices, glyph_indices);
        let grip = header_pos + header_size / 2.0;
        for i in -1..=1 {
            push_circle(Vec2::new(grip.x + i as f32 * 10.0, grip.y), 2.0, COLOR_TEXT, 8, out_vertices, out_indices);
        }
        let (close_pos, close_size) = self.close_rect();
        let c = close_pos + close_size / 2.0;
        let r = close_size.x * 0.2;
        push_line(c - Vec2::splat(r), c + Vec2::splat(r), 2.5, COLOR_TEXT, out_vertices, out_indices);
        push_line(c + Vec2::new(-r, r), c + Vec2::new(r, -r), 2.5, COLOR_TEXT, out_vertices, out_indices);

        // Equation field: the balanced equation with its coefficients, or
        // what's being typed.
        let (epos, esize) = self.eq_rect();
        if self.focus == Focus::Equation {
            push_rect(epos - Vec2::splat(2.0), esize + Vec2::splat(4.0), COLOR_FIELD_FOCUS, out_vertices, out_indices);
        }
        push_rect(epos, esize, COLOR_FIELD, out_vertices, out_indices);
        let h = 32.0;
        let y = epos.y + (esize.y - h) / 2.0;
        let mut x = epos.x + 16.0;
        if let Some(b) = self.balanced() {
            let n_left = b.reactants.len();
            for (i, (f, &coef)) in b.species().zip(&b.coefficients).enumerate() {
                if i > 0 {
                    let sep = if i == n_left { "  →  " } else { "  +  " };
                    x += font_atlas.push_text(sep, Vec2::new(x, y), h, COLOR_TEXT_DIM, glyph_vertices, glyph_indices);
                }
                if coef != 1 {
                    x += font_atlas.push_text(&coef.to_string(), Vec2::new(x, y), h, COLOR_COEF, glyph_vertices, glyph_indices) + 4.0;
                }
                x += font_atlas.push_text(&pretty(f), Vec2::new(x, y), h, COLOR_TEXT, glyph_vertices, glyph_indices);
            }
        } else {
            let text = pretty(&self.equation());
            let text = crate::font_atlas::fit_text(&text, esize.x - 40.0, true, |t| font_atlas.measure(t, h));
            x += font_atlas.push_text(&text, Vec2::new(x, y), h, COLOR_TEXT, glyph_vertices, glyph_indices);
            if self.focus == Focus::Equation {
                push_rect(Vec2::new(x + 3.0, y + 2.0), Vec2::new(2.0, h), COLOR_TEXT, out_vertices, out_indices);
            }
        }

        // Atom counts or the problem.
        let ip = self.info_pos();
        match &self.result {
            Some(Ok(b)) => {
                let mut x = ip.x;
                x += font_atlas.push_text("Denk ✓  ", ip, 16.0, COLOR_OK, glyph_vertices, glyph_indices);
                for (e, l, r) in &b.atom_counts {
                    let t = format!("{e}: {l} = {r}    ");
                    x += font_atlas.push_text(&t, Vec2::new(x, ip.y), 16.0, COLOR_TEXT_DIM, glyph_vertices, glyph_indices);
                }
            }
            Some(Err(e)) => {
                font_atlas.push_text(e, ip, 16.0, COLOR_WARN, glyph_vertices, glyph_indices);
            }
            None => {
                font_atlas.push_text("Denklemi yazın ya da hazır bir tepkime seçin, sonra Denkleştir'e dokunun.", ip, 15.0, COLOR_TEXT_DIM, glyph_vertices, glyph_indices);
            }
        }

        // Keypad.
        let amount_mode = self.focus == Focus::Amount;
        let rows: [&[&str]; 6] = [&ELEMENT_KEYS[0..9], &ELEMENT_KEYS[9..18], &ELEMENT_KEYS[18..27], &ELEMENT_KEYS[27..36], &DIGIT_KEYS, &SIGN_KEYS];
        for (row, keys) in rows.iter().enumerate() {
            for (col, key) in keys.iter().enumerate() {
                if key.is_empty() {
                    continue;
                }
                let is_element = row < 4;
                let off = (amount_mode && (is_element || matches!(*key, "(" | ")" | "+" | "→"))) || (!amount_mode && *key == ",");
                let color = if off {
                    COLOR_KEY_OFF
                } else if is_element {
                    COLOR_KEY_ELEMENT
                } else if *key == "Sil" {
                    COLOR_KEY_CLEAR
                } else {
                    COLOR_KEY_IDLE
                };
                let label = if *key == "⌫" { "" } else { key };
                button(self.key_rect(row, col), label, color, 19.0, if off { COLOR_TEXT_DIM } else { COLOR_TEXT }, font_atlas, out_vertices, out_indices, glyph_vertices, glyph_indices);
                if *key == "⌫" {
                    let (p, s) = self.key_rect(row, col);
                    let c = p + s / 2.0;
                    push_line(c + Vec2::new(-12.0, 0.0), c + Vec2::new(12.0, 0.0), 2.5, COLOR_TEXT, out_vertices, out_indices);
                    push_line(c + Vec2::new(-12.0, 0.0), c + Vec2::new(-5.0, -7.0), 2.5, COLOR_TEXT, out_vertices, out_indices);
                    push_line(c + Vec2::new(-12.0, 0.0), c + Vec2::new(-5.0, 7.0), 2.5, COLOR_TEXT, out_vertices, out_indices);
                }
            }
        }
        button(self.balance_rect(), "Denkleştir", COLOR_KEY_GO, 18.0, COLOR_TEXT, font_atlas, out_vertices, out_indices, glyph_vertices, glyph_indices);

        // Presets.
        for (i, preset) in PRESETS.iter().enumerate() {
            let rect = self.preset_rect(i);
            let label = crate::font_atlas::fit_text(&pretty(preset), rect.1.x - 12.0, false, |t| font_atlas.measure(t, 14.0));
            button(rect, &label, COLOR_KEY_IDLE, 14.0, COLOR_TEXT, font_atlas, out_vertices, out_indices, glyph_vertices, glyph_indices);
        }

        // Mole table.
        let top = self.mol_top();
        font_atlas.push_text("Mol hesabı", Vec2::new(self.right_x(), top + 6.0), 17.0, COLOR_TEXT, glyph_vertices, glyph_indices);
        for unit in [Unit::Gram, Unit::Mol] {
            let color = if self.unit == unit { COLOR_KEY_ACTIVE } else { COLOR_KEY_IDLE };
            button(self.unit_rect(unit), if unit == Unit::Gram { "g" } else { "mol" }, color, 15.0, COLOR_TEXT, font_atlas, out_vertices, out_indices, glyph_vertices, glyph_indices);
        }
        let cols = [0.0, 120.0, 220.0, 310.0];
        let hy = top + 38.0;
        for (k, t) in ["Madde", "M (g/mol)", "mol", "kütle (g)"].iter().enumerate() {
            font_atlas.push_text(t, Vec2::new(self.right_x() + 8.0 + cols[k], hy), 13.0, COLOR_TEXT_DIM, glyph_vertices, glyph_indices);
        }
        match self.balanced() {
            Some(b) => {
                let amounts = self.amounts();
                for (i, f) in b.species().enumerate().take(6) {
                    let (pos, size) = self.row_rect(i);
                    let given = self.given == Some(i);
                    push_rect(pos, size, if given { [0.20, 0.32, 0.52, 1.0] } else { COLOR_FIELD }, out_vertices, out_indices);
                    let ty = pos.y + 7.0;
                    font_atlas.push_text(&pretty(f), Vec2::new(pos.x + 8.0, ty), 16.0, COLOR_TEXT, glyph_vertices, glyph_indices);
                    font_atlas.push_text(&number(b.molar_masses[i]), Vec2::new(pos.x + 8.0 + cols[1], ty), 15.0, COLOR_TEXT_DIM, glyph_vertices, glyph_indices);
                    let (n_text, m_text) = match (&amounts, given) {
                        (Some(a), _) => (number(a[i].0), number(a[i].1)),
                        (None, true) => {
                            let typed = format!("{}▏", self.amount);
                            if self.unit == Unit::Mol { (typed, String::new()) } else { (String::new(), typed) }
                        }
                        _ => (String::new(), String::new()),
                    };
                    let (nc, mc) = if given {
                        if self.unit == Unit::Mol { (COLOR_COEF, COLOR_TEXT) } else { (COLOR_TEXT, COLOR_COEF) }
                    } else {
                        (COLOR_TEXT, COLOR_TEXT)
                    };
                    font_atlas.push_text(&n_text.replace('▏', "|"), Vec2::new(pos.x + 8.0 + cols[2], ty), 15.0, nc, glyph_vertices, glyph_indices);
                    font_atlas.push_text(&m_text.replace('▏', "|"), Vec2::new(pos.x + 8.0 + cols[3], ty), 15.0, mc, glyph_vertices, glyph_indices);
                }
                if self.given.is_none() {
                    let y = self.row_rect(b.coefficients.len().min(6)).0.y + 6.0;
                    font_atlas.push_text("Miktarı bilinen maddeye dokunup değeri yazın.", Vec2::new(self.right_x(), y), 13.0, COLOR_TEXT_DIM, glyph_vertices, glyph_indices);
                }
            }
            None => {
                font_atlas.push_text("Önce denklemi denkleştirin.", Vec2::new(self.right_x(), self.row_rect(0).0.y + 6.0), 14.0, COLOR_TEXT_DIM, glyph_vertices, glyph_indices);
            }
        }
        button(self.stamp_rect(), "Tahtaya bas", COLOR_KEY_STAMP, 15.0, COLOR_TEXT, font_atlas, out_vertices, out_indices, glyph_vertices, glyph_indices);
    }
}

#[allow(clippy::too_many_arguments)]
fn button(
    (pos, size): (Vec2, Vec2),
    label: &str,
    color: [f32; 4],
    h: f32,
    text_color: [f32; 4],
    font_atlas: &FontAtlas,
    out_vertices: &mut Vec<Vertex>,
    out_indices: &mut Vec<u32>,
    glyph_vertices: &mut Vec<GlyphVertex>,
    glyph_indices: &mut Vec<u32>,
) {
    push_rect(pos, size, color, out_vertices, out_indices);
    let w = font_atlas.measure(label, h);
    font_atlas.push_text(label, pos + Vec2::new((size.x - w) / 2.0, (size.y - h) / 2.0), h, text_color, glyph_vertices, glyph_indices);
}

fn rect_contains((top_left, size): (Vec2, Vec2), point: Vec2) -> bool {
    point.x >= top_left.x && point.x <= top_left.x + size.x && point.y >= top_left.y && point.y <= top_left.y + size.y
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Vec2 = Vec2::new(1920.0, 1080.0);

    fn coefs(eq: &str) -> Vec<u32> {
        balance(eq).unwrap_or_else(|e| panic!("{eq}: {e}")).coefficients
    }

    #[test]
    fn formulas_parse_with_parentheses() {
        let c = parse_formula("Cu(NO3)2").unwrap();
        assert_eq!((c["Cu"], c["N"], c["O"]), (1, 2, 6));
        assert_eq!(parse_formula("Ca3(PO4)2").unwrap()["O"], 8);
        assert!(parse_formula("Xy2").unwrap_err().contains("Xy"));
        assert!(parse_formula("Cu(NO3").is_err());
        assert!(parse_formula("NO3)").is_err());
        assert!((molar_mass(&parse_formula("H2O").unwrap()) - 18.015).abs() < 0.01);
    }

    #[test]
    fn every_preset_balances() {
        assert_eq!(coefs("H2 + O2 → H2O"), vec![2, 1, 2]);
        assert_eq!(coefs("Fe + O2 → Fe2O3"), vec![4, 3, 2]);
        assert_eq!(coefs("C3H8 + O2 → CO2 + H2O"), vec![1, 5, 3, 4]);
        assert_eq!(coefs("KClO3 → KCl + O2"), vec![2, 2, 3]);
        assert_eq!(coefs("Cu + HNO3 → Cu(NO3)2 + NO + H2O"), vec![3, 8, 3, 2, 4]);
        assert_eq!(coefs("C6H12O6 + O2 → CO2 + H2O"), vec![1, 6, 6, 6]);
        for p in PRESETS {
            let b = balance(p).unwrap();
            assert!(b.atom_counts.iter().all(|(_, l, r)| l == r), "{p}");
        }
        // Typed coefficients are ignored.
        assert_eq!(coefs("3H2 + N2 → 7NH3"), vec![3, 1, 2]);
    }

    #[test]
    fn bad_equations_explain_themselves() {
        assert!(balance("H2 + O2").unwrap_err().contains("Ok"));
        assert!(balance("H2 → O2").unwrap_err().contains("yalnız bir tarafta"));
        assert!(balance("H2 + → H2O").is_err());
    }

    #[test]
    fn keypad_typing_balancing_and_moles() {
        let mut r = Reaction::new(SCREEN);
        let c = |rect: (Vec2, Vec2)| rect.0 + rect.1 / 2.0;
        // Clear, then type "H2 + O2 → H2O" on the keypad.
        r.press_at(c(r.key_rect(5, 7)));
        assert!(r.tokens.is_empty());
        let key = |r: &mut Reaction, name: &str| {
            let rows: [&[&str]; 6] = [&ELEMENT_KEYS[0..9], &ELEMENT_KEYS[9..18], &ELEMENT_KEYS[18..27], &ELEMENT_KEYS[27..36], &DIGIT_KEYS, &SIGN_KEYS];
            let (row, col) = rows.iter().enumerate().find_map(|(ri, ks)| ks.iter().position(|k| *k == name).map(|ci| (ri, ci))).unwrap();
            r.press_at(c(r.key_rect(row, col)));
        };
        for k in ["H", "2", "+", "O", "2", "→", "H", "2", "O"] {
            key(&mut r, k);
        }
        assert_eq!(r.equation(), "H2 + O2 → H2O");
        r.press_at(c(r.balance_rect()));
        assert_eq!(r.balanced().unwrap().coefficients, vec![2, 1, 2]);
        // 4 g of H₂ = 1,98 mol → as much H₂O = 35,74 g.
        r.press_at(c(r.row_rect(0)));
        assert_eq!(r.focus, Focus::Amount);
        key(&mut r, "H"); // ignored while typing an amount
        key(&mut r, "4");
        let a = r.amounts().unwrap();
        assert!((a[0].0 - 1.984).abs() < 0.01, "{:?}", a[0]);
        assert_eq!(number(a[2].1), "35,74");
        r.press_at(c(r.unit_rect(Unit::Mol)));
        let a = r.amounts().unwrap();
        assert_eq!((number(a[1].0), number(a[2].0)), ("2".to_string(), "4".to_string()));
        assert_eq!(r.press_at(c(r.stamp_rect())), ReactionPress::Stamp);
        assert!(r.tokens.len() == 9, "the equation stays");
    }

    #[test]
    fn stamp_fits_and_presets_fill_the_field() {
        let mut r = Reaction::new(SCREEN);
        assert!(r.position.y + r.panel_height() <= SCREEN.y - 75.0, "{}", r.position.y + r.panel_height());
        let c = |rect: (Vec2, Vec2)| rect.0 + rect.1 / 2.0;
        assert!(r.stamp_lines(SCREEN, [1.0; 4]).is_none(), "nothing balanced yet");
        r.press_at(c(r.preset_rect(11)));
        assert_eq!(r.equation(), "Cu + HNO3 → Cu(NO3)2 + NO + H2O");
        r.press_at(c(r.stamp_rect()));
        r.press_at(c(r.row_rect(0)));
        r.press_at(c(r.key_rect(4, 5))); // 6 g Cu
        let lines = r.stamp_lines(SCREEN, [1.0; 4]).unwrap();
        for l in &lines {
            for p in &l.points {
                assert!(p.x >= 0.0 && p.x <= SCREEN.x && p.y >= 0.0 && p.y <= SCREEN.y - 70.0, "{p:?}");
            }
        }
        assert_eq!(number(123.456), "123,5");
        assert_eq!(number(0.5), "0,5");
    }
}
