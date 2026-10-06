// SPDX-License-Identifier: GPL-3.0-or-later
//! RFC 4226 (HOTP) and RFC 6238 (TOTP).

use hmac::{Hmac, KeyInit, Mac};
use subtle::ConstantTimeEq;

/// HMAC hash behind the OTP. SHA-1 is what every authenticator supports and
/// is still sound for HMAC; SHA-256/512 are offered for policies that ban it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Algorithm {
    #[default]
    Sha1,
    Sha256,
    Sha512,
}

impl Algorithm {
    /// Name as written in `otpauth://` URIs and the store file.
    pub fn as_str(self) -> &'static str {
        match self {
            Algorithm::Sha1 => "SHA1",
            Algorithm::Sha256 => "SHA256",
            Algorithm::Sha512 => "SHA512",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_uppercase().replace('-', "").as_str() {
            "SHA1" => Some(Algorithm::Sha1),
            "SHA256" => Some(Algorithm::Sha256),
            "SHA512" => Some(Algorithm::Sha512),
            _ => None,
        }
    }

    /// Secret length the CLI generates: the hash's output size, as RFC 4226
    /// §4 recommends (at least 128 bits; 160 for SHA-1).
    pub fn recommended_secret_len(self) -> usize {
        match self {
            Algorithm::Sha1 => 20,
            Algorithm::Sha256 => 32,
            Algorithm::Sha512 => 64,
        }
    }
}

/// The per-enrollment OTP parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OtpParams {
    pub algorithm: Algorithm,
    /// 6 or 8.
    pub digits: u32,
    /// TOTP period in seconds (30 by convention).
    pub period: u64,
}

impl Default for OtpParams {
    fn default() -> Self {
        OtpParams { algorithm: Algorithm::Sha1, digits: 6, period: 30 }
    }
}

impl OtpParams {
    pub fn is_valid(&self) -> bool {
        matches!(self.digits, 6 | 8) && (15..=300).contains(&self.period)
    }
}

fn hmac_digest(alg: Algorithm, key: &[u8], msg: &[u8]) -> Vec<u8> {
    // `new_from_slice` accepts any key length for HMAC; it cannot fail.
    match alg {
        Algorithm::Sha1 => {
            let mut m = <Hmac<sha1::Sha1> as KeyInit>::new_from_slice(key).expect("hmac key");
            m.update(msg);
            m.finalize().into_bytes().to_vec()
        }
        Algorithm::Sha256 => {
            let mut m = <Hmac<sha2::Sha256> as KeyInit>::new_from_slice(key).expect("hmac key");
            m.update(msg);
            m.finalize().into_bytes().to_vec()
        }
        Algorithm::Sha512 => {
            let mut m = <Hmac<sha2::Sha512> as KeyInit>::new_from_slice(key).expect("hmac key");
            m.update(msg);
            m.finalize().into_bytes().to_vec()
        }
    }
}

/// RFC 4226 HOTP value for `counter`, as a zero-padded decimal string.
pub fn hotp(secret: &[u8], counter: u64, alg: Algorithm, digits: u32) -> String {
    let mac = hmac_digest(alg, secret, &counter.to_be_bytes());
    // Dynamic truncation (RFC 4226 §5.3).
    let offset = (mac[mac.len() - 1] & 0x0f) as usize;
    let bin = ((mac[offset] as u32 & 0x7f) << 24)
        | ((mac[offset + 1] as u32) << 16)
        | ((mac[offset + 2] as u32) << 8)
        | (mac[offset + 3] as u32);
    let value = bin % 10u32.pow(digits);
    format!("{value:0width$}", width = digits as usize)
}

/// RFC 6238 time step for `unix_time`.
pub fn time_step(unix_time: u64, period: u64) -> u64 {
    unix_time / period
}

/// TOTP code at `unix_time`.
pub fn totp(secret: &[u8], params: &OtpParams, unix_time: u64) -> String {
    hotp(secret, time_step(unix_time, params.period), params.algorithm, params.digits)
}

/// Verify a user-typed TOTP `code`.
///
/// Accepts steps within `±window` of now (clock drift between phone and PC),
/// but only steps **strictly after** `last_step` — a code that was already
/// used to log in can't be replayed, even inside its 30 s window. Returns the
/// matched step so the caller can persist it as the new `last_step`.
pub fn verify_totp(
    secret: &[u8],
    params: &OtpParams,
    code: &str,
    unix_time: u64,
    window: u64,
    last_step: Option<u64>,
) -> Option<u64> {
    let code = code.trim();
    if code.len() != params.digits as usize || !code.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let now = time_step(unix_time, params.period);
    let mut matched = None;
    // Check every candidate (no early exit) so timing doesn't reveal which
    // step, if any, matched.
    for step in now.saturating_sub(window)..=now.saturating_add(window) {
        let candidate = hotp(secret, step, params.algorithm, params.digits);
        let eq: bool = candidate.as_bytes().ct_eq(code.as_bytes()).into();
        if eq && last_step.is_none_or(|l| step > l) && matched.is_none() {
            matched = Some(step);
        }
    }
    matched
}

/// Fresh random secret of `len` bytes from the OS CSPRNG.
pub fn generate_secret(len: usize) -> std::io::Result<Vec<u8>> {
    let mut buf = vec![0u8; len];
    getrandom::fill(&mut buf).map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEED20: &[u8] = b"12345678901234567890";
    const SEED32: &[u8] = b"12345678901234567890123456789012";
    const SEED64: &[u8] =
        b"1234567890123456789012345678901234567890123456789012345678901234";

    #[test]
    fn rfc4226_appendix_d() {
        let expected = [
            "755224", "287082", "359152", "969429", "338314",
            "254676", "287922", "162583", "399871", "520489",
        ];
        for (i, want) in expected.iter().enumerate() {
            assert_eq!(hotp(SEED20, i as u64, Algorithm::Sha1, 6), *want);
        }
    }

    #[test]
    fn rfc6238_appendix_b() {
        let cases: &[(u64, &str, &str, &str)] = &[
            (59, "94287082", "46119246", "90693936"),
            (1111111109, "07081804", "68084774", "25091201"),
            (1111111111, "14050471", "67062674", "99943326"),
            (1234567890, "89005924", "91819424", "93441116"),
            (2000000000, "69279037", "90698825", "38618901"),
            (20000000000, "65353130", "77737706", "47863826"),
        ];
        for &(t, s1, s256, s512) in cases {
            let p = |algorithm| OtpParams { algorithm, digits: 8, period: 30 };
            assert_eq!(totp(SEED20, &p(Algorithm::Sha1), t), s1, "sha1 t={t}");
            assert_eq!(totp(SEED32, &p(Algorithm::Sha256), t), s256, "sha256 t={t}");
            assert_eq!(totp(SEED64, &p(Algorithm::Sha512), t), s512, "sha512 t={t}");
        }
    }

    #[test]
    fn verify_accepts_drift_and_blocks_replay() {
        let p = OtpParams::default();
        let t = 1_700_000_000;
        let prev = totp(SEED20, &p, t - 30);
        let step = verify_totp(SEED20, &p, &prev, t, 1, None).expect("1 step drift ok");
        assert_eq!(step, time_step(t, 30) - 1);
        // Same code again → replay, rejected.
        assert!(verify_totp(SEED20, &p, &prev, t, 1, Some(step)).is_none());
        // Two steps old → outside the window.
        let old = totp(SEED20, &p, t - 60);
        assert!(verify_totp(SEED20, &p, &old, t, 1, None).is_none());
        // Malformed input.
        assert!(verify_totp(SEED20, &p, "12a456", t, 1, None).is_none());
        assert!(verify_totp(SEED20, &p, "1234567", t, 1, None).is_none());
    }

    #[test]
    fn algorithm_names_roundtrip() {
        for a in [Algorithm::Sha1, Algorithm::Sha256, Algorithm::Sha512] {
            assert_eq!(Algorithm::parse(a.as_str()), Some(a));
        }
        assert_eq!(Algorithm::parse("sha-256"), Some(Algorithm::Sha256));
        assert_eq!(Algorithm::parse("md5"), None);
    }
}
