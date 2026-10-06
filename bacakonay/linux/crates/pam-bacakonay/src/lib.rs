// SPDX-License-Identifier: GPL-3.0-or-later
//! `pam_bacakonay.so` — second factor for BacakOS logins.
//!
//! Stacked after `pam_unix` in `/etc/pam.d/bacak-display-manager` (Turan),
//! it asks for the 6/8-digit code shown in the Bacak Onay app and checks it
//! against the user's enrollment in `/var/lib/bacakonay`.
//!
//! Return codes:
//! * user **not enrolled** → `PAM_IGNORE` (2FA is opt-in per user), unless
//!   the `zorunlu` option is set → `PAM_AUTH_ERR`;
//! * right code → `PAM_SUCCESS` (and the step is consumed — no replay);
//! * wrong/empty code → `PAM_AUTH_ERR` (so `pam_faillock authfail` counts it);
//! * insecure or corrupt store → `PAM_AUTH_ERR` (fail closed) + syslog;
//! * internal panic → `PAM_SERVICE_ERR` (never unwinds into the daemon).
//!
//! Options (module arguments):
//! * `zorunlu` — enrolled-or-deny.
//! * `pencere=N` — accept ±N time steps of clock drift (default 1, max 10).
//! * `dizin=/yol` — store directory (default `/var/lib/bacakonay`; tests).
//! * `istem=Metin` — prompt text (`_` is shown as a space).

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::{SystemTime, UNIX_EPOCH};

use bacakonay_core::otp;
use bacakonay_core::store::{Store, StoreError, DEFAULT_DIR};
use zeroize::Zeroize;

pub const PAM_SUCCESS: c_int = 0;
pub const PAM_SERVICE_ERR: c_int = 3;
pub const PAM_AUTH_ERR: c_int = 7;
pub const PAM_AUTHINFO_UNAVAIL: c_int = 9;
pub const PAM_IGNORE: c_int = 25;
const PAM_PROMPT_ECHO_ON: c_int = 2;
const PAM_ERROR_MSG: c_int = 3;
const PAM_SILENT: c_int = 0x8000;

const LOG_ERR: c_int = 3;
const LOG_WARNING: c_int = 4;
const LOG_NOTICE: c_int = 5;
const LOG_INFO: c_int = 6;

const DEFAULT_PROMPT: &str = "Bacak Onay kodu: ";

#[repr(C)]
pub struct PamHandle {
    _private: [u8; 0],
}

#[link(name = "pam")]
extern "C" {
    fn pam_get_user(pamh: *mut PamHandle, user: *mut *const c_char, prompt: *const c_char) -> c_int;
    fn pam_prompt(
        pamh: *mut PamHandle,
        style: c_int,
        response: *mut *mut c_char,
        fmt: *const c_char,
        ...
    ) -> c_int;
    fn pam_syslog(pamh: *const PamHandle, priority: c_int, fmt: *const c_char, ...);
}

struct Options {
    required: bool,
    window: u64,
    dir: String,
    prompt: String,
}

impl Options {
    unsafe fn parse(argc: c_int, argv: *const *const c_char) -> Options {
        let mut o = Options {
            required: false,
            window: 1,
            dir: DEFAULT_DIR.to_string(),
            prompt: DEFAULT_PROMPT.to_string(),
        };
        if argv.is_null() {
            return o;
        }
        for i in 0..argc.max(0) as usize {
            let p = *argv.add(i);
            if p.is_null() {
                continue;
            }
            let arg = CStr::from_ptr(p).to_string_lossy();
            if arg == "zorunlu" {
                o.required = true;
            } else if let Some(v) = arg.strip_prefix("pencere=") {
                if let Ok(n) = v.parse::<u64>() {
                    o.window = n.min(10);
                }
            } else if let Some(v) = arg.strip_prefix("dizin=") {
                o.dir = v.to_string();
            } else if let Some(v) = arg.strip_prefix("istem=") {
                o.prompt = format!("{} ", v.replace('_', " ").trim_end());
            }
        }
        o
    }
}

fn log(pamh: *mut PamHandle, prio: c_int, msg: &str) {
    if let Ok(c) = CString::new(msg.replace('\0', "")) {
        unsafe { pam_syslog(pamh, prio, c"%s".as_ptr(), c.as_ptr()) };
    }
}

fn tell(pamh: *mut PamHandle, flags: c_int, msg: &str) {
    if flags & PAM_SILENT != 0 {
        return;
    }
    if let Ok(c) = CString::new(msg) {
        unsafe { pam_prompt(pamh, PAM_ERROR_MSG, std::ptr::null_mut(), c"%s".as_ptr(), c.as_ptr()) };
    }
}

/// Ask the conversation for the code; `None` if the front-end gave nothing.
fn ask_code(pamh: *mut PamHandle, prompt: &str) -> Option<String> {
    let c = CString::new(prompt).ok()?;
    let mut resp: *mut c_char = std::ptr::null_mut();
    let rc = unsafe { pam_prompt(pamh, PAM_PROMPT_ECHO_ON, &mut resp, c"%s".as_ptr(), c.as_ptr()) };
    if rc != PAM_SUCCESS || resp.is_null() {
        return None;
    }
    let out = unsafe { CStr::from_ptr(resp) }.to_string_lossy().into_owned();
    // Scrub libpam's copy before handing it back to the allocator.
    unsafe {
        let len = libc::strlen(resp);
        std::ptr::write_bytes(resp, 0, len);
        libc::free(resp as *mut c_void);
    }
    Some(out)
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

unsafe fn authenticate(pamh: *mut PamHandle, flags: c_int, argc: c_int, argv: *const *const c_char) -> c_int {
    let opts = Options::parse(argc, argv);

    let mut user_ptr: *const c_char = std::ptr::null();
    if pam_get_user(pamh, &mut user_ptr, std::ptr::null()) != PAM_SUCCESS || user_ptr.is_null() {
        return PAM_AUTHINFO_UNAVAIL;
    }
    let user = CStr::from_ptr(user_ptr).to_string_lossy().into_owned();
    let store = Store::new(&opts.dir);

    // Fast path: no enrollment → this module doesn't apply to the user.
    let enrolled = match store.load(&user) {
        Ok(Some(_)) => true,
        Ok(None) => false,
        Err(StoreError::InvalidUser) => false,
        Err(e) => {
            log(pamh, LOG_ERR, &format!("bacakonay: {user} kaydı okunamadı: {e}"));
            return PAM_AUTH_ERR;
        }
    };
    if !enrolled {
        if opts.required {
            log(pamh, LOG_NOTICE, &format!("bacakonay: {user} kayıtlı değil ve zorunlu; reddedildi"));
            tell(pamh, flags, "Bu hesap için Bacak Onay kaydı gerekli.");
            return PAM_AUTH_ERR;
        }
        return PAM_IGNORE;
    }

    let Some(mut code) = ask_code(pamh, &opts.prompt) else {
        return PAM_AUTH_ERR;
    };

    // Verify + consume under the per-user lock (no concurrent replay).
    let result = store.with_lock(&user, |s| {
        let Some(mut e) = s.load(&user)? else { return Ok(false) };
        match otp::verify_totp(&e.secret, &e.params, &code, now(), opts.window, e.last_step) {
            Some(step) => {
                e.last_step = Some(step);
                s.save(&user, &e)?;
                Ok(true)
            }
            None => Ok(false),
        }
    });
    code.zeroize();

    match result {
        Ok(true) => {
            log(pamh, LOG_INFO, &format!("bacakonay: {user} için kod doğrulandı"));
            PAM_SUCCESS
        }
        Ok(false) => {
            log(pamh, LOG_WARNING, &format!("bacakonay: {user} için geçersiz kod"));
            tell(pamh, flags, "Bacak Onay kodu geçersiz.");
            PAM_AUTH_ERR
        }
        Err(e) => {
            log(pamh, LOG_ERR, &format!("bacakonay: {user} kaydı güncellenemedi: {e}"));
            PAM_AUTH_ERR
        }
    }
}

/// # Safety
/// Called by libpam with a valid handle and `argc`/`argv` module arguments.
#[no_mangle]
pub unsafe extern "C" fn pam_sm_authenticate(
    pamh: *mut PamHandle,
    flags: c_int,
    argc: c_int,
    argv: *const *const c_char,
) -> c_int {
    catch_unwind(AssertUnwindSafe(|| authenticate(pamh, flags, argc, argv))).unwrap_or(PAM_SERVICE_ERR)
}

/// No credentials to establish; required so `auth` stacks can call setcred.
///
/// # Safety
/// Called by libpam.
#[no_mangle]
pub unsafe extern "C" fn pam_sm_setcred(
    _pamh: *mut PamHandle,
    _flags: c_int,
    _argc: c_int,
    _argv: *const *const c_char,
) -> c_int {
    PAM_SUCCESS
}
