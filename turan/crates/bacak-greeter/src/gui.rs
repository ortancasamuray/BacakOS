//! Graphical greeter frontend (egui / eframe).
//!
//! Renders the login screen from `docs/GREETER_UI.md` and drives the
//! [`bacak_greeter::GreeterClient`]. The client speaks a **blocking** protocol
//! over a UNIX socket, so all IPC runs on a dedicated worker thread; the egui
//! UI thread exchanges [`Cmd`]s and [`Evt`]s with it over channels and polls
//! events each frame (immediate-mode fits this naturally — no async runtime).
//!
//! Threading:
//! ```text
//!   egui UI thread ──Cmd──▶ worker thread ──▶ GreeterClient ──▶ daemon
//!                  ◀─Evt──                   (PAM, sessions, power)
//! ```
//!
//! Running this needs the daemon's socket in `BDM_GREETER_SOCKET` and a Wayland
//! or X11 display (the daemon normally starts it on `bacak-compositor`). Without
//! a socket it shows a clear error state instead of crashing.

use bacak_common::config::{ColorMode, KeyboardMode};
use bacak_common::ipc::{GreeterPolicy, Secret};
use bacak_common::power::PowerAction;
use bacak_common::sessions::Session;
use bacak_common::users::User;
use bacak_greeter::{AuthStep, GreeterClient, Welcome};
use eframe::egui;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Duration;

/// Commands the UI sends to the worker.
enum Cmd {
    StartAuth(String),
    Secret(Secret),
    Cancel,
    StartSession(String),
    Power(PowerAction),
}

/// Events the worker sends to the UI.
enum Evt {
    Connected(Welcome),
    Users(Vec<User>),
    Sessions(Vec<Session>),
    Prompt { message: String, echo: bool },
    Info(String),
    AuthOk,
    AuthFail(String),
    SessionStarting,
    Error(String),
    Fatal(String),
}

/// Entry point used by `main` under `--features gui`.
pub fn run() -> std::process::ExitCode {
    let (cmd_tx, cmd_rx) = channel::<Cmd>();
    let (evt_tx, evt_rx) = channel::<Evt>();

    std::thread::spawn(move || worker(cmd_rx, evt_tx));

    let app = GreeterApp::new(cmd_tx, evt_rx);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            // A greeter owns the whole screen: borderless and fullscreen so it
            // fills the display rather than floating as a decorated window.
            .with_fullscreen(true)
            .with_decorations(false)
            .with_inner_size([960.0, 640.0])
            .with_title("Bacak Display Manager"),
        ..Default::default()
    };

    match eframe::run_native(
        "Bacak Display Manager",
        options,
        Box::new(|cc| {
            cc.egui_ctx.set_visuals(egui::Visuals::dark());
            Ok(Box::new(app))
        }),
    ) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("greeter gui: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

// ---------------------------------------------------------------------------
// Worker thread: owns the blocking GreeterClient.
// ---------------------------------------------------------------------------

fn worker(cmd_rx: Receiver<Cmd>, evt_tx: Sender<Evt>) {
    let (mut client, welcome) = match GreeterClient::connect_from_env() {
        Ok(pair) => pair,
        Err(e) => {
            let _ = evt_tx.send(Evt::Fatal(format!("cannot connect to daemon: {e}")));
            return;
        }
    };
    let _ = evt_tx.send(Evt::Connected(welcome));
    if let Ok(users) = client.list_users() {
        let _ = evt_tx.send(Evt::Users(users));
    }
    if let Ok(sessions) = client.list_sessions() {
        let _ = evt_tx.send(Evt::Sessions(sessions));
    }

    while let Ok(cmd) = cmd_rx.recv() {
        match cmd {
            Cmd::StartAuth(user) => run_auth(&mut client, &evt_tx, &cmd_rx, user),
            Cmd::StartSession(id) => match client.start_session(&id) {
                Ok(()) => {
                    let _ = evt_tx.send(Evt::SessionStarting);
                    return; // greeter hands over; worker is done
                }
                Err(e) => {
                    let _ = evt_tx.send(Evt::Error(e.to_string()));
                }
            },
            Cmd::Power(action) => {
                if let Err(e) = client.power(action) {
                    let _ = evt_tx.send(Evt::Error(e.to_string()));
                }
            }
            // Stray secret/cancel outside an auth flow: ignore.
            Cmd::Secret(_) | Cmd::Cancel => {}
        }
    }
}

/// Drive one authentication conversation, blocking for secrets as PAM asks.
fn run_auth(
    client: &mut GreeterClient,
    evt_tx: &Sender<Evt>,
    cmd_rx: &Receiver<Cmd>,
    user: String,
) {
    let mut step = match client.start_auth(&user) {
        Ok(s) => s,
        Err(e) => {
            let _ = evt_tx.send(Evt::AuthFail(e.to_string()));
            return;
        }
    };
    loop {
        match step {
            AuthStep::Prompt { message, echo } => {
                let _ = evt_tx.send(Evt::Prompt { message, echo });
                // Block until the UI supplies the secret (or cancels), ignoring
                // any stray commands that arrive mid-conversation.
                let answer = loop {
                    match cmd_rx.recv() {
                        Ok(Cmd::Secret(secret)) => break Some(secret),
                        Ok(Cmd::Cancel) | Err(_) => break None,
                        _ => continue,
                    }
                };
                match answer {
                    Some(secret) => {
                        step = match client.answer(secret) {
                            Ok(s) => s,
                            Err(e) => {
                                let _ = evt_tx.send(Evt::AuthFail(e.to_string()));
                                return;
                            }
                        };
                    }
                    None => {
                        let _ = client.cancel_auth();
                        let _ = evt_tx.send(Evt::AuthFail("Cancelled".into()));
                        return;
                    }
                }
            }
            AuthStep::Info { message } => {
                let _ = evt_tx.send(Evt::Info(message));
                step = match client.next_step() {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = evt_tx.send(Evt::AuthFail(e.to_string()));
                        return;
                    }
                };
            }
            AuthStep::Done { success, message } => {
                if success {
                    let _ = evt_tx.send(Evt::AuthOk);
                } else {
                    let _ = evt_tx.send(Evt::AuthFail(message.unwrap_or_default()));
                }
                return;
            }
        }
    }
}

/// Uppercase a key for display/entry. In the Turkish layout, dotted-i maps to
/// the dotted capital `İ` (not ASCII `I`); the dotless `ı` already uppercases to
/// `I` under Unicode default rules, so only `i` needs special-casing.
fn osk_upper(c: char, tr: bool) -> String {
    if tr && c == 'i' {
        "İ".to_string()
    } else {
        c.to_uppercase().collect()
    }
}

/// Parse a `#rrggbb` accent string into an egui colour; `None` if malformed.
fn parse_hex(s: &str) -> Option<egui::Color32> {
    let h = s.strip_prefix('#').unwrap_or(s);
    if h.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&h[0..2], 16).ok()?;
    let g = u8::from_str_radix(&h[2..4], 16).ok()?;
    let b = u8::from_str_radix(&h[4..6], 16).ok()?;
    Some(egui::Color32::from_rgb(r, g, b))
}

// ---------------------------------------------------------------------------
// egui application.
// ---------------------------------------------------------------------------

#[derive(PartialEq)]
enum Phase {
    Connecting,
    Login,
    /// PAM asked for an additional secret (e.g. PIN) we didn't pre-fill.
    Prompting,
    Authenticating,
    Starting,
    Fatal,
}

/// Which text field the on-screen keyboard types into.
#[derive(Clone, Copy, PartialEq)]
enum Field {
    User,
    Secret,
}

/// A key pressed on the on-screen keyboard.
enum OskKey {
    Char(char),
    Space,
    Backspace,
    Shift,
    Enter,
}

struct GreeterApp {
    cmd_tx: Sender<Cmd>,
    evt_rx: Receiver<Evt>,
    phase: Phase,
    touchscreen: bool,
    /// Admin policy received in `Welcome`; gates visibility, power, appearance.
    policy: GreeterPolicy,
    users: Vec<User>,
    sessions: Vec<Session>,
    selected_user: String,
    selected_session: String,
    secret: String,
    /// Secret typed on the login screen, sent when the first prompt arrives.
    pending_secret: Option<Secret>,
    secret_mask: bool,
    prompt_label: String,
    info: String,
    error: String,
    /// On-screen keyboard state.
    osk_target: Field,
    osk_open: bool,
    osk_shift: bool,
}

impl GreeterApp {
    fn new(cmd_tx: Sender<Cmd>, evt_rx: Receiver<Evt>) -> Self {
        Self {
            cmd_tx,
            evt_rx,
            phase: Phase::Connecting,
            touchscreen: false,
            policy: GreeterPolicy::default(),
            users: Vec::new(),
            sessions: Vec::new(),
            selected_user: String::new(),
            selected_session: String::new(),
            secret: String::new(),
            pending_secret: None,
            secret_mask: true,
            prompt_label: "Password".into(),
            info: String::new(),
            error: String::new(),
            osk_target: Field::Secret,
            osk_open: false,
            osk_shift: false,
        }
    }

    fn drain_events(&mut self, ctx: &egui::Context) {
        while let Ok(evt) = self.evt_rx.try_recv() {
            match evt {
                Evt::Connected(w) => {
                    self.touchscreen = w.touchscreen;
                    if let Some(u) = w.last_user {
                        self.selected_user = u;
                    }
                    if let Some(s) = w.last_session {
                        self.selected_session = s;
                    }
                    self.policy = w.policy;
                    self.apply_appearance(ctx);
                    self.phase = Phase::Login;
                }
                Evt::Users(users) => {
                    if self.selected_user.is_empty() {
                        if let Some(first) = users.first() {
                            self.selected_user = first.name.clone();
                        }
                    }
                    self.users = users;
                }
                Evt::Sessions(sessions) => {
                    if self.selected_session.is_empty() {
                        if let Some(first) = sessions.first() {
                            self.selected_session = first.id.clone();
                        }
                    }
                    self.sessions = sessions;
                }
                Evt::Prompt { message, echo } => {
                    // If we already have a typed secret, answer immediately.
                    if let Some(secret) = self.pending_secret.take() {
                        let _ = self.cmd_tx.send(Cmd::Secret(secret));
                        self.phase = Phase::Authenticating;
                    } else {
                        self.prompt_label = message;
                        self.secret_mask = !echo;
                        self.secret.clear();
                        self.phase = Phase::Prompting;
                    }
                }
                Evt::Info(msg) => self.info = msg,
                Evt::AuthOk => {
                    // Authenticated: launch the selected session.
                    self.error.clear();
                    self.phase = Phase::Starting;
                    let _ = self
                        .cmd_tx
                        .send(Cmd::StartSession(self.selected_session.clone()));
                }
                Evt::AuthFail(msg) => {
                    self.error = if msg.is_empty() {
                        "Authentication failed.".into()
                    } else {
                        msg
                    };
                    self.secret.clear();
                    self.pending_secret = None;
                    self.phase = Phase::Login;
                }
                Evt::SessionStarting => {
                    self.phase = Phase::Starting;
                    // The session is taking over; close the greeter window.
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                Evt::Error(msg) => {
                    self.error = msg;
                    if self.phase == Phase::Authenticating {
                        self.phase = Phase::Login;
                    }
                }
                Evt::Fatal(msg) => {
                    self.error = msg;
                    self.phase = Phase::Fatal;
                }
            }
        }
    }

    /// Apply theme + accessibility from the admin policy. Called once when the
    /// daemon's `Welcome` arrives.
    fn apply_appearance(&self, ctx: &egui::Context) {
        let a = &self.policy.accessibility;
        let mut visuals = match self.policy.theme.mode {
            ColorMode::Light => egui::Visuals::light(),
            // No system `org.freedesktop.appearance` signal reaches the greeter
            // yet, so `auto` falls back to dark (the project default).
            ColorMode::Dark | ColorMode::Auto => egui::Visuals::dark(),
        };
        if let Some(accent) = parse_hex(&self.policy.theme.accent) {
            visuals.selection.bg_fill = accent;
            visuals.hyperlink_color = accent;
        }
        if a.high_contrast {
            visuals.override_text_color = Some(if visuals.dark_mode {
                egui::Color32::WHITE
            } else {
                egui::Color32::BLACK
            });
        }

        // Round every widget and give windows/popups a soft drop shadow, so the
        // flat immediate-mode UI gains a little depth — the login card in
        // `update` builds on this for its raised, 3D look.
        let rounding = egui::Rounding::same(8.0);
        for w in [
            &mut visuals.widgets.noninteractive,
            &mut visuals.widgets.inactive,
            &mut visuals.widgets.hovered,
            &mut visuals.widgets.active,
            &mut visuals.widgets.open,
        ] {
            w.rounding = rounding;
        }
        visuals.window_rounding = egui::Rounding::same(14.0);
        let shadow = egui::epaint::Shadow {
            offset: egui::vec2(0.0, 8.0),
            blur: 32.0,
            spread: 0.0,
            color: egui::Color32::from_black_alpha(96),
        };
        visuals.window_shadow = shadow;
        visuals.popup_shadow = shadow;
        ctx.set_visuals(visuals);

        // Global UI scale: the accessibility factor, bumped for large fonts.
        // Clamped to a sane range so a bad config can't make the UI unusable.
        let mut zoom = if a.scale.is_finite() { a.scale } else { 1.0 };
        if a.large_fonts {
            zoom *= 1.25;
        }
        ctx.set_zoom_factor(zoom.clamp(0.5, 3.0));
    }

    /// The localized UI string table for the configured greeter language.
    fn t(&self) -> &'static bacak_common::i18n::Ui {
        self.policy.greeter.language.ui()
    }

    /// Whether the on-screen keyboard may appear at all, per `keyboard.mode`.
    fn osk_by_mode(&self) -> bool {
        match self.policy.keyboard.mode {
            KeyboardMode::Always => true,
            KeyboardMode::Never => false,
            KeyboardMode::Auto => self.touchscreen,
        }
    }

    /// Whether to render the keyboard right now. With `show_on_focus`, it only
    /// appears once a text field has been focused (`osk_open`); otherwise it is
    /// always present when the mode permits it.
    fn osk_active(&self) -> bool {
        self.osk_by_mode() && (!self.policy.keyboard.show_on_focus || self.osk_open)
    }

    /// The buffer the keyboard currently edits.
    fn osk_buffer(&mut self) -> &mut String {
        match self.osk_target {
            Field::User => &mut self.selected_user,
            Field::Secret => &mut self.secret,
        }
    }

    /// Apply one on-screen key press to the targeted field.
    fn apply_osk(&mut self, key: OskKey) {
        match key {
            OskKey::Char(c) => {
                if self.osk_shift {
                    let tr = self.policy.keyboard.layout.eq_ignore_ascii_case("tr");
                    let up = osk_upper(c, tr);
                    self.osk_buffer().push_str(&up);
                } else {
                    self.osk_buffer().push(c);
                }
                self.osk_shift = false; // one-shot shift
            }
            OskKey::Space => self.osk_buffer().push(' '),
            OskKey::Backspace => {
                self.osk_buffer().pop();
            }
            OskKey::Shift => self.osk_shift = !self.osk_shift,
            OskKey::Enter => match self.phase {
                Phase::Login => self.submit_login(),
                Phase::Prompting => self.submit_prompt(),
                _ => {}
            },
        }
    }

    /// Note that `field` was focused this frame: it becomes the keyboard target,
    /// and (when `show_on_focus`) opens the keyboard.
    fn note_focus(&mut self, field: Field) {
        self.osk_target = field;
        if self.policy.keyboard.show_on_focus {
            self.osk_open = true;
        }
    }

    /// Render the on-screen keyboard. Letters honour the one-shot shift; the
    /// `tr` layout adds a row of Turkish-specific characters.
    fn view_osk(&mut self, ui: &mut egui::Ui) {
        let t = self.t();
        let shift = self.osk_shift;
        let layout = self.policy.keyboard.layout.to_ascii_lowercase();
        // Turkish casing (dotted İ / dotless ı) applies to every Turkish layout.
        let tr = layout.starts_with("tr") || layout == "f";
        let mut pressed: Option<OskKey> = None;
        let key = egui::vec2(38.0, 38.0);

        let row = |ui: &mut egui::Ui, chars: &str, pressed: &mut Option<OskKey>| {
            ui.horizontal(|ui| {
                ui.add_space((ui.available_width() - chars.chars().count() as f32 * (key.x + 4.0)).max(0.0) / 2.0);
                for c in chars.chars() {
                    let label = if shift {
                        osk_upper(c, tr)
                    } else {
                        c.to_string()
                    };
                    if ui.add_sized(key, egui::Button::new(label)).clicked() {
                        *pressed = Some(OskKey::Char(c));
                    }
                }
            });
        };

        ui.add_space(4.0);
        row(ui, "1234567890", &mut pressed);
        match layout.as_str() {
            // Turkish F-keyboard (the project default) — a distinct letter
            // arrangement, not QWERTY. `tr` and `trf` both select it.
            "tr" | "trf" | "tr-f" | "f" => {
                row(ui, "fgğıodrnhpqw", &mut pressed);
                row(ui, "uieaütkmlyşx", &mut pressed);
                row(ui, "jövcçzsb", &mut pressed);
            }
            // Turkish Q-keyboard — QWERTY base with Turkish letters added.
            "trq" | "tr-q" => {
                row(ui, "qwertyuıopğü", &mut pressed);
                row(ui, "asdfghjklşi", &mut pressed);
                row(ui, "zxcvbnmöç", &mut pressed);
            }
            // US QWERTY.
            _ => {
                row(ui, "qwertyuiop", &mut pressed);
                row(ui, "asdfghjkl", &mut pressed);
                row(ui, "zxcvbnm", &mut pressed);
            }
        }

        // Control row.
        ui.horizontal(|ui| {
            let shift_btn = egui::Button::new(t.osk_shift).fill(if shift {
                ui.visuals().selection.bg_fill
            } else {
                ui.visuals().widgets.inactive.bg_fill
            });
            if ui.add_sized([90.0, key.y], shift_btn).clicked() {
                pressed = Some(OskKey::Shift);
            }
            if ui
                .add_sized([260.0, key.y], egui::Button::new(t.osk_space))
                .clicked()
            {
                pressed = Some(OskKey::Space);
            }
            if ui
                .add_sized([90.0, key.y], egui::Button::new(t.osk_back))
                .clicked()
            {
                pressed = Some(OskKey::Backspace);
            }
            if ui
                .add_sized([90.0, key.y], egui::Button::new(t.osk_enter))
                .clicked()
            {
                pressed = Some(OskKey::Enter);
            }
        });
        ui.add_space(4.0);

        if let Some(k) = pressed {
            self.apply_osk(k);
        }
    }

    fn submit_login(&mut self) {
        if self.selected_user.is_empty() {
            self.error = self.t().choose_user.into();
            return;
        }
        self.error.clear();
        self.info.clear();
        self.osk_open = false;
        self.osk_shift = false;
        // Move the plaintext out of the egui buffer into a Secret (zeroized on
        // drop); the buffer is left empty.
        self.pending_secret = Some(Secret::new(std::mem::take(&mut self.secret)));
        self.phase = Phase::Authenticating;
        let _ = self.cmd_tx.send(Cmd::StartAuth(self.selected_user.clone()));
    }

    fn submit_prompt(&mut self) {
        let secret = Secret::new(std::mem::take(&mut self.secret));
        self.osk_open = false;
        self.osk_shift = false;
        self.phase = Phase::Authenticating;
        let _ = self.cmd_tx.send(Cmd::Secret(secret));
    }
}

impl eframe::App for GreeterApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events(ctx);

        let t = self.t();
        egui::TopBottomPanel::bottom("power").show(ctx, |ui| {
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.label(if self.osk_by_mode() { t.osk_active } else { "" });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Comfortable touch targets with clear spacing — the power
                    // actions are easy to hit and read on a touchscreen.
                    ui.spacing_mut().item_spacing.x = 12.0;
                    // Only offer the power actions the admin permits; the daemon
                    // rejects the rest anyway, so showing them would just error.
                    for action in PowerAction::ALL {
                        if !action.allowed_by(&self.policy.power) {
                            continue;
                        }
                        let label = match action {
                            PowerAction::Shutdown => t.shutdown,
                            PowerAction::Restart => t.restart,
                            PowerAction::Suspend => t.suspend,
                            PowerAction::Hibernate => t.hibernate,
                        };
                        let button = egui::Button::new(egui::RichText::new(label).size(17.0))
                            .min_size(egui::vec2(0.0, 48.0))
                            .fill(ui.visuals().widgets.inactive.bg_fill.gamma_multiply(1.6))
                            .stroke(egui::Stroke::new(
                                1.0,
                                ui.visuals().widgets.inactive.bg_stroke.color,
                            ));
                        if ui.add_sized([150.0, 48.0], button).clicked() {
                            let _ = self.cmd_tx.send(Cmd::Power(action));
                        }
                    }
                });
            });
            ui.add_space(10.0);
        });

        // On-screen keyboard sits above the power bar, only while typing.
        if self.osk_active() && matches!(self.phase, Phase::Login | Phase::Prompting) {
            egui::TopBottomPanel::bottom("osk").show(ctx, |ui| {
                self.view_osk(ui);
            });
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            // A single fixed-width column, centred horizontally and biased a
            // little above the vertical centre, so every row shares one width
            // and left edge instead of each centring at its own size.
            const FORM_W: f32 = 360.0;
            const PAD: f32 = 28.0;
            let form_w = FORM_W.min(ui.available_width() - 32.0 - PAD * 2.0);
            let card_w = form_w + PAD * 2.0;
            let top = ((ui.available_height() - 480.0) * 0.4).max(16.0);
            ui.add_space(top);

            ui.vertical_centered(|ui| {
                ui.heading(egui::RichText::new("Bacak").size(44.0).strong());
                ui.add_space(2.0);
                ui.label(egui::RichText::new(t.subtitle).weak());
            });
            ui.add_space(20.0);

            ui.horizontal(|ui| {
                let side = ((ui.available_width() - card_w) / 2.0).max(0.0);
                ui.add_space(side);

                // Lift the form onto a raised, rounded card: one shared width and
                // left edge for every row, with a border + drop shadow so it reads
                // as a single panel instead of loose, ragged fields.
                let v = ui.visuals();
                let card_fill = v.widgets.noninteractive.bg_fill.gamma_multiply(1.7);
                let card_stroke =
                    egui::Stroke::new(1.0, v.widgets.noninteractive.bg_stroke.color);
                egui::Frame::none()
                    .fill(card_fill)
                    .rounding(14.0)
                    .stroke(card_stroke)
                    .shadow(egui::epaint::Shadow {
                        offset: egui::vec2(0.0, 10.0),
                        blur: 40.0,
                        spread: 0.0,
                        color: egui::Color32::from_black_alpha(120),
                    })
                    .inner_margin(egui::Margin::same(PAD))
                    .show(ui, |ui| {
                        ui.set_width(form_w);
                        // The card lives inside `ui.horizontal(..)`, so its inner
                        // Ui inherits a left-to-right layout — that would lay the
                        // user list, User and Password fields out side by side.
                        // Force top-down so every row stacks vertically.
                        ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                            ui.set_width(form_w);
                            match self.phase {
                                Phase::Connecting => Self::centered_status(ui, t.connecting),
                                Phase::Fatal => {
                                    ui.vertical_centered(|ui| {
                                        ui.colored_label(egui::Color32::LIGHT_RED, &self.error);
                                    });
                                }
                                Phase::Starting => Self::centered_status(ui, t.starting_session),
                                Phase::Authenticating => Self::centered_status(ui, t.authenticating),
                                Phase::Login => self.view_login(ui),
                                Phase::Prompting => self.view_prompt(ui),
                            }

                            if !self.info.is_empty() {
                                ui.add_space(8.0);
                                ui.label(&self.info);
                            }
                            if !self.error.is_empty() && self.phase != Phase::Fatal {
                                ui.add_space(8.0);
                                ui.colored_label(egui::Color32::LIGHT_RED, &self.error);
                            }
                        });
                    });
            });
        });

        // Keep polling worker events even when idle.
        ctx.request_repaint_after(Duration::from_millis(80));
    }
}

impl GreeterApp {
    /// Drop egui's per-widget undo history for a text field. egui keeps the
    /// `TextEditState` (incl. its undoer, which snapshots the edited string) in
    /// context memory keyed by widget id, so even though our live buffer is moved
    /// into a zeroizing [`Secret`] on submit, a plaintext copy of the password
    /// would otherwise linger in that undo history. Calling this every frame for
    /// the secret field keeps the undoer from accumulating, and the post-submit
    /// call clears the final snapshot once the field stops being drawn.
    fn wipe_undoer(ctx: &egui::Context, id: egui::Id) {
        if let Some(mut state) = egui::text_edit::TextEditState::load(ctx, id) {
            state.clear_undoer();
            state.store(ctx, id);
        }
    }

    /// A centred spinner + message used for the transient phases.
    fn centered_status(ui: &mut egui::Ui, msg: &str) {
        ui.vertical_centered(|ui| {
            ui.add_space(8.0);
            ui.spinner();
            ui.add_space(4.0);
            ui.label(msg);
        });
    }

    fn view_login(&mut self, ui: &mut egui::Ui) {
        let t = self.t();
        let show_list = self.policy.greeter.show_user_list && !self.users.is_empty();
        let w = ui.available_width();

        // User list — full-width rows, tidy and touch-friendly.
        if show_list {
            let users: Vec<(String, String)> = self
                .users
                .iter()
                .map(|u| (u.name.clone(), u.display_name().to_string()))
                .collect();
            for (name, display) in users {
                let selected = self.selected_user == name;
                if ui
                    .add_sized(
                        [w, 44.0],
                        egui::SelectableLabel::new(selected, display.as_str()),
                    )
                    .clicked()
                {
                    self.selected_user = name;
                }
            }
            ui.add_space(12.0);
        }

        // Manual username entry. Shown when the admin allows it, or as a
        // fallback when there is no user list to pick from (otherwise there
        // would be no way to choose an account).
        if self.policy.greeter.allow_manual_login || !show_list {
            ui.label(t.user);
            let r = ui.add_sized(
                [w, 34.0],
                egui::TextEdit::singleline(&mut self.selected_user).hint_text(t.username_hint),
            );
            if r.has_focus() {
                self.note_focus(Field::User);
            }
            ui.add_space(8.0);
        }

        // Password field.
        ui.label(t.password);
        let resp = ui.add_sized(
            [w, 34.0],
            egui::TextEdit::singleline(&mut self.secret)
                .password(true)
                .hint_text(t.password_hint),
        );
        let secret_id = resp.id;
        if resp.has_focus() {
            self.note_focus(Field::Secret);
        }
        let entered = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        ui.add_space(12.0);

        // Session selector — full width.
        ui.label(t.session);
        let current = self
            .sessions
            .iter()
            .find(|s| s.id == self.selected_session)
            .map(|s| s.name.clone())
            .unwrap_or_else(|| "Default".into());
        egui::ComboBox::from_id_salt("session")
            .selected_text(current)
            .width(w)
            .show_ui(ui, |ui| {
                let sessions: Vec<(String, String)> = self
                    .sessions
                    .iter()
                    .map(|s| (s.id.clone(), s.name.clone()))
                    .collect();
                for (id, name) in sessions {
                    ui.selectable_value(&mut self.selected_session, id, name);
                }
            });
        ui.add_space(16.0);

        // Log in — full-width, accent-filled primary button.
        let accent = ui.visuals().selection.bg_fill;
        let login = egui::Button::new(
            egui::RichText::new(t.log_in).strong().color(egui::Color32::WHITE),
        )
        .fill(accent);
        if ui.add_sized([w, 44.0], login).clicked() || entered {
            self.submit_login();
        }

        // Never let egui retain a plaintext password in its undo history.
        Self::wipe_undoer(ui.ctx(), secret_id);
    }

    fn view_prompt(&mut self, ui: &mut egui::Ui) {
        let t = self.t();
        let w = ui.available_width();
        ui.label(&self.prompt_label);
        let resp = ui.add_sized(
            [w, 34.0],
            egui::TextEdit::singleline(&mut self.secret).password(self.secret_mask),
        );
        let secret_id = resp.id;
        if resp.has_focus() {
            self.note_focus(Field::Secret);
        }
        let entered = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            let bw = (w - 8.0) / 2.0;
            if ui
                .add_sized([bw, 42.0], egui::Button::new(t.submit))
                .clicked()
                || entered
            {
                self.submit_prompt();
            }
            if ui
                .add_sized([bw, 42.0], egui::Button::new(t.cancel))
                .clicked()
            {
                let _ = self.cmd_tx.send(Cmd::Cancel);
                self.phase = Phase::Login;
                self.secret.clear();
                self.osk_open = false;
            }
        });

        // Never let egui retain a plaintext secret in its undo history.
        Self::wipe_undoer(ui.ctx(), secret_id);
    }
}
