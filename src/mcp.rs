//! MCP (Model Context Protocol) server over stdio.
//!
//! JSON-RPC 2.0, one message per line, implementing `initialize`, `tools/list`
//! and `tools/call`. Tool calls run on their own task: a send or an SSE follow
//! can outlive any sane client timeout, and blocking the read loop made later
//! calls look like they had timed out when they had never started.

use crate::client::PickleClient;
use crate::error::PickleError;
use serde_json::{json, Value};
use std::io::BufRead;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;

pub const PROTOCOL_VERSION: &str = "2024-11-05";

/// Handshake text. It states the operating constraints a caller needs before
/// it picks a tool: where the session comes from, what the auth step costs, and
/// which calls mutate a real account.
pub const PICKLE_INSTRUCTIONS: &str = "\
Pickle tools, reconstructed from the shipped client.

Auth: a login session is reused from a state file (--state, then PICKLE_CLI_STATE, \
PICKLE_STATE or PICKLE_MCP_STATE, else $XDG_CONFIG_HOME/pickle-cli/session.json or \
%APPDATA%/pickle-cli/session.json). \
Call pickle_status first. When signed_in=false, run pickle_login_start with an \
E.164 phone number, then pickle_login_verify with the code the user reads back. \
Each new login consumes one SMS and repeated calls hit a 429, so do not call \
login_start speculatively. Neither the phone number nor the OTP code is stored \
or echoed back; the code can also be fed on stdin to keep it out of argv.

Every call is JSON REST against api_base_url. Requests need \
X-Client-Build; without it the CDN answers with its own HTML 403 instead of a \
JSON error, and X-Client-Version alone does not satisfy that gate.

Writes hit a real account: pickle_message_send posts a real message to a real \
chat partner, and pickle_react adds a real reaction. Read the room id from \
pickle_rooms first. pickle_attachment_upload counts toward the account's \
attachment quota and stays pending until a message references its ref.

Realtime: pickle_events reads the server-sent event stream and returns the \
frames it saw. It is a bounded read, not a subscription; call it again to \
continue. Only the account event carries structured JSON; every other event is \
an opaque name plus bytes.

Build: this client reports its own app_version and app_build constants. The resolver \
at pickle.com currently advertises a newer Android build, and the app refuses \
to proceed when its own build is older, so a 4xx on session bootstrap that \
looks like a credential problem may be a version gate instead. pickle_doctor \
names it explicitly.";

type SharedWriter = Arc<tokio::sync::Mutex<tokio::io::Stdout>>;

pub struct Server {
    client: Arc<PickleClient>,
}

impl Server {
    pub fn new(client: Arc<PickleClient>) -> Self {
        Self { client }
    }

    pub async fn serve_stdio(&self) -> crate::error::Result<()> {
        let stdin = std::io::stdin();
        let stdout: SharedWriter = Arc::new(tokio::sync::Mutex::new(tokio::io::stdout()));
        // Tool calls run on their own task so a slow one cannot block the read
        // loop. When stdin closes, in-flight calls must still be awaited: a
        // client that pipes its requests and closes stdin would otherwise see
        // the process exit before any response was written.
        let pending = Arc::new(tokio::sync::Notify::new());
        let in_flight = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut line = String::new();
        loop {
            line.clear();
            let read = stdin.lock().read_line(&mut line)?;
            if read == 0 {
                break;
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let request: Value = match serde_json::from_str(trimmed) {
                Ok(value) => value,
                Err(err) => {
                    let response =
                        error_response(Value::Null, -32700, &format!("parse error: {err}"));
                    write_line(&stdout, &response.to_string()).await;
                    continue;
                }
            };
            let id = request.get("id").cloned();
            let method = request
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let params = request.get("params").cloned().unwrap_or(Value::Null);

            if let Some(outcome) = self
                .dispatch(&method, params, &id, &stdout, &pending, &in_flight)
                .await
            {
                let Some(id) = id else {
                    // Notification: no response is written.
                    continue;
                };
                let response = match outcome {
                    Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                    Err((code, message)) => error_response(id, code, &message),
                };
                write_line(&stdout, &response.to_string()).await;
            }
        }

        // Drain: wait until every spawned tool call has written its response.
        //
        // `Notify::notify_waiters` only wakes waiters that are already
        // registered, so checking the counter and then awaiting would miss a
        // decrement that lands in between: the loop would block on a
        // notification that already fired. Register the waiter first, then
        // re-check, which is the only ordering that cannot hang.
        loop {
            let notified = pending.notified();
            if in_flight.load(std::sync::atomic::Ordering::SeqCst) == 0 {
                break;
            }
            notified.await;
        }
        Ok(())
    }

    async fn dispatch(
        &self,
        method: &str,
        params: Value,
        id: &Option<Value>,
        stdout: &SharedWriter,
        pending: &Arc<tokio::sync::Notify>,
        in_flight: &Arc<std::sync::atomic::AtomicUsize>,
    ) -> Option<std::result::Result<Value, (i64, String)>> {
        match method {
            "initialize" => Some(Ok(json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": {
                    "name": "pickle-mcp",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "instructions": PICKLE_INSTRUCTIONS,
            }))),
            "notifications/initialized" | "initialized" => None,
            "ping" => Some(Ok(json!({}))),
            "tools/list" => Some(Ok(json!({ "tools": crate::tools::definitions() }))),
            "tools/call" => {
                let Some(id) = id.clone() else {
                    return Some(Err((-32600, "tools/call requires an id".to_string())));
                };
                let client = self.client.clone();
                let stdout = stdout.clone();
                let pending = pending.clone();
                let in_flight = in_flight.clone();
                in_flight.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                tokio::spawn(async move {
                    let outcome = call_tool(&client, params).await;
                    let response = match outcome {
                        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                        Err((code, message)) => error_response(id, code, &message),
                    };
                    write_line(&stdout, &response.to_string()).await;
                    in_flight.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                    pending.notify_waiters();
                });
                None
            }
            other => Some(Err((-32601, format!("method not found: {other}")))),
        }
    }
}

async fn call_tool(
    client: &Arc<PickleClient>,
    params: Value,
) -> std::result::Result<Value, (i64, String)> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or((-32602, "tools/call requires a tool name".to_string()))?;
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let result = crate::tools::call(client, name, arguments).await;
    // A login through MCP must survive the process, or the next run asks for a
    // new SMS. The CLI persists on exit; MCP has no exit path, so it saves
    // right after the tool that changed the session.
    if matches!(
        name,
        "pickle_login_verify" | "pickle_login" | "pickle_logout"
    ) {
        crate::state::persist_session_from_client(client).await;
    }
    match result {
        Ok(payload) => Ok(json!({
            "content": [{ "type": "text", "text": payload }],
            "isError": false,
        })),
        Err(PickleError::Unsupported(message)) => Ok(json!({
            "content": [{ "type": "text", "text": format!("unknown tool `{name}`: {message}") }],
            "isError": true,
        })),
        Err(err) => Ok(json!({
            "content": [{ "type": "text", "text": err.to_string() }],
            "isError": true,
        })),
    }
}

async fn write_line(stdout: &SharedWriter, line: &str) {
    let mut out = stdout.lock().await;
    let _ = out.write_all(line.as_bytes()).await;
    let _ = out.write_all(b"\n").await;
    let _ = out.flush().await;
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_response_shape() {
        let value = error_response(json!(1), -32601, "nope");
        assert_eq!(value["jsonrpc"], json!("2.0"));
        assert_eq!(value["error"]["code"], json!(-32601));
    }
}
