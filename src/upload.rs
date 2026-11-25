//! Attachment upload helpers.
//!
//! The app uploads raw bytes, not multipart (アプリ側の実装 is a streaming
//! RequestBody). Query keys are `filename`, `upload_id` and, for workspace
//! attachments, `pending=0`. The server answers `{status, ref}` (blobs answer
//! `blob_ref`), and a ref only becomes permanent when a message references it.

use crate::client::PickleClient;
use crate::error::{PickleError, Result};
use serde_json::{json, Value};
use std::path::Path;

/// Whether the upload should carry `pending=0`.
///
/// The app does not take this as an input; it derives it
/// (`docs/attachments.md` §1.1): the `pending` flag turns on for a workspace
/// (attachments) upload whose kind is not video (アプリ側の実装),
/// and `pending=0` is sent exactly when the flag is **off**. So:
///
/// * attachments + non-video  -> no `pending=0` (left pending, resolved by the post)
/// * attachments + video      -> `pending=0`
/// * blobs / issue_reports    -> `pending=0` (never pending)
pub fn needs_pending_zero(target: &str, mime: &str) -> bool {
    match target {
        "attachments" => mime.starts_with("video/"),
        // Everything else is outside the workspace path and is final on upload.
        _ => true,
    }
}

pub fn guess_mime(path: &str) -> &'static str {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".png") {
        "image/png"
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg"
    } else if lower.ends_with(".webp") {
        "image/webp"
    } else if lower.ends_with(".gif") {
        "image/gif"
    } else if lower.ends_with(".mp4") {
        "video/mp4"
    } else if lower.ends_with(".mov") {
        "video/quicktime"
    } else if lower.ends_with(".m4a") || lower.ends_with(".aac") {
        "audio/mp4"
    } else if lower.ends_with(".mp3") {
        "audio/mpeg"
    } else if lower.ends_with(".pdf") {
        "application/pdf"
    } else {
        "application/octet-stream"
    }
}

/// Read one local file and upload it. Returns the server payload with the ref.
pub async fn upload_file(
    client: &PickleClient,
    path: &str,
    target: &str,
    mime: &str,
) -> Result<String> {
    let file = Path::new(path);
    let bytes = std::fs::read(file).map_err(|err| PickleError::State(format!("{path}: {err}")))?;
    if bytes.is_empty() {
        return Err(PickleError::Unsupported(format!("{path} is empty")));
    }
    let filename = file
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .ok_or_else(|| PickleError::Unsupported(format!("{path}: no file name")))?;
    let upload_id = uuid::Uuid::new_v4().to_string().to_lowercase();
    let pending_zero = needs_pending_zero(target, mime);
    let payload = client
        .upload(target, bytes, &filename, mime, &upload_id, pending_zero)
        .await?;
    let reference = payload
        .get("ref")
        .or_else(|| payload.get("blob_ref"))
        .and_then(Value::as_str)
        .map(str::to_string);

    // Second step, only when the app would do it: a workspace upload whose ref
    // is not UUID-shaped gets an explicit pending registration.
    let needs_registration = payload
        .get("pending_registration_needed")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let registration = match (needs_registration, reference.as_deref()) {
        (true, Some(reference)) => Some(client.register_pending(&upload_id, reference).await?),
        _ => None,
    };
    Ok(json!({
        "target": target,
        "upload_id": upload_id,
        "filename": filename,
        "mime": mime,
        "bytes": file.metadata().map(|meta| meta.len()).unwrap_or(0),
        "ref": reference,
        "pending_zero_sent": pending_zero,
        "pending_registered": registration.is_some(),
        "registration": registration,
        "status": payload.get("status").cloned().unwrap_or(Value::Null),
        "response": payload,
    })
    .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_zero_is_derived_like_the_app() {
        // A workspace image stays pending so the post resolves it.
        assert!(!needs_pending_zero("attachments", "image/png"));
        assert!(!needs_pending_zero("attachments", "application/pdf"));
        // A workspace video is finalised on upload.
        assert!(needs_pending_zero("attachments", "video/mp4"));
        assert!(needs_pending_zero("attachments", "video/quicktime"));
        // Blobs and issue reports are never pending.
        assert!(needs_pending_zero("blobs", "image/png"));
        assert!(needs_pending_zero("issue_reports", "image/png"));
    }

    #[test]
    fn mime_guessing_covers_the_common_types() {
        assert_eq!(guess_mime("a.PNG"), "image/png");
        assert_eq!(guess_mime("a.jpg"), "image/jpeg");
        assert_eq!(guess_mime("a.mp4"), "video/mp4");
        assert_eq!(guess_mime("a.pdf"), "application/pdf");
        assert_eq!(guess_mime("a.unknownext"), "application/octet-stream");
    }
}
