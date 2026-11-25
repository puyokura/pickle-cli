//! Command handlers shared by the CLI and the MCP tools.
//!
//! Everything returns a JSON string so the CLI can print it either raw or
//! pretty, and the MCP layer can hand the same text to a model without a
//! second rendering path.

use crate::client::PickleClient;
use crate::error::{PickleError, Result};
use crate::state::{jwt_seconds_left, Bootstrap, Env, LoginSession, PendingPhoneCode, State};
use serde_json::{json, Value};
use std::sync::Arc;

/// `pickle status` — what a caller needs before anything else.
pub async fn status_report(client: &Arc<PickleClient>) -> Result<String> {
    let session = client.session().await;
    let seconds_left = session
        .as_ref()
        .and_then(|s| jwt_seconds_left(&s.login_session_jwt));
    let body = json!({
        "signed_in": session.is_some(),
        "env": client.env().as_str(),
        "api_base": client.base(),
        "device_id": client.device_id(),
        "user_id": session.as_ref().map(|s| s.user_id.clone()),
        "expires_at": session.as_ref().map(|s| s.expires_at.clone()),
        "jwt_seconds_left": seconds_left,
        "needs_refresh": seconds_left.map(|left| left <= 30),
    });
    Ok(body.to_string())
}

/// `pickle login --start <phone>` — request the SMS code.
pub async fn login_start(client: &Arc<PickleClient>, phone: &str, region: &str) -> Result<String> {
    let (pending_id, expires_in) = client.request_phone_code(phone, region).await?;
    let body = json!({
        "pending_id": pending_id,
        "expires_in": expires_in,
        "region": region,
        "next": "pickle login --finish <code>",
    });
    Ok(body.to_string())
}

/// `pickle login --finish <code>` — exchange the code for a session.
pub async fn login_verify(
    client: &Arc<PickleClient>,
    pending_id: &str,
    code: &str,
) -> Result<String> {
    let session = client.login_with_phone_code(pending_id, code).await?;
    // Install it on the live client: otherwise the rest of an MCP session keeps
    // running unauthenticated even though the login just succeeded.
    client.set_session(session.clone()).await;
    let body = json!({
        "user_id": session.user_id,
        "expires_at": session.expires_at,
    });
    Ok(body.to_string())
}

pub async fn logout(client: &Arc<PickleClient>) -> Result<String> {
    client.revoke().await?;
    Ok(json!({ "signed_out": true }).to_string())
}

/// `pickle session` — establish the connection context.
pub async fn session_bootstrap(client: &Arc<PickleClient>, room_demand: &str) -> Result<String> {
    let bootstrap = client.bootstrap(room_demand).await?;
    Ok(bootstrap_json(&bootstrap).to_string())
}

pub fn bootstrap_json(bootstrap: &Bootstrap) -> Value {
    json!({
        "session_id": bootstrap.session_id,
        "room": bootstrap.room,
        "livekit_url": bootstrap.livekit_url,
        "participant_identity": bootstrap.participant_identity,
        "account_rev": bootstrap.account_rev,
        "user_id": bootstrap.user_id,
        "plan": bootstrap.plan,
        "plan_rank": bootstrap.plan_rank,
        "character_name": bootstrap.character_name,
    })
}

/// `pickle rooms` — the room list.
pub async fn rooms(client: &Arc<PickleClient>) -> Result<String> {
    let value = client.sync().await?;
    let rooms = value.get("rooms").cloned().unwrap_or_else(|| json!([]));
    Ok(json!({
        "rev": value.get("rev").cloned().unwrap_or(Value::Null),
        "count": rooms.as_array().map(|list| list.len()).unwrap_or(0),
        "rooms": rooms,
    })
    .to_string())
}

/// `pickle history` — delta, page or full read.
pub async fn history(
    client: &Arc<PickleClient>,
    room: &str,
    mode: &str,
    after_seq: i64,
    limit: i64,
    before_seq: Option<i64>,
) -> Result<String> {
    let value = match mode {
        "page" => client.history_page(room, limit, before_seq).await?,
        "full" => client.history(room).await?,
        "delta" => client.history_delta(room, after_seq, limit).await?,
        other => {
            return Err(PickleError::Unsupported(format!(
                "history mode `{other}` is not delta/page/full"
            )))
        }
    };
    Ok(value.to_string())
}

/// `pickle screen <name>` — server-rendered screen payload.
pub async fn screen(
    client: &Arc<PickleClient>,
    name: &str,
    query: &[(String, String)],
) -> Result<String> {
    Ok(client.screen(name, query).await?.to_string())
}

/// `pickle events` — bounded SSE read.
pub async fn events(
    client: &Arc<PickleClient>,
    max_frames: usize,
    timeout_sec: u64,
) -> Result<String> {
    // Frames are collected into a local Vec: `read_events` calls the closure
    // synchronously from a single reader, so no lock is needed. Wrapping it in
    // a mutex with `try_lock` could drop a frame under contention, and a
    // dropped frame is silently lost evidence.
    let mut collected: Vec<Value> = Vec::new();
    let sink = &mut collected;
    let read = tokio::time::timeout(
        std::time::Duration::from_secs(timeout_sec),
        client.read_events(max_frames, |event, data| {
            sink.push(json!({ "event": event, "data": data }));
        }),
    )
    .await;
    match read {
        Ok(Ok(count)) => Ok(json!({
            "frames": collected,
            "count": count,
            "timed_out": false,
        })
        .to_string()),
        Ok(Err(err)) => Err(err),
        Err(_) => Ok(json!({
            "frames": collected,
            "count": collected.len(),
            "timed_out": true,
        })
        .to_string()),
    }
}

/// `pickle send <room>` — real message send.
pub async fn send_message(client: &Arc<PickleClient>, room: &str, body: &Value) -> Result<String> {
    let response = client.send_message(room, body).await?;
    Ok(response.to_string())
}

pub async fn react(client: &Arc<PickleClient>, room: &str, body: &Value) -> Result<String> {
    Ok(client.react(room, body).await?.to_string())
}

pub async fn mark_read(client: &Arc<PickleClient>, room: &str, seq: i64) -> Result<String> {
    client.mark_read(room, seq).await?;
    Ok(json!({ "room_id": room, "seq": seq, "marked": true }).to_string())
}

pub async fn room_settings(
    client: &Arc<PickleClient>,
    room: &str,
    muted: Option<bool>,
    pinned: Option<bool>,
) -> Result<String> {
    if muted.is_none() && pinned.is_none() {
        return Err(PickleError::Unsupported(
            "supply --set muted=true or --set pinned=true".into(),
        ));
    }
    if let Some(value) = muted {
        client.set_muted(room, value).await?;
    }
    if let Some(value) = pinned {
        client.set_pinned(room, value).await?;
    }
    Ok(json!({ "room_id": room, "muted": muted, "pinned": pinned }).to_string())
}

pub async fn leave_room(client: &Arc<PickleClient>, room: &str) -> Result<String> {
    let value = client.leave_room(room).await?;
    Ok(serde_json::json!({ "room_id": room, "left": true, "response": value }).to_string())
}

pub async fn create_room(client: &Arc<PickleClient>, kind: &str, user_id: &str) -> Result<String> {
    let room_id = client.create_room(kind, user_id).await?;
    Ok(json!({ "room_id": room_id, "kind": kind, "user_id": user_id }).to_string())
}

pub async fn answer_request(
    client: &Arc<PickleClient>,
    request_id: &str,
    body: &Value,
) -> Result<String> {
    Ok(client.answer_request(request_id, body).await?.to_string())
}

pub async fn live_view(client: &Arc<PickleClient>, request_id: &str) -> Result<String> {
    Ok(client.live_view(request_id).await?.to_string())
}

pub async fn run_action(
    client: &Arc<PickleClient>,
    action_id: &str,
    kind: &str,
    payload: Value,
) -> Result<String> {
    Ok(client
        .run_action(action_id, kind, payload)
        .await?
        .to_string())
}

pub async fn action_result(client: &Arc<PickleClient>, action_id: &str) -> Result<String> {
    Ok(client.action_result(action_id).await?.to_string())
}

/// `pickle raw` — direct call for anything without a dedicated handler.
pub async fn raw(
    client: &Arc<PickleClient>,
    method: &str,
    path: &str,
    query: &[(String, String)],
    body: &Value,
) -> Result<String> {
    if !path.starts_with("api/") {
        return Err(PickleError::Unsupported("path must start with api/".into()));
    }
    Ok(client
        .raw_request(method, path, query, body)
        .await?
        .to_string())
}

/// Persist the fields a command learned, then return the state to write.
pub fn absorb_bootstrap(state: &mut State, bootstrap: &Bootstrap) {
    state.bootstrap = Some(bootstrap.clone());
}

pub fn absorb_login(state: &mut State, session: &LoginSession) {
    state.session = Some(session.clone());
    state.pending_phone_code = None;
}

/// Record the pending phone-code so `--finish` can run in another process.
/// Record the pending phone-code so `--finish` can run in another process.
///
/// The phone number is intentionally not stored: it is personal data, and the
/// exchange needs only the `pending_id`.
pub fn absorb_pending(state: &mut State, region: &str, pending_id: &str, expires_in: i64) {
    state.pending_phone_code = Some(PendingPhoneCode {
        pending_id: pending_id.to_string(),
        region: region.to_string(),
        expires_in,
    });
}

/// Resolve the environment and base URL in one step, so both the CLI and MCP
/// paths behave identically.
/// Resolve the API base and record when it happened.
///
/// `force` is for `--env` (the environment changed, so a cached answer for the
/// old one is meaningless). Otherwise the stored answer is reused until the
/// document's `ttl_s` expires, which is what the app does.
pub async fn resolve(env_override: Option<&str>, state: &mut State, force: bool) -> Result<bool> {
    let env = match env_override {
        Some(raw) => Env::parse(raw).ok_or_else(|| {
            PickleError::Unsupported(format!("unknown environment `{raw}` (prod/dev/alpha)"))
        })?,
        None => state.env,
    };
    if !should_resolve(env, state, force, now_seconds()) {
        return Ok(false);
    }
    let now = now_seconds();
    let (base, document) = PickleClient::resolve_base(env).await;
    // The document's own ttl_s is reported, not enforced: a resolver that
    // answers a very small or malformed TTL would otherwise turn every run into
    // an extra round trip. The stored timestamp is what the next run compares.
    let _ttl = document
        .as_ref()
        .and_then(|doc| doc.get("ttl_s"))
        .and_then(Value::as_f64);
    state.env = env;
    state.api_base = base;
    state.base_resolved_at = Some(now);
    Ok(true)
}

/// Fallback TTL when the resolver omits `ttl_s`. The live document says 300.
pub const BASE_TTL_SECS: i64 = 300;

/// Whether the stored base should be replaced by a fresh resolution.
///
/// Three reasons to resolve: the caller forced it (`--env`), the environment
/// changed, or the stored answer is older than the TTL. One reason never to:
/// the stored base is not this environment's build-time fallback, which means
/// it was seeded from outside (a fixture, a test, or an operator pointing the
/// CLI at a mirror). Overwriting that would silently redirect every call.
fn should_resolve(env: Env, state: &State, force: bool, now: i64) -> bool {
    if force {
        return true;
    }
    if env != state.env {
        return true;
    }
    if let Some(at) = state.base_resolved_at {
        return now - at >= BASE_TTL_SECS;
    }
    // Never resolved yet: only take it over when it still holds the build-time
    // default. A seeded base stays put.
    state.api_base == env.fallback_base()
}

fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Shared layer check used by `pickle doctor` and the `pickle_doctor` tool.
///
/// Returns one row per layer so the CLI can print a table and MCP can hand the
/// same facts to a model without a second code path.
pub async fn doctor_layers(
    client: &Arc<PickleClient>,
    state: &crate::state::State,
    state_path: &std::path::Path,
    live: bool,
) -> Vec<(String, &'static str, String)> {
    let mut rows: Vec<(String, &'static str, String)> = Vec::new();

    match std::fs::metadata(state_path) {
        Ok(meta) => {
            let mode = state_mode(state_path);
            let ok = match mode.as_deref() {
                None => true,
                Some("600") => true,
                Some(_) => false,
            };
            rows.push((
                "state".into(),
                if ok { "ok" } else { "warn" },
                match mode {
                    Some(mode) => {
                        format!(
                            "{} bytes at {}, mode {mode}",
                            meta.len(),
                            state_path.display()
                        )
                    }
                    None => format!("{} bytes at {}", meta.len(), state_path.display()),
                },
            ));
        }
        Err(_) => rows.push((
            "state".into(),
            "warn",
            format!(
                "absent at {} (defaults will be created)",
                state_path.display()
            ),
        )),
    }

    rows.push((
        "device".into(),
        if state.device_id.is_empty() {
            "fail"
        } else {
            "ok"
        },
        if state.device_id.is_empty() {
            "no device_id".to_string()
        } else {
            let prefix: String = state.device_id.chars().take(8).collect();
            format!("present ({prefix}…)")
        },
    ));

    match client.session().await {
        None => rows.push(("auth".into(), "warn", "not signed in".into())),
        Some(session) => {
            let left = jwt_seconds_left(&session.login_session_jwt);
            rows.push((
                "auth".into(),
                match left {
                    Some(seconds) if seconds > 30 => "ok",
                    _ => "warn",
                },
                match left {
                    Some(seconds) => format!("validated, jwt ends in {seconds}s"),
                    None => "stored (jwt has no exp claim)".to_string(),
                },
            ));
        }
    }

    let (_base, document) = PickleClient::resolve_base(state.env).await;
    match &document {
        Some(document) => {
            let ttl = document.get("ttl_s").and_then(Value::as_f64);
            rows.push((
                "resolver".into(),
                "ok",
                match ttl {
                    Some(ttl) => format!("{} via pickle.com (ttl_s={ttl})", state.api_base),
                    None => state.api_base.clone(),
                },
            ));
            if PickleClient::build_is_stale(document, state.env) {
                let released = PickleClient::released_build(document, state.env)
                    .unwrap_or_else(|| "?".to_string());
                rows.push((
                    "build".into(),
                    "warn",
                    format!(
                        "client build {} is older than released {released}",
                        crate::client::APP_BUILD
                    ),
                ));
            } else {
                rows.push((
                    "build".into(),
                    "ok",
                    format!("client build {} is current", crate::client::APP_BUILD),
                ));
            }
        }
        None => rows.push((
            "resolver".into(),
            "warn",
            format!("unreachable; using fallback {}", state.api_base),
        )),
    }

    if live {
        if client.session().await.is_some() {
            match client.sync().await {
                Ok(value) => {
                    let rooms = value
                        .get("rooms")
                        .and_then(Value::as_array)
                        .map(Vec::len)
                        .unwrap_or(0);
                    rows.push(("rooms".into(), "ok", format!("{rooms} listed")));
                }
                Err(err) => rows.push(("rooms".into(), "fail", err.to_string())),
            }
            match client.establishment().await {
                Ok(value) => {
                    let phase = value
                        .get("phase")
                        .or_else(|| value.get("state"))
                        .and_then(Value::as_str)
                        .unwrap_or("reached");
                    rows.push(("establishment".into(), "ok", phase.to_string()));
                }
                Err(err) => rows.push(("establishment".into(), "fail", err.to_string())),
            }
        } else {
            rows.push((
                "live".into(),
                "skip",
                "needs a login session (pickle login --start)".into(),
            ));
        }
    }
    rows
}

/// `pickle_doctor` — the same layers as JSON, for MCP callers.
fn state_mode(path: &std::path::Path) -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::metadata(path).ok()?;
        let bits = meta.permissions().mode() & 0o777;
        Some(format!("{bits:03o}"))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

pub async fn doctor_report(client: &Arc<PickleClient>, live: bool) -> Result<String> {
    let path = crate::state::State::default_path()?;
    let state = crate::state::State::load(&path)?;
    let rows = doctor_layers(client, &state, &path, live).await;
    let payload: Vec<Value> = rows
        .into_iter()
        .map(
            |(layer, status, detail)| json!({ "layer": layer, "status": status, "detail": detail }),
        )
        .collect();
    Ok(json!({ "layers": payload }).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeded_bases_are_never_overwritten() {
        // A state file pointing at a fixture or mirror must keep that base.
        let seeded = State {
            api_base: "http://127.0.0.1:1234".to_string(),
            ..State::default()
        };
        assert!(!should_resolve(Env::Prod, &seeded, false, 1000));

        // The untouched default is taken over on first use.
        let default = State::default();
        assert!(should_resolve(Env::Prod, &default, false, 1000));

        // Once resolved, the answer is reused until the TTL expires.
        let resolved = State {
            base_resolved_at: Some(1000),
            ..State::default()
        };
        assert!(!should_resolve(Env::Prod, &resolved, false, 1000 + 299));
        assert!(should_resolve(
            Env::Prod,
            &resolved,
            false,
            1000 + BASE_TTL_SECS
        ));

        // --env always forces, and a different environment always re-resolves.
        assert!(should_resolve(Env::Prod, &seeded, true, 1000));
        assert!(should_resolve(Env::Dev, &seeded, false, 1000));
    }

    #[test]
    fn a_stale_timestamp_forces_a_resolve() {
        // Even a seeded base is refreshed once its timestamp ages past the TTL,
        // which is how a hand-written fixture state would expire.
        let stale = State {
            api_base: "http://127.0.0.1:1234".to_string(),
            base_resolved_at: Some(1000),
            ..State::default()
        };
        assert!(should_resolve(
            Env::Prod,
            &stale,
            false,
            1000 + BASE_TTL_SECS + 1
        ));
    }

    #[test]
    fn absorb_login_clears_pending() {
        let mut state = State::default();
        absorb_pending(&mut state, "jp", "p", 600);
        assert!(state.pending_phone_code.is_some());
        absorb_login(
            &mut state,
            &LoginSession {
                user_id: "u".into(),
                login_session_token: "t".into(),
                login_session_jwt: "j".into(),
                expires_at: "x".into(),
            },
        );
        assert!(state.session.is_some());
        assert!(state.pending_phone_code.is_none());
    }

    #[test]
    fn bootstrap_json_hides_the_token() {
        let bootstrap = Bootstrap {
            session_id: "s".into(),
            room: "r".into(),
            livekit_url: "wss://x".into(),
            participant_identity: "p".into(),
            account_rev: 1,
            user_id: "u".into(),
            plan: "free".into(),
            plan_rank: 1,
            character_name: "c".into(),
            raw: json!({ "token": "secret-value" }),
        };
        let rendered = bootstrap_json(&bootstrap).to_string();
        assert!(!rendered.contains("secret-value"));
    }
}
