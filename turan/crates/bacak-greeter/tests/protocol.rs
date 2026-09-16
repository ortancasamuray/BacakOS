//! End-to-end IPC test: the real `GreeterClient` against a scripted daemon.
//!
//! No root, no PAM, no compositor — just the wire protocol over a temp socket,
//! proving the login conversation round-trips exactly as the daemon expects.

use bacak_common::ipc::{Request, Response, PROTOCOL_VERSION};
use bacak_greeter::{AuthStep, GreeterClient};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;

/// A minimal daemon stand-in that plays the happy path: hello → users →
/// auth (one masked prompt) → start session.
fn scripted_daemon(listener: UnixListener) {
    let (stream, _) = listener.accept().unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut writer = stream;

    let send = |w: &mut std::os::unix::net::UnixStream, resp: Response| {
        let mut b = serde_json::to_vec(&resp).unwrap();
        b.push(b'\n');
        w.write_all(&b).unwrap();
        w.flush().unwrap();
    };
    let recv = |r: &mut BufReader<std::os::unix::net::UnixStream>| -> Request {
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        serde_json::from_str(line.trim_end()).unwrap()
    };

    // Handshake.
    assert!(
        matches!(recv(&mut reader), Request::Hello { protocol } if protocol == PROTOCOL_VERSION)
    );
    send(
        &mut writer,
        Response::Welcome {
            protocol: PROTOCOL_VERSION,
            touchscreen: true,
            last_user: Some("ayse".into()),
            last_session: Some("bacak".into()),
            policy: bacak_common::ipc::GreeterPolicy::default(),
        },
    );

    // Users.
    assert!(matches!(recv(&mut reader), Request::ListUsers));
    send(
        &mut writer,
        Response::Users {
            users: vec![bacak_common::users::User {
                uid: 1000,
                gid: 1000,
                name: "ayse".into(),
                full_name: Some("Ayşe".into()),
                home: "/home/ayse".into(),
                shell: "/bin/bash".into(),
            }],
        },
    );

    // Auth: one masked prompt, correct answer, success.
    match recv(&mut reader) {
        Request::StartAuth { username } => assert_eq!(username, "ayse"),
        other => panic!("expected StartAuth, got {other:?}"),
    }
    send(
        &mut writer,
        Response::AuthPrompt {
            message: "Password: ".into(),
            echo: false,
        },
    );
    match recv(&mut reader) {
        Request::AuthResponse { secret } => assert_eq!(secret.expose(), "hunter2"),
        other => panic!("expected AuthResponse, got {other:?}"),
    }
    send(
        &mut writer,
        Response::AuthResult {
            success: true,
            message: None,
        },
    );

    // Start session.
    match recv(&mut reader) {
        Request::StartSession { session_id } => assert_eq!(session_id, "bacak"),
        other => panic!("expected StartSession, got {other:?}"),
    }
    send(&mut writer, Response::SessionStarting);
}

#[test]
fn full_login_flow_round_trips() {
    let dir = std::env::temp_dir().join(format!("bdm-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let sock = dir.join("greeter.sock");
    let _ = std::fs::remove_file(&sock);

    let listener = UnixListener::bind(&sock).unwrap();
    let server = std::thread::spawn(move || scripted_daemon(listener));

    let (mut client, welcome) = GreeterClient::connect(&sock).unwrap();
    assert!(welcome.touchscreen);
    assert_eq!(welcome.last_user.as_deref(), Some("ayse"));

    let users = client.list_users().unwrap();
    assert_eq!(users.len(), 1);
    assert_eq!(users[0].display_name(), "Ayşe");

    // Authenticate.
    let step = client.start_auth("ayse").unwrap();
    let step = match step {
        AuthStep::Prompt { echo, .. } => {
            assert!(!echo, "password prompt must be masked");
            client.answer("hunter2").unwrap()
        }
        other => panic!("expected Prompt, got {other:?}"),
    };
    match step {
        AuthStep::Done { success, .. } => assert!(success),
        other => panic!("expected Done(success), got {other:?}"),
    }

    client.start_session("bacak").unwrap();
    server.join().unwrap();
    let _ = std::fs::remove_file(&sock);
}
