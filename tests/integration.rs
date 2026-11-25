//! Integration tests: drive the built binary as a real process.
//!
//! `cargo test` builds the binary first and exposes its path through
//! `CARGO_BIN_EXE_pickle`, so these run the same artifact a user runs, not a
//! library shim. Anything that would touch a real account is asserted only for
//! its refusal, so the suite never sends a message or spends an SMS.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_pickle"))
}

/// A private state file per test so runs cannot interfere.
fn state_path(name: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!("pickle-it-{name}-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&path);
    path
}

fn run(name: &str, args: &[&str]) -> (i32, String, String) {
    let state = state_path(name);
    let output = Command::new(bin())
        .args(args)
        .env("PICKLE_CLI_STATE", &state)
        .output()
        .expect("spawn pickle");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

#[test]
fn version_and_help_exit_zero() {
    let (code, out, _) = run("version", &["version"]);
    assert_eq!(code, 0);
    assert!(out.contains("pickle "), "stdout: {out}");

    let (code, out, _) = run("help", &["help"]);
    assert_eq!(code, 0);
    assert!(out.contains("Usage:"), "stdout: {out}");
}

#[test]
fn status_reports_signed_out_and_creates_state() {
    let path = state_path("status");
    let output = Command::new(bin())
        .args(["status", "--json"])
        .env("PICKLE_CLI_STATE", &path)
        .output()
        .expect("spawn pickle");
    assert_eq!(output.status.code(), Some(0));
    let body = String::from_utf8_lossy(&output.stdout);
    assert!(body.contains("\"signed_in\":false"), "body: {body}");
    assert!(path.is_file(), "state file was not created");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn device_id_is_stable_across_runs() {
    let state = state_path("device");
    let first = Command::new(bin())
        .args(["status", "--json"])
        .env("PICKLE_CLI_STATE", &state)
        .output()
        .expect("first run");
    let second = Command::new(bin())
        .args(["status", "--json"])
        .env("PICKLE_CLI_STATE", &state)
        .output()
        .expect("second run");
    let a = String::from_utf8_lossy(&first.stdout);
    let b = String::from_utf8_lossy(&second.stdout);
    let device = |body: &str| {
        body.split("\"device_id\":\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .unwrap_or_default()
            .to_string()
    };
    assert!(!device(&a).is_empty());
    assert_eq!(device(&a), device(&b), "device_id drifted between runs");
    let _ = std::fs::remove_file(&state);
}

#[test]
fn contract_table_has_the_expected_shape() {
    let (code, out, err) = run("contract", &["contract", "--json"]);
    assert_eq!(code, 0, "stderr: {err}");
    // Count path entries without a JSON dependency: one `"path"` per endpoint.
    let endpoints = out.matches("\"path\":").count();
    assert!(
        endpoints >= 128,
        "expected >=128 endpoints in contract output, got {endpoints}"
    );
    assert!(out.contains("api/sync"), "api/sync missing from contract");
    assert!(out.contains("SESSION_REQUIRED") || out.contains("session_required"));
}

#[test]
fn tools_catalogue_is_json_and_complete() {
    let (code, out, err) = run("tools", &["tools"]);
    assert_eq!(code, 0, "stderr: {err}");
    for tool in [
        "pickle_status",
        "pickle_login_start",
        "pickle_login_verify",
        "pickle_rooms",
        "pickle_history",
        "pickle_events",
        "pickle_message_send",
        "pickle_react",
        "pickle_attachment_upload",
        "pickle_request_answer",
        "pickle_raw",
    ] {
        assert!(out.contains(tool), "tool {tool} missing from catalogue");
    }
}

#[test]
fn write_paths_refuse_before_reaching_the_network() {
    let cases: &[(&str, &[&str], &str)] = &[
        ("send", &["send", "room1"], "text"),
        (
            "react",
            &["react", "room1", "--set", "emoji=x"],
            "target_id",
        ),
        ("read", &["read", "room1"], "--seq"),
        ("answer", &["answer", "req1"], "action"),
        ("action", &["action", "--set", "payload.a=1"], "kind"),
        (
            "raw",
            &["raw", "api/profile", "--method", "POST"],
            "--confirm",
        ),
        ("rawpath", &["raw", "api/not-real"], "not an endpoint"),
    ];
    for (name, args, needle) in cases {
        let (code, _, err) = run(name, args);
        assert_eq!(code, 1, "{name}: expected refusal, stderr: {err}");
        assert!(
            err.contains(needle),
            "{name}: refusal did not mention {needle:?}: {err}"
        );
        assert!(
            !err.contains("-> 4") && !err.contains("-> 5"),
            "{name}: reached the server instead of refusing: {err}"
        );
    }
}

#[test]
fn unauthenticated_read_gets_json_401_not_the_cdn_page() {
    let (code, _, err) = run("unauth", &["rooms", "--json"]);
    assert_eq!(code, 1, "expected auth failure, stderr: {err}");
    assert!(
        err.contains("no stored session"),
        "expected the local refusal, got: {err}"
    );
}

#[test]
fn mcp_stdio_answers_every_request_before_exiting() {
    let state = state_path("mcp");
    let requests = [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"pickle_status","arguments":{}}}"#,
        r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"pickle_nope","arguments":{}}}"#,
    ];

    let mut child = Command::new(bin())
        .arg("mcp")
        .env("PICKLE_CLI_STATE", &state)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn mcp");
    {
        let stdin = child.stdin.as_mut().expect("stdin");
        for request in requests {
            writeln!(stdin, "{request}").expect("write request");
        }
        // Closing stdin must not make the process exit before it answers.
    }
    let output = child.wait_with_output().expect("wait for mcp");
    assert_eq!(output.status.code(), Some(0), "mcp exited non-zero");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert_eq!(
        lines.len(),
        4,
        "expected 4 responses, got {}: {stdout}",
        lines.len()
    );
    for line in &lines {
        serde_json_check(line);
    }
    assert!(stdout.contains("pickle-mcp"), "serverInfo.name missing");
    assert!(stdout.contains("\"isError\":false"), "pickle_status failed");
    assert!(
        stdout.contains("\"isError\":true"),
        "unknown tool was not an error"
    );
    let _ = std::fs::remove_file(&state);
}

/// Minimal JSON shape check without pulling a parser into the test crate.
fn serde_json_check(line: &str) {
    assert!(
        line.starts_with('{') && line.ends_with('}'),
        "not an object: {line}"
    );
    assert!(
        line.contains("\"jsonrpc\":\"2.0\""),
        "missing jsonrpc: {line}"
    );
    assert!(line.contains("\"id\":"), "missing id: {line}");
    assert!(
        line.contains("\"result\"") || line.contains("\"error\""),
        "missing result/error: {line}"
    );
}
