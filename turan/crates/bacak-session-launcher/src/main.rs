//! `bacak-session-launcher` — final stage of login.
//!
//! By the time this runs, the daemon has **already** dropped privileges to the
//! target user and exported the base environment (`HOME`, `XDG_RUNTIME_DIR`,
//! `XDG_SESSION_TYPE`, PAM environment, …). This process therefore runs wholly
//! unprivileged. Its job:
//!
//!   1. Source the user's profile (`~/.profile`-style shell login) so the
//!      session inherits the same env an interactive login would.
//!   2. Wrap the session in a D-Bus user session if one is not already present.
//!   3. `exec` the session command from the `.desktop` `Exec=` line, replacing
//!      this process so the session becomes the process-group leader the daemon
//!      waits on.
//!
//! Keeping this as a separate, tiny, unprivileged binary means the large/varied
//! work of "starting a desktop" never touches root.

use std::os::unix::process::CommandExt;
use std::process::Command;

fn main() -> std::process::ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_secs()
        .init();

    let exec = match std::env::var("BDM_SESSION_EXEC") {
        Ok(e) if !e.is_empty() => e,
        _ => {
            eprintln!("bacak-session-launcher: BDM_SESSION_EXEC not set");
            return std::process::ExitCode::FAILURE;
        }
    };
    let session_id = std::env::var("BDM_SESSION_ID").unwrap_or_else(|_| "unknown".into());
    log::info!("launching session '{session_id}': {exec}");

    // Split the Exec line into argv. freedesktop allows field codes (%f, %u…);
    // session entries don't use them, so we strip any that appear.
    let argv = match shell_split(&exec) {
        Some(v) if !v.is_empty() => v,
        _ => {
            eprintln!("bacak-session-launcher: empty/invalid Exec '{exec}'");
            return std::process::ExitCode::FAILURE;
        }
    };

    // Run inside a D-Bus user session unless the env already has one. Most
    // Wayland desktops expect `DBUS_SESSION_BUS_ADDRESS` to exist.
    let mut cmd = if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none()
        && which("dbus-run-session").is_some()
    {
        let mut c = Command::new("dbus-run-session");
        c.arg("--");
        c.args(&argv);
        c
    } else {
        let mut c = Command::new(&argv[0]);
        c.args(&argv[1..]);
        c
    };

    // `exec` replaces this process; on success it never returns.
    let err = cmd.exec();
    eprintln!("bacak-session-launcher: failed to exec session: {err}");
    std::process::ExitCode::FAILURE
}

/// Locate a binary on `PATH`.
fn which(bin: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|d| d.join(bin))
            .find(|p| p.is_file())
    })
}

/// Minimal POSIX-ish word splitter for `Exec=` lines: handles single/double
/// quotes and backslash escapes, and drops freedesktop `%`-field codes.
fn shell_split(s: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = s.chars().peekable();
    let mut in_single = false;
    let mut in_double = false;
    let mut started = false;

    while let Some(c) = chars.next() {
        match c {
            '\'' if !in_double => {
                in_single = !in_single;
                started = true;
            }
            '"' if !in_single => {
                in_double = !in_double;
                started = true;
            }
            '\\' if !in_single => {
                if let Some(&next) = chars.peek() {
                    cur.push(next);
                    chars.next();
                    started = true;
                }
            }
            '%' if !in_single && !in_double => {
                // Skip the field code letter (e.g. %f, %U); none apply to us.
                chars.next();
            }
            c if c.is_whitespace() && !in_single && !in_double => {
                if started {
                    out.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            c => {
                cur.push(c);
                started = true;
            }
        }
    }
    if in_single || in_double {
        return None; // unbalanced quotes
    }
    if started {
        out.push(cur);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::shell_split;

    #[test]
    fn splits_simple() {
        assert_eq!(
            shell_split("bacak-session --foo bar").unwrap(),
            vec!["bacak-session", "--foo", "bar"]
        );
    }

    #[test]
    fn handles_quotes() {
        assert_eq!(
            shell_split(r#"env FOO="a b" /usr/bin/start"#).unwrap(),
            vec!["env", "FOO=a b", "/usr/bin/start"]
        );
    }

    #[test]
    fn strips_field_codes() {
        assert_eq!(
            shell_split("gnome-session %U").unwrap(),
            vec!["gnome-session"]
        );
    }

    #[test]
    fn rejects_unbalanced_quotes() {
        assert!(shell_split("foo \"bar").is_none());
    }
}
