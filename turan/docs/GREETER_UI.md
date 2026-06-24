# Greeter UI Design

The greeter is a Wayland client of `bacak-compositor`. Its logic lives in the
display-free [`GreeterClient`](../crates/bacak-greeter/src/lib.rs) core; the
graphical frontend (behind `--features gui`,
[`src/gui.rs`](../crates/bacak-greeter/src/gui.rs), built with **egui/eframe** —
winit on Wayland/X11, glow renderer) is a thin renderer over that core. Because
the IPC protocol is blocking, the GUI runs all client calls on a worker thread
and polls events each frame (immediate mode). The reference **TTY** frontend
drives the identical flow, so the protocol is exercised the same way headless or
graphical. A bundled `mock_daemon` example serves the daemon side so the GUI can
be run without root, PAM, or a compositor.

## Layout

```
┌─────────────────────────────────────────────────────────────┐
│                                                             ◑ │  ← clock / a11y
│                                                               │
│                        ⬡  Bacak                               │  logo
│                                                               │
│        ┌────────┐   ┌────────┐   ┌────────┐                   │
│        │  ◓ Ayşe │   │  ◓ Meh │   │   + …  │                   │  user grid
│        └────────┘   └────────┘   └────────┘                   │  (avatars)
│                                                               │
│            ┌─────────────────────────────────────┐           │
│            │  Password / PIN              👁  ⌨   │           │  secret field
│            └─────────────────────────────────────┘           │
│                                                               │
│            ┌──────────┐   ▾ Bacak Desktop                     │  login + session
│            │  Log in   │     (session menu)                   │
│            └──────────┘                                       │
│                                                               │
│   ⏻ Shutdown    ⟳ Restart    ☾ Suspend    🔒 Lock            │  power bar
└─────────────────────────────────────────────────────────────┘
                     (blurred wallpaper behind)
```

This mirrors the brief: logo → user list → username/password → login button →
power menu. The card floats over the (optionally blurred) wallpaper.

## User selection

- Local, login-capable users only (policy in `bacak-common::users`: UID range,
  non-`nologin` shell, hidden-list, root excluded by default).
- Each tile shows **avatar** (`~/.face`, else `/var/lib/AccountsService/icons/<user>`),
  **full name** (GECOS) with username subtitle.
- The **last logged-in user** is pre-selected (`Welcome.last_user`) when
  `[greeter] remember_last_user`.
- A `+ …` tile reveals a free-form username field when `allow_manual_login`, and
  a **Guest** tile when `[greeter] allow_guest`.

## Authentication UX

- The secret field renders per the PAM prompt: masked for `echo=false`
  (password/PIN), visible for `echo=on` (e.g. OTP token). The `👁` toggle reveals
  the field on demand.
- `Info`/`Error` PAM messages appear inline above the button (e.g. "Password
  expired").
- A numeric pad is shown automatically when the prompt indicates a PIN
  (`allow_pin`).

## Virtual keyboard

Driven by Wayland's `input-method-v2` + `virtual-keyboard-v1`:

- **When shown** — `[keyboard] mode`: `auto` shows it only when a touchscreen is
  present (`Welcome.touchscreen`, probed by the daemon from
  `/proc/bus/input/devices`); `always`/`never` force it. With `show_on_focus`,
  it slides up when a text/secret field gains focus and hides on blur.
- **Layout** from `[keyboard] layout`. The keyboard injects keystrokes through
  the input-method protocol so the secret never round-trips through anything but
  the focused field.

## Touch support

- All interactive targets are sized ≥ 48 px (scaled by `[accessibility] scale`),
  satisfying finger-friendly hit areas.
- Gestures: **tap** to select a user/button, **double-tap** a user tile to log in
  with the remembered session, **long-press** a power button for a confirm
  dialog (avoids accidental shutdown).
- The user grid is horizontally swipeable when it overflows.

## Multi-monitor

Handled by the compositor (see ARCHITECTURE.md §5):

- **Mirrored** (default): the login card is mirrored/centered on every output.
- **Independent**: card on the primary output, wallpaper-only elsewhere.
- Output hotplug re-lays-out via `wl_output` events; no greeter restart.

## Wallpaper

`[wallpaper] mode`:
- `solid` — palette color from the theme accent.
- `image` — single image, optionally Gaussian-blurred (`blur_sigma`) behind the
  card for legibility.
- `slideshow` — cycles images from `slideshow_dir` every `slideshow_interval`
  seconds.

## Theming

- `[theme] mode`: `dark` / `light` / `auto` (follows
  `org.freedesktop.appearance` when available).
- `accent` color, `name`d theme bundle under
  `/usr/share/bacak-display-manager/themes`, and `logo` path.
- High-contrast and large-font presets come from `[accessibility]` and override
  the theme palette/metrics.

## Accessibility

- **Scaling** — `[accessibility] scale` drives global UI and touch-target sizing
  (1.0 / 1.25 / 1.5 for HiDPI and touch).
- **Large fonts** and **high contrast** presets.
- **Screen reader** — when `screen_reader`, the greeter exposes an AT-SPI tree
  and can start an Orca bridge so the login screen itself is navigable
  non-visually.
- Full keyboard navigation (Tab/arrows/Enter) is always available, independent of
  pointer/touch.

## Frontend ↔ core contract

The GUI implements exactly the `run_tty` flow on `GreeterClient`:

```
connect_from_env() → Welcome
list_users(), list_sessions()
start_auth(user) → loop { Prompt → answer(secret) | Info | Done }
start_session(session_id)            // on success: exit, compositor hands over
power(action)                        // from the power bar
```

Because all protocol logic is in the core, the GUI can be reskinned or replaced
without touching the security-relevant conversation handling.
