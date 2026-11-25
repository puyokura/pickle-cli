//! Contract tests against a local fixture server.
//!
//! The live service cannot be driven without an SMS code, so the request and
//! response shapes are pinned against a stand-in that speaks the same JSON.
//! This is the "integration" half of the suite: the binary runs as a real
//! process, the HTTP client is real, only the origin is local.
//!
//! Every fixture below mirrors a shape observed in the decompiled client
//! (`docs/api.md`, `docs/session.md`, `docs/chat.md`); the ones marked
//! "measured" were also confirmed against the live service.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_pickle"))
}

/// One canned response the fixture server will hand out.
struct Reply {
    status: u16,
    body: String,
}

impl Reply {
    fn ok(body: &str) -> Self {
        Self {
            status: 200,
            body: body.to_string(),
        }
    }
    fn status(code: u16, body: &str) -> Self {
        Self {
            status: code,
            body: body.to_string(),
        }
    }
}

/// Serve `replies` in order on a loopback port, then stop.
///
/// Returns the base URL and a handle that resolves when the server is finished.
/// Requests are recorded so a test can assert on what was actually sent.
///
/// **The number of replies must match the number of requests the CLI makes.
/// Too few makes `join()` block forever, because the thread waits inside
/// `accept`. A test that expects no request must pass an empty vector.**
fn fixture(replies: Vec<Reply>) -> (String, thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let port = listener.local_addr().expect("addr").port();
    // Every expected reply needs a real request; a test that expects none must
    // pass an empty `replies` vector. `accept` blocks, so there is no timeout
    // here on purpose: a bounded wait would need a watchdog thread, and a
    // decorative check would only hide the mistake.
    let handle = thread::spawn(move || {
        let mut seen = Vec::new();
        for reply in replies {
            let Ok((mut stream, _)) = listener.accept() else {
                break;
            };
            // Read the request head, and the body when Content-Length is set.
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            let mut head = String::new();
            let mut length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                if let Some(value) = line
                    .to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(str::trim)
                {
                    length = value.trim().parse().unwrap_or(0);
                }
                head.push_str(&line);
                if line == "\r\n" {
                    break;
                }
            }
            let mut body = vec![0u8; length];
            if length > 0 {
                let _ = reader.read_exact(&mut body);
            }
            seen.push(format!("{head}{}", String::from_utf8_lossy(&body)));

            // `Connection: close` plus an explicit shutdown: the client must not
            // be left waiting for a body on a socket the fixture is about to drop.
            let response = format!(
                "HTTP/1.1 {} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                reply.status,
                reply.body.len(),
                reply.body
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
            // Half-close and finish: the client has all bytes and sees EOF.
            // Draining here would block on a connection the client keeps open,
            // which turned one hanging test into a two minute suite.
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
        seen
    });
    (format!("http://127.0.0.1:{port}"), handle)
}

/// Write a state file pointing at `base` with an optional stored session.
fn state_at(base: &str, signed_in: bool) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let unique = NEXT.fetch_add(1, Ordering::SeqCst);
    let mut path = std::env::temp_dir();
    path.push(format!(
        "pickle-fixture-{}-{unique}.json",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    // A syntactically valid JWT so the expiry check has something to read.
    let jwt = "eyJhbGciOiJub25lIn0.eyJzdWIiOiJmaXh0dXJlIiwiZXhwIjoyMDAwMDAwMDAwfQ.sig";
    let session = if signed_in {
        format!(
            r#"{{"user_id":"fixture-user","login_session_token":"fixture-token","login_session_jwt":"{jwt}","expires_at":"2033-05-18T00:00:00Z"}}"#
        )
    } else {
        "null".to_string()
    };
    let body = format!(
        r#"{{"env":"prod","api_base":"{base}","device_id":"fixture-device","session":{session},"pending_phone_code":null,"bootstrap":null,"base_resolved_at":{resolved_at}}}"#,
        resolved_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    );
    std::fs::write(&path, body).expect("write state");
    path
}

fn run_with(state: &PathBuf, args: &[&str]) -> (i32, String, String) {
    let output = Command::new(bin())
        .args(args)
        .env("PICKLE_CLI_STATE", state)
        .output()
        .expect("spawn pickle");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

/// Run the CLI with text on stdin, for the one-command login flow and `--stdin`.
fn run_with_stdin(state: &PathBuf, args: &[&str], input: &str) -> (i32, String, String) {
    use std::io::Write;
    let mut child = Command::new(bin())
        .args(args)
        .env("PICKLE_CLI_STATE", state)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn pickle");
    {
        let stdin = child.stdin.as_mut().expect("stdin");
        stdin.write_all(input.as_bytes()).expect("write stdin");
    }
    let output = child.wait_with_output().expect("wait");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

#[test]
fn sync_parses_the_room_list() {
    let (base, handle) = fixture(vec![Reply::ok(
        r#"{"v":1,"rev":7,"rooms":[{"room_id":"r1","kind":"character_direct","title":"A","last_seq":3}]}"#,
    )]);
    let state = state_at(&base, true);
    let (code, out, err) = run_with(&state, &["rooms", "--json"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("\"count\":1"), "stdout: {out}");
    assert!(out.contains("\"room_id\":\"r1\""), "stdout: {out}");

    let seen = handle.join().expect("fixture join");
    assert_eq!(seen.len(), 1);
    assert!(
        seen[0].starts_with("GET /api/sync "),
        "request: {}",
        seen[0]
    );
    // Measured: `login-sessions*` answers a CDN 403 without X-Client-Build,
    // even when X-Client-Version is present. The rest of this set is the app's
    // own interceptor (アプリ側の実装, アプリ側の実装).
    let head = seen[0].to_ascii_lowercase();
    for header in [
        "x-client-version: 1.1.16",
        "x-client-build: 428293",
        "x-client-platform: android",
        "x-client-timezone: ",
        "x-pickle-sign-in-flow: 2",
        "x-pickle-onboarding-flow: 4",
        "x-pickle-language: en",
        "x-pickle-language-source: device_language",
        "x-pickle-store-country-resolved: ",
        // Storefront and device region are omitted unless the host locale or the
        // PICKLE_* override names a market in the app's table (us/kr/jp).
        "authorization: bearer ",
    ] {
        assert!(
            head.contains(header),
            "missing header {header:?} in: {head}"
        );
    }
    let _ = std::fs::remove_file(&state);
}

#[test]
fn storefront_headers_follow_the_app_market_table() {
    // A Japanese locale must yield jp for both the storefront and the region.
    let (base, handle) = fixture(vec![Reply::ok(r#"{"v":1,"rooms":[]}"#)]);
    let state = state_at(&base, true);
    let output = Command::new(bin())
        .args(["rooms", "--json"])
        .env("PICKLE_CLI_STATE", &state)
        .env("PICKLE_STOREFRONT", "ja_JP.UTF-8")
        .env("PICKLE_DEVICE_REGION", "ja_JP.UTF-8")
        .output()
        .expect("spawn pickle");
    assert_eq!(output.status.code(), Some(0), "stderr: {:?}", output.stderr);
    let seen = handle.join().expect("fixture join");
    let head = seen[0].to_ascii_lowercase();
    assert!(head.contains("x-pickle-storefront-country: jp"), "{head}");
    assert!(head.contains("x-pickle-device-region: jp"), "{head}");
    assert!(
        head.contains("x-pickle-store-country-resolved: true"),
        "{head}"
    );
    let _ = std::fs::remove_file(&state);

    // A market the app does not know must omit the storefront and say so.
    let (base, handle) = fixture(vec![Reply::ok(r#"{"v":1,"rooms":[]}"#)]);
    let state = state_at(&base, true);
    let output = Command::new(bin())
        .args(["rooms", "--json"])
        .env("PICKLE_CLI_STATE", &state)
        .env("PICKLE_STOREFRONT", "de_DE.UTF-8")
        .env("PICKLE_DEVICE_REGION", "de_DE.UTF-8")
        .output()
        .expect("spawn pickle");
    assert_eq!(output.status.code(), Some(0));
    let seen = handle.join().expect("fixture join");
    let head = seen[0].to_ascii_lowercase();
    assert!(!head.contains("x-pickle-storefront-country"), "{head}");
    assert!(
        head.contains("x-pickle-store-country-resolved: false"),
        "{head}"
    );
    let _ = std::fs::remove_file(&state);
}

#[test]
fn credentials_can_come_from_stdin_instead_of_argv() {
    // A phone number and an OTP code in argv land in the shell history file,
    // which is written before the command runs. Both must be readable from the
    // pipe instead, with no trace in argv or on disk.
    let (base, handle) = fixture(vec![Reply::ok(
        r#"{"pending_id":"pend-s","expires_in":600}"#,
    )]);
    let state = state_at(&base, false);
    let mut child = Command::new(bin())
        .args(["login", "--start", "--stdin", "--region", "jp", "--json"])
        .env("PICKLE_CLI_STATE", &state)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn");
    {
        let mut stdin = child.stdin.take().expect("stdin");
        writeln!(stdin, "+819012345678").expect("write");
    }
    let mut stdout = String::new();
    child
        .stdout
        .take()
        .expect("stdout")
        .read_to_string(&mut stdout)
        .expect("read");
    child.wait().expect("wait");
    assert!(stdout.contains("pend-s"), "stdout: {stdout}");
    assert!(!stdout.contains("+819012345678"), "number echoed: {stdout}");
    let persisted = std::fs::read_to_string(&state).expect("state");
    assert!(
        !persisted.contains("+819012345678"),
        "number persisted: {persisted}"
    );
    let seen = handle.join().expect("fixture join");
    assert!(
        seen[0].contains("\"phone\":\"+819012345678\""),
        "the number must still reach the server: {}",
        seen[0]
    );
    let _ = std::fs::remove_file(&state);
}

#[test]
fn history_delta_rejects_a_missing_version_field() {
    // Otherwise well formed, but without `v`: the guard must name that field.
    let (base, handle) = fixture(vec![Reply::ok(
        r#"{"items":[],"reactions":[],"card_states":[],"next_after":0,"has_more":false,"head_seq":0,"projection_version":1}"#,
    )]);
    let state = state_at(&base, true);
    let (code, _, err) = run_with(&state, &["history", "r1", "--mode", "delta", "--json"]);
    assert_eq!(code, 1, "expected the v=1 guard to fire");
    assert!(err.contains("missing v"), "stderr: {err}");
    let seen = handle.join().expect("fixture join");
    assert!(
        seen[0].starts_with("GET /api/rooms/r1/history/delta?after_seq=0&limit=50"),
        "request line: {}",
        seen[0]
    );
    let _ = std::fs::remove_file(&state);
}

#[test]
fn full_history_uses_the_measured_snapshot_shape() {
    // Measured on the live service: api/rooms/{id}/history answers `v` plus
    // `messages`, not the `rooms` shape of api/sync.
    let (base, handle) = fixture(vec![Reply::ok(
        r#"{"v":1,"messages":[],"reactions":[],"card_states":[],"window_max_seq":211,"log_epoch":"e","projection_version":6,"read_seq":193}"#,
    )]);
    let state = state_at(&base, true);
    let (code, out, err) = run_with(&state, &["history", "r1", "--mode", "full", "--json"]);
    assert_eq!(code, 0, "full history: {err}");
    assert!(out.contains("messages"), "stdout: {out}");
    let seen = handle.join().expect("fixture join");
    assert!(
        seen[0].starts_with("GET /api/rooms/r1/history "),
        "request: {}",
        seen[0]
    );
    let _ = std::fs::remove_file(&state);
}

#[test]
fn full_history_rejects_a_missing_version() {
    // `v` must be 1 (measured).
    let (base, handle) = fixture(vec![Reply::ok(r#"{"messages":[]}"#)]);
    let state = state_at(&base, true);
    let (code, _, err) = run_with(&state, &["history", "r1", "--mode", "full", "--json"]);
    assert_eq!(code, 1, "expected rejection");
    assert!(err.contains("missing v"), "stderr: {err}");
    let _ = handle.join().expect("fixture join");
    let _ = std::fs::remove_file(&state);
}

#[test]
fn full_history_rejects_a_body_without_messages() {
    let (base, handle) = fixture(vec![Reply::ok(r#"{"v":1,"rooms":[]}"#)]);
    let state = state_at(&base, true);
    let (code, _, err) = run_with(&state, &["history", "r1", "--mode", "full", "--json"]);
    assert_eq!(code, 1, "expected rejection");
    assert!(err.contains("messages"), "stderr: {err}");
    let _ = handle.join().expect("fixture join");
    let _ = std::fs::remove_file(&state);
}

#[test]
fn send_posts_the_documented_body_keys() {
    let (base, handle) = fixture(vec![Reply::ok(
        r#"{"status":"ok","seq":11,"responder":"character"}"#,
    )]);
    let state = state_at(&base, true);
    let (code, out, err) = run_with(
        &state,
        &[
            "send",
            "r1",
            "--set",
            "text=hello",
            "--set",
            "client_msg_id=cmid-1",
            "--json",
        ],
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("\"seq\":11"), "stdout: {out}");

    let seen = handle.join().expect("fixture join");
    assert!(
        seen[0].starts_with("POST /api/rooms/r1/messages "),
        "request: {}",
        seen[0]
    );
    assert!(seen[0].contains("\"text\":\"hello\""), "body: {}", seen[0]);
    assert!(
        seen[0].contains("\"client_msg_id\":\"cmid-1\""),
        "body: {}",
        seen[0]
    );
    // No attachments were supplied, so the key must be absent entirely.
    assert!(!seen[0].contains("\"attachments\""), "body: {}", seen[0]);
    let _ = std::fs::remove_file(&state);
}

#[test]
fn a_refreshed_jwt_is_written_back_to_the_state_file() {
    // A refresh kept only in memory is lost on exit, so every run refreshes
    // again. The refreshed value must reach the file.
    //
    // The fixture answers /api/login-sessions/refresh, and the state below
    // carries an already-expired JWT so `auth` must refresh before the first
    // request.
    use base64::Engine;
    let payload =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"sub":"f","exp":1000000000}"#);
    let expired = format!("eyJhbGciOiJub25lIn0.{payload}.sig");

    let (base, handle) = fixture(vec![
        Reply::ok(
            r#"{"login_session_jwt":"eyJhbGciOiJub25lIn0.eyJzdWIiOiJmIiwiZXhwIjoyMDAwMDAwMDAwfQ.refreshed","expires_at":"2033-05-18T00:00:00Z"}"#,
        ),
        Reply::ok(r#"{"v":1,"rooms":[],"rev":1}"#),
    ]);
    let state = state_at(&base, true);
    // Replace the fixture's JWT with an expired one.
    let mut value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state).expect("state")).expect("json");
    value["session"]["login_session_jwt"] = serde_json::json!(expired);
    std::fs::write(&state, value.to_string()).expect("rewrite");

    let (code, _, err) = run_with(&state, &["rooms", "--json"]);
    assert_eq!(code, 0, "rooms after refresh: {err}");

    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state).expect("state")).expect("json");
    assert_ne!(
        after["session"]["login_session_jwt"],
        serde_json::json!(expired),
        "the refreshed JWT was not persisted"
    );
    assert_eq!(
        after["session"]["expires_at"],
        serde_json::json!("2033-05-18T00:00:00Z")
    );

    let seen = handle.join().expect("fixture join");
    assert_eq!(
        seen.len(),
        2,
        "expected refresh then sync: {:?}",
        seen.len()
    );
    assert!(
        seen[0].starts_with("POST /api/login-sessions/refresh "),
        "first request: {}",
        seen[0]
    );
    let _ = std::fs::remove_file(&state);
}

#[test]
fn each_send_gets_its_own_idempotency_key() {
    // Reusing `client_msg_id` across sends lets the server dedupe the second
    // message as a retry of the first, which silently loses a message.
    let (base, handle) = fixture(vec![
        Reply::ok(r#"{"status":"ok","seq":6}"#),
        Reply::ok(r#"{"status":"ok","seq":7}"#),
    ]);
    let state = state_at(&base, true);
    let (code, _, err) = run_with(&state, &["send", "r1", "--set", "text=one", "--json"]);
    assert_eq!(code, 0, "first send: {err}");
    let (code, _, err) = run_with(&state, &["send", "r1", "--set", "text=two", "--json"]);
    assert_eq!(code, 0, "second send: {err}");

    let seen = handle.join().expect("fixture join");
    assert_eq!(seen.len(), 2);
    let id_of = |request: &str| {
        request
            .split("\"client_msg_id\":\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .unwrap_or_default()
            .to_string()
    };
    let first = id_of(&seen[0]);
    let second = id_of(&seen[1]);
    assert!(!first.is_empty(), "no client_msg_id in: {}", seen[0]);
    assert!(!second.is_empty(), "no client_msg_id in: {}", seen[1]);
    assert_ne!(first, second, "the idempotency key was reused");
    let _ = std::fs::remove_file(&state);
}

#[test]
fn session_parses_required_keys_and_stores_none_of_the_token() {
    let (base, handle) = fixture(vec![Reply::ok(
        r#"{"session_id":"s1","room":"room-1","livekit_url":"wss://lk","participant_identity":"p1","account_rev":3,"user_id":"u1","plan":"free","plan_rank":0,"token":"livekit-secret","character_name":"Char"}"#,
    )]);
    let state = state_at(&base, true);
    let (code, out, err) = run_with(&state, &["session", "--json"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("\"session_id\":\"s1\""), "stdout: {out}");
    // The LiveKit token is short-lived and must never be printed.
    assert!(
        !out.contains("livekit-secret"),
        "token leaked to stdout: {out}"
    );

    let seen = handle.join().expect("fixture join");
    assert!(
        seen[0].starts_with("POST /api/session "),
        "request: {}",
        seen[0]
    );
    assert!(
        seen[0].contains("\"room_demand\":\"none\""),
        "body: {}",
        seen[0]
    );
    let _ = std::fs::remove_file(&state);
}

#[test]
fn server_422_detail_is_flattened_for_the_caller() {
    let (base, handle) = fixture(vec![Reply::status(
        422,
        r#"{"detail":[{"type":"literal_error","loc":["body","room_demand"],"msg":"Input should be 'none', 'voice', 'motion' or 'world'"}]}"#,
    )]);
    let state = state_at(&base, true);
    // Bypass the client-side guard so the server-side body is what surfaces.
    let (code, _, err) = run_with(
        &state,
        &[
            "raw",
            "api/session",
            "--method",
            "POST",
            "--set",
            "room_demand=voice",
            "--confirm",
            "--json",
        ],
    );
    assert_eq!(code, 1, "expected 422 to be an error");
    assert!(
        err.contains("body.room_demand"),
        "detail was not flattened: {err}"
    );
    assert!(
        err.contains("'none'"),
        "detail lost the allowed values: {err}"
    );
    let _ = handle.join().expect("fixture join");
    let _ = std::fs::remove_file(&state);
}

#[test]
fn events_reads_frames_from_a_stream() {
    // The SSE body is served as a normal response; the parser does not care.
    let sse = "event: account\ndata: {\"rev\":1}\n\nevent: read\ndata: 7\n\n";
    let (base, handle) = fixture(vec![Reply::ok(sse)]);
    let state = state_at(&base, true);
    let (code, out, err) = run_with(
        &state,
        &["events", "--frames", "2", "--timeout", "5", "--json"],
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("\"count\":2"), "stdout: {out}");
    assert!(out.contains("account"), "stdout: {out}");
    assert!(out.contains("read"), "stdout: {out}");
    let seen = handle.join().expect("fixture join");
    assert!(
        seen[0].starts_with("GET /api/events "),
        "request: {}",
        seen[0]
    );
    assert!(
        seen[0].contains("text/event-stream"),
        "accept header: {}",
        seen[0]
    );
    let _ = std::fs::remove_file(&state);
}

#[test]
fn a_uuid_shaped_ref_skips_pending_registration() {
    // Rule from docs/attachments.md §1.2: a UUID-shaped ref is already tracked
    // by another route, so registering it would double-track the object.
    let uuid_ref = "3f2b8c1e-0000-4000-8000-000000000001";
    let (base, handle) = fixture(vec![Reply::ok(&format!(
        r#"{{"status":"ok","ref":"{uuid_ref}"}}"#
    ))]);
    let state = state_at(&base, true);
    let mut file = std::env::temp_dir();
    file.push(format!("pickle-uuid-{}.txt", std::process::id()));
    std::fs::write(&file, b"bytes").expect("write");
    let (code, out, err) = run_with(&state, &["upload", file.to_str().expect("path"), "--json"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        out.contains("\"pending_registered\":false"),
        "stdout: {out}"
    );
    let seen = handle.join().expect("fixture join");
    assert_eq!(seen.len(), 1, "no registration expected for a UUID ref");
    let _ = std::fs::remove_file(&state);
    let _ = std::fs::remove_file(&file);
}

#[test]
fn an_image_omits_pending_zero_but_a_video_sends_it() {
    // docs/attachments.md §1.1: the app sends `pending=0` only when its own
    // `pending` flag is off, which happens for video attachments. An image
    // stays pending so the post resolves it.
    let (base, handle) = fixture(vec![
        Reply::ok(r#"{"status":"ok","ref":"image-ref"}"#),
        Reply::ok(r#"{"status":"ok"}"#),
    ]);
    let state = state_at(&base, true);
    let mut image = std::env::temp_dir();
    image.push(format!("pickle-img-{}.png", std::process::id()));
    std::fs::write(&image, b"png").expect("write");
    let (code, out, err) = run_with(&state, &["upload", image.to_str().expect("path"), "--json"]);
    assert_eq!(code, 0, "image upload: {err}");
    assert!(out.contains("\"pending_zero_sent\":false"), "stdout: {out}");
    let seen = handle.join().expect("fixture join");
    assert!(
        !seen[0].contains("pending=0"),
        "image must not send pending=0: {}",
        seen[0]
    );
    let _ = std::fs::remove_file(&image);

    // A video takes the opposite path.
    let (base, handle) = fixture(vec![Reply::ok(r#"{"status":"ok","ref":"video-ref"}"#)]);
    let state = state_at(&base, true);
    let mut video = std::env::temp_dir();
    video.push(format!("pickle-vid-{}.mp4", std::process::id()));
    std::fs::write(&video, b"mp4").expect("write");
    let (code, out, err) = run_with(&state, &["upload", video.to_str().expect("path"), "--json"]);
    assert_eq!(code, 0, "video upload: {err}");
    assert!(out.contains("\"pending_zero_sent\":true"), "stdout: {out}");
    let seen = handle.join().expect("fixture join");
    assert!(
        seen[0].contains("pending=0"),
        "video must send pending=0: {}",
        seen[0]
    );
    let _ = std::fs::remove_file(&video);
    let _ = std::fs::remove_file(&state);
}

#[test]
fn a_blob_upload_never_registers_pending() {
    // Only `target=attachments` takes the second step.
    let (base, handle) = fixture(vec![Reply::ok(r#"{"status":"ok","blob_ref":"blob-1"}"#)]);
    let state = state_at(&base, true);
    let mut file = std::env::temp_dir();
    file.push(format!("pickle-blob-{}.png", std::process::id()));
    std::fs::write(&file, b"png").expect("write");
    let (code, _, err) = run_with(
        &state,
        &[
            "upload",
            file.to_str().expect("path"),
            "--target",
            "blobs",
            "--json",
        ],
    );
    assert_eq!(code, 0, "stderr: {err}");
    let seen = handle.join().expect("fixture join");
    assert_eq!(seen.len(), 1, "blobs must not hit api/attachments/pending");
    assert!(
        seen[0].starts_with("POST /api/blobs?"),
        "request: {}",
        seen[0]
    );
    let _ = std::fs::remove_file(&state);
    let _ = std::fs::remove_file(&file);
}

#[test]
fn history_delta_rejects_a_stalled_response() {
    // No `next_after`: the poll loop would never advance. The app's own guard
    // only checks `v`, so this is stricter on purpose.
    let (base, handle) = fixture(vec![Reply::ok(
        r#"{"v":1,"items":[],"reactions":[],"card_states":[],"has_more":false,"head_seq":0,"projection_version":1}"#,
    )]);
    let state = state_at(&base, true);
    let (code, _, err) = run_with(&state, &["history", "r1", "--mode", "delta", "--json"]);
    assert_eq!(code, 1, "expected rejection");
    assert!(err.contains("next_after"), "stderr: {err}");
    let _ = handle.join().expect("fixture join");
    let _ = std::fs::remove_file(&state);
}

#[test]
fn history_page_uses_messages_not_items() {
    // ChatHistoryPage names its list `messages`, unlike delta's `items`.
    let (base, handle) = fixture(vec![Reply::ok(
        r#"{"v":1,"messages":[{"id":"m1"}],"has_more":false,"next_before":7}"#,
    )]);
    let state = state_at(&base, true);
    let (code, out, err) = run_with(
        &state,
        &["history", "r1", "--mode", "page", "--limit", "5", "--json"],
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("m1"), "stdout: {out}");
    let seen = handle.join().expect("fixture join");
    assert!(
        seen[0].starts_with("GET /api/rooms/r1/history/page?limit=5"),
        "request: {}",
        seen[0]
    );
    let _ = std::fs::remove_file(&state);
}

#[test]
fn a_zero_frame_count_is_a_usage_error() {
    // It used to be clamped to 1, which silently answered the wrong question.
    //
    // Zero replies on purpose: the usage error fires before any request, so the
    // fixture thread must not be waiting for one.
    let (base, handle) = fixture(vec![]);
    let state = state_at(&base, true);
    let (code, _, err) = run_with(&state, &["events", "--frames", "0", "--json"]);
    assert_eq!(code, 1, "expected a usage error, got exit {code}");
    assert!(err.contains("at least 1"), "stderr: {err}");
    let seen = handle.join().expect("fixture join");
    assert!(
        seen.is_empty(),
        "no request should have been sent: {seen:?}"
    );
    let _ = std::fs::remove_file(&state);
}

#[test]
fn event_frames_follow_the_app_parsing_rules() {
    // Multi-line data joins with a newline; a frame without an event name or
    // without data is dropped; a comment line is skipped; CRLF is accepted.
    let sse = concat!(
        ": a comment with retry: 100\n",
        "\n",
        "event: account\n",
        "data: line-one\n",
        "data: line-two\n",
        "\n",
        "data: orphan\n",
        "\n",
        "event: nameless\n",
        "\n",
        "event: second\r\n",
        "data: tail\r\n",
        "\r\n",
    );
    let (base, handle) = fixture(vec![Reply::ok(sse)]);
    let state = state_at(&base, true);
    // `--frames` is clamped to >= 1, so ask for more than the stream holds and
    // let the stream end drive the stop instead of a count of zero.
    let (code, out, err) = run_with(
        &state,
        &["events", "--frames", "50", "--timeout", "10", "--json"],
    );
    assert_eq!(code, 0, "stderr: {err}");
    let parsed: serde_json::Value = serde_json::from_str(&out).expect("json");
    let frames = parsed["frames"].as_array().expect("frames");
    // Only the two well-formed frames survive.
    assert_eq!(frames.len(), 2, "frames: {out}");
    assert_eq!(frames[0]["event"], serde_json::json!("account"));
    assert_eq!(frames[0]["data"], serde_json::json!("line-one\nline-two"));
    assert_eq!(frames[1]["event"], serde_json::json!("second"));
    assert_eq!(frames[1]["data"], serde_json::json!("tail"));
    let _ = handle.join().expect("fixture join");
    let _ = std::fs::remove_file(&state);
}

#[test]
fn events_tolerates_a_quiet_stream() {
    // The 30s total timeout used for the REST calls would kill a subscription
    // that stays quiet. The streaming client uses a 60s read timeout instead,
    // so a short silence must not end the read.
    let sse = "event: account\ndata: {}\n\n";
    let (base, handle) = fixture(vec![Reply::ok(sse)]);
    let state = state_at(&base, true);
    let (code, out, err) = run_with(
        &state,
        &["events", "--frames", "1", "--timeout", "10", "--json"],
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("account"), "stdout: {out}");
    assert!(out.contains("\"timed_out\":false"), "stdout: {out}");
    let seen = handle.join().expect("fixture join");
    assert!(
        seen[0].starts_with("GET /api/events "),
        "request: {}",
        seen[0]
    );
    let _ = std::fs::remove_file(&state);
}

#[test]
fn upload_sends_a_raw_body_with_query_keys() {
    let (base, handle) = fixture(vec![
        Reply::ok(r#"{"status":"ok","ref":"ref-1"}"#),
        Reply::ok(r#"{"status":"ok"}"#),
    ]);
    let state = state_at(&base, true);
    let mut file = std::env::temp_dir();
    file.push(format!("pickle-upload-{}.txt", std::process::id()));
    std::fs::write(&file, b"fixture bytes").expect("write upload");

    let (code, out, err) = run_with(&state, &["upload", file.to_str().expect("path"), "--json"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("\"ref\":\"ref-1\""), "stdout: {out}");

    let seen = handle.join().expect("fixture join");
    assert_eq!(seen.len(), 2, "expected upload + pending registration");
    assert!(
        seen[0].starts_with("POST /api/attachments?filename="),
        "request: {}",
        seen[0]
    );
    // Second step: the non-UUID ref must be registered.
    assert!(
        seen[1].starts_with("POST /api/attachments/pending "),
        "registration: {}",
        seen[1]
    );
    assert!(
        seen[1].contains("\"ref\":\"ref-1\""),
        "registration body: {}",
        seen[1]
    );
    assert!(
        seen[1].contains("\"upload_id\":\""),
        "registration body: {}",
        seen[1]
    );
    assert!(seen[0].contains("upload_id="), "request: {}", seen[0]);
    // Raw body, not multipart.
    assert!(
        !seen[0].contains("multipart/form-data"),
        "request: {}",
        seen[0]
    );
    assert!(seen[0].contains("fixture bytes"), "body missing bytes");
    let _ = std::fs::remove_file(&state);
    let _ = std::fs::remove_file(&file);
}

#[test]
fn mcp_tools_reach_the_fixture_and_return_json() {
    let (base, handle) = fixture(vec![Reply::ok(
        r#"{"v":1,"rev":1,"rooms":[{"room_id":"r9","title":"Z"}]}"#,
    )]);
    let state = state_at(&base, true);
    let mut child = Command::new(bin())
        .arg("mcp")
        .env("PICKLE_CLI_STATE", &state)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn mcp");
    {
        let mut stdin = child.stdin.take().expect("stdin");
        writeln!(
            stdin,
            r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"pickle_rooms","arguments":{{}}}}}}"#
        )
        .expect("write");
    }
    let mut stdout = String::new();
    child
        .stdout
        .take()
        .expect("stdout")
        .read_to_string(&mut stdout)
        .expect("read");
    child.wait().expect("wait");
    assert!(
        stdout.contains("r9"),
        "tool did not return the room: {stdout}"
    );
    assert!(stdout.contains("\"isError\":false"), "stdout: {stdout}");
    let seen = handle.join().expect("fixture join");
    assert_eq!(seen.len(), 1);
    let _ = std::fs::remove_file(&state);
}

#[test]
fn fixture_harness_itself_works() {
    // Guards against a broken harness making every other test vacuously pass:
    // one plain request must reach the fixture and come back.
    let (base, handle) = fixture(vec![Reply::ok("{\"ok\":true}")]);
    use std::io::Write as _;
    let mut stream =
        std::net::TcpStream::connect(base.trim_start_matches("http://")).expect("connect fixture");
    write!(
        stream,
        "GET /probe HTTP/1.1\r\nHost: fixture\r\nConnection: close\r\n\r\n"
    )
    .expect("write request");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read response");
    assert!(response.contains("HTTP/1.1 200"), "response: {response}");
    assert!(response.contains("{\"ok\":true}"), "response: {response}");
    let requests = handle.join().expect("fixture join");
    assert_eq!(requests.len(), 1, "fixture did not receive a request");
    assert!(
        requests[0].starts_with("GET /probe "),
        "request: {}",
        requests[0]
    );
}

#[test]
fn login_flow_persists_the_session_between_processes() {
    // Two replies: the phone-code request, then the exchange.
    let (base, handle) = fixture(vec![
        Reply::ok(r#"{"pending_id":"pend-1","expires_in":600}"#),
        Reply::ok(
            r#"{"status":"issued","user_id":"u9","login_session_token":"tok-9","login_session_jwt":"eyJhbGciOiJub25lIn0.eyJzdWIiOiJ1OSIsImV4cCI6MjAwMDAwMDAwMH0.sig","expires_at":"2033-05-18T00:00:00Z"}"#,
        ),
    ]);
    let state = state_at(&base, false);

    let (code, out, err) = run_with(
        &state,
        &[
            "login",
            "--start",
            "+819012345678",
            "--region",
            "jp",
            "--json",
        ],
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("pend-1"), "stdout: {out}");
    // Privacy: the number is not echoed back and not written to disk.
    assert!(
        !out.contains("+819012345678"),
        "the phone number was echoed: {out}"
    );
    let persisted = std::fs::read_to_string(&state).expect("state");
    assert!(
        !persisted.contains("+819012345678"),
        "the phone number was written to the state file: {persisted}"
    );

    // The exchange runs as a second process: pending_id must come from the file.
    let (code, out, err) = run_with(&state, &["login", "--finish", "123456", "--json"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("\"user_id\":\"u9\""), "stdout: {out}");

    // And the session must now be visible to a third process.
    let (code, out, _) = run_with(&state, &["status", "--json"]);
    assert_eq!(code, 0);
    assert!(
        out.contains("\"signed_in\":true"),
        "status after login: {out}"
    );

    let seen = handle.join().expect("fixture join");
    assert_eq!(seen.len(), 2);
    assert!(
        seen[0].starts_with("POST /api/login-sessions/phone-code "),
        "1st: {}",
        seen[0]
    );
    assert!(
        seen[0].contains("\"phone\":\"+819012345678\""),
        "1st body: {}",
        seen[0]
    );
    assert!(
        seen[1].starts_with("POST /api/login-sessions "),
        "2nd: {}",
        seen[1]
    );
    assert!(
        seen[1].contains("\"kind\":\"phone_code\""),
        "2nd body: {}",
        seen[1]
    );
    assert!(
        seen[1].contains("\"pending_id\":\"pend-1\""),
        "2nd body: {}",
        seen[1]
    );
    let _ = std::fs::remove_file(&state);
}

#[test]
fn react_read_mute_and_pin_send_the_documented_bodies() {
    // Four write calls, four replies.
    let (base, handle) = fixture(vec![
        Reply::ok(r#"{"status":"ok"}"#),
        Reply::ok("{}"),
        Reply::ok("{}"),
        Reply::ok("{}"),
    ]);
    let state = state_at(&base, true);

    let (code, _, err) = run_with(
        &state,
        &[
            "react",
            "r1",
            "--set",
            "target_id=m1",
            "--set",
            "target_seq=4",
            "--set",
            "emoji=👍",
            "--set",
            "client_msg_id=react-1",
            "--json",
        ],
    );
    assert_eq!(code, 0, "react stderr: {err}");
    let (code, _, err) = run_with(&state, &["read", "r1", "--seq", "12", "--json"]);
    assert_eq!(code, 0, "read stderr: {err}");
    let (code, _, err) = run_with(&state, &["mute", "r1", "--set", "muted=true", "--json"]);
    assert_eq!(code, 0, "mute stderr: {err}");
    let (code, _, err) = run_with(&state, &["pin", "r1", "--set", "pinned=true", "--json"]);
    assert_eq!(code, 0, "pin stderr: {err}");

    let seen = handle.join().expect("fixture join");
    assert_eq!(seen.len(), 4);
    assert!(
        seen[0].starts_with("POST /api/rooms/r1/reactions "),
        "react: {}",
        seen[0]
    );
    assert!(
        seen[0].contains("\"target_id\":\"m1\""),
        "react body: {}",
        seen[0]
    );
    assert!(
        seen[0].contains("\"target_seq\":4"),
        "react body: {}",
        seen[0]
    );
    assert!(
        seen[0].contains("\"part_index\":0"),
        "react body: {}",
        seen[0]
    );
    assert!(
        seen[1].starts_with("PUT /api/rooms/r1/read "),
        "read: {}",
        seen[1]
    );
    assert!(seen[1].contains("\"seq\":12"), "read body: {}", seen[1]);
    // The app always sends an empty device_id here.
    assert!(
        seen[1].contains("\"device_id\":\"\""),
        "read body: {}",
        seen[1]
    );
    assert!(
        seen[2].starts_with("PUT /api/rooms/r1/notifications "),
        "mute: {}",
        seen[2]
    );
    assert!(seen[2].contains("\"muted\":true"), "mute body: {}", seen[2]);
    assert!(
        seen[3].starts_with("PUT /api/rooms/r1/pin "),
        "pin: {}",
        seen[3]
    );
    assert!(seen[3].contains("\"pinned\":true"), "pin body: {}", seen[3]);
    let _ = std::fs::remove_file(&state);
}

#[test]
fn screen_returns_the_payload_and_the_cache_header() {
    let (base, handle) = fixture(vec![Reply::ok(r#"{"tree":{"atom":"view"}}"#)]);
    let state = state_at(&base, true);
    let (code, out, err) = run_with(&state, &["screen", "profile", "--json"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("\"screen\""), "stdout: {out}");
    assert!(out.contains("view"), "stdout: {out}");
    let seen = handle.join().expect("fixture join");
    // `profile` is a dedicated route in the app, not a generic screen, and it
    // carries a `region` query (app-side implementation).
    assert!(
        seen[0].starts_with("GET /api/profile?region="),
        "request: {}",
        seen[0]
    );
    let _ = std::fs::remove_file(&state);
}

#[test]
fn one_login_command_does_both_steps_and_retries_a_wrong_code() {
    // A single `pickle login` must send the code, ask for the SMS code, and
    // retry when the server answers invalid_code. Two separate runs are what
    // this replaces.
    let (base, handle) = fixture(vec![
        Reply::ok(r#"{"pending_id":"fixture-pending","expires_in":600}"#),
        Reply::status(401, r#"{"detail":"invalid_code"}"#),
        Reply::ok(
            r#"{"status":"issued","user_id":"u1","login_session_token":"t","login_session_jwt":"eyJhbGciOiJub25lIn0.eyJzdWIiOiJ1MSIsImV4cCI6MjAwMDAwMDAwMH0.sig","expires_at":"2033-05-18T00:00:00Z"}"#,
        ),
    ]);
    let state = state_at(&base, false);
    let (code, out, err) = run_with_stdin(
        &state,
        &["login", "--region", "jp", "--json"],
        "+819012345678\nwrong\n123456\n",
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("\"sent\":true"), "stdout: {out}");
    assert!(out.contains("u1"), "stdout: {out}");
    // The retry was announced, not silently swallowed.
    assert!(err.contains("invalid_code"), "stderr: {err}");

    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state).expect("state")).expect("json");
    assert!(after["session"].is_object(), "not signed in: {after}");
    assert!(
        after["pending_phone_code"].is_null(),
        "the pending id should be consumed by a successful login: {after}"
    );
    // The number must not appear anywhere: not in output, not on disk.
    assert!(!out.contains("+819012345678"), "number echoed: {out}");
    let persisted = std::fs::read_to_string(&state).expect("state");
    assert!(
        !persisted.contains("+819012345678"),
        "number persisted: {persisted}"
    );

    let seen = handle.join().expect("fixture join");
    assert_eq!(
        seen.len(),
        3,
        "expected code request + two exchanges: {seen:?}"
    );
    assert!(
        seen[0].starts_with("POST /api/login-sessions/phone-code "),
        "{}",
        seen[0]
    );
    assert!(
        seen[1].starts_with("POST /api/login-sessions "),
        "{}",
        seen[1]
    );
    assert!(
        seen[2].starts_with("POST /api/login-sessions "),
        "{}",
        seen[2]
    );
    let _ = std::fs::remove_file(&state);
}

#[test]
fn a_terminal_code_error_clears_the_pending_id() {
    // Measured on the live service: an unknown or aged-out pending id answers
    // 401 `code_expired`, which cannot be retried. The stored id must go, or
    // the next --finish replays it and fails identically.
    let (base, handle) = fixture(vec![Reply::status(401, r#"{"detail":"code_expired"}"#)]);
    let state = state_at(&base, false);
    // Seed a pending id the way `login --start` would.
    let mut value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state).expect("state")).expect("json");
    value["pending_phone_code"] = serde_json::json!({
        "pending_id": "dead-id",
        "region": "jp",
        "expires_in": 600
    });
    std::fs::write(&state, value.to_string()).expect("rewrite");

    let (code, _, err) = run_with(&state, &["login", "--finish", "123456", "--json"]);
    assert_eq!(code, 1, "expected a refusal");
    assert!(err.contains("code_expired"), "stderr: {err}");
    assert!(err.contains("login --start"), "stderr: {err}");

    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state).expect("state")).expect("json");
    assert!(
        after["pending_phone_code"].is_null(),
        "the dead pending id survived: {after}"
    );
    let _ = handle.join().expect("fixture join");
    let _ = std::fs::remove_file(&state);
}

#[test]
fn a_retryable_code_error_keeps_the_pending_id() {
    // `invalid_code` means the code was wrong, not that the id is dead.
    let (base, handle) = fixture(vec![Reply::status(401, r#"{"detail":"invalid_code"}"#)]);
    let state = state_at(&base, false);
    let mut value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state).expect("state")).expect("json");
    value["pending_phone_code"] = serde_json::json!({
        "pending_id": "good-id",
        "region": "jp",
        "expires_in": 600
    });
    std::fs::write(&state, value.to_string()).expect("rewrite");

    let (code, _, err) = run_with(&state, &["login", "--finish", "999999", "--json"]);
    assert_eq!(code, 1);
    assert!(err.contains("invalid_code"), "stderr: {err}");
    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state).expect("state")).expect("json");
    assert_eq!(
        after["pending_phone_code"]["pending_id"],
        serde_json::json!("good-id"),
        "a retryable error must not discard the id: {after}"
    );
    let _ = handle.join().expect("fixture join");
    let _ = std::fs::remove_file(&state);
}

#[test]
fn the_session_body_measures_device_memory() {
    // A hard-coded 0 would claim a 0-byte device on every host. The app reports
    // ActivityManager.MemoryInfo.totalMem (app-side implementation).
    let (base, handle) = fixture(vec![Reply::ok(
        r#"{"session_id":"s","room":"r","livekit_url":"ws://x","participant_identity":"p","account_rev":1,"user_id":"u","plan":"free","plan_rank":0,"character_name":"c"}"#,
    )]);
    let state = state_at(&base, true);
    let (code, _, err) = run_with(&state, &["session", "--json"]);
    assert_eq!(code, 0, "stderr: {err}");
    let seen = handle.join().expect("fixture join");
    let body = &seen[0];
    let memory = body
        .split("\"device_memory_bytes\":")
        .nth(1)
        .and_then(|rest| rest.split([',', '}']).next())
        .unwrap_or_default()
        .trim()
        .to_string();
    assert!(!memory.is_empty(), "no device_memory_bytes in: {memory}");
    if cfg!(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "windows"
    )) {
        assert_ne!(memory, "0", "memory was not measured: {body}");
    }
    let _ = std::fs::remove_file(&state);
}

#[test]
fn body_json_can_come_from_a_file_from_an_argument_or_from_stdin() {
    // `--set` cannot express an array of objects, so these three routes exist.
    // Precedence: file, then inline, then stdin; `--set` wins over all of them.
    // Two replies: the third case fails before any request is made, so it must
    // not consume one (a missing reply hangs the fixture thread in accept).
    let (base, handle) = fixture(vec![
        Reply::ok(r#"{"status":"ok","seq":6}"#),
        Reply::ok(r#"{"status":"ok","seq":6}"#),
    ]);
    let state = state_at(&base, true);
    let mut body_file = std::env::temp_dir();
    body_file.push(format!("pickle-body-{}.json", std::process::id()));
    std::fs::write(
        &body_file,
        r#"{"text":"from-file","reply_to":{"target_id":"m1","target_seq":1,"part_index":0}}"#,
    )
    .expect("write body");

    let (code, _, err) = run_with(
        &state,
        &[
            "send",
            "r1",
            "--json-file",
            body_file.to_str().expect("path"),
            "--json",
        ],
    );
    assert_eq!(code, 0, "file route: {err}");
    let (code, _, err) = run_with(
        &state,
        &[
            "send",
            "r1",
            "--json-body",
            r#"{"text":"inline"}"#,
            "--json",
        ],
    );
    assert_eq!(code, 0, "inline route: {err}");
    let (code, _, err) = run_with(&state, &["send", "r1", "--json-body", "-", "--json"]);
    assert_eq!(code, 1, "stdin body needs input on the pipe: {err}");

    let seen = handle.join().expect("fixture join");
    assert!(
        seen[0].contains("\"text\":\"from-file\""),
        "file body: {}",
        seen[0]
    );
    assert!(
        seen[0].contains("\"reply_to\""),
        "nested JSON survived: {}",
        seen[0]
    );
    assert!(
        seen[1].contains("\"text\":\"inline\""),
        "inline body: {}",
        seen[1]
    );
    let _ = std::fs::remove_file(&body_file);
    let _ = std::fs::remove_file(&state);
}

#[test]
fn a_missing_positional_names_what_to_pass() {
    // "missing argument #1" told the caller an internal index, not a value.
    for (command, expected) in [
        ("send", "missing room"),
        ("screen", "missing name"),
        ("upload", "missing path"),
        ("raw", "missing path"),
        ("live-view", "missing request_id"),
    ] {
        let (base, handle) = fixture(vec![]);
        let state = state_at(&base, true);
        let (code, _, err) = run_with(&state, &[command, "--json"]);
        assert_eq!(code, 1, "{command}: expected a usage error");
        assert!(
            err.contains(expected),
            "{command}: expected {expected:?}, got {err}"
        );
        let seen = handle.join().expect("fixture join");
        assert!(seen.is_empty(), "{command}: no request expected");
        let _ = std::fs::remove_file(&state);
    }
}

#[test]
fn raw_accepts_a_leading_method_word() {
    // `raw GET api/sync` used to treat "GET" as the path and blame the path.
    let (base, handle) = fixture(vec![Reply::ok(r#"{"v":1,"rooms":[]}"#)]);
    let state = state_at(&base, true);
    let (code, out, err) = run_with(&state, &["raw", "GET", "api/sync", "--json"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("rooms"), "stdout: {out}");
    let seen = handle.join().expect("fixture join");
    assert!(
        seen[0].starts_with("GET /api/sync "),
        "request: {}",
        seen[0]
    );
    let _ = std::fs::remove_file(&state);
}

#[test]
fn raw_without_a_path_names_the_missing_argument() {
    // A method with no path must say so, not report an unknown endpoint.
    let (base, handle) = fixture(vec![]);
    let state = state_at(&base, true);
    let (code, _, err) = run_with(&state, &["raw", "POST", "--confirm", "--json"]);
    assert_eq!(code, 1, "expected a usage error");
    assert!(err.contains("needs a path"), "stderr: {err}");
    let seen = handle.join().expect("fixture join");
    assert!(seen.is_empty(), "no request expected: {seen:?}");
    let _ = std::fs::remove_file(&state);
}

#[test]
fn a_booting_session_names_the_state_instead_of_a_missing_key() {
    // The server can answer `{state, poll_after_s}` while the room is being
    // prepared. Reporting "missing session_id" for that reads like a bug.
    let (base, handle) = fixture(vec![Reply::ok(
        r#"{"state":"core_spawning","poll_after_s":2.5}"#,
    )]);
    let state = state_at(&base, true);
    let (code, _, err) = run_with(&state, &["session", "--json"]);
    assert_eq!(code, 1, "expected a refusal");
    assert!(err.contains("core_spawning"), "stderr: {err}");
    assert!(err.contains("poll_after_s"), "stderr: {err}");
    assert!(!err.contains("missing session_id"), "stderr: {err}");
    let _ = handle.join().expect("fixture join");
    let _ = std::fs::remove_file(&state);
}

#[test]
fn a_session_without_a_state_key_still_reports_the_missing_field() {
    // No `state` either: the original message must survive, naming the field.
    let (base, handle) = fixture(vec![Reply::ok(r#"{"room":"r","account_rev":1}"#)]);
    let state = state_at(&base, true);
    let (code, _, err) = run_with(&state, &["session", "--json"]);
    assert_eq!(code, 1);
    assert!(err.contains("session_id"), "stderr: {err}");
    let _ = handle.join().expect("fixture join");
    let _ = std::fs::remove_file(&state);
}

#[test]
fn leave_posts_without_a_body() {
    // The app posts api/rooms/{id}/leave with a null body
    // so nothing but the headers may be sent.
    let (base, handle) = fixture(vec![Reply::ok(r#"{"status":"ok"}"#)]);
    let state = state_at(&base, true);
    let (code, out, err) = run_with(&state, &["leave", "r1", "--json"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("\"left\":true"), "stdout: {out}");
    let seen = handle.join().expect("fixture join");
    assert!(
        seen[0].starts_with("POST /api/rooms/r1/leave "),
        "request: {}",
        seen[0]
    );
    // No body: a POST with a body would carry Content-Length or a JSON payload.
    let head = seen[0].to_ascii_lowercase();
    assert!(
        !head.contains("content-length: 0"),
        "unexpected body: {head}"
    );
    assert!(
        !head.contains("\"status\""),
        "body leaked into the request: {head}"
    );
    let _ = std::fs::remove_file(&state);
}

#[test]
fn screen_names_map_to_the_routes_the_app_uses() {
    // Seven names have dedicated routes; everything else falls through to
    // api/screens/<name> (app-side implementation).
    type Case = (
        &'static str,
        &'static [(&'static str, &'static str)],
        &'static str,
    );
    let cases: [Case; 10] = [
        ("profile", &[], "GET /api/profile?region="),
        ("vault", &[], "GET /api/vault "),
        ("character", &[], "GET /api/character "),
        ("character_settings", &[], "GET /api/character/settings "),
        ("character_colors", &[], "GET /api/character/colors "),
        (
            "notification_settings",
            &[],
            "GET /api/notification-settings ",
        ),
        (
            "handle_check",
            &[("handle", "someone")],
            "GET /api/profile/handle/check?handle=someone ",
        ),
        (
            "custom_screen",
            &[("k", "v")],
            "GET /api/screens/custom_screen?k=v ",
        ),
        // `bubble` puts `path` into the URL and drops the query.
        ("bubble", &[("path", "b1")], "GET /api/bubbles/b1 "),
        // `bubbles` keeps only cursor/date.
        (
            "bubbles",
            &[("cursor", "c1"), ("other", "x")],
            "GET /api/bubbles?cursor=c1 ",
        ),
    ];
    for (name, query, expected) in cases {
        let (base, handle) = fixture(vec![Reply::ok(r#"{"tree":{}}"#)]);
        let state = state_at(&base, true);
        let mut args = vec!["screen", name];
        // `--query` takes one `key=value` argument per pair.
        let owned: Vec<String> = query.iter().map(|(k, v)| format!("{k}={v}")).collect();
        for pair in &owned {
            args.push("--query");
            args.push(pair);
        }
        args.push("--json");
        let (code, _, err) = run_with(&state, &args);
        assert_eq!(code, 0, "{name}: {err}");
        let seen = handle.join().expect("fixture join");
        assert!(
            seen[0].starts_with(expected),
            "{name}: expected {expected:?}, got {}",
            seen[0]
        );
        let _ = std::fs::remove_file(&state);
    }
}

#[test]
fn answer_builds_the_nested_item_only_when_both_halves_are_given() {
    // The app sends `item` as {dataset, name} only when both are non-null
    // A half-filled item is not what the app sends.
    let (base, handle) = fixture(vec![
        Reply::ok(r#"{"status":"ok"}"#),
        Reply::ok(r#"{"status":"ok"}"#),
    ]);
    let state = state_at(&base, true);

    let (code, _, err) = run_with(
        &state,
        &[
            "answer",
            "req-1",
            "--set",
            "action=accept",
            "--set",
            "item.dataset=ds",
            "--set",
            "item.name=nm",
            "--json",
        ],
    );
    assert_eq!(code, 0, "answer with a full item: {err}");

    // Only one half: the app would omit the object entirely.
    let (code, _, err) = run_with(
        &state,
        &[
            "answer",
            "req-2",
            "--set",
            "action=accept",
            "--set",
            "item.dataset=ds",
            "--json",
        ],
    );
    assert_eq!(code, 0, "answer with a half item: {err}");

    let seen = handle.join().expect("fixture join");
    assert_eq!(seen.len(), 2);
    assert!(
        seen[0].contains("\"item\":{\"dataset\":\"ds\",\"name\":\"nm\"}"),
        "full item body: {}",
        seen[0]
    );
    assert!(
        !seen[1].contains("\"item\""),
        "a half-filled item must be dropped: {}",
        seen[1]
    );
    let _ = std::fs::remove_file(&state);
}

#[test]
fn agent_actions_and_answers_use_the_documented_paths() {
    let (base, handle) = fixture(vec![
        Reply::ok(r#"{"status":"accepted"}"#),
        Reply::ok(r#"{"status":"done","result":{}}"#),
        Reply::ok(r#"{"status":"ok"}"#),
        Reply::ok(r#"{"status":"ready","url":"https://example/x"}"#),
    ]);
    let state = state_at(&base, true);

    let (code, _, err) = run_with(
        &state,
        &[
            "action",
            "--set",
            "action_id=aaaa-bbbb",
            "--set",
            "kind=display_name",
            "--set",
            "payload.name=New",
            "--json",
        ],
    );
    assert_eq!(code, 0, "run stderr: {err}");
    let (code, _, err) = run_with(
        &state,
        &["action", "--mode", "result", "aaaa-bbbb", "--json"],
    );
    assert_eq!(code, 0, "result stderr: {err}");
    let (code, _, err) = run_with(
        &state,
        &[
            "answer",
            "req-1",
            "--set",
            "action=accept",
            "--set",
            "option_id=opt-1",
            "--json",
        ],
    );
    assert_eq!(code, 0, "answer stderr: {err}");
    let (code, _, err) = run_with(&state, &["live-view", "req-1", "--json"]);
    assert_eq!(code, 0, "live-view stderr: {err}");

    let seen = handle.join().expect("fixture join");
    assert_eq!(seen.len(), 4);
    assert!(
        seen[0].starts_with("POST /api/actions "),
        "run: {}",
        seen[0]
    );
    // Action ids are lower-cased before they are sent.
    assert!(
        seen[0].contains("\"action_id\":\"aaaa-bbbb\""),
        "run body: {}",
        seen[0]
    );
    assert!(
        seen[1].starts_with("GET /api/actions/aaaa-bbbb "),
        "result: {}",
        seen[1]
    );
    assert!(
        seen[2].starts_with("POST /api/chat/requests/req-1/answer "),
        "answer: {}",
        seen[2]
    );
    assert!(
        seen[2].contains("\"in_call\":false"),
        "answer body: {}",
        seen[2]
    );
    assert!(
        seen[3].starts_with("GET /api/chat/requests/req-1/live-view "),
        "live-view: {}",
        seen[3]
    );
    let _ = std::fs::remove_file(&state);
}
