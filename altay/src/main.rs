// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! Altay — a sandboxed, touch-friendly Linux file manager.
//!
//! `main.rs` is the controller: it owns the application state and bridges the
//! Slint UI to the backend modules. All filesystem access goes through
//! [`security::Sandbox`], so the UI can never reach a forbidden path.

mod archive;
mod config;
mod desktop;
mod devices;
mod exo;
mod filesystem;
mod network;
mod permissions;
mod portal;
mod preview;
mod search;
mod security;
mod transfer;
mod trash;

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;

use filesystem::{Entry, SortKey};
use security::{RootKind, Sandbox};
use slint::{ModelRc, SharedString, VecModel};


slint::include_modules!();

thread_local! {
    /// Lets UI-thread callbacks (e.g. the hotplug handler dispatched via
    /// `upgrade_in_event_loop`) reach the app state without capturing the
    /// non-`Send` `Rc` across thread boundaries.
    static APP: RefCell<Option<Rc<RefCell<AppState>>>> = const { RefCell::new(None) };
}

/// Mutable application state, shared between UI callbacks.
struct AppState {
    sandbox: Sandbox,
    current: PathBuf,
    entries: Vec<Entry>,
    selected: HashSet<usize>,
    show_hidden: bool,
    sort: SortKey,
    selection_mode: bool,
    in_trash_view: bool,
    /// Last (entry index, time) tapped — used to distinguish tap from double-tap.
    last_click: Option<(usize, std::time::Instant)>,
    /// Copy/cut clipboard: the staged paths and whether paste should move them.
    clipboard: Option<(Vec<PathBuf>, transfer::Kind)>,
    transfers: transfer::Manager,
    /// How many transfers had finished at the last poll (to trigger a reload).
    last_terminal: usize,
    /// Live filename index for the home tree (fast search).
    index: Option<search::Index>,
    /// Path currently shown in the preview pane (target of chmod actions).
    preview_path: Option<PathBuf>,
    /// A chmod that was refused for lack of ownership, awaiting elevation.
    pending_chmod: Option<(PathBuf, u32)>,
    /// File currently shown in the "Open With" dialog.
    open_with_path: Option<PathBuf>,
    /// If Some, we're in archive-browser mode for this archive file.
    archive_path: Option<PathBuf>,
}

impl AppState {
    fn new(sandbox: Sandbox) -> Self {
        let current = sandbox.home().into_path_buf();
        AppState {
            sandbox,
            current,
            entries: Vec::new(),
            selected: HashSet::new(),
            show_hidden: false,
            sort: SortKey::Name,
            selection_mode: false,
            in_trash_view: false,
            last_click: None,
            clipboard: None,
            transfers: transfer::Manager::new(),
            last_terminal: 0,
            index: None,
            preview_path: None,
            pending_chmod: None,
            open_with_path: None,
            archive_path: None,
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Quiet zbus's very chatty socket-level INFO logs by default.
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info,zbus=warn,tracing=warn"),
    )
    .init();

    let sandbox = Sandbox::from_env();
    // Start building the home filename index in the background for fast search.
    let index = search::Index::build(&sandbox, sandbox.home().into_path_buf());
    let state = Rc::new(RefCell::new(AppState::new(sandbox)));
    state.borrow_mut().index = index;

    // Apply persisted preferences.
    let cfg = config::load();
    state.borrow_mut().show_hidden = cfg.prefs.show_hidden;

    APP.with(|a| *a.borrow_mut() = Some(state.clone()));

    let _ = slint::set_xdg_app_id("altay");
    let ui = MainWindow::new()?;
    if let Some(img) = load_file_icon("folder") {
        ui.set_new_folder_icon(img);
    }
    ui.set_view_mode(0);
    ui.set_grid_scale(cfg.prefs.grid_scale.clamp(0.6, 2.2));
    ui.set_show_hidden_pref(cfg.prefs.show_hidden);
    ui.global::<Theme>().set_dark(cfg.prefs.dark);
    let lang = if cfg.prefs.lang.is_empty() { detect_locale() } else { cfg.prefs.lang.clone() };
    apply_language(&ui, &lang);
    ui.set_lang(SharedString::from(lang));
    populate_roots(&ui, &state.borrow());
    sync_connections(&ui);

    // Defer the blocking udisks2 D-Bus call so the window appears immediately.
    // volumes are populated ~150 ms after startup instead of blocking main thread.
    {
        let weak = ui.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(150), move || {
            if let Some(ui) = weak.upgrade() {
                sync_volumes(&ui);
            }
        });
    }

    // Live device hotplug: rebuild the sandbox roots (which pick up new mount
    // points) and the sidebar whenever udisks2 reports a change.
    {
        let weak = ui.as_weak();
        devices::monitor(move || {
            let _ = weak.upgrade_in_event_loop(|ui| reload_roots(&ui));
        });
    }

    // ---- Wire callbacks -----------------------------------------------------
    {
        let ui_w = ui.as_weak();
        let st = state.clone();
        ui.on_navigate(move |path| {
            if let Some(ui) = ui_w.upgrade() {
                navigate(&ui, &st, path.as_str());
            }
        });
    }
    {
        let ui_w = ui.as_weak();
        let st = state.clone();
        ui.on_open_entry(move |idx| {
            if let Some(ui) = ui_w.upgrade() {
                open_entry(&ui, &st, idx as usize);
            }
        });
    }
    {
        let ui_w = ui.as_weak();
        let st = state.clone();
        ui.on_toggle_entry(move |idx| {
            if let Some(ui) = ui_w.upgrade() {
                {
                    let mut s = st.borrow_mut();
                    let i = idx as usize;
                    if s.selected.contains(&i) {
                        s.selected.remove(&i);
                    } else {
                        s.selected.insert(i);
                    }
                }
                refresh_view(&ui, &st);
            }
        });
    }
    {
        let ui_w = ui.as_weak();
        let st = state.clone();
        ui.on_go_up(move || {
            if let Some(ui) = ui_w.upgrade() {
                // If in archive-browser mode, exit back to the folder containing the archive.
                let arc = st.borrow().archive_path.clone();
                if let Some(arc_path) = arc {
                    st.borrow_mut().archive_path = None;
                    ui.set_in_archive(false);
                    let parent = arc_path.parent().map(PathBuf::from).unwrap_or_else(|| st.borrow().current.clone());
                    navigate(&ui, &st, &parent.to_string_lossy());
                } else {
                    let parent = st.borrow().current.parent().map(PathBuf::from);
                    if let Some(p) = parent {
                        navigate(&ui, &st, &p.to_string_lossy());
                    }
                }
            }
        });
    }
    {
        let ui_w = ui.as_weak();
        let st = state.clone();
        ui.on_go_down(move || {
            let Some(ui) = ui_w.upgrade() else { return };
            // Enter the first selected directory, or first directory in the listing.
            let target = {
                let s = st.borrow();
                let selected_dir = s.selected.iter()
                    .filter_map(|&i| s.entries.get(i))
                    .find(|e| e.is_dir)
                    .map(|e| e.path.clone());
                selected_dir.or_else(|| {
                    s.entries.iter().find(|e| e.is_dir).map(|e| e.path.clone())
                })
            };
            if let Some(path) = target {
                navigate(&ui, &st, &path.to_string_lossy());
            }
        });
    }
    {
        let ui_w = ui.as_weak();
        let st = state.clone();
        ui.on_go_home(move || {
            if let Some(ui) = ui_w.upgrade() {
                let home = st.borrow().sandbox.home().into_path_buf();
                navigate(&ui, &st, &home.to_string_lossy());
            }
        });
    }
    {
        let ui_w = ui.as_weak();
        let st = state.clone();
        ui.on_refresh(move || {
            if let Some(ui) = ui_w.upgrade() {
                reload_roots(&ui); // pick up newly-mounted devices manually too
                reload(&ui, &st);
            }
        });
    }
    {
        let ui_w = ui.as_weak();
        let st = state.clone();
        ui.on_do_search(move |term| {
            if let Some(ui) = ui_w.upgrade() {
                run_search(&ui, &st, term.as_str());
            }
        });
    }
    {
        let ui_w = ui.as_weak();
        let st = state.clone();
        ui.on_new_folder(move || {
            if let Some(ui) = ui_w.upgrade() {
                make_new_folder(&ui, &st);
            }
        });
    }
    {
        let ui_w = ui.as_weak();
        let st = state.clone();
        ui.on_trash_selected(move || {
            if let Some(ui) = ui_w.upgrade() {
                trash_selected(&ui, &st);
            }
        });
    }
    {
        let ui_w = ui.as_weak();
        let st = state.clone();
        ui.on_compress_selected(move || {
            if let Some(ui) = ui_w.upgrade() {
                compress_selected(&ui, &st);
            }
        });
    }
    {
        let ui_w = ui.as_weak();
        let st = state.clone();
        ui.on_restore_selected(move || {
            if let Some(ui) = ui_w.upgrade() {
                restore_selected(&ui, &st);
            }
        });
    }
    {
        let ui_w = ui.as_weak();
        let st = state.clone();
        ui.on_empty_trash(move || {
            if let Some(ui) = ui_w.upgrade() {
                empty_trash(&ui, &st);
            }
        });
    }
    {
        let ui_w = ui.as_weak();
        let st = state.clone();
        ui.on_extract_selected(move || {
            if let Some(ui) = ui_w.upgrade() {
                extract_selected(&ui, &st);
            }
        });
    }
    {
        let ui_w = ui.as_weak();
        let st = state.clone();
        ui.on_extract_archive(move || {
            if let Some(ui) = ui_w.upgrade() {
                extract_from_archive_view(&ui, &st);
            }
        });
    }
    {
        let st = state.clone();
        let ui_w = ui.as_weak();
        ui.on_copy_selected(move || {
            if let Some(ui) = ui_w.upgrade() {
                stage_clipboard(&ui, &st, transfer::Kind::Copy);
            }
        });
    }
    {
        let st = state.clone();
        let ui_w = ui.as_weak();
        ui.on_cut_selected(move || {
            if let Some(ui) = ui_w.upgrade() {
                stage_clipboard(&ui, &st, transfer::Kind::Move);
            }
        });
    }
    {
        let st = state.clone();
        let ui_w = ui.as_weak();
        ui.on_paste(move || {
            if let Some(ui) = ui_w.upgrade() {
                paste(&ui, &st);
            }
        });
    }
    {
        let st = state.clone();
        ui.on_pause_transfer(move |id| st.borrow().transfers.pause(id as u64));
    }
    {
        let st = state.clone();
        ui.on_resume_transfer(move |id| st.borrow().transfers.resume(id as u64));
    }
    {
        let st = state.clone();
        ui.on_cancel_transfer(move |id| st.borrow().transfers.cancel(id as u64));
    }
    {
        let st = state.clone();
        let ui_w = ui.as_weak();
        ui.on_clear_transfers(move || {
            st.borrow().transfers.clear_finished();
            if let Some(ui) = ui_w.upgrade() {
                sync_transfers(&ui, &st);
            }
        });
    }
    {
        let ui_w = ui.as_weak();
        ui.on_toggle_preview(move || {
            if let Some(ui) = ui_w.upgrade() {
                ui.set_show_preview(!ui.get_show_preview());
            }
        });
    }
    {
        let ui_w = ui.as_weak();
        let st = state.clone();
        ui.on_chmod_toggle_exec(move || {
            if let Some(ui) = ui_w.upgrade() {
                chmod_toggle_exec(&ui, &st);
            }
        });
    }
    {
        let ui_w = ui.as_weak();
        let st = state.clone();
        ui.on_chmod_as_admin(move || {
            if let Some(ui) = ui_w.upgrade() {
                chmod_as_admin(&ui, &st);
            }
        });
    }
    {
        let ui_w = ui.as_weak();
        let st = state.clone();
        ui.on_import_files(move || {
            if let Some(ui) = ui_w.upgrade() {
                import_files(&ui, &st);
            }
        });
    }
    // Context-menu actions.
    macro_rules! ctx_handler {
        ($setter:ident, $func:ident) => {{
            let ui_w = ui.as_weak();
            let st = state.clone();
            ui.$setter(move || {
                if let Some(ui) = ui_w.upgrade() {
                    $func(&ui, &st);
                }
            });
        }};
    }
    ctx_handler!(on_ctx_open, ctx_open);
    ctx_handler!(on_ctx_copy, ctx_copy);
    ctx_handler!(on_ctx_cut, ctx_cut);
    ctx_handler!(on_ctx_rename, ctx_rename);
    ctx_handler!(on_ctx_compress, ctx_compress);
    ctx_handler!(on_ctx_extract, ctx_extract);
    ctx_handler!(on_ctx_trash, ctx_trash);
    ctx_handler!(on_do_rename, do_rename);
    ctx_handler!(on_open_with_dialog, open_with_dialog);
    ctx_handler!(on_key_rename, key_rename);
    {
        let ui_w = ui.as_weak();
        let st = state.clone();
        ui.on_rename_selected(move || {
            if let Some(ui) = ui_w.upgrade() {
                let first = st.borrow().selected.iter().min().copied();
                if let Some(i) = first {
                    ui.global::<ContextState>().set_index(i as i32);
                    ctx_rename(&ui, &st);
                }
            }
        });
    }
    {
        let ui_w = ui.as_weak();
        let st = state.clone();
        ui.on_open_with_selected(move || {
            if let Some(ui) = ui_w.upgrade() {
                let first = st.borrow().selected.iter().min().copied();
                if let Some(i) = first {
                    ui.global::<ContextState>().set_index(i as i32);
                    open_with_dialog(&ui, &st);
                }
            }
        });
    }
    {
        let ui_w = ui.as_weak();
        let st = state.clone();
        ui.on_ow_pick(move |id| {
            if let Some(ui) = ui_w.upgrade() {
                ow_pick(&ui, &st, id.as_str());
            }
        });
    }
    ctx_handler!(on_select_all, select_all);
    ctx_handler!(on_clear_selection, clear_selection);
    {
        // Persist the grid zoom level when it changes.
        ui.on_grid_scale_changed(move |scale| {
            let mut cfg = config::load();
            cfg.prefs.grid_scale = scale;
            let _ = config::save(&cfg);
        });
    }
    {
        let ui_w = ui.as_weak();
        ui.on_open_connect_dialog(move || {
            if let Some(ui) = ui_w.upgrade() {
                open_connect_dialog(&ui);
            }
        });
    }
    {
        let ui_w = ui.as_weak();
        ui.on_cancel_connect(move || {
            if let Some(ui) = ui_w.upgrade() {
                ui.set_show_connect(false);
                ui.set_connect_password(SharedString::from(""));
            }
        });
    }
    {
        let ui_w = ui.as_weak();
        let st = state.clone();
        ui.on_do_connect(move || {
            if let Some(ui) = ui_w.upgrade() {
                do_connect(&ui, &st);
            }
        });
    }
    {
        let ui_w = ui.as_weak();
        let st = state.clone();
        ui.on_open_connection(move |uri| {
            if let Some(ui) = ui_w.upgrade() {
                open_connection(&ui, &st, uri.as_str());
            }
        });
    }
    {
        let ui_w = ui.as_weak();
        let st = state.clone();
        ui.on_mount_volume(move |obj| {
            if let Some(ui) = ui_w.upgrade() {
                ui.set_status_text(sx(&ui, "Mounting…", "Bağlanıyor…", "Montando…"));
                match devices::mount(obj.as_str()) {
                    Ok(mp) => {
                        reload_roots(&ui); // sandbox now trusts the new mount point
                        navigate(&ui, &st, &mp.to_string_lossy());
                    }
                    Err(e) => ui.set_status_text(sx(&ui, format!("Mount failed: {e}"), format!("Bağlama başarısız: {e}"), format!("Error al montar: {e}"))),
                }
            }
        });
    }
    {
        let ui_w = ui.as_weak();
        ui.on_unmount_volume(move |obj| {
            if let Some(ui) = ui_w.upgrade() {
                match devices::unmount(obj.as_str()) {
                    Ok(()) => {
                        reload_roots(&ui);
                        ui.set_status_text(sx(&ui, "Ejected", "Çıkarıldı", "Expulsado"));
                    }
                    Err(e) => ui.set_status_text(sx(&ui, format!("Eject failed: {e}"), format!("Çıkarma başarısız: {e}"), format!("Error al expulsar: {e}"))),
                }
            }
        });
    }
    {
        let ui_w = ui.as_weak();
        let st = state.clone();
        ui.on_perform_drop(move || {
            if let Some(ui) = ui_w.upgrade() {
                let ds = ui.global::<DragState>();
                let source_index = ds.get_source_index();
                let target = ds.get_target_path().to_string();
                perform_drop(&ui, &st, source_index, &target);
            }
        });
    }
    {
        let ui_w = ui.as_weak();
        ui.on_set_view(move |mode| {
            if let Some(ui) = ui_w.upgrade() {
                ui.set_view_mode(mode);
                let mut cfg = config::load();
                cfg.prefs.view_mode = mode;
                let _ = config::save(&cfg);
            }
        });
    }
    {
        let ui_w = ui.as_weak();
        let st = state.clone();
        ui.on_set_show_hidden(move |show| {
            state_set_show_hidden(&st, show);
            let mut cfg = config::load();
            cfg.prefs.show_hidden = show;
            let _ = config::save(&cfg);
            if let Some(ui) = ui_w.upgrade() {
                reload(&ui, &st);
            }
        });
    }
    {
        ui.on_set_dark(move |dark| {
            let mut cfg = config::load();
            cfg.prefs.dark = dark;
            let _ = config::save(&cfg);
        });
    }
    {
        let ui_w = ui.as_weak();
        ui.on_set_language(move |lang| {
            if let Some(ui) = ui_w.upgrade() {
                apply_language(&ui, lang.as_str());
                ui.set_lang(lang.clone());
                let mut cfg = config::load();
                cfg.prefs.lang = lang.to_string();
                let _ = config::save(&cfg);
            }
        });
    }
    {
        let ui_w = ui.as_weak();
        let st = state.clone();
        ui.on_toggle_selection_mode(move || {
            if let Some(ui) = ui_w.upgrade() {
                let on = {
                    let mut s = st.borrow_mut();
                    s.selection_mode = !s.selection_mode;
                    if !s.selection_mode {
                        s.selected.clear();
                    }
                    s.selection_mode
                };
                ui.set_selection_mode(on);
                refresh_view(&ui, &st);
            }
        });
    }

    // Poll the transfer manager so the queue panel stays live and finished
    // pastes refresh the listing. The timer must outlive `run()`.
    let poll_timer = slint::Timer::default();
    {
        let ui_w = ui.as_weak();
        let st = state.clone();
        poll_timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(250),
            move || {
                if let Some(ui) = ui_w.upgrade() {
                    // Only touch the model while transfers exist (cheap otherwise).
                    let has_any = !st.borrow().transfers.snapshots().is_empty();
                    if has_any {
                        sync_transfers(&ui, &st);
                    }
                }
            },
        );
    }

    // ---- Samsung-style new callbacks ----------------------------------------
    {
        let ui_w = ui.as_weak();
        let st = state.clone();
        ui.on_navigate_category(move |cat| {
            if let Some(ui) = ui_w.upgrade() {
                let home = st.borrow().sandbox.home().into_path_buf();
                let dir = xdg_dir_for_category(cat.as_str(), &home);
                navigate(&ui, &st, &dir.to_string_lossy());
            }
        });
    }
    {
        let ui_w = ui.as_weak();
        let st = state.clone();
        ui.on_show_recent(move || {
            if let Some(ui) = ui_w.upgrade() {
                // Navigate home; future work: filter by mtime descending
                let home = st.borrow().sandbox.home().into_path_buf();
                navigate(&ui, &st, &home.to_string_lossy());
            }
        });
    }
    {
        let ui_w = ui.as_weak();
        let st = state.clone();
        ui.on_sort_by_column(move |col| {
            if let Some(ui) = ui_w.upgrade() {
                let key = match col.as_str() {
                    "date" => SortKey::Modified,
                    "type" => SortKey::Kind,
                    "size" => SortKey::Size,
                    _      => SortKey::Name,
                };
                st.borrow_mut().sort = key;
                reload(&ui, &st);
            }
        });
    }

    // Initial disk usage.
    {
        let home = state.borrow().sandbox.home().into_path_buf();
        ui.set_disk_usage(build_disk_usage(&home));
    }

    // Initial load.
    reload(&ui, &state);
    ui.run()?;
    Ok(())
}

// ---- Controller actions -----------------------------------------------------

fn navigate(ui: &MainWindow, state: &Rc<RefCell<AppState>>, target: &str) {
    if target == "trash://" {
        load_trash(ui, state);
        return;
    }
    // A network URI in the path bar (smb://, sftp://, …) triggers a gvfs mount.
    if let Some((scheme, _)) = target.split_once("://") {
        if network::Protocol::from_scheme(scheme).is_some() {
            mount_uri(ui, state, target);
            return;
        }
    }
    let sandbox_result = state.borrow().sandbox.resolve(target);
    match sandbox_result {
        Ok(safe) => {
            {
                let mut s = state.borrow_mut();
                s.current = safe.into_path_buf();
                s.in_trash_view = false;
                s.archive_path = None;
                s.selected.clear();
            }
            reload(ui, state);
        }
        Err(e) => {
            ui.set_status_text(sx(ui, format!("Access denied: {e}"), format!("Erişim reddedildi: {e}"), format!("Acceso denegado: {e}")));
        }
    }
}

/// Mount a network share from a URI typed in the path bar. Uses a keyring
/// password if one is saved; otherwise opens the credential dialog on failure.
fn mount_uri(ui: &MainWindow, state: &Rc<RefCell<AppState>>, uri: &str) {
    let Some(conn) = network::Connection::from_uri(uri) else {
        ui.set_status_text(sx(ui, format!("Invalid network URI: {uri}"), format!("Geçersiz ağ adresi: {uri}"), format!("URI de red no válida: {uri}")));
        return;
    };
    ui.set_status_text(sx(ui, format!("Connecting to {}…", conn.host), format!("{} sunucusuna bağlanılıyor…", conn.host), format!("Conectando a {}…", conn.host)));
    let secret = network::load_secret(&conn.uri());
    match network::mount(&conn, secret.as_deref()) {
        Ok(local) => {
            persist_connection(&conn);
            reload_roots(ui);
            sync_connections(ui);
            navigate(ui, state, &local.to_string_lossy());
        }
        Err(_) => open_connect_prefilled(ui, &conn.uri(), conn.username.as_deref()),
    }
}

/// Import files chosen through the file-chooser portal into the current folder.
/// The sources are portal-authorized (they may live outside the sandbox); the
/// destination is the current sandboxed directory, validated as usual.
fn import_files(ui: &MainWindow, state: &Rc<RefCell<AppState>>) {
    let files = portal::pick_files();
    if files.is_empty() {
        ui.set_status_text(sx(ui, "Nothing imported", "İçe aktarılan yok", "No se importó nada"));
        return;
    }
    let dir = state.borrow().current.clone();
    let mut ok = 0usize;
    let mut last_err = String::new();
    for src in &files {
        let Some(name) = src.file_name() else { continue };
        let stem = std::path::Path::new(name).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
        let ext = std::path::Path::new(name).extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
        let dst = unique_path(&dir, &stem, &ext);
        // Validate only the destination; the source is authorized by the portal.
        match state.borrow().sandbox.resolve_for_create(&dst) {
            Ok(safe) => match copy_external(src, safe.as_path()) {
                Ok(()) => ok += 1,
                Err(e) => last_err = e.to_string(),
            },
            Err(e) => last_err = e.to_string(),
        }
    }
    if last_err.is_empty() {
        ui.set_status_text(sx(ui, format!("Imported {ok} item(s)"), format!("{ok} öğe içe aktarıldı"), format!("{ok} elemento(s) importado(s)")));
    } else {
        ui.set_status_text(sx(ui, format!("Imported {ok}/{} — {last_err}", files.len()), format!("{ok}/{} içe aktarıldı — {last_err}", files.len()), format!("Importados {ok}/{} — {last_err}", files.len())));
    }
    reload(ui, state);
}

/// Recursively copy a portal-authorized source (possibly outside the sandbox)
/// to a sandbox-validated destination.
fn copy_external(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(src)?;
    if meta.is_dir() {
        std::fs::create_dir_all(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            copy_external(&entry.path(), &dst.join(entry.file_name()))?;
        }
    } else {
        std::fs::copy(src, dst)?;
    }
    Ok(())
}

// ---- Context-menu actions ---------------------------------------------------

/// The entry index the context menu targets (read from the Slint global).
fn ctx_target(ui: &MainWindow) -> Option<usize> {
    let i = ui.global::<ContextState>().get_index();
    (i >= 0).then_some(i as usize)
}

/// Make `idx` the sole selection (context actions reuse the selection-based ops).
fn select_one(state: &Rc<RefCell<AppState>>, idx: usize) {
    let mut s = state.borrow_mut();
    s.selected.clear();
    s.selected.insert(idx);
}

fn ctx_open(ui: &MainWindow, state: &Rc<RefCell<AppState>>) {
    if let Some(i) = ctx_target(ui) {
        open_entry_real(ui, state, i);
    }
}

fn ctx_copy(ui: &MainWindow, state: &Rc<RefCell<AppState>>) {
    if let Some(i) = ctx_target(ui) {
        select_one(state, i);
        stage_clipboard(ui, state, transfer::Kind::Copy);
    }
}

fn ctx_cut(ui: &MainWindow, state: &Rc<RefCell<AppState>>) {
    if let Some(i) = ctx_target(ui) {
        select_one(state, i);
        stage_clipboard(ui, state, transfer::Kind::Move);
    }
}

fn ctx_compress(ui: &MainWindow, state: &Rc<RefCell<AppState>>) {
    if let Some(i) = ctx_target(ui) {
        select_one(state, i);
        compress_selected(ui, state);
    }
}

fn ctx_extract(ui: &MainWindow, state: &Rc<RefCell<AppState>>) {
    if let Some(i) = ctx_target(ui) {
        select_one(state, i);
        extract_selected(ui, state);
    }
}

fn ctx_trash(ui: &MainWindow, state: &Rc<RefCell<AppState>>) {
    if let Some(i) = ctx_target(ui) {
        select_one(state, i);
        trash_selected(ui, state);
    }
}

/// Populate and show the "Open With" dialog for the context-menu target.
fn open_with_dialog(ui: &MainWindow, state: &Rc<RefCell<AppState>>) {
    let Some(i) = ctx_target(ui) else { return };
    let (path, name) = match state.borrow().entries.get(i) {
        Some(e) => (e.path.clone(), e.name.clone()),
        None => return,
    };
    let apps = exo::apps_for(&state.borrow().sandbox, &path).unwrap_or_default();
    if apps.is_empty() {
        ui.set_status_text(sx(
            ui,
            "No applications found for this file",
            "Bu dosya için uygulama bulunamadı",
            "No se encontraron aplicaciones para este archivo",
        ));
        return;
    }
    let default_id = exo::mime_type(&path).and_then(|m| exo::default_app(&m));
    let models: Vec<AppModel> = apps
        .iter()
        .map(|a| {
            let icon = exo::icon_path(a).and_then(|p| slint::Image::load_from_path(&p).ok());
            AppModel {
                id: SharedString::from(a.id.clone()),
                name: SharedString::from(a.name.clone()),
                is_default: default_id.as_deref() == Some(a.id.as_str()),
                has_icon: icon.is_some(),
                icon: icon.unwrap_or_default(),
            }
        })
        .collect();
    state.borrow_mut().open_with_path = Some(path);
    ui.set_open_with_apps(ModelRc::new(VecModel::from(models)));
    ui.set_ow_title(sx(
        ui,
        format!("Open “{name}” with"),
        format!("“{name}” şununla aç"),
        format!("Abrir “{name}” con"),
    ));
    ui.set_ow_set_default(false);
    ui.set_show_open_with(true);
}

/// The user picked an application in the "Open With" dialog.
fn ow_pick(ui: &MainWindow, state: &Rc<RefCell<AppState>>, app_id: &str) {
    let Some(path) = state.borrow().open_with_path.clone() else { return };
    if ui.get_ow_set_default() {
        if let Some(mime) = exo::mime_type(&path) {
            let _ = exo::set_default(&mime, app_id);
        }
    }
    let res = exo::open_with(&state.borrow().sandbox, &path, app_id);
    ui.set_show_open_with(false);
    match res {
        Ok(()) => ui.set_status_text(sx(
            ui,
            format!("Opening with {app_id}…"),
            format!("{app_id} ile açılıyor…"),
            format!("Abriendo con {app_id}…"),
        )),
        Err(e) => ui.set_status_text(sx(
            ui,
            format!("Open failed: {e}"),
            format!("Açma başarısız: {e}"),
            format!("Error al abrir: {e}"),
        )),
    }
}

/// Open the rename dialog prefilled with the target's current name.
fn ctx_rename(ui: &MainWindow, state: &Rc<RefCell<AppState>>) {
    if let Some(i) = ctx_target(ui) {
        if let Some(e) = state.borrow().entries.get(i) {
            ui.set_rename_text(SharedString::from(e.name.clone()));
            ui.set_show_rename(true);
        }
    }
}

/// Apply the rename from the dialog (uses `filesystem::rename`, sandbox-checked).
fn do_rename(ui: &MainWindow, state: &Rc<RefCell<AppState>>) {
    let Some(i) = ctx_target(ui) else {
        ui.set_show_rename(false);
        return;
    };
    let new_name = ui.get_rename_text().to_string();
    let Some(path) = state.borrow().entries.get(i).map(|e| e.path.clone()) else {
        ui.set_show_rename(false);
        return;
    };
    let res = filesystem::rename(&state.borrow().sandbox, &path, &new_name);
    match res {
        Ok(_) => {
            ui.set_show_rename(false);
            reload(ui, state);
            ui.set_status_text(sx(ui, format!("Renamed to {new_name}"), format!("{new_name} olarak yeniden adlandırıldı"), format!("Renombrado a {new_name}")));
        }
        Err(e) => ui.set_status_text(sx(ui, format!("Rename failed: {e}"), format!("Yeniden adlandırma başarısız: {e}"), format!("Error al renombrar: {e}"))),
    }
}

/// Toggle whether dotfiles are listed.
fn state_set_show_hidden(state: &Rc<RefCell<AppState>>, show: bool) {
    state.borrow_mut().show_hidden = show;
}

// ---- Keyboard-shortcut helpers ----------------------------------------------

/// F2: rename the first selected entry (reuses the context-menu rename dialog).
fn key_rename(ui: &MainWindow, state: &Rc<RefCell<AppState>>) {
    let first = state.borrow().selected.iter().min().copied();
    if let Some(i) = first {
        ui.global::<ContextState>().set_index(i as i32);
        ctx_rename(ui, state);
    } else {
        ui.set_status_text(sx(ui, "Select an item to rename (F2)", "Yeniden adlandırmak için bir öğe seçin (F2)", "Seleccione un elemento para renombrar (F2)"));
    }
}

/// Ctrl+A: select every entry in the current listing.
fn select_all(ui: &MainWindow, state: &Rc<RefCell<AppState>>) {
    {
        let mut s = state.borrow_mut();
        let n = s.entries.len();
        s.selected = (0..n).collect();
    }
    refresh_view(ui, state);
}

/// Esc: clear the current selection.
fn clear_selection(ui: &MainWindow, state: &Rc<RefCell<AppState>>) {
    state.borrow_mut().selected.clear();
    refresh_view(ui, state);
}

/// Convert a saved connection record into a live [`network::Connection`].
fn saved_to_conn(s: &config::SavedConnection) -> Option<network::Connection> {
    Some(network::Connection {
        name: s.name.clone(),
        protocol: s.protocol()?,
        host: s.host.clone(),
        port: s.port,
        share: s.share.clone(),
        username: s.username.clone(),
        keyring_ref: None,
    })
}

/// Refresh the sidebar's saved-connection list from the config.
fn sync_connections(ui: &MainWindow) {
    let cfg = config::load();
    let models: Vec<ConnModel> = cfg
        .connections
        .iter()
        .filter_map(|s| {
            let conn = saved_to_conn(s)?;
            let name = if s.name.is_empty() { conn.host.clone() } else { s.name.clone() };
            Some(ConnModel {
                name: SharedString::from(name),
                uri: SharedString::from(conn.uri()),
            })
        })
        .collect();
    ui.set_connections(ModelRc::new(VecModel::from(models)));
}

/// Open the connect dialog with empty fields.
fn open_connect_dialog(ui: &MainWindow) {
    ui.set_connect_address(SharedString::from(""));
    ui.set_connect_user(SharedString::from(""));
    ui.set_connect_password(SharedString::from(""));
    ui.set_connect_error(SharedString::from(""));
    ui.set_connect_save(true);
    ui.set_show_connect(true);
}

/// Open the connect dialog prefilled for a known share (e.g. saved connection
/// whose stored password failed or is missing).
fn open_connect_prefilled(ui: &MainWindow, uri: &str, user: Option<&str>) {
    ui.set_connect_address(SharedString::from(uri));
    ui.set_connect_user(SharedString::from(user.unwrap_or_default()));
    ui.set_connect_password(SharedString::from(""));
    ui.set_connect_error(SharedString::from("Password required"));
    ui.set_connect_save(true);
    ui.set_show_connect(true);
}

/// Attempt to connect using the dialog's fields, saving the password to the
/// keyring when requested.
fn do_connect(ui: &MainWindow, state: &Rc<RefCell<AppState>>) {
    let address = ui.get_connect_address().to_string();
    let user = ui.get_connect_user().to_string();
    let password = ui.get_connect_password().to_string();
    let save = ui.get_connect_save();

    let Some(mut conn) = network::Connection::from_uri(&address) else {
        ui.set_connect_error(SharedString::from("Invalid address — use smb://host/share"));
        return;
    };
    if !user.is_empty() {
        conn.username = Some(user);
    }
    let pw = if password.is_empty() { None } else { Some(password.as_str()) };
    match network::mount(&conn, pw) {
        Ok(local) => {
            if save && !password.is_empty() {
                network::store_secret(&conn.uri(), &password);
            }
            persist_connection(&conn);
            ui.set_connect_password(SharedString::from(""));
            ui.set_show_connect(false);
            reload_roots(ui);
            sync_connections(ui);
            navigate(ui, state, &local.to_string_lossy());
        }
        Err(e) => ui.set_connect_error(SharedString::from(format!("{e}"))),
    }
}

/// Mount a saved connection, using its keyring password if present.
fn open_connection(ui: &MainWindow, state: &Rc<RefCell<AppState>>, uri: &str) {
    let Some(conn) = network::Connection::from_uri(uri) else { return };
    let secret = network::load_secret(uri);
    ui.set_status_text(sx(ui, format!("Connecting to {}…", conn.host), format!("{} sunucusuna bağlanılıyor…", conn.host), format!("Conectando a {}…", conn.host)));
    match network::mount(&conn, secret.as_deref()) {
        Ok(local) => {
            reload_roots(ui);
            navigate(ui, state, &local.to_string_lossy());
        }
        Err(_) => open_connect_prefilled(ui, uri, conn.username.as_deref()),
    }
}

/// Save a successfully-mounted connection to the config (deduplicated by URI).
fn persist_connection(conn: &network::Connection) {
    let mut cfg = config::load();
    let uri = conn.uri();
    if cfg.connections.iter().any(|c| {
        c.host == conn.host && c.share == conn.share && c.protocol == conn.protocol.scheme()
    }) {
        return;
    }
    cfg.connections.push(config::SavedConnection {
        name: conn.name.clone(),
        protocol: conn.protocol.scheme().to_string(),
        host: conn.host.clone(),
        port: conn.port,
        share: conn.share.clone(),
        username: conn.username.clone(),
    });
    if let Err(e) = config::save(&cfg) {
        log::warn!("could not save connection {uri}: {e}");
    }
}

/// Tap dispatcher: a second tap on the same item within 400 ms opens it
/// (Android-style); a single tap selects it and updates the preview pane.
fn open_entry(ui: &MainWindow, state: &Rc<RefCell<AppState>>, idx: usize) {
    use std::time::{Duration, Instant};
    let now = Instant::now();
    let is_double = {
        let mut s = state.borrow_mut();
        let dbl = matches!(s.last_click, Some((i, t)) if i == idx && now.duration_since(t) < Duration::from_millis(400));
        s.last_click = Some((idx, now));
        dbl
    };
    if is_double {
        open_entry_real(ui, state, idx);
    } else {
        select_and_preview(ui, state, idx);
    }
}

/// Single-tap: make `idx` the sole selection and refresh the preview pane.
fn select_and_preview(ui: &MainWindow, state: &Rc<RefCell<AppState>>, idx: usize) {
    {
        let mut s = state.borrow_mut();
        s.selected.clear();
        s.selected.insert(idx);
    }
    update_preview(ui, state, idx);
    refresh_view(ui, state);
}

/// Load preview content for `idx` and push it into the UI preview properties.
fn update_preview(ui: &MainWindow, state: &Rc<RefCell<AppState>>, idx: usize) {
    let entry = match state.borrow().entries.get(idx).cloned() {
        Some(e) => e,
        None => return,
    };
    ui.set_preview_title(SharedString::from(entry.name.clone()));
    // Remember the previewed path and refresh its permission display.
    {
        let mut s = state.borrow_mut();
        s.preview_path = Some(entry.path.clone());
        s.pending_chmod = None;
    }
    ui.set_preview_needs_elevation(false);
    // Meta bilgisi: boyut + değiştirilme tarihi
    let meta_size = if entry.is_dir {
        let n = filesystem::child_count(&entry.path);
        sx(ui, format!("{n} items"), format!("{n} öğe"), format!("{n} elementos")).to_string()
    } else {
        humansize::format_size(entry.size, humansize::DECIMAL)
    };
    let meta_date = format_time(entry.modified);
    ui.set_preview_meta(SharedString::from(format!("{meta_size}\n{meta_date}")));
    refresh_preview_perms(ui, state, &entry.path);
    let content = preview::load(&state.borrow().sandbox, &entry);
    match content {
        Ok(preview::Content::Image(path)) => {
            match slint::Image::load_from_path(&path) {
                Ok(img) => {
                    ui.set_preview_image(img);
                    ui.set_preview_is_image(true);
                }
                Err(_) => {
                    ui.set_preview_is_image(false);
                    ui.set_preview_text(SharedString::from("(could not decode image)"));
                }
            }
        }
        Ok(preview::Content::Text(text)) => {
            ui.set_preview_is_image(false);
            ui.set_preview_text(SharedString::from(text));
        }
        Ok(preview::Content::Info(info)) => {
            ui.set_preview_is_image(false);
            ui.set_preview_text(SharedString::from(info));
        }
        Err(e) => {
            ui.set_preview_is_image(false);
            ui.set_preview_text(SharedString::from(format!("No preview: {e}")));
        }
    }
    // Surface the pane automatically on first preview.
    ui.set_show_preview(true);
}

/// Refresh the preview pane's permission line for `path`.
fn refresh_preview_perms(ui: &MainWindow, state: &Rc<RefCell<AppState>>, path: &PathBuf) {
    match permissions::read(&state.borrow().sandbox, path) {
        Ok(p) => {
            ui.set_preview_can_chmod(true);
            ui.set_preview_perms(SharedString::from(format!("{} ({:o})", p.symbolic(), p.mode)));
        }
        Err(_) => {
            ui.set_preview_can_chmod(false);
            ui.set_preview_perms(SharedString::from(""));
        }
    }
}

/// Toggle the executable bits of the previewed file. If the OS refuses because
/// the user does not own the file, stage the change for PolicyKit elevation.
fn chmod_toggle_exec(ui: &MainWindow, state: &Rc<RefCell<AppState>>) {
    let Some(path) = state.borrow().preview_path.clone() else { return };
    let current = permissions::read(&state.borrow().sandbox, &path);
    let Ok(p) = current else {
        ui.set_status_text(sx(ui, "Cannot read permissions", "İzinler okunamadı", "No se pueden leer los permisos"));
        return;
    };
    let new_mode = if p.mode & 0o111 != 0 { p.mode & !0o111 } else { p.mode | 0o111 };
    let res = permissions::set_mode(&state.borrow().sandbox, &path, new_mode);
    match res {
        Ok(()) => {
            ui.set_preview_needs_elevation(false);
            state.borrow_mut().pending_chmod = None;
            refresh_preview_perms(ui, state, &path);
            ui.set_status_text(sx(ui, "Permissions updated", "İzinler güncellendi", "Permisos actualizados"));
        }
        Err(e) if e.is_permission_denied() => {
            state.borrow_mut().pending_chmod = Some((path, new_mode));
            ui.set_preview_needs_elevation(true);
            ui.set_status_text(sx(
                ui,
                "Permission denied — you don't own this file. Use 🔑 As administrator.",
                "İzin reddedildi — bu dosya size ait değil. 🔑 Yönetici olarak'ı kullanın.",
                "Permiso denegado — no eres el propietario de este archivo. Usa 🔑 Como administrador.",
            ));
        }
        Err(e) => ui.set_status_text(sx(ui, format!("chmod failed: {e}"), format!("chmod başarısız: {e}"), format!("chmod falló: {e}"))),
    }
}

/// Re-apply the pending chmod with PolicyKit elevation (`pkexec`).
fn chmod_as_admin(ui: &MainWindow, state: &Rc<RefCell<AppState>>) {
    let Some((path, mode)) = state.borrow().pending_chmod.clone() else {
        ui.set_status_text(sx(ui, "Nothing to elevate", "Yükseltilecek bir şey yok", "Nada que elevar"));
        return;
    };
    let res = permissions::set_mode_elevated(&state.borrow().sandbox, &path, mode);
    match res {
        Ok(()) => {
            ui.set_preview_needs_elevation(false);
            state.borrow_mut().pending_chmod = None;
            refresh_preview_perms(ui, state, &path);
            ui.set_status_text(sx(ui, "Permissions updated (elevated)", "İzinler güncellendi (yükseltildi)", "Permisos actualizados (elevado)"));
        }
        Err(e) => ui.set_status_text(sx(ui, format!("Elevation failed: {e}"), format!("Yükseltme başarısız: {e}"), format!("Error de elevación: {e}"))),
    }
}

fn open_entry_real(ui: &MainWindow, state: &Rc<RefCell<AppState>>, idx: usize) {
    let (is_dir, path) = {
        let s = state.borrow();
        match s.entries.get(idx) {
            Some(e) => (e.is_dir, e.path.clone()),
            None => return,
        }
    };
    if is_dir {
        navigate(ui, state, &path.to_string_lossy());
    } else if archive::Format::detect(&path).is_some() {
        open_archive_view(ui, state, &path);
    } else {
        // Launch in the associated/default application (exo-utils opener,
        // falling back to xdg-open).
        match exo::open(&state.borrow().sandbox, &path) {
            Ok(()) => ui.set_status_text(sx(ui, format!("Opening {}…", path.display()), format!("{} açılıyor…", path.display()), format!("Abriendo {}…", path.display()))),
            Err(e) => ui.set_status_text(sx(ui, format!("Open failed: {e}"), format!("Açma başarısız: {e}"), format!("Error al abrir: {e}"))),
        }
    }
}

/// Enter archive-browser mode: list the archive members and show them in the grid.
fn open_archive_view(ui: &MainWindow, state: &Rc<RefCell<AppState>>, archive: &std::path::Path) {
    let sandbox = state.borrow().sandbox.clone();
    let arc_name = archive
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let archive_path = archive.to_path_buf();

    // Show the archive banner immediately with loading state.
    {
        let mut s = state.borrow_mut();
        s.archive_path = Some(archive_path.clone());
        s.entries = Vec::new();
        s.selected.clear();
    }
    ui.set_in_archive(true);
    ui.set_archive_loading(true);
    ui.set_archive_loaded_count(0);
    ui.set_archive_load_fraction(0.0);
    ui.set_archive_name(SharedString::from(arc_name));

    let weak = ui.as_weak();
    std::thread::Builder::new()
        .name("archive-list".into())
        .spawn(move || {
            let result = archive::list_members_tracked(
                &sandbox,
                &archive_path,
                &archive::Options::default(),
                &mut |done, total| {
                    let fraction = if total > 0 { (done as f32 / total as f32).clamp(0.0, 1.0) } else { 0.0 };
                    let count = done as i32;
                    let _ = weak.upgrade_in_event_loop(move |ui| {
                        ui.set_archive_loaded_count(count);
                        ui.set_archive_load_fraction(fraction);
                    });
                },
            );

            let _ = weak.upgrade_in_event_loop(move |ui| {
                ui.set_archive_loading(false);
                ui.set_archive_load_fraction(0.0);
                match result {
                    Err(e) => {
                        ui.set_in_archive(false);
                        ui.set_status_text(sx(&ui,
                            format!("Cannot read archive: {e}"),
                            format!("Arşiv okunamadı: {e}"),
                            format!("Error al leer: {e}")));
                    }
                    Ok(members) => {
                        APP.with(|a| {
                            if let Some(state) = a.borrow().as_ref() {
                                let entries: Vec<Entry> = members.into_iter().map(|m| {
                                    let name = m.path.file_name()
                                        .map(|n| n.to_string_lossy().into_owned())
                                        .unwrap_or_else(|| m.path.to_string_lossy().into_owned());
                                    let extension = if m.is_dir { String::new() } else {
                                        m.path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
                                    };
                                    Entry {
                                        path: archive_path.join(&m.path),
                                        name,
                                        is_dir: m.is_dir,
                                        is_symlink: false,
                                        size: m.size,
                                        modified: None,
                                        extension,
                                    }
                                }).collect();
                                let count = entries.len();
                                state.borrow_mut().entries = entries;
                                let msg = sx(&ui, format!("{count} items"), format!("{count} öğe"), format!("{count} elementos"));
                                ui.set_status_text(msg);
                                refresh_view(&ui, state);
                            }
                        });
                    }
                }
            });
        })
        .ok();
}

/// Extract the archive currently open in archive-browser mode to the folder beside it.
fn extract_from_archive_view(ui: &MainWindow, state: &Rc<RefCell<AppState>>) {
    let (arc, dir, sandbox) = {
        let s = state.borrow();
        let arc = match s.archive_path.clone() {
            Some(p) => p,
            None => return,
        };
        let dir = arc.parent().map(PathBuf::from).unwrap_or_else(|| s.current.clone());
        (arc, dir, s.sandbox.clone())
    };
    let stem = archive_stem(&arc);
    let dest = unique_path(&dir, &stem, "");
    let total = 1usize;
    let arc_name = arc.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();

    ui.set_extract_active(true);
    ui.set_extract_fraction(0.0);
    ui.set_extract_label(SharedString::from(format!("1/1 — {arc_name}")));

    let weak = ui.as_weak();
    std::thread::Builder::new()
        .name("extract".into())
        .spawn(move || {
            let result = archive::extract(
                &sandbox,
                &arc,
                &dest,
                &archive::Options::default(),
                &mut |p| {
                    let frac = if p.files_total > 0 {
                        p.files_done as f32 / p.files_total as f32
                    } else { 0.0 };
                    let _ = weak.upgrade_in_event_loop(move |ui| {
                        ui.set_extract_fraction(frac.clamp(0.0, 1.0));
                    });
                },
            );
            let msg = match result {
                Ok(()) => SharedString::from(format!("{arc_name} çıkarıldı")),
                Err(e) => SharedString::from(format!("Çıkarma başarısız: {e}")),
            };
            let _ = weak.upgrade_in_event_loop(move |ui| {
                ui.set_extract_active(false);
                ui.set_extract_fraction(0.0);
                ui.set_status_text(msg);
                APP.with(|a| {
                    if let Some(state) = a.borrow().as_ref() {
                        // Stay in archive view; the extracted folder appeared beside the archive
                        let _ = total; // suppress unused warning
                    }
                });
            });
        })
        .ok();
}

fn reload(ui: &MainWindow, state: &Rc<RefCell<AppState>>) {
    let (dir, show_hidden, sort) = {
        let s = state.borrow();
        (s.current.clone(), s.show_hidden, s.sort)
    };
    let listing = filesystem::list_dir(&state.borrow().sandbox, &dir, show_hidden, sort);
    match listing {
        Ok(entries) => {
            let count = entries.len();
            {
                let mut s = state.borrow_mut();
                s.entries = entries;
                s.selected.clear();
            }
            ui.set_current_path(SharedString::from(dir.to_string_lossy().to_string()));
            ui.set_active_root_path(SharedString::from(active_root(&state.borrow(), &dir)));
            set_breadcrumbs(ui, state, &dir);
            ui.set_in_trash(false);
            ui.set_in_archive(false);
            ui.set_status_text(sx(ui, format!("{count} items"), format!("{count} öğe"), format!("{count} elementos")));
            refresh_view(ui, state);
        }
        Err(e) => {
            ui.set_status_text(sx(ui, format!("Cannot open folder: {e}"), format!("Klasör açılamadı: {e}"), format!("No se puede abrir la carpeta: {e}")));
        }
    }
}

fn run_search(ui: &MainWindow, state: &Rc<RefCell<AppState>>, term: &str) {
    if term.trim().is_empty() {
        reload(ui, state);
        return;
    }
    let in_content = ui.get_search_in_content();
    let (root, query) = {
        let s = state.borrow();
        let q = if in_content {
            // Content search: match file bodies, ignore the name filter.
            search::Query {
                content_contains: Some(term.to_string()),
                include_hidden: s.show_hidden,
                limit: 1000,
                ..Default::default()
            }
        } else {
            search::Query {
                name_contains: Some(term.to_string()),
                include_hidden: s.show_hidden,
                limit: 1000,
                ..Default::default()
            }
        };
        (s.current.clone(), q)
    };
    // Content searches must read files, so they always use the live walk; name
    // searches prefer the in-memory index when it covers this directory.
    let (hits, how) = {
        let s = state.borrow();
        match &s.index {
            Some(idx) if !in_content && idx.covers(&root) && idx.is_ready() => {
                (idx.query(&root, &query), "indexed")
            }
            _ => (search::search(&s.sandbox, &root, &query), if in_content { "in files" } else { "scanned" }),
        }
    };
    let count = hits.len();
    {
        let mut s = state.borrow_mut();
        s.entries = hits;
        s.selected.clear();
    }
    let how_tr = match how {
        "indexed" => "indeksli",
        "in files" => "dosyada",
        _ => "tarandı",
    };
    let how_es = match how {
        "indexed" => "indexado",
        "in files" => "en archivos",
        _ => "escaneado",
    };
    ui.set_status_text(sx(
        ui,
        format!("{count} results for \"{term}\" ({how})"),
        format!("\"{term}\" için {count} sonuç ({how_tr})"),
        format!("{count} resultados para \"{term}\" ({how_es})"),
    ));
    refresh_view(ui, state);
}

fn make_new_folder(ui: &MainWindow, state: &Rc<RefCell<AppState>>) {
    let dir = state.borrow().current.clone();
    let mut name = "New Folder".to_string();
    let mut n = 2;
    while dir.join(&name).exists() {
        name = format!("New Folder {n}");
        n += 1;
    }
    let created = filesystem::create_dir(&state.borrow().sandbox, dir.join(&name));
    match created {
        Ok(_) => reload(ui, state),
        Err(e) => ui.set_status_text(sx(ui, format!("Create failed: {e}"), format!("Oluşturma başarısız: {e}"), format!("Error al crear: {e}"))),
    }
}

fn trash_selected(ui: &MainWindow, state: &Rc<RefCell<AppState>>) {
    let paths: Vec<PathBuf> = {
        let s = state.borrow();
        s.selected.iter().filter_map(|i| s.entries.get(*i)).map(|e| e.path.clone()).collect()
    };
    if paths.is_empty() {
        ui.set_status_text(sx(ui, "Nothing selected", "Seçim yok", "Nada seleccionado"));
        return;
    }
    let trashed = trash::move_many_to_trash(&state.borrow().sandbox, &paths);
    match trashed {
        Ok(()) => {
            ui.set_status_text(sx(ui, format!("Moved {} to Trash", paths.len()), format!("{} öğe çöpe taşındı", paths.len()), format!("{} movido(s) a la papelera", paths.len())));
            reload(ui, state);
        }
        Err(e) => ui.set_status_text(sx(ui, format!("Trash failed: {e}"), format!("Çöpe atma başarısız: {e}"), format!("Error al enviar a la papelera: {e}"))),
    }
}

fn compress_selected(ui: &MainWindow, state: &Rc<RefCell<AppState>>) {
    let (dir, paths) = {
        let s = state.borrow();
        let paths: Vec<PathBuf> =
            s.selected.iter().filter_map(|i| s.entries.get(*i)).map(|e| e.path.clone()).collect();
        (s.current.clone(), paths)
    };
    if paths.is_empty() {
        ui.set_status_text(sx(ui, "Select items to compress", "Sıkıştırılacak öğeleri seçin", "Seleccione elementos para comprimir"));
        return;
    }
    let target = unique_path(&dir, "Archive", "zip");
    let result = archive::create(
        &state.borrow().sandbox,
        &target,
        &paths,
        &archive::Options::default(),
        &mut |_p| {},
    );
    match result {
        Ok(()) => {
            let name = target.file_name().unwrap_or_default().to_string_lossy().into_owned();
            ui.set_status_text(sx(
                ui,
                format!("Compressed {} items → {name}", paths.len()),
                format!("{} öğe sıkıştırıldı → {name}", paths.len()),
                format!("{} elementos comprimidos → {name}", paths.len()),
            ));
            reload(ui, state);
        }
        Err(e) => ui.set_status_text(sx(ui, format!("Compress failed: {e}"), format!("Sıkıştırma başarısız: {e}"), format!("Error al comprimir: {e}"))),
    }
}

fn extract_selected(ui: &MainWindow, state: &Rc<RefCell<AppState>>) {
    let (dir, archives, sandbox) = {
        let s = state.borrow();
        let archives: Vec<PathBuf> = s
            .selected
            .iter()
            .filter_map(|i| s.entries.get(*i))
            .filter(|e| !e.is_dir && archive::Format::detect(&e.path).is_some())
            .map(|e| e.path.clone())
            .collect();
        (s.current.clone(), archives, s.sandbox.clone())
    };
    if archives.is_empty() {
        ui.set_status_text(sx(ui, "No archive selected", "Arşiv seçilmedi", "Ningún archivo seleccionado"));
        return;
    }
    let total = archives.len();
    ui.set_extract_active(true);
    ui.set_extract_fraction(0.0);
    ui.set_extract_label(SharedString::from("Hazırlanıyor…"));

    let weak = ui.as_weak();
    std::thread::Builder::new()
        .name("extract".into())
        .spawn(move || {
            let mut ok = 0usize;
            let mut last_err = String::new();
            for (idx, arc) in archives.iter().enumerate() {
                let stem = archive_stem(arc);
                let dest = unique_path(&dir, &stem, "");
                let arc_name = arc.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let label = format!("{}/{} — {}", idx + 1, total, arc_name);
                let fraction = idx as f32 / total as f32;
                let lbl = SharedString::from(label.clone());
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    ui.set_extract_label(lbl);
                    ui.set_extract_fraction(fraction);
                });
                let result = archive::extract(
                    &sandbox,
                    arc,
                    &dest,
                    &archive::Options::default(),
                    &mut |p| {
                        // send per-file progress within a single archive
                        let file_frac = if p.files_total > 0 {
                            p.files_done as f32 / p.files_total as f32
                        } else {
                            0.0
                        };
                        let overall = (idx as f32 + file_frac) / total as f32;
                        let _ = weak.upgrade_in_event_loop(move |ui| {
                            ui.set_extract_fraction(overall);
                        });
                    },
                );
                match result {
                    Ok(()) => ok += 1,
                    Err(e) => last_err = e.to_string(),
                }
            }
            let msg = if ok == total {
                format!("{ok} arşiv çıkarıldı")
            } else {
                format!("{ok}/{total} çıkarıldı — {last_err}")
            };
            let msg = SharedString::from(msg);
            let _ = weak.upgrade_in_event_loop(move |ui| {
                ui.set_extract_active(false);
                ui.set_extract_fraction(0.0);
                ui.set_status_text(msg);
                APP.with(|a| {
                    if let Some(state) = a.borrow().as_ref() {
                        reload(&ui, state);
                    }
                });
            });
        })
        .ok();
}

/// Strip compound archive suffixes (.tar.gz → name, .tar.bz2 → name, etc.)
fn archive_stem(path: &std::path::Path) -> String {
    let name = path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "extracted".into());
    const COMPOUND: &[&str] = &[
        ".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst", ".tar.lz", ".tar.lzo",
        ".tar.lzma", ".tar.br", ".tar.z",
        ".tgz", ".tbz2", ".txz", ".tlz", ".tzo", ".taz",
    ];
    let lower = name.to_lowercase();
    for suffix in COMPOUND {
        if lower.ends_with(suffix) {
            return name[..name.len() - suffix.len()].to_string();
        }
    }
    // Single-extension: strip once
    std::path::Path::new(&name)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or(name)
}

/// Build a non-colliding `dir/<base>.<ext>` (or `dir/<base>` if ext empty).
fn unique_path(dir: &std::path::Path, base: &str, ext: &str) -> PathBuf {
    let make = |n: u32| {
        let stem = if n == 1 { base.to_string() } else { format!("{base} {n}") };
        if ext.is_empty() {
            dir.join(stem)
        } else {
            dir.join(format!("{stem}.{ext}"))
        }
    };
    (1..=9999).map(make).find(|p| !p.exists()).unwrap_or_else(|| make(1))
}

/// Handle a drag-and-drop release: move the dragged item(s) into `target_path`.
/// If the dragged entry is part of a multi-selection, the whole selection moves.
fn perform_drop(ui: &MainWindow, state: &Rc<RefCell<AppState>>, source_index: i32, target_path: &str) {
    if target_path.is_empty() || source_index < 0 {
        return;
    }
    let dest = PathBuf::from(target_path);
    let sources: Vec<PathBuf> = {
        let s = state.borrow();
        let idx = source_index as usize;
        if s.selected.contains(&idx) && s.selected.len() > 1 {
            s.selected.iter().filter_map(|i| s.entries.get(*i)).map(|e| e.path.clone()).collect()
        } else {
            s.entries.get(idx).map(|e| vec![e.path.clone()]).unwrap_or_default()
        }
    };
    if sources.is_empty() {
        return;
    }
    // Don't drop something into itself, its own parent (no-op), or its subtree.
    if sources.iter().any(|src| {
        dest == *src || dest.starts_with(src) || src.parent() == Some(dest.as_path())
    }) {
        ui.set_status_text(sx(ui, "Nothing to do (same location)", "Yapılacak bir şey yok (aynı konum)", "Nada que hacer (misma ubicación)"));
        return;
    }
    let id = {
        let s = state.borrow();
        s.transfers.enqueue(&s.sandbox, sources.clone(), dest.clone(), transfer::Kind::Move)
    };
    let name = dest.file_name().unwrap_or_default().to_string_lossy().into_owned();
    ui.set_status_text(sx(
        ui,
        format!("Moving {} item(s) → {name} (#{id})", sources.len()),
        format!("{} öğe taşınıyor → {name} (#{id})", sources.len()),
        format!("Moviendo {} elemento(s) → {name} (#{id})", sources.len()),
    ));
    sync_transfers(ui, state);
}

/// Stage the current selection into the copy/cut clipboard.
fn stage_clipboard(ui: &MainWindow, state: &Rc<RefCell<AppState>>, kind: transfer::Kind) {
    let paths: Vec<PathBuf> = {
        let s = state.borrow();
        s.selected.iter().filter_map(|i| s.entries.get(*i)).map(|e| e.path.clone()).collect()
    };
    if paths.is_empty() {
        ui.set_status_text(sx(ui, "Nothing selected", "Seçim yok", "Nada seleccionado"));
        return;
    }
    let n = paths.len();
    // Also publish to the system clipboard so other apps can paste these files.
    let exported = desktop::clipboard_export(&paths, kind == transfer::Kind::Move).is_ok();
    state.borrow_mut().clipboard = Some((paths, kind));
    ui.set_has_clipboard(true);
    let cut = kind == transfer::Kind::Move;
    let en = format!(
        "{} {n} item(s){} — Paste to a folder",
        if cut { "Cut" } else { "Copied" },
        if exported { " (also to system clipboard)" } else { "" },
    );
    let tr = format!(
        "{n} öğe {}{} — Bir klasöre yapıştırın",
        if cut { "kesildi" } else { "kopyalandı" },
        if exported { " (sistem panosuna da)" } else { "" },
    );
    let es = format!(
        "{n} elemento(s) {}{} — Pegar en una carpeta",
        if cut { "cortado(s)" } else { "copiado(s)" },
        if exported { " (también al portapapeles del sistema)" } else { "" },
    );
    ui.set_status_text(sx(ui, en, tr, es));
}

/// Paste the clipboard into the current directory as a background transfer.
fn paste(ui: &MainWindow, state: &Rc<RefCell<AppState>>) {
    let (clip, dir) = {
        let s = state.borrow();
        (s.clipboard.clone(), s.current.clone())
    };
    // Prefer Altay's own clipboard; otherwise accept files copied in another app.
    let (paths, kind) = match clip {
        Some(c) => c,
        None => match desktop::clipboard_import() {
            Some((paths, cut)) => {
                (paths, if cut { transfer::Kind::Move } else { transfer::Kind::Copy })
            }
            None => {
                ui.set_status_text(sx(ui, "Clipboard is empty", "Pano boş", "El portapapeles está vacío"));
                return;
            }
        },
    };
    let id = {
        let s = state.borrow();
        s.transfers.enqueue(&s.sandbox, paths, dir, kind)
    };
    if kind == transfer::Kind::Move {
        state.borrow_mut().clipboard = None;
        ui.set_has_clipboard(false);
    }
    ui.set_status_text(sx(ui, format!("Transfer #{id} started"), format!("Aktarım #{id} başladı"), format!("Transferencia n.º {id} iniciada")));
    sync_transfers(ui, state);
}

/// Rebuild the transfers model from the manager, and reload the listing when a
/// transfer has just finished (so newly-pasted files appear).
fn sync_transfers(ui: &MainWindow, state: &Rc<RefCell<AppState>>) {
    let snaps = state.borrow().transfers.snapshots();
    let terminal_now = snaps.iter().filter(|s| s.status.is_terminal()).count();
    let prev = state.borrow().last_terminal;
    state.borrow_mut().last_terminal = terminal_now;

    let models: Vec<TransferModel> = snaps
        .iter()
        .map(|s| {
            let active = !s.status.is_terminal();
            TransferModel {
                id: s.id as i32,
                label: SharedString::from(s.label.clone()),
                status: SharedString::from(transfer_status_str(s.status)),
                detail: SharedString::from(match &s.error {
                    Some(e) => e.clone(),
                    None => format!(
                        "{} / {}",
                        humansize::format_size(s.bytes_done, humansize::DECIMAL),
                        humansize::format_size(s.bytes_total, humansize::DECIMAL),
                    ),
                }),
                fraction: s.fraction(),
                paused: matches!(s.status, transfer::Status::Paused),
                active,
            }
        })
        .collect();
    ui.set_transfers(ModelRc::new(VecModel::from(models)));

    if terminal_now > prev {
        reload(ui, state);
    }
}

fn transfer_status_str(s: transfer::Status) -> &'static str {
    match s {
        transfer::Status::Running => "Running",
        transfer::Status::Paused => "Paused",
        transfer::Status::Done => "Done",
        transfer::Status::Cancelled => "Cancelled",
        transfer::Status::Failed => "Failed",
    }
}

/// Restore the selected trashed items to their original locations.
fn restore_selected(ui: &MainWindow, state: &Rc<RefCell<AppState>>) {
    let paths: Vec<PathBuf> = {
        let s = state.borrow();
        s.selected.iter().filter_map(|i| s.entries.get(*i)).map(|e| e.path.clone()).collect()
    };
    if paths.is_empty() {
        ui.set_status_text(sx(ui, "Select items to restore", "Geri yüklenecek öğeleri seçin", "Seleccione elementos para restaurar"));
        return;
    }
    let result = trash::restore_many(&state.borrow().sandbox, &paths);
    match result {
        Ok(n) => {
            ui.set_status_text(sx(ui, format!("Restored {n} item(s)"), format!("{n} öğe geri yüklendi"), format!("{n} elemento(s) restaurado(s)")));
            load_trash(ui, state);
        }
        Err(e) => ui.set_status_text(sx(ui, format!("Restore failed: {e}"), format!("Geri yükleme başarısız: {e}"), format!("Error al restaurar: {e}"))),
    }
}

/// Permanently empty the trash.
fn empty_trash(ui: &MainWindow, state: &Rc<RefCell<AppState>>) {
    match trash::empty() {
        Ok(()) => {
            ui.set_status_text(sx(ui, "Trash emptied", "Çöp boşaltıldı", "Papelera vaciada"));
            load_trash(ui, state);
        }
        Err(e) => ui.set_status_text(sx(ui, format!("Empty failed: {e}"), format!("Boşaltma başarısız: {e}"), format!("Error al vaciar: {e}"))),
    }
}

fn load_trash(ui: &MainWindow, state: &Rc<RefCell<AppState>>) {
    let items = trash::list().unwrap_or_default();
    let entries: Vec<Entry> = items
        .into_iter()
        .map(|t| Entry {
            path: t.original_path.clone(),
            name: t.name,
            is_dir: false,
            is_symlink: false,
            size: 0,
            modified: None,
            extension: String::new(),
        })
        .collect();
    let count = entries.len();
    {
        let mut s = state.borrow_mut();
        s.entries = entries;
        s.in_trash_view = true;
        s.selected.clear();
    }
    ui.set_current_path(SharedString::from("trash://"));
    ui.set_active_root_path(SharedString::from(""));
    ui.set_breadcrumbs(ModelRc::new(VecModel::from(vec![BreadcrumbModel {
        label: SharedString::from("🗑 Trash"),
        path: SharedString::from("trash://"),
    }])));
    ui.set_in_trash(true);
    ui.set_status_text(sx(ui, format!("Trash — {count} items"), format!("Çöp — {count} öğe"), format!("Papelera — {count} elementos")));
    refresh_view(ui, state);
}

// ---- View synchronisation ---------------------------------------------------

fn refresh_view(ui: &MainWindow, state: &Rc<RefCell<AppState>>) {
    let s = state.borrow();
    let models: Vec<EntryModel> = s
        .entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let icon_name = icon_name_for(e);
            let icon = load_file_icon(icon_name);
            EntryModel {
                name: SharedString::from(e.name.clone()),
                path: SharedString::from(e.path.to_string_lossy().to_string()),
                is_dir: e.is_dir,
                is_archive: !e.is_dir && archive::Format::from_path(&e.path).is_some(),
                is_symlink: e.is_symlink,
                size_text: SharedString::from(if e.is_dir {
                    let n = filesystem::child_count(&e.path);
                    sx(ui, format!("{n} items"), format!("{n} öğe"), format!("{n} elementos")).to_string()
                } else {
                    humansize::format_size(e.size, humansize::DECIMAL)
                }),
                modified_text: SharedString::from(format_time(e.modified)),
                type_text: SharedString::from(type_text_for(ui, e)),
                glyph: SharedString::from(glyph_for(e)),
                selected: s.selected.contains(&i),
                has_icon: icon.is_some(),
                icon: icon.unwrap_or_default(),
            }
        })
        .collect();
    ui.set_entries(ModelRc::new(VecModel::from(models)));
}

/// Rebuild the sandbox (re-scanning mount points) and refresh the sidebar.
/// Runs on the UI thread; reaches state via the `APP` thread-local.
fn reload_roots(ui: &MainWindow) {
    APP.with(|a| {
        if let Some(state) = a.borrow().clone() {
            {
                let mut s = state.borrow_mut();
                s.sandbox = Sandbox::from_env();
            }
            populate_roots(ui, &state.borrow());
            let active = active_root(&state.borrow(), &state.borrow().current.clone());
            ui.set_active_root_path(SharedString::from(active));
        }
    });
    sync_volumes(ui);
}

/// Refresh the sidebar's removable/external volume list from udisks2.
fn sync_volumes(ui: &MainWindow) {
    let models: Vec<VolumeModel> = devices::list_volumes()
        .into_iter()
        .map(|v| {
            let mp = v.mount_point.as_deref();
            let (used, total) = mp.map(disk_usage_bytes).unwrap_or((0, 0));
            let fraction = if total > 0 { (used as f32 / total as f32).clamp(0.0, 1.0) } else { 0.0 };
            let vol_label = if v.label.trim().is_empty() {
                sx(ui, "USB Storage", "USB Depolama", "Almacenamiento USB").to_string()
            } else {
                v.label.clone()
            };
            VolumeModel {
                object_path: SharedString::from(v.object_path),
                label: SharedString::from(vol_label),
                mounted: v.mount_point.is_some(),
                mount_point: SharedString::from(
                    v.mount_point.map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
                ),
                used_text: SharedString::from(if total > 0 { humansize::format_size(used, humansize::DECIMAL) } else { String::new() }),
                total_text: SharedString::from(if total > 0 { humansize::format_size(total, humansize::DECIMAL) } else { String::new() }),
                fraction,
            }
        })
        .collect();
    ui.set_volumes(ModelRc::new(VecModel::from(models)));
}

/// Localized status text: returns the Turkish or English variant per the live
/// UI language. Used for the transient status-bar messages.
fn sx(ui: &MainWindow, en: impl Into<String>, tr: impl Into<String>, es: impl Into<String>) -> SharedString {
    SharedString::from(match ui.get_lang().as_str() {
        "tr" => tr.into(),
        "es" => es.into(),
        _ => en.into(),
    })
}

/// Pick a default UI language from the locale environment when none is saved.
/// Maps the system locale to one of the supported codes (en/tr/es).
fn detect_locale() -> String {
    // 1) Ortam değişkenleri (oturum tarafından set edilmişse)
    for var in ["LC_ALL", "LC_MESSAGES", "LANG", "LANGUAGE"] {
        if let Ok(v) = std::env::var(var) {
            let v = v.to_lowercase();
            if v.is_empty() || v == "c" || v == "posix" {
                continue;
            }
            return locale_code(&v);
        }
    }
    // 2) Sistem locale dosyaları (systemd servisleri /etc/default/locale okumaz)
    for path in ["/etc/environment", "/etc/default/locale", "/etc/locale.conf"] {
        if let Ok(text) = std::fs::read_to_string(path) {
            for line in text.lines() {
                let line = line.trim();
                if let Some(val) = line.strip_prefix("LANG=") {
                    let v = val.trim_matches('"').to_lowercase();
                    if !v.is_empty() && v != "c" && v != "posix" {
                        return locale_code(&v);
                    }
                }
            }
        }
    }
    "en".into()
}

fn locale_code(v: &str) -> String {
    if v.starts_with("tr") {
        "tr".into()
    } else if v.starts_with("es") {
        "es".into()
    } else {
        "en".into()
    }
}

/// Push the chosen language's strings into the Slint `L` global. Sets every key
/// for all languages so switching at runtime works in any direction.
fn apply_language(ui: &MainWindow, lang: &str) {
    let l = ui.global::<L>();
    let code = lang;
    macro_rules! t {
        ($setter:ident, $en:expr, $tr:expr, $es:expr) => {
            l.$setter(SharedString::from(match code {
                "tr" => $tr,
                "es" => $es,
                _ => $en,
            }));
        };
    }
    t!(set_grid, "▦ Grid", "▦ Izgara", "▦ Cuadrícula");
    t!(set_list, "≣ List", "≣ Liste", "≣ Lista");
    t!(set_compact, "≡ Compact", "≡ Sıkışık", "≡ Compacto");
    t!(set_folder, "＋ Folder", "＋ Klasör", "＋ Carpeta");
    // Toolbar copy/cut/paste/select are icon-only (language-independent).
    t!(set_copy, "📋", "📋", "📋");
    t!(set_cut, "✂", "✂", "✂");
    t!(set_paste, "📥", "📥", "📥");
    t!(set_connect, "🌐 Connect", "🌐 Bağlan", "🌐 Conectar");
    t!(set_import, "📎 Import…", "📎 İçe Aktar…", "📎 Importar…");
    t!(set_select, "☐", "☐", "☐");
    t!(set_selecting, "✓", "✓", "✓");
    t!(set_preview, "👁 Preview", "👁 Önizleme", "👁 Vista previa");
    t!(set_preview_on, "👁 Preview ✓", "👁 Önizleme ✓", "👁 Vista previa ✓");
    t!(set_trash_btn, "🗑 Trash", "🗑 Çöp", "🗑 Papelera");
    t!(set_compress_btn, "🗜 Compress", "🗜 Sıkıştır", "🗜 Comprimir");
    t!(set_extract_btn, "📂 Extract", "📂 Çıkar", "📂 Extraer");
    t!(set_restore, "♻ Restore", "♻ Geri Yükle", "♻ Restaurar");
    t!(set_empty_trash, "🗑 Empty Trash", "🗑 Çöpü Boşalt", "🗑 Vaciar papelera");
    t!(set_search_ph, "Search…", "Ara…", "Buscar…");
    t!(set_search_files_ph, "Search in files…", "Dosyalarda ara…", "Buscar en archivos…");
    t!(set_in_files, "In files", "Dosya içinde", "En archivos");
    t!(set_places, "PLACES", "YERLER", "LUGARES");
    t!(set_devices, "DEVICES", "AYGITLAR", "DISPOSITIVOS");
    t!(set_usb_storage, "USB Storage", "USB Depolama", "Almacenamiento USB");
    t!(set_network, "NETWORK", "AĞ", "RED");
    t!(set_trash_place, "Trash", "Çöp", "Papelera");
    t!(set_perms, "Permissions: ", "İzinler: ", "Permisos: ");
    t!(set_toggle_exec, "Toggle executable", "Çalıştırma iznini değiştir", "Alternar ejecutable");
    t!(set_as_admin, "🔑 As administrator", "🔑 Yönetici olarak", "🔑 Como administrador");
    t!(set_transfers, "Transfers", "Aktarımlar", "Transferencias");
    t!(set_clear_finished, "Clear finished", "Bitenleri temizle", "Borrar finalizadas");
    t!(set_connect_to_server, "Connect to server", "Sunucuya bağlan", "Conectar al servidor");
    t!(set_address, "Address", "Adres", "Dirección");
    t!(set_password, "Password", "Parola", "Contraseña");
    t!(set_username_ph, "Username (optional)", "Kullanıcı adı (isteğe bağlı)", "Usuario (opcional)");
    t!(set_addr_ph, "smb://server/share", "smb://sunucu/paylaşım", "smb://servidor/recurso");
    t!(set_path_ph, "Path or smb://server/share", "Yol veya smb://sunucu/paylaşım", "Ruta o smb://servidor/recurso");
    t!(set_save_pw, "Save password in keyring", "Parolayı anahtarlıkta sakla", "Guardar contraseña en el llavero");
    t!(set_cancel, "Cancel", "İptal", "Cancelar");
    t!(set_connect_btn, "Connect", "Bağlan", "Conectar");
    t!(set_open, "Open", "Aç", "Abrir");
    t!(set_open_folder, "Open folder", "Klasörü aç", "Abrir carpeta");
    t!(set_ctx_copy, "Copy", "Kopyala", "Copiar");
    t!(set_ctx_cut, "Cut", "Kes", "Cortar");
    t!(set_ctx_compress, "Compress", "Sıkıştır", "Comprimir");
    t!(set_rename_item, "Rename…", "Yeniden adlandır…", "Renombrar…");
    t!(set_extract_here, "Extract here", "Buraya çıkar", "Extraer aquí");
    t!(set_move_to_trash, "Move to Trash", "Çöpe taşı", "Mover a la papelera");
    t!(set_rename_title, "Rename", "Yeniden adlandır", "Renombrar");
    t!(set_settings, "Settings", "Ayarlar", "Ajustes");
    t!(set_show_hidden, "Show hidden files", "Gizli dosyaları göster", "Mostrar archivos ocultos");
    t!(set_dark_mode, "Dark mode", "Koyu tema", "Modo oscuro");
    t!(set_default_view, "Default view", "Varsayılan görünüm", "Vista predeterminada");
    t!(set_language, "Language", "Dil", "Idioma");
    t!(set_close, "Close", "Kapat", "Cerrar");
    t!(set_open_with, "Open With…", "Birlikte Aç…", "Abrir con…");
    t!(set_set_default, "Set as default", "Varsayılan yap", "Predeterminado");
    t!(set_tip_paste, "Paste", "Yapıştır", "Pegar");
    t!(set_tip_select, "Select", "Seç", "Seleccionar");
    t!(set_tip_up, "Up", "Üst", "Arriba");
    t!(set_tip_home, "Home", "Ana dizin", "Inicio");
    t!(set_tip_refresh, "Refresh", "Yenile", "Actualizar");
    t!(set_tip_zoom_in, "Zoom in", "Yakınlaştır", "Acercar");
    t!(set_tip_zoom_out, "Zoom out", "Uzaklaştır", "Alejar");
    t!(set_tip_pause, "Pause", "Duraklat", "Pausar");
    t!(set_tip_resume, "Resume", "Devam et", "Reanudar");
    t!(set_tip_eject, "Eject", "Çıkar", "Expulsar");
    t!(set_tip_edit, "Edit path", "Yolu düzenle", "Editar ruta");
    // Sidebar sections
    t!(set_recent, "Recent Files", "Son Dosyalar", "Archivos recientes");
    t!(set_categories, "CATEGORIES", "KATEGORİLER", "CATEGORÍAS");
    t!(set_cat_images, "Images", "Görseller", "Imágenes");
    t!(set_cat_audio, "Audio", "Ses", "Audio");
    t!(set_cat_videos, "Videos", "Videolar", "Videos");
    t!(set_cat_docs, "Documents", "Belgeler", "Documentos");
    t!(set_cat_downloads, "Downloads", "İndirilenler", "Descargas");
    t!(set_cat_archives, "Archives", "Arşivler", "Archivos comprimidos");
    // Column headers
    t!(set_col_name, "Name", "Ad", "Nombre");
    t!(set_col_size, "Size", "Boyut", "Tamaño");
    t!(set_col_type, "Type", "Tür", "Tipo");
    t!(set_col_date, "Date", "Tarih", "Fecha");
    // Bottom action bar
    t!(set_act_move, "Move", "Taşı", "Mover");
    t!(set_act_copy, "Copy", "Kopyala", "Copiar");
    t!(set_act_paste, "Paste", "Yapıştır", "Pegar");
    t!(set_act_delete, "Delete", "Sil", "Eliminar");
    t!(set_act_more, "More", "Daha Fazla", "Más");
    t!(set_act_compress, "Compress", "Sıkıştır", "Comprimir");
    t!(set_act_extract, "Extract", "Çıkart", "Extraer");
    t!(set_act_new_folder, "New Folder", "Yeni Klasör", "Nueva carpeta");
    t!(set_act_restore, "Restore", "Geri Yükle", "Restaurar");
    t!(set_act_empty_trash, "Empty Trash", "Çöpü Boşalt", "Vaciar papelera");
}

/// Returns true only if `path` is an actual mount point (different device from parent).
fn is_mount_point(path: &std::path::Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(meta) = std::fs::metadata(path) else { return false; };
    let Some(parent) = path.parent() else { return true; };
    let Ok(parent_meta) = std::fs::metadata(parent) else { return false; };
    meta.dev() != parent_meta.dev()
}

fn populate_roots(ui: &MainWindow, state: &AppState) {
    let roots: Vec<RootModel> = state
        .sandbox
        .roots()
        .iter()
        .filter(|r| match r.kind() {
            RootKind::Home | RootKind::Network => true,
            RootKind::Removable => false, // volumes section handles removable drives
        })
        .map(|r| {
            let (used, total) = disk_usage_bytes(r.path());
            let fraction = if total > 0 { (used as f32 / total as f32).clamp(0.0, 1.0) } else { 0.0 };
            let label = match r.kind() {
                RootKind::Home      => sx(ui, "Internal Storage", "Dahili Depolama", "Almacenamiento interno"),
                RootKind::Removable => sx(ui, "USB Storage", "USB Depolama", "Almacenamiento USB"),
                RootKind::Network   => SharedString::from(r.label()),
            };
            RootModel {
                label: SharedString::from(label),
                path: SharedString::from(r.path().to_string_lossy().to_string()),
                kind: match r.kind() {
                    RootKind::Home => 0,
                    RootKind::Removable => 1,
                    RootKind::Network => 2,
                },
                glyph: SharedString::from(match r.kind() {
                    RootKind::Home => "🏠",
                    RootKind::Removable => "💾",
                    RootKind::Network => "🌐",
                }),
                used_text: SharedString::from(if total > 0 { humansize::format_size(used, humansize::DECIMAL) } else { String::new() }),
                total_text: SharedString::from(if total > 0 { humansize::format_size(total, humansize::DECIMAL) } else { String::new() }),
                fraction,
            }
        })
        .collect();
    ui.set_roots(ModelRc::new(VecModel::from(roots)));
}

/// Build clickable breadcrumb segments from the active sandbox root down to
/// `dir` (ancestors above the root are outside the sandbox, so we start at the
/// root's friendly label, e.g. "Home › Documents › Work").
fn set_breadcrumbs(ui: &MainWindow, state: &Rc<RefCell<AppState>>, dir: &std::path::Path) {
    let segs = breadcrumb_segments(&state.borrow().sandbox, dir);
    let models: Vec<BreadcrumbModel> = segs
        .into_iter()
        .map(|(label, path)| BreadcrumbModel {
            label: SharedString::from(label),
            path: SharedString::from(path),
        })
        .collect();
    ui.set_breadcrumbs(ModelRc::new(VecModel::from(models)));
}

/// Compute `(label, cumulative-path)` breadcrumb segments from the active
/// sandbox root down to `dir`. Pure (no UI) so it can be unit-tested.
fn breadcrumb_segments(sandbox: &Sandbox, dir: &std::path::Path) -> Vec<(String, String)> {
    let root = sandbox
        .roots()
        .iter()
        .filter(|r| dir.starts_with(r.path()))
        .max_by_key(|r| r.path().as_os_str().len());
    let mut segs = Vec::new();
    match root {
        Some(r) => {
            segs.push((r.label().to_string(), r.path().to_string_lossy().to_string()));
            if let Ok(rel) = dir.strip_prefix(r.path()) {
                let mut acc = r.path().to_path_buf();
                for comp in rel.components() {
                    if let std::path::Component::Normal(c) = comp {
                        acc.push(c);
                        segs.push((c.to_string_lossy().to_string(), acc.to_string_lossy().to_string()));
                    }
                }
            }
        }
        None => {
            let label = dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| dir.to_string_lossy().into_owned());
            segs.push((label, dir.to_string_lossy().to_string()));
        }
    }
    segs
}

/// Which sidebar root contains `dir` (for highlighting).
fn active_root(state: &AppState, dir: &PathBuf) -> String {
    state
        .sandbox
        .roots()
        .iter()
        .filter(|r| dir.starts_with(r.path()))
        // Prefer the most specific (longest) matching root.
        .max_by_key(|r| r.path().as_os_str().len())
        .map(|r| r.path().to_string_lossy().to_string())
        .unwrap_or_default()
}

fn format_time(t: Option<std::time::SystemTime>) -> String {
    match t {
        Some(st) => {
            let dt: chrono::DateTime<chrono::Local> = st.into();
            dt.format("%Y-%m-%d %H:%M").to_string()
        }
        None => String::new(),
    }
}

/// Pick an emoji glyph for an entry (fallback when no system icon found).
fn glyph_for(e: &Entry) -> &'static str {
    if e.is_dir {
        return "📁";
    }
    match e.extension.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg" | "tiff" | "ico" | "avif" => "🖼",
        "pdf" => "📕",
        "doc" | "docx" | "odt" | "rtf" => "📄",
        "xls" | "xlsx" | "ods" | "csv" => "📊",
        "ppt" | "pptx" => "📽",
        "txt" | "md" | "log" => "📝",
        "mp4" | "mkv" | "webm" | "mov" | "avi" | "m4v" => "🎬",
        "mp3" | "flac" | "wav" | "ogg" | "m4a" | "opus" => "🎵",
        "zip" | "zipx" | "7z" | "rar" | "cbr" | "ace" | "alz" | "arj" | "lzh" | "lha" | "zoo"
        | "tar" | "gz" | "tgz" | "bz2" | "bz" | "xz" | "zst" | "lz" | "lzo" | "br" | "rz"
        | "tbz2" | "txz" | "tlz" | "tzo" | "taz" | "z" => "🗜",
        "deb" | "rpm" | "appimage" | "apk" | "jar" | "war" | "ear" | "cab" | "iso" | "cpio" => "📦",
        "sh" | "bin" | "run" => "⚙",
        _ => "📄",
    }
}

/// Map entry extension/type to a freedesktop icon name.
fn icon_name_for(e: &Entry) -> &'static str {
    if e.is_dir {
        return "folder";
    }
    match e.extension.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tiff" | "ico" | "avif" => "image-x-generic",
        "svg" => "image-x-generic",
        "mp3" | "flac" | "wav" | "ogg" | "m4a" | "opus" | "aac" => "audio-x-generic",
        "mp4" | "mkv" | "webm" | "mov" | "avi" | "m4v" => "video-x-generic",
        "pdf" | "doc" | "docx" | "odt" | "rtf" => "x-office-document",
        "xls" | "xlsx" | "ods" | "csv" => "x-office-spreadsheet",
        "ppt" | "pptx" | "odp" => "x-office-presentation",
        "zip" | "zipx" | "7z" | "rar" | "cbr" | "ace" | "alz" | "arj" | "lzh" | "lha" | "zoo"
        | "tar" | "gz" | "tgz" | "bz2" | "bz" | "xz" | "zst" | "lz" | "lzo" | "br" | "rz"
        | "tbz2" | "txz" | "tlz" | "tzo" | "taz" | "z" => "package-x-generic",
        "deb" | "rpm" | "appimage" | "apk" | "jar" | "war" | "ear" | "cab" | "iso" | "cpio" => "package-x-generic",
        "sh" | "bash" | "zsh" | "fish" | "py" | "rb" | "js" | "ts" | "rs" | "c" | "cpp" | "h" => "text-x-script",
        "html" | "htm" => "text-html",
        "txt" | "md" | "log" | "conf" | "ini" | "toml" | "yaml" | "yml" | "json" => "text-x-generic",
        "bin" | "run" | "exe" => "application-x-executable",
        _ => "application-x-generic",
    }
}

/// Load (and cache) a system icon image by freedesktop icon name.
/// Uses thread-local storage because slint::Image is not Send+Sync.
fn load_file_icon(icon_name: &str) -> Option<slint::Image> {
    use std::cell::RefCell;
    use std::collections::HashMap;
    thread_local! {
        static CACHE: RefCell<HashMap<String, Option<slint::Image>>> = RefCell::new(HashMap::new());
    }
    CACHE.with(|c| {
        let mut cache = c.borrow_mut();
        if let Some(entry) = cache.get(icon_name) {
            return entry.clone();
        }
        let img = exo::resolve_mime_icon(icon_name)
            .and_then(|p| slint::Image::load_from_path(&p).ok());
        cache.insert(icon_name.to_string(), img.clone());
        img
    })
}

/// Short type label shown in the Type column / badge.
fn type_text_for(ui: &MainWindow, e: &Entry) -> String {
    if e.is_dir {
        return sx(ui, "Folder", "Klasör", "Carpeta").to_string();
    }
    if e.extension.is_empty() {
        return sx(ui, "File", "Dosya", "Archivo").to_string();
    }
    let s = |en, tr, es| sx(ui, en, tr, es).to_string();
    match e.extension.as_str() {
        "mp3"  => s("MP3 audio",   "MP3 sesi",          "Audio MP3"),
        "flac" => s("FLAC audio",  "FLAC sesi",         "Audio FLAC"),
        "ogg"  => s("OGG audio",   "OGG sesi",          "Audio OGG"),
        "wav"  => s("WAV audio",   "WAV sesi",          "Audio WAV"),
        "aac"  => s("AAC audio",   "AAC sesi",          "Audio AAC"),
        "m4a"  => s("M4A audio",   "M4A sesi",          "Audio M4A"),
        "opus" => s("Opus audio",  "Opus sesi",         "Audio Opus"),
        "mp4"  => s("MP4 video",   "MP4 videosu",       "Video MP4"),
        "mkv"  => s("MKV video",   "MKV videosu",       "Video MKV"),
        "avi"  => s("AVI video",   "AVI videosu",       "Video AVI"),
        "mov"  => s("MOV video",   "MOV videosu",       "Video MOV"),
        "webm" => s("WebM video",  "WebM videosu",      "Video WebM"),
        "jpg" | "jpeg" => s("JPEG image", "JPEG görüntüsü", "Imagen JPEG"),
        "png"  => s("PNG image",   "PNG görüntüsü",     "Imagen PNG"),
        "gif"  => s("GIF image",   "GIF görüntüsü",     "Imagen GIF"),
        "webp" => s("WebP image",  "WebP görüntüsü",    "Imagen WebP"),
        "svg"  => s("SVG image",   "SVG görüntüsü",     "Imagen SVG"),
        "pdf"  => s("PDF document","PDF belgesi",        "Documento PDF"),
        "doc"  => s("Word document","Word belgesi",      "Documento Word"),
        "docx" => s("Word document","Word belgesi",      "Documento Word"),
        "xls"  => s("Spreadsheet", "Excel çalışma sayfası", "Hoja de cálculo"),
        "xlsx" => s("Spreadsheet", "Excel çalışma sayfası", "Hoja de cálculo"),
        "ppt"  => s("Presentation","Sunum",              "Presentación"),
        "pptx" => s("Presentation","Sunum",              "Presentación"),
        "zip"  => s("ZIP archive", "ZIP arşivi",         "Archivo ZIP"),
        "gz" | "tar" | "bz2" | "xz" | "zst" =>
                   s("Archive",    "Arşiv",              "Archivo comprimido"),
        "7z"   => s("7-Zip archive","7-Zip arşivi",     "Archivo 7-Zip"),
        "rar"  => s("RAR archive", "RAR arşivi",         "Archivo RAR"),
        "deb"  => s("Debian package","Debian paketi",    "Paquete Debian"),
        "rpm"  => s("RPM package", "RPM paketi",         "Paquete RPM"),
        "txt"  => s("Text file",   "Metin dosyası",      "Archivo de texto"),
        "log"  => s("Log file",    "Günlük dosyası",     "Archivo de registro"),
        "sh" | "bash" => s("Shell script","Kabuk betiği","Script de shell"),
        "rs"   => s("Rust source", "Rust kaynağı",       "Fuente Rust"),
        "py"   => s("Python script","Python betiği",     "Script Python"),
        "js"   => s("JavaScript",  "JavaScript",         "JavaScript"),
        "ts"   => s("TypeScript",  "TypeScript",         "TypeScript"),
        "html" | "htm" => s("HTML document","HTML belgesi","Documento HTML"),
        "css"  => s("Stylesheet",  "CSS dosyası",        "Hoja de estilo"),
        "json" => s("JSON data",   "JSON verisi",        "Datos JSON"),
        "xml"  => s("XML document","XML belgesi",        "Documento XML"),
        "toml" | "yaml" | "yml" =>
                   s("Config file","Yapılandırma dosyası","Archivo de configuración"),
        _ => e.extension.to_uppercase(),
    }
}

/// Disk usage for the filesystem containing `path` via statvfs(2).
/// Returns (used_bytes, total_bytes). Returns (0, 0) on error.
fn disk_usage_bytes(path: &std::path::Path) -> (u64, u64) {
    use std::ffi::CString;
    let Ok(cpath) = CString::new(path.to_string_lossy().as_bytes()) else { return (0, 0); };
    unsafe {
        let mut st: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(cpath.as_ptr(), &mut st) != 0 { return (0, 0); }
        let bsize = st.f_frsize as u64;
        let total = st.f_blocks as u64 * bsize;
        let free  = st.f_bavail as u64 * bsize;
        (total.saturating_sub(free), total)
    }
}

/// Build a DiskUsageModel for the current home directory.
fn build_disk_usage(home: &std::path::Path) -> DiskUsageModel {
    let (used, total) = disk_usage_bytes(home);
    if total == 0 {
        return DiskUsageModel {
            label: SharedString::from(""),
            used_text: SharedString::from(""),
            total_text: SharedString::from(""),
            fraction: 0.0,
        };
    }
    DiskUsageModel {
        label: SharedString::from("Dahili Depolama"),
        used_text: SharedString::from(humansize::format_size(used, humansize::DECIMAL)),
        total_text: SharedString::from(humansize::format_size(total, humansize::DECIMAL)),
        fraction: (used as f32 / total as f32).clamp(0.0, 1.0),
    }
}

/// Navigate to the XDG directory that best represents a category.
/// Falls back to home if the directory does not exist.
fn xdg_dir_for_category(category: &str, home: &std::path::Path) -> std::path::PathBuf {
    let candidate = match category {
        "images"    => dirs::picture_dir(),
        "audio"     => dirs::audio_dir(),
        "videos"    => dirs::video_dir(),
        "docs"      => dirs::document_dir(),
        "downloads" => dirs::download_dir(),
        _           => None,
    };
    candidate
        .filter(|p| p.exists())
        .unwrap_or_else(|| home.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::{breadcrumb_segments, copy_external};
    use crate::security::{AllowedRoot, Sandbox};

    #[test]
    fn breadcrumbs_start_at_root_label() {
        let home = std::env::temp_dir().join(format!("altay-bc-{}", std::process::id()));
        std::fs::create_dir_all(home.join("Documents/Work")).unwrap();
        let canon = std::fs::canonicalize(&home).unwrap();
        let sb = Sandbox::with_roots(vec![AllowedRoot::home(canon.clone())]);

        let segs = breadcrumb_segments(&sb, &canon.join("Documents/Work"));
        let labels: Vec<&str> = segs.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(labels, ["Home", "Documents", "Work"]);
        // Each segment's path is the cumulative ancestor.
        assert_eq!(segs[0].1, canon.to_string_lossy());
        assert_eq!(segs[2].1, canon.join("Documents/Work").to_string_lossy());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn copy_external_handles_nested_dirs() {
        let base = std::env::temp_dir().join(format!("altay-import-{}", std::process::id()));
        let src = base.join("src");
        std::fs::create_dir_all(src.join("inner")).unwrap();
        std::fs::write(src.join("a.txt"), b"alpha").unwrap();
        std::fs::write(src.join("inner/b.txt"), b"beta").unwrap();

        let dst = base.join("dst");
        copy_external(&src, &dst).unwrap();
        assert_eq!(std::fs::read(dst.join("a.txt")).unwrap(), b"alpha");
        assert_eq!(std::fs::read(dst.join("inner/b.txt")).unwrap(), b"beta");
        let _ = std::fs::remove_dir_all(&base);
    }
}
