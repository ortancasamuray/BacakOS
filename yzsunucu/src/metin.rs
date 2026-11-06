//! Turns the model's answer into plain text tahta's card font can draw:
//! models slip into Markdown and LaTeX even when told not to.

pub fn duzelt(girdi: &str) -> String {
    let mut s = girdi.to_string();
    for sinir in ["\\(", "\\)", "\\[", "\\]"] {
        s = s.replace(sinir, "");
    }
    let mut s = latex(&s);
    for isaret in ["**", "__", "`", "$"] {
        s = s.replace(isaret, "");
    }
    let mut satirlar: Vec<String> = Vec::new();
    for satir in s.lines() {
        let mut t = satir.trim_end().to_string();
        let bas = t.trim_start();
        if let Some(r) = bas.strip_prefix('#') {
            t = r.trim_start_matches('#').trim().to_string();
        } else if let Some(r) = bas.strip_prefix("- ").or_else(|| bas.strip_prefix("* ")) {
            t = format!("• {r}");
        }
        if t.trim() == "---" {
            continue;
        }
        // The card is small: blank lines only waste it.
        if !t.trim().is_empty() {
            satirlar.push(t);
        }
    }
    satirlar.join("\n")
}

fn latex(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < c.len() {
        match c[i] {
            '\\' => {
                let mut j = i + 1;
                while j < c.len() && c[j].is_ascii_alphabetic() {
                    j += 1;
                }
                let ad: String = c[i + 1..j].iter().collect();
                i = j;
                match ad.as_str() {
                    "frac" | "dfrac" | "tfrac" => {
                        let (pay, k) = grup(&c, i);
                        let (payda, k) = grup(&c, k);
                        i = k;
                        out += &format!("{}/{}", sar(&latex(&pay)), sar(&latex(&payda)));
                    }
                    "sqrt" => {
                        let (ic, k) = grup(&c, i);
                        i = k;
                        out.push('√');
                        out += &sar(&latex(&ic));
                    }
                    "text" | "mathrm" | "mathbf" | "operatorname" => {
                        let (ic, k) = grup(&c, i);
                        i = k;
                        out += &latex(&ic);
                    }
                    "left" | "right" | "displaystyle" | "quad" | "," | "" => {
                        if ad == "quad" {
                            out.push(' ');
                        }
                    }
                    _ => out += sembol(&ad).unwrap_or(&ad),
                }
            }
            '^' => {
                let (us, k) = grup(&c, i + 1);
                i = k;
                let us = latex(&us);
                match us.as_str() {
                    "2" => out.push('²'),
                    "3" => out.push('³'),
                    _ => out += &format!("^{}", sar(&us)),
                }
            }
            '{' | '}' => i += 1,
            ch => {
                out.push(ch);
                i += 1;
            }
        }
    }
    out
}

/// A `{…}` group (or the single next char) starting at `i`; returns the
/// content and the index after it.
fn grup(c: &[char], mut i: usize) -> (String, usize) {
    while i < c.len() && c[i] == ' ' {
        i += 1;
    }
    if i >= c.len() {
        return (String::new(), i);
    }
    if c[i] != '{' {
        return (c[i].to_string(), i + 1);
    }
    let mut derinlik = 0;
    for j in i..c.len() {
        match c[j] {
            '{' => derinlik += 1,
            '}' => {
                derinlik -= 1;
                if derinlik == 0 {
                    return (c[i + 1..j].iter().collect(), j + 1);
                }
            }
            _ => {}
        }
    }
    (c[i + 1..].iter().collect(), c.len())
}

/// Parenthesizes anything longer than a single number or letter.
fn sar(s: &str) -> String {
    let t = s.trim();
    if t.chars().all(|ch| ch.is_alphanumeric() || ch == ',' || ch == '.') || (t.starts_with('(') && t.ends_with(')')) {
        t.to_string()
    } else {
        format!("({t})")
    }
}

fn sembol(ad: &str) -> Option<&'static str> {
    Some(match ad {
        "cdot" => "·",
        "times" => "×",
        "div" => "÷",
        "pi" => "π",
        "le" | "leq" => "≤",
        "ge" | "geq" => "≥",
        "ne" | "neq" => "≠",
        "pm" => "±",
        "approx" => "≈",
        "infty" => "∞",
        "to" | "rightarrow" | "Rightarrow" | "implies" => "→",
        "alpha" => "α",
        "beta" => "β",
        "theta" => "θ",
        "angle" => "∠",
        "triangle" => "△",
        "degree" | "circ" => "°",
        "sin" | "cos" | "tan" | "cot" | "log" | "ln" => return None,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::duzelt;

    #[test]
    fn latex_ve_markdown_temizlenir() {
        assert_eq!(duzelt("**Adım 1:** $x^2 - 4 = 0$"), "Adım 1: x² - 4 = 0");
        assert_eq!(duzelt("\\(\\frac{x+1}{2}\\)"), "(x+1)/2");
        assert_eq!(duzelt("$\\sqrt{x+1} \\cdot 3$"), "√(x+1) · 3");
        assert_eq!(duzelt("$\\sqrt{9} = 3$, $a^{10}$"), "√9 = 3, a^10");
        assert_eq!(duzelt("### Çözüm\n\n\n- bir\n* iki\n\n"), "Çözüm\n• bir\n• iki");
        assert_eq!(duzelt("\\frac{1}{x} \\geq 0, x \\neq 0"), "1/x ≥ 0, x ≠ 0");
        assert_eq!(duzelt("$\\sin(x)$"), "sin(x)");
    }
}
