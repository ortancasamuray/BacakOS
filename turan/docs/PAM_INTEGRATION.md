# PAM Integration Design

## Conversation abstraction

PAM is a *driven* protocol: the stack emits messages (prompt-for-secret,
prompt-with-echo, info, error) and the application answers. BDM models exactly
this with two traits in [`bacak-pam`](../crates/bacak-pam/src/lib.rs):

```rust
enum Prompt { SecretInput(String), VisibleInput(String), Info(String), Error(String) }

trait Conversation {
    fn handle(&mut self, prompt: &Prompt) -> AuthResult<Option<String>>;
}

trait Authenticator {
    fn authenticate(&mut self, user: &str, conv: &mut dyn Conversation) -> AuthResult<AuthedUser>;
    fn open_session(&mut self) -> AuthResult<Vec<(String, String)>>;  // PAM env out
    fn close_session(&mut self) -> AuthResult<()>;
}
```

Benefits:
- The same `Authenticator` works for the **mock** backend (tests/dev), the
  **real libpam** backend, and the **autologin** path (a conversation that emits
  no prompts).
- The greeter UI never appears in PAM's type signatures — it is just *one*
  `Conversation` implementation ([`IpcConversation`](../crates/bacak-display-manager/src/auth.rs))
  that turns prompts into IPC `AuthPrompt`/`AuthInfo` frames and reads back
  `AuthResponse` secrets.

## Mapping to libpam

The `system-pam` backend ([`bacak_pam::system`](../crates/bacak-pam/src/system.rs))
binds libpam through a **small hand-written FFI** — no `bindgen`, no C headers,
no `libclang`. It declares the ~10 stable Linux-PAM ABI functions it needs and
is linked against `libpam.so.0` by the daemon's `build.rs` (by exact SONAME, so
even `libpam0g-dev`'s `libpam.so` symlink is optional). This keeps the
dependency tree tiny and auditable for a pre-login root daemon:

| BDM call          | libpam                                            |
|-------------------|---------------------------------------------------|
| `authenticate`    | `pam_start(service, user, conv)` → `pam_authenticate` → `pam_acct_mgmt` |
| `open_session`    | `pam_setcred(PAM_ESTABLISH_CRED)` → `pam_open_session`, then `pam_getenvlist` |
| `close_session`   | `pam_close_session` → `pam_setcred(PAM_DELETE_CRED)` → `pam_end` |

Conversation callbacks map 1:1:

| PAM message style          | `Prompt`               |
|----------------------------|------------------------|
| `PAM_PROMPT_ECHO_OFF`      | `SecretInput`          |
| `PAM_PROMPT_ECHO_ON`       | `VisibleInput`         |
| `PAM_TEXT_INFO`            | `Info`                 |
| `PAM_ERROR_MSG`            | `Error`                |

`PAM_TTY` is set to the seat VT (`ttyN` from `XDG_VTNR`) so `pam_faillock`,
`pam_loginuid` and audit see a coherent origin.

## Service files

BDM ships two PAM services:

- **`/etc/pam.d/bacak-display-manager`** — interactive logins. A standard
  graphical stack: `pam_faillock` (brute-force), `pam_unix` (or your directory
  module), `pam_systemd` (session registration + `XDG_RUNTIME_DIR`),
  `pam_limits`, optional `pam_gnome_keyring`. Fingerprint/PIN modules slot in as
  `sufficient`/`requisite` in the `auth` group — BDM needs no code changes.
- **`/etc/pam.d/bacak-autologin`** — passwordless path. `pam_permit` for auth,
  but full account+session management still runs so logind registers the session
  identically to an interactive login.

## PIN support

`[greeter] allow_pin = true` advertises PIN as an alternative. Mechanically a PIN
is just another `auth` factor in the PAM stack (e.g. a `pam_pin`/`pam_pkcs11`
module). Because BDM relays whatever prompts PAM emits, the greeter renders a
numeric pad when the prompt text/echo indicates a PIN; no protocol change is
needed. The choice of factor remains the administrator's, in `pam.d`.

## Threading

The PAM conversation callback is invoked **synchronously** from inside
`pam_authenticate`, on the calling thread. The FFI backend therefore takes the
front-end as a plain `&mut dyn Conversation` — the exact same shape as the mock
backend — and the daemon's `IpcConversation` is used directly. No worker thread,
no channels, no `Arc<Mutex<…>>`: when PAM needs a secret, the trampoline calls
straight into the IPC relay, which writes an `AuthPrompt` and blocks reading the
`AuthResponse`. The `pam_handle` is created, used, and freed on the one daemon
thread that owns it (the `Drop` impl guarantees `pam_end` even on early return).

## Testing

`bacak-pam`'s mock backend has unit tests proving: correct password →
auth + `open_session`; wrong password rejected; unknown user rejected with the
**same** error (no enumeration); `open_session` refused before auth. The
greeter↔daemon protocol that wraps the conversation is covered end-to-end in
`bacak-greeter/tests/protocol.rs`.
