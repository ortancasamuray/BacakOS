# Session Startup & Shutdown Sequence

## Startup (post-authentication)

When `AuthSlot::finish_session` resolves a session and `open_session` returns the
PAM environment, the daemon performs:

```
1. Resolve target user           seat::lookup_user → (uid, gid, home, shell)
2. Ensure XDG_RUNTIME_DIR         /run/user/<uid>, chown to the user (logind also
                                  manages this via pam_systemd)
3. Build the child environment    env_clear(), then set a clean base:
      HOME, USER, LOGNAME, SHELL, PATH
      XDG_RUNTIME_DIR, XDG_SEAT, XDG_SESSION_CLASS=user
      XDG_SESSION_TYPE = wayland | x11   (from the .desktop kind)
      XDG_CURRENT_DESKTOP = DesktopNames (if present)
      + all KEY=VALUE pairs from pam_getenvlist (DBUS, ccache, cgroup…)
      BDM_SESSION_EXEC, BDM_SESSION_ID   (handed to the launcher)
4. Fork + drop privileges         pre_exec: setsid + initgroups(user, gid)
                                  then setgid(gid), then setuid(uid)
5. exec bacak-session-launcher    runs entirely as the user
6. Launcher:                      source profile → dbus-run-session wrap →
                                  execve(Exec line from the .desktop)
7. Daemon waits on the session    (its process-group leader)
```

### Why a separate launcher binary?

Step 6 — "start a desktop" — is the most varied, third-party-code-heavy part of
login (shell profiles, D-Bus, the DE's own startup). Isolating it in a tiny
unprivileged binary (`bacak-session-launcher`) means none of that ever runs with
elevated privilege, and the daemon's privileged code stays small and auditable.
The launcher only ever runs *after* the daemon has already dropped to the user.

### Privilege-drop ordering (critical)

```
pre_exec (still root):  setsid()              # new session, detach controlling tty
                        initgroups(user,gid)  # supplementary groups, needs root
std applies:            setgid(gid)           # drop primary group
                        setuid(uid)           # drop user LAST
```

Dropping uid before gid/groups would leave the child with residual group
privileges; doing groups+gid first, uid last, is the only safe order. This is
implemented in `launch::as_user`.

## The Exec line

`bacak-session-launcher` parses the `.desktop` `Exec=` value with a POSIX-ish
splitter (quotes, escapes) and strips freedesktop `%`-field codes (session
entries don't use them). It then wraps the command in `dbus-run-session` when no
session bus is present, and `execve`s — replacing itself so the session becomes
the process the daemon waits on.

## Logout / shutdown

```
1. Session process exits          daemon's wait() returns
2. PAM close                      pam_close_session + pam_setcred(DELETE) +
                                  pam_end  (AuthSlot::reset / Drop, system-pam)
3. Cleanup                        logind tears down the session scope and
                                  XDG_RUNTIME_DIR; the daemon kills strays
4. Return to greeter              main loop re-binds the socket and respawns
                                  bacak-greeter
```

For **autologin**, step 4 loops back into the session for the same user rather
than showing a greeter (configurable).

## State persistence

`/var/lib/bacak-display-manager/last-user` and `last-session` record the most
recent successful login so the greeter can pre-select them (gated by
`[greeter] remember_last_user` / `remember_last_session`). These are written by
the daemon, not the greeter, so the unprivileged UI can't forge "last login".
