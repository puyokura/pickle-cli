//! Errors surfaced by the client. One type per failure layer so callers can
//! tell "the token expired" from "the network died" without parsing messages.

use std::fmt;

/// Failure classes that the CLI and MCP layers branch on.
#[derive(Debug)]
pub enum PickleError {
    /// No stored session: the caller must run `pickle login` first.
    NoSession,
    /// A stored pending phone code can no longer be exchanged.
    ///
    /// The server answers `code_expired` (or a similar terminal 401) and the
    /// `pending_id` cannot be retried. Keeping it on disk would make the next
    /// `login --finish` replay a dead id and fail identically, with no hint
    /// that a fresh `login --start` is needed.
    PendingCodeDead(String),
    /// A session exists but its JWT has expired and the refresh failed, so
    /// every request would answer 401. Distinct from `NoSession` because the
    /// fix is the same but the diagnosis is not.
    SessionExpired,
    /// HTTP status outside 2xx with a server-supplied detail when present.
    Http {
        status: u16,
        detail: String,
        method: String,
        path: String,
    },
    /// Response body did not match the contract the app expects.
    InvalidResponse { path: String, reason: String },
    /// Transport-level failure (DNS, TLS, timeout, broken stream).
    Transport(String),
    /// Local I/O or state-file problem.
    State(String),
    /// The caller asked for something the contract does not support.
    Unsupported(String),
}

impl PickleError {
    pub fn http(status: u16, detail: impl Into<String>, method: &str, path: &str) -> Self {
        Self::Http {
            status,
            detail: detail.into(),
            method: method.to_string(),
            path: path.to_string(),
        }
    }

    pub fn invalid(path: &str, reason: impl Into<String>) -> Self {
        Self::InvalidResponse {
            path: path.to_string(),
            reason: reason.into(),
        }
    }
}

impl fmt::Display for PickleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSession => write!(f, "no stored session (run `pickle login`)"),
            Self::SessionExpired => write!(
                f,
                "session expired and refresh failed (run `pickle login` again)"
            ),
            Self::PendingCodeDead(reason) => write!(
                f,
                "phone code unusable ({reason}); run `pickle login --start` again"
            ),
            Self::Http {
                status,
                detail,
                method,
                path,
            } => write!(f, "{method} {path} -> {status}: {detail}"),
            Self::InvalidResponse { path, reason } => {
                write!(f, "{path}: invalid response: {reason}")
            }
            Self::Transport(message) => write!(f, "transport: {message}"),
            Self::State(message) => write!(f, "state: {message}"),
            Self::Unsupported(message) => write!(f, "unsupported: {message}"),
        }
    }
}

impl std::error::Error for PickleError {}

impl From<reqwest::Error> for PickleError {
    fn from(value: reqwest::Error) -> Self {
        Self::Transport(value.to_string())
    }
}

impl From<std::io::Error> for PickleError {
    fn from(value: std::io::Error) -> Self {
        Self::State(value.to_string())
    }
}

pub type Result<T> = std::result::Result<T, PickleError>;
