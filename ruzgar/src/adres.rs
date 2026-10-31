//! What the address bar turns typed text into.

/// Turkey's national education platform — a classroom browser's home.
pub const ANASAYFA: &str = "https://www.eba.gov.tr";

/// A URL to open for what was typed: anything with a scheme as is, a single
/// dotted word as a site, everything else as a search. Case is kept — paths
/// and queries are case-sensitive.
pub fn cozumle(yazi: &str) -> String {
    let t = yazi.trim();
    if t.is_empty() {
        return ANASAYFA.to_string();
    }
    if t.contains("://") || t.starts_with("about:") {
        return t.to_string();
    }
    if !t.contains(' ') && t.contains('.') {
        return format!("https://{t}");
    }
    let mut q = String::new();
    for b in t.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => q.push(b as char),
            b' ' => q.push('+'),
            _ => q.push_str(&format!("%{b:02X}")),
        }
    }
    format!("https://www.google.com/search?q={q}")
}

/// The page an address names, without the `#fragment` — what a page's
/// drawings are kept under (jumping to an anchor is still the same page).
pub fn sayfa_anahtari(uri: &str) -> &str {
    uri.split('#').next().unwrap_or(uri)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_text_becomes_a_url() {
        assert_eq!(cozumle("https://example.com/Ders?id=AbC"), "https://example.com/Ders?id=AbC");
        assert_eq!(cozumle("eba.gov.tr/Video"), "https://eba.gov.tr/Video");
        assert_eq!(cozumle("  "), ANASAYFA);
        assert_eq!(cozumle("Fotosentez nedir"), "https://www.google.com/search?q=Fotosentez+nedir");
        assert_eq!(cozumle("çiçek"), "https://www.google.com/search?q=%C3%A7i%C3%A7ek");
    }

    #[test]
    fn fragment_is_the_same_page() {
        assert_eq!(sayfa_anahtari("https://a.tr/ders#bolum2"), "https://a.tr/ders");
        assert_eq!(sayfa_anahtari("https://a.tr/ders"), "https://a.tr/ders");
    }
}
