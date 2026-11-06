//! Handwritten formula recognition for the "Grafik" panel's "Tahtadan
//! oku": the teacher writes `y = x² − 3` on the board with the pen, draws
//! a rectangle around it, and the formula lands in the panel.
//!
//! Runs fully offline, only when asked (a few ms; nothing while idle):
//!
//! 1. **Symbols.** Strokes are grouped into symbols in writing order by a
//!    small dynamic program over groups of 1–3 consecutive strokes that
//!    overlap horizontally: each group is scored by its best class, with a
//!    per-symbol penalty so `+` (two strokes) beats `−` followed by `1`.
//!    Dots, flat dashes (`−`) and two stacked dashes (`=`) are recognized
//!    by shape; everything else by a tiny CNN (`assets/model`, trained
//!    with `tools/el-yazisi` on MNIST, EMNIST and HASYv2 — see its
//!    README for sources and licenses).
//! 2. **Layout.** Fractions (a long dash with symbols above and below),
//!    square roots (symbols under `√`), and exponents (a smaller symbol
//!    raised above its base's middle, `x²`).
//! 3. **Context.** Each symbol keeps its top three classes; a beam search
//!    picks the most likely string that `expr::parse` accepts — so the
//!    ambiguous pairs a classifier can't tell apart from the image alone
//!    (`o`/`0`, `l`/`1`, `t`/`+`, `g`/`9`, `s`/`5`) resolve themselves:
//!    `c0s` isn't a formula, `cos` is.
//!
//! The CNN's input rendering mirrors the training preprocessing exactly:
//! crop to the ink, fit the longer side (ink width included) into 20 px
//! keeping the aspect ratio, center in 28×28.

use std::sync::OnceLock;

use glam::Vec2;

const MODEL_BYTES: &[u8] = include_bytes!("../assets/model/semboller.bin");
const SIZE: usize = 28;
/// Ink width (px) in the 28×28 input — about MNIST's stroke width.
const PEN: f32 = 2.4;
/// Per-symbol penalty (log space) against over-segmentation.
const SYMBOL_PENALTY: f32 = 0.5;
const MAX_GROUP: usize = 3;
const TOP_K: usize = 3;
const BEAM: usize = 96;

/// The classifier: conv 1→16 3×3, ReLU, pool 2; conv 16→32 3×3, ReLU,
/// pool 2; fc 1568→128, ReLU; fc 128→C. Weights as trained (f32 LE).
pub struct Model {
    classes: Vec<String>,
    c1w: Vec<f32>,
    c1b: Vec<f32>,
    c2w: Vec<f32>,
    c2b: Vec<f32>,
    f1w: Vec<f32>,
    f1b: Vec<f32>,
    f2w: Vec<f32>,
    f2b: Vec<f32>,
}

impl Model {
    fn parse(bytes: &[u8]) -> Option<Model> {
        if bytes.get(..4)? != b"TZK1" {
            return None;
        }
        let u32_at = |o: usize| Some(u32::from_le_bytes(bytes.get(o..o + 4)?.try_into().ok()?) as usize);
        let n = u32_at(4)?;
        let names_len = u32_at(8)?;
        let names = std::str::from_utf8(bytes.get(12..12 + names_len)?).ok()?;
        let classes: Vec<String> = names.split('\n').map(String::from).collect();
        if classes.len() != n {
            return None;
        }
        let mut off = 12 + names_len;
        let mut take = |count: usize| -> Option<Vec<f32>> {
            let raw = bytes.get(off..off + count * 4)?;
            off += count * 4;
            Some(raw.chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect())
        };
        Some(Model {
            c1w: take(16 * 9)?,
            c1b: take(16)?,
            c2w: take(32 * 16 * 9)?,
            c2b: take(32)?,
            f1w: take(128 * 1568)?,
            f1b: take(128)?,
            f2w: take(n * 128)?,
            f2b: take(n)?,
            classes,
        })
    }

    fn get() -> &'static Model {
        static MODEL: OnceLock<Model> = OnceLock::new();
        MODEL.get_or_init(|| Model::parse(MODEL_BYTES).expect("bundled handwriting model is valid"))
    }

    /// Raw class scores for one 28×28 image (ink = 1).
    fn logits(&self, img: &[f32]) -> Vec<f32> {
        let a = conv_relu_pool(img, 1, SIZE, &self.c1w, &self.c1b, 16);
        let b = conv_relu_pool(&a, 16, SIZE / 2, &self.c2w, &self.c2b, 32);
        let h: Vec<f32> = (0..128)
            .map(|o| {
                let w = &self.f1w[o * 1568..(o + 1) * 1568];
                (self.f1b[o] + w.iter().zip(&b).map(|(x, y)| x * y).sum::<f32>()).max(0.0)
            })
            .collect();
        (0..self.classes.len())
            .map(|o| self.f2b[o] + self.f2w[o * 128..(o + 1) * 128].iter().zip(&h).map(|(x, y)| x * y).sum::<f32>())
            .collect()
    }

    /// Top-`k` (class text, log-probability).
    fn top(&self, img: &[f32], k: usize) -> Vec<(String, f32)> {
        let z = self.logits(img);
        let max = z.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        let lse = max + z.iter().map(|v| (v - max).exp()).sum::<f32>().ln();
        let mut idx: Vec<usize> = (0..z.len()).collect();
        idx.sort_by(|&a, &b| z[b].partial_cmp(&z[a]).unwrap_or(std::cmp::Ordering::Equal));
        idx.into_iter().take(k).map(|i| (self.classes[i].clone(), z[i] - lse)).collect()
    }
}

/// 3×3 convolution (padding 1) + ReLU + 2×2 max-pool, channel-major.
fn conv_relu_pool(x: &[f32], cin: usize, n: usize, w: &[f32], b: &[f32], cout: usize) -> Vec<f32> {
    let mut conv = vec![0.0f32; cout * n * n];
    for o in 0..cout {
        for yy in 0..n {
            for xx in 0..n {
                let mut acc = b[o];
                for c in 0..cin {
                    for ky in 0..3 {
                        let sy = yy as isize + ky as isize - 1;
                        if sy < 0 || sy >= n as isize {
                            continue;
                        }
                        for kx in 0..3 {
                            let sx = xx as isize + kx as isize - 1;
                            if sx < 0 || sx >= n as isize {
                                continue;
                            }
                            acc += w[((o * cin + c) * 3 + ky) * 3 + kx] * x[(c * n + sy as usize) * n + sx as usize];
                        }
                    }
                }
                conv[(o * n + yy) * n + xx] = acc.max(0.0);
            }
        }
    }
    let h = n / 2;
    let mut out = vec![0.0f32; cout * h * h];
    for o in 0..cout {
        for yy in 0..h {
            for xx in 0..h {
                let at = |dy: usize, dx: usize| conv[(o * n + yy * 2 + dy) * n + xx * 2 + dx];
                out[(o * h + yy) * h + xx] = at(0, 0).max(at(0, 1)).max(at(1, 0)).max(at(1, 1));
            }
        }
    }
    out
}

#[derive(Clone, Copy, Debug)]
struct Rect {
    min: Vec2,
    max: Vec2,
}

impl Rect {
    fn of(points: impl IntoIterator<Item = Vec2>) -> Rect {
        let mut r = Rect { min: Vec2::splat(f32::INFINITY), max: Vec2::splat(f32::NEG_INFINITY) };
        for p in points {
            r.min = r.min.min(p);
            r.max = r.max.max(p);
        }
        r
    }
    fn union(self, o: Rect) -> Rect {
        Rect { min: self.min.min(o.min), max: self.max.max(o.max) }
    }
    fn w(&self) -> f32 {
        self.max.x - self.min.x
    }
    fn h(&self) -> f32 {
        self.max.y - self.min.y
    }
    fn cx(&self) -> f32 {
        (self.min.x + self.max.x) / 2.0
    }
    fn cy(&self) -> f32 {
        (self.min.y + self.max.y) / 2.0
    }
}

/// Renders strokes (y down) into the classifier's 28×28 input.
fn render(strokes: &[&[Vec2]]) -> Vec<f32> {
    let r = Rect::of(strokes.iter().flat_map(|s| s.iter().copied()));
    let extent = r.w().max(r.h()).max(1e-3);
    let s = (20.0 - PEN) / extent;
    let (nw, nh) = (r.w() * s + PEN, r.h() * s + PEN);
    let origin = Vec2::new(((SIZE as f32 - nw) / 2.0).floor(), ((SIZE as f32 - nh) / 2.0).floor()) + Vec2::splat(PEN / 2.0);
    let segs: Vec<(Vec2, Vec2)> = strokes
        .iter()
        .flat_map(|st| {
            let pts: Vec<Vec2> = st.iter().map(|&p| origin + (p - r.min) * s).collect();
            if pts.len() == 1 {
                vec![(pts[0], pts[0])]
            } else {
                pts.windows(2).map(|w| (w[0], w[1])).collect()
            }
        })
        .collect();
    let mut img = vec![0.0f32; SIZE * SIZE];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
            let d = segs.iter().map(|&(a, b)| seg_dist(p, a, b)).fold(f32::INFINITY, f32::min);
            img[y * SIZE + x] = (PEN / 2.0 + 0.5 - d).clamp(0.0, 1.0);
        }
    }
    img
}

fn seg_dist(p: Vec2, a: Vec2, b: Vec2) -> f32 {
    let ab = b - a;
    let t = if ab.length_squared() < 1e-9 { 0.0 } else { ((p - a).dot(ab) / ab.length_squared()).clamp(0.0, 1.0) };
    p.distance(a + ab * t)
}

/// One recognized symbol: where it is and its candidate readings.
#[derive(Clone, Debug)]
struct Symbol {
    rect: Rect,
    cands: Vec<(String, f32)>,
}

impl Symbol {
    fn best(&self) -> &str {
        &self.cands[0].0
    }
    fn is(&self, s: &str) -> bool {
        self.cands.len() == 1 && self.cands[0].0 == s
    }
}

fn is_flat(r: &Rect, unit: f32) -> bool {
    r.h() < r.w() * 0.3 && r.w() > unit * 0.25
}

fn is_dot(r: &Rect, unit: f32) -> bool {
    r.w().max(r.h()) < unit * 0.18
}

/// Strokes → symbols (dynamic program over writing order).
fn segment(strokes: &[Vec<Vec2>], model: &Model) -> Vec<Symbol> {
    let n = strokes.len();
    let rects: Vec<Rect> = strokes.iter().map(|s| Rect::of(s.iter().copied())).collect();
    // Typical symbol size: median of the larger stroke extents, ignoring dots.
    let mut sizes: Vec<f32> = rects.iter().map(|r| r.w().max(r.h())).collect();
    sizes.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let big = sizes.last().copied().unwrap_or(1.0);
    let real: Vec<f32> = sizes.iter().copied().filter(|&v| v > big * 0.15).collect();
    let unit = real.get(real.len() / 2).copied().unwrap_or(big).max(1.0);

    // Can strokes j..=i form one symbol?
    let groupable = |j: usize, i: usize| -> bool {
        let mut acc = rects[j];
        for k in j + 1..=i {
            let r = rects[k];
            let x_overlap = r.max.x.min(acc.max.x) - r.min.x.max(acc.min.x);
            let y_gap = (r.min.y - acc.max.y).max(acc.min.y - r.max.y);
            if x_overlap < -unit * 0.1 || y_gap > unit * 0.45 {
                return false;
            }
            // A long dash only joins a stroke it touches (π's bar, a t's
            // crossbar) — never a numerator or denominator floating apart.
            let (fa, fr) = (is_flat(&acc, unit), is_flat(&r, unit));
            if fa != fr && y_gap > unit * 0.15 {
                return false;
            }
            acc = acc.union(r);
        }
        acc.w() < unit * 1.8 || is_flat(&acc, unit)
    };

    let classify = |j: usize, i: usize| -> Vec<(String, f32)> {
        let r = (j..=i).map(|k| rects[k]).reduce(Rect::union).unwrap();
        if j == i && is_dot(&r, unit) {
            return vec![(".".into(), (0.3f32).ln())];
        }
        if j == i && is_flat(&r, unit) {
            return vec![("−".into(), (0.95f32).ln())];
        }
        if i == j + 1 && is_flat(&rects[j], unit) && is_flat(&rects[i], unit) {
            return vec![("=".into(), (0.97f32).ln())];
        }
        let parts: Vec<&[Vec2]> = (j..=i).map(|k| strokes[k].as_slice()).collect();
        model.top(&render(&parts), TOP_K)
    };

    // best[i] = (score, start of last group, candidates) for strokes 0..i
    let mut best: Vec<Option<(f32, usize, Vec<(String, f32)>)>> = vec![None; n + 1];
    best[0] = Some((0.0, 0, Vec::new()));
    for end in 1..=n {
        for len in 1..=MAX_GROUP.min(end) {
            let start = end - len;
            let Some((prev, _, _)) = &best[start] else { continue };
            if len > 1 && !groupable(start, end - 1) {
                continue;
            }
            let cands = classify(start, end - 1);
            let score = prev + cands[0].1 - SYMBOL_PENALTY;
            if best[end].as_ref().map_or(true, |(s, _, _)| score > *s) {
                best[end] = Some((score, start, cands));
            }
        }
    }
    let mut out = Vec::new();
    let mut end = n;
    while end > 0 {
        let (_, start, cands) = best[end].clone().unwrap();
        let rect = (start..end).map(|k| rects[k]).reduce(Rect::union).unwrap();
        out.push(Symbol { rect, cands });
        end = start;
    }
    out.reverse();
    out
}

/// A piece of the formula: literal text, or one symbol's alternatives.
#[derive(Clone, Debug)]
enum Piece {
    Lit(String),
    Alt(Vec<(String, f32)>),
}

/// Lays symbols out left to right, turning fractions, roots and exponents
/// into the linear syntax `expr.rs` reads.
fn layout(syms: Vec<Symbol>, out: &mut Vec<Piece>) {
    if syms.is_empty() {
        return;
    }
    let median_h = {
        let mut hs: Vec<f32> = syms.iter().filter(|s| !s.is("−") && !s.is(".")).map(|s| s.rect.h()).collect();
        hs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        hs.get(hs.len() / 2).copied().unwrap_or(1.0).max(1.0)
    };

    // Fractions, widest bar first (a main fraction bar spans the inner
    // ones): a dash with symbols both above and below it becomes one
    // stand-in symbol whose text is laid out recursively.
    let mut syms = syms;
    let mut fracs: Vec<Vec<Piece>> = Vec::new();
    loop {
        let mut bars: Vec<usize> = (0..syms.len()).filter(|&i| syms[i].is("−")).collect();
        bars.sort_by(|&a, &b| syms[b].rect.w().partial_cmp(&syms[a].rect.w()).unwrap());
        let found = bars.into_iter().find_map(|bi| {
            let bar = syms[bi].rect;
            // A numerator/denominator sits over the bar; '=' or '+' merely
            // overlapping its end belongs to the line, not the fraction.
            let within = |s: &Symbol| {
                let over = s.rect.max.x.min(bar.max.x) - s.rect.min.x.max(bar.min.x);
                s.rect.cx() > bar.min.x && s.rect.cx() < bar.max.x && over > s.rect.w() * 0.6 && !s.is("=") && s.cands[0].0 != "+"
            };
            let above: Vec<usize> = (0..syms.len()).filter(|&i| i != bi && within(&syms[i]) && syms[i].rect.cy() < bar.cy()).collect();
            let below: Vec<usize> = (0..syms.len()).filter(|&i| i != bi && within(&syms[i]) && syms[i].rect.cy() > bar.cy()).collect();
            (!above.is_empty() && !below.is_empty()).then_some((bi, above, below))
        });
        let Some((bi, above, below)) = found else { break };
        let pick = |ids: &[usize]| ids.iter().map(|&i| syms[i].clone()).collect::<Vec<_>>();
        let (num, den) = (pick(&above), pick(&below));
        let mut text = Vec::new();
        for (part, sep) in [(num.clone(), "÷"), (den.clone(), "")] {
            let wrap = part.len() > 1;
            if wrap {
                text.push(Piece::Lit("(".into()));
            }
            layout(part, &mut text);
            if wrap {
                text.push(Piece::Lit(")".into()));
            }
            text.push(Piece::Lit(sep.into()));
        }
        let rect = num.iter().chain(&den).fold(syms[bi].rect, |r, s| r.union(s.rect));
        let stand_in = Symbol { rect, cands: vec![(format!("\u{0}{}", fracs.len()), 0.0)] };
        fracs.push(text);
        syms = syms
            .into_iter()
            .enumerate()
            .filter(|(i, _)| *i != bi && !above.contains(i) && !below.contains(i))
            .map(|(_, s)| s)
            .chain(std::iter::once(stand_in))
            .collect();
    }
    layout_line(syms, out, median_h, &fracs);
}

/// One baseline of symbols: roots, exponents, left-to-right order.
fn layout_line(mut syms: Vec<Symbol>, out: &mut Vec<Piece>, median_h: f32, fracs: &[Vec<Piece>]) {
    syms.sort_by(|a, b| a.rect.min.x.partial_cmp(&b.rect.min.x).unwrap());
    let mut i = 0;
    let mut base: Option<Rect> = None;
    let mut in_sup = false;
    while i < syms.len() {
        let s = syms[i].clone();
        if let Some(idx) = s.best().strip_prefix('\u{0}').and_then(|n| n.parse::<usize>().ok()) {
            out.extend(fracs[idx].iter().cloned());
            base = Some(s.rect);
            i += 1;
            continue;
        }
        if s.cands[0].0 == "√" && s.cands[0].1 > (0.5f32).ln() {
            // Everything starting inside the root's span is its radicand.
            let span = s.rect;
            let mut inner = Vec::new();
            let mut k = i + 1;
            while k < syms.len() && syms[k].rect.cx() < span.max.x && syms[k].rect.min.x > span.min.x + span.w() * 0.15 {
                inner.push(syms[k].clone());
                k += 1;
            }
            out.push(Piece::Lit("√(".into()));
            layout_line(inner, out, median_h, fracs);
            out.push(Piece::Lit(")".into()));
            base = Some(span);
            i = k;
            continue;
        }
        let operator = s.is("−") || s.is("=") || s.is(".") || s.cands[0].0 == "+";
        let raised = match base {
            Some(b) if !operator => s.rect.max.y < b.min.y + b.h() * 0.5 && s.rect.h() < b.h().max(median_h * 0.6) * 0.85,
            _ => false,
        };
        // An operator back down at the base's height ends the exponent
        // (`x² − 3`); one floating above the base stays in it (`x⁻¹`).
        let back_down = match base {
            Some(b) => if operator { s.rect.cy() > b.min.y } else { !raised },
            None => true,
        };
        if raised && !in_sup {
            out.push(Piece::Lit("^(".into()));
            in_sup = true;
        } else if in_sup && back_down {
            out.push(Piece::Lit(")".into()));
            in_sup = false;
        }
        out.push(Piece::Alt(s.cands.clone()));
        if !in_sup && !operator {
            base = Some(s.rect);
        }
        i += 1;
    }
    if in_sup {
        out.push(Piece::Lit(")".into()));
    }
}

/// Prettier, equivalent spelling: `x^(2)` → `x²`, `√(x)` → `√x`.
fn tidy(s: &str) -> String {
    let s = s.replace("^(2)", "²").replace("^(3)", "³");
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '√' && chars.get(i + 1) == Some(&'(') && chars.get(i + 3) == Some(&')') && chars[i + 2] != ')' {
            out.push('√');
            out.push(chars[i + 2]);
            i += 4;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// The right-hand side of an equation: `y = …` → `…`.
fn rhs(s: &str) -> &str {
    s.rsplit('=').next().unwrap_or(s)
}

/// School-notation prior on top of the classifier's scores: function
/// names are likely (`cos`, not `c0s` — both parse, `c` is a parameter),
/// a digit right after a letter is not (coefficients come first: `2x`).
fn notation_prior(s: &str) -> f32 {
    const NAMES: [&str; 6] = ["sin", "cos", "tan", "cot", "log", "ln"];
    let names = NAMES.iter().map(|n| s.matches(n).count()).sum::<usize>() as f32;
    let chars: Vec<char> = s.chars().collect();
    let letter_digit = chars.windows(2).filter(|w| w[0].is_alphabetic() && w[1].is_ascii_digit()).count() as f32;
    1.2 * names - 1.0 * letter_digit
}

/// Beam search over the symbols' alternatives for the likeliest string
/// the formula parser accepts.
fn decode(pieces: &[Piece]) -> Option<String> {
    let mut beam: Vec<(String, f32)> = vec![(String::new(), 0.0)];
    for p in pieces {
        match p {
            Piece::Lit(t) => beam.iter_mut().for_each(|(s, _)| s.push_str(t)),
            Piece::Alt(alts) => {
                let mut next = Vec::with_capacity(beam.len() * alts.len());
                for (s, score) in &beam {
                    for (c, lp) in alts {
                        next.push((format!("{s}{c}"), score + lp));
                    }
                }
                next.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
                next.truncate(BEAM);
                beam = next;
            }
        }
    }
    let valid = |s: &str| {
        let left = s.matches('=').count();
        // At most one '=', with only `y` (or nothing) on its left.
        left <= 1 && (left == 0 || matches!(s.split('=').next(), Some("y") | Some(""))) && crate::expr::parse(rhs(s)).is_ok()
    };
    let raw_best = beam.first().map(|(s, _)| s.clone());
    beam.into_iter()
        .filter(|(s, _)| valid(s))
        .map(|(s, score)| {
            let total = score + notation_prior(&s);
            (s, total)
        })
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
        .map(|(s, _)| s)
        .or(raw_best)
        .map(|s| tidy(rhs(&s)))
}

/// Recognizes handwritten strokes (canvas coordinates, y down, in writing
/// order) as the right-hand side of `y = …`. `None` when there's nothing
/// to read.
pub fn recognize(strokes: &[Vec<Vec2>]) -> Option<String> {
    let strokes: Vec<Vec<Vec2>> = strokes.iter().filter(|s| !s.is_empty()).cloned().collect();
    if strokes.is_empty() {
        return None;
    }
    let model = Model::get();
    let syms = segment(&strokes, model);
    let mut pieces = Vec::new();
    layout(syms, &mut pieces);
    decode(&pieces).filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ink::stroke_text;

    /// Debug aid: `TAHTA_DEBUG_DIR=<board dir> TAHTA_DEBUG_RECT=x0,y0,x1,y1
    /// cargo test --release debug_board -- --ignored --nocapture` prints how
    /// the strokes inside the rectangle (canvas coordinates) were grouped,
    /// what each group looks like to the classifier, and the result.
    #[test]
    #[ignore]
    fn debug_board() {
        let dir = std::env::var("TAHTA_DEBUG_DIR").expect("TAHTA_DEBUG_DIR");
        let r: Vec<f32> = std::env::var("TAHTA_DEBUG_RECT").expect("TAHTA_DEBUG_RECT").split(',').map(|v| v.parse().unwrap()).collect();
        let board = crate::persist::load(std::path::Path::new(&dir)).unwrap().unwrap();
        let strokes: Vec<Vec<Vec2>> = board.pages[board.current_page]
            .strokes
            .iter()
            .filter(|s| {
                let c = s.points.iter().copied().sum::<Vec2>() / s.points.len().max(1) as f32;
                c.x >= r[0] && c.y >= r[1] && c.x <= r[2] && c.y <= r[3]
            })
            .map(|s| s.points.clone())
            .collect();
        println!("{} çizgi", strokes.len());
        let model = Model::get();
        for sym in segment(&strokes, model) {
            println!("sembol {:?} adaylar {:?}", (sym.rect.min, sym.rect.max), sym.cands);
        }
        // Each stroke alone, as the classifier sees it.
        for (k, st) in strokes.iter().enumerate() {
            let img = render(&[st.as_slice()]);
            println!("çizgi {k}: {:?}", model.top(&img, 3));
            for y in 0..SIZE {
                let row: String = (0..SIZE).map(|x| match img[y * SIZE + x] { v if v > 0.66 => '#', v if v > 0.2 => '+', _ => '.' }).collect();
                println!("  {row}");
            }
        }
        println!("sonuç: {:?}", recognize(&strokes));
    }

    #[test]
    fn rust_inference_matches_training() {
        let probe: &[u8] = include_bytes!("../assets/model/semboller.bin.probe");
        let n = u32::from_le_bytes(probe[..4].try_into().unwrap()) as usize;
        let imgs = &probe[4..4 + n * 784];
        let logits: Vec<f32> = probe[4 + n * 784..].chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect();
        let model = Model::get();
        let c = model.classes.len();
        for k in 0..n {
            let img: Vec<f32> = imgs[k * 784..(k + 1) * 784].iter().map(|&v| v as f32 / 255.0).collect();
            let got = model.logits(&img);
            for (g, e) in got.iter().zip(&logits[k * c..(k + 1) * c]) {
                assert!((g - e).abs() < 1e-3, "sample {k}: {g} vs {e}");
            }
        }
    }

    /// Strokes of `text` in tahta's own single-line stroke font, `h` tall,
    /// starting at `at` — a stand-in for handwriting in tests.
    fn ink(text: &str, at: Vec2, h: f32) -> Vec<Vec<Vec2>> {
        stroke_text(text, at, h).into_iter().map(|s| crate::ink::densify(&s, 2.0)).collect()
    }

    #[test]
    fn reads_a_parabola_with_an_exponent() {
        let mut strokes = ink("y", Vec2::new(0.0, 0.0), 60.0);
        // '=' isn't in the stroke font: two dashes.
        strokes.push(crate::ink::densify(&[Vec2::new(60.0, 25.0), Vec2::new(95.0, 25.0)], 2.0));
        strokes.push(crate::ink::densify(&[Vec2::new(60.0, 40.0), Vec2::new(95.0, 40.0)], 2.0));
        strokes.extend(ink("x", Vec2::new(120.0, 0.0), 60.0));
        strokes.extend(ink("2", Vec2::new(160.0, -15.0), 30.0)); // small, raised
        strokes.extend(ink("-", Vec2::new(200.0, 0.0), 60.0));
        strokes.extend(ink("3", Vec2::new(250.0, 0.0), 60.0));
        assert_eq!(recognize(&strokes).as_deref(), Some("x²−3"));
    }

    #[test]
    fn layout_rules() {
        let sym = |c: &str, x: f32, y: f32, w: f32, h: f32| Symbol {
            rect: Rect { min: Vec2::new(x, y), max: Vec2::new(x + w, y + h) },
            cands: vec![(c.to_string(), 0.0)],
        };
        let run = |syms: Vec<Symbol>| {
            let mut p = Vec::new();
            layout(syms, &mut p);
            decode(&p)
        };
        // 1 over x, as a fraction.
        assert_eq!(run(vec![sym("1", 10.0, 0.0, 10.0, 40.0), sym("−", 0.0, 50.0, 40.0, 3.0), sym("x", 8.0, 60.0, 25.0, 30.0)]).as_deref(), Some("1÷x"));
        // Root over x+1.
        assert_eq!(
            run(vec![sym("√", 0.0, 0.0, 120.0, 50.0), sym("x", 35.0, 15.0, 25.0, 30.0), sym("+", 65.0, 15.0, 20.0, 25.0), sym("1", 95.0, 5.0, 10.0, 40.0)]).as_deref(),
            Some("√(x+1)")
        );
        assert_eq!(run(vec![sym("√", 0.0, 0.0, 60.0, 50.0), sym("x", 30.0, 15.0, 25.0, 30.0), sym("+", 70.0, 15.0, 20.0, 25.0), sym("1", 100.0, 5.0, 10.0, 40.0)]).as_deref(), Some("√x+1"));
        // y = 1/x with '=' touching the bar's end: still a fraction of 1 and x.
        assert_eq!(
            run(vec![
                sym("y", -60.0, 40.0, 30.0, 50.0),
                sym("=", -15.0, 55.0, 40.0, 15.0),
                sym("1", 30.0, 0.0, 10.0, 40.0),
                sym("−", 10.0, 50.0, 60.0, 3.0),
                sym("x", 25.0, 60.0, 25.0, 30.0),
            ])
            .as_deref(),
            Some("1÷x")
        );
        // Context: "c0s" isn't a formula, "cos" is.
        let alts = |a: &[(&str, f32)]| Symbol { rect: Rect { min: Vec2::ZERO, max: Vec2::ONE }, cands: a.iter().map(|(c, p)| (c.to_string(), *p)).collect() };
        let mut p = Vec::new();
        for (i, s) in [alts(&[("c", -0.1)]), alts(&[("0", -0.2), ("o", -1.8)]), alts(&[("s", -0.6), ("5", -0.9)]), alts(&[("x", -0.1)])].into_iter().enumerate() {
            let mut s = s;
            s.rect = Rect { min: Vec2::new(i as f32 * 40.0, 0.0), max: Vec2::new(i as f32 * 40.0 + 30.0, 30.0) };
            p.push(s);
        }
        let mut pieces = Vec::new();
        layout(p, &mut pieces);
        assert_eq!(decode(&pieces).as_deref(), Some("cosx"));
    }
}
