# kur — Architecture

A Slint-UI wizard over a pure-Rust installer backend. The hard rule this
codebase is built around, stated in `main.rs`'s own doc comment: **the
backend never imports the UI, and the UI never runs a command.**

---

## 1. Module map

```
kur/
├─ src/
│  ├─ main.rs        # controller shell: wizard state, step navigation,
│  │                  # bridges the install thread to the Slint event loop
│  ├─ pages.rs        # per-page Slint callback wiring (steps 0–3)
│  ├─ wizard.rs        # the disk page's mutable selection state + rules
│  │                  # ("assign root role → forces a format", etc.)
│  ├─ headless.rs      # env-var-driven unattended install — same engine, no window
│  └─ backend/         # everything that touches the system — UI-agnostic, unit-tested
│     ├─ mod.rs
│     ├─ disk.rs        # block-device discovery via `lsblk -J`
│     ├─ plan/          # partition planning — inspectable, hardware-free `Plan`
│     │  ├─ mod.rs
│     │  └─ rules.rs
│     ├─ install.rs     # the install engine: stage table, threading, progress math
│     ├─ stages.rs      # individual installation stages, in run order
│     ├─ medium.rs      # locates the squashfs image(s) kur is running from
│     ├─ locale.rs      # locale/keyboard layout discovery (reads locales/xkb-data)
│     ├─ timezone.rs    # timezone discovery (parses tzdata's zone1970.tab)
│     ├─ user.rs        # account/hostname validation (adduser(8) + RFC 1123 rules)
│     └─ cmd.rs         # shared subprocess-invocation helpers
├─ ui/                  # Slint UI: main.slint, pages.slint, models.slint, theme.slint
├─ scripts/
│  ├─ kur-baslat         # user-side launcher — the actual entry point (see README)
│  ├─ kur-root           # root-side wrapper kur-baslat invokes via sudo
│  ├─ test-vm-boot.sh    # boots the live image in a VM
│  └─ test-vm-install.sh # full install against a VM loop device
└─ debian/               # packaging: changelog, lintian-overrides
```

---

## 2. UI ↔ backend boundary

- `main.rs` owns the wizard shell: which step is active, the shared
  `RefCell`/`Rc` state, and the bridge between the backend's install thread
  and Slint's event loop.
- `pages.rs` owns what happens *inside* a page — one `wire_*` function per
  step, called once at startup, installing that page's Slint callbacks.
- `backend/` deals purely in data types and reports progress through
  callbacks. No backend module knows the UI exists, which is what makes it
  unit-testable and lets the exact same engine drive `headless.rs` with no
  window at all.

## 3. Disk planning vs. execution

`backend::plan::Plan` is a complete, inspectable description of what the
installer is about to do to a disk — built, validated, and rendered to text
entirely without touching hardware, so it's fully unit-testable.
`backend::install` is the only module that ever executes a `Plan`.

## 4. Install engine threading model

`backend::install::spawn` moves the whole install onto a plain
`std::thread` and reports back through a `Fn(Progress) + Send` callback —
there is no async runtime here. `stages.rs` defines the stage table
(`install::STAGES`) executed in order; every stage is expected to be
restartable from a wiped disk on failure — none of them attempt to be
idempotent against a half-built target. `medium.rs` locates the squashfs
image(s) the live session itself booted from; `stages::extract_squashfs`
unpacks that same stack onto the target disk instead of rebuilding it via
`debootstrap`.

## 5. Root privilege handshake

`kur` needs root to partition disks, but the live session's autologin user
has no real password, so `pkexec` (which requires PAM auth) always fails.
Instead:

```
kur-baslat (user, live session)
    │  forwards WAYLAND_DISPLAY / XDG_RUNTIME_DIR / DISPLAY as argv
    │  (sudo strips them from the environment)
    ▼
sudo kur-root $WAYLAND_DISPLAY $XDG_RUNTIME_DIR $DISPLAY
    │  root can open the user's Wayland socket under /run/user/<uid>
    │  regardless of file permissions
    ▼
kur (runs as root, renders into the user's compositor session)
```

The live user already has passwordless `sudo` (standard `live-boot`
behaviour — whoever physically booted the medium is already trusted), so
this handshake needs no additional authentication step.

## 6. Headless mode

`headless.rs` is the preseed-style counterpart to the wizard: identical
engine, driven entirely by environment variables (`KUR_HEADLESS=1` triggers
it from `main` before any Slint window is created). It exists so the
installer can be exercised end-to-end in CI or a VM against a loop device,
where there's no display to click — see `scripts/test-vm-install.sh`.

## 7. Rendering

The UI uses Slint's software renderer only (`backend-winit-wayland` +
software rendering, no GPU backend) — the one renderer guaranteed to work on
a live image with no GPU driver loaded. Compiled Slint dependency is shared
with `altay`.

## 8. Testing

```sh
cargo test                      # disk/plan/locale/timezone/user validation logic
scripts/test-vm-boot.sh         # live-image boot smoke test
scripts/test-vm-install.sh      # full install against a real (virtual) disk
```

Unit tests deliberately stay hardware-free; the VM scripts are what actually
exercise `backend::install` end-to-end.
