//! Greeter frontends.
//!
//! Two interchangeable frontends drive the same [`bacak_greeter::GreeterClient`]:
//!
//! * **GUI** (`--features gui`) — an egui/eframe graphical greeter
//!   ([`gui`]) rendering the login screen described in `docs/GREETER_UI.md`.
//! * **TTY** (default) — a small, display-free reference frontend, useful for
//!   development, headless testing, and as the executable specification of the
//!   login flow:
//!
//!   ```text
//!   connect → Welcome → pick user → authenticate (prompt/answer loop) →
//!   pick session → StartSession → exit.
//!   ```

#[cfg(feature = "gui")]
mod gui;

fn main() -> std::process::ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    #[cfg(feature = "gui")]
    {
        gui::run()
    }

    #[cfg(not(feature = "gui"))]
    match tty::run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("greeter: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(not(feature = "gui"))]
mod tty {
    use bacak_greeter::{AuthStep, GreeterClient};
    use std::io::{self, BufRead, Write};

    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        let (mut client, welcome) = GreeterClient::connect_from_env()?;
        println!("== Bacak Display Manager ==");
        if welcome.touchscreen {
            println!("(touchscreen detected — GUI would show the virtual keyboard)");
        }

        let users = client.list_users()?;
        let sessions = client.list_sessions()?;

        if welcome.policy.greeter.show_user_list {
            println!("\nUsers:");
            for u in &users {
                let marker = if Some(&u.name) == welcome.last_user.as_ref() {
                    " (last)"
                } else {
                    ""
                };
                println!("  - {} <{}>{}", u.display_name(), u.name, marker);
            }
        }

        let username = prompt_line("\nUsername: ")?;
        if username.is_empty() {
            return Ok(());
        }

        // Authentication loop.
        let mut step = client.start_auth(&username)?;
        loop {
            match step {
                AuthStep::Prompt { message, echo } => {
                    let answer = if echo {
                        prompt_line(&message)?
                    } else {
                        prompt_secret(&message)?
                    };
                    step = client.answer(answer)?;
                }
                AuthStep::Info { message } => {
                    println!("{message}");
                    step = client.next_step()?;
                }
                AuthStep::Done { success, message } => {
                    if success {
                        println!("Authenticated.");
                        break;
                    }
                    println!("Login failed: {}", message.unwrap_or_default());
                    return Ok(());
                }
            }
        }

        println!("\nSessions:");
        for (i, s) in sessions.iter().enumerate() {
            println!("  [{i}] {} ({:?})", s.name, s.kind);
        }
        let idx = prompt_line("Session number (blank = default): ")?;
        let session_id = idx
            .parse::<usize>()
            .ok()
            .and_then(|i| sessions.get(i))
            .map(|s| s.id.clone())
            .unwrap_or_default();

        client.start_session(&session_id)?;
        println!("Starting session… (greeter exiting)");
        Ok(())
    }

    fn prompt_line(prompt: &str) -> io::Result<String> {
        print!("{prompt}");
        io::stdout().flush()?;
        let mut line = String::new();
        io::stdin().lock().read_line(&mut line)?;
        Ok(line.trim_end().to_string())
    }

    /// Read a secret with terminal echo disabled via `stty` when available.
    fn prompt_secret(prompt: &str) -> io::Result<String> {
        let echo_off = std::process::Command::new("stty")
            .arg("-echo")
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        let line = prompt_line(prompt);
        if echo_off {
            let _ = std::process::Command::new("stty").arg("echo").status();
            println!();
        }
        line
    }
}
