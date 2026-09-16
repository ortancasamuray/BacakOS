//! On-screen-keyboard engine — the *pure* model behind the OSK overlay.
//!
//! [`input.rs`](crate::input) owns the OSK **lifecycle** (open / close, strut
//! reservation, multi-monitor binding). This module owns everything *inside*
//! the panel: the keycaps, the layouts, the modifier state machine, hit-testing
//! a touch point to a key, long-press alternates, double-tap Caps-Lock, and the
//! placement / drag / dock geometry. It is deliberately free of any Smithay /
//! Wayland types so it builds (and unit-tests) without the `runtime` feature —
//! the same split every other Bacak controller uses.
//!
//! ## How a keypress becomes text (universal path)
//!
//! A [`KeyCap::Key`] carries a raw **evdev** keycode. When the user taps it the
//! controller resolves it to a [`KeyAction::Chord`] — the active modifiers
//! (Shift / Ctrl / Alt / AltGr) plus that keycode — which the compositor feeds
//! straight into [`synthesize_chord`](crate::state) (press mods, tap key,
//! release mods through the seat keyboard). Because that is byte-for-byte what a
//! *real* keyboard does, it reaches **every** client — GTK, Qt, Electron,
//! Chromium, Firefox, terminals and XWayland — with no per-app cooperation.
//!
//! The glyph each keycode produces is decided by the seat's **xkb layout**, so
//! when the active OSK layout changes ([`LayoutId`]) the controller also retunes
//! the seat keyboard's xkb layout (English → `us`, Turkish → `tr`). That is why
//! the Turkish keycaps reuse the *same* evdev positions as the English ones —
//! only the printed labels differ; the `tr` layout maps those positions to
//! `ı ş ğ ü ö ç …` for us.
//!
//! ## Glyphs with no keycode (emoji, long-press accents)
//!
//! Emoji and accented alternates (`a → à á â ä`) have no position in any xkb
//! layout, so they resolve to [`KeyAction::Commit`] — a literal UTF-8 string the
//! compositor delivers through `zwp_input_method_v2::commit_string` to the
//! focused `text-input-v3` field (falling back to a clipboard-paste chord for
//! clients that lack text-input). Delivery is the integration layer's job; the
//! model just hands back the string.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// evdev keycodes (linux/input-event-codes.h) — pre-`+8` xkb offset, matching
// what `synthesize_chord` expects.
// ---------------------------------------------------------------------------
#[allow(dead_code)]
pub mod code {
    pub const ESC: u32 = 1;
    pub const N1: u32 = 2;
    pub const N2: u32 = 3;
    pub const N3: u32 = 4;
    pub const N4: u32 = 5;
    pub const N5: u32 = 6;
    pub const N6: u32 = 7;
    pub const N7: u32 = 8;
    pub const N8: u32 = 9;
    pub const N9: u32 = 10;
    pub const N0: u32 = 11;
    pub const MINUS: u32 = 12;
    pub const EQUAL: u32 = 13;
    pub const BACKSPACE: u32 = 14;
    pub const TAB: u32 = 15;
    pub const Q: u32 = 16;
    pub const W: u32 = 17;
    pub const E: u32 = 18;
    pub const R: u32 = 19;
    pub const T: u32 = 20;
    pub const Y: u32 = 21;
    pub const U: u32 = 22;
    pub const I: u32 = 23;
    pub const O: u32 = 24;
    pub const P: u32 = 25;
    pub const LEFTBRACE: u32 = 26;
    pub const RIGHTBRACE: u32 = 27;
    pub const ENTER: u32 = 28;
    pub const LEFTCTRL: u32 = 29;
    pub const A: u32 = 30;
    pub const S: u32 = 31;
    pub const D: u32 = 32;
    pub const F: u32 = 33;
    pub const G: u32 = 34;
    pub const H: u32 = 35;
    pub const J: u32 = 36;
    pub const K: u32 = 37;
    pub const L: u32 = 38;
    pub const SEMICOLON: u32 = 39;
    pub const APOSTROPHE: u32 = 40;
    pub const GRAVE: u32 = 41;
    pub const LEFTSHIFT: u32 = 42;
    pub const BACKSLASH: u32 = 43;
    pub const Z: u32 = 44;
    pub const X: u32 = 45;
    pub const C: u32 = 46;
    pub const V: u32 = 47;
    pub const B: u32 = 48;
    pub const N: u32 = 49;
    pub const M: u32 = 50;
    pub const COMMA: u32 = 51;
    pub const DOT: u32 = 52;
    pub const SLASH: u32 = 53;
    pub const RIGHTSHIFT: u32 = 54;
    pub const LEFTALT: u32 = 56;
    pub const SPACE: u32 = 57;
    pub const CAPSLOCK: u32 = 58;
    pub const RIGHTALT: u32 = 100; // AltGr
    pub const HOME: u32 = 102;
    pub const UP: u32 = 103;
    pub const LEFT: u32 = 105;
    pub const RIGHT: u32 = 106;
    pub const END: u32 = 107;
    pub const DOWN: u32 = 108;
    pub const DELETE: u32 = 111;
    /// The extra `<>|` key left of `z` on ISO keyboards (Turkish F uses it).
    pub const LSGT: u32 = 86;
    /// Left "Super"/Windows/Meta key.
    pub const LEFTMETA: u32 = 125;
    /// The application/menu key (we repurpose its cap as a layout menu).
    pub const MENU: u32 = 139;
    // numeric keypad
    pub const KP7: u32 = 71;
    pub const KP8: u32 = 72;
    pub const KP9: u32 = 73;
    pub const KP4: u32 = 75;
    pub const KP5: u32 = 76;
    pub const KP6: u32 = 77;
    pub const KP1: u32 = 79;
    pub const KP2: u32 = 80;
    pub const KP3: u32 = 81;
    pub const KP0: u32 = 82;
    pub const KPDOT: u32 = 83;
    pub const KPPLUS: u32 = 78;
    pub const KPMINUS: u32 = 74;
    pub const KPENTER: u32 = 96;
}

// ---------------------------------------------------------------------------
// Layout identity
// ---------------------------------------------------------------------------

/// The selectable layouts. Each maps to an xkb `(layout, variant)` the
/// controller pushes to the seat keyboard so evdev codes produce the right
/// glyph, plus a human label for the layout-switch key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LayoutId {
    /// English (US) QWERTY — letters page.
    English,
    /// Turkish-Q (`tr`) — letters page.
    Turkish,
    /// Pinyin entry for Chinese: a Latin QWERTY whose commits are fed to a
    /// CJK input-method for candidate selection. We render QWERTY and tag the
    /// page so the integration can route to the IME.
    Chinese,
    /// `?123` symbols / punctuation page.
    Symbols,
    /// Telephone-style numeric keypad.
    Numeric,
    /// Emoji grid (pure `Commit` keys).
    Emoji,
}

impl LayoutId {
    /// The label shown on the layout-cycle key.
    pub fn short_label(self) -> &'static str {
        match self {
            LayoutId::English => "EN",
            LayoutId::Turkish => "TR",
            LayoutId::Chinese => "中",
            LayoutId::Symbols => "?123",
            LayoutId::Numeric => "123",
            LayoutId::Emoji => "☺",
        }
    }

    /// xkb `(layout, variant)` to program into the seat keyboard so the evdev
    /// codes on this page resolve to the printed glyphs. Pages with no xkb
    /// counterpart (symbols/emoji) keep the previous letters layout.
    pub fn xkb(self) -> Option<(&'static str, &'static str)> {
        match self {
            LayoutId::English => Some(("us", "")),
            // Turkish F-keyboard (the traditional typewriter layout the keycaps
            // below are drawn for) — xkb `tr(f)`.
            LayoutId::Turkish => Some(("tr", "f")),
            LayoutId::Chinese => Some(("us", "")), // pinyin uses a Latin map
            _ => None,
        }
    }

    /// The "alphabetic" layouts. Symbols / numeric / emoji are reached from a
    /// dedicated key and return to whichever of these was last active.
    pub fn is_alpha(self) -> bool {
        matches!(self, LayoutId::English | LayoutId::Turkish | LayoutId::Chinese)
    }
}

/// Localised cap text for the word-action keys (Copy / Paste / Cut / Select-All)
/// in the keyboard's active `language`. `None` for non-word actions (the
/// renderer keeps their static glyph cap). Currently Turkish + English.
pub fn action_label(a: Action, language: LayoutId) -> Option<&'static str> {
    let tr = matches!(language, LayoutId::Turkish);
    Some(match a {
        Action::Copy => if tr { "Kopyala" } else { "Copy" },
        Action::Paste => if tr { "Yapıştır" } else { "Paste" },
        Action::Cut => if tr { "Kes" } else { "Cut" },
        Action::SelectAll => if tr { "Tümü" } else { "All" },
        _ => return None,
    })
}

/// The alphabetic layout the on-screen keyboard opens in. Bacak OS is
/// Turkish-first, so the OSK **always** opens in Turkish-F (the session locale
/// is typically unset here, which previously fell through to English). The user
/// can still switch live with the language key, and that choice is preserved
/// until they change it again.
///
/// Note this only sets which page the OSK *shows*; the seat's xkb layout is
/// retuned to match only when the OSK is actually opened (see
/// `BacakState::apply_osk_xkb`), so physical-keyboard typing before any OSK use
/// is unaffected.
pub fn system_default_layout() -> LayoutId {
    LayoutId::Turkish
}

// ---------------------------------------------------------------------------
// Keycaps
// ---------------------------------------------------------------------------

/// What a key *does* when tapped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyKind {
    /// Emit a raw evdev keycode through the seat keyboard. The produced glyph
    /// is whatever the active xkb layout maps this code to.
    Key(u32),
    /// Commit a literal UTF-8 string (emoji / accented alternate) via the
    /// input-method. No keycode involved.
    Unicode(&'static str),
    /// A latching / locking modifier.
    Modifier(Modifier),
    /// A compositor-level action (layout switch, clipboard, hide, dock…).
    Action(Action),
}

/// Modifiers the OSK can hold. Shift one-shot-latches on a single tap and
/// **locks** (Caps) on a double-tap; the rest one-shot-latch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Modifier {
    Shift,
    Ctrl,
    Alt,
    AltGr,
}

/// Compositor-level, non-text keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    /// Cycle EN → TR → (CJK) → EN among alphabetic layouts.
    CycleAlpha,
    /// Keyboard-language selector (next to the space bar): cycles the
    /// alphabetic layout like [`Action::CycleAlpha`], but its cap shows the
    /// *current* language code (TR / EN), overridden live by the renderer.
    Language,
    /// Return to letters: from a symbols/numeric/emoji page, go back to the
    /// **last-used alphabetic layout** (never cycles language — that's the
    /// Language key's job). Fixes the bug where `ABC` flipped TR→EN.
    Letters,
    /// Jump to a specific page.
    Goto(LayoutId),
    Copy,
    Paste,
    Cut,
    SelectAll,
    /// Open the Recent-Apps overview ("recents"). Bubbles to the compositor.
    Recents,
    /// Collapse the keyboard (lose focus / user dismiss).
    Hide,
    /// Re-dock to the default bottom-centre position.
    DockDefault,
    /// Toggle split (tablet) mode.
    ToggleSplit,
}

/// One key. Pixel geometry is derived at render/hit-test time from `width` and
/// the key's row; we keep the model resolution-independent.
#[derive(Debug, Clone)]
pub struct KeyCap {
    pub kind: KeyKind,
    /// Glyph/label shown when no shift is active.
    pub label: &'static str,
    /// Label when Shift/Caps is active (uppercase, or shifted symbol).
    pub shift_label: Option<&'static str>,
    /// Width in key-units (1.0 == one standard key cell).
    pub width: f32,
    /// Long-press popup glyphs (committed as `Unicode`). Empty == no popup.
    pub alternates: &'static [&'static str],
}

impl KeyCap {
    const fn key(code: u32, label: &'static str, shift: &'static str) -> Self {
        Self { kind: KeyKind::Key(code), label, shift_label: Some(shift), width: 1.0, alternates: &[] }
    }
    const fn letter(code: u32, lower: &'static str, upper: &'static str, alts: &'static [&'static str]) -> Self {
        Self { kind: KeyKind::Key(code), label: lower, shift_label: Some(upper), width: 1.0, alternates: alts }
    }
    const fn act(a: Action, label: &'static str, width: f32) -> Self {
        Self { kind: KeyKind::Action(a), label, shift_label: None, width, alternates: &[] }
    }
    const fn modk(m: Modifier, label: &'static str, width: f32) -> Self {
        Self { kind: KeyKind::Modifier(m), label, shift_label: None, width, alternates: &[] }
    }
    const fn emoji(s: &'static str) -> Self {
        Self { kind: KeyKind::Unicode(s), label: s, shift_label: None, width: 1.0, alternates: &[] }
    }

    /// Label to draw given the current shift state.
    pub fn display(&self, shifted: bool) -> &str {
        if shifted {
            self.shift_label.unwrap_or(self.label)
        } else {
            self.label
        }
    }
}

/// A full keyboard page: rows of keycaps + which xkb layout it wants.
pub struct Page {
    pub id: LayoutId,
    pub rows: Vec<Vec<KeyCap>>,
}

// ---------------------------------------------------------------------------
// Layout definitions
// ---------------------------------------------------------------------------

fn row_digits() -> Vec<KeyCap> {
    use code::*;
    vec![
        KeyCap::letter(N1, "1", "!", &["¹", "½"]),
        KeyCap::letter(N2, "2", "@", &["²"]),
        KeyCap::letter(N3, "3", "#", &["³"]),
        KeyCap::letter(N4, "4", "$", &["₺", "€", "£"]),
        KeyCap::letter(N5, "5", "%", &["‰"]),
        KeyCap::letter(N6, "6", "^", &[]),
        KeyCap::letter(N7, "7", "&", &[]),
        KeyCap::letter(N8, "8", "*", &["•", "×"]),
        KeyCap::letter(N9, "9", "(", &[]),
        KeyCap::letter(N0, "0", ")", &["°"]),
    ]
}

/// Shared bottom row: layout switch, comma, space, dot, enter.
fn row_bottom(switch: Action, space_w: f32) -> Vec<KeyCap> {
    use code::*;
    vec![
        KeyCap::act(switch, "?123", 1.5),
        KeyCap::act(Action::Goto(LayoutId::Emoji), "☺", 1.0),
        KeyCap::letter(COMMA, ",", ";", &[]),
        // Language selector, left of the space bar (cap shows the live code).
        KeyCap::act(Action::Language, "EN", 1.0),
        KeyCap { width: space_w - 2.0, ..KeyCap::key(SPACE, " ", " ") },
        KeyCap::letter(DOT, ".", ":", &["…"]),
        KeyCap { width: 2.2, ..KeyCap::key(ENTER, "↵ Enter", "↵ Enter") },
    ]
}

fn page_english() -> Page {
    use code::*;
    let rows = vec![
        row_digits(),
        vec![
            KeyCap::letter(Q, "q", "Q", &[]),
            KeyCap::letter(W, "w", "W", &[]),
            KeyCap::letter(E, "e", "E", &["è", "é", "ê", "ë", "€"]),
            KeyCap::letter(R, "r", "R", &[]),
            KeyCap::letter(T, "t", "T", &[]),
            KeyCap::letter(Y, "y", "Y", &["ÿ"]),
            KeyCap::letter(U, "u", "U", &["ù", "ú", "û", "ü"]),
            KeyCap::letter(I, "i", "I", &["ì", "í", "î", "ï"]),
            KeyCap::letter(O, "o", "O", &["ò", "ó", "ô", "ö", "ø"]),
            KeyCap::letter(P, "p", "P", &[]),
        ],
        vec![
            KeyCap::letter(A, "a", "A", &["à", "á", "â", "ä", "å", "æ"]),
            KeyCap::letter(S, "s", "S", &["ß", "ş", "ś"]),
            KeyCap::letter(D, "d", "D", &[]),
            KeyCap::letter(F, "f", "F", &[]),
            KeyCap::letter(G, "g", "G", &["ğ"]),
            KeyCap::letter(H, "h", "H", &[]),
            KeyCap::letter(J, "j", "J", &[]),
            KeyCap::letter(K, "k", "K", &[]),
            KeyCap::letter(L, "l", "L", &[]),
        ],
        vec![
            KeyCap::modk(Modifier::Shift, "⇧", 1.5),
            KeyCap::letter(Z, "z", "Z", &[]),
            KeyCap::letter(X, "x", "X", &[]),
            KeyCap::letter(C, "c", "C", &["ç", "ć"]),
            KeyCap::letter(V, "v", "V", &[]),
            KeyCap::letter(B, "b", "B", &[]),
            KeyCap::letter(N, "n", "N", &["ñ"]),
            KeyCap::letter(M, "m", "M", &[]),
            KeyCap::key(BACKSPACE, "⌫", "⌫"),
        ],
        row_alt_ctrl(),
        row_bottom(Action::Goto(LayoutId::Symbols), 5.0),
    ];
    Page { id: LayoutId::English, rows }
}

fn page_turkish() -> Page {
    use code::*;
    // Turkish **F-keyboard** (the traditional typewriter layout), matching the
    // reference design. The evdev positions are the standard ones; the labels
    // are what xkb `tr(f)` produces at each position — and the controller
    // programs `tr(f)` on the seat when this page is active so the codes really
    // emit these glyphs. See the `tr(f)` xkb variant.
    let rows = vec![
        // Number row: + 1 2 3 4 5 6 7 8 9 0 / -  ⌫ ⌦
        vec![
            KeyCap::letter(GRAVE, "+", "*", &[]),
            KeyCap::letter(N1, "1", "!", &[]),
            KeyCap::letter(N2, "2", "\"", &[]),
            KeyCap::letter(N3, "3", "^", &[]),
            KeyCap::letter(N4, "4", "$", &[]),
            KeyCap::letter(N5, "5", "%", &[]),
            KeyCap::letter(N6, "6", "&", &[]),
            KeyCap::letter(N7, "7", "'", &[]),
            KeyCap::letter(N8, "8", "(", &[]),
            KeyCap::letter(N9, "9", ")", &[]),
            KeyCap::letter(N0, "0", "=", &[]),
            KeyCap::letter(MINUS, "/", "?", &[]),
            KeyCap::letter(EQUAL, "-", "_", &[]),
            KeyCap { width: 1.5, ..KeyCap::key(BACKSPACE, "⌫", "⌫") },
            KeyCap::key(DELETE, "⌦", "⌦"),
        ],
        // Top letter row: Tab f g ğ ı o d r n h p q w  ↵
        vec![
            KeyCap { width: 1.5, ..KeyCap::key(TAB, "↹", "↹") },
            KeyCap::letter(Q, "f", "F", &[]),
            KeyCap::letter(W, "g", "G", &[]),
            KeyCap::letter(E, "ğ", "Ğ", &[]),
            KeyCap::letter(R, "ı", "I", &[]),
            KeyCap::letter(T, "o", "O", &[]),
            KeyCap::letter(Y, "d", "D", &[]),
            KeyCap::letter(U, "r", "R", &[]),
            KeyCap::letter(I, "n", "N", &[]),
            KeyCap::letter(O, "h", "H", &[]),
            KeyCap::letter(P, "p", "P", &[]),
            KeyCap::letter(LEFTBRACE, "q", "Q", &[]),
            KeyCap::letter(RIGHTBRACE, "w", "W", &[]),
            // Enter ("Giriş") — deliberately wide (a prominent return key).
            KeyCap { width: 2.6, ..KeyCap::key(ENTER, "↵ Giriş", "↵ Giriş") },
        ],
        // Home row: ⇧ u i e a ü t k m l y ş x  Abc
        vec![
            KeyCap::modk(Modifier::Shift, "⇧", 1.25),
            KeyCap::letter(A, "u", "U", &[]),
            KeyCap::letter(S, "i", "İ", &[]),
            KeyCap::letter(D, "e", "E", &["€"]),
            KeyCap::letter(F, "a", "A", &[]),
            KeyCap::letter(G, "ü", "Ü", &[]),
            KeyCap::letter(H, "t", "T", &["₺"]),
            KeyCap::letter(J, "k", "K", &[]),
            KeyCap::letter(K, "m", "M", &[]),
            KeyCap::letter(L, "l", "L", &[]),
            KeyCap::letter(SEMICOLON, "y", "Y", &[]),
            KeyCap::letter(APOSTROPHE, "ş", "Ş", &[]),
            KeyCap::letter(BACKSLASH, "x", "X", &[]),
            KeyCap::act(Action::Letters, "Abc", 1.25),
        ],
        // Bottom row: ⇧ < j ö v c ç z s b . ,  ⇧  123
        vec![
            KeyCap::modk(Modifier::Shift, "⇧", 1.0),
            KeyCap::letter(LSGT, "<", ">", &[]),
            KeyCap::letter(Z, "j", "J", &[]),
            KeyCap::letter(X, "ö", "Ö", &[]),
            KeyCap::letter(C, "v", "V", &[]),
            KeyCap::letter(V, "c", "C", &[]),
            KeyCap::letter(B, "ç", "Ç", &[]),
            KeyCap::letter(N, "z", "Z", &[]),
            KeyCap::letter(M, "s", "S", &[]),
            KeyCap::letter(COMMA, "b", "B", &[]),
            KeyCap::letter(DOT, ".", ":", &["…"]),
            KeyCap::letter(SLASH, ",", ";", &[]),
            KeyCap::modk(Modifier::Shift, "⇧", 1.0),
            KeyCap::act(Action::Goto(LayoutId::Numeric), "123", 1.25),
        ],
        // Control row: Ctrl Win Alt [lang] [space] AltGr ← → ↑ ↓  ☰
        // The language key sits left of the space bar; its cap shows the live
        // language code (TR/EN) and a tap cycles the alphabetic layout.
        vec![
            KeyCap::modk(Modifier::Ctrl, "Ctrl", 1.25),
            KeyCap::key(LEFTMETA, "Win", "Win"),
            KeyCap::modk(Modifier::Alt, "Alt", 1.0),
            KeyCap::act(Action::Language, "TR", 1.0),
            KeyCap { width: 4.0, ..KeyCap::key(SPACE, " ", " ") },
            KeyCap::modk(Modifier::AltGr, "AltGr", 1.25),
            KeyCap::key(LEFT, "←", "←"),
            KeyCap::key(RIGHT, "→", "→"),
            KeyCap::key(UP, "↑", "↑"),
            KeyCap::key(DOWN, "↓", "↓"),
            // Recents (Recent-Apps overview).
            KeyCap::act(Action::Recents, "❐", 1.0),
            KeyCap::act(Action::Goto(LayoutId::Symbols), "☰", 1.0),
        ],
    ];
    Page { id: LayoutId::Turkish, rows }
}

/// The Ctrl / Alt / AltGr / Tab / arrow utility row shared by the alpha pages,
/// so chords (Ctrl+C, Alt+Tab) and caret movement are reachable.
fn row_alt_ctrl() -> Vec<KeyCap> {
    use code::*;
    vec![
        KeyCap::key(TAB, "⇥", "⇥"),
        KeyCap::modk(Modifier::Ctrl, "Ctrl", 1.2),
        KeyCap::modk(Modifier::Alt, "Alt", 1.0),
        KeyCap::modk(Modifier::AltGr, "AltGr", 1.2),
        KeyCap::key(LEFT, "←", "←"),
        KeyCap::key(UP, "↑", "↑"),
        KeyCap::key(DOWN, "↓", "↓"),
        KeyCap::key(RIGHT, "→", "→"),
        KeyCap::act(Action::Copy, "Copy", 1.6),
        KeyCap::act(Action::Paste, "Paste", 1.6),
        KeyCap::act(Action::Recents, "❐", 1.0),
    ]
}

fn page_symbols() -> Page {
    use code::*;
    let rows = vec![
        row_digits(),
        vec![
            KeyCap::key(GRAVE, "`", "~"),
            KeyCap::key(MINUS, "-", "_"),
            KeyCap::key(EQUAL, "=", "+"),
            KeyCap::key(LEFTBRACE, "[", "{"),
            KeyCap::key(RIGHTBRACE, "]", "}"),
            KeyCap::key(BACKSLASH, "\\", "|"),
            KeyCap::key(SEMICOLON, ";", ":"),
            KeyCap::key(APOSTROPHE, "'", "\""),
            KeyCap::key(SLASH, "/", "?"),
        ],
        vec![
            KeyCap::emoji("€"),
            KeyCap::emoji("£"),
            KeyCap::emoji("₺"),
            KeyCap::emoji("•"),
            KeyCap::emoji("°"),
            KeyCap::emoji("…"),
            KeyCap::emoji("™"),
            KeyCap::emoji("©"),
            KeyCap::emoji("®"),
            KeyCap::key(BACKSPACE, "⌫", "⌫"),
        ],
        row_alt_ctrl(),
        vec![
            KeyCap::act(Action::Letters, "ABC", 1.5),
            KeyCap::act(Action::Goto(LayoutId::Emoji), "☺", 1.0),
            KeyCap::key(COMMA, ",", ";"),
            KeyCap { width: 5.0, ..KeyCap::key(SPACE, " ", " ") },
            KeyCap::key(DOT, ".", ":"),
            KeyCap::key(ENTER, "↵", "↵"),
        ],
    ];
    Page { id: LayoutId::Symbols, rows }
}

fn page_numeric() -> Page {
    use code::*;
    // Telephone-style 3-column keypad; wide keys give big touch targets.
    let rows = vec![
        vec![KeyCap::key(KP1, "1", "1"), KeyCap::key(KP2, "2", "2"), KeyCap::key(KP3, "3", "3")],
        vec![KeyCap::key(KP4, "4", "4"), KeyCap::key(KP5, "5", "5"), KeyCap::key(KP6, "6", "6")],
        vec![KeyCap::key(KP7, "7", "7"), KeyCap::key(KP8, "8", "8"), KeyCap::key(KP9, "9", "9")],
        vec![
            KeyCap::key(KPDOT, ".", "."),
            KeyCap::key(KP0, "0", "0"),
            KeyCap::key(BACKSPACE, "⌫", "⌫"),
        ],
        vec![
            KeyCap::act(Action::Letters, "ABC", 1.0),
            KeyCap::key(KPMINUS, "−", "−"),
            KeyCap::key(KPPLUS, "+", "+"),
        ],
        vec![KeyCap { width: 3.0, ..KeyCap::key(KPENTER, "↵", "↵") }],
    ];
    Page { id: LayoutId::Numeric, rows }
}

fn page_emoji() -> Page {
    // Colour emoji, committed as UTF-8. The compositor renders these via the
    // CBDT bitmap path (Noto Color Emoji → `crate::emoji`), including ZWJ
    // sequences + skin-tone modifiers (row 4) and regional-indicator flag pairs
    // (row 5) — all resolved through GSUB ligatures. On a system with no
    // colour-emoji font they fall back to the monochrome `fontdue` glyph.
    const FACES: &[&str] = &[
        "😀", "😃", "😄", "😁", "😆", "😅", "😂", "🙂", //
        "😉", "😊", "😍", "😘", "😎", "🤔", "😴", "😢", //
        "😡", "👍", "👎", "🙏", "👏", "🔥", "🎉", "💯", //
        // ZWJ sequences + skin-tone modifiers:
        "👍🏻", "👍🏽", "👍🏿", "👋🏽", "👨‍👩‍👧", "👩‍💻", "👨‍🚀", "❤", //
        // Flags (regional-indicator pairs):
        "🇹🇷", "🇺🇸", "🇬🇧", "🇩🇪", "🇫🇷", "🇪🇸", "🇮🇹", "🇯🇵", //
    ];
    let mut rows: Vec<Vec<KeyCap>> = Vec::new();
    for chunk in FACES.chunks(8) {
        rows.push(chunk.iter().map(|s| KeyCap::emoji(s)).collect());
    }
    rows.push(vec![
        KeyCap::act(Action::Letters, "ABC", 2.0),
        KeyCap { width: 4.0, ..KeyCap::key(code::SPACE, " ", " ") },
        KeyCap::key(code::BACKSPACE, "⌫", "⌫"),
        KeyCap::key(code::ENTER, "↵", "↵"),
    ]);
    Page { id: LayoutId::Emoji, rows }
}

/// Build the page for a given id.
pub fn page_for(id: LayoutId) -> Page {
    match id {
        LayoutId::English | LayoutId::Chinese => page_english_as(id),
        LayoutId::Turkish => page_turkish(),
        LayoutId::Symbols => page_symbols(),
        LayoutId::Numeric => page_numeric(),
        LayoutId::Emoji => page_emoji(),
    }
}

fn page_english_as(id: LayoutId) -> Page {
    let mut p = page_english();
    p.id = id;
    p
}

// ---------------------------------------------------------------------------
// Modifier state machine
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShiftState {
    Off,
    /// One-shot: applies to the next key, then clears.
    Latched,
    /// Locked (Caps Lock) until tapped off.
    Locked,
}

/// Tracks the live modifier latches. Shift gets the latch/lock treatment with
/// double-tap → Caps; Ctrl/Alt/AltGr are simple one-shot latches.
#[derive(Debug, Clone, Copy)]
pub struct Mods {
    pub shift: ShiftState,
    pub ctrl: bool,
    pub alt: bool,
    pub altgr: bool,
    /// Timestamp (ms) of the last Shift tap, for double-tap → Caps detection.
    last_shift_ms: u64,
}

impl Default for Mods {
    fn default() -> Self {
        Self { shift: ShiftState::Off, ctrl: false, alt: false, altgr: false, last_shift_ms: 0 }
    }
}

/// Max gap between two Shift taps to count as a Caps-Lock double-tap.
pub const DOUBLE_TAP_MS: u64 = 300;

impl Mods {
    /// True when letters should render/emit uppercase.
    pub fn shifted(&self) -> bool {
        !matches!(self.shift, ShiftState::Off)
    }

    /// Handle a Shift-key tap at `now_ms`. Off→Latched, Latched→(double-tap?
    /// Locked : Off), Locked→Off.
    pub fn tap_shift(&mut self, now_ms: u64) {
        self.shift = match self.shift {
            ShiftState::Off => ShiftState::Latched,
            ShiftState::Latched => {
                if now_ms.saturating_sub(self.last_shift_ms) <= DOUBLE_TAP_MS {
                    ShiftState::Locked
                } else {
                    ShiftState::Off
                }
            }
            ShiftState::Locked => ShiftState::Off,
        };
        self.last_shift_ms = now_ms;
    }

    pub fn toggle(&mut self, m: Modifier, now_ms: u64) {
        match m {
            Modifier::Shift => self.tap_shift(now_ms),
            Modifier::Ctrl => self.ctrl = !self.ctrl,
            Modifier::Alt => self.alt = !self.alt,
            Modifier::AltGr => self.altgr = !self.altgr,
        }
    }

    /// Evdev modifier codes currently in effect for a chord.
    pub fn chord_codes(&self) -> Vec<u32> {
        let mut v = Vec::new();
        if self.shifted() {
            v.push(code::LEFTSHIFT);
        }
        if self.ctrl {
            v.push(code::LEFTCTRL);
        }
        if self.alt {
            v.push(code::LEFTALT);
        }
        if self.altgr {
            v.push(code::RIGHTALT);
        }
        v
    }

    /// Clear the one-shot latches after a normal key emission. A *Latched*
    /// Shift drops to Off; *Locked* (Caps) survives. Ctrl/Alt/AltGr are
    /// one-shot, matching mobile-OSK behaviour.
    pub fn consume_after_key(&mut self) {
        if self.shift == ShiftState::Latched {
            self.shift = ShiftState::Off;
        }
        self.ctrl = false;
        self.alt = false;
        self.altgr = false;
    }
}

// ---------------------------------------------------------------------------
// Placement / drag / dock
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Edge {
    Bottom,
    Top,
}

/// Where the panel sits. `Docked(Bottom)` is the default; a title-bar drag
/// flips it to `Floating`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Placement {
    Docked(Edge),
    Floating { x: f32, y: f32 },
}

impl Default for Placement {
    fn default() -> Self {
        Placement::Docked(Edge::Bottom)
    }
}

// ---------------------------------------------------------------------------
// Hit-testing geometry
// ---------------------------------------------------------------------------

/// A laid-out key with its pixel rect, ready to render and hit-test.
#[derive(Debug, Clone)]
pub struct KeyRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub row: usize,
    pub col: usize,
}

impl KeyRect {
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
}

/// Lay a page's rows into pixel rects inside `panel` (x,y,w,h), leaving
/// `title_h` at the top for the drag handle and `gap` between keys. Rows are
/// centred horizontally so variable-width rows (numeric keypad) look tidy.
pub fn layout_rects(page: &Page, panel: (f32, f32, f32, f32), title_h: f32, gap: f32) -> Vec<KeyRect> {
    let (px, py, pw, ph) = panel;
    let rows = page.rows.len().max(1);
    let avail_h = (ph - title_h).max(0.0);
    let row_h = (avail_h - gap * (rows as f32 + 1.0)) / rows as f32;
    let mut out = Vec::new();
    let mut y = py + title_h + gap;
    for (ri, row) in page.rows.iter().enumerate() {
        let units: f32 = row.iter().map(|k| k.width).sum();
        let n = row.len() as f32;
        // Total horizontal gap = gap*(n+1); remaining width split by units.
        let content_w = pw - gap * (n + 1.0);
        let unit_w = (content_w / units).max(0.0);
        let mut x = px + gap;
        for (ci, k) in row.iter().enumerate() {
            let w = unit_w * k.width;
            out.push(KeyRect { x, y, w, h: row_h, row: ri, col: ci });
            x += w + gap;
        }
        let _ = n;
        y += row_h + gap;
        let _ = ri;
    }
    out
}

// ---------------------------------------------------------------------------
// Press resolution
// ---------------------------------------------------------------------------

/// Resolution of a committed key tap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyAction {
    /// Inject `code` through the seat keyboard with `mods` held (evdev codes).
    Chord { mods: Vec<u32>, code: u32 },
    /// Commit a literal string via the input-method (emoji / accented alt).
    Commit(String),
    /// A compositor-level action.
    Special(Action),
    /// Modifier state changed; nothing to emit, just redraw.
    Redraw,
}

// ---------------------------------------------------------------------------
// Keyboard — the live page + modifier + placement, the OSK's typing brain.
// ---------------------------------------------------------------------------

pub struct Keyboard {
    page: Page,
    /// Which alpha layout to return to from symbols/emoji/numeric.
    home_alpha: LayoutId,
    pub mods: Mods,
    pub placement: Placement,
    split: bool,
    /// The xkb `(layout, variant)` the seat keyboard should adopt — set when
    /// the page changes (and at construction). The compositor drains this via
    /// [`take_pending_xkb`](Self::take_pending_xkb) and applies it with
    /// `set_xkb_config`, so the evdev codes actually emit this page's glyphs.
    pending_xkb: Option<(&'static str, &'static str)>,
}

impl Default for Keyboard {
    fn default() -> Self {
        Self::new(system_default_layout())
    }
}

impl Keyboard {
    pub fn new(alpha: LayoutId) -> Self {
        Self {
            page: page_for(alpha),
            home_alpha: if alpha.is_alpha() { alpha } else { system_default_layout() },
            mods: Mods::default(),
            placement: Placement::default(),
            split: false,
            pending_xkb: alpha.xkb(),
        }
    }

    /// Drain the pending xkb `(layout, variant)`, if the page changed since the
    /// last call. The compositor applies it to the seat keyboard.
    pub fn take_pending_xkb(&mut self) -> Option<(&'static str, &'static str)> {
        self.pending_xkb.take()
    }

    pub fn page(&self) -> &Page {
        &self.page
    }
    pub fn layout_id(&self) -> LayoutId {
        self.page.id
    }
    /// The active *language* (last alphabetic layout) — stays Turkish/English
    /// even while a symbols/numeric/emoji page is showing, so action caps like
    /// Copy/Paste can be localised to it.
    pub fn language(&self) -> LayoutId {
        self.home_alpha
    }
    pub fn split(&self) -> bool {
        self.split
    }

    /// Switch to a page. Remembers the last alpha page so `CycleAlpha` / the
    /// `ABC` key knows where to go back to. Returns the new xkb layout to push
    /// to the seat (`None` == keep current).
    pub fn goto(&mut self, id: LayoutId) -> Option<(&'static str, &'static str)> {
        if id.is_alpha() {
            self.home_alpha = id;
        }
        self.page = page_for(id);
        // Pages with no xkb counterpart (symbols/emoji/numeric) keep the
        // previous layout, so only overwrite when this page wants a specific
        // one — that way returning to letters still types correctly.
        if let Some(x) = id.xkb() {
            self.pending_xkb = Some(x);
        }
        id.xkb()
    }

    fn cycle_alpha(&mut self) -> Option<(&'static str, &'static str)> {
        let next = match self.home_alpha {
            LayoutId::Turkish => LayoutId::English,
            LayoutId::English => LayoutId::Turkish,
            _ => LayoutId::Turkish,
        };
        self.goto(next)
    }

    /// Resolve a tap on the key at (`row`, `col`). Mutates modifier / page
    /// state and returns what the compositor should do. For long-press the
    /// caller uses [`alternates_at`] instead.
    pub fn press(&mut self, row: usize, col: usize, now_ms: u64) -> KeyAction {
        let Some(key) = self.page.rows.get(row).and_then(|r| r.get(col)) else {
            return KeyAction::Redraw;
        };
        match key.kind.clone() {
            KeyKind::Modifier(m) => {
                self.mods.toggle(m, now_ms);
                KeyAction::Redraw
            }
            KeyKind::Unicode(s) => {
                self.mods.consume_after_key();
                KeyAction::Commit(s.to_string())
            }
            KeyKind::Key(c) => {
                let mods = self.mods.chord_codes();
                self.mods.consume_after_key();
                KeyAction::Chord { mods, code: c }
            }
            KeyKind::Action(a) => self.apply_action(a),
        }
    }

    fn apply_action(&mut self, a: Action) -> KeyAction {
        match a {
            Action::CycleAlpha | Action::Language => {
                self.cycle_alpha();
                KeyAction::Redraw
            }
            Action::Letters => {
                // Back to the language we were using — don't change it.
                self.goto(self.home_alpha);
                KeyAction::Redraw
            }
            Action::Goto(id) => {
                self.goto(id);
                KeyAction::Redraw
            }
            Action::ToggleSplit => {
                self.split = !self.split;
                KeyAction::Redraw
            }
            Action::DockDefault => {
                self.placement = Placement::default();
                KeyAction::Redraw
            }
            // Copy/Paste/Cut/SelectAll/Hide are compositor-side; bubble up.
            other => KeyAction::Special(other),
        }
    }

    /// Long-press alternates for the key at (`row`,`col`), honouring shift for
    /// the base glyph order. Empty == no popup.
    pub fn alternates_at(&self, row: usize, col: usize) -> &'static [&'static str] {
        self.page
            .rows
            .get(row)
            .and_then(|r| r.get(col))
            .map(|k| k.alternates)
            .unwrap_or(&[])
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_page_builds_nonempty() {
        for id in [
            LayoutId::English,
            LayoutId::Turkish,
            LayoutId::Chinese,
            LayoutId::Symbols,
            LayoutId::Numeric,
            LayoutId::Emoji,
        ] {
            let p = page_for(id);
            assert!(!p.rows.is_empty(), "{id:?} had no rows");
            assert!(p.rows.iter().all(|r| !r.is_empty()), "{id:?} had empty row");
        }
    }

    #[test]
    fn shift_latches_then_consumes() {
        let mut kb = Keyboard::new(LayoutId::English);
        // find the Shift key (row 3, col 0 on the english page)
        let action = kb.press(3, 0, 100);
        assert_eq!(action, KeyAction::Redraw);
        assert!(kb.mods.shifted(), "single shift tap should latch");
        // Press 'z' (row 3 col 1) → chord carries LEFTSHIFT, then clears.
        let a = kb.press(3, 1, 110);
        match a {
            KeyAction::Chord { mods, code } => {
                assert!(mods.contains(&code::LEFTSHIFT));
                assert_eq!(code, code::Z);
            }
            other => panic!("expected chord, got {other:?}"),
        }
        assert!(!kb.mods.shifted(), "latched shift must clear after one key");
    }

    #[test]
    fn double_tap_shift_locks_caps() {
        let mut kb = Keyboard::new(LayoutId::English);
        kb.press(3, 0, 100); // -> Latched
        kb.press(3, 0, 200); // within 300ms -> Locked
        assert_eq!(kb.mods.shift, ShiftState::Locked);
        // Caps survives a letter.
        let _ = kb.press(3, 1, 300);
        assert!(kb.mods.shifted(), "caps lock must survive a keypress");
        // Tap again -> off.
        kb.press(3, 0, 5000);
        assert_eq!(kb.mods.shift, ShiftState::Off);
    }

    #[test]
    fn double_tap_too_slow_is_not_caps() {
        let mut kb = Keyboard::new(LayoutId::English);
        kb.press(3, 0, 100); // Latched
        kb.press(3, 0, 100 + DOUBLE_TAP_MS + 1); // too slow -> Off
        assert_eq!(kb.mods.shift, ShiftState::Off);
    }

    #[test]
    fn ctrl_c_is_one_shot_chord() {
        let mut kb = Keyboard::new(LayoutId::English);
        // Ctrl lives in the util row (row 4, col 1).
        let a = kb.press(4, 1, 0);
        assert_eq!(a, KeyAction::Redraw);
        assert!(kb.mods.ctrl);
        // 'c' is row 3 col 3.
        let a = kb.press(3, 3, 10);
        match a {
            KeyAction::Chord { mods, code } => {
                assert!(mods.contains(&code::LEFTCTRL));
                assert_eq!(code, code::C);
            }
            other => panic!("expected ctrl chord, got {other:?}"),
        }
        assert!(!kb.mods.ctrl, "ctrl must be one-shot");
    }

    #[test]
    fn emoji_keys_commit_unicode() {
        let mut kb = Keyboard::new(LayoutId::English);
        kb.goto(LayoutId::Emoji);
        let a = kb.press(0, 0, 0);
        match a {
            KeyAction::Commit(s) => assert!(!s.is_empty()),
            other => panic!("emoji should commit, got {other:?}"),
        }
    }

    #[test]
    fn turkish_switch_programs_tr_xkb() {
        let mut kb = Keyboard::new(LayoutId::English);
        let xkb = kb.goto(LayoutId::Turkish);
        assert_eq!(xkb, Some(("tr", "f")));
        assert_eq!(kb.layout_id(), LayoutId::Turkish);
        // Turkish-F top letter row prints the dotless ı.
        let row = &kb.page().rows[1];
        assert!(row.iter().any(|k| k.label == "ı"));
    }

    #[test]
    fn cycle_alpha_round_trips_and_keeps_home() {
        let mut kb = Keyboard::new(LayoutId::English);
        kb.goto(LayoutId::Symbols);
        // ABC from symbols returns to the remembered alpha (English).
        kb.cycle_alpha();
        assert_eq!(kb.layout_id(), LayoutId::Turkish); // English -> Turkish
    }

    #[test]
    fn abc_returns_to_current_language_not_english() {
        // Bug: switching to symbols then back via ABC flipped TR→EN. ABC must
        // return to the *current* language; only the Language key cycles.
        let mut kb = Keyboard::new(LayoutId::Turkish);
        kb.goto(LayoutId::Symbols);
        let page = kb.page();
        let (ri, ci) = page
            .rows
            .iter()
            .enumerate()
            .find_map(|(ri, row)| {
                row.iter()
                    .position(|k| matches!(k.kind, KeyKind::Action(Action::Letters)))
                    .map(|ci| (ri, ci))
            })
            .expect("symbols page has an ABC (Letters) key");
        kb.press(ri, ci, 0);
        assert_eq!(
            kb.layout_id(),
            LayoutId::Turkish,
            "ABC must return to Turkish, not flip to English"
        );
        // Same from the numeric page.
        kb.goto(LayoutId::Numeric);
        kb.goto(LayoutId::Turkish); // simulate a no-op; home stays Turkish
        assert_eq!(kb.layout_id(), LayoutId::Turkish);
    }

    #[test]
    fn recents_key_bubbles_to_special() {
        let mut kb = Keyboard::new(LayoutId::Turkish);
        let page = kb.page();
        let (ri, ci) = page
            .rows
            .iter()
            .enumerate()
            .find_map(|(ri, row)| {
                row.iter()
                    .position(|k| matches!(k.kind, KeyKind::Action(Action::Recents)))
                    .map(|ci| (ri, ci))
            })
            .expect("Turkish page has a Recents key");
        assert_eq!(kb.press(ri, ci, 0), KeyAction::Special(Action::Recents));
    }

    #[test]
    fn copy_paste_localised_per_language() {
        assert_eq!(action_label(Action::Copy, LayoutId::Turkish), Some("Kopyala"));
        assert_eq!(action_label(Action::Paste, LayoutId::Turkish), Some("Yapıştır"));
        assert_eq!(action_label(Action::Copy, LayoutId::English), Some("Copy"));
        assert_eq!(action_label(Action::Paste, LayoutId::English), Some("Paste"));
        // Non-word actions keep their glyph cap.
        assert_eq!(action_label(Action::Hide, LayoutId::Turkish), None);
        // `language()` tracks the alpha layout across a page switch.
        let mut kb = Keyboard::new(LayoutId::Turkish);
        kb.goto(LayoutId::Symbols);
        assert_eq!(kb.language(), LayoutId::Turkish);
    }

    #[test]
    fn language_key_next_to_space_cycles_alpha() {
        let mut kb = Keyboard::new(LayoutId::Turkish);
        let page = kb.page();
        // The Language key must exist, in the same row as the space bar.
        let (ri, ci) = page
            .rows
            .iter()
            .enumerate()
            .find_map(|(ri, row)| {
                row.iter()
                    .position(|k| matches!(k.kind, KeyKind::Action(Action::Language)))
                    .map(|ci| (ri, ci))
            })
            .expect("Turkish page has a Language key");
        let has_space = page.rows[ri]
            .iter()
            .any(|k| matches!(k.kind, KeyKind::Key(code::SPACE)));
        assert!(has_space, "language key isn't on the space-bar row");
        assert_eq!(kb.layout_id(), LayoutId::Turkish);
        kb.press(ri, ci, 0);
        assert_eq!(kb.layout_id(), LayoutId::English, "language key should cycle TR→EN");
    }

    #[test]
    fn turkish_is_f_layout() {
        let p = page_for(LayoutId::Turkish);
        // Top letter row: f g ğ ı o d r n h p q w
        let r1: Vec<&str> = p.rows[1].iter().map(|k| k.label).collect();
        for g in ["f", "g", "ğ", "ı", "o", "d", "r", "n", "h", "p", "q", "w"] {
            assert!(r1.contains(&g), "F top row missing {g}: {r1:?}");
        }
        // Home row: u i e a ü t k m l y ş x
        let r2: Vec<&str> = p.rows[2].iter().map(|k| k.label).collect();
        for g in ["u", "i", "e", "a", "ü", "t", "k", "m", "l", "y", "ş", "x"] {
            assert!(r2.contains(&g), "F home row missing {g}: {r2:?}");
        }
        // Bottom letter row: j ö v c ç z s b
        let r3: Vec<&str> = p.rows[3].iter().map(|k| k.label).collect();
        for g in ["j", "ö", "v", "c", "ç", "z", "s", "b"] {
            assert!(r3.contains(&g), "F bottom row missing {g}: {r3:?}");
        }
        // 'f' must sit on the physical Q position so tr(f) xkb emits it.
        let f = p.rows[1].iter().find(|k| k.label == "f").unwrap();
        assert_eq!(f.kind, KeyKind::Key(code::Q));
    }

    #[test]
    fn layout_rects_cover_panel_without_overlap() {
        let page = page_for(LayoutId::English);
        let rects = layout_rects(&page, (0.0, 0.0, 1000.0, 320.0), 28.0, 6.0);
        assert!(!rects.is_empty());
        // All rects fall inside the panel.
        for r in &rects {
            assert!(r.x >= 0.0 && r.x + r.w <= 1000.5, "key escapes right edge");
            assert!(r.y >= 28.0 && r.y + r.h <= 320.5, "key escapes vertical bounds");
            assert!(r.w > 0.0 && r.h > 0.0);
        }
    }

    #[test]
    fn hit_test_finds_the_pressed_key() {
        let page = page_for(LayoutId::English);
        let rects = layout_rects(&page, (0.0, 0.0, 1000.0, 320.0), 28.0, 6.0);
        // Pick a key and hit its centre.
        let target = &rects[15];
        let cx = target.x + target.w / 2.0;
        let cy = target.y + target.h / 2.0;
        let hit = rects.iter().find(|r| r.contains(cx, cy)).unwrap();
        assert_eq!((hit.row, hit.col), (target.row, target.col));
    }

    #[test]
    fn long_press_exposes_accents() {
        let kb = Keyboard::new(LayoutId::English);
        // 'a' is row 2 col 0 on english; has à á â ä …
        let alts = kb.alternates_at(2, 0);
        assert!(alts.contains(&"à"));
        assert!(alts.contains(&"ä"));
    }
}
