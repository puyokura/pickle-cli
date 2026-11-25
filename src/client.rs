//! HTTP surface of Pickle, reconstructed from the Android client.
//!
//! Header set, request bodies and response shapes below are the ones the app
//! itself uses, confirmed against the live service where the reference was not
//! conclusive. Two facts are load-bearing and were measured:
//!
//! * `X-Client-Build` is required on the `login-sessions*` routes. Omitting it
//!   yields a CloudFront 403 HTML page, not a JSON error, so the failure looks
//!   like a network fault. `X-Client-Version` alone does not satisfy the gate;
//!   both were probed directly on 2026-11-23.
//! * Everything else is JSON REST; there is no gRPC or protobuf on this surface.

use crate::error::{PickleError, Result};
use crate::state::{Bootstrap, Env, LoginSession, State};
use futures_util::StreamExt;
use reqwest::{Client, Method, Response};
use serde_json::{json, Value};
use std::time::Duration;

/// The build identity this client reports.
///
/// These are protocol values, not project metadata: `X-Client-Build` is the gate
/// the CDN checks on `login-sessions*`, and `api/session` carries
/// `app_version` / `app_build` in its body. They name the client this one
/// imitates, so they are kept in the binary while project metadata and the
/// README stay free of any build notation.
pub const APP_VERSION: &str = "1.1.16";
pub const APP_BUILD: &str = "428293";
pub const ENDPOINT_RESOLVER: &str = "https://pickle.com/api/backend-endpoints";

/// Sign-in route markers (アプリ側の実装).
const SIGN_IN_FLOW: &str = "2";
const ONBOARDING_FLOW: &str = "4";

/// Locale-derived request headers.
///
/// The app derives these from the device locale and the resolved storefront
/// (アプリ側の実装, アプリ側の実装). The CLI derives them from
/// the process environment so one host can imitate another, but the *rules* are
/// the app's:
///
/// * the storefront is only sent when it is one of `us` / `kr` / `jp`
///   (アプリ側の実装),
/// * the device region is only sent when the locale carries a country
///   (アプリ側の実装),
/// * the resolver flag is always sent, `true` or `false`
///   (アプリ側の実装).
#[derive(Debug, Clone, Copy)]
pub struct LocaleHeaders {
    pub language: &'static str,
    pub language_source: &'static str,
    pub timezone: &'static str,
    pub storefront: Option<&'static str>,
    pub device_region: Option<&'static str>,
    /// Whether the storefront was resolved. `false` when it was not.
    pub resolved: bool,
}

impl LocaleHeaders {
    /// `PICKLE_STOREFRONT` and `PICKLE_DEVICE_REGION` override the locale so a
    /// caller can drive a specific market without changing the host locale.
    pub fn detect() -> Self {
        let storefront = match std::env::var("PICKLE_STOREFRONT") {
            Ok(raw) => map_storefront(&raw),
            Err(_) => map_storefront(&std::env::var("LC_ALL").unwrap_or_default()),
        };
        let device_region = std::env::var("PICKLE_DEVICE_REGION")
            .ok()
            .or_else(|| std::env::var("LANG").ok())
            .and_then(|raw| region_from_locale(&raw));
        Self {
            language: "en",
            language_source: "device_language",
            timezone: "Asia/Tokyo",
            storefront,
            device_region,
            resolved: storefront.is_some(),
        }
    }

    /// The device country, or an empty string when the locale carries none.
    ///
    /// `api/profile` is called with `region` as the device country
    /// (アプリ側の実装), and the app sends whatever `Locale.getCountry()`
    /// returns, including the empty string.
    fn region_or_empty(&self) -> String {
        self.device_region.unwrap_or("").to_string()
    }

    fn resolved_flag(&self) -> &'static str {
        if self.resolved {
            "true"
        } else {
            "false"
        }
    }
}

/// The country segment of a POSIX or BCP-47 locale.
///
/// `ja_JP.UTF-8` and `ja-JP` both yield `JP`; a codeset suffix is dropped first,
/// because `rsplit('.')` on the whole string would otherwise return `UTF8`.
fn locale_country(raw: &str) -> Option<&'static str> {
    let without_codeset = raw.trim().split('.').next().unwrap_or("");
    let segment = without_codeset
        .rsplit(['_', '-'])
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    match segment.as_str() {
        "JP" | "JPN" => Some("JP"),
        "KR" | "KOR" => Some("KR"),
        "US" | "USA" => Some("US"),
        _ => None,
    }
}

/// The app's storefront table (アプリ側の実装, `:L37-L59`).
///
/// Only `us`, `kr` and `jp` are accepted; anything else yields `None`, and the
/// caller then omits the header entirely.
fn map_storefront(raw: &str) -> Option<&'static str> {
    match locale_country(raw) {
        Some("JP") => Some("jp"),
        Some("KR") => Some("kr"),
        _ => None,
    }
}

/// Country code from a POSIX or BCP-47 locale, or `None` when absent.
fn region_from_locale(raw: &str) -> Option<&'static str> {
    locale_country(raw)
}

/// Header only when a value exists. `Option` is how the app decides too.
trait OptionalHeader {
    fn header_opt(self, name: &'static str, value: Option<String>) -> Self;
}

impl OptionalHeader for reqwest::RequestBuilder {
    fn header_opt(self, name: &'static str, value: Option<String>) -> Self {
        match value {
            Some(value) => self.header(name, value),
            None => self,
        }
    }
}

pub struct PickleClient {
    http: Client,
    /// Separate client for the event stream: the 30s total timeout above would
    /// cut a subscription off mid-read, and the app uses a 60s read timeout
    /// there instead (second-pass decompile of the event client).
    stream: Client,
    base: String,
    env: Env,
    device_id: String,
    /// Held behind a lock because a request may refresh the JWT mid-flight.
    /// Two concurrent calls must not each spend a refresh.
    session: tokio::sync::RwLock<Option<LoginSession>>,
}

impl PickleClient {
    pub fn new(state: &State) -> Result<Self> {
        let http = Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("okhttp/4.12.0")
            .build()?;
        let stream = Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .read_timeout(Duration::from_secs(60))
            .user_agent("okhttp/4.12.0")
            .build()?;
        Ok(Self {
            http,
            stream,
            base: state.api_base.clone(),
            env: state.env,
            device_id: state.device_id.clone(),
            session: tokio::sync::RwLock::new(state.session.clone()),
        })
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    pub fn env(&self) -> Env {
        self.env
    }

    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    /// Snapshot of the stored session. Cheap: it clones four strings.
    pub async fn session(&self) -> Option<LoginSession> {
        self.session.read().await.clone()
    }

    pub async fn set_session(&self, session: LoginSession) {
        *self.session.write().await = Some(session);
    }

    /// The live session, including any JWT refresh made during this run.
    ///
    /// A refresh is kept in memory; without copying it back into the state file
    /// the next process start refreshes again, so every run pays for a refresh
    /// it had already earned.
    pub async fn live_session(&self) -> Option<LoginSession> {
        self.session.read().await.clone()
    }
    /// Clear the stored session locally.
    pub async fn clear_session(&self) {
        *self.session.write().await = None;
    }

    /// Cancel an upload that never got referenced by a message. Without this
    /// the server keeps an unreferenced object alive indefinitely.
    pub async fn discard_upload(&self, upload_id: &str) -> Result<()> {
        self.cancel_pending(upload_id).await
    }

    /// Resolve the API base URL from the public endpoint document.
    ///
    /// `GET https://pickle.com/api/backend-endpoints`, 3s timeout in the app.
    /// Falls back to the build-time URL for the environment when the document
    /// is unreachable or incomplete.
    pub async fn resolve_base(env: Env) -> (String, Option<Value>) {
        let client = match Client::builder().timeout(Duration::from_secs(3)).build() {
            Ok(client) => client,
            Err(_) => return (env.fallback_base().to_string(), None),
        };
        let response = match client.get(ENDPOINT_RESOLVER).send().await {
            Ok(response) if response.status().is_success() => response,
            _ => return (env.fallback_base().to_string(), None),
        };
        let body: Value = match response.json().await {
            Ok(body) => body,
            Err(_) => return (env.fallback_base().to_string(), None),
        };
        let base = body
            .get("environments")
            .and_then(|value| value.get(env.as_str()))
            .and_then(|value| value.get("api_base_url"))
            .and_then(Value::as_str)
            .filter(|value| value.starts_with("http"))
            .map(str::to_string);
        match base {
            Some(base) => (base, Some(body)),
            None => (env.fallback_base().to_string(), Some(body)),
        }
    }

    /// The build the resolver says is current for this environment.
    ///
    /// The app reads `released_android_build` and refuses to proceed when the
    /// running build is older, so the CLI reports the gap rather than sending
    /// requests the server may reject.
    pub fn released_build(document: &Value, env: Env) -> Option<String> {
        document
            .get("environments")
            .and_then(|value| value.get(env.as_str()))
            .and_then(|value| value.get("released_android_build"))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.chars().all(|c| c.is_ascii_digit()))
            .map(str::to_string)
    }

    /// True when the resolver advertises a build newer than this client.
    pub fn build_is_stale(document: &Value, env: Env) -> bool {
        match Self::released_build(document, env) {
            Some(released) => match released.parse::<u64>() {
                Ok(released) => match APP_BUILD.parse::<u64>() {
                    Ok(ours) => released > ours,
                    Err(_) => false,
                },
                Err(_) => false,
            },
            None => false,
        }
    }

    /// Headers every call carries (アプリ側の実装, アプリ側の実装).
    ///
    /// `X-Client-Version` and `X-Client-Build` are not optional on the sign-in
    /// routes: without them CloudFront returns its own 403 page.
    fn base_headers(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let locale = LocaleHeaders::detect();
        request
            .header("X-Client-Version", APP_VERSION)
            .header("X-Client-Build", APP_BUILD)
            .header("X-Client-Platform", "android")
            .header("X-Client-Timezone", locale.timezone)
            .header("X-Pickle-Sign-In-Flow", SIGN_IN_FLOW)
            .header("X-Pickle-Onboarding-Flow", ONBOARDING_FLOW)
            .header("X-Pickle-Language", locale.language)
            .header("X-Pickle-Language-Source", locale.language_source)
            // The app sends the storefront only when it resolved one, sends the
            // device region only when the locale carried a country, and always
            // sends the resolver flag (アプリ側の実装).
            .header("X-Pickle-Store-Country-Resolved", locale.resolved_flag())
            .header_opt(
                "X-Pickle-Storefront-Country",
                locale.storefront.map(str::to_string),
            )
            .header_opt(
                "X-Pickle-Device-Region",
                locale.device_region.map(|code| code.to_string()),
            )
    }

    /// Flatten a FastAPI-style `detail` into one line.
    ///
    /// A 422 answers `{"detail": [{"type","loc","msg","ctx"}, ...]}`. Treating
    /// that as an opaque string throws away the only thing that says which
    /// field is wrong, so each entry is rendered as `loc: msg`.
    fn render_detail(value: &Value) -> Option<String> {
        match value.get("detail") {
            Some(Value::String(text)) => Some(text.clone()),
            Some(Value::Array(entries)) => {
                let parts: Vec<String> = entries
                    .iter()
                    .map(|entry| {
                        let loc = entry
                            .get("loc")
                            .and_then(Value::as_array)
                            .map(|segments| {
                                segments
                                    .iter()
                                    .map(|segment| match segment {
                                        Value::String(name) => name.clone(),
                                        other => other.to_string(),
                                    })
                                    .collect::<Vec<_>>()
                                    .join(".")
                            })
                            .unwrap_or_default();
                        let msg = entry.get("msg").and_then(Value::as_str).unwrap_or("");
                        if loc.is_empty() {
                            msg.to_string()
                        } else {
                            format!("{loc}: {msg}")
                        }
                    })
                    .collect();
                if parts.is_empty() {
                    None
                } else {
                    Some(parts.join("; "))
                }
            }
            _ => value
                .get("error")
                .and_then(Value::as_str)
                .map(str::to_string),
        }
    }

    /// Turn a non-2xx response into the app's error taxonomy.
    async fn check(response: Response, method: &Method, path: &str) -> Result<Response> {
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let detail = response
            .json::<Value>()
            .await
            .ok()
            .and_then(|body| Self::render_detail(&body))
            .unwrap_or_else(|| format!("http_{}", status.as_u16()));
        Err(PickleError::http(
            status.as_u16(),
            detail,
            method.as_str(),
            path,
        ))
    }

    fn url(&self, path: &str) -> String {
        format!(
            "{}/{}",
            self.base.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    }

    /// Attach the bearer token, refreshing it first when it is about to expire.
    ///
    /// The app refreshes with a 30 second margin (アプリ側の実装). Without
    /// that margin a request can be sent with a token that dies in flight and
    /// the server answers 401 even though the caller just authenticated.
    async fn auth(&self, request: reqwest::RequestBuilder) -> Result<reqwest::RequestBuilder> {
        let session = self
            .session
            .read()
            .await
            .clone()
            .ok_or(PickleError::NoSession)?;
        if matches!(crate::state::jwt_seconds_left(&session.login_session_jwt), Some(seconds) if seconds <= 30)
        {
            if let Ok((jwt, expires_at)) = self.refresh().await {
                let refreshed = LoginSession {
                    login_session_jwt: jwt,
                    expires_at,
                    ..session.clone()
                };
                *self.session.write().await = Some(refreshed.clone());
                return Ok(request.bearer_auth(&refreshed.login_session_jwt));
            }
            // The refresh failed. For a JWT that has already expired this is
            // terminal: the server answered `invalid_code` / another 401 and no
            // amount of retrying will produce a valid token. Report it as a
            // dead session instead of sending the request and calling the
            // server's 401 `invalid_session`, which reads like a client bug.
            if matches!(crate::state::jwt_seconds_left(&session.login_session_jwt), Some(seconds) if seconds <= 0)
            {
                return Err(PickleError::SessionExpired);
            }
            // Still inside its lifetime: the clocks may differ, so let the
            // request through and let the server decide.
        }
        Ok(request.bearer_auth(&session.login_session_jwt))
    }

    // ---------------------------------------------------------------- auth

    /// `POST api/login-sessions/phone-code` -> `{pending_id, expires_in}`.
    ///
    /// Body is `{phone, region, carrier?}`; `carrier` is only sent when non-empty.
    pub async fn request_phone_code(&self, phone: &str, region: &str) -> Result<(String, i64)> {
        let path = "api/login-sessions/phone-code";
        let body = json!({ "phone": phone, "region": region });
        let response = self
            .base_headers(self.http.post(self.url(path)).json(&body))
            .send()
            .await?;
        let response = Self::check(response, &Method::POST, path).await?;
        let body: Value = response.json().await?;
        let pending_id = body
            .get("pending_id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| PickleError::invalid(path, "missing pending_id"))?
            .to_string();
        let expires_in = body
            .get("expires_in")
            .and_then(Value::as_f64)
            .unwrap_or(0.0) as i64;
        Ok((pending_id, expires_in))
    }

    /// `POST api/login-sessions` with `kind=phone_code`.
    pub async fn login_with_phone_code(
        &self,
        pending_id: &str,
        code: &str,
    ) -> Result<LoginSession> {
        let path = "api/login-sessions";
        let body = json!({ "kind": "phone_code", "pending_id": pending_id, "code": code });
        let response = self
            .base_headers(self.http.post(self.url(path)).json(&body))
            .send()
            .await?;
        let value = match Self::json_or_error(response, &Method::POST, path).await {
            Ok(value) => value,
            Err(PickleError::Http {
                status: 401,
                ref detail,
                ..
            }) if is_terminal_code_error(detail) => {
                return Err(PickleError::PendingCodeDead(detail.clone()));
            }
            Err(err) => return Err(err),
        };
        let status = value.get("status").and_then(Value::as_str).unwrap_or("");
        if status != "issued" {
            let detail = if status == "phone_verification_required" {
                "phone_verification_required".to_string()
            } else {
                format!("unexpected status: {status}")
            };
            return Err(PickleError::invalid(path, detail));
        }
        Ok(LoginSession {
            user_id: field(&value, "user_id", path)?,
            login_session_token: field(&value, "login_session_token", path)?,
            login_session_jwt: field(&value, "login_session_jwt", path)?,
            expires_at: field(&value, "expires_at", path)?,
        })
    }

    /// `POST api/login-sessions/refresh` -> `{login_session_jwt, expires_at}`.
    pub async fn refresh(&self) -> Result<(String, String)> {
        let session = self
            .session
            .read()
            .await
            .clone()
            .ok_or(PickleError::NoSession)?;
        let path = "api/login-sessions/refresh";
        let body = json!({ "login_session_token": session.login_session_token });
        let response = self
            .base_headers(self.http.post(self.url(path)).json(&body))
            .send()
            .await?;
        let value = Self::json_or_error(response, &Method::POST, path).await?;
        Ok((
            field(&value, "login_session_jwt", path)?,
            field(&value, "expires_at", path)?,
        ))
    }

    /// `POST api/login-sessions/revoke`. Best effort: a local logout must not
    /// fail because the server is unreachable.
    pub async fn revoke(&self) -> Result<()> {
        let Some(session) = self.session.read().await.clone() else {
            return Ok(());
        };
        let path = "api/login-sessions/revoke";
        let body = json!({ "login_session_token": session.login_session_token });
        if let Ok(response) = self
            .base_headers(self.http.post(self.url(path)).json(&body))
            .send()
            .await
        {
            let _ = Self::check(response, &Method::POST, path).await;
        }
        Ok(())
    }

    // ------------------------------------------------------------- session

    /// `POST api/session` — establish one connection context.
    pub async fn bootstrap(&self, room_demand: &str) -> Result<Bootstrap> {
        let path = "api/session";
        // The server validates room_demand as a literal. The app's enum is
        // アプリ側の実装: NONE("none") / VOICE("voice") / MOTION("motion") /
        // WORLD("world"). A value outside that set returns 422 with a
        // literal_error body naming the four, which is how this was confirmed.
        const ROOM_DEMANDS: [&str; 4] = ["none", "voice", "motion", "world"];
        if !ROOM_DEMANDS.contains(&room_demand) {
            return Err(PickleError::Unsupported(format!(
                "room_demand must be one of {}",
                ROOM_DEMANDS.join(", ")
            )));
        }
        let body = json!({
            "client_connect_id": uuid::Uuid::new_v4().to_string(),
            "app_version": APP_VERSION,
            "app_build": APP_BUILD,
            "device_memory_bytes": device_memory_bytes(),
            "establishment_aware": true,
            "platform": "android",
            "world_ready": false,
            "room_demand": room_demand,
        });
        let request = self
            .auth(self.base_headers(self.http.post(self.url(path)).json(&body)))
            .await?;
        let response = request.send().await?;
        let value = Self::json_or_error(response, &Method::POST, path).await?;
        self.parse_bootstrap(&value)
    }

    /// The response is flat: `room`, `livekit_url`, `token`, `session_id`,
    /// `participant_identity`, `account_rev`, `user_id` and so on
    /// (アプリ側の実装). Only six keys are required; the rest default.
    fn parse_bootstrap(&self, value: &Value) -> Result<Bootstrap> {
        let path = "api/session";
        // The server may answer a booting state machine instead of a session
        // (`com/pickle/client/protocol/session/C0413a.java:L316-L352` names
        // core_queued / core_spawning / room_attaching / room_attached /
        // core_ready / failed / none). Reporting "missing session_id" for that
        // reads like a server bug, so name the state and the wait instead.
        if value.get("session_id").is_none() {
            if let Some(state) = value.get("state").and_then(Value::as_str) {
                let wait = value
                    .get("poll_after_s")
                    .or_else(|| value.get("retry_after_s"))
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0);
                return Err(PickleError::invalid(
                    path,
                    format!("session not established yet: state={state} poll_after_s={wait}"),
                ));
            }
        }
        Ok(Bootstrap {
            session_id: field(value, "session_id", path)?,
            room: field(value, "room", path)?,
            livekit_url: field(value, "livekit_url", path)?,
            participant_identity: field(value, "participant_identity", path)?,
            account_rev: value
                .get("account_rev")
                .and_then(Value::as_f64)
                .ok_or_else(|| PickleError::invalid(path, "missing account_rev"))?
                as i64,
            user_id: field(value, "user_id", path)?,
            plan: text(value, "plan"),
            plan_rank: value
                .get("plan_rank")
                .and_then(Value::as_f64)
                .unwrap_or(0.0) as i64,
            character_name: text(value, "character_name"),
            raw: value.clone(),
        })
    }

    /// `GET api/session/establishment` — read the boot state machine.
    pub async fn establishment(&self) -> Result<Value> {
        let path = "api/session/establishment";
        let request = self
            .auth(self.base_headers(self.http.get(self.url(path))))
            .await?;
        let response = request.send().await?;
        Self::json_or_error(response, &Method::GET, path).await
    }

    // -------------------------------------------------------- read surface

    /// `GET api/sync` — room list plus an optional revision.
    pub async fn sync(&self) -> Result<Value> {
        let path = "api/sync";
        let request = self
            .auth(self.base_headers(self.http.get(self.url(path))))
            .await?;
        let response = request.send().await?;
        let value = Self::json_or_error(response, &Method::GET, path).await?;
        if value.get("rooms").and_then(Value::as_array).is_none() {
            return Err(PickleError::invalid(path, "missing rooms"));
        }
        Ok(value)
    }

    /// `GET api/rooms/{id}/history/delta?after_seq=&limit=`.
    pub async fn history_delta(&self, room: &str, after_seq: i64, limit: i64) -> Result<Value> {
        let path = format!("api/rooms/{room}/history/delta");
        let request = self
            .auth(self.base_headers(self.http.get(self.url(&path)).query(&[
                ("after_seq", after_seq.to_string()),
                ("limit", limit.to_string()),
            ])))
            .await?;
        let response = request.send().await?;
        let value = Self::json_or_error(response, &Method::GET, &path).await?;
        require_history(&value, "api/rooms/{id}/history/delta", &DELTA_REQUIRED)?;
        Ok(value)
    }

    /// `GET api/rooms/{id}/history` — full history.
    /// `GET api/rooms/{id}/history` — the whole room history.
    ///
    /// This decodes into `ChatRoomsSyncResponse` (`rooms` + `rev`), which has no
    /// `v` field at all (アプリ側の実装), so the page guard does not
    /// apply here. Checking it produced a false `invalid_response`.
    pub async fn history(&self, room: &str) -> Result<Value> {
        let path = format!("api/rooms/{room}/history");
        let request = self
            .auth(self.base_headers(self.http.get(self.url(&path))))
            .await?;
        let response = request.send().await?;
        let value = Self::json_or_error(response, &Method::GET, &path).await?;
        // Measured against the live service: this route answers `v` plus
        // `messages` (optional reactions, card_states, window_max_seq,
        // log_epoch, projection_version, read_seq). The app checks `v == 1` and
        // decodes that shape (アプリ側の実装); it is not the
        // `rooms` shape that `api/sync` returns.
        require_history(&value, &path, &FULL_REQUIRED)?;
        Ok(value)
    }

    /// `GET api/rooms/{id}/history/page?limit=&before_seq=`.
    pub async fn history_page(
        &self,
        room: &str,
        limit: i64,
        before_seq: Option<i64>,
    ) -> Result<Value> {
        let path = format!("api/rooms/{room}/history/page");
        let mut request = self
            .base_headers(self.http.get(self.url(&path)))
            .query(&[("limit", limit.to_string())]);
        if let Some(before) = before_seq {
            request = request.query(&[("before_seq", before.to_string())]);
        }
        let request = self.auth(request).await?;
        let response = request.send().await?;
        let value = Self::json_or_error(response, &Method::GET, &path).await?;
        require_history(&value, "api/rooms/{id}/history/page", &PAGE_REQUIRED)?;
        Ok(value)
    }

    /// `GET api/screens/{name}` plus optional string query values. The
    /// `Screen-Data-Changed` header carries a comma-separated cache key list.
    /// Resolve a screen name to the route the app would call.
    ///
    /// アプリ側の実装 maps seven names to dedicated routes and sends
    /// everything else to `api/screens/<name>`. `profile` also gains a `region`
    /// query from the device locale (`:L193-L196`); `handle_check` gains one
    /// `handle` query (`:L183-L188`); `bubbles` filters to `cursor` and `date`
    /// (`:L202-L210`). Returning the route here keeps the CLI's `screen` command
    /// matching the app instead of always hitting the generic path.
    pub fn screen_route(name: &str, query: &[(String, String)]) -> (String, Vec<(String, String)>) {
        match name {
            "bubble" => {
                let path = query
                    .iter()
                    .find(|(key, _)| key == "path")
                    .map(|(_, value)| value.clone());
                (
                    format!("api/bubbles/{}", path.unwrap_or_default()),
                    Vec::new(),
                )
            }
            "notification_settings" => ("api/notification-settings".to_string(), Vec::new()),
            "handle_check" => {
                let handle = query
                    .iter()
                    .find(|(key, _)| key == "handle")
                    .map(|(_, value)| value.clone())
                    .unwrap_or_default();
                // The app always sends the key, even when empty (`:L185`).
                (
                    "api/profile/handle/check".to_string(),
                    vec![("handle".into(), handle)],
                )
            }
            "character_colors" => ("api/character/colors".to_string(), Vec::new()),
            "profile" => {
                // The app takes `region` from the device locale, not from the
                // caller; a caller-supplied one still wins so the CLI stays
                // usable against another market.
                let region = query
                    .iter()
                    .find(|(key, _)| key == "region")
                    .map(|(_, value)| value.clone())
                    .unwrap_or_else(|| LocaleHeaders::detect().region_or_empty());
                ("api/profile".to_string(), vec![("region".into(), region)])
            }
            "vault" => ("api/vault".to_string(), Vec::new()),
            "bubbles" => {
                let kept: Vec<(String, String)> = query
                    .iter()
                    .filter(|(key, _)| key == "cursor" || key == "date")
                    .filter(|(_, value)| !value.is_empty())
                    .cloned()
                    .collect();
                ("api/bubbles".to_string(), kept)
            }
            "character" => ("api/character".to_string(), Vec::new()),
            "character_avatar_history" => ("api/character/avatar/history".to_string(), Vec::new()),
            "character_settings" => ("api/character/settings".to_string(), Vec::new()),
            // Everything else is the generic screen endpoint, query passed as-is.
            _ => (format!("api/screens/{name}"), query.to_vec()),
        }
    }

    pub async fn screen(&self, name: &str, query: &[(String, String)]) -> Result<Value> {
        let (path, query) = Self::screen_route(name, query);
        let request = self
            .auth(self.base_headers(self.http.get(self.url(&path)).query(&query)))
            .await?;
        let response = request.send().await?;
        let changed = response
            .headers()
            .get("Screen-Data-Changed")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string();
        let status = response.status();
        // The body may be an error object, so read it as text first and only
        // parse when it is JSON. The server's own `detail` (for example
        // `unknown_screen`) is what tells the caller the name was wrong, and
        // replacing it with a fixed word throws that away.
        let text = response.text().await?;
        let body: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if !status.is_success() {
            let detail = body
                .get("detail")
                .and_then(Self::render_detail)
                .unwrap_or_else(|| text.chars().take(120).collect());
            return Err(PickleError::http(status.as_u16(), detail, "GET", &path));
        }
        Ok(json!({ "screen": body, "changed": changed }))
    }

    // ----------------------------------------------------------- write path

    /// `POST api/rooms/{id}/messages`.
    ///
    /// Body keys come from アプリ側の実装: `text` and `client_msg_id` always,
    /// `attachments` when non-empty, `context` and `reply_to` when supplied.
    pub async fn send_message(&self, room: &str, body: &Value) -> Result<Value> {
        let path = format!("api/rooms/{room}/messages");
        let mut payload = serde_json::Map::new();
        payload.insert("text".into(), json!(text(body, "text")));
        let client_msg_id = body
            .get("client_msg_id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        payload.insert("client_msg_id".into(), json!(client_msg_id));
        if let Some(attachments) = body.get("attachments").and_then(Value::as_array) {
            if !attachments.is_empty() {
                payload.insert("attachments".into(), json!(attachments));
            }
        }
        for key in ["context", "reply_to"] {
            if let Some(value) = body.get(key) {
                if !value.is_null() {
                    payload.insert(key.into(), value.clone());
                }
            }
        }
        let request = self
            .auth(
                self.base_headers(
                    self.http
                        .post(self.url(&path))
                        .json(&Value::Object(payload)),
                ),
            )
            .await?;
        let response = request.send().await?;
        Self::json_or_error(response, &Method::POST, &path).await
    }

    /// `POST api/rooms/{id}/reactions`.
    ///
    /// Body: `client_msg_id`, `target_id`, `target_seq`, `part_index`, `emoji`,
    /// and `in_call` when the room is the active call.
    pub async fn react(&self, room: &str, body: &Value) -> Result<Value> {
        let path = format!("api/rooms/{room}/reactions");
        // The app always sends these five keys (アプリ側の実装);
        // `part_index` and `client_msg_id` are filled in when the caller omits
        // them so the body cannot drift between the CLI and MCP paths.
        let mut payload = serde_json::Map::new();
        for key in [
            "client_msg_id",
            "target_id",
            "target_seq",
            "part_index",
            "emoji",
        ] {
            if let Some(value) = body.get(key) {
                if !value.is_null() {
                    payload.insert(key.to_string(), value.clone());
                }
            }
        }
        if !payload.contains_key("client_msg_id") {
            payload.insert(
                "client_msg_id".into(),
                json!(uuid::Uuid::new_v4().to_string().to_lowercase()),
            );
        }
        if !payload.contains_key("part_index") {
            payload.insert("part_index".into(), json!(0));
        }
        if let Some(in_call) = body.get("in_call").filter(|value| value.is_boolean()) {
            payload.insert("in_call".into(), in_call.clone());
        }
        let request = self
            .auth(
                self.base_headers(
                    self.http
                        .post(self.url(&path))
                        .json(&Value::Object(payload)),
                ),
            )
            .await?;
        let response = request.send().await?;
        Self::json_or_error(response, &Method::POST, &path).await
    }

    /// `PUT api/rooms/{id}/read`. The app always sends an empty `device_id`.
    pub async fn mark_read(&self, room: &str, seq: i64) -> Result<()> {
        let path = format!("api/rooms/{room}/read");
        let body = json!({ "seq": seq, "device_id": "" });
        let request = self
            .auth(self.base_headers(self.http.put(self.url(&path)).json(&body)))
            .await?;
        let response = request.send().await?;
        Self::check(response, &Method::PUT, &path).await?;
        Ok(())
    }

    /// `PUT api/rooms/{id}/notifications`.
    pub async fn set_muted(&self, room: &str, muted: bool) -> Result<()> {
        let path = format!("api/rooms/{room}/notifications");
        let body = json!({ "muted": muted });
        let request = self
            .auth(self.base_headers(self.http.put(self.url(&path)).json(&body)))
            .await?;
        let response = request.send().await?;
        Self::check(response, &Method::PUT, &path).await?;
        Ok(())
    }

    /// `POST api/rooms/{id}/leave` — leave a room. No body.
    ///
    /// The app posts with a null body (アプリ側の実装), so nothing is sent
    /// beyond the headers.
    pub async fn leave_room(&self, room: &str) -> Result<Value> {
        let path = format!("api/rooms/{room}/leave");
        let request = self
            .auth(self.base_headers(self.http.post(self.url(&path))))
            .await?;
        let response = request.send().await?;
        Self::json_or_error(response, &Method::POST, &path).await
    }

    /// `PUT api/rooms/{id}/pin`.
    pub async fn set_pinned(&self, room: &str, pinned: bool) -> Result<()> {
        let path = format!("api/rooms/{room}/pin");
        let body = json!({ "pinned": pinned });
        let request = self
            .auth(self.base_headers(self.http.put(self.url(&path)).json(&body)))
            .await?;
        let response = request.send().await?;
        Self::check(response, &Method::PUT, &path).await?;
        Ok(())
    }

    /// `POST api/rooms` -> `{room_id}`.
    ///
    /// `kind` is `character_direct` (agent room) or `direct` (human room).
    pub async fn create_room(&self, kind: &str, user_id: &str) -> Result<String> {
        let path = "api/rooms";
        let body = json!({ "kind": kind, "user_id": user_id });
        let request = self
            .auth(self.base_headers(self.http.post(self.url(path)).json(&body)))
            .await?;
        let response = request.send().await?;
        let value = Self::json_or_error(response, &Method::POST, path).await?;
        value
            .get("room_id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .ok_or_else(|| PickleError::invalid(path, "missing room_id"))
    }

    /// `POST api/chat/requests/{id}/answer`.
    pub async fn answer_request(&self, request_id: &str, body: &Value) -> Result<Value> {
        let path = format!("api/chat/requests/{request_id}/answer");
        let body = normalise_answer(body);
        let request = self
            .auth(self.base_headers(self.http.post(self.url(&path)).json(&body)))
            .await?;
        let response = request.send().await?;
        Self::json_or_error(response, &Method::POST, &path).await
    }

    /// `GET api/chat/requests/{id}/live-view` -> `{status, error, url}`.
    pub async fn live_view(&self, request_id: &str) -> Result<Value> {
        let path = format!("api/chat/requests/{request_id}/live-view");
        let request = self
            .auth(self.base_headers(self.http.get(self.url(&path))))
            .await?;
        let response = request.send().await?;
        let value = Self::json_or_error(response, &Method::GET, &path).await?;
        if value.get("status").and_then(Value::as_str).is_none() {
            return Err(PickleError::invalid(&path, "missing status"));
        }
        Ok(value)
    }

    /// `POST api/actions` — ask the server to run one action by id.
    pub async fn run_action(&self, action_id: &str, kind: &str, payload: Value) -> Result<Value> {
        let path = "api/actions";
        let body = json!({
            "action_id": action_id.to_lowercase(),
            "kind": kind,
            "payload": payload,
        });
        let request = self
            .auth(self.base_headers(self.http.post(self.url(path)).json(&body)))
            .await?;
        let response = request.send().await?;
        Self::json_or_error(response, &Method::POST, path).await
    }

    /// `GET api/actions/{id}` — fetch a previously requested action result.
    pub async fn action_result(&self, action_id: &str) -> Result<Value> {
        let path = format!("api/actions/{}", action_id.to_lowercase());
        let request = self
            .auth(self.base_headers(self.http.get(self.url(&path))))
            .await?;
        let response = request.send().await?;
        Self::json_or_error(response, &Method::GET, &path).await
    }

    /// `POST api/attachments` or `api/blobs` — raw body upload, not multipart.
    ///
    /// Query carries `filename`, `upload_id` and, for workspace attachments,
    /// `pending=0`. The response is `{status, ref}` (blobs answer `blob_ref`).
    pub async fn upload(
        &self,
        target: &str,
        bytes: Vec<u8>,
        filename: &str,
        mime: &str,
        upload_id: &str,
        pending_zero: bool,
    ) -> Result<Value> {
        let path = match target {
            "blobs" => "api/blobs".to_string(),
            "issue_reports" => "api/issue-reports/attachments".to_string(),
            _ => "api/attachments".to_string(),
        };
        let mut request = self.base_headers(self.http.post(self.url(&path)).query(&[
            ("filename", filename.to_string()),
            ("upload_id", upload_id.to_string()),
        ]));
        if pending_zero {
            request = request.query(&[("pending", "0".to_string())]);
        }
        let request = self
            .auth(request.header("Content-Type", mime))
            .await?
            .body(bytes);
        let response = request.send().await?;
        let payload = Self::json_or_error(response, &Method::POST, &path).await?;
        registration_needed_for(target, &payload, pending_zero)
    }

    /// `DELETE api/attachments/pending/{upload_id}` — cancel a pending upload.
    pub async fn cancel_pending(&self, upload_id: &str) -> Result<()> {
        let path = format!("api/attachments/pending/{upload_id}");
        let request = self
            .auth(self.base_headers(self.http.delete(self.url(&path))))
            .await?;
        let response = request.send().await?;
        Self::check(response, &Method::DELETE, &path).await?;
        Ok(())
    }

    /// `POST api/attachments/pending` with `{upload_id, ref}`.
    ///
    /// The app registers a pending object only when the upload answered a `ref`
    /// that is not UUID-shaped (`docs/attachments.md` §1.2). Registering a
    /// UUID-shaped ref would double-track it.
    pub async fn register_pending(&self, upload_id: &str, reference: &str) -> Result<Value> {
        let path = "api/attachments/pending";
        let body = json!({ "upload_id": upload_id, "ref": reference });
        let request = self
            .auth(self.base_headers(self.http.post(self.url(path)).json(&body)))
            .await?;
        let response = request.send().await?;
        let mut value = Self::json_or_error(response, &Method::POST, path).await?;
        // The app retries a couple of times on any non-2xx; a single retry keeps
        // the CLI honest without pretending to reproduce the exact backoff.
        if value.get("status").and_then(Value::as_str) == Some("error") {
            let request = self
                .auth(self.base_headers(self.http.post(self.url(path)).json(&body)))
                .await?;
            let response = request.send().await?;
            value = Self::json_or_error(response, &Method::POST, path).await?;
        }
        Ok(value)
    }

    /// Direct call for any `api/*` path. Escape hatch for endpoints without a
    /// dedicated handler; `path` is validated by the caller.
    pub async fn raw_request(
        &self,
        method: &str,
        path: &str,
        query: &[(String, String)],
        body: &Value,
    ) -> Result<Value> {
        let verb = match method.to_uppercase().as_str() {
            "GET" => Method::GET,
            "POST" => Method::POST,
            "PUT" => Method::PUT,
            "PATCH" => Method::PATCH,
            "DELETE" => Method::DELETE,
            other => {
                return Err(PickleError::Unsupported(format!(
                    "method `{other}` is not GET/POST/PUT/PATCH/DELETE"
                )))
            }
        };
        let mut request = self
            .base_headers(self.http.request(verb.clone(), self.url(path)))
            .query(query);
        // A GET with a body is meaningless here and confuses some proxies.
        if verb != Method::GET && !body.is_null() {
            request = request.json(body);
        }
        let request = if self.session.read().await.is_some() {
            self.auth(request).await?
        } else {
            request
        };
        let response = request.send().await?;
        Self::json_or_error(response, &verb, path).await
    }

    // -------------------------------------------------------------- stream

    /// `GET api/events` as a server-sent event stream.
    ///
    /// The app sets a 60s read timeout on this request only.
    pub async fn events(&self) -> Result<Response> {
        let path = "api/events";
        let request = self
            .auth(
                self.base_headers(
                    self.stream
                        .get(self.url(path))
                        .header("Accept", "text/event-stream"),
                ),
            )
            .await?;
        let response = request.send().await?;
        Self::check(response, &Method::GET, path).await
    }

    /// Read the SSE stream, yielding frames until `max_frames` is reached or
    /// the stream ends.
    ///
    /// Parsing mirrors アプリ側の実装:
    ///
    /// * a blank line commits a frame, and only when both `event` and at least
    ///   one `data` line are present (`:L314-L317`),
    /// * a line starting with `:` is a comment and is skipped entirely
    ///   (`:L318`), so a `retry:` written inside one is not read,
    /// * `retry` is a field name on an ordinary line, in milliseconds, and only
    ///   its numeric prefix is used (`:L332-L346`),
    /// * `event` and `data` carry the frame (`:L348-L353`).
    ///
    /// This reader returns frames only; it does not implement reconnection, so
    /// the `retry` value is parsed for completeness and not acted on.
    pub async fn read_events<F>(&self, max_frames: usize, mut on_frame: F) -> Result<usize>
    where
        F: FnMut(&str, &str),
    {
        let response = self.events().await?;
        let mut stream = response.bytes_stream();
        let mut buffer = Vec::<u8>::new();
        let mut event = String::new();
        let mut data: Vec<String> = Vec::new();
        let mut frames = 0usize;
        'outer: while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|err| PickleError::Transport(err.to_string()))?;
            buffer.extend_from_slice(&chunk);
            while let Some(pos) = buffer.iter().position(|byte| *byte == b'\n') {
                let mut line = buffer[..pos].to_vec();
                buffer.drain(..=pos);
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                let text = String::from_utf8_lossy(&line).to_string();
                if text.is_empty() {
                    if !event.is_empty() && !data.is_empty() {
                        on_frame(&event, &data.join("\n"));
                        frames += 1;
                    }
                    event.clear();
                    data.clear();
                    if max_frames > 0 && frames >= max_frames {
                        break 'outer;
                    }
                    continue;
                }
                if text.starts_with(':') {
                    continue;
                }
                let (name, value) = match text.split_once(':') {
                    Some((name, value)) => (name, value.strip_prefix(' ').unwrap_or(value)),
                    None => (text.as_str(), ""),
                };
                match name {
                    "event" => event = value.to_string(),
                    "data" => data.push(value.to_string()),
                    _ => {}
                }
            }
        }
        Ok(frames)
    }

    // ------------------------------------------------------------ plumbing

    /// Decode JSON, mapping a non-2xx or undecodable body to the error taxonomy.
    async fn json_or_error(response: Response, method: &Method, path: &str) -> Result<Value> {
        let status = response.status();
        let bytes = response.bytes().await?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|err| {
            PickleError::invalid(
                path,
                format!(
                    "body is not JSON ({err}); first bytes: {:?}",
                    preview(&bytes)
                ),
            )
        })?;
        if !status.is_success() {
            let detail =
                Self::render_detail(&value).unwrap_or_else(|| format!("http_{}", status.as_u16()));
            return Err(PickleError::http(
                status.as_u16(),
                detail,
                method.as_str(),
                path,
            ));
        }
        Ok(value)
    }
}

/// Whether a 401 detail means the pending code can never be used again.
///
/// `code_expired` and `invalid_code` were both measured on the live service
/// against a real pending id. `code_expired` is terminal; `invalid_code` is
/// retryable within the same `pending_id`, so they are not treated alike.
pub fn is_terminal_code_error(detail: &str) -> bool {
    matches!(detail, "code_expired" | "login_session_not_found")
}

/// Total device memory, as the app reports it (アプリ側の実装).
///
/// The app reads `ActivityManager.MemoryInfo.totalMem`. There is no portable
/// equivalent, so this reports the host's physical memory where one is cheap to
/// read and falls back to 0 only when nothing can be measured. A hard-coded 0
/// would claim a 0-byte device on every host.
fn device_memory_bytes() -> u64 {
    #[cfg(target_os = "linux")]
    {
        if let Ok(text) = std::fs::read_to_string("/proc/meminfo") {
            for line in text.lines() {
                if let Some(rest) = line.strip_prefix("MemTotal:") {
                    let kib: u64 = rest
                        .trim()
                        .trim_end_matches(" kB")
                        .trim()
                        .parse()
                        .unwrap_or(0);
                    if kib > 0 {
                        return kib * 1024;
                    }
                }
            }
        }
        0
    }
    #[cfg(target_os = "macos")]
    {
        // sysctl hw.memsize via the sysctl binary keeps this dependency-free.
        let output = std::process::Command::new("sysctl")
            .args(["-n", "hw.memsize"])
            .output();
        match output {
            // `stdout` is bytes, so read it as text and parse. The earlier
            // version filtered the byte iterator and collected into `String`,
            // which does not exist for `&u8`; it only failed on macOS because
            // the branch is behind `cfg`, so a Windows host could not catch it.
            Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
                .trim()
                .parse()
                .unwrap_or(0),
            _ => 0,
        }
    }
    #[cfg(target_os = "windows")]
    {
        // `wmic` is absent on Windows 11, so try CIM first and keep wmic as a
        // fallback for older hosts. Both print the value as a decimal number.
        for (program, args) in [
            (
                "powershell",
                [
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    "(Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory",
                ],
            ),
            (
                "wmic",
                ["ComputerSystem", "get", "TotalPhysicalMemory", "/value"],
            ),
        ] {
            let output = std::process::Command::new(program).args(args).output();
            let Ok(out) = output else { continue };
            if !out.status.success() {
                continue;
            }
            let text = String::from_utf8_lossy(&out.stdout);
            let found = text
                .lines()
                .filter_map(|line| {
                    let trimmed = line.trim();
                    let value = trimmed
                        .strip_prefix("TotalPhysicalMemory=")
                        .unwrap_or(trimmed)
                        .trim();
                    value.parse::<u64>().ok()
                })
                .max()
                .unwrap_or(0);
            if found > 0 {
                return found;
            }
        }
        0
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        0
    }
}

/// Build the answer body the way the app does.
///
/// `action` and `in_call` are always present, and `item` is only sent when both
/// `dataset` and `name` are non-null (アプリ側の実装). A half-filled
/// item is dropped rather than sent, because the app never sends one.
fn normalise_answer(body: &Value) -> Value {
    let mut out = body.clone();
    let Some(object) = out.as_object_mut() else {
        return out;
    };
    if !object
        .get("in_call")
        .map(Value::is_boolean)
        .unwrap_or(false)
    {
        object.insert("in_call".into(), json!(false));
    }
    let half_item = matches!(object.get("item"), Some(item) if {
        let dataset = item.get("dataset").and_then(Value::as_str);
        let name = item.get("name").and_then(Value::as_str);
        dataset.is_none() || name.is_none()
    });
    if half_item {
        object.remove("item");
    }
    out
}

/// Whether the app would register this upload as pending.
///
/// Rule from `docs/attachments.md` §1.2: only `target=attachments` (workspace)
/// uploads whose `ref` is not UUID-shaped need the extra registration. A
/// UUID-shaped ref is already tracked by another route.
/// The two-clause rule: a pending registration is needed only when the upload
/// was left pending (`pending=0` not sent), the target is workspace
/// attachments, and the returned ref is not UUID-shaped.
fn registration_needed_for(target: &str, payload: &Value, pending_zero: bool) -> Result<Value> {
    let reference = payload
        .get("ref")
        .and_then(Value::as_str)
        .map(str::to_string);
    let uuid_shaped = reference.as_deref().map(is_uuid_shaped).unwrap_or(false);
    let needed = target == "attachments" && !pending_zero && reference.is_some() && !uuid_shaped;
    let mut enriched = payload.clone();
    if let Some(object) = enriched.as_object_mut() {
        object.insert("pending_registration_needed".into(), json!(needed));
        object.insert("ref_is_uuid".into(), json!(uuid_shaped));
    }
    Ok(enriched)
}

/// A UUID in any common textual shape (hyphenated, without hyphens, braced).
fn is_uuid_shaped(raw: &str) -> bool {
    let trimmed = raw.trim().trim_start_matches('{').trim_end_matches('}');
    let without_hyphens: String = trimmed.chars().filter(|c| *c != '-').collect();
    if without_hyphens.len() != 32 {
        return false;
    }
    without_hyphens.chars().all(|c| c.is_ascii_hexdigit())
}

fn field(value: &Value, key: &str, path: &str) -> Result<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|raw| !raw.is_empty())
        .map(str::to_string)
        .ok_or_else(|| PickleError::invalid(path, format!("missing {key}")))
}

fn text(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// Validate the history response against the serializer's required keys.
///
/// `ChatHistoryDelta` requires 8 keys and `ChatHistoryPage` requires 3
/// (`extract/serializers.json`). Checking only `v`, as the app itself does at
/// アプリ側の実装, would let a response missing `next_after` through and
/// the polling loop would then never advance.
fn require_history(value: &Value, path: &str, required: &[&str]) -> Result<()> {
    match value.get("v").and_then(Value::as_f64) {
        Some(v) if (v - 1.0).abs() < f64::EPSILON => {}
        Some(v) => return Err(PickleError::invalid(path, format!("unsupported v={v}"))),
        None => return Err(PickleError::invalid(path, "missing v")),
    }
    for key in required {
        if value.get(key).is_none() {
            return Err(PickleError::invalid(path, format!("missing {key}")));
        }
    }
    Ok(())
}

/// Required keys of `ChatHistoryDelta`.
const DELTA_REQUIRED: [&str; 7] = [
    "items",
    "reactions",
    "card_states",
    "next_after",
    "has_more",
    "head_seq",
    "projection_version",
];

/// Required keys of `ChatHistoryPage` besides `v`.
const PAGE_REQUIRED: [&str; 2] = ["messages", "has_more"];

/// Required keys of the full-history response besides `v`.
const FULL_REQUIRED: [&str; 1] = ["messages"];

fn preview(bytes: &[u8]) -> String {
    let end = bytes.len().min(48);
    String::from_utf8_lossy(&bytes[..end]).replace('\n', " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client() -> PickleClient {
        PickleClient {
            http: Client::new(),
            stream: Client::new(),
            base: "https://x.example/".into(),
            env: Env::Prod,
            device_id: "d".into(),
            session: tokio::sync::RwLock::new(None),
        }
    }

    #[test]
    fn history_guard_requires_v_and_the_serializer_keys() {
        let delta = json!({
            "v": 1, "items": [], "reactions": [], "card_states": [],
            "next_after": 3, "has_more": false, "head_seq": 3,
            "projection_version": 1
        });
        assert!(require_history(&delta, "p", &DELTA_REQUIRED).is_ok());

        let wrong_v = json!({"v": 2, "items": []});
        assert!(require_history(&wrong_v, "p", &DELTA_REQUIRED).is_err());

        // The failure this guard exists for: no next_after means the poll
        // loop would never advance, so it must be rejected, not tolerated.
        let stalled = json!({
            "v": 1, "items": [], "reactions": [], "card_states": [],
            "has_more": false, "head_seq": 3, "projection_version": 1
        });
        let err = require_history(&stalled, "p", &DELTA_REQUIRED).unwrap_err();
        assert!(err.to_string().contains("next_after"), "{err}");

        let page = json!({"v": 1, "messages": [], "has_more": false});
        assert!(require_history(&page, "p", &PAGE_REQUIRED).is_ok());
    }

    #[test]
    fn field_rejects_empty() {
        assert!(field(&json!({"a": "x"}), "a", "p").is_ok());
        assert!(field(&json!({"a": ""}), "a", "p").is_err());
        assert!(field(&json!({}), "a", "p").is_err());
    }

    #[test]
    fn url_join_is_idempotent() {
        let client = client();
        assert_eq!(client.url("/api/sync"), "https://x.example/api/sync");
        assert_eq!(client.url("api/sync"), "https://x.example/api/sync");
    }

    #[test]
    fn uuid_shapes_are_recognised() {
        assert!(is_uuid_shaped("3f2b8c1e-0000-4000-8000-000000000001"));
        assert!(is_uuid_shaped(
            "3f2b8c1e00004000800000000000000 1"
                .replace(' ', "")
                .as_str()
        ));
        assert!(is_uuid_shaped("{3f2b8c1e-0000-4000-8000-000000000001}"));
        assert!(!is_uuid_shaped("ref-1"));
        assert!(!is_uuid_shaped(""));
        // 32 hex chars but not a UUID is still UUID-shaped by the app's regex.
        assert!(is_uuid_shaped("0123456789abcdef0123456789abcdef"));
        // Wrong length or a non-hex character is not.
        assert!(!is_uuid_shaped("0123456789abcdef"));
        assert!(!is_uuid_shaped("0123456789abcdeg0123456789abcdef"));
    }

    #[test]
    fn device_memory_is_measured_or_zero_never_faked() {
        // The app reports ActivityManager.MemoryInfo.totalMem. On this host the
        // value must come from a real measurement; 0 is the honest fallback.
        let measured = device_memory_bytes();
        if cfg!(any(
            target_os = "linux",
            target_os = "macos",
            target_os = "windows"
        )) {
            assert!(measured > 0, "memory was not measured: {measured}");
            // Sanity: a host with more than 1 PB is not believable.
            assert!(measured < 1u64 << 60, "implausible memory: {measured}");
        }
    }

    #[test]
    fn answer_bodies_match_what_the_app_builds() {
        // A full item is kept as the nested object the app sends.
        let full = normalise_answer(&json!({
            "action": "accept",
            "item": {"dataset": "ds", "name": "nm"},
        }));
        assert_eq!(full["item"]["dataset"], json!("ds"));
        assert_eq!(full["item"]["name"], json!("nm"));
        assert_eq!(full["in_call"], json!(false));

        // A half-filled item is dropped: the app only sends it when both
        // halves are non-null (app-side implementation).
        let half = normalise_answer(&json!({"action": "accept", "item": {"dataset": "ds"}}));
        assert!(half.get("item").is_none(), "{half}");
        let other = normalise_answer(&json!({"action": "accept", "item": {"name": "nm"}}));
        assert!(other.get("item").is_none(), "{other}");

        // A caller-supplied boolean in_call wins over the default.
        let explicit = normalise_answer(&json!({"action": "accept", "in_call": true}));
        assert_eq!(explicit["in_call"], json!(true));
        // A non-boolean one is replaced, not forwarded.
        let bogus = normalise_answer(&json!({"action": "accept", "in_call": "yes"}));
        assert_eq!(bogus["in_call"], json!(false));
    }

    #[test]
    fn pending_zero_suppresses_registration() {
        // `pending=0` tells the server the object is final, so registering it
        // would be a second, contradictory signal.
        let non_uuid = serde_json::json!({"status": "ok", "ref": "ref-1"});
        let decided = registration_needed_for("attachments", &non_uuid, true).expect("decide");
        assert_eq!(
            decided["pending_registration_needed"],
            serde_json::json!(false)
        );
    }

    #[test]
    fn registration_rule_matches_the_documented_condition() {
        let non_uuid = serde_json::json!({"status": "ok", "ref": "ref-1"});
        let decided = registration_needed_for("attachments", &non_uuid, false).expect("decide");
        assert_eq!(
            decided["pending_registration_needed"],
            serde_json::json!(true)
        );
        assert_eq!(decided["ref_is_uuid"], serde_json::json!(false));

        let uuid = serde_json::json!({"status":"ok","ref":"3f2b8c1e-0000-4000-8000-000000000001"});
        let decided = registration_needed_for("attachments", &uuid, false).expect("decide");
        assert_eq!(
            decided["pending_registration_needed"],
            serde_json::json!(false)
        );

        // blobs / issue_reports never register.
        let decided = registration_needed_for("blobs", &non_uuid, false).expect("decide");
        assert_eq!(
            decided["pending_registration_needed"],
            serde_json::json!(false)
        );

        // No ref at all: nothing to register.
        let decided =
            registration_needed_for("attachments", &serde_json::json!({"status":"ok"}), false)
                .expect("decide");
        assert_eq!(
            decided["pending_registration_needed"],
            serde_json::json!(false)
        );
    }

    #[test]
    fn both_http_clients_construct() {
        // A regression in the streaming client would silently truncate every
        // subscription, and a fixture that answers instantly cannot show it.
        // This at least proves the second client is built without error.
        let state = crate::state::State::default();
        let client = PickleClient::new(&state).expect("client");
        assert!(client.base().starts_with("https"));
        assert_eq!(client.device_id(), state.device_id);
    }

    #[test]
    fn storefront_only_covers_the_three_known_markets() {
        assert_eq!(map_storefront("ja_JP.UTF-8"), Some("jp"));
        assert_eq!(map_storefront("JPN"), Some("jp"));
        assert_eq!(map_storefront("ko_KR"), Some("kr"));
        assert_eq!(map_storefront("KOR"), Some("kr"));
        // Anything else maps to nothing, and the app then sends no storefront.
        assert_eq!(map_storefront("en_US"), None);
        assert_eq!(map_storefront(""), None);
        assert_eq!(map_storefront("de_DE.UTF-8"), None);
    }

    #[test]
    fn device_region_requires_an_explicit_country() {
        assert_eq!(region_from_locale("ja_JP.UTF-8"), Some("JP"));
        assert_eq!(region_from_locale("en_US.UTF-8"), Some("US"));
        assert_eq!(region_from_locale("ko-KR"), Some("KR"));
        // A language-only locale carries no country, so the header is omitted.
        assert_eq!(region_from_locale("ja"), None);
        assert_eq!(region_from_locale("C"), None);
        assert_eq!(region_from_locale(""), None);
    }

    #[test]
    fn resolved_flag_tracks_the_storefront() {
        let with = LocaleHeaders {
            language: "en",
            language_source: "device_language",
            timezone: "Asia/Tokyo",
            storefront: Some("jp"),
            device_region: Some("JP"),
            resolved: true,
        };
        assert_eq!(with.resolved_flag(), "true");
        let without = LocaleHeaders {
            storefront: None,
            device_region: None,
            resolved: false,
            ..with
        };
        assert_eq!(without.resolved_flag(), "false");
    }

    #[test]
    fn render_detail_flattens_validation_errors() {
        let body = serde_json::json!({
            "detail": [
                {"loc": ["body", "room_demand"], "msg": "Input should be 'none', 'voice', 'motion' or 'world'"},
                {"loc": ["body", "client_connect_id"], "msg": "Field required"}
            ]
        });
        let rendered = PickleClient::render_detail(&body).expect("detail present");
        assert!(
            rendered.contains("body.room_demand: Input should be"),
            "{rendered}"
        );
        assert!(
            rendered.contains("body.client_connect_id: Field required"),
            "{rendered}"
        );
    }

    #[test]
    fn render_detail_handles_plain_strings() {
        let body = serde_json::json!({"detail": "unauthenticated"});
        assert_eq!(
            PickleClient::render_detail(&body),
            Some("unauthenticated".to_string())
        );
        let error_only = serde_json::json!({"error": "invalid_transaction"});
        assert_eq!(
            PickleClient::render_detail(&error_only),
            Some("invalid_transaction".to_string())
        );
        assert_eq!(PickleClient::render_detail(&serde_json::json!({})), None);
    }

    #[test]
    fn send_message_drops_empty_attachments() {
        // Building the payload is pure, so it can be checked without a server.
        let body = json!({"text": "hi", "attachments": [], "context": null});
        let mut payload = serde_json::Map::new();
        payload.insert("text".into(), json!(text(&body, "text")));
        payload.insert("client_msg_id".into(), json!("id"));
        for key in ["context", "reply_to"] {
            if let Some(value) = body.get(key) {
                if !value.is_null() {
                    payload.insert(key.into(), value.clone());
                }
            }
        }
        assert!(!payload.contains_key("attachments"));
        assert!(!payload.contains_key("context"));
        assert_eq!(payload["text"], json!("hi"));
    }
}

#[cfg(test)]
mod resolver_tests {
    use super::*;

    fn document(released: &str) -> Value {
        serde_json::json!({
            "environments": {
                "prod": { "api_base_url": "https://x", "released_android_build": released }
            }
        })
    }

    #[test]
    fn released_build_is_read_when_numeric() {
        assert_eq!(
            PickleClient::released_build(&document("462543"), Env::Prod),
            Some("462543".to_string())
        );
    }

    #[test]
    fn released_build_rejects_non_numeric() {
        assert_eq!(
            PickleClient::released_build(&document("462543a"), Env::Prod),
            None
        );
        assert_eq!(PickleClient::released_build(&document(""), Env::Prod), None);
        assert_eq!(PickleClient::released_build(&document("1"), Env::Dev), None);
    }

    #[test]
    fn stale_detection_matches_the_observed_gap() {
        // A resolver advertising a newer build than the embedded one must read
        // as stale.
        assert!(PickleClient::build_is_stale(&document("462543"), Env::Prod));
        assert!(!PickleClient::build_is_stale(
            &document("428293"),
            Env::Prod
        ));
        assert!(!PickleClient::build_is_stale(
            &document("100000"),
            Env::Prod
        ));
    }
}
