# Bacak OS — Design System

🌐 [Türkçe](DESIGN_SYSTEM.tr.md) · **English**

A premium, tech-forward visual language: glassmorphism on an Aegean gradient with a Rust accent. Touch-first interaction zones, spring physics motion, and 8pt rhythm throughout.

---

## 1. Tokens

### 1.1 Color

| Role               | Token              | Value      |
| ------------------ | ------------------ | ---------- |
| Rust accent (primary)  | `--rust-500`   | `#D96C2D`  |
| Rust accent (hover)    | `--rust-400`   | `#EF8348`  |
| Rust accent (subtle)   | `--rust-300`   | `#F9A679`  |
| Rust glow              | `--rust-glow`  | `rgba(217,108,45,0.55)` |
| Aegean — deep         | `--aegean-deep`| `#07334A`  |
| Aegean — mid          | `--aegean-mid` | `#1A6C8A`  |
| Aegean — soft         | `--aegean-soft`| `#5CB0C4`  |
| Aegean — pale         | `--aegean-pale`| `#A8DDE6`  |
| Aegean — foam         | `--aegean-foam`| `#E6F6F8`  |

**Background.** Always a multi-stop radial composite — never a flat color. Default desktop:

```css
background:
  radial-gradient(120% 80% at 80% 10%, #2c8aa3 0%, transparent 55%),
  radial-gradient( 90% 70% at 10% 90%, #0f5970 0%, transparent 60%),
  linear-gradient(180deg, #0c4e6b 0%, #0a3c55 45%, #062a3d 100%);
```

Three floating orbs at `filter: blur(80px)` add slow parallax (22 s loop). A 3 px radial-dot grain at 4 % opacity, `mix-blend-mode: overlay`, kills the banding you'd otherwise see at full screen.

### 1.2 Glass surfaces

| Surface        | Background                    | Border                       | Blur     |
| -------------- | ----------------------------- | ---------------------------- | -------- |
| Dock           | `rgba(255,255,255,0.10)`      | `rgba(255,255,255,0.22)`     | 30 px + saturate(160%) |
| Window chrome  | `rgba(12,38,56,0.55)`         | `rgba(255,255,255,0.22)`     | 28 px + saturate(140%) |
| Keyboard       | `rgba(255,255,255,0.10)`      | `rgba(255,255,255,0.22)`     | 34 px + saturate(160%) |
| Tooltip / menu | `rgba(8,26,38,0.92)` (solid)  | `rgba(255,255,255,0.14)`     | — |

Rule: anything that *floats above* the desktop blurs; anything that *carries text* does not (legibility wins). The Firefox content viewport is intentionally non-blurred — only its chrome blurs.

### 1.3 Shadow & depth hierarchy

| Elevation | Use                | Shadow                                                                              |
| --------- | ------------------ | ----------------------------------------------------------------------------------- |
| 0         | Desktop wallpaper  | none                                                                                |
| 1         | Inactive window    | `0 18px 60px -18px rgba(2,28,42,0.55), 0 2px 12px -4px rgba(2,28,42,0.35)`         |
| 2         | Focused window     | `0 24px 72px -16px rgba(2,28,42,0.70), 0 0 36px -4px var(--rust-glow)`              |
| 3         | Dock / OSK         | inherits 1 + inner highlight `inset 0 1px 0 rgba(255,255,255,0.18)`                 |

Focus = a soft Rust-tinted glow, never a hard outline. Inactive windows lose the glow *and* drop to `opacity: 0.92 + filter: saturate(85%)` — present but quiet.

### 1.4 Spacing (8pt)

`4 · 8 · 12 · 16 · 20 · 24 · 32 · 40` → `--s-1 … --s-10`. Half-step `4 px` is allowed only for icon-internal padding. Everything else lands on the 8 grid.

### 1.5 Radius

| Token        | Value | Use                          |
| ------------ | ----- | ---------------------------- |
| `--r-sm`     | 8 px  | input pills, key caps inner  |
| `--r-md`     | 14 px | cards, dock icons            |
| `--r-lg`     | 22 px | windows, snap preview         |
| `--r-xl`     | 32 px | dock container, OSK shell    |
| `--r-pill`   | 999   | address bar, tag chips       |

### 1.6 Typography

- **Family.** `Inter` first, falls back through SF Pro Text, system-ui.
- **Scale.** 28 (display) · 18 (h2) · 15 (body large) · 14 (body) · 13 (compact) · 12 (caption) · 10 (meta).
- **Weight.** 400 body, 500 UI labels, 600 emphasis, 700 reserved for hero headings.
- **Tracking.** Negative for the display headline (`-0.02em`); +0.3 px for the digital clock to keep the colon centered.

The hero `<h1>` uses a gradient text fill from white → `--rust-300` at 90 % — the *one* place we mix the warm accent into otherwise cool typography.

### 1.7 Motion

| Curve            | Value                                  | Use                                  |
| ---------------- | -------------------------------------- | ------------------------------------ |
| `--ease-spring`  | `cubic-bezier(0.34, 1.56, 0.64, 1)`    | Window snap, dock magnify, key press |
| `--ease-out`     | `cubic-bezier(0.16, 1, 0.3, 1)`        | Opacity, color, snap preview fade    |
| `--ease-inout`   | `cubic-bezier(0.65, 0, 0.35, 1)`       | Workspace pan, ambient orb drift     |

| Duration | Token       | Use                                |
| -------- | ----------- | ---------------------------------- |
| 140 ms   | `--t-fast`  | Hover, tooltip, key press          |
| 280 ms   | `--t-med`   | Window move/snap, magnification    |
| 460 ms   | `--t-slow`  | OSK open/close, workspace switch   |

Rule: **never linear.** The only linear motion in the system is the clock.

---

## 2. Components

### 2.1 Dock

```
┌────────────────────────────────────────────────────────────┐
│  ▢  ▣  ▢  ▢  ▢  │  📶  🔋  🔊  │  14:22                   │
│   ·  •  ·  ·  ·                       Mon, 11 May         │
└────────────────────────────────────────────────────────────┘
```

- 48 px app tiles, 8 px gaps, 12 px padding inside the glass shell.
- **Magnification.** Hover scales the focused icon to 1.18× and lifts it 10 px; *neighbors* scale to 1.06× via `:has(+ .dock-app:hover)`. No JS for the magnify — pure CSS.
- **Indicator.** A 4 px dot, 6 px below the icon, in Rust accent with a soft glow. Indicator appears only when the app has at least one live window.
- **States.**
  - *Idle.* Full opacity, full glass.
  - *App focused.* Dock dims to `opacity: 0.72` (`.dock--dimmed`).
  - *Fullscreen.* Dock translates `Y+100% + 24` and `opacity: 0`. Edge-reveal: a 6 px hit zone at screen bottom unhides on hover.
- **Tray.** Network, battery, sound, clock. Tooltips appear above on hover with 140 ms fade.
- **Right-click (planned).** Context menu with recents, pinned actions, "Show all windows" → fires task switcher pre-filtered by app.

### 2.2 Window

```
┌────────────────────────────────────────────────────┐
│ ●●●   Mozilla Firefox                              │  ← title bar (36 px)
├────────────────────────────────────────────────────┤
│ ‹ › ⟳   [ ⌬  https://_____________ ]   ≡          │  ← app chrome
├────────────────────────────────────────────────────┤
│                                                    │
│              ( app viewport )                      │
│                                                    │
│                                              ╲╲   │  ← resize handle
└────────────────────────────────────────────────────┘
```

- Title bar drags the window. Controls (close/min/max) live left, mac-style — the most common chrome pattern for glassmorphic designs.
- Focus state adds the Rust-tinted glow; blur state desaturates and dims to 92 %.
- **Snap zones.** Hot 24 px edges. Drag near → an orange-tinted preview rectangle springs in with the spring curve, then the window commits on `pointerup`. Zones: left/right half, top = maximize, four corners = quadrant tile.
- **Resize.** Bottom-right grip for the prototype; production will support all 8 directions with cursor-appropriate handles invisible until hover.

### 2.3 On-screen keyboard

```
┌──────────────────────────────────────────────────────┐
│  [https://] [bacak.dev] [rust-lang.org]  …           │ ← suggestions (rounded pills)
│  1 2 3 4 5 6 7 8 9 0                                 │
│  q w e r t y u i o p                                 │
│   a s d f g h j k l                                  │
│  ⇧  z x c v b n m  ⌫                                │
│  ⎚   @  .  [ space ]  /  ⏎                          │
└──────────────────────────────────────────────────────┘
```

- Slides up from the bottom on input focus using `--ease-spring` over 460 ms.
- 44 px key height — Apple's minimum touch target. Modifier keys (`⇧ ⌫ ⏎ ⎚`) get `key--wide`; space gets `key--space` (flex-grow 6).
- **Layout-aware.** On open, the OSK emits a "reserve area" event to the WM. The focused window's content scrolls so the focused input stays at least 16 px above the keyboard top — never just covers it.
- **Predictive suggestions.** Pill row above the keys. Tap inserts the suggestion. In v1 these are static; the architecture leaves room for an on-device n-gram or small ONNX LM.
- **Modes.** Default (full-width), floating (drag handle on the top edge), split (two halves docked to bottom corners for tablet thumb-typing). Mode toggle in `⎚` long-press.

### 2.4 Task switcher (overview)

- 3-finger swipe up or `Alt+Tab` opens a full-screen grid of live window thumbnails grouped by app.
- Each card shows a real-time preview at ~240 px wide with the app icon overlaid bottom-left.
- Horizontal pan navigates between workspaces; cards from adjacent workspaces slide in from the sides.

### 2.5 File manager

- **Layout.** Adaptive: single-pane on narrow widths, dual-pane above 1280 px (Miller columns optional).
- **Archives.** A `.zip` or `.7z` renders as a directory icon with a small ribbon badge. Double-click navigates into it; the breadcrumb shows `archive.7z › docs › api.md`.
- **Touch zones.** Row height 56 px; long-press (550 ms) enters multi-select with checkboxes appearing in a slide-down animation.
- **Drag-and-drop compression.** Drop a selection onto a `.zip` → "Add to archive" sheet; drop a `.7z` onto an empty pane → "Extract here".
- **Previews.** Hover (or tap-and-hold) over an image shows a 320 px preview popover with EXIF dateline; video shows a scrubbable thumbnail strip.

---

## 3. Interaction patterns

### 3.1 Snap

1. Drag begins → window enters "transient drag" state; opacity 0.95, slight downscale 0.98×.
2. Cursor crosses snap threshold → preview overlay fades in (`140 ms`, `--ease-out`).
3. Cursor leaves threshold → preview fades out (`140 ms`).
4. Release inside threshold → window springs to the preview's geometry (`280 ms`, `--ease-spring`). The preview itself dissolves into the window during the transition.

### 3.2 Magnify

Pure CSS. Hover on a dock tile applies `translateY(-10px) scale(1.18)`; sibling combinator (`:has(+ .dock-app:hover)`) and (`.dock-app:hover + .dock-app`) propagate a softer `scale(1.06)` to immediate neighbors. No JS, no rAF, no jank.

### 3.3 Gestures

| Gesture                | Action                       |
| ---------------------- | ---------------------------- |
| 3-finger swipe ←/→     | Switch workspace              |
| 3-finger swipe ↑       | Task overview                 |
| 4-finger pinch         | Show desktop                  |
| Edge swipe from bottom | Reveal dock in fullscreen     |
| Long-press (touch)     | Multi-select / context menu   |
| Two-finger tap         | Right-click equivalent        |

### 3.4 Focus & dimming

- Active window: full opacity, Rust-tinted shadow glow, sharp.
- Inactive window: `opacity: 0.92`, `saturate(85%)`, no glow. Subtle but unmistakable.
- The dock follows the same grammar: focused-app context = dim the dock to 72 %.

---

## 4. Accessibility

- All interactive elements ≥ 44 × 44 logical px (WCAG 2.5.5 target).
- Tooltips have `role="tooltip"` and are referenced by `aria-describedby` on their target.
- The OSK respects `inputmode` and `enterkeyhint` of the focused field and rerenders the action key (`⏎` becomes "Go", "Search", etc.).
- `prefers-reduced-motion` collapses every transition to 1 ms.
- Color: WCAG AA against the Aegean background for all text (foreground `--glass-fg` = 92 % white, AA at 13 px+).
- Keyboard nav: every dock and window control is `Tab`-reachable with a visible focus ring (`box-shadow: 0 0 0 3px rgba(217,108,45,0.45)`).

---

## 5. Iconography

- Outline-style monoline icons at 1.5 px stroke, optical-rounded joins, 24 × 24 grid.
- App icons are 2-color: primary glyph in Rust or Aegean pale, secondary detail in white at 60 %.
- System tray glyphs (network, battery, sound) use `currentColor` so the dock dim state propagates.

---

## 6. Token map (Tailwind preset, excerpt)

```ts
// ui/src/design/tokens.ts
export const tokens = {
  color: {
    rust:   { 300: "#F9A679", 400: "#EF8348", 500: "#D96C2D" },
    aegean: { 050: "#E6F6F8", 100: "#A8DDE6", 300: "#5CB0C4",
              500: "#1A6C8A", 700: "#07334A" },
    glass:  { fg: "rgba(255,255,255,0.92)",
              fgDim: "rgba(255,255,255,0.62)",
              bg: "rgba(255,255,255,0.10)",
              border: "rgba(255,255,255,0.22)" },
  },
  radius: { sm: 8, md: 14, lg: 22, xl: 32, pill: 9999 },
  space:  [0, 4, 8, 12, 16, 20, 24, 32, 40],
  motion: {
    spring: "cubic-bezier(0.34, 1.56, 0.64, 1)",
    out:    "cubic-bezier(0.16, 1, 0.3, 1)",
    inout:  "cubic-bezier(0.65, 0, 0.35, 1)",
  },
  duration: { fast: 140, med: 280, slow: 460 },
  blur:     { dock: 30, window: 28, osk: 34 },
} as const;
```

This file is the single source of truth — the CSS variables in `styles.css` and the Tailwind config both consume it. Adding a new accent or radius is a one-line change here.
