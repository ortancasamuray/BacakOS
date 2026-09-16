// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! Subprocess execution with line-by-line output streaming and cooperative
//! cancellation.
//!
//! Every destructive command in the installer goes through here, which gives us
//! one place to enforce the dry-run guard and one place that logs the exact
//! argv that was executed.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};

/// Set `KUR_DRY_RUN=1` to log commands instead of running them.
///
/// This exists so the installer can be exercised on a developer machine without
/// repartitioning it. Read once per call rather than cached: integration tests
/// flip it between cases.
pub fn is_dry_run() -> bool {
    std::env::var_os("KUR_DRY_RUN").is_some_and(|v| v == "1")
}

/// A flag shared with the UI thread so "İptal" can stop an in-flight command.
#[derive(Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            bail!("kurulum kullanıcı tarafından iptal edildi");
        }
        Ok(())
    }
}

/// Run `program` to completion, invoking `on_line` for each line of merged
/// stdout/stderr.
///
/// `on_line` is called on the *worker* thread, not the UI thread — it is the
/// caller's job to marshal anything it touches. Returns an error if the process
/// exits non-zero, if it cannot be spawned, or if `cancel` is tripped.
pub fn run_streaming<F>(
    program: &str,
    args: &[&str],
    cancel: &Cancel,
    mut on_line: F,
) -> Result<()>
where
    F: FnMut(&str),
{
    cancel.check()?;
    log::info!("exec: {program} {}", args.join(" "));

    if is_dry_run() {
        on_line(&format!("[dry-run] {program} {}", args.join(" ")));
        return Ok(());
    }

    let mut child = Command::new(program)
        .args(args)
        // Debian tooling asks questions on a tty; make sure it never blocks.
        .env("DEBIAN_FRONTEND", "noninteractive")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("`{program}` başlatılamadı"))?;

    // stderr is drained on its own thread; otherwise a chatty command can fill
    // the pipe buffer and deadlock while we are still blocked reading stdout.
    let stderr = child.stderr.take().expect("stderr piped");
    let stderr_reader = std::thread::spawn(move || {
        let mut collected = Vec::new();
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            log::warn!("{line}");
            collected.push(line);
        }
        collected
    });

    let stdout = child.stdout.take().expect("stdout piped");
    for line in BufReader::new(stdout).lines() {
        let line = line.context("alt sürecin çıktısı okunamadı")?;
        log::debug!("{line}");
        on_line(&line);

        if cancel.is_cancelled() {
            kill(&mut child);
            let _ = stderr_reader.join();
            bail!("kurulum kullanıcı tarafından iptal edildi");
        }
    }

    let status = child.wait().context("alt süreç beklenemedi")?;
    let errors = stderr_reader.join().map_err(|_| anyhow!("stderr thread paniği"))?;

    if !status.success() {
        // The last few stderr lines are what a user can act on; the full
        // transcript is already in the log file.
        let tail = errors
            .iter()
            .rev()
            .take(5)
            .rev()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        bail!("`{program}` başarısız oldu ({status}):\n{tail}");
    }
    Ok(())
}

/// Run a command, discarding its output. Convenience wrapper around
/// [`run_streaming`] for the many short commands (`mkfs`, `mount`, …).
pub fn run(program: &str, args: &[&str], cancel: &Cancel) -> Result<()> {
    run_streaming(program, args, cancel, |_| {})
}

/// Run a command and capture stdout as a `String`. Used for the JSON-emitting
/// query tools (`lsblk -J`, `blkid`), never for destructive operations.
pub fn capture(program: &str, args: &[&str]) -> Result<String> {
    log::debug!("capture: {program} {}", args.join(" "));
    let out = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("`{program}` başlatılamadı"))?;

    if !out.status.success() {
        bail!(
            "`{program}` başarısız oldu ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    String::from_utf8(out.stdout).context("çıktı geçerli UTF-8 değil")
}

/// Run a command, feeding `stdin_data` to its standard input.
///
/// This is the *only* correct way to hand a password to `chpasswd`: an argv
/// entry is world-readable through `/proc/<pid>/cmdline` for the lifetime of
/// the process, and an environment variable is readable by the same means.
pub fn run_with_stdin(program: &str, args: &[&str], stdin_data: &str, cancel: &Cancel) -> Result<()> {
    use std::io::Write;

    cancel.check()?;
    // Deliberately never log `stdin_data`.
    log::info!("exec (stdin): {program} {}", args.join(" "));

    if is_dry_run() {
        return Ok(());
    }

    let mut child = Command::new(program)
        .args(args)
        .env("DEBIAN_FRONTEND", "noninteractive")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("`{program}` başlatılamadı"))?;

    child
        .stdin
        .take()
        .expect("stdin piped")
        .write_all(stdin_data.as_bytes())
        .with_context(|| format!("`{program}` girdisine yazılamadı"))?;
    // `stdin` dropped here, closing the pipe so the child sees EOF.

    let out = child.wait_with_output().context("alt süreç beklenemedi")?;
    if !out.status.success() {
        bail!(
            "`{program}` başarısız oldu ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// Run a command inside the target root via `chroot`.
///
/// Callers must have bind-mounted /dev, /proc and /sys beforehand — see
/// [`crate::backend::install`].
pub fn chroot(root: &str, program: &str, args: &[&str], cancel: &Cancel) -> Result<()> {
    let mut argv = vec![root, program];
    argv.extend_from_slice(args);
    run("chroot", &argv, cancel)
}

/// SIGKILL the child and reap it. Best-effort: a command that already exited
/// makes both calls fail harmlessly.
fn kill(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}
