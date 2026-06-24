//! Bacak OS — Demonstration CLI
//!
//! Wires every service crate into a small command-line surface so the Rust
//! backend can be exercised independently of the desktop frontend.
//!
//! Examples:
//!   bacak ls .
//!   bacak ls --archive release.zip /docs
//!   bacak archive list release.tar.gz
//!   bacak archive extract release.zip path/inside.txt /tmp/out.txt
//!   bacak wm demo
//!   bacak osk demo

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum Toggle { On, Off }

impl Toggle {
    fn as_bool(self) -> bool { matches!(self, Toggle::On) }
}

use bacak_compositor::input::{FocusedField, InputMode, OskConfig, OskController, OskRect};
use bacak_compositor::wm::{Monitor, Rect, SnapZone, WindowManager};
use bacak_services::device::{DeviceProvider, MockProvider};
use bacak_services::fs::VfsPath;

// ---------------------------------------------------------------------------
// CLI surface
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(name = "bacak", version, about = "Bacak OS service CLI")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,

    /// Emit machine-readable JSON instead of pretty output.
    #[arg(long, global = true)]
    json: bool,
}

#[derive(Subcommand)]
enum Cmd {
    /// List a directory (native or inside an archive).
    Ls {
        /// Path to list. If `--archive` is set, this is the path *inside* the archive.
        #[arg(default_value = "")]
        path: String,

        /// Treat the listing as being inside this archive file.
        #[arg(long)]
        archive: Option<PathBuf>,
    },

    /// Archive operations.
    Archive {
        #[command(subcommand)]
        op: ArcOp,
    },

    /// Window manager operations.
    Wm {
        #[command(subcommand)]
        op: WmOp,
    },

    /// On-screen keyboard operations.
    Osk {
        #[command(subcommand)]
        op: OskOp,
    },

    /// Device controls — audio, Wi-Fi, Bluetooth (uses the in-memory mock).
    Device {
        #[command(subcommand)]
        op: DeviceOp,
    },

    /// Launch the Bacak shell session (currently: prototype in kiosk browser).
    /// Wired up as the `Exec` of /usr/share/wayland-sessions/bacak.desktop.
    Shell {
        /// Don't actually launch a browser, just print what would run.
        #[arg(long)]
        dry_run: bool,
    },

    /// Inspect / validate the compositor config (`compositor.json`).
    Config {
        #[command(subcommand)]
        op: ConfigOp,
    },

    /// Inspect the saved workspace/window session (`session.json`).
    Session {
        #[command(subcommand)]
        op: SessionOp,
    },

    /// Manage the dock's pinned apps (`dock_pinned` in
    /// `compositor.json`). Writes are atomic and picked up by the
    /// compositor's config hot-reload.
    Dock {
        #[command(subcommand)]
        op: DockOp,
    },
}

#[derive(Subcommand)]
enum DockOp {
    /// Pin an app id (or `.desktop` basename) to the dock. Appended in
    /// order; pinning an already-pinned app is a no-op.
    Pin {
        /// App id, e.g. `firefox` or `org.kde.kate`.
        app: String,
    },

    /// Remove an app from the dock's pins. Unpinning one that isn't
    /// pinned succeeds (idempotent) and says so.
    Unpin {
        /// App id to remove.
        app: String,
    },

    /// List the pinned apps, in order.
    List,
}

#[derive(Subcommand)]
enum SessionOp {
    /// Print the persisted session snapshot.
    Show,

    /// Delete `session.json` so the next launch starts fresh.
    Clear,
}

#[derive(Subcommand)]
enum ConfigOp {
    /// Show the effective config the compositor would use (file +
    /// defaults + `BACAK_BLUR` override).
    Show,

    /// Parse a config file strictly and report why it's bad, if so.
    /// Exits non-zero on a parse error.
    Validate {
        /// File to check. Defaults to the resolved compositor.json
        /// path.
        #[arg(long)]
        file: Option<PathBuf>,
    },

    /// Open the config in `$VISUAL`/`$EDITOR` (creating it from
    /// defaults if absent), then validate. Exits non-zero if the
    /// saved file is invalid — your edits are never discarded.
    Edit,

    /// Set one key without an editor: validates the value, clamps it
    /// like a loaded file, and writes atomically.
    Set {
        /// Field name (e.g. `blur`, `blur_radius`, `shadow_step`).
        key: String,
        /// New value (`true`/`false` for `blur`, a number otherwise).
        value: String,
    },

    /// Reset config to defaults. With a `key`, only that field;
    /// without one, the whole file (also the recovery path for a
    /// corrupt config). Writes atomically.
    Reset {
        /// Field to reset. Omit to reset everything.
        key: Option<String>,
    },
}

#[derive(Subcommand)]
enum DeviceOp {
    /// Audio controls.
    Sound {
        #[command(subcommand)]
        op: SoundOp,
    },
    /// Wi-Fi controls.
    Net {
        #[command(subcommand)]
        op: NetOp,
    },
    /// Bluetooth controls.
    Bt {
        #[command(subcommand)]
        op: BtOp,
    },
}

#[derive(Subcommand)]
enum SoundOp {
    /// Print current audio state.
    State,
    /// Set master volume (0..=100).
    Volume { value: u8 },
    /// Toggle mute or set explicitly with --on/--off.
    Mute {
        #[arg(long)]
        on: bool,
        #[arg(long)]
        off: bool,
    },
    /// Select active output sink by id.
    Output { id: String },
}

#[derive(Subcommand)]
enum NetOp {
    /// Print Wi-Fi state and visible networks.
    State,
    /// Enable or disable the Wi-Fi radio.
    Toggle { state: Toggle },
    /// Connect to a network. Use `--password` for secured networks.
    Connect {
        ssid: String,
        #[arg(long)]
        password: Option<String>,
    },
    /// Disconnect from the current network.
    Disconnect,
}

#[derive(Subcommand)]
enum BtOp {
    /// Print Bluetooth state and known devices.
    State,
    /// Enable or disable the Bluetooth radio.
    Toggle { state: Toggle },
    /// Start or stop discovery.
    Scan { state: Toggle },
    /// Pair with a device by MAC.
    Pair { mac: String },
    /// Connect to a device by MAC.
    Connect { mac: String },
    /// Disconnect a device by MAC.
    Disconnect { mac: String },
}

#[derive(Subcommand)]
enum ArcOp {
    /// List entries directly inside `dir` of an archive (default: root).
    List {
        archive: PathBuf,
        #[arg(default_value = "")]
        dir: String,
    },
    /// Extract one entry to a destination.
    Extract {
        archive: PathBuf,
        inside: String,
        to: PathBuf,
    },
}

#[derive(Subcommand)]
enum WmOp {
    /// Run a scripted demo (open windows, snap, move, list).
    Demo,
}

#[derive(Subcommand)]
enum OskOp {
    /// Run a scripted OSK lifecycle demo.
    Demo,
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Ls { path, archive } => cmd_ls(path, archive, cli.json).await,
        Cmd::Archive { op } => match op {
            ArcOp::List { archive, dir } => cmd_arc_list(archive, dir, cli.json),
            ArcOp::Extract { archive, inside, to } => cmd_arc_extract(archive, inside, to, cli.json),
        },
        Cmd::Wm { op } => match op {
            WmOp::Demo => cmd_wm_demo(cli.json),
        },
        Cmd::Osk { op } => match op {
            OskOp::Demo => cmd_osk_demo(cli.json),
        },
        Cmd::Device { op } => cmd_device(op, cli.json),
        Cmd::Shell { dry_run } => cmd_shell(dry_run),
        Cmd::Config { op } => cmd_config(op, cli.json),
        Cmd::Session { op } => cmd_session(op, cli.json),
        Cmd::Dock { op } => cmd_dock(op, cli.json),
    }
}

fn cmd_dock(op: DockOp, as_json: bool) -> Result<()> {
    use bacak_compositor::config::CompositorConfig;

    match op {
        // Read-only: the effective, merged + sanitised list.
        DockOp::List => {
            let cfg = CompositorConfig::load();
            if as_json {
                println!(
                    "{}",
                    serde_json::json!({
                        "dock": cfg.dock,
                        "dock_pinned": cfg.dock_pinned,
                    })
                );
            } else if cfg.dock_pinned.is_empty() {
                println!("(no pinned apps)");
            } else {
                for (i, app) in cfg.dock_pinned.iter().enumerate() {
                    println!("{}. {app}", i + 1);
                }
            }
            return Ok(());
        }
        DockOp::Pin { .. } | DockOp::Unpin { .. } => {}
    }

    let path = CompositorConfig::config_path()
        .context("could not resolve a config path")?;

    // Start from the existing file so the other keys survive; refuse
    // to clobber an invalid file, mirroring `config set`.
    let mut cfg = if path.exists() {
        CompositorConfig::load_from(&path).map_err(|m| {
            anyhow::anyhow!(
                "{}: {m}\nfix it or run `bacak config edit` first",
                path.display()
            )
        })?
    } else {
        CompositorConfig::default()
    };

    let (what, note) = match op {
        DockOp::Pin { app } => {
            if cfg.dock_pinned.iter().any(|p| p == &app) {
                (format!("pin {app}"), Some(format!("{app} was already pinned")))
            } else {
                cfg.dock_pinned.push(app.clone());
                (format!("pin {app}"), None)
            }
        }
        DockOp::Unpin { app } => {
            let before = cfg.dock_pinned.len();
            cfg.dock_pinned.retain(|p| p != &app);
            let note = (cfg.dock_pinned.len() == before)
                .then(|| format!("{app} was not pinned"));
            (format!("unpin {app}"), note)
        }
        DockOp::List => unreachable!("handled above"),
    };

    // `sanitize` runs the same normaliser `config set` uses (trim,
    // drop empties, de-dupe, preserve order).
    cfg.sanitize();
    write_config_atomic(&path, &cfg)?;

    if as_json {
        println!(
            "{}",
            serde_json::json!({
                "path": path.display().to_string(),
                "action": what,
                "note": note,
                "dock_pinned": cfg.dock_pinned,
            })
        );
    } else {
        if let Some(n) = &note {
            println!("note: {n}");
        }
        println!("{}: {what}", path.display());
        if cfg.dock_pinned.is_empty() {
            println!("pinned: (none)");
        } else {
            println!("pinned: {}", cfg.dock_pinned.join(", "));
        }
    }
    Ok(())
}

fn cmd_config(op: ConfigOp, as_json: bool) -> Result<()> {
    use bacak_compositor::config::CompositorConfig;

    match op {
        ConfigOp::Show => {
            let path = CompositorConfig::config_path();
            let exists = path.as_ref().map(|p| p.is_file()).unwrap_or(false);
            // `load()` already merges file + defaults + sanitises.
            let cfg = CompositorConfig::load();
            let blur_effective = cfg.blur_enabled();
            if as_json {
                println!(
                    "{}",
                    serde_json::json!({
                        "path": path.as_ref().map(|p| p.display().to_string()),
                        "file_exists": exists,
                        "config": cfg,
                        "blur_effective": blur_effective,
                    })
                );
            } else {
                match &path {
                    Some(p) => println!("path:           {}", p.display()),
                    None => println!("path:           <unresolved>"),
                }
                println!("file exists:    {exists}");
                println!("blur:           {}", cfg.blur);
                println!(
                    "blur effective: {blur_effective}{}",
                    if blur_effective && !cfg.blur {
                        "  (forced by BACAK_BLUR)"
                    } else {
                        ""
                    }
                );
                println!("blur radius:    {}", cfg.blur_radius);
                println!(
                    "shadow ramp:    top {} step {} floor {}",
                    cfg.shadow_top, cfg.shadow_step, cfg.shadow_floor
                );
                println!(
                    "dock slot:      {} x {}",
                    cfg.dock_slot_w, cfg.dock_slot_h
                );
            }
            Ok(())
        }
        ConfigOp::Validate { file } => {
            let target = match file.or_else(CompositorConfig::config_path) {
                Some(p) => p,
                None => bail!("could not resolve a config path"),
            };
            match CompositorConfig::load_from(&target) {
                Ok(cfg) => {
                    if as_json {
                        println!(
                            "{}",
                            serde_json::json!({
                                "valid": true,
                                "path": target.display().to_string(),
                                "sanitized": cfg,
                            })
                        );
                    } else {
                        println!("{}: valid", target.display());
                        println!("(sanitised) blur_radius={} shadow=({},{},{}) dock=({}x{})",
                            cfg.blur_radius, cfg.shadow_top, cfg.shadow_step,
                            cfg.shadow_floor, cfg.dock_slot_w, cfg.dock_slot_h);
                    }
                    Ok(())
                }
                Err(msg) => bail!("{}: {msg}", target.display()),
            }
        }
        ConfigOp::Edit => {
            let path = CompositorConfig::config_path()
                .context("could not resolve a config path")?;

            // Seed a fully-populated default file so the user edits a
            // complete template rather than a blank. Creating the
            // config dir is fine here — it's an explicit user action.
            if !path.exists() {
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir).with_context(|| {
                        format!("cannot create config dir {}", dir.display())
                    })?;
                }
                let template = serde_json::to_string_pretty(
                    &CompositorConfig::default(),
                )
                .expect("default config always serialises");
                std::fs::write(&path, template + "\n")
                    .with_context(|| format!("cannot write {}", path.display()))?;
            }

            // `$VISUAL` then `$EDITOR` then `vi`. The value may carry
            // args (`code --wait`), so split on whitespace.
            let editor = std::env::var("VISUAL")
                .or_else(|_| std::env::var("EDITOR"))
                .unwrap_or_else(|_| "vi".to_string());
            let mut parts = editor.split_whitespace();
            let Some(prog) = parts.next() else {
                bail!("empty $VISUAL/$EDITOR");
            };
            let status = std::process::Command::new(prog)
                .args(parts)
                .arg(&path)
                .status()
                .with_context(|| format!("failed to launch editor `{prog}`"))?;
            if !status.success() {
                // Editor itself errored/was killed — still validate
                // whatever's on disk so the user gets a verdict.
                eprintln!("warning: editor exited with {status}");
            }

            match CompositorConfig::load_from(&path) {
                Ok(_) => {
                    println!("{}: valid (hot-reload will pick it up)", path.display());
                    Ok(())
                }
                Err(msg) => bail!(
                    "{}: {msg}\n(your edits are kept; the compositor \
                     falls back to defaults until fixed)",
                    path.display()
                ),
            }
        }
        ConfigOp::Set { key, value } => {
            let path = CompositorConfig::config_path()
                .context("could not resolve a config path")?;

            // Start from the existing file (preserving its other
            // keys); refuse to silently clobber an invalid file.
            let mut cfg = if path.exists() {
                CompositorConfig::load_from(&path).map_err(|m| {
                    anyhow::anyhow!(
                        "{}: {m}\nfix it or run `bacak config edit` first",
                        path.display()
                    )
                })?
            } else {
                CompositorConfig::default()
            };

            cfg.set_field(&key, &value).map_err(|m| anyhow::anyhow!(m))?;
            cfg.sanitize();
            write_config_atomic(&path, &cfg)?;
            report_stored(&path, &format!("set {key}"), &cfg, as_json);
            Ok(())
        }
        ConfigOp::Reset { key } => {
            let path = CompositorConfig::config_path()
                .context("could not resolve a config path")?;

            let (mut cfg, what) = match &key {
                // Single field: preserve the rest, so the existing
                // file must be valid.
                Some(k) => {
                    let mut c = if path.exists() {
                        CompositorConfig::load_from(&path).map_err(|m| {
                            anyhow::anyhow!(
                                "{}: {m}\nfix it or run `bacak config reset` \
                                 (no key) to recover",
                                path.display()
                            )
                        })?
                    } else {
                        CompositorConfig::default()
                    };
                    c.reset_field(k).map_err(|m| anyhow::anyhow!(m))?;
                    (c, format!("reset {k}"))
                }
                // Whole file → defaults. This is also the escape
                // hatch for a corrupt config, so we don't read the
                // old one at all.
                None => (CompositorConfig::default(), "reset all".to_string()),
            };
            cfg.sanitize();
            write_config_atomic(&path, &cfg)?;
            report_stored(&path, &what, &cfg, as_json);
            Ok(())
        }
    }
}

/// Atomically replace the config file: write a sibling temp file then
/// rename over the target (same-dir rename is atomic on POSIX).
fn write_config_atomic(
    path: &std::path::Path,
    cfg: &bacak_compositor::config::CompositorConfig,
) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("cannot create config dir {}", dir.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(cfg).expect("config always serialises");
    std::fs::write(&tmp, json + "\n")
        .with_context(|| format!("cannot write {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("cannot replace {}", path.display()))?;
    Ok(())
}

/// Print the stored config after a `set`/`reset`, human or JSON.
fn report_stored(
    path: &std::path::Path,
    what: &str,
    cfg: &bacak_compositor::config::CompositorConfig,
    as_json: bool,
) {
    if as_json {
        println!(
            "{}",
            serde_json::json!({
                "path": path.display().to_string(),
                "action": what,
                "config": cfg,
            })
        );
    } else {
        println!("{}: {what} (stored config below)", path.display());
        println!(
            "blur={} blur_radius={} shadow=({},{},{}) dock=({}x{})",
            cfg.blur, cfg.blur_radius, cfg.shadow_top, cfg.shadow_step,
            cfg.shadow_floor, cfg.dock_slot_w, cfg.dock_slot_h
        );
    }
}

fn cmd_session(op: SessionOp, as_json: bool) -> Result<()> {
    use bacak_compositor::session::SessionSnapshot;

    match op {
        SessionOp::Clear => {
            let path = SessionSnapshot::session_path()
                .context("could not resolve a session path")?;
            let removed = match std::fs::remove_file(&path) {
                Ok(()) => true,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
                Err(e) => bail!("cannot remove {}: {e}", path.display()),
            };
            if as_json {
                println!(
                    "{}",
                    serde_json::json!({
                        "path": path.display().to_string(),
                        "removed": removed,
                    })
                );
            } else if removed {
                println!("{}: cleared", path.display());
            } else {
                println!("{}: nothing to clear", path.display());
            }
            Ok(())
        }
        SessionOp::Show => {
            let path = SessionSnapshot::session_path();
            let exists = path.as_ref().map(|p| p.is_file()).unwrap_or(false);
            let snap = SessionSnapshot::load();
            if as_json {
                println!(
                    "{}",
                    serde_json::json!({
                        "path": path.as_ref().map(|p| p.display().to_string()),
                        "file_exists": exists,
                        "session": snap,
                    })
                );
            } else {
                match &path {
                    Some(p) => println!("path:           {}", p.display()),
                    None => println!("path:           <unresolved>"),
                }
                println!("file exists:    {exists}");
                println!(
                    "primary:        {} workspace(s), active #{}",
                    snap.primary.workspace_count, snap.primary.active_index
                );
                println!("windows:        {}", snap.windows.len());
                for w in &snap.windows {
                    let label = if w.title.trim().is_empty() {
                        w.app.clone()
                    } else {
                        format!("{} — {}", w.app, w.title)
                    };
                    println!(
                        "  {:<28} ({:.0}, {:.0}, {:.0}, {:.0})",
                        label, w.x, w.y, w.w, w.h
                    );
                }
            }
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// shell — locate the prototype + spawn it in a kiosk browser
// ---------------------------------------------------------------------------

fn cmd_shell(dry_run: bool) -> Result<()> {
    let candidates = [
        PathBuf::from("/usr/share/bacak/ui/index.html"),
        PathBuf::from("/usr/local/share/bacak/ui/index.html"),
        PathBuf::from("./index.html"),
    ];
    let ui = candidates
        .iter()
        .find(|p| p.exists())
        .with_context(|| {
            "Bacak frontend not found (expected /usr/share/bacak/ui/index.html or ./index.html)"
        })?
        .canonicalize()?;
    let url = format!("file://{}", ui.display());

    // Browser candidates in order of preference. Each entry is (program, args
    // appended *before* the URL). Kiosk mode hides browser chrome.
    let attempts: &[(&str, &[&str])] = &[
        ("chromium",         &["--kiosk", "--no-first-run", "--ozone-platform-hint=auto"]),
        ("chromium-browser", &["--kiosk", "--no-first-run"]),
        ("google-chrome",    &["--kiosk", "--no-first-run"]),
        ("firefox",          &["--kiosk"]),
        ("xdg-open",         &[]),
    ];

    eprintln!("bacak shell: serving {}", url);

    for (cmd, args) in attempts {
        if dry_run {
            println!("would exec: {} {} {}", cmd, args.join(" "), url);
            return Ok(());
        }
        let mut c = std::process::Command::new(cmd);
        c.args(*args).arg(&url);
        match c.status() {
            Ok(status) if status.success() => return Ok(()),
            Ok(_) => continue,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                return Err(anyhow::Error::new(e).context(format!("failed to spawn {cmd}")));
            }
        }
    }
    anyhow::bail!(
        "no supported browser found — install chromium, firefox, or xdg-utils, \
         or replace this command with the Bacak compositor"
    )
}

// ---------------------------------------------------------------------------
// device
// ---------------------------------------------------------------------------

fn cmd_device(op: DeviceOp, as_json: bool) -> Result<()> {
    // The mock is created fresh per invocation. A persistent daemon would
    // hold one provider for the lifetime of the process.
    let p = MockProvider::new();
    match op {
        DeviceOp::Sound { op } => cmd_sound(&p, op, as_json),
        DeviceOp::Net   { op } => cmd_net(&p, op, as_json),
        DeviceOp::Bt    { op } => cmd_bt(&p, op, as_json),
    }
}

fn cmd_sound(p: &MockProvider, op: SoundOp, as_json: bool) -> Result<()> {
    match op {
        SoundOp::State => {
            let s = p.audio_state()?;
            if as_json {
                println!("{}", serde_json::to_string_pretty(&s)?);
            } else {
                println!("Volume:  {}{}", s.master_volume, if s.muted { "  (muted)" } else { "" });
                println!("\nOutput sinks:");
                for sink in &s.sinks {
                    println!(
                        "  {} {:<22}  {:?}",
                        if sink.is_default { "✓" } else { " " },
                        sink.name, sink.kind
                    );
                }
                println!("\nInput sources:");
                for src in &s.sources {
                    println!(
                        "  {} {:<22}  {:?}",
                        if src.is_default { "✓" } else { " " },
                        src.name, src.kind
                    );
                }
            }
        }
        SoundOp::Volume { value } => {
            p.set_volume(value).context("set_volume")?;
            let s = p.audio_state()?;
            print_or_json(as_json, &s, || format!("Volume now {}", s.master_volume))?;
        }
        SoundOp::Mute { on, off } => {
            let target = match (on, off) {
                (true, false) => true,
                (false, true) => false,
                _ => !p.audio_state()?.muted,
            };
            p.set_muted(target)?;
            print_or_json(as_json, &p.audio_state()?, || {
                format!("Mute: {}", if target { "on" } else { "off" })
            })?;
        }
        SoundOp::Output { id } => {
            p.select_sink(&id).with_context(|| format!("select_sink({id})"))?;
            let s = p.audio_state()?;
            print_or_json(as_json, &s, || {
                format!("Active output: {}", s.active_sink().map(|x| x.name.as_str()).unwrap_or("?"))
            })?;
        }
    }
    Ok(())
}

fn cmd_net(p: &MockProvider, op: NetOp, as_json: bool) -> Result<()> {
    match op {
        NetOp::State => {
            let s = p.wifi_state()?;
            if as_json {
                println!("{}", serde_json::to_string_pretty(&s)?);
            } else {
                println!(
                    "Wi-Fi:      {}",
                    if s.enabled { "on" } else { "off" }
                );
                match (&s.connected_ssid, &s.ip) {
                    (Some(ssid), Some(ip)) => println!("Connected:  {ssid}  ({ip})"),
                    _ => println!("Connected:  —"),
                }
                println!("\nNetworks ({}):", s.networks.len());
                println!("{:<22} {:>5} {:>4} {:<8} BSSID", "SSID", "dBm", "bars", "secured");
                for n in &s.networks {
                    println!(
                        "{:<22} {:>5} {:>4} {:<8} {}",
                        n.ssid,
                        n.signal_dbm,
                        n.bars(),
                        if n.secured { "yes" } else { "no" },
                        n.bssid
                    );
                }
            }
        }
        NetOp::Toggle { state } => {
            let on = state.as_bool();
            p.set_wifi_enabled(on)?;
            print_or_json(as_json, &p.wifi_state()?, || {
                format!("Wi-Fi {}", if on { "enabled" } else { "disabled" })
            })?;
        }
        NetOp::Connect { ssid, password } => {
            p.wifi_connect(&ssid, password.as_deref())
                .with_context(|| format!("connect({ssid})"))?;
            let s = p.wifi_state()?;
            print_or_json(as_json, &s, || {
                format!("Connected to {} ({})", ssid, s.ip.as_deref().unwrap_or("?"))
            })?;
        }
        NetOp::Disconnect => {
            p.wifi_disconnect()?;
            print_or_json(as_json, &p.wifi_state()?, || "Disconnected".into())?;
        }
    }
    Ok(())
}

fn cmd_bt(p: &MockProvider, op: BtOp, as_json: bool) -> Result<()> {
    match op {
        BtOp::State => {
            let s = p.bt_state()?;
            if as_json {
                println!("{}", serde_json::to_string_pretty(&s)?);
            } else {
                println!(
                    "Bluetooth:    {}",
                    if s.enabled { "on" } else { "off" }
                );
                println!("Discovering:  {}", if s.discovering { "yes" } else { "no" });
                println!("Device name:  {}\n", s.device_name);
                println!("{:<22} {:<12} {:<8} {:<10} {}", "Name", "Kind", "Paired", "Connected", "Battery");
                for d in &s.devices {
                    println!(
                        "{:<22} {:<12} {:<8} {:<10} {}",
                        d.name,
                        format!("{:?}", d.kind),
                        if d.paired { "yes" } else { "no" },
                        if d.connected { "yes" } else { "no" },
                        d.battery.map(|b| format!("{b}%")).unwrap_or_else(|| "—".into()),
                    );
                }
            }
        }
        BtOp::Toggle { state } => {
            let on = state.as_bool();
            p.set_bt_enabled(on)?;
            print_or_json(as_json, &p.bt_state()?, || {
                format!("Bluetooth {}", if on { "enabled" } else { "disabled" })
            })?;
        }
        BtOp::Scan { state } => {
            let on = state.as_bool();
            p.bt_scan(on)?;
            print_or_json(as_json, &p.bt_state()?, || {
                format!("Scan {}", if on { "started" } else { "stopped" })
            })?;
        }
        BtOp::Pair { mac } => {
            p.bt_pair(&mac).with_context(|| format!("pair({mac})"))?;
            print_or_json(as_json, &p.bt_state()?, || format!("Paired {mac}"))?;
        }
        BtOp::Connect { mac } => {
            p.bt_connect(&mac).with_context(|| format!("connect({mac})"))?;
            print_or_json(as_json, &p.bt_state()?, || format!("Connected {mac}"))?;
        }
        BtOp::Disconnect { mac } => {
            p.bt_disconnect(&mac).with_context(|| format!("disconnect({mac})"))?;
            print_or_json(as_json, &p.bt_state()?, || format!("Disconnected {mac}"))?;
        }
    }
    Ok(())
}

fn print_or_json<T: serde::Serialize, F: FnOnce() -> String>(
    as_json: bool,
    state: &T,
    pretty: F,
) -> Result<()> {
    if as_json {
        println!("{}", serde_json::to_string_pretty(state)?);
    } else {
        println!("{}", pretty());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// ls
// ---------------------------------------------------------------------------

async fn cmd_ls(path: String, archive: Option<PathBuf>, as_json: bool) -> Result<()> {
    let vfs = match archive {
        Some(a) => VfsPath::archive(a, PathBuf::from(path)),
        None    => VfsPath::native(if path.is_empty() { ".".to_string() } else { path }),
    };
    let entries = bacak_services::fs::read_dir(&vfs)
        .await
        .with_context(|| format!("read_dir({vfs:?})"))?;

    if as_json {
        println!("{}", serde_json::to_string_pretty(&entries)?);
        return Ok(());
    }

    for e in &entries {
        let kind = if e.is_dir { "DIR " } else { "    " };
        let size = if e.is_dir { String::from("    -") } else { format_size(e.size) };
        let mime = e.mime.as_deref().unwrap_or("");
        println!("{kind} {:>10}  {:<28}  {}", size, e.name, mime);
    }
    Ok(())
}

fn format_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "K", "M", "G", "T"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u + 1 < UNITS.len() {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} {}", UNITS[0])
    } else {
        format!("{v:>5.1} {}", UNITS[u])
    }
}

// ---------------------------------------------------------------------------
// archive
// ---------------------------------------------------------------------------

fn cmd_arc_list(archive: PathBuf, dir: String, as_json: bool) -> Result<()> {
    let entries = bacak_services::archive::list_dir(&archive, std::path::Path::new(&dir))
        .with_context(|| format!("list_dir({archive:?}!{dir})"))?;

    if as_json {
        println!("{}", serde_json::to_string_pretty(&entries)?);
        return Ok(());
    }
    for e in &entries {
        let kind = if e.is_dir { "DIR " } else { "    " };
        println!("{kind} {:>10}  {}", format_size(e.size), e.name);
    }
    Ok(())
}

fn cmd_arc_extract(archive: PathBuf, inside: String, to: PathBuf, as_json: bool) -> Result<()> {
    let n = bacak_services::archive::extract_entry(&archive, std::path::Path::new(&inside), &to)
        .with_context(|| format!("extract({archive:?}!{inside} -> {to:?})"))?;
    if as_json {
        println!("{}", serde_json::json!({"bytes": n, "to": to.display().to_string()}));
    } else {
        println!("wrote {n} bytes to {}", to.display());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// wm demo
// ---------------------------------------------------------------------------

fn cmd_wm_demo(as_json: bool) -> Result<()> {
    let monitor = Monitor { work_area: Rect::new(0.0, 0.0, 1440.0, 800.0) };
    let wm = WindowManager::new(monitor);

    let ff = wm.open("firefox", "Mozilla Firefox", Rect::new(180.0, 90.0,  880.0, 560.0));
    let fm = wm.open("files",   "Files",           Rect::new(300.0, 160.0, 760.0, 520.0));

    wm.snap(ff, SnapZone::Left)?;
    wm.snap(fm, SnapZone::Right)?;
    wm.focus(ff)?;

    let windows = wm.list_active();
    if as_json {
        println!("{}", serde_json::to_string_pretty(&windows)?);
    } else {
        println!("Active workspace: {}", wm.active_workspace());
        println!("{:<5} {:<10} {:<24} {:<32} state", "id", "z", "app", "geom");
        for w in windows {
            println!(
                "{:<5} {:<10} {:<24} ({:.0}, {:.0}, {:.0}, {:.0})       {:?}{}",
                w.id, w.z, w.app, w.geom.x, w.geom.y, w.geom.w, w.geom.h,
                w.state,
                if w.focused { "  *focused*" } else { "" }
            );
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// osk demo
// ---------------------------------------------------------------------------

fn cmd_osk_demo(as_json: bool) -> Result<()> {
    let osk = OskController::new(OskConfig::default());

    let field = FocusedField {
        rect: OskRect { x: 200.0, y: 720.0, w: 600.0, h: 32.0 },
        mode: InputMode::Url,
        has_hw_keyboard: false,
    };

    let geo = osk.open_for(field).expect("OSK should open without HW keyboard");
    osk.confirm_open().unwrap();
    let scroll = osk.scroll_offset_for(&field);

    if as_json {
        println!(
            "{}",
            serde_json::json!({
                "state": format!("{:?}", osk.state()),
                "enter_label": osk.enter_label(),
                "geometry": geo,
                "scroll_offset": scroll,
            })
        );
    } else {
        println!("OSK state:        {:?}", osk.state());
        println!("Enter key label:  {}", osk.enter_label());
        println!(
            "OSK panel:        ({:.0}, {:.0}, {:.0}, {:.0})",
            geo.panel.x, geo.panel.y, geo.panel.w, geo.panel.h
        );
        println!("Reserved top y:   {:.0}", geo.reserved_top_y);
        println!("Field overlaps → viewport scroll: {scroll:.1}px");
    }

    osk.close();
    osk.confirm_close().unwrap();
    Ok(())
}
