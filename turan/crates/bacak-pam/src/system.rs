//! Real libpam backend, compiled only with `--features system-pam`.
//!
//! This is a **direct, hand-written FFI** to `libpam` — no `bindgen`, no C
//! headers, no `libclang`. The handful of `extern "C"` declarations below are
//! the stable Linux-PAM ABI (`pam_appl.h`), so the only build requirement is
//! `libpam.so.0` at link time (provided by `libpam` itself, not the `-dev`
//! package). The consuming binary's `build.rs` emits the link directive.
//!
//! The flow maps 1:1 onto libpam:
//!
//! | BDM call         | libpam                                              |
//! |------------------|-----------------------------------------------------|
//! | `authenticate`   | `pam_start` → `pam_authenticate` → `pam_acct_mgmt`  |
//! | `open_session`   | `pam_setcred(ESTABLISH)` → `pam_open_session` → `pam_getenvlist` |
//! | `close_session`  | `pam_close_session` → `pam_setcred(DELETE)` → `pam_end` |
//!
//! Because the PAM conversation callback is invoked synchronously from inside
//! `pam_authenticate` on the calling thread, the backend takes the front-end as
//! a plain `&mut dyn Conversation` — the exact same shape as the mock backend.
//! No worker thread, no `Arc<Mutex<…>>`: the daemon's `IpcConversation` is used
//! directly.

use crate::{AuthError, AuthResult, AuthedUser, Authenticator, Conversation, Prompt};
use std::ffi::{CStr, CString};
use std::os::raw::{c_int, c_void};
use std::ptr;

mod ffi {
    use std::os::raw::{c_char, c_int, c_void};

    // Opaque PAM handle.
    #[repr(C)]
    pub struct PamHandle {
        _private: [u8; 0],
    }

    #[repr(C)]
    pub struct PamMessage {
        pub msg_style: c_int,
        pub msg: *const c_char,
    }

    #[repr(C)]
    pub struct PamResponse {
        pub resp: *mut c_char,
        pub resp_retcode: c_int,
    }

    pub type ConvFn = extern "C" fn(
        num_msg: c_int,
        msg: *mut *const PamMessage,
        resp: *mut *mut PamResponse,
        appdata_ptr: *mut c_void,
    ) -> c_int;

    #[repr(C)]
    pub struct PamConv {
        pub conv: ConvFn,
        pub appdata_ptr: *mut c_void,
    }

    extern "C" {
        pub fn pam_start(
            service_name: *const c_char,
            user: *const c_char,
            pam_conversation: *const PamConv,
            pamh: *mut *mut PamHandle,
        ) -> c_int;
        pub fn pam_end(pamh: *mut PamHandle, pam_status: c_int) -> c_int;
        pub fn pam_authenticate(pamh: *mut PamHandle, flags: c_int) -> c_int;
        pub fn pam_acct_mgmt(pamh: *mut PamHandle, flags: c_int) -> c_int;
        pub fn pam_setcred(pamh: *mut PamHandle, flags: c_int) -> c_int;
        pub fn pam_open_session(pamh: *mut PamHandle, flags: c_int) -> c_int;
        pub fn pam_close_session(pamh: *mut PamHandle, flags: c_int) -> c_int;
        pub fn pam_set_item(pamh: *mut PamHandle, item_type: c_int, item: *const c_void) -> c_int;
        pub fn pam_getenvlist(pamh: *mut PamHandle) -> *mut *mut c_char;
        pub fn pam_putenv(pamh: *mut PamHandle, name_value: *const c_char) -> c_int;
        pub fn pam_strerror(pamh: *mut PamHandle, errnum: c_int) -> *const c_char;
    }

    // libc allocator entry points used to build the response array PAM frees.
    extern "C" {
        pub fn calloc(nmemb: usize, size: usize) -> *mut c_void;
        pub fn strdup(s: *const c_char) -> *mut c_char;
        pub fn free(p: *mut c_void);
    }
}

// --- Linux-PAM constants (stable ABI) ---------------------------------------
const PAM_SUCCESS: c_int = 0;
const PAM_PERM_DENIED: c_int = 6;
const PAM_AUTH_ERR: c_int = 7;
const PAM_CRED_INSUFFICIENT: c_int = 8;
const PAM_AUTHINFO_UNAVAIL: c_int = 9;
const PAM_USER_UNKNOWN: c_int = 10;
const PAM_MAXTRIES: c_int = 11;
const PAM_NEW_AUTHTOK_REQD: c_int = 12;
const PAM_ACCT_EXPIRED: c_int = 13;
const PAM_CRED_EXPIRED: c_int = 16;
const PAM_ABORT: c_int = 26;

const PAM_CONV_ERR: c_int = 19;

// Message styles.
const PAM_PROMPT_ECHO_OFF: c_int = 1;
const PAM_PROMPT_ECHO_ON: c_int = 2;
const PAM_ERROR_MSG: c_int = 3;
const PAM_TEXT_INFO: c_int = 4;

// Item types.
const PAM_TTY: c_int = 3;

// setcred flags.
const PAM_ESTABLISH_CRED: c_int = 0x0002;
const PAM_DELETE_CRED: c_int = 0x0004;

/// Carries the front-end conversation across the C boundary. Lives on the
/// `authenticate` stack frame; its pointer is handed to PAM as `appdata_ptr`
/// and only dereferenced synchronously during `pam_authenticate`/`pam_acct_mgmt`.
struct ConvState<'a> {
    conv: &'a mut dyn Conversation,
}

/// The C conversation callback. Translates each PAM message to a [`Prompt`],
/// asks the front-end, and returns answers in a PAM-owned (libc-allocated)
/// response array.
extern "C" fn conversation_trampoline(
    num_msg: c_int,
    msg: *mut *const ffi::PamMessage,
    resp: *mut *mut ffi::PamResponse,
    appdata_ptr: *mut c_void,
) -> c_int {
    if num_msg <= 0 || msg.is_null() || resp.is_null() || appdata_ptr.is_null() {
        return PAM_CONV_ERR;
    }
    let n = num_msg as usize;
    // SAFETY: appdata_ptr is the &mut ConvState we passed to pam_start, valid
    // for the duration of the PAM call that invoked this trampoline.
    let state = unsafe { &mut *(appdata_ptr as *mut ConvState) };

    // PAM frees this array (and each resp string) with free(), so it must come
    // from the libc allocator.
    let responses =
        unsafe { ffi::calloc(n, std::mem::size_of::<ffi::PamResponse>()) } as *mut ffi::PamResponse;
    if responses.is_null() {
        return PAM_CONV_ERR;
    }

    for i in 0..n {
        // On Linux-PAM `msg` is an array of pointers: msg[i] is `*const PamMessage`.
        let m = unsafe { *msg.add(i) };
        if m.is_null() {
            continue;
        }
        let style = unsafe { (*m).msg_style };
        let text = unsafe {
            if (*m).msg.is_null() {
                String::new()
            } else {
                CStr::from_ptr((*m).msg).to_string_lossy().into_owned()
            }
        };

        let prompt = match style {
            PAM_PROMPT_ECHO_OFF => Prompt::SecretInput(text),
            PAM_PROMPT_ECHO_ON => Prompt::VisibleInput(text),
            PAM_ERROR_MSG => Prompt::Error(text),
            PAM_TEXT_INFO => Prompt::Info(text),
            _ => Prompt::Info(text),
        };

        match state.conv.handle(&prompt) {
            Ok(Some(answer)) => {
                // PAM wants a libc-allocated, NUL-terminated string it can free().
                // Build a transient CString from the secret, strdup it for PAM,
                // then scrub our copy. PAM owns (and later frees) `dup`; both our
                // CString and `answer` (a Secret) are zeroized before this arm ends,
                // so no extra plaintext lingers on the heap.
                if let Ok(c) = CString::new(answer.expose()) {
                    let dup = unsafe { ffi::strdup(c.as_ptr()) };
                    unsafe { (*responses.add(i)).resp = dup };

                    // Volatile zero the transient buffer before it is freed (same
                    // technique as `Secret::drop`; the optimiser can't elide it).
                    let mut bytes = c.into_bytes();
                    for b in bytes.iter_mut() {
                        unsafe { ptr::write_volatile(b, 0u8) };
                    }
                    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
                }
            }
            Ok(None) => {
                // Info/Error: no reply.
                unsafe { (*responses.add(i)).resp = ptr::null_mut() };
            }
            Err(_) => {
                // Free anything already allocated and abort the conversation.
                for j in 0..=i {
                    let r = unsafe { (*responses.add(j)).resp };
                    if !r.is_null() {
                        unsafe { ffi::free(r as *mut c_void) };
                    }
                }
                unsafe { ffi::free(responses as *mut c_void) };
                return PAM_CONV_ERR;
            }
        }
    }

    unsafe { *resp = responses };
    PAM_SUCCESS
}

/// Production authenticator backed by the real PAM stack.
pub struct SystemAuthenticator {
    tty: String,
    handle: *mut ffi::PamHandle,
    session_open: bool,
}

// The PAM handle is only ever used from the thread that created it; we never
// share it. Marking it Send lets it live in the daemon's `Box<dyn Authenticator>`
// without forcing the rest of the code to be thread-local.
unsafe impl Send for SystemAuthenticator {}

impl SystemAuthenticator {
    /// `tty` is the seat's VT (e.g. `tty1`); exposed to PAM modules via PAM_TTY.
    pub fn new(tty: impl Into<String>) -> Self {
        Self {
            tty: tty.into(),
            handle: ptr::null_mut(),
            session_open: false,
        }
    }

    fn strerror(&self, code: c_int) -> String {
        if self.handle.is_null() {
            return format!("pam error {code}");
        }
        let p = unsafe { ffi::pam_strerror(self.handle, code) };
        if p.is_null() {
            format!("pam error {code}")
        } else {
            unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
        }
    }

    fn map_err(&self, code: c_int) -> AuthError {
        match code {
            PAM_AUTH_ERR
            | PAM_USER_UNKNOWN
            | PAM_CRED_INSUFFICIENT
            | PAM_PERM_DENIED
            | PAM_MAXTRIES => AuthError::AuthFailed,
            PAM_ACCT_EXPIRED | PAM_CRED_EXPIRED | PAM_NEW_AUTHTOK_REQD => {
                AuthError::CredentialsExpired
            }
            PAM_AUTHINFO_UNAVAIL => AuthError::AccountUnavailable(self.strerror(code)),
            PAM_ABORT | PAM_CONV_ERR => AuthError::Aborted,
            other => AuthError::Pam {
                code: other,
                message: self.strerror(other),
            },
        }
    }
}

impl Authenticator for SystemAuthenticator {
    fn authenticate(
        &mut self,
        username: &str,
        conv: &mut dyn Conversation,
    ) -> AuthResult<AuthedUser> {
        let service = CString::new(crate::PAM_SERVICE).map_err(|_| AuthError::Aborted)?;
        let user_c = CString::new(username).map_err(|_| AuthError::Aborted)?;

        let mut state = ConvState { conv };
        let pam_conv = ffi::PamConv {
            conv: conversation_trampoline,
            appdata_ptr: &mut state as *mut ConvState as *mut c_void,
        };

        let mut handle: *mut ffi::PamHandle = ptr::null_mut();
        let rc =
            unsafe { ffi::pam_start(service.as_ptr(), user_c.as_ptr(), &pam_conv, &mut handle) };
        if rc != PAM_SUCCESS {
            return Err(AuthError::Pam {
                code: rc,
                message: "pam_start failed".into(),
            });
        }
        self.handle = handle;

        // Expose the seat VT to PAM modules (faillock/loginuid/audit).
        if let Ok(tty) = CString::new(self.tty.clone()) {
            unsafe { ffi::pam_set_item(handle, PAM_TTY, tty.as_ptr() as *const c_void) };
        }

        let rc = unsafe { ffi::pam_authenticate(handle, 0) };
        if rc != PAM_SUCCESS {
            let e = self.map_err(rc);
            self.end(rc);
            return Err(e);
        }

        let rc = unsafe { ffi::pam_acct_mgmt(handle, 0) };
        if rc != PAM_SUCCESS {
            let e = self.map_err(rc);
            self.end(rc);
            return Err(e);
        }

        Ok(AuthedUser {
            username: username.to_string(),
        })
    }

    fn open_session(&mut self) -> AuthResult<Vec<(String, String)>> {
        if self.handle.is_null() {
            return Err(AuthError::Aborted);
        }
        let rc = unsafe { ffi::pam_setcred(self.handle, PAM_ESTABLISH_CRED) };
        if rc != PAM_SUCCESS {
            return Err(self.map_err(rc));
        }
        let rc = unsafe { ffi::pam_open_session(self.handle, 0) };
        if rc != PAM_SUCCESS {
            return Err(self.map_err(rc));
        }
        self.session_open = true;
        Ok(self.read_envlist())
    }

    fn close_session(&mut self) -> AuthResult<()> {
        if self.handle.is_null() {
            return Ok(());
        }
        if self.session_open {
            unsafe { ffi::pam_close_session(self.handle, 0) };
            self.session_open = false;
        }
        unsafe { ffi::pam_setcred(self.handle, PAM_DELETE_CRED) };
        self.end(PAM_SUCCESS);
        Ok(())
    }
}

impl SystemAuthenticator {
    fn end(&mut self, status: c_int) {
        if !self.handle.is_null() {
            unsafe { ffi::pam_end(self.handle, status) };
            self.handle = ptr::null_mut();
        }
    }

    /// Read `pam_getenvlist` into owned (KEY, VALUE) pairs, freeing PAM's array.
    fn read_envlist(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let list = unsafe { ffi::pam_getenvlist(self.handle) };
        if list.is_null() {
            return out;
        }
        let mut i = 0isize;
        loop {
            let entry = unsafe { *list.offset(i) };
            if entry.is_null() {
                break;
            }
            let s = unsafe { CStr::from_ptr(entry) }
                .to_string_lossy()
                .into_owned();
            if let Some((k, v)) = s.split_once('=') {
                out.push((k.to_string(), v.to_string()));
            }
            unsafe { ffi::free(entry as *mut c_void) };
            i += 1;
        }
        unsafe { ffi::free(list as *mut c_void) };
        out
    }
}

impl Drop for SystemAuthenticator {
    fn drop(&mut self) {
        // Ensure the PAM handle is always released, even on early returns.
        let _ = self.close_session();
    }
}

// ---------------------------------------------------------------------------
// Non-interactive session opening for the greeter / autologin fast-paths.
// ---------------------------------------------------------------------------

/// A PAM conversation that never prompts — used by services that authenticate
/// with `pam_permit` (the greeter, autologin). It returns an empty, PAM-freeable
/// response array so libpam is satisfied if a module unexpectedly emits a prompt.
extern "C" fn null_conv(
    num_msg: c_int,
    _msg: *mut *const ffi::PamMessage,
    resp: *mut *mut ffi::PamResponse,
    _appdata: *mut c_void,
) -> c_int {
    if num_msg <= 0 || resp.is_null() {
        return PAM_CONV_ERR;
    }
    let n = num_msg as usize;
    let responses =
        unsafe { ffi::calloc(n, std::mem::size_of::<ffi::PamResponse>()) } as *mut ffi::PamResponse;
    if responses.is_null() {
        return PAM_CONV_ERR;
    }
    // All `resp` fields stay null (calloc zeroes them).
    unsafe { *resp = responses };
    PAM_SUCCESS
}

/// Open a PAM/logind session **from inside a just-forked child**, before
/// `execve`. Intended for use in `std::os::unix::process::CommandExt::pre_exec`.
///
/// Why a child, not the parent: `pam_systemd` registers the logind session with
/// the *calling* process as the session leader. By opening it here — in the
/// child that is about to become weston (or another compositor) — that process
/// becomes the leader, so libseat/weston discovers the session via
/// `sd_pid_get_session` and is granted DRM master + input on the seat. The
/// session ends when the leader exits, which `systemd-logind` reaps
/// automatically, so the handle is intentionally not closed here.
///
/// `service` is e.g. `bacak-greeter`; `putenv` holds `KEY=VALUE` entries
/// (`XDG_SESSION_CLASS=greeter`, `XDG_SESSION_TYPE=wayland`, `XDG_SEAT=…`,
/// `XDG_VTNR=…`) that `pam_systemd` reads to classify the session.
///
/// # Safety
/// Must be called in the narrow window after `fork()` and before `execve()` in a
/// single-threaded child (the BDM daemon is single-threaded). Returns the PAM
/// error code on failure so the caller can abort the exec.
pub unsafe fn open_session_preexec(
    service: &CStr,
    user: &CStr,
    tty: &CStr,
    putenv: &[CString],
) -> Result<(), c_int> {
    let conv = ffi::PamConv {
        conv: null_conv,
        appdata_ptr: ptr::null_mut(),
    };
    let mut handle: *mut ffi::PamHandle = ptr::null_mut();

    let rc = ffi::pam_start(service.as_ptr(), user.as_ptr(), &conv, &mut handle);
    if rc != PAM_SUCCESS {
        return Err(rc);
    }
    ffi::pam_set_item(handle, PAM_TTY, tty.as_ptr() as *const c_void);
    for kv in putenv {
        ffi::pam_putenv(handle, kv.as_ptr());
    }
    let rc = ffi::pam_acct_mgmt(handle, 0);
    if rc != PAM_SUCCESS {
        return Err(rc);
    }
    let rc = ffi::pam_setcred(handle, PAM_ESTABLISH_CRED);
    if rc != PAM_SUCCESS {
        return Err(rc);
    }
    let rc = ffi::pam_open_session(handle, 0);
    if rc != PAM_SUCCESS {
        return Err(rc);
    }
    // Deliberately leak `handle`: the session must outlive this call and is
    // reaped by logind when the exec'd leader process exits.
    Ok(())
}
