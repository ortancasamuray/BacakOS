# systemd Service Design

## The unit

`systemd/bacak-display-manager.service` (installed to
`/usr/lib/systemd/system/`) is a conventional display-manager unit:

```ini
[Unit]
Conflicts=getty@tty1.service
After=systemd-user-sessions.service getty@tty1.service systemd-logind.service
Wants=systemd-logind.service

[Service]
Type=simple
ExecStart=/usr/bin/bacak-display-manager
TTYPath=/dev/tty1
Environment=XDG_VTNR=1
User=root
Restart=always
ExecStopPost=/usr/bin/loginctl terminate-seat seat0

[Install]
Alias=display-manager.service
WantedBy=graphical.target
```

Key decisions:

- **`Alias=display-manager.service`** — distros symlink the active DM to this
  generic name. Enabling BDM (`systemctl enable bacak-display-manager`) makes it
  *the* display manager, mutually exclusive with GDM/SDDM/LightDM.
- **`Conflicts=getty@tty1` + `TTYPath=/dev/tty1`** — BDM takes VT1. logind does
  the actual seat/VT bookkeeping; the daemon just needs to own the tty.
- **`Type=simple` + `Restart=always`** — if the daemon dies, systemd brings the
  login screen straight back. Sessions are *not* children constrained by the
  unit's resource directives — they run as the user under logind's own scope, so
  a daemon restart doesn't kill a live desktop.
- **`ExecStopPost=loginctl terminate-seat seat0`** — clean teardown of stragglers
  when the DM stops.

## Lifecycle

```
systemctl start bacak-display-manager      → daemon up, greeter shown
systemctl stop  bacak-display-manager      → greeter killed, seat terminated
systemctl restart bacak-display-manager    → back to greeter (live sessions survive)
systemctl enable  bacak-display-manager    → becomes display-manager.service
```

## Interaction with logind

BDM deliberately offloads to `systemd-logind` rather than reimplementing seat
management:

- **Seat/VT allocation** — logind owns `seat0` and the VTs.
- **Session registration** — `pam_systemd.so` in the PAM session stack creates
  the logind session, sets up `XDG_RUNTIME_DIR=/run/user/<uid>` (an automounted
  tmpfs), and puts the session in its own cgroup scope.
- **Power actions** — the daemon's `power` module calls logind verbs
  (`PowerOff`/`Reboot`/`Suspend`/`Hibernate`), which go through polkit.

This means BDM does not need `CAP_SYS_ADMIN` gymnastics for VT switching or a
private seat database — it cooperates with the platform.

## Autologin under systemd

When `[autologin] enabled = true`, the daemon skips the greeter and goes
straight to `bacak-session-launcher` for the configured user, running the
`bacak-autologin` PAM service so logind still registers a first-class session.
`delay_seconds` allows a cancel window (e.g. to reach the greeter by holding a
key), implemented in the daemon before the launcher fork.

## Packaging notes

A package should install:
- binaries → `/usr/bin/{bacak-display-manager,bacak-greeter,bacak-session-launcher}`
- unit → `/usr/lib/systemd/system/bacak-display-manager.service`
- PAM → `/etc/pam.d/{bacak-display-manager,bacak-autologin}`
- config → `/etc/bacak-display-manager.conf`
- session → `/usr/share/wayland-sessions/bacak.desktop`
- create the system user `bacak-greeter` (nologin shell, home
  `/var/lib/bacak-greeter`).
