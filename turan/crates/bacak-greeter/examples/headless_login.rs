//! Headless test greeter — drives the BDM interactive login over IPC with no UI,
//! so the password→PAM→user-session path can be verified in CI/VMs where typing
//! into the graphical greeter can't be automated.
//!
//! It behaves like a greeter the daemon spawns: it connects to
//! `$BDM_GREETER_SOCKET`, authenticates a user with a real password, and starts
//! a session — exercising exactly what the GUI greeter does, minus rendering.
//!
//! Credentials come from a file (default `/etc/bdm-test-login`, override with
//! `$BDM_TEST_LOGIN`): line 1 = username, line 2 = password, line 3 = session id
//! (optional, default `bacak`). Install this as `/usr/bin/bacak-greeter` in a
//! test image so the compositor launches it via `$BACAK_STARTUP`.

use bacak_greeter::{AuthStep, GreeterClient};

fn main() -> std::process::ExitCode {
    let path = std::env::var("BDM_TEST_LOGIN").unwrap_or_else(|_| "/etc/bdm-test-login".into());
    let creds = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("headless-login: cannot read {path}: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let mut lines = creds.lines();
    let user = lines.next().unwrap_or("").trim().to_string();
    let password = lines.next().unwrap_or("").to_string();
    let session = lines.next().unwrap_or("bacak").trim().to_string();
    let session = if session.is_empty() {
        "bacak".into()
    } else {
        session
    };
    if user.is_empty() {
        eprintln!("headless-login: no username in {path}");
        return std::process::ExitCode::FAILURE;
    }

    match run(&user, &password, &session) {
        Ok(()) => {
            eprintln!("headless-login: session '{session}' started for '{user}'");
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("headless-login: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run(user: &str, password: &str, session: &str) -> Result<(), Box<dyn std::error::Error>> {
    let (mut client, _welcome) = GreeterClient::connect_from_env()?;
    let _ = client.list_users();
    let _ = client.list_sessions();

    eprintln!("headless-login: authenticating '{user}'…");
    let mut step = client.start_auth(user)?;
    loop {
        match step {
            AuthStep::Prompt { message, echo } => {
                eprintln!("headless-login: prompt {message:?} (echo={echo}) → sending secret");
                step = client.answer(password.to_string())?;
            }
            AuthStep::Info { message } => {
                eprintln!("headless-login: info: {message}");
                step = client.next_step()?;
            }
            AuthStep::Done { success, message } => {
                if success {
                    eprintln!("headless-login: authenticated");
                    break;
                }
                return Err(
                    format!("authentication failed: {}", message.unwrap_or_default()).into(),
                );
            }
        }
    }

    client.start_session(session)?;
    Ok(())
}
