// SPDX-License-Identifier: GPL-3.0-or-later
//! End-to-end: drive the real libpam through a stack shaped exactly like
//! Turan's `/etc/pam.d/bacak-display-manager`, using `pam_start_confdir` so
//! no root and no system PAM config are needed. `pam_permit`/`pam_deny`
//! stand in for `pam_unix` (password accepted) and `pam_faillock`.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use bacakonay_core::store::{Enrollment, Store};
use bacakonay_core::{otp, OtpParams};
use zeroize::Zeroizing;

const PAM_SUCCESS: c_int = 0;
const PAM_AUTH_ERR: c_int = 7;
const PAM_PROMPT_ECHO_ON: c_int = 2;

#[repr(C)]
struct PamMessage {
    msg_style: c_int,
    msg: *const c_char,
}
#[repr(C)]
struct PamResponse {
    resp: *mut c_char,
    resp_retcode: c_int,
}
type ConvFn = extern "C" fn(c_int, *mut *const PamMessage, *mut *mut PamResponse, *mut c_void) -> c_int;
#[repr(C)]
struct PamConv {
    conv: ConvFn,
    appdata_ptr: *mut c_void,
}

#[link(name = "pam")]
extern "C" {
    fn pam_start_confdir(
        service: *const c_char,
        user: *const c_char,
        conv: *const PamConv,
        confdir: *const c_char,
        pamh: *mut *mut c_void,
    ) -> c_int;
    fn pam_authenticate(pamh: *mut c_void, flags: c_int) -> c_int;
    fn pam_end(pamh: *mut c_void, status: c_int) -> c_int;
}

/// What the fake greeter types, plus what it was asked.
struct Answers {
    code: String,
    prompts: Vec<String>,
}

extern "C" fn conv(n: c_int, msgs: *mut *const PamMessage, resp: *mut *mut PamResponse, data: *mut c_void) -> c_int {
    unsafe {
        let a = &mut *(data as *mut Answers);
        let out = libc::calloc(n as usize, std::mem::size_of::<PamResponse>()) as *mut PamResponse;
        for i in 0..n as usize {
            let m = &**msgs.add(i);
            let text = CStr::from_ptr(m.msg).to_string_lossy().into_owned();
            if m.msg_style == PAM_PROMPT_ECHO_ON {
                (*out.add(i)).resp = libc::strdup(CString::new(a.code.clone()).unwrap().as_ptr());
            }
            a.prompts.push(text);
        }
        *resp = out;
    }
    PAM_SUCCESS
}

fn module_path() -> PathBuf {
    // target/debug/deps/pam_stack-… → target/debug/libpam_bacakonay.so
    let exe = std::env::current_exe().unwrap();
    let so = exe.parent().unwrap().parent().unwrap().join("libpam_bacakonay.so");
    assert!(so.exists(), "{} yok — önce `cargo build -p pam-bacakonay`", so.display());
    so
}

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Fixture {
        let root = std::env::temp_dir().join(format!("bacakonay-pam-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("pam.d")).unwrap();
        Fixture { root }
    }
    fn store_dir(&self) -> PathBuf {
        self.root.join("store")
    }
    /// Write the Turan-shaped stack; `module` may point at a missing file.
    /// The bacakonay line is the one shipped in `pam/bacak-display-manager`:
    /// a missing module returns PAM_MODULE_UNKNOWN (not PAM_IGNORE, even with
    /// the `-` prefix), so `module_unknown=1` is what keeps login working on
    /// machines without the package.
    fn write_stack(&self, module: &Path, extra: &str) {
        let stack = format!(
            "auth  [success=1 default=bad]          pam_permit.so\n\
             auth  [default=die]                    pam_deny.so\n\
             -auth [success=1 ignore=1 module_unknown=1 default=bad] {} dizin={} {extra}\n\
             auth  [default=die]                    pam_deny.so\n\
             auth  required                         pam_permit.so\n",
            module.display(),
            self.store_dir().display()
        );
        std::fs::write(self.root.join("pam.d/bacak-test"), stack).unwrap();
    }
    fn auth(&self, user: &str, code: &str) -> (c_int, Vec<String>) {
        let mut answers = Answers { code: code.into(), prompts: Vec::new() };
        let c = PamConv { conv, appdata_ptr: &mut answers as *mut Answers as *mut c_void };
        let service = CString::new("bacak-test").unwrap();
        let user = CString::new(user).unwrap();
        let dir = CString::new(self.root.join("pam.d").to_str().unwrap()).unwrap();
        let mut h: *mut c_void = std::ptr::null_mut();
        let rc = unsafe { pam_start_confdir(service.as_ptr(), user.as_ptr(), &c, dir.as_ptr(), &mut h) };
        assert_eq!(rc, PAM_SUCCESS, "pam_start_confdir");
        let rc = unsafe { pam_authenticate(h, 0) };
        unsafe { pam_end(h, rc) };
        (rc, answers.prompts)
    }
    fn enroll(&self, user: &str) -> Zeroizing<Vec<u8>> {
        let secret = Zeroizing::new(otp::generate_secret(20).unwrap());
        let e = Enrollment { secret: secret.clone(), params: OtpParams::default(), last_step: None };
        Store::new(self.store_dir()).save(user, &e).unwrap();
        secret
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

#[test]
fn unenrolled_user_logs_in_without_a_prompt() {
    let f = Fixture::new("unenrolled");
    f.write_stack(&module_path(), "");
    let (rc, prompts) = f.auth("ayse", "");
    assert_eq!(rc, PAM_SUCCESS);
    assert!(prompts.is_empty(), "kayıtsız kullanıcıya soru sorulmamalı: {prompts:?}");
}

#[test]
fn missing_module_does_not_break_login() {
    let f = Fixture::new("missing");
    f.write_stack(Path::new("/nonexistent/pam_bacakonay.so"), "");
    assert_eq!(f.auth("ayse", "").0, PAM_SUCCESS);
}

#[test]
fn enrolled_user_needs_the_right_code_once() {
    let f = Fixture::new("enrolled");
    f.write_stack(&module_path(), "");
    let secret = f.enroll("ali");
    let code = otp::totp(&secret, &OtpParams::default(), now());

    let (rc, prompts) = f.auth("ali", &code);
    assert_eq!(rc, PAM_SUCCESS);
    assert_eq!(prompts, vec!["Bacak Onay kodu: ".to_string()]);

    // Same code again → replay, rejected (and pam_deny/"authfail" runs).
    let (rc, prompts) = f.auth("ali", &code);
    assert_eq!(rc, PAM_AUTH_ERR);
    assert!(prompts.iter().any(|p| p.contains("geçersiz")), "{prompts:?}");

    // Wrong / empty codes.
    let wrong = if code == "000000" { "111111" } else { "000000" };
    assert_eq!(f.auth("ali", wrong).0, PAM_AUTH_ERR);
    assert_eq!(f.auth("ali", "").0, PAM_AUTH_ERR);
}

#[test]
fn required_option_denies_unenrolled() {
    let f = Fixture::new("required");
    f.write_stack(&module_path(), "zorunlu");
    assert_eq!(f.auth("veli", "").0, PAM_AUTH_ERR);
}

#[test]
fn insecure_store_fails_closed() {
    let f = Fixture::new("insecure");
    f.write_stack(&module_path(), "");
    let secret = f.enroll("can");
    std::fs::set_permissions(f.store_dir().join("can"), std::fs::Permissions::from_mode(0o644)).unwrap();
    let code = otp::totp(&secret, &OtpParams::default(), now());
    assert_eq!(f.auth("can", &code).0, PAM_AUTH_ERR);
}

#[test]
fn custom_prompt_text() {
    let f = Fixture::new("prompt");
    f.write_stack(&module_path(), "istem=Doğrulama_kodu:");
    let secret = f.enroll("deniz");
    let code = otp::totp(&secret, &OtpParams::default(), now());
    let (rc, prompts) = f.auth("deniz", &code);
    assert_eq!(rc, PAM_SUCCESS);
    assert_eq!(prompts, vec!["Doğrulama kodu: ".to_string()]);
}
