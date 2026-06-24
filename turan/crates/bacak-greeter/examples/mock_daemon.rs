//! Tiny mock of the BDM daemon side of the IPC protocol, for manually driving
//! the greeter (TTY or GUI) without root, PAM, or a compositor.
//!
//!   BDM_GREETER_SOCKET=/tmp/bdm.sock cargo run -p bacak-greeter --example mock_daemon &
//!   BDM_GREETER_SOCKET=/tmp/bdm.sock cargo run -p bacak-greeter --features gui
//!
//! Accepts the password "bacak". Logs every request it receives to stderr, so
//! you can see the greeter drive the conversation.

use bacak_common::ipc::{Request, Response, PROTOCOL_VERSION};
use bacak_common::sessions::{Session, SessionType};
use bacak_common::users::User;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};

fn main() {
    let sock = std::env::var("BDM_GREETER_SOCKET").expect("set BDM_GREETER_SOCKET");
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock).expect("bind socket");
    eprintln!("[mock-daemon] listening on {sock}");
    for stream in listener.incoming() {
        match stream {
            Ok(s) => handle(s),
            Err(e) => eprintln!("[mock-daemon] accept error: {e}"),
        }
    }
}

fn demo_users() -> Vec<User> {
    vec![
        User {
            uid: 1000,
            gid: 1000,
            name: "ayse".into(),
            full_name: Some("Ayşe Yılmaz".into()),
            home: "/home/ayse".into(),
            shell: "/bin/bash".into(),
        },
        User {
            uid: 1001,
            gid: 1001,
            name: "mehmet".into(),
            full_name: Some("Mehmet Demir".into()),
            home: "/home/mehmet".into(),
            shell: "/bin/zsh".into(),
        },
    ]
}

fn demo_sessions() -> Vec<Session> {
    vec![
        Session {
            id: "bacak".into(),
            name: "Bacak Desktop".into(),
            comment: None,
            exec: "bacak-session".into(),
            desktop_names: Some("Bacak".into()),
            kind: SessionType::Wayland,
            path: "/usr/share/wayland-sessions/bacak.desktop".into(),
        },
        Session {
            id: "gnome".into(),
            name: "GNOME".into(),
            comment: None,
            exec: "gnome-session".into(),
            desktop_names: Some("GNOME".into()),
            kind: SessionType::Wayland,
            path: "/usr/share/wayland-sessions/gnome.desktop".into(),
        },
    ]
}

fn handle(stream: UnixStream) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
    let mut writer = stream;
    let mut send = |resp: &Response| {
        let mut b = serde_json::to_vec(resp).unwrap();
        b.push(b'\n');
        let _ = writer.write_all(&b);
        let _ = writer.flush();
    };

    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let req: Request = match serde_json::from_str(line.trim_end()) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[mock-daemon] bad request: {e}");
                continue;
            }
        };
        match req {
            Request::Hello { protocol } => {
                eprintln!("[mock-daemon] Hello (v{protocol})");
                send(&Response::Welcome {
                    protocol: PROTOCOL_VERSION,
                    touchscreen: true,
                    last_user: Some("ayse".into()),
                    last_session: Some("bacak".into()),
                    policy: bacak_common::ipc::GreeterPolicy::default(),
                });
            }
            Request::ListUsers => {
                eprintln!("[mock-daemon] ListUsers");
                send(&Response::Users {
                    users: demo_users(),
                });
            }
            Request::ListSessions => {
                eprintln!("[mock-daemon] ListSessions");
                send(&Response::Sessions {
                    sessions: demo_sessions(),
                });
            }
            Request::StartAuth { username } => {
                eprintln!("[mock-daemon] StartAuth({username})");
                send(&Response::AuthPrompt {
                    message: "Password: ".into(),
                    echo: false,
                });
            }
            Request::AuthResponse { secret } => {
                let ok = secret.expose() == "bacak";
                eprintln!("[mock-daemon] AuthResponse (ok={ok})");
                send(&Response::AuthResult {
                    success: ok,
                    message: if ok {
                        None
                    } else {
                        Some("Authentication failed.".into())
                    },
                });
            }
            Request::CancelAuth => eprintln!("[mock-daemon] CancelAuth"),
            Request::StartSession { session_id } => {
                eprintln!("[mock-daemon] StartSession({session_id})");
                send(&Response::SessionStarting);
            }
            Request::Power { action } => {
                eprintln!("[mock-daemon] Power({action:?}) [ignored in mock]");
            }
        }
    }
    eprintln!("[mock-daemon] greeter disconnected");
}
