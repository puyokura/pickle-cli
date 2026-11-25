//! `pickle` — terminal client and MCP server for Pickle.
//!
//! Same shape as the Muse CLI: one binary, `pickle <command>`, and
//! `pickle mcp` turns it into an MCP server over stdio.

mod cli;
mod client;
mod commands;
mod contract;
mod error;
mod mcp;
mod state;
mod tools;
mod upload;

use crate::cli::Args;
use crate::client::PickleClient;
use crate::error::{PickleError, Result};
use crate::state::{Env, State};
use std::path::PathBuf;
use std::sync::Arc;

fn main() {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("pickle: failed to start the async runtime: {err}");
            std::process::exit(1);
        }
    };
    if let Err(err) = runtime.block_on(run()) {
        // Display, not Debug: the message is the whole diagnosis, and a caller
        // scripting this CLI should not have to read the enum variant name.
        eprintln!("pickle: {err}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let raw: Vec<String> = std::env::args().skip(1).collect();

    // `--state` is global: pull it out before command parsing.
    let mut state_path = State::default_path()?;
    let mut filtered: Vec<String> = Vec::with_capacity(raw.len());
    let mut index = 0usize;
    while index < raw.len() {
        if raw[index] == "--state" {
            let value = raw
                .get(index + 1)
                .ok_or_else(|| PickleError::Unsupported("--state requires a path".into()))?;
            state_path = PathBuf::from(value);
            index += 2;
            continue;
        }
        if let Some(value) = raw[index].strip_prefix("--state=") {
            state_path = PathBuf::from(value);
            index += 1;
            continue;
        }
        filtered.push(raw[index].clone());
        index += 1;
    }

    if filtered.is_empty()
        || filtered[0] == "help"
        || filtered[0] == "--help"
        || filtered[0] == "-h"
    {
        print_help();
        return Ok(());
    }
    if filtered[0] == "version" || filtered[0] == "--version" || filtered[0] == "-V" {
        println!("pickle {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    let args = Args::parse(&filtered)?;
    let command = args.positionals.first().cloned().unwrap_or_default();
    reject_unknown_flags(&command, &args)?;

    // `--env` may change the base URL, so resolve it before building the client.
    let mut state = State::load(&state_path)?;
    // A fresh state file must be written before anything else runs: `device_id`
    // is generated on load, so without persisting it every invocation invents a
    // new device and the app's read receipts would drift.
    if !state_path.exists() {
        state.save(&state_path)?;
    }
    let env_flag = args.flag("env").map(str::to_string);
    if let Some(raw_env) = env_flag.as_deref() {
        Env::parse(raw_env).ok_or_else(|| {
            PickleError::Unsupported(format!("unknown environment `{raw_env}` (prod/dev/alpha)"))
        })?;
    }
    // Resolve once at startup, honouring the document's ttl_s. `--env` forces it.
    if commands::resolve(env_flag.as_deref(), &mut state, env_flag.is_some()).await? {
        state.save(&state_path)?;
    }

    let mut client = PickleClient::new(&state)?;
    if client.base() != state.api_base {
        // Defensive: `new` copies the state, so this never fires in practice.
        client = PickleClient::new(&state)?;
    }
    let client = Arc::new(client);

    let outcome = dispatch(&command, &client, &mut state, &state_path, &args).await;
    // Persist any JWT refreshed during the run so the next start does not
    // refresh again. A failure here must not mask the command's own result.
    if let Some(session) = client.live_session().await {
        if state.session.as_ref().map(|s| &s.login_session_jwt) != Some(&session.login_session_jwt)
        {
            state.session = Some(session);
            let _ = state.save(&state_path);
        }
    }
    outcome
}

async fn dispatch(
    command: &str,
    client: &Arc<PickleClient>,
    state: &mut State,
    state_path: &std::path::Path,
    args: &Args,
) -> Result<()> {
    match command {
        "mcp" | "serve" | "stdio" => mcp::Server::new(client.clone()).serve_stdio().await,
        "status" => emit(&commands::status_report(client).await?),
        "doctor" => doctor(client, state, state_path, args).await,
        "login" => cmd_login(client, state, state_path, args).await,
        "logout" => {
            let out = commands::logout(client).await?;
            client.clear_session().await;
            state.session = None;
            state.bootstrap = None;
            state.save(state_path)?;
            emit(&out)
        }
        "session" => {
            let demand = args.flag("room-demand").unwrap_or("none").to_string();
            // One POST api/session, one parse: fetching again to persist would
            // open a second connection context and the printed values would no
            // longer match the stored ones.
            let bootstrap = client.bootstrap(&demand).await?;
            commands::absorb_bootstrap(state, &bootstrap);
            state.save(state_path)?;
            let out = commands::bootstrap_json(&bootstrap).to_string();
            emit(&out)
        }
        "rooms" | "sync" => emit(&commands::rooms(client).await?),
        "history" => {
            let room = positional_named(args, 1, "room")?;
            let mode = args.flag("mode").unwrap_or("delta").to_string();
            let limit = args.flag("limit").and_then(parse_i64).unwrap_or(50);
            let after_seq = args.flag("after-seq").and_then(parse_i64).unwrap_or(0);
            let before_seq = args.flag("before-seq").and_then(parse_i64);
            let out = commands::history(client, &room, &mode, after_seq, limit, before_seq).await?;
            emit(&out)
        }
        "screen" => {
            let name = positional_named(args, 1, "name")?;
            let query = query_pairs(args)?;
            emit(&commands::screen(client, &name, &query).await?)
        }
        "events" => {
            // `--frames` is a count, not a mode switch: zero is a mistake, not
            // "unlimited". Rejecting it beats silently handing back one frame.
            let frames = match args.flag("frames").and_then(parse_i64) {
                Some(value) if value < 1 => {
                    return Err(PickleError::Unsupported(
                        "--frames must be at least 1".into(),
                    ))
                }
                Some(value) => value.min(200),
                None => 3,
            };
            let timeout = args
                .flag("timeout")
                .and_then(parse_i64)
                .unwrap_or(20)
                .clamp(1, 600);
            let out = commands::events(client, frames as usize, timeout as u64).await?;
            emit(&out)
        }
        "send" | "message" => {
            let room = positional_named(args, 1, "room")?;
            let body = args.set_object()?;
            if body.get("text").is_none() && body.get("attachments").is_none() {
                return Err(PickleError::Unsupported(
                    "send needs text or attachments in the body \
                     (--set text=... or --json-body '{\"text\":\"...\"}' or --json-file PATH)"
                        .into(),
                ));
            }
            let out = commands::send_message(client, &room, &body).await?;
            emit(&out)
        }
        "react" => {
            let room = positional_named(args, 1, "room")?;
            let body = args.set_object()?;
            for key in ["target_id", "emoji"] {
                if body.get(key).is_none() {
                    return Err(PickleError::Unsupported(format!(
                        "react needs {key} in the body (--set {key}=... or --json-body/-file)"
                    )));
                }
            }
            let out = commands::react(client, &room, &body).await?;
            emit(&out)
        }
        "read" => {
            let room = positional_named(args, 1, "room")?;
            let seq = args
                .flag("seq")
                .and_then(parse_i64)
                .ok_or_else(|| PickleError::Unsupported("read needs --seq N".into()))?;
            emit(&commands::mark_read(client, &room, seq).await?)
        }
        "mute" | "pin" => {
            let room = positional_named(args, 1, "room")?;
            let body = args.set_object()?;
            let muted = if command == "mute" {
                Some(
                    body.get("muted")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(true),
                )
            } else {
                body.get("muted").and_then(serde_json::Value::as_bool)
            };
            let pinned = if command == "pin" {
                Some(
                    body.get("pinned")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(true),
                )
            } else {
                body.get("pinned").and_then(serde_json::Value::as_bool)
            };
            emit(&commands::room_settings(client, &room, muted, pinned).await?)
        }
        "leave" => {
            let room = positional_named(args, 1, "room")?;
            emit(&commands::leave_room(client, &room).await?)
        }
        "room-create" => {
            let kind = args.flag("kind").unwrap_or("character_direct").to_string();
            let user_id = args
                .flag("user-id")
                .ok_or_else(|| PickleError::Unsupported("room-create needs --user-id".into()))?;
            emit(&commands::create_room(client, &kind, user_id).await?)
        }
        "upload" => {
            let path = positional_named(args, 1, "path")?;
            let target = args.flag("target").unwrap_or("attachments").to_string();
            let mime = args
                .flag("mime")
                .unwrap_or(upload::guess_mime(&path))
                .to_string();
            // `pending-zero` is not a caller choice: the app derives it from
            // target + kind (docs/attachments.md §1.1). The old flag is accepted
            // but ignored so existing scripts do not break.
            let out = upload::upload_file(client, &path, &target, &mime).await?;
            emit(&out)
        }
        "upload-cancel" => {
            let upload_id = positional_named(args, 1, "upload_id")?;
            client.discard_upload(&upload_id).await?;
            emit(&serde_json::json!({ "upload_id": upload_id, "cancelled": true }).to_string())
        }
        "answer" => {
            let request_id = positional_named(args, 1, "request_id")?;
            let body = args.set_object()?;
            if body.get("action").is_none() {
                return Err(PickleError::Unsupported(
                    "answer needs --set action=...".into(),
                ));
            }
            let mut payload = body.clone();
            if payload.get("in_call").is_none() {
                payload["in_call"] = serde_json::json!(false);
            }
            emit(&commands::answer_request(client, &request_id, &payload).await?)
        }
        "live-view" => {
            let request_id = positional_named(args, 1, "request_id")?;
            emit(&commands::live_view(client, &request_id).await?)
        }
        "action" => {
            let body = args.set_object()?;
            let mode = args.flag("mode").unwrap_or("run").to_string();
            if mode == "result" {
                let id = body
                    .get("action_id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
                    .or_else(|| first_positional(args, 1).ok())
                    .ok_or_else(|| {
                        PickleError::Unsupported("action --mode result needs an action_id".into())
                    })?;
                emit(&commands::action_result(client, &id).await?)
            } else {
                let id = body
                    .get("action_id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                let kind = body
                    .get("kind")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
                    .ok_or_else(|| {
                        PickleError::Unsupported("action needs --set kind=...".into())
                    })?;
                let payload = body
                    .get("payload")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({}));
                emit(&commands::run_action(client, &id, &kind, payload).await?)
            }
        }
        "raw" => {
            // `raw api/sync` uses --method; `raw POST api/x --confirm` is the
            // common shape elsewhere, so a leading method word is accepted too.
            // Without this, `raw GET api/sync` blamed the path for being
            // unknown, which sent the caller looking in the wrong place.
            let first = positional_named(args, 1, "path")?;
            let leading = first.to_uppercase();
            let is_method = matches!(
                leading.as_str(),
                "GET" | "POST" | "PUT" | "PATCH" | "DELETE"
            );
            let (method, path) = if is_method {
                (
                    leading,
                    first_positional(args, 2).map_err(|_| {
                        PickleError::Unsupported(format!(
                            "raw {first} needs a path, e.g. `raw {first} api/sync`"
                        ))
                    })?,
                )
            } else {
                (args.flag("method").unwrap_or("GET").to_uppercase(), first)
            };
            let mutating = method != "GET";
            if mutating && !args.has("confirm") {
                return Err(PickleError::Unsupported(format!(
                    "{method} {path} needs --confirm"
                )));
            }
            // Reject a path the app never calls. The escape hatch exists to
            // reach documented endpoints without a dedicated command, not to
            // probe the server for something that may not exist.
            if !contract::is_known_path(&path) {
                return Err(PickleError::Unsupported(format!(
                    "{path} is not an endpoint this app calls; see docs/api.md"
                )));
            }
            if let Some(methods) = contract::known_methods(&path) {
                if !methods.is_empty() && !methods.iter().any(|known| known == &method.as_str()) {
                    eprintln!(
                        "pickle: warning: {method} {path} is not a confirmed method ({})",
                        methods.join(", ")
                    );
                }
            }
            let query = query_pairs(args)?;
            let body = args.set_object()?;
            let out = commands::raw(client, &method, &path, &query, &body).await?;
            emit(&out)
        }
        "contract" => {
            let text = serde_json::to_string_pretty(&serde_json::json!({
                "endpoints": contract::ENDPOINTS.iter()
                    .map(|(path, methods)| serde_json::json!({"path": path, "methods": methods}))
                    .collect::<Vec<_>>(),
                "fallback_only": contract::ENDPOINTS_FALLBACK_ONLY.iter()
                    .map(|(path, methods)| serde_json::json!({"path": path, "methods": methods}))
                    .collect::<Vec<_>>(),
                "session_required": contract::SESSION_REQUIRED,
                "session_default_empty": contract::SESSION_DEFAULT_EMPTY,
                "session_default_null": contract::SESSION_DEFAULT_NULL,
                "error_types": contract::ERROR_TYPES,
            }))
            .map_err(|err| PickleError::State(format!("serialize: {err}")))?;
            println!("{text}");
            Ok(())
        }
        "tools" => {
            let tools = tools::definitions();
            let text = serde_json::to_string_pretty(&tools)
                .map_err(|err| PickleError::State(format!("serialize: {err}")))?;
            println!("{text}");
            Ok(())
        }
        other => Err(PickleError::Unsupported(format!(
            "unknown command `{other}`; run `pickle help`"
        ))),
    }
}

/// Flags every command understands.
const GLOBAL_FLAGS: [&str; 5] = ["env", "json", "json-body", "json-file", "state"];

fn reject_unknown_flags(command: &str, args: &Args) -> Result<()> {
    let known: &[&str] = match command {
        "status" | "logout" | "rooms" | "sync" | "tools" | "doctor" | "contract" => {
            &["json", "live"]
        }
        "login" => &["start", "finish", "region", "pending-id", "stdin"],
        "session" => &["room-demand", "json"],
        "history" => &["mode", "limit", "after-seq", "before-seq", "json"],
        "screen" => &["query", "json"],
        "events" => &["frames", "timeout", "json"],
        "send" | "message" => &["set", "json"],
        "react" => &["set", "json"],
        "read" => &["seq", "json"],
        "mute" | "pin" => &["set", "json"],
        "room-create" => &["kind", "user-id", "json"],
        "upload" => &["target", "mime", "pending-zero", "json"],
        "upload-cancel" => &["json"],
        "answer" => &["set", "json"],
        "live-view" => &["json"],
        "action" => &["mode", "set", "json"],
        "raw" => &["method", "query", "set", "confirm", "json"],
        "mcp" | "serve" | "stdio" => &[],
        _ => return Ok(()),
    };
    let mut all: Vec<&str> = GLOBAL_FLAGS.to_vec();
    all.extend_from_slice(known);
    all.sort();
    all.dedup();
    args.reject_unknown(command, &all).map_err(|_| {
        PickleError::Unsupported(format!(
            "unknown flag for `pickle {command}`; known flags: {}",
            all.join(", ")
        ))
    })
}

async fn cmd_login(
    client: &Arc<PickleClient>,
    state: &mut State,
    state_path: &std::path::Path,
    args: &Args,
) -> Result<()> {
    // `--start <phone>` and `--finish <code>` take their argument as the flag
    // value, matching the Muse CLI's `login --start you@example.com`. A bare
    // `pickle login` with no flags is a usage error, never an implicit start.
    // `--finish` may arrive as a flag (with the code) or as a switch followed by
    // `--stdin` (code read from the pipe). Treat the switch form as an empty code
    // so the stdin branch below fills it.
    let finish_arg = args
        .flag("finish")
        .map(str::to_string)
        .or_else(|| args.has("finish").then(String::new));
    // `--start --stdin` reads the number from the pipe, so it never becomes
    // argv and therefore never reaches the shell history file.
    let start_arg = args
        .flag("start")
        .map(str::to_string)
        .or_else(|| args.has("start").then(String::new));
    match (start_arg, finish_arg) {
        (Some(phone), None) => {
            let phone = if phone.is_empty() {
                // No value on the command line: ask for it, so the number is
                // typed at the prompt and never becomes part of the shell
                // command that history records. `--stdin` still works for pipes.
                prompt_line("Phone number (E.164, e.g. +819012345678): ")?
            } else {
                phone
            };
            let region = args.flag("region").unwrap_or("jp").to_string();
            let (pending_id, expires_in) = client.request_phone_code(&phone, &region).await?;
            // The number is used once, for this request, and then dropped.
            commands::absorb_pending(state, &region, &pending_id, expires_in);
            state.save(state_path)?;
            let out = serde_json::json!({
                "pending_id": pending_id,
                "expires_in": expires_in,
                "region": region,
                "next": "pickle login --finish <code>",
            });
            emit(&out.to_string())
        }
        (None, Some(code)) => {
            // `--stdin` keeps the code out of argv, and therefore out of the
            // shell history file, which is written before the command runs.
            let code = if code.is_empty() {
                prompt_line("SMS code: ")?
            } else {
                code.to_string()
            };
            let pending_id = args
                .flag("pending-id")
                .map(str::to_string)
                .or_else(|| {
                    state
                        .pending_phone_code
                        .as_ref()
                        .map(|p| p.pending_id.clone())
                })
                .ok_or_else(|| {
                    PickleError::Unsupported(
                        "no pending phone code; run `pickle login --start <phone>` first".into(),
                    )
                })?;
            let session = match client.login_with_phone_code(&pending_id, &code).await {
                Ok(session) => session,
                Err(err @ PickleError::PendingCodeDead(_)) => {
                    // The id cannot be retried. Drop it so the next --finish does
                    // not replay a dead id, then report what is needed instead.
                    state.pending_phone_code = None;
                    state.save(state_path)?;
                    return Err(err);
                }
                Err(err) => return Err(err),
            };
            let out = serde_json::json!({
                "user_id": session.user_id,
                "expires_at": session.expires_at,
            });
            commands::absorb_login(state, &session);
            client.set_session(session).await;
            state.save(state_path)?;
            emit(&out.to_string())
        }
        (Some(_), Some(_)) => Err(PickleError::Unsupported(
            "pass either --start <phone> or --finish <code>, not both".into(),
        )),
        // One command, both steps. The number and the code are typed at prompts
        // so neither becomes part of the shell command that history records.
        (None, None) => {
            let phone = prompt_line("Phone number (E.164, e.g. +819012345678): ")?;
            let region = args.flag("region").unwrap_or("jp").to_string();
            let (pending_id, expires_in) = client.request_phone_code(&phone, &region).await?;
            commands::absorb_pending(state, &region, &pending_id, expires_in);
            state.save(state_path)?;
            emit(
                &serde_json::json!({
                    "sent": true,
                    "pending_id": pending_id,
                    "expires_in": expires_in,
                    "region": region,
                    "next": "enter the SMS code below",
                })
                .to_string(),
            )?;

            // `invalid_code` is retryable within the same pending id; anything
            // terminal ends the attempt so the caller can start over.
            let mut attempt = 0;
            loop {
                attempt += 1;
                let code = match prompt_line("SMS code: ") {
                    Ok(code) => code,
                    Err(err) => return Err(err),
                };
                match client.login_with_phone_code(&pending_id, &code).await {
                    Ok(session) => {
                        let out = serde_json::json!({
                            "user_id": session.user_id,
                            "expires_at": session.expires_at,
                        });
                        commands::absorb_login(state, &session);
                        client.set_session(session).await;
                        state.save(state_path)?;
                        return emit(&out.to_string());
                    }
                    Err(PickleError::PendingCodeDead(reason)) => {
                        state.pending_phone_code = None;
                        state.save(state_path)?;
                        return Err(PickleError::PendingCodeDead(reason));
                    }
                    Err(PickleError::Http {
                        status: 401,
                        detail,
                        ..
                    }) if !crate::client::is_terminal_code_error(&detail) && attempt < 3 => {
                        eprintln!("pickle: {detail}; try again ({attempt}/3)");
                        continue;
                    }
                    Err(err) => return Err(err),
                }
            }
        }
    }
}

async fn doctor(
    client: &Arc<PickleClient>,
    state: &State,
    state_path: &std::path::Path,
    args: &Args,
) -> Result<()> {
    let rows = commands::doctor_layers(client, state, state_path, args.has("live")).await;
    let failures = rows.iter().filter(|(_, word, _)| *word == "fail").count();
    if args.has("json") {
        let body: Vec<serde_json::Value> = rows
            .iter()
            .map(|(layer, word, detail)| {
                serde_json::json!({ "layer": layer, "status": word, "detail": detail })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&body)
                .map_err(|err| PickleError::State(format!("serialize: {err}")))?,
        );
    } else {
        for (layer, word, detail) in &rows {
            println!("{layer:<14}{word:<6}{detail}");
        }
    }
    if failures > 0 {
        return Err(PickleError::Unsupported(format!(
            "{failures} layer(s) failed"
        )));
    }
    Ok(())
}

/// Print one JSON payload, pretty unless `--json` asked for a single line.
///
/// `--json` is a process-wide flag, so reading it here keeps every command on
/// one output rule. The flag is a switch, never a value.
fn emit(payload: &str) -> Result<()> {
    let single_line = std::env::args().any(|arg| arg == "--json");
    match serde_json::from_str::<serde_json::Value>(payload) {
        Ok(value) if !single_line => {
            let text = serde_json::to_string_pretty(&value)
                .map_err(|err| PickleError::State(format!("serialize: {err}")))?;
            println!("{text}");
        }
        _ => println!("{payload}"),
    }
    Ok(())
}

/// Read one line from stdin, trimmed. Used for values that must not reach argv.
/// Ask for a value on the terminal and read it from stdin.
///
/// The point is that the value is *typed at the prompt*, not passed on the
/// command line: bash writes its history file before the command runs, and
/// PowerShell writes PSReadLine history for the whole typed line, so any secret
/// carried as an argument is recorded either way. Text typed in response to a
/// prompt is not part of that line.
fn prompt_line(prompt: &str) -> Result<String> {
    eprint!("{prompt}");
    let _ = std::io::Write::flush(&mut std::io::stderr());
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|err| PickleError::State(format!("stdin: {err}")))?;
    let value = line.trim().to_string();
    if value.is_empty() {
        return Err(PickleError::Unsupported("nothing was entered".into()));
    }
    Ok(value)
}

fn first_positional(args: &Args, index: usize) -> Result<String> {
    args.positionals
        .get(index)
        .cloned()
        .ok_or_else(|| PickleError::Unsupported(format!("missing argument #{index}")))
}

/// The same positional, with a name the caller can act on.
fn positional_named(args: &Args, index: usize, name: &str) -> Result<String> {
    args.positionals
        .get(index)
        .cloned()
        .ok_or_else(|| PickleError::Unsupported(format!("missing {name}")))
}

fn parse_i64(raw: &str) -> Option<i64> {
    raw.parse::<i64>().ok()
}

/// Build `(key, value)` pairs from repeated `--query k=v` flags.
fn query_pairs(args: &Args) -> Result<Vec<(String, String)>> {
    let mut pairs = Vec::new();
    for raw in args.flag_all("query") {
        let Some((key, value)) = raw.split_once('=') else {
            return Err(PickleError::Unsupported(format!(
                "--query needs key=value, got {raw}"
            )));
        };
        pairs.push((key.to_string(), value.to_string()));
    }
    Ok(pairs)
}

fn print_help() {
    println!(
        r#"pickle — terminal client and MCP server for Pickle

Usage:
  pickle <command> [positional...] [--flags]

Auth
  pickle login [--region jp]                   ask for the number, send the SMS,
                                              ask for the code, sign in
  pickle login --start <phone>                 only send the SMS
  pickle login --finish <code>                 only exchange the code
  pickle logout                                revoke and forget
  pickle status                                session state as JSON
  pickle doctor [--live] [--json]               layer-by-layer health check

Read
  pickle rooms                                 GET api/sync (room list)
  pickle history <room> [--mode delta|page|full] [--limit N]
                 [--after-seq N] [--before-seq N]
  pickle screen <name> [--query k=v]           GET api/screens/<name>
  pickle events [--frames N] [--timeout SEC]   bounded SSE read of api/events
  pickle session [--room-demand none|voice|motion|world]  POST api/session

Write (hits a real account)
  pickle send <room> --set text=... [--set attachments=<ref>]
  pickle react <room> --set target_id=... --set emoji=...
  pickle read <room> --seq N
  pickle mute <room> [--set muted=false]
  pickle pin <room> [--set pinned=false]
  pickle room-create --kind character_direct --user-id <id>
  pickle leave <room>                         POST api/rooms/<room>/leave
  pickle upload <path> [--target attachments|blobs|issue_reports] [--mime TYPE]
  pickle upload-cancel <upload_id>            drop an unreferenced pending upload

Agent
  pickle answer <request_id> --set action=... [--set option_id=...]
  pickle live-view <request_id>
  pickle action --set kind=... [--set payload.key=...] | --mode result <id>

Escape hatch
  pickle raw <path> --method GET|POST|PUT|PATCH|DELETE [--query k=v] [--set k=v]
                 [--confirm]                   POST/PUT/PATCH/DELETE need --confirm

MCP
  pickle mcp                                   serve MCP over stdio

Reference
  pickle contract                              the endpoint/session/error tables
                                               this client validates against
  pickle tools                                 the MCP tool catalogue as JSON

Global
  --env prod|dev|alpha    switch the API base (default prod)
  --state PATH            state file (default: PICKLE_CLI_STATE, PICKLE_STATE
                          or PICKLE_MCP_STATE, else the config dir)
  --json                  print one line instead of pretty JSON

Body JSON has three routes, in this precedence:
  --json-file PATH        read a JSON object from a file
  --json-body JSON        pass a JSON object inline
  --json-body -           read a JSON object from stdin (UTF-8)
--set key=value is applied on top of them, so a large body can come from a file
and one field be overridden inline. Bodies built with --set use dots for
nesting (--set context.kind=card). --json only changes output formatting.

Values passed to --set type themselves: true/false, integers, floats, and
comma-separated lists. --set and --json-body contain no mandatory quoting, so
they survive PowerShell 5.1, which drops double quotes before a native process
sees them.
"#
    );
}
