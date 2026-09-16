# BDM Security Model

## Threat model

BDM sits on the pre-authentication boundary: it runs as root, before any user
is logged in, and accepts input from whoever is at the keyboard/touchscreen.
The adversary we design against is **an unauthenticated local attacker at the
login screen**, plus **a compromised greeter process**.

Goals:
- A compromised greeter must not be able to gain root or authenticate as another
  user.
- The login screen must not leak which usernames are valid.
- Power and seat actions must be governed by system policy (polkit), not by BDM
  alone.

## Privilege separation

| Process                 | Runs as          | Holds                                  |
|-------------------------|------------------|----------------------------------------|
| `bacak-display-manager` | root             | PAM, seat setup, fork/exec, logind calls |
| `bacak-greeter`         | `bacak-greeter`  | only the IPC socket fd; no PAM, no root |
| `bacak-session-launcher`| the target user  | only the user's own session            |

The greeter is the largest, most exposed attack surface (parses themes, renders
fonts/images, handles input). It is therefore the least privileged: a dedicated
system account with `nologin` shell, its own throwaway `HOME`, and `env_clear()`
before spawn. Everything it can ask for is enumerated in the IPC protocol
([`bacak-common::ipc`](../crates/bacak-common/src/ipc.rs)); it cannot touch PAM,
shadow, the seat, or logind directly.

The privilege drop is done in the right order (`docs/SESSION_STARTUP.md`):
`setsid` + `initgroups` while still root, then `setgid` **then** `setuid`, so no
residual group membership survives into the child.

## Authentication

- All credential validation goes through **PAM** under the
  `bacak-display-manager` service, run by the root daemon. The greeter only
  relays prompt text and secrets.
- Secrets travel over a local `SOCK_STREAM` UNIX socket (filesystem-permissioned
  `0660`, group = greeter), never over the network, and are never logged.
- **No user-enumeration oracle**: an unknown user and a wrong password return the
  same generic `AuthResult{success=false, "Authentication failed."}`. Only
  account-state conditions (expired password/account) get specific text, matching
  what PAM itself would surface post-auth.
- **Brute-force resistance** is delegated to `pam_faillock` in the PAM stack
  (see `pam/bacak-display-manager`), so lockout policy is administrator-owned.
- The mock backend (default build) is dev-only; the daemon emits a startup
  warning and the README/`--features system-pam` gate make the production path
  explicit.

## IPC hardening

- Socket lives under root-owned `/run/bacak-display-manager` (`0755`), the socket
  itself `0660` chowned to the greeter group → only the greeter user and root
  may connect.
- Framing is newline-delimited JSON with a typed schema. Malformed frames are
  dropped (treated as `CancelAuth`) rather than crashing the daemon, so a buggy
  or hostile greeter cannot wedge it.
- A protocol-version handshake (`Hello`/`Welcome`) prevents version-skew between
  daemon and greeter.

## Power & seat policy

Power actions are **not** performed by BDM directly. The daemon calls
`systemd-logind` (`org.freedesktop.login1`), which consults **polkit**. The
admin config (`[power]`) is an *additional* allow-list on top of polkit — BDM can
only ever be more restrictive, never less. Seat/VT allocation is logind's, via
the `.service` unit.

## Secret hygiene

- Secrets are kept only as long as PAM needs them; the conversation hands the
  `String` to PAM and drops it.
- A production hardening step (tracked in code comments) is to wrap secret
  buffers in a zeroizing type and use `mlock` to keep them out of swap; the
  conversation API is already shaped to localize secrets to a single call.

## Build-time posture

- `panic = "abort"` in release: no unwinding through FFI/PAM boundaries.
- `strip`, `lto`, `codegen-units = 1` for a small, predictable binary.
- `deny_unknown_fields` on config rejects typo'd/injected keys instead of
  silently ignoring them.

## Residual risks / future work

- The real PAM backend (`system-pam`) is a hand-written `libpam` FFI; it carries
  `unsafe` and warrants a focused audit (the conversation trampoline and the
  libc-allocated response array are the sharp edges). It is small and
  self-contained precisely so that audit is tractable.
- The GUI greeter is a feature-gated seam; font/image/theme parsing is the
  highest-value follow-up — run image/theme decode in a `seccomp`-confined
  helper.
- Session-lifetime PAM `close_session`/`pam_end` runs when the `AuthSlot` is
  reset for the next login and is additionally guaranteed by the authenticator's
  `Drop` impl.
