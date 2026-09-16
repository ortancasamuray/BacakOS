# BDM Architecture

🌐 [Türkçe özet](ARCHITECTURE.tr.md) · **English**

## 1. Component & privilege overview

```
                       ┌──────────────────────────────────────────────┐
   ROOT  (uid 0)       │             bacak-display-manager             │
                       │  ┌───────┐ ┌──────┐ ┌──────┐ ┌───────┐ ┌────┐ │
                       │  │ config│ │ seat │ │ ipc  │ │ auth  │ │powr│ │
                       │  └───────┘ └──────┘ └───┬──┘ └───┬───┘ └─┬──┘ │
                       └─────────────────────────┼────────┼───────┼────┘
                                  UNIX socket     │  PAM   │ logind│ (D-Bus)
                       /run/bacak-display-manager/│greeter │       │
                                  greeter.sock    │.so     │       ▼
   GREETER user ┌───────────────────────┐         │     ┌──────────────────┐
  (unprivileged)│     bacak-greeter      │◀────────┘     │ systemd-logind / │
                │ GreeterClient + GUI    │  prompts/      │ polkit           │
                │ on bacak-compositor    │  answers       └──────────────────┘
                └───────────────────────┘
                                  fork + drop privileges
   TARGET user  ┌───────────────────────────────────────────────┐
   (the human)  │ bacak-session-launcher → bacak-compositor → DE │
                └───────────────────────────────────────────────┘
```

Three privilege domains, crossed exactly twice:

1. **root → greeter user**: the daemon forks and drops to `bacak-greeter` to run
   the login UI. The UI talks back only through the IPC socket.
2. **root → target user**: after authentication, the daemon forks and drops to
   the authenticated user to run the session launcher.

The greeter never holds root and never links PAM. logind/polkit enforce power
and seat policy independently of BDM.

## 2. Rust module structure

```
bacak-common (lib, no system deps — unit-tested everywhere)
├── config      Config + sub-structs, TOML load, defaults, deny_unknown_fields
├── users       UserProvider trait, PasswdProvider, UID-range/shell/ hidden policy
├── sessions    .desktop parsing, Wayland/X11 discovery, dedup + precedence
├── ipc         Request/Response enums (newline-delimited JSON), PROTOCOL_VERSION
├── power       PowerAction ↔ logind method ↔ polkit action, config gating
└── error       crate Error/Result

bacak-pam (lib)
├── lib         Authenticator + Conversation traits, Prompt, AuthError, PAM_SERVICE
├── mock        MockAuthenticator (tests/dev), in-memory creds
└── system      SystemAuthenticator (feature system-pam) — real libpam

bacak-display-manager (bin, root)
├── main        root check, config load, autologin-vs-greeter, main loop
├── seat        runtime dir, greeter/user id lookup, XDG_RUNTIME_DIR
├── ipc         GreeterServer (accept-one), Conn framing, Outcome, touchscreen probe
├── auth        AuthSlot, IpcConversation (PAM↔IPC relay), backend selection
├── launch      privilege-dropping spawns: greeter, session, autologin
└── power       logind delegation (systemctl / zbus), config + polkit gating

bacak-greeter (bin + lib, unprivileged)
├── lib         GreeterClient (connect, list, auth loop, start session, power)
└── main        reference TTY frontend; gui module behind `gui` feature

bacak-session-launcher (bin, the user)
└── main        Exec-line word-split, D-Bus session wrap, execve of the session
```

## 3. Login workflow (happy path)

```
 daemon                         greeter                         PAM / logind
   │  spawn greeter (uid drop)    │                                  │
   │─────────────────────────────▶                                  │
   │              Hello           │                                  │
   │◀─────────────────────────────                                  │
   │      Welcome{touchscreen,    │                                  │
   │        last_user, last_sess} │                                  │
   │─────────────────────────────▶ render: logo, user grid, field   │
   │         ListUsers/ListSessions                                  │
   │◀──────────────▶ Users / Sessions                               │
   │      StartAuth{username}     │                                  │
   │◀─────────────────────────────                                  │
   │   pam_start + pam_authenticate ───────────────────────────────▶│
   │      AuthPrompt{"Password:", echo=false}                        │
   │─────────────────────────────▶ (show masked field / vkbd)       │
   │      AuthResponse{secret}     │                                  │
   │◀─────────────────────────────                                  │
   │   feed secret to PAM ─────────────────────────────────────────▶│
   │   pam_acct_mgmt OK                                              │
   │      AuthResult{success}      │                                  │
   │─────────────────────────────▶                                  │
   │      StartSession{session_id} │                                  │
   │◀─────────────────────────────                                  │
   │   pam_setcred + pam_open_session ─────────────────────────────▶│ (logind
   │      SessionStarting          │                                  registers
   │─────────────────────────────▶ greeter tears down                session)
   │   fork → drop to user → exec bacak-session-launcher → compositor
```

Failure branches: a wrong secret yields `AuthResult{success=false}` with a
deliberately generic message; the greeter returns to the field. `CancelAuth`
aborts an in-flight conversation. A daemon-side error (unknown session, denied
power action) is reported as `Error{message}` without tearing down the greeter.

## 4. Concurrency model

The daemon is single-greeter and fully synchronous: one `accept()`, one
serve-to-completion loop. This keeps the privilege boundary auditable. Both PAM
backends (mock and the real libpam FFI) invoke the conversation synchronously on
this thread — when PAM needs a secret, the conversation writes an `AuthPrompt`
and blocks reading the `AuthResponse` on the same socket. No worker threads or
channels are involved, so there is no shared mutable state to reason about.

## 5. Multi-monitor

The greeter is a Wayland client of `bacak-compositor`. Output handling is the
compositor's job:

- **Mirrored login**: the compositor mirrors the greeter surface to every
  connected output (default), so the login card appears centered on each.
- **Independent outputs**: the compositor can instead place the card on the
  primary output and show wallpaper-only on the others.
- Hotplug (connect/disconnect) is handled by the compositor's output manager;
  the greeter simply re-lays-out on `wl_output` changes.

See [GREETER_UI.md](GREETER_UI.md) for the visual design.
