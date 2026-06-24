//! Tier C selection bridge — AT-SPI2 accessibility over D-Bus.
//!
//! Tiers A and B can't select text *accurately* inside a foreign client:
//! Wayland never exposes a client's glyph layout. AT-SPI2 is the one portable
//! way to get it. Apps (GTK/Qt/Chromium/Firefox/LibreOffice) publish their
//! text as accessible objects on a dedicated **accessibility bus**; the
//! `org.a11y.atspi.Text` interface then gives us offset↔point mapping, word/
//! line boundaries, `SetSelection`, the text itself, and range geometry — the
//! pieces needed to drive real Android-style selection over an arbitrary app.
//!
//! Two hard truths drive the design:
//!
//! 1. **The AT handshake.** Apps only build their accessibility tree once an
//!    assistive technology announces itself: `org.a11y.Status.IsEnabled` must
//!    be `true`. We set it on startup — that's what makes any of this work.
//! 2. **Wayland has no global coordinates.** `GetRangeExtents` in SCREEN coords
//!    is meaningless for a Wayland-native client (it doesn't know where it is).
//!    The fix (next slice) is to request WINDOW coords and add the on-screen
//!    position *we* assigned the window. X11/XWayland SCREEN coords work as-is.
//!
//! This module is the foundation: a side thread owns a blocking zbus connection
//! to the a11y bus, flips the AT handshake on, and tracks the focused text
//! accessible from `state-changed:focused` events. The proxy methods for the
//! selection ops are defined here; wiring them into the gesture pipeline is the
//! next slice.
#![cfg(feature = "runtime")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use zbus::blocking::{Connection, MessageIterator};
use zbus::zvariant::{OwnedObjectPath, OwnedValue};
use zbus::{proxy, MatchRule};

/// AT-SPI coordinate spaces (`ATSPI_COORD_TYPE_*`).
pub const COORD_SCREEN: u32 = 0;
pub const COORD_WINDOW: u32 = 1;
/// AT-SPI text granularities (`ATSPI_TEXT_GRANULARITY_*`).
pub const GRANULARITY_WORD: u32 = 1;
pub const GRANULARITY_LINE: u32 = 3;

/// `org.a11y.Bus` on the **session** bus — hands out the a11y bus address.
#[proxy(
    interface = "org.a11y.Bus",
    default_service = "org.a11y.Bus",
    default_path = "/org/a11y/bus",
    gen_async = false,
    gen_blocking = true
)]
trait A11yBus {
    fn get_address(&self) -> zbus::Result<String>;
}

/// `org.a11y.Status` — the AT handshake. Setting `IsEnabled` (and, for good
/// measure, `ScreenReaderEnabled`) is what makes apps populate their trees.
#[proxy(
    interface = "org.a11y.Status",
    default_service = "org.a11y.Bus",
    default_path = "/org/a11y/bus",
    gen_async = false,
    gen_blocking = true
)]
trait A11yStatus {
    #[zbus(property)]
    fn is_enabled(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn set_is_enabled(&self, value: bool) -> zbus::Result<()>;
    #[zbus(property)]
    fn set_screen_reader_enabled(&self, value: bool) -> zbus::Result<()>;
}

/// `org.a11y.atspi.Text` — per-object (destination + path set at build time).
/// The selection engine for foreign apps.
#[proxy(interface = "org.a11y.atspi.Text", assume_defaults = false, gen_async = false, gen_blocking = true)]
pub trait AtspiText {
    /// Character offset under a point (`coord_type` = [`COORD_SCREEN`]/[`COORD_WINDOW`]).
    fn get_offset_at_point(&self, x: i32, y: i32, coord_type: u32) -> zbus::Result<i32>;
    /// `(text, start, end)` of the word/line/… (`granularity`) around `offset`.
    fn get_string_at_offset(&self, offset: i32, granularity: u32) -> zbus::Result<(String, i32, i32)>;
    /// The substring `[start, end)`.
    fn get_text(&self, start: i32, end: i32) -> zbus::Result<String>;
    /// Replace selection `selection_num` with `[start, end)`.
    fn set_selection(&self, selection_num: i32, start: i32, end: i32) -> zbus::Result<bool>;
    fn add_selection(&self, start: i32, end: i32) -> zbus::Result<bool>;
    fn get_n_selections(&self) -> zbus::Result<i32>;
    fn get_selection(&self, selection_num: i32) -> zbus::Result<(i32, i32)>;
    /// Bounding box `(x, y, w, h)` of `[start, end)` in `coord_type` space.
    fn get_range_extents(&self, start: i32, end: i32, coord_type: u32) -> zbus::Result<(i32, i32, i32, i32)>;
    #[zbus(property)]
    fn caret_offset(&self) -> zbus::Result<i32>;
    #[zbus(property)]
    fn character_count(&self) -> zbus::Result<i32>;
}

/// `org.a11y.atspi.EditableText` — direct text mutation, so Paste can insert at
/// the caret without synthesising Ctrl+V (which depends on focus + the app's
/// keybinding). Per-object.
#[proxy(interface = "org.a11y.atspi.EditableText", assume_defaults = false, gen_async = false, gen_blocking = true)]
pub trait AtspiEditableText {
    fn insert_text(&self, position: i32, text: &str, length: i32) -> zbus::Result<bool>;
}

/// `org.a11y.atspi.Component` — window/widget geometry for coordinate mapping.
#[proxy(interface = "org.a11y.atspi.Component", assume_defaults = false, gen_async = false, gen_blocking = true)]
pub trait AtspiComponent {
    fn get_extents(&self, coord_type: u32) -> zbus::Result<(i32, i32, i32, i32)>;
}

/// `org.a11y.atspi.Registry` — the event broker on the a11y bus. Apps only
/// emit an event type once it's registered here, so without `RegisterEvent`
/// `GetRegisteredEvents` is empty and *no* focus/selection events ever arrive.
#[proxy(
    interface = "org.a11y.atspi.Registry",
    default_service = "org.a11y.atspi.Registry",
    default_path = "/org/a11y/atspi/registry",
    gen_async = false,
    gen_blocking = true
)]
pub trait AtspiRegistry {
    fn register_event(&self, event: &str, properties: &[&str], app_bus_name: &str) -> zbus::Result<()>;
    fn get_registered_events(&self) -> zbus::Result<Vec<(String, String)>>;
}

/// `org.a11y.atspi.Accessible` — tree walking (used by the live probe to locate
/// a text object without waiting for a focus event).
#[proxy(interface = "org.a11y.atspi.Accessible", assume_defaults = false, gen_async = false, gen_blocking = true)]
pub trait AtspiAccessible {
    fn get_children(&self) -> zbus::Result<Vec<(String, OwnedObjectPath)>>;
    fn get_interfaces(&self) -> zbus::Result<Vec<String>>;
}

/// A reference to an accessible object: its owning bus name + object path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccRef {
    pub bus: String,
    pub path: OwnedObjectPath,
}

/// Live bridge to the accessibility bus. Holds the connection (for issuing
/// selection ops) plus the focused-text accessible tracked by the event thread.
pub struct AtspiBridge {
    a11y: Connection,
    /// The currently focused **text** accessible, or `None`.
    focused: Arc<Mutex<Option<AccRef>>>,
    /// Set by the event thread when the focused accessible's selection changes
    /// (the app's own selection moved), so the main loop re-reads it and keeps
    /// our overlay in sync. Polled + cleared via [`take_selection_dirty`].
    ///
    /// [`take_selection_dirty`]: AtspiBridge::take_selection_dirty
    sel_dirty: Arc<AtomicBool>,
}

impl AtspiBridge {
    /// Connect to the a11y bus, flip the AT handshake on so apps expose their
    /// text, and spawn the focus-tracking thread. `None` if no session/a11y bus
    /// is reachable (the compositor then simply runs without Tier C).
    pub fn start() -> Option<Self> {
        let session = match Connection::session() {
            Ok(c) => c,
            Err(err) => {
                tracing::info!(?err, "atspi: no session bus; Tier C disabled");
                return None;
            }
        };

        // The handshake: announce ourselves as an enabled AT.
        match A11yStatusProxy::new(&session) {
            Ok(status) => {
                if let Err(err) = status.set_is_enabled(true) {
                    tracing::warn!(?err, "atspi: could not set IsEnabled");
                }
                let _ = status.set_screen_reader_enabled(true);
            }
            Err(err) => tracing::warn!(?err, "atspi: org.a11y.Status unavailable"),
        }

        let addr = A11yBusProxy::new(&session).ok()?.get_address().ok()?;
        let a11y = zbus::blocking::connection::Builder::address(addr.as_str())
            .ok()?
            .build()
            .ok()?;
        // Register the event types we consume, or apps never emit them (the
        // registry's GetRegisteredEvents would stay empty). "" = all apps.
        match AtspiRegistryProxy::new(&a11y) {
            Ok(reg) => {
                for ev in [
                    "object:state-changed:focused",
                    "object:text-selection-changed",
                    "object:text-caret-moved",
                ] {
                    if let Err(err) = reg.register_event(ev, &[], "") {
                        tracing::warn!(?err, ev, "atspi: RegisterEvent failed");
                    }
                }
            }
            Err(err) => tracing::warn!(?err, "atspi: registry unavailable; events disabled"),
        }
        tracing::info!("atspi: connected to accessibility bus; Tier C ready");

        let focused = Arc::new(Mutex::new(None));
        let sel_dirty = Arc::new(AtomicBool::new(false));
        // Detached threads: each blocks on its signal stream for the life of the
        // session; there's nothing to join.
        {
            let (conn, focused) = (a11y.clone(), focused.clone());
            thread::Builder::new()
                .name("bacak-atspi-focus".into())
                .spawn(move || focus_event_loop(conn, focused))
                .ok()?;
        }
        {
            let (conn, focused, dirty) = (a11y.clone(), focused.clone(), sel_dirty.clone());
            thread::Builder::new()
                .name("bacak-atspi-sel".into())
                .spawn(move || selection_event_loop(conn, focused, dirty))
                .ok()?;
        }

        Some(Self { a11y, focused, sel_dirty })
    }

    /// The currently focused text accessible, if any.
    pub fn focused(&self) -> Option<AccRef> {
        self.focused.lock().unwrap().clone()
    }

    /// The events currently registered with the registry (debug/probe).
    pub fn registered_events(&self) -> Vec<(String, String)> {
        AtspiRegistryProxy::new(&self.a11y)
            .and_then(|r| r.get_registered_events())
            .unwrap_or_default()
    }

    /// Take (and clear) the "focused selection changed" flag — the main loop
    /// polls this each tick to resync the overlay with the app's selection.
    pub fn take_selection_dirty(&self) -> bool {
        self.sel_dirty.swap(false, Ordering::Relaxed)
    }

    /// A `Text` proxy bound to `acc`, for issuing selection ops.
    pub fn text(&self, acc: &AccRef) -> Option<AtspiTextProxy<'static>> {
        AtspiTextProxy::builder(&self.a11y)
            .destination(acc.bus.clone())
            .ok()?
            .path(acc.path.clone())
            .ok()?
            .build()
            .ok()
    }

    /// An `EditableText` proxy bound to `acc`, for caret-insert Paste.
    pub fn editable(&self, acc: &AccRef) -> Option<AtspiEditableTextProxy<'static>> {
        AtspiEditableTextProxy::builder(&self.a11y)
            .destination(acc.bus.clone())
            .ok()?
            .path(acc.path.clone())
            .ok()?
            .build()
            .ok()
    }

    /// A `Component` proxy bound to `acc`, for geometry / coordinate mapping.
    pub fn component(&self, acc: &AccRef) -> Option<AtspiComponentProxy<'static>> {
        AtspiComponentProxy::builder(&self.a11y)
            .destination(acc.bus.clone())
            .ok()?
            .path(acc.path.clone())
            .ok()?
            .build()
            .ok()
    }

    /// An `Accessible` proxy bound to `acc` (tree walking).
    pub fn accessible(&self, acc: &AccRef) -> Option<AtspiAccessibleProxy<'static>> {
        AtspiAccessibleProxy::builder(&self.a11y)
            .destination(acc.bus.clone())
            .ok()?
            .path(acc.path.clone())
            .ok()?
            .build()
            .ok()
    }

    /// The deepest accessible implementing `org.a11y.atspi.Text` whose WINDOW
    /// extents contain `(x, y)` (window-relative px). This is how Tier C finds
    /// "the text under the finger" without depending on focus events (which
    /// many apps don't emit) and without picking the wrong object (e.g. the
    /// window-title text, which a plain first-match would grab). BFS keeps the
    /// last (deepest) hit; bounded so a huge tree can't hang us.
    pub fn find_text_at(&self, x: i32, y: i32) -> Option<AccRef> {
        let root = AccRef {
            bus: "org.a11y.atspi.Registry".to_string(),
            path: OwnedObjectPath::try_from("/org/a11y/atspi/accessible/root").ok()?,
        };
        let mut queue = std::collections::VecDeque::from([root]);
        let mut budget = 4000;
        let mut best: Option<AccRef> = None;
        while let Some(acc) = queue.pop_front() {
            budget -= 1;
            if budget == 0 {
                break;
            }
            let Some(a) = self.accessible(&acc) else { continue };
            let is_text = a
                .get_interfaces()
                .map(|ifs| ifs.iter().any(|i| i == "org.a11y.atspi.Text"))
                .unwrap_or(false);
            if is_text {
                if let Some((ex, ey, ew, eh)) =
                    self.component(&acc).and_then(|c| c.get_extents(COORD_WINDOW).ok())
                {
                    if x >= ex && x < ex + ew && y >= ey && y < ey + eh {
                        best = Some(acc.clone());
                    }
                }
            }
            if let Ok(children) = a.get_children() {
                for (bus, path) in children {
                    queue.push_back(AccRef { bus, path });
                }
            }
        }
        best
    }

    /// Breadth-first walk from the registry root to the first accessible that
    /// implements `org.a11y.atspi.Text`. A fallback for the live probe when no
    /// focus event has fired yet; bounded so a huge tree can't hang us.
    pub fn find_first_text(&self) -> Option<AccRef> {
        let root = AccRef {
            bus: "org.a11y.atspi.Registry".to_string(),
            path: OwnedObjectPath::try_from("/org/a11y/atspi/accessible/root").ok()?,
        };
        let mut queue = std::collections::VecDeque::from([root]);
        let mut budget = 4000;
        while let Some(acc) = queue.pop_front() {
            budget -= 1;
            if budget == 0 {
                break;
            }
            let Some(a) = self.accessible(&acc) else { continue };
            if a
                .get_interfaces()
                .map(|ifs| ifs.iter().any(|i| i == "org.a11y.atspi.Text"))
                .unwrap_or(false)
            {
                return Some(acc);
            }
            if let Ok(children) = a.get_children() {
                for (bus, path) in children {
                    queue.push_back(AccRef { bus, path });
                }
            }
        }
        None
    }

    /// `(caret_offset, character_count)` of the focused text — a cheap probe to
    /// confirm the tree is reachable (used for logging / verification).
    pub fn focused_text_snapshot(&self) -> Option<(i32, i32)> {
        let acc = self.focused()?;
        let text = self.text(&acc)?;
        Some((text.caret_offset().ok()?, text.character_count().ok()?))
    }

    /// Per-visual-line bounding boxes `(x, y, w, h)` (in `coord` space) covering
    /// the range `[start, end)`. `GetRangeExtents` only ever returns one union
    /// box, which is wrong across line breaks — so we walk line by line
    /// (`GetStringAtOffset` with LINE granularity) and clamp each line to the
    /// selection. Empty if the accessible vanished. The guard caps pathological
    /// loops (e.g. an app returning non-advancing line bounds).
    pub fn range_line_boxes(
        &self,
        acc: &AccRef,
        start: i32,
        end: i32,
        coord: u32,
    ) -> Vec<(i32, i32, i32, i32)> {
        let Some(text) = self.text(acc) else { return Vec::new() };
        let mut boxes = Vec::new();
        let mut off = start;
        let mut guard = 0;
        while off < end && guard < 4096 {
            guard += 1;
            let Ok((_t, lstart, lend)) = text.get_string_at_offset(off, GRANULARITY_LINE) else {
                break;
            };
            let seg_start = off.max(lstart);
            let seg_end = end.min(lend);
            if seg_end > seg_start {
                if let Ok((x, y, w, h)) = text.get_range_extents(seg_start, seg_end, coord) {
                    if w > 0 && h > 0 {
                        boxes.push((x, y, w, h));
                    }
                }
            }
            off = if lend > off { lend } else { off + 1 };
        }
        boxes
    }
}

/// Block on `org.a11y.atspi.Event.Object` `StateChanged` signals and keep
/// [`AtspiBridge::focused`] pointing at the focused **text** accessible. We
/// confirm an object is text by reading its `CharacterCount` (cheap, and it
/// filters out buttons/containers that also take focus).
fn focus_event_loop(conn: Connection, focused: Arc<Mutex<Option<AccRef>>>) {
    let rule = match MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface("org.a11y.atspi.Event.Object")
        .and_then(|b| b.member("StateChanged"))
    {
        Ok(b) => b.build(),
        Err(err) => {
            tracing::warn!(?err, "atspi: bad match rule");
            return;
        }
    };
    let iter = match MessageIterator::for_match_rule(rule, &conn, None) {
        Ok(it) => it,
        Err(err) => {
            tracing::warn!(?err, "atspi: cannot subscribe to focus events");
            return;
        }
    };

    for msg in iter.flatten() {
        // Event.Object body: (minor: s, detail1: i, detail2: i, any: v, source: (so)).
        let body = msg.body();
        let Ok((minor, detail1, _detail2, _any, (bus, path))) =
            body.deserialize::<(String, i32, i32, OwnedValue, (String, OwnedObjectPath))>()
        else {
            continue;
        };
        if minor != "focused" {
            continue;
        }
        if detail1 != 1 {
            // Focus left this object; only clear if it was the one we tracked.
            let mut g = focused.lock().unwrap();
            if g.as_ref().is_some_and(|a| a.path == path && a.bus == bus) {
                *g = None;
            }
            continue;
        }
        let acc = AccRef { bus, path };
        // Keep only text accessibles: a CharacterCount read succeeds for those.
        let is_text = AtspiTextProxy::builder(&conn)
            .destination(acc.bus.clone())
            .and_then(|b| b.path(acc.path.clone()))
            .ok()
            .and_then(|b| b.build().ok())
            .and_then(|t| t.character_count().ok())
            .is_some();
        if is_text {
            tracing::info!(bus = %acc.bus, path = %acc.path.as_str(), "atspi: focused text");
            *focused.lock().unwrap() = Some(acc);
        }
    }
}

/// Block on `TextSelectionChanged` signals; when the **focused** accessible's
/// selection changes (the app moved it — natively, or as our `SetSelection`
/// settles), raise the dirty flag so the main loop resyncs our overlay. We
/// only flag for the focused object to avoid waking on unrelated apps.
fn selection_event_loop(
    conn: Connection,
    focused: Arc<Mutex<Option<AccRef>>>,
    dirty: Arc<AtomicBool>,
) {
    let rule = match MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface("org.a11y.atspi.Event.Object")
        .and_then(|b| b.member("TextSelectionChanged"))
    {
        Ok(b) => b.build(),
        Err(err) => {
            tracing::warn!(?err, "atspi: bad selection match rule");
            return;
        }
    };
    let iter = match MessageIterator::for_match_rule(rule, &conn, None) {
        Ok(it) => it,
        Err(err) => {
            tracing::warn!(?err, "atspi: cannot subscribe to selection events");
            return;
        }
    };

    for msg in iter.flatten() {
        let body = msg.body();
        let Ok((_minor, _d1, _d2, _any, (bus, path))) =
            body.deserialize::<(String, i32, i32, OwnedValue, (String, OwnedObjectPath))>()
        else {
            continue;
        };
        let is_focused = focused
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|a| a.bus == bus && a.path == path);
        if is_focused {
            dirty.store(true, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The connection path: session bus → a11y address → a11y bus. Skips when
    /// no bus is present (CI / headless). Does **not** flip the global AT
    /// handshake, so it has no session-wide side effects.
    #[test]
    fn connects_to_a11y_bus_when_present() {
        let Ok(session) = Connection::session() else {
            eprintln!("skipping: no session bus");
            return;
        };
        let Ok(bus) = A11yBusProxy::new(&session) else {
            eprintln!("skipping: no org.a11y.Bus");
            return;
        };
        let Ok(addr) = bus.get_address() else {
            eprintln!("skipping: GetAddress failed");
            return;
        };
        assert!(addr.starts_with("unix:"), "a11y bus address looks valid: {addr}");
        let conn = zbus::blocking::connection::Builder::address(addr.as_str())
            .and_then(|b| b.build());
        assert!(conn.is_ok(), "can open the accessibility bus connection");
    }

    /// Live end-to-end probe of the Text read path against a **real running
    /// app**. Ignored by default — it flips the global AT handshake on and
    /// needs a focused/visible text widget. Run it on the device *after*
    /// launching a GUI app (e.g. a terminal/editor) under the compositor:
    ///   cargo test -p bacak-compositor --features udev -- --ignored --nocapture live_probe_text
    /// Dumps exactly what Tier C relies on (offsets, word boundaries, range
    /// extents in both coord spaces) so we can see whether an app's geometry is
    /// usable and which coordinate space it speaks.
    #[test]
    #[ignore]
    fn live_probe_text() {
        let Some(bridge) = AtspiBridge::start() else {
            eprintln!("no a11y bus — nothing to probe");
            return;
        };
        std::thread::sleep(std::time::Duration::from_millis(300));
        eprintln!("registered events after RegisterEvent: {:?}", bridge.registered_events());
        // Give focus events a moment; fall back to a tree walk.
        std::thread::sleep(std::time::Duration::from_millis(800));
        let Some(acc) = bridge.focused().or_else(|| bridge.find_first_text()) else {
            eprintln!("no Text accessible found — launch a GUI app with a text field first");
            return;
        };
        eprintln!("Text accessible: {} {}", acc.bus, acc.path.as_str());
        let Some(text) = bridge.text(&acc) else {
            eprintln!("could not bind Text proxy");
            return;
        };
        eprintln!("  character_count = {:?}", text.character_count());
        eprintln!("  caret_offset    = {:?}", text.caret_offset());
        eprintln!("  text[0..40]     = {:?}", text.get_text(0, 40));
        eprintln!("  word @0         = {:?}", text.get_string_at_offset(0, GRANULARITY_WORD));
        eprintln!("  line @0         = {:?}", text.get_string_at_offset(0, GRANULARITY_LINE));
        eprintln!("  extents 0..5 WINDOW = {:?}", text.get_range_extents(0, 5, COORD_WINDOW));
        eprintln!("  extents 0..5 SCREEN = {:?}", text.get_range_extents(0, 5, COORD_SCREEN));
        eprintln!("  n_selections    = {:?}", text.get_n_selections());
    }

    /// Live watch of the production event path (our own bridge): for 25s, print
    /// every focus change and selection change. Run on-device, then click into
    /// a text widget, type, and select text with the mouse:
    ///   cargo test -p bacak-compositor --features udev -- --ignored --nocapture live_probe_events
    #[test]
    #[ignore]
    fn live_probe_events() {
        use std::time::{Duration, Instant};
        let Some(bridge) = AtspiBridge::start() else {
            eprintln!("no a11y bus");
            return;
        };
        eprintln!("watching 25s — click a text widget, type, then drag-select with the mouse…");
        let start = Instant::now();
        let mut last: Option<String> = None;
        while start.elapsed() < Duration::from_secs(25) {
            std::thread::sleep(Duration::from_millis(400));
            let f = bridge.focused().map(|a| format!("{} {}", a.bus, a.path.as_str()));
            if f != last {
                eprintln!("[{:?}] focused -> {:?}", start.elapsed(), f);
                last = f;
            }
            if bridge.take_selection_dirty() {
                let sel = bridge
                    .focused()
                    .and_then(|a| bridge.text(&a))
                    .and_then(|t| t.get_selection(0).ok());
                eprintln!("[{:?}] TextSelectionChanged; selection={:?}", start.elapsed(), sel);
            }
        }
        eprintln!("done watching");
    }
}
