//! EPUB chapter (X)HTML → readable plain text.
//!
//! Not an HTML engine: it keeps what a reader needs — paragraphs, headings,
//! list items, line breaks and every character (named and numeric entities
//! decoded) — and drops what must never show up as text: `<head>` (with its
//! `<title>`), `<style>`, `<script>`, comments and processing instructions.
//! Whitespace is collapsed like a browser does, except inside `<pre>`.

/// Elements whose whole content is skipped.
const SKIP: &[&str] = &["head", "style", "script", "title", "noscript", "template"];

/// Elements that start and end on their own line.
const BLOCK: &[&str] = &[
    "p", "div", "section", "article", "aside", "header", "footer", "nav", "main",
    "blockquote", "figure", "figcaption", "ul", "ol", "dl", "dt", "dd", "table", "tr",
    "pre", "address", "body",
];

pub fn html_to_text(html: &str) -> String {
    let mut out = Out::default();
    let mut skip_depth = 0usize;
    let mut pre_depth = 0usize;
    let mut rest = html;

    while let Some(c) = rest.chars().next() {
        if c == '<' {
            // Comments, CDATA, <!DOCTYPE>, <?xml?>.
            if let Some(r) = rest.strip_prefix("<!--") {
                rest = r.find("-->").map_or("", |i| &r[i + 3..]);
                continue;
            }
            if let Some(r) = rest.strip_prefix("<![CDATA[") {
                let end = r.find("]]>").unwrap_or(r.len());
                if skip_depth == 0 {
                    out.text(&r[..end], pre_depth > 0);
                }
                rest = r.get(end + 3..).unwrap_or("");
                continue;
            }
            let Some(end) = rest.find('>') else {
                // A lone '<' with no tag after it is just text.
                if skip_depth == 0 {
                    out.text("<", pre_depth > 0);
                }
                rest = &rest[1..];
                continue;
            };
            let inner = &rest[1..end];
            rest = &rest[end + 1..];
            if inner.starts_with('!') || inner.starts_with('?') {
                continue;
            }
            let closing = inner.starts_with('/');
            let self_closing = inner.ends_with('/');
            let name: String = inner
                .trim_start_matches('/')
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == ':' || *c == '-')
                .collect::<String>()
                .to_ascii_lowercase();
            // `epub:switch` & co. keep their local name.
            let name = name.rsplit(':').next().unwrap_or("").to_string();

            if SKIP.contains(&name.as_str()) {
                if closing {
                    skip_depth = skip_depth.saturating_sub(1);
                } else if !self_closing {
                    skip_depth += 1;
                }
                continue;
            }
            if skip_depth > 0 {
                continue;
            }
            match name.as_str() {
                "br" => out.line_break(),
                "hr" => {
                    out.paragraph();
                    out.raw("⁂");
                    out.paragraph();
                }
                "li" if !closing => {
                    out.block();
                    out.raw("• ");
                }
                "li" => out.block(),
                "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => out.paragraph(),
                "pre" => {
                    out.block();
                    if closing {
                        pre_depth = pre_depth.saturating_sub(1);
                    } else if !self_closing {
                        pre_depth += 1;
                    }
                }
                "p" => out.paragraph(),
                "td" | "th" if closing => out.raw("  "),
                n if BLOCK.contains(&n) => out.block(),
                _ => {}
            }
            continue;
        }

        // Text up to the next tag.
        let end = rest.find('<').unwrap_or(rest.len());
        if skip_depth == 0 {
            out.text(&decode_entities(&rest[..end]), pre_depth > 0);
        }
        rest = &rest[end..];
    }
    out.finish()
}

/// Builds the text with collapsed whitespace and at most one blank line
/// between paragraphs.
#[derive(Default)]
struct Out {
    s: String,
    /// Pending whitespace between two words.
    space: bool,
    /// Newlines owed before the next text: 1 = new line, 2 = new paragraph.
    breaks: u8,
}

impl Out {
    fn flush_breaks(&mut self) {
        if self.s.is_empty() {
            self.breaks = 0;
            self.space = false;
            return;
        }
        if self.breaks > 0 {
            let trailing = self.s.len() - self.s.trim_end_matches('\n').len();
            for _ in trailing..self.breaks as usize {
                self.s.push('\n');
            }
            self.breaks = 0;
            self.space = false;
        } else if self.space {
            self.s.push(' ');
            self.space = false;
        }
    }

    fn text(&mut self, t: &str, pre: bool) {
        if pre {
            self.flush_breaks();
            self.s.push_str(t);
            return;
        }
        for (i, word) in t.split(|c: char| c.is_whitespace() && c != '\u{a0}').enumerate() {
            if i > 0 {
                self.space = true;
            }
            if word.is_empty() {
                continue;
            }
            self.flush_breaks();
            self.s.push_str(word);
        }
    }

    fn raw(&mut self, t: &str) {
        self.flush_breaks();
        self.s.push_str(t);
    }

    fn line_break(&mut self) {
        // A <br> always breaks, even right after another one.
        self.flush_breaks();
        if !self.s.is_empty() {
            self.s.push('\n');
        }
        self.space = false;
    }

    fn block(&mut self) {
        self.breaks = self.breaks.max(1);
    }

    fn paragraph(&mut self) {
        self.breaks = 2;
    }

    fn finish(mut self) -> String {
        let t = self.s.trim_end().len();
        self.s.truncate(t);
        self.s.push('\n');
        self.s
    }
}

/// `&amp;`, `&#8217;`, `&#x201C;` … → characters. Anything that isn't a
/// complete, known entity is left exactly as written (a bare `&` stays).
pub fn decode_entities(s: &str) -> std::borrow::Cow<'_, str> {
    if !s.contains('&') {
        return s.into();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let decoded = rest[1..]
            .find(';')
            .filter(|&n| n > 0 && n <= 10)
            .and_then(|n| entity(&rest[1..1 + n]).map(|c| (c, n + 2)));
        match decoded {
            Some((c, len)) => {
                if let Some(c) = c {
                    out.push(c);
                }
                rest = &rest[len..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out.into()
}

/// `Some(None)`: a known entity that renders as nothing (soft hyphen).
fn entity(name: &str) -> Option<Option<char>> {
    if let Some(num) = name.strip_prefix('#') {
        let code = match num.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => num.parse::<u32>().ok()?,
        };
        return match code {
            0xAD => Some(None),
            _ => Some(Some(char::from_u32(code).filter(|c| *c != '\0').unwrap_or('\u{FFFD}'))),
        };
    }
    let c = match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{a0}',
        "shy" => return Some(None),
        "hellip" => '…',
        "mdash" => '—',
        "ndash" => '–',
        "lsquo" => '‘',
        "rsquo" => '’',
        "sbquo" => '‚',
        "ldquo" => '“',
        "rdquo" => '”',
        "bdquo" => '„',
        "laquo" => '«',
        "raquo" => '»',
        "bull" => '•',
        "middot" => '·',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "deg" => '°',
        "times" => '×',
        "divide" => '÷',
        "euro" => '€',
        "para" => '¶',
        "sect" => '§',
        "Ccedil" => 'Ç',
        "ccedil" => 'ç',
        "Ouml" => 'Ö',
        "ouml" => 'ö',
        "Uuml" => 'Ü',
        "uuml" => 'ü',
        "Acirc" => 'Â',
        "acirc" => 'â',
        "Icirc" => 'Î',
        "icirc" => 'î',
        "Ucirc" => 'Û',
        "ucirc" => 'û',
        "eacute" => 'é',
        "Eacute" => 'É',
        "egrave" => 'è',
        "agrave" => 'à',
        "auml" => 'ä',
        "Auml" => 'Ä',
        "szlig" => 'ß',
        _ => return None,
    };
    Some(Some(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_style_and_script_never_become_text() {
        let html = r#"<?xml version="1.0"?><!DOCTYPE html><html><head><title>Kitap</title>
            <style>p { color: red }</style></head><body><script>var a=1;</script>
            <!-- yorum --><p>Merhaba</p></body></html>"#;
        assert_eq!(html_to_text(html), "Merhaba\n");
    }

    #[test]
    fn paragraphs_lists_and_breaks() {
        let html = "<h1>Başlık</h1><p>Bir\n   iki</p><div>Üç</div><ul><li>a</li><li>b</li></ul>x<br/>y";
        assert_eq!(html_to_text(html), "Başlık\n\nBir iki\n\nÜç\n• a\n• b\nx\ny\n");
    }

    #[test]
    fn entities_are_decoded_and_bare_ampersands_kept() {
        assert_eq!(decode_entities("Ali &amp; Veli"), "Ali & Veli");
        assert_eq!(decode_entities("Ali & Veli eve gitti."), "Ali & Veli eve gitti.");
        assert_eq!(decode_entities("&#8217;&#x201C;&#X201D;"), "’“”");
        assert_eq!(decode_entities("&ccedil;i&ccedil;ek &#287;"), "çiçek ğ");
        assert_eq!(decode_entities("bilin&shy;mez"), "bilinmez");
        assert_eq!(decode_entities("&bilinmeyen; &;"), "&bilinmeyen; &;");
        // Text after a bare '&' in a paragraph is not lost.
        assert_eq!(html_to_text("<p>Tom & Jerry</p><p>son</p>"), "Tom & Jerry\n\nson\n");
    }

    #[test]
    fn pre_keeps_its_layout() {
        assert_eq!(html_to_text("<p>a</p><pre>x  y\n  z</pre><p>b</p>"), "a\n\nx  y\n  z\n\nb\n");
    }

    #[test]
    fn namespaced_and_malformed_markup() {
        assert_eq!(html_to_text("<epub:switch><p>a</p></epub:switch> 1 < 2"), "a\n\n1 < 2\n");
        assert_eq!(html_to_text(""), "\n");
    }
}
