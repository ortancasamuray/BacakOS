// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! xdg-desktop-portal integration (FileChooser) via `ashpd`.
//!
//! A file manager normally browses its own sandbox, but the file-chooser portal
//! is the freedesktop-blessed way to let the user grant access to files *outside*
//! the sandbox: the user explicitly picks them in the system dialog, the portal
//! authorizes access, and Altay imports them into the current (sandboxed)
//! directory. This matches the security model — external files enter only
//! through explicit user authorization.

use std::path::PathBuf;

use ashpd::desktop::file_chooser::OpenFileRequest;

/// Run a portal request on a temporary runtime (portal calls are occasional and
/// user-initiated). Returns the selected local paths, or empty on cancel/error
/// (e.g. no portal backend running).
fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
        .block_on(fut)
}

/// Show the portal's "open files" dialog (multi-select) and return the chosen
/// local file paths.
pub fn pick_files() -> Vec<PathBuf> {
    block_on(async {
        let request = OpenFileRequest::default()
            .title("Import files into this folder")
            .multiple(true);
        match request.send().await.and_then(|r| r.response()) {
            Ok(files) => files.uris().iter().filter_map(|u| u.to_file_path().ok()).collect(),
            Err(e) => {
                log::info!("file-chooser portal unavailable or cancelled: {e}");
                Vec::new()
            }
        }
    })
}

