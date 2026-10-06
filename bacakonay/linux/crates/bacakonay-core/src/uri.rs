// SPDX-License-Identifier: GPL-3.0-or-later
//! `otpauth://` URI builder (Key URI Format, as used by Google Authenticator
//! and every compatible app). Bacak Onay reads the same format, so an
//! enrollment QR works with any standard authenticator as well.

use crate::{base32, OtpParams};

/// Issuer shown in the phone app; Bacak Onay badges these entries as
/// BacakOS logins.
pub const ISSUER: &str = "BacakOS";

/// `otpauth://totp/BacakOS:<user>@<host>?secret=…&issuer=BacakOS&…`
pub fn totp_uri(user: &str, host: &str, secret: &[u8], params: &OtpParams) -> String {
    let label = format!("{ISSUER}:{user}@{host}");
    format!(
        "otpauth://totp/{}?secret={}&issuer={}&algorithm={}&digits={}&period={}",
        encode_component(&label),
        base32::encode(secret),
        ISSUER,
        params.algorithm.as_str(),
        params.digits,
        params.period,
    )
}

/// Percent-encode everything outside RFC 3986 "unreserved" (and keep the
/// label's `:` separator readable, which the Key URI Format allows).
fn encode_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b':' | b'@' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_standard_key_uri() {
        let uri = totp_uri("ayşe", "bacak pc", b"12345678901234567890", &OtpParams::default());
        assert_eq!(
            uri,
            "otpauth://totp/BacakOS:ay%C5%9Fe@bacak%20pc?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ\
             &issuer=BacakOS&algorithm=SHA1&digits=6&period=30"
        );
    }
}
