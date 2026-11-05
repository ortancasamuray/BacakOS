//! Tiny math-expression parser/evaluator for the "Grafik" panel
//! (`graph.rs`): `y = f(x)` with up to three sliders `a`, `b`, `c`.
//!
//! Grammar (school notation, implicit multiplication allowed — `2x`,
//! `3sin(x)`, `a(x-1)^2`, `2π`):
//!
//! ```text
//! expr    := term (('+' | '-') term)*
//! term    := unary (('*' | '/') unary | <implicit> power)*
//! unary   := '-' unary | '+' unary | power
//! power   := primary ('^' unary)?          (right-associative: 2^3^2 = 2^9)
//! primary := number | x | a | b | c | π | e | func primary | '(' expr ')'
//! ```
//!
//! `-x^2` is `-(x^2)` and `2^-1` works, like a math teacher expects.
//! Functions take any primary as argument, so both `sin(x)` and `√x`
//! parse; the keypad always inserts `sin(` so most input is parenthesized.
//! A missing closing parenthesis at the very end is tolerated (`sin(x`)
//! since the formula is typed live and plotted on every key press.

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Func {
    Sin,
    Cos,
    Tan,
    Cot,
    Sqrt,
    Abs,
    /// Base-10 (Turkish curriculum: `log` = 10 tabanında).
    Log,
    Ln,
}

impl Func {
    fn apply(self, v: f64) -> f64 {
        match self {
            Func::Sin => v.sin(),
            Func::Cos => v.cos(),
            Func::Tan => v.tan(),
            Func::Cot => 1.0 / v.tan(),
            Func::Sqrt => v.sqrt(),
            Func::Abs => v.abs(),
            Func::Log => v.log10(),
            Func::Ln => v.ln(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    Num(f64),
    X,
    /// Slider parameter `a` (0), `b` (1) or `c` (2).
    Param(usize),
    Neg(Box<Expr>),
    Add(Box<Expr>, Box<Expr>),
    Sub(Box<Expr>, Box<Expr>),
    Mul(Box<Expr>, Box<Expr>),
    Div(Box<Expr>, Box<Expr>),
    Pow(Box<Expr>, Box<Expr>),
    Call(Func, Box<Expr>),
}

impl Expr {
    pub fn eval(&self, x: f64, params: &[f64; 3]) -> f64 {
        match self {
            Expr::Num(v) => *v,
            Expr::X => x,
            Expr::Param(i) => params[*i],
            Expr::Neg(e) => -e.eval(x, params),
            Expr::Add(l, r) => l.eval(x, params) + r.eval(x, params),
            Expr::Sub(l, r) => l.eval(x, params) - r.eval(x, params),
            Expr::Mul(l, r) => l.eval(x, params) * r.eval(x, params),
            Expr::Div(l, r) => l.eval(x, params) / r.eval(x, params),
            Expr::Pow(l, r) => pow(l.eval(x, params), r.eval(x, params)),
            Expr::Call(f, e) => f.apply(e.eval(x, params)),
        }
    }

    /// Which of `a`, `b`, `c` appear anywhere — the panel only shows
    /// sliders for parameters actually in use.
    pub fn params_used(&self, out: &mut [bool; 3]) {
        match self {
            Expr::Num(_) | Expr::X => {}
            Expr::Param(i) => out[*i] = true,
            Expr::Neg(e) | Expr::Call(_, e) => e.params_used(out),
            Expr::Add(l, r) | Expr::Sub(l, r) | Expr::Mul(l, r) | Expr::Div(l, r) | Expr::Pow(l, r) => {
                l.params_used(out);
                r.params_used(out);
            }
        }
    }
}

/// `f64::powf` returns NaN for a negative base with a non-integer exponent
/// — right for `(-8)^0.5`, but `x^(1/3)` should still draw the real cube
/// root on the negative side, as on a school graphing calculator. Odd
/// roots (`1/n`, n odd) of negatives are therefore taken as `-(|b|^e)`.
fn pow(base: f64, exp: f64) -> f64 {
    if base < 0.0 && exp.fract() != 0.0 {
        let inv = 1.0 / exp;
        if (inv - inv.round()).abs() < 1e-9 && (inv.round() as i64) % 2 != 0 {
            return -(-base).powf(exp);
        }
    }
    base.powf(exp)
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(f64),
    X,
    Param(usize),
    Pi,
    E,
    Func(Func),
    Plus,
    Minus,
    Star,
    Slash,
    Caret,
    LParen,
    RParen,
}

fn tokenize(s: &str) -> Result<Vec<Tok>, ()> {
    let chars: Vec<char> = s.chars().collect();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c.is_ascii_digit() || c == '.' || c == ',' {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.' || chars[i] == ',') {
                i += 1;
            }
            // Turkish decimal comma accepted too.
            let text: String = chars[start..i].iter().map(|&ch| if ch == ',' { '.' } else { ch }).collect();
            toks.push(Tok::Num(text.parse().map_err(|_| ())?));
            continue;
        }
        if c.is_alphabetic() {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_alphabetic() {
                i += 1;
            }
            if i == start {
                // A non-ASCII letter (π is handled below, ı/ş etc. aren't valid).
                if c == 'π' {
                    toks.push(Tok::Pi);
                    i += 1;
                    continue;
                }
                return Err(());
            }
            let word: String = chars[start..i].iter().collect::<String>().to_ascii_lowercase();
            lex_word(&word, &mut toks)?;
            continue;
        }
        let tok = match c {
            '+' => Tok::Plus,
            '-' | '−' => Tok::Minus,
            '*' | '·' | '×' => Tok::Star,
            '/' | '÷' => Tok::Slash,
            '^' => Tok::Caret,
            '(' | '[' => Tok::LParen,
            ')' | ']' => Tok::RParen,
            '√' => Tok::Func(Func::Sqrt),
            '²' => {
                toks.push(Tok::Caret);
                Tok::Num(2.0)
            }
            '³' => {
                toks.push(Tok::Caret);
                Tok::Num(3.0)
            }
            _ => return Err(()),
        };
        toks.push(tok);
        i += 1;
    }
    Ok(toks)
}

/// Splits a run of letters into function names and single-letter symbols,
/// longest match first — so `xsin` reads as `x·sin`, `ax` as `a·x`.
fn lex_word(word: &str, toks: &mut Vec<Tok>) -> Result<(), ()> {
    const FUNCS: &[(&str, Func)] = &[
        ("sqrt", Func::Sqrt),
        ("sin", Func::Sin),
        ("cos", Func::Cos),
        ("tan", Func::Tan),
        ("cot", Func::Cot),
        ("abs", Func::Abs),
        ("log", Func::Log),
        ("ln", Func::Ln),
        ("tg", Func::Tan),
    ];
    let mut rest = word;
    'outer: while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix("pi") {
            toks.push(Tok::Pi);
            rest = r;
            continue;
        }
        for &(name, f) in FUNCS {
            if let Some(r) = rest.strip_prefix(name) {
                toks.push(Tok::Func(f));
                rest = r;
                continue 'outer;
            }
        }
        let c = rest.as_bytes()[0];
        toks.push(match c {
            b'x' => Tok::X,
            b'a' => Tok::Param(0),
            b'b' => Tok::Param(1),
            b'c' => Tok::Param(2),
            b'e' => Tok::E,
            _ => return Err(()),
        });
        rest = &rest[1..];
    }
    Ok(())
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        self.pos += 1;
        t
    }

    fn expr(&mut self) -> Result<Expr, ()> {
        let mut lhs = self.term()?;
        loop {
            match self.peek() {
                Some(Tok::Plus) => {
                    self.pos += 1;
                    lhs = Expr::Add(Box::new(lhs), Box::new(self.term()?));
                }
                Some(Tok::Minus) => {
                    self.pos += 1;
                    lhs = Expr::Sub(Box::new(lhs), Box::new(self.term()?));
                }
                _ => return Ok(lhs),
            }
        }
    }

    fn term(&mut self) -> Result<Expr, ()> {
        let mut lhs = self.unary()?;
        loop {
            match self.peek() {
                Some(Tok::Star) => {
                    self.pos += 1;
                    lhs = Expr::Mul(Box::new(lhs), Box::new(self.unary()?));
                }
                Some(Tok::Slash) => {
                    self.pos += 1;
                    lhs = Expr::Div(Box::new(lhs), Box::new(self.unary()?));
                }
                Some(Tok::Num(_) | Tok::X | Tok::Param(_) | Tok::Pi | Tok::E | Tok::Func(_) | Tok::LParen) => {
                    lhs = Expr::Mul(Box::new(lhs), Box::new(self.power()?));
                }
                _ => return Ok(lhs),
            }
        }
    }

    fn unary(&mut self) -> Result<Expr, ()> {
        match self.peek() {
            Some(Tok::Minus) => {
                self.pos += 1;
                Ok(Expr::Neg(Box::new(self.unary()?)))
            }
            Some(Tok::Plus) => {
                self.pos += 1;
                self.unary()
            }
            _ => self.power(),
        }
    }

    fn power(&mut self) -> Result<Expr, ()> {
        let base = self.primary()?;
        if self.peek() == Some(&Tok::Caret) {
            self.pos += 1;
            return Ok(Expr::Pow(Box::new(base), Box::new(self.unary()?)));
        }
        Ok(base)
    }

    fn primary(&mut self) -> Result<Expr, ()> {
        match self.next().ok_or(())? {
            Tok::Num(v) => Ok(Expr::Num(v)),
            Tok::X => Ok(Expr::X),
            Tok::Param(i) => Ok(Expr::Param(i)),
            Tok::Pi => Ok(Expr::Num(std::f64::consts::PI)),
            Tok::E => Ok(Expr::Num(std::f64::consts::E)),
            Tok::Func(f) => {
                // `sin x^2` = sin(x^2), like most school calculators.
                let arg = self.power()?;
                Ok(Expr::Call(f, Box::new(arg)))
            }
            Tok::LParen => {
                let inner = self.expr()?;
                match self.next() {
                    Some(Tok::RParen) => Ok(inner),
                    // Unclosed at the very end — tolerated (live typing).
                    None => Ok(inner),
                    Some(_) => Err(()),
                }
            }
            _ => Err(()),
        }
    }
}

/// Parses the right-hand side of `y = …`. An optional leading `y=` is
/// accepted and stripped.
pub fn parse(src: &str) -> Result<Expr, ()> {
    let s = src.trim();
    let s = s.strip_prefix("y=").or_else(|| s.strip_prefix("y =")).unwrap_or(s);
    let toks = tokenize(s)?;
    if toks.is_empty() {
        return Err(());
    }
    let mut p = Parser { toks, pos: 0 };
    let e = p.expr()?;
    // Leftover tokens (e.g. a stray ')') — invalid. Running past the end
    // (`pos > len`) only happens via a tolerated missing ')'.
    if p.pos < p.toks.len() {
        return Err(());
    }
    Ok(e)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(s: &str, x: f64) -> f64 {
        parse(s).unwrap_or_else(|_| panic!("parse failed: {s}")).eval(x, &[2.0, 3.0, 5.0])
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn precedence_and_implicit_multiplication() {
        assert!(close(ev("x^2-3", 2.0), 1.0));
        assert!(close(ev("2x+1", 3.0), 7.0));
        assert!(close(ev("-x^2", 3.0), -9.0));
        assert!(close(ev("2^-1", 0.0), 0.5));
        assert!(close(ev("2^3^2", 0.0), 512.0));
        assert!(close(ev("a(x-1)^2", 3.0), 8.0));
        assert!(close(ev("6/2x", 3.0), 9.0));
        assert!(close(ev("2π", 0.0), std::f64::consts::TAU));
        assert!(close(ev("x²+1", 3.0), 10.0));
        assert!(close(ev("1,5x", 2.0), 3.0));
    }

    #[test]
    fn functions_and_params() {
        assert!(close(ev("a·sin(x)+b", std::f64::consts::FRAC_PI_2), 5.0));
        assert!(close(ev("asinx+b", std::f64::consts::FRAC_PI_2), 5.0));
        assert!(close(ev("√(x)", 9.0), 3.0));
        assert!(close(ev("√x+1", 9.0), 4.0));
        assert!(close(ev("abs(x-5)", 2.0), 3.0));
        assert!(close(ev("log(100)", 0.0), 2.0));
        assert!(close(ev("ln(e)", 0.0), 1.0));
        assert!(close(ev("y=x×2÷4", 2.0), 1.0));
        assert!(close(ev("x^(1/3)", -8.0), -2.0));
        assert!(ev("√x", -1.0).is_nan());
    }

    #[test]
    fn live_typing_and_errors() {
        assert!(close(ev("sin(x", 0.0), 0.0));
        assert!(close(ev("2(x+1", 1.0), 4.0));
        assert!(parse("").is_err());
        assert!(parse("x+").is_err());
        assert!(parse("x)").is_err());
        assert!(parse("q").is_err());
        assert!(parse("sin(").is_err());
        let mut used = [false; 3];
        parse("a·sin(x)+c").unwrap().params_used(&mut used);
        assert_eq!(used, [true, false, true]);
    }
}
