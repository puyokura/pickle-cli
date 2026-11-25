//! Stored session state and its file layout.
//!
//! The Android app keeps `login_session.<env>.bin` under `filesDir`, encrypted
//! with an AndroidKeyStore key (`pickle.login.wrap`, AES/GCM). That ciphertext
//! is not portable: the key never leaves the device. This CLI therefore keeps
//! its own plaintext JSON with owner-only permissions, and never tries to read
//! the app's file.

use crate::error::{PickleError, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Environment selector. The app has `prod` / `dev` / `alpha` (plus `local`
/// for local builds); the API base URL differs per environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Env {
    Prod,
    Dev,
    Alpha,
}

impl Env {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prod => "prod",
            Self::Dev => "dev",
            Self::Alpha => "alpha",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "prod" | "production" => Some(Self::Prod),
            "dev" | "development" => Some(Self::Dev),
            "alpha" => Some(Self::Alpha),
            _ => None,
        }
    }

    /// Build-time fallback URLs, taken from the app's default table.
    pub fn fallback_base(self) -> &'static str {
        match self {
            Self::Prod => "https://character-api-prod.picklemem.com",
            Self::Dev => "https://character-api-dev.picklemem.com",
            Self::Alpha => "https://character-api-alpha.picklemem.com",
        }
    }
}

/// One issued login session, mirroring the app's `IssuedLoginSession`
/// (`userId`, `loginSessionToken`, `loginSessionJwt`, `expiresAt`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginSession {
    pub user_id: String,
    pub login_session_token: String,
    pub login_session_jwt: String,
    pub expires_at: String,
}

/// Everything persisted between runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    pub env: Env,
    /// Resolved API base URL (may be refreshed from the endpoint resolver).
    pub api_base: String,
    /// Stable per-install device id. The app derives it from `android_id`;
    /// the CLI generates one and keeps it so read receipts stay consistent.
    pub device_id: String,
    pub session: Option<LoginSession>,
    /// `pending_id` from `api/login-sessions/phone-code`, between the two OTP steps.
    #[serde(default)]
    pub pending_phone_code: Option<PendingPhoneCode>,
    /// Server-side session bootstrap values (`api/session`).
    #[serde(default)]
    pub bootstrap: Option<Bootstrap>,
    /// When the API base was last resolved from `pickle.com`, as Unix seconds.
    /// The document carries `ttl_s` (300 on the live service), and the app
    /// re-resolves when it expires (アプリ側の実装).
    #[serde(default)]
    pub base_resolved_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingPhoneCode {
    pub pending_id: String,
    pub region: String,
    pub expires_in: i64,
}

/// The subset of the `api/session` response this CLI keeps across invocations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bootstrap {
    pub session_id: String,
    pub room: String,
    pub livekit_url: String,
    pub participant_identity: String,
    pub account_rev: i64,
    pub user_id: String,
    pub plan: String,
    pub plan_rank: i64,
    pub character_name: String,
    /// Full response, so callers can inspect anything not modelled here. The
    /// `token` field inside it is a short-lived LiveKit credential and is never
    /// printed by the CLI.
    #[serde(default)]
    pub raw: serde_json::Value,
}

impl Default for State {
    fn default() -> Self {
        Self {
            env: Env::Prod,
            api_base: Env::Prod.fallback_base().to_string(),
            device_id: uuid::Uuid::new_v4().to_string(),
            session: None,
            pending_phone_code: None,
            bootstrap: None,
            base_resolved_at: None,
        }
    }
}

/// Environment variables that can name the state file, in priority order.
///
/// `PICKLE_CLI_STATE` is the older, narrower name and stays first so existing
/// setups keep winning. `PICKLE_STATE` and `PICKLE_MCP_STATE` are the
/// product-wide spellings the sibling CLIs also accept, so one rule covers all
/// three: `<PRODUCT>_STATE` and `<PRODUCT>_MCP_STATE` both work.
pub const STATE_ENV_VARS: &[&str] = &["PICKLE_CLI_STATE", "PICKLE_STATE", "PICKLE_MCP_STATE"];

/// The fallback location, matching the sibling CLIs' order.
///
/// `XDG_CONFIG_HOME`, then `%APPDATA%` on Windows, then `HOME`. The `dirs`
/// crate gave the same answer but pulled in `option-ext`, which is MPL-2.0 and
/// not compatible with this project's AGPL-3.0-or-later; the order is written
/// out here instead so the licence set stays clean.
pub fn resolve_default_path<F>(var: F) -> PathBuf
where
    F: Fn(&str) -> Option<String>,
{
    if let Some(dir) = var("XDG_CONFIG_HOME") {
        if !dir.trim().is_empty() {
            return PathBuf::from(dir).join("pickle-cli").join("session.json");
        }
    }
    if cfg!(windows) {
        if let Some(appdata) = var("APPDATA") {
            if !appdata.trim().is_empty() {
                return PathBuf::from(appdata)
                    .join("pickle-cli")
                    .join("session.json");
            }
        }
    }
    if let Some(home) = var("HOME") {
        if !home.trim().is_empty() {
            return PathBuf::from(home)
                .join(".config")
                .join("pickle-cli")
                .join("session.json");
        }
    }
    PathBuf::from(".pickle-session.json")
}

/// Resolve the state path from an environment lookup.
///
/// Split out from [`State::default_path`] so the priority order can be tested
/// without mutating the process environment.
pub fn resolve_state_path<F>(var: F) -> Option<PathBuf>
where
    F: Fn(&str) -> Option<String>,
{
    for name in STATE_ENV_VARS {
        if let Some(value) = var(name) {
            // An empty value is set-but-unusable. Falling through to the next
            // name is right: treating it as a path would put the session file
            // in the current directory.
            if !value.trim().is_empty() {
                return Some(PathBuf::from(value));
            }
        }
    }
    None
}

impl State {
    /// Default state location: the environment variables in [`STATE_ENV_VARS`],
    /// else the platform config dir.
    pub fn default_path() -> Result<PathBuf> {
        Ok(resolve_state_path(|name| std::env::var(name).ok())
            .unwrap_or_else(|| resolve_default_path(|name| std::env::var(name).ok())))
    }

    /// Load from `path`, or return defaults when the file is absent.
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(raw) => serde_json::from_str(&raw)
                .map_err(|err| PickleError::State(format!("{}: {err}", path.display()))),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(PickleError::State(format!("{}: {err}", path.display()))),
        }
    }

    /// Write atomically: a sibling `.tmp` file then a rename, mode 0600.
    ///
    /// A half-written state file would lose the login session, and that costs
    /// a new SMS to recover, so the write is never in place.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let body = serde_json::to_vec_pretty(self)
            .map_err(|err| PickleError::State(format!("serialize: {err}")))?;
        let mut tmp = path.to_path_buf();
        tmp.set_extension("json.tmp");
        std::fs::write(&tmp, &body)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
        }
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

/// Copy the live session (including a refreshed JWT) into the state file.
///
/// Errors are swallowed on purpose: a failed save must not turn a successful
/// tool call into a failure, and the session is still usable in memory.
pub async fn persist_session_from_client(client: &crate::client::PickleClient) {
    let session = client.live_session().await;
    let Ok(path) = State::default_path() else {
        return;
    };
    // Nothing to write when the client has no session. Clearing the file from
    // here would let any process that merely failed to load the session erase a
    // good one, so `logout` is the only path that removes it.
    let (Some(session), Ok(text)) = (session, std::fs::read_to_string(&path)) else {
        return;
    };
    if let Ok(mut state) = serde_json::from_str::<State>(&text) {
        state.session = Some(session);
        let _ = state.save(&path);
    }
}

/// Decide a state path from the override value alone.
///
/// An empty or whitespace-only value is not a path: treating it as one produced
/// "The system cannot find the path specified" and hid which setting was wrong.
/// Remaining seconds before the JWT expires, if it carries an `exp` claim.
///
/// The app decodes the JWT itself and keeps a 30 second margin
/// (アプリ側の実装). The CLI does the same so a request is never sent with a
/// token that dies in flight.
pub fn jwt_seconds_left(jwt: &str) -> Option<i64> {
    use base64::Engine;
    let mut parts = jwt.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let value: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
    let exp = value.get("exp")?.as_f64()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs_f64();
    Some((exp - now) as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_roundtrip() {
        for env in [Env::Prod, Env::Dev, Env::Alpha] {
            assert_eq!(Env::parse(env.as_str()), Some(env));
        }
        assert_eq!(Env::parse("PRODUCTION"), Some(Env::Prod));
        assert_eq!(Env::parse("nope"), None);
    }

    #[test]
    fn state_default_has_device_id() {
        let state = State::default();
        assert_eq!(state.env, Env::Prod);
        assert!(!state.device_id.is_empty());
        assert_eq!(state.api_base, Env::Prod.fallback_base());
    }

    #[test]
    fn state_roundtrip_preserves_session() {
        let state = State {
            session: Some(LoginSession {
                user_id: "u".into(),
                login_session_token: "t".into(),
                login_session_jwt: "j".into(),
                expires_at: "2027-01-01T00:00:00Z".into(),
            }),
            ..State::default()
        };
        let json = serde_json::to_string(&state).unwrap();
        let back: State = serde_json::from_str(&json).unwrap();
        assert_eq!(back.session.unwrap().user_id, "u");
    }

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |name: &str| {
            owned
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        }
    }

    #[test]
    fn the_new_state_names_are_honoured_in_order() {
        // One rule for the three sibling CLIs: <PRODUCT>_STATE and
        // <PRODUCT>_MCP_STATE both work, and the older CLI name stays first.
        assert_eq!(
            STATE_ENV_VARS,
            &["PICKLE_CLI_STATE", "PICKLE_STATE", "PICKLE_MCP_STATE"]
        );
        let path = resolve_state_path(lookup(&[("PICKLE_MCP_STATE", "/c/three.json")]));
        assert_eq!(path, Some(std::path::PathBuf::from("/c/three.json")));
        let path = resolve_state_path(lookup(&[("PICKLE_STATE", "/b/two.json")]));
        assert_eq!(path, Some(std::path::PathBuf::from("/b/two.json")));
        // Both set: the older, narrower name wins.
        let path = resolve_state_path(lookup(&[
            ("PICKLE_CLI_STATE", "/a/one.json"),
            ("PICKLE_STATE", "/b/two.json"),
        ]));
        assert_eq!(path, Some(std::path::PathBuf::from("/a/one.json")));
    }

    #[test]
    fn an_empty_state_override_falls_through() {
        // An empty value is not a path: it must not put the session in the
        // current directory, and the next name must get a chance.
        assert_eq!(
            resolve_state_path(lookup(&[("PICKLE_CLI_STATE", "   ")])),
            None
        );
        assert_eq!(
            resolve_state_path(lookup(&[
                ("PICKLE_CLI_STATE", ""),
                ("PICKLE_STATE", "/b/two.json"),
            ])),
            Some(std::path::PathBuf::from("/b/two.json"))
        );
        assert_eq!(resolve_state_path(lookup(&[])), None);
    }

    #[test]
    fn save_then_load_roundtrip_in_temp_dir() {
        let dir = std::env::temp_dir().join(format!("pickle-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.json");
        let state = State {
            device_id: "fixed-device".into(),
            ..State::default()
        };
        state.save(&path).unwrap();
        let loaded = State::load(&path).unwrap();
        assert_eq!(loaded.device_id, "fixed-device");
        assert!(path.exists());
        assert!(!dir.join("session.json.tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_missing_file_returns_defaults() {
        let path =
            std::env::temp_dir().join(format!("pickle-missing-{}.json", uuid::Uuid::new_v4()));
        let state = State::load(&path).unwrap();
        assert!(state.session.is_none());
    }

    #[test]
    fn jwt_exp_is_read() {
        use base64::Engine;
        let payload =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"exp":2000000000}"#);
        let jwt = format!("eyJhbGciOiJub25lIn0.{payload}.sig");
        let left = jwt_seconds_left(&jwt).expect("exp present");
        assert!(left > 0, "left={left}");
    }

    #[test]
    fn jwt_without_exp_is_none() {
        assert_eq!(jwt_seconds_left("not-a-jwt"), None);
    }
}
