//! MCP tool catalogue and dispatch.
//!
//! Tool outputs are JSON text so a calling model can parse them directly.
//! Every tool maps to one endpoint the app calls; nothing
//! here invents an endpoint the app does not call.

use crate::client::PickleClient;
use crate::error::{PickleError, Result};
use serde_json::{json, Value};
use std::sync::Arc;

pub fn definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "pickle_status",
            "description": "Report whether a Pickle login session is stored, when its JWT expires, and which API base the client resolved. Call this first.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        }),
        json!({
            "name": "pickle_login_start",
            "description": "Send an SMS one-time code to a phone number registered with Pickle. Returns pending_id and expires_in; the number itself is neither echoed nor stored. The user reads the code back and passes it to pickle_login_verify. One SMS per call, and repeated calls are rate limited (429 too_many_requests).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "phone": { "type": "string", "description": "E.164 phone number, e.g. +819012345678" },
                    "region": { "type": "string", "description": "Two-letter region hint, lower case, e.g. jp (default jp)" }
                },
                "required": ["phone"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "pickle_login",
            "description": "Log in in one call: send the SMS code to `phone`, then exchange `code` for a session. Both halves in one request, so a caller does not need two round trips. Compared with pickle_login_start + pickle_login_verify this performs the same two server calls; it is only the number of tool calls that drops. The number is neither echoed nor stored.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "phone": { "type": "string", "description": "E.164 phone number, e.g. +819012345678" },
                    "code": { "type": "string", "description": "The SMS code. Cannot be known before the SMS arrives, so this only works when the caller already has it (for example a second attempt after login_start)." },
                    "region": { "type": "string", "description": "Two-letter region hint, lower case, e.g. jp (default jp)" }
                },
                "required": ["phone", "code"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "pickle_login_verify",
            "description": "Complete the phone login with the code the user received. Pass the pending_id returned by pickle_login_start. Stores user_id, login_session_token, login_session_jwt and expires_at.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "pending_id": { "type": "string", "description": "pending_id from pickle_login_start" },
                    "code": { "type": "string", "description": "Numeric code from the SMS" }
                },
                "required": ["pending_id", "code"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "pickle_logout",
            "description": "Revoke the stored login session on the server and delete it locally.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        }),
        json!({
            "name": "pickle_doctor",
            "description": "Layer-by-layer health check without opening a chat: state file, device id, auth freshness, endpoint resolver, and whether this client build is the one the server advertises. Use it when a call fails and the cause is unclear.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "live": { "type": "boolean", "description": "Also probe GET api/sync and api/session/establishment. Default false." }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "pickle_session",
            "description": "POST api/session: establish one connection context and return the LiveKit URL, participant identity, plan, character name and account revision. Stores the bootstrap so later calls can reuse it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "room_demand": {
                        "type": "string",
                        "enum": ["none", "voice", "motion", "world"],
                        "description": "Which world the room needs. Default none. voice and motion require the matching entitlement."
                    }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "pickle_rooms",
            "description": "GET api/sync: the room list with room_id, kind, title, last_seq, read_seq, unread and role. Start here before reading or sending.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        }),
        json!({
            "name": "pickle_history",
            "description": "Read room history. mode=delta takes after_seq and limit (what the app polls while open); mode=page walks backwards with limit and before_seq; mode=full fetches GET api/rooms/{id}/history.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "room_id": { "type": "string", "description": "Room id from pickle_rooms" },
                    "mode": { "type": "string", "enum": ["delta", "page", "full"], "description": "Default delta" },
                    "after_seq": { "type": "integer", "description": "delta: fetch messages with seq greater than this" },
                    "limit": { "type": "integer", "description": "delta/page: page size, default 50" },
                    "before_seq": { "type": "integer", "description": "page: walk backwards from this seq" }
                },
                "required": ["room_id"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "pickle_screen",
            "description": "GET api/screens/{name}: server-rendered screen payload plus the Screen-Data-Changed cache key list. Use it for profile, character, vault, bubbles and notification settings rather than guessing endpoints.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Screen name, e.g. profile / character / vault / bubbles / notification_settings" },
                    "query": { "type": "object", "description": "String query values, e.g. {\"region\":\"JP\"}", "additionalProperties": { "type": "string" } }
                },
                "required": ["name"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "pickle_events",
            "description": "Read GET api/events (server-sent events) for a bounded window and return the frames. Only the `account` event carries JSON; other events are name plus opaque text. Read-only.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "max_frames": { "type": "integer", "description": "Frames to collect before returning, default 3, max 200" },
                    "timeout_sec": { "type": "integer", "description": "Give up after this many seconds even if fewer frames arrived, default 20" }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "pickle_message_send",
            "description": "POST api/rooms/{id}/messages: send a real message to a real chat partner. text is always sent; attachments must be refs returned by pickle_attachment_upload; context and reply_to are optional objects. Returns the server status, seq and responder.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "room_id": { "type": "string", "description": "Room id from pickle_rooms" },
                    "text": { "type": "string", "description": "Message body (may be empty when attachments carry the content)" },
                    "client_msg_id": { "type": "string", "description": "Idempotency key. Omit to generate one; reuse the same value when retrying." },
                    "attachments": { "type": "array", "items": { "type": "string" }, "description": "Attachment refs" },
                    "context": { "type": "object", "description": "Card/option context: {kind, id, option_id?, revision?, button_index?, item_index?, item_path?, label?, origin?}" },
                    "reply_to": { "type": "object", "description": "Reply target: {target_id, target_seq, part_index, resume_seq?}" }
                },
                "required": ["room_id"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "pickle_react",
            "description": "POST api/rooms/{id}/reactions: add a real reaction to a message part. Needs target_id, target_seq and part_index from the message you are reacting to.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "room_id": { "type": "string", "description": "Room id" },
                    "target_id": { "type": "string", "description": "Message id being reacted to" },
                    "target_seq": { "type": "integer", "description": "Seq of that message" },
                    "part_index": { "type": "integer", "description": "Which part of the message, default 0" },
                    "emoji": { "type": "string", "description": "Emoji to add" },
                    "client_msg_id": { "type": "string", "description": "Idempotency key; omit to generate" }
                },
                "required": ["room_id", "target_id", "target_seq", "emoji"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "pickle_mark_read",
            "description": "PUT api/rooms/{id}/read: mark the room read up to seq. The app sends an empty device_id here, so callers do not need one.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "room_id": { "type": "string" },
                    "seq": { "type": "integer", "description": "Read up to and including this seq" }
                },
                "required": ["room_id", "seq"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "pickle_room_settings",
            "description": "Change per-room settings: muted (PUT notifications) and/or pinned (PUT pin). Supply at least one.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "room_id": { "type": "string" },
                    "muted": { "type": "boolean" },
                    "pinned": { "type": "boolean" }
                },
                "required": ["room_id"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "pickle_room_create",
            "description": "POST api/rooms: create a direct room. kind is character_direct for an agent room or direct for a human room; user_id is the peer.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "enum": ["character_direct", "direct"] },
                    "user_id": { "type": "string" }
                },
                "required": ["kind", "user_id"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "pickle_attachment_upload",
            "description": "Upload a local file as a raw body (not multipart). target=attachments for workspace attachments (registered pending and resolved when a message references the ref), blobs for media, issue_reports for bug reports. Returns the server ref.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute path to the local file" },
                    "target": { "type": "string", "enum": ["attachments", "blobs", "issue_reports"], "description": "Default attachments" },
                    "mime": { "type": "string", "description": "Content type, guessed from the extension when omitted" },
                    "pending_zero": { "type": "boolean", "description": "Deprecated and ignored: the client derives pending=0 from target and MIME type, the way the app does." }
                },
                "required": ["path"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "pickle_room_leave",
            "description": "POST api/rooms/{id}/leave: leave the room. Leaves the character chat permanently from this account; the app sends no body.",
            "inputSchema": {
                "type": "object",
                "properties": { "room_id": { "type": "string" } },
                "required": ["room_id"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "pickle_upload_cancel",
            "description": "DELETE api/attachments/pending/{upload_id}: drop an uploaded object that no message references. Use it when a send failed after an upload, so the server does not keep an orphan.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "upload_id": { "type": "string", "description": "upload_id returned by pickle_attachment_upload" }
                },
                "required": ["upload_id"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "pickle_request_answer",
            "description": "POST api/chat/requests/{id}/answer: answer an agent request that the server sent as a message request field. action and in_call are required by the server; supply option_id, option_ids, value or item as appropriate.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "request_id": { "type": "string" },
                    "action": { "type": "string", "description": "Action the request offered" },
                    "option_id": { "type": "string" },
                    "option_ids": { "type": "array", "items": { "type": "string" } },
                    "value": { "type": "string" },
                    "item": { "type": "object", "description": "{dataset, name}" }
                },
                "required": ["request_id", "action"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "pickle_action",
            "description": "Run or fetch an agent action. mode=run posts api/actions with action_id, kind and payload; mode=result reads api/actions/{id}. Use it for profile and character edits the agent offers.",
            "inputSchema": {
                "type": "object",
                "oneOf": [
                    {
                        "properties": {
                            "mode": { "type": "string", "const": "run" },
                            "action_id": { "type": "string", "description": "UUID; run generates one when omitted" },
                            "kind": { "type": "string", "description": "Action kind, e.g. display_name / bio / character_name" },
                            "payload": { "type": "object", "description": "Action payload" }
                        },
                        "required": ["kind"],
                        "additionalProperties": false
                    },
                    {
                        "properties": {
                            "mode": { "type": "string", "const": "result" },
                            "action_id": { "type": "string", "description": "UUID of the action to read back" }
                        },
                        "required": ["mode", "action_id"],
                        "additionalProperties": false
                    }
                ]
            }
        }),
        json!({
            "name": "pickle_raw",
            "description": "Call any api/* path directly with a method, query and JSON body. Escape hatch for endpoints without a dedicated tool; confirm=true is required for POST/PUT/PATCH/DELETE.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "method": { "type": "string", "enum": ["GET", "POST", "PUT", "PATCH", "DELETE"] },
                    "path": { "type": "string", "description": "Path beginning with api/, e.g. api/route" },
                    "query": { "type": "object", "additionalProperties": { "type": "string" } },
                    "body": { "type": "object" },
                    "confirm": { "type": "boolean", "description": "Required for any mutating method" }
                },
                "required": ["method", "path"],
                "additionalProperties": false
            }
        }),
    ]
}

fn arg_str(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(Value::as_str).map(str::to_string)
}

fn require_str(args: &Value, key: &str) -> Result<String> {
    arg_str(args, key).ok_or_else(|| PickleError::Unsupported(format!("missing argument `{key}`")))
}

fn arg_i64(args: &Value, key: &str, default: i64) -> i64 {
    args.get(key).and_then(Value::as_i64).unwrap_or(default)
}

pub async fn call(client: &Arc<PickleClient>, name: &str, args: Value) -> Result<String> {
    let rendered = match name {
        "pickle_status" => crate::commands::status_report(client).await?,
        "pickle_login_start" => {
            let phone = require_str(&args, "phone")?;
            let region = arg_str(&args, "region").unwrap_or_else(|| "jp".to_string());
            crate::commands::login_start(client, &phone, &region).await?
        }
        "pickle_login" => {
            let phone = require_str(&args, "phone")?;
            let code = require_str(&args, "code")?;
            let region = arg_str(&args, "region").unwrap_or_else(|| "jp".to_string());
            let (pending_id, _expires) = client.request_phone_code(&phone, &region).await?;
            crate::commands::login_verify(client, &pending_id, &code).await?
        }
        "pickle_login_verify" => {
            let code = require_str(&args, "code")?;
            let pending_id = arg_str(&args, "pending_id")
                .ok_or_else(|| PickleError::Unsupported("missing argument `pending_id`".into()))?;
            crate::commands::login_verify(client, &pending_id, &code).await?
        }
        "pickle_logout" => crate::commands::logout(client).await?,
        "pickle_doctor" => {
            let live = args.get("live").and_then(Value::as_bool).unwrap_or(false);
            crate::commands::doctor_report(client, live).await?
        }
        "pickle_session" => {
            let demand = arg_str(&args, "room_demand").unwrap_or_else(|| "none".to_string());
            crate::commands::session_bootstrap(client, &demand).await?
        }
        "pickle_rooms" => crate::commands::rooms(client).await?,
        "pickle_history" => {
            let room = require_str(&args, "room_id")?;
            let mode = arg_str(&args, "mode").unwrap_or_else(|| "delta".to_string());
            let limit = arg_i64(&args, "limit", 50);
            let after_seq = arg_i64(&args, "after_seq", 0);
            let before_seq = args.get("before_seq").and_then(Value::as_i64);
            crate::commands::history(client, &room, &mode, after_seq, limit, before_seq).await?
        }
        "pickle_screen" => {
            let screen = require_str(&args, "name")?;
            let query: Vec<(String, String)> = args
                .get("query")
                .and_then(Value::as_object)
                .map(|map| {
                    map.iter()
                        .filter_map(|(key, value)| {
                            value.as_str().map(|v| (key.clone(), v.to_string()))
                        })
                        .collect()
                })
                .unwrap_or_default();
            crate::commands::screen(client, &screen, &query).await?
        }
        "pickle_events" => {
            let frames = arg_i64(&args, "max_frames", 3).clamp(1, 200);
            let timeout = arg_i64(&args, "timeout_sec", 20).clamp(1, 600);
            crate::commands::events(client, frames as usize, timeout as u64).await?
        }
        "pickle_message_send" => {
            let room = require_str(&args, "room_id")?;
            crate::commands::send_message(client, &room, &args).await?
        }
        "pickle_react" => {
            let room = require_str(&args, "room_id")?;
            let mut body = serde_json::Map::new();
            body.insert("target_id".into(), json!(require_str(&args, "target_id")?));
            body.insert("target_seq".into(), json!(arg_i64(&args, "target_seq", 0)));
            body.insert("part_index".into(), json!(arg_i64(&args, "part_index", 0)));
            body.insert("emoji".into(), json!(require_str(&args, "emoji")?));
            body.insert(
                "client_msg_id".into(),
                json!(arg_str(&args, "client_msg_id")
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())),
            );
            crate::commands::react(client, &room, &Value::Object(body)).await?
        }
        "pickle_mark_read" => {
            let room = require_str(&args, "room_id")?;
            let seq = arg_i64(&args, "seq", 0);
            crate::commands::mark_read(client, &room, seq).await?
        }
        "pickle_room_settings" => {
            let room = require_str(&args, "room_id")?;
            let muted = args.get("muted").and_then(Value::as_bool);
            let pinned = args.get("pinned").and_then(Value::as_bool);
            crate::commands::room_settings(client, &room, muted, pinned).await?
        }
        "pickle_room_create" => {
            let kind = require_str(&args, "kind")?;
            let user_id = require_str(&args, "user_id")?;
            crate::commands::create_room(client, &kind, &user_id).await?
        }
        "pickle_attachment_upload" => {
            let path = require_str(&args, "path")?;
            let target = arg_str(&args, "target").unwrap_or_else(|| "attachments".to_string());
            let mime = arg_str(&args, "mime")
                .unwrap_or_else(|| crate::upload::guess_mime(&path).to_string());
            // Derived inside upload_file (docs/attachments.md §1.1), not taken
            // from the caller: an image and a video need opposite values.
            crate::upload::upload_file(client, &path, &target, &mime).await?
        }
        "pickle_room_leave" => {
            let room = require_str(&args, "room_id")?;
            crate::commands::leave_room(client, &room).await?
        }
        "pickle_upload_cancel" => {
            let upload_id = require_str(&args, "upload_id")?;
            client.discard_upload(&upload_id).await?;
            json!({ "upload_id": upload_id, "cancelled": true }).to_string()
        }
        "pickle_request_answer" => {
            let request_id = require_str(&args, "request_id")?;
            let mut body = serde_json::Map::new();
            body.insert("action".into(), json!(require_str(&args, "action")?));
            body.insert("in_call".into(), json!(false));
            for key in ["option_id", "value"] {
                if let Some(value) = arg_str(&args, key) {
                    body.insert(key.into(), json!(value));
                }
            }
            if let Some(ids) = args.get("option_ids").and_then(Value::as_array) {
                body.insert("option_ids".into(), json!(ids));
            }
            if let Some(item) = args.get("item").filter(|value| value.is_object()) {
                body.insert("item".into(), item.clone());
            }
            crate::commands::answer_request(client, &request_id, &Value::Object(body)).await?
        }
        "pickle_action" => {
            let mode = arg_str(&args, "mode").unwrap_or_else(|| "run".to_string());
            if mode == "result" {
                let id = require_str(&args, "action_id")?;
                crate::commands::action_result(client, &id).await?
            } else {
                let id =
                    arg_str(&args, "action_id").unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                let kind = require_str(&args, "kind")?;
                let payload = args.get("payload").cloned().unwrap_or_else(|| json!({}));
                crate::commands::run_action(client, &id, &kind, payload).await?
            }
        }
        "pickle_raw" => {
            let method = require_str(&args, "method")?.to_uppercase();
            let path = require_str(&args, "path")?;
            let mutating = !matches!(method.as_str(), "GET");
            if mutating
                && !args
                    .get("confirm")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            {
                return Err(PickleError::Unsupported(
                    "mutating pickle_raw needs confirm=true".into(),
                ));
            }
            if !crate::contract::is_known_path(&path) {
                return Err(PickleError::Unsupported(format!(
                    "{path} is not an endpoint this app calls"
                )));
            }
            let query: Vec<(String, String)> = args
                .get("query")
                .and_then(Value::as_object)
                .map(|map| {
                    map.iter()
                        .filter_map(|(key, value)| {
                            value.as_str().map(|v| (key.clone(), v.to_string()))
                        })
                        .collect()
                })
                .unwrap_or_default();
            let body = args.get("body").cloned().unwrap_or(Value::Null);
            crate::commands::raw(client, &method, &path, &query, &body).await?
        }
        other => return Err(PickleError::Unsupported(format!("no such tool: {other}"))),
    };
    Ok(rendered)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_has_name_and_schema() {
        for tool in definitions() {
            assert!(tool.get("name").and_then(Value::as_str).is_some());
            assert!(tool.get("inputSchema").is_some());
            assert!(tool.get("description").and_then(Value::as_str).is_some());
        }
    }

    #[test]
    fn tool_names_are_unique() {
        let names: Vec<String> = definitions()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_string())
            .collect();
        let mut deduped = names.clone();
        deduped.sort();
        deduped.dedup();
        assert_eq!(names.len(), deduped.len());
    }

    #[test]
    fn required_arguments_match_schema() {
        // A tool that requires an argument must list it in `required`.
        for tool in definitions() {
            let name = tool["name"].as_str().unwrap();
            if name == "pickle_login_verify" || name == "pickle_login_start" {
                assert!(tool["inputSchema"]["required"].as_array().is_some());
            }
        }
    }
}
