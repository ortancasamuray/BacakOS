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

/// Daemon stand-in for "İptal" on the Bacak Onay code prompt: the cancel ends
/// the PAM conversation, the daemon reports the failed verdict, and the next
/// login must start from a clean slate.
fn cancelling_daemon(listener: UnixListener) {
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
    let prompt = |m: &str| Response::AuthPrompt { message: m.into(), echo: false };

    assert!(matches!(recv(&mut reader), Request::Hello { .. }));
    send(
        &mut writer,
        Response::Welcome {
            protocol: PROTOCOL_VERSION,
            touchscreen: true,
            last_user: None,
            last_session: None,
            policy: bacak_common::ipc::GreeterPolicy::default(),
        },
    );

    // First attempt: password, then the code prompt is cancelled.
    assert!(matches!(recv(&mut reader), Request::StartAuth { .. }));
    send(&mut writer, prompt("Parola:"));
    assert!(matches!(recv(&mut reader), Request::AuthResponse { .. }));
    send(&mut writer, Response::AuthInfo { message: "bacakonay:qr".into() });
    send(&mut writer, prompt("Bacak Onay kodu:"));
    assert!(matches!(recv(&mut reader), Request::CancelAuth));
    send(&mut writer, Response::AuthResult { success: false, message: Some("Kimlik doğrulama başarısız.".into()) });

    // Second attempt must begin with a fresh password prompt.
    assert!(matches!(recv(&mut reader), Request::StartAuth { .. }));
    send(&mut writer, prompt("Parola:"));
    match recv(&mut reader) {
        Request::AuthResponse { secret } => assert_eq!(secret.expose(), "hunter2"),
        other => panic!("expected AuthResponse, got {other:?}"),
    }
    send(&mut writer, Response::AuthResult { success: true, message: None });
}

#[test]
fn cancel_on_the_code_prompt_leaves_the_next_login_in_step() {
    let dir = std::env::temp_dir().join(format!("bdm-test-iptal-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let sock = dir.join("greeter.sock");
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock).unwrap();
    let server = std::thread::spawn(move || cancelling_daemon(listener));

    let (mut client, _) = GreeterClient::connect(&sock).unwrap();
    assert!(matches!(client.start_auth("ayse").unwrap(), AuthStep::Prompt { .. }));
    assert!(matches!(client.answer("hunter2").unwrap(), AuthStep::Info { .. }));
    assert!(matches!(client.next_step().unwrap(), AuthStep::Prompt { .. }));
    client.cancel_auth().unwrap();

    match client.start_auth("ayse").unwrap() {
        AuthStep::Prompt { message, .. } => assert_eq!(message, "Parola:"),
        other => panic!("second login out of step: {other:?}"),
    }
    assert!(matches!(client.answer("hunter2").unwrap(), AuthStep::Done { success: true, .. }));
    server.join().unwrap();
    let _ = std::fs::remove_file(&sock);
}
