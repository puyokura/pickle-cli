//! Generated from the endpoint table by the generator in `tools/`.
//! Do not edit by hand; edit the extract and regenerate.
//!
//! These tables exist so a request can be checked against what the app
//! actually calls: an unknown path or a missing required response key is a
//! contract violation, not a server quirk to work around.

/// Every `api/*` path the app calls, with the methods confirmed from source.
pub const ENDPOINTS: &[(&str, &[&str])] = &[
    ("api/account", &["DELETE"]),
    ("api/account/market", &["PUT"]),
    ("api/account/register", &["POST"]),
    ("api/account/reset", &["POST"]),
    ("api/actions", &["POST"]),
    ("api/actions/{id}", &["GET"]),
    ("api/activity/seen", &["PUT"]),
    ("api/activity/unread", &["GET"]),
    ("api/app-references", &["GET"]),
    ("api/attachments", &["POST"]),
    ("api/attachments/pending", &["POST"]),
    ("api/attachments/pending/{id}", &["DELETE"]),
    ("api/bgm/audio", &["GET"]),
    ("api/blobs", &["POST"]),
    ("api/bubbles", &["GET"]),
    ("api/bubbles/{id}", &[]),
    ("api/call", &["DELETE", "POST"]),
    ("api/call-entry-sounds/{id}/{id}", &["GET"]),
    ("api/character", &["GET", "POST"]),
    ("api/character/avatar/delete", &["POST"]),
    ("api/character/avatar/history", &["GET"]),
    ("api/character/avatar/select", &["POST"]),
    ("api/character/body", &["POST"]),
    ("api/character/colors", &["GET"]),
    ("api/character/jaum-portrait", &["POST"]),
    ("api/character/settings", &["GET", "PUT"]),
    ("api/character/sharing/connections/{id}", &["PUT"]),
    ("api/character/voice-samples", &["GET"]),
    ("api/chat-requests", &["POST"]),
    ("api/chat-requests/{id}/accept", &["POST"]),
    ("api/chat-requests/{id}/decline", &["POST"]),
    ("api/chat/requests/{id}/answer", &["POST"]),
    ("api/chat/requests/{id}/live-view", &["GET"]),
    ("api/clone-briefs/exclusions", &[]),
    ("api/clone-briefs/people/{id}", &[]),
    ("api/connections/{id}", &["DELETE"]),
    ("api/contacts/match", &["POST"]),
    ("api/device/location", &["POST"]),
    ("api/device/location/settings", &["PUT"]),
    ("api/device/location/token", &["POST"]),
    ("api/device/permissions", &["PUT"]),
    ("api/device/results", &["POST"]),
    ("api/extra-conversations/{id}", &["DELETE", "PUT"]),
    ("api/following", &["GET"]),
    ("api/friends", &["GET"]),
    ("api/friends/blocked", &["GET", "POST"]),
    ("api/friends/blocked/{id}", &["DELETE"]),
    ("api/friends/requests", &["GET", "POST"]),
    ("api/friends/requests/{id}", &["DELETE"]),
    ("api/friends/requests/{id}/accept", &["POST"]),
    ("api/friends/{id}", &["DELETE"]),
    ("api/friends/{id}/settings", &["PUT"]),
    ("api/integrations", &["DELETE"]),
    ("api/integrations/{id}/api-key", &["POST"]),
    ("api/integrations/{id}/oauth/url", &["GET"]),
    ("api/invite", &["GET"]),
    ("api/invite/code", &["POST"]),
    ("api/issue-reports", &["POST"]),
    ("api/issue-reports/attachments", &["POST"]),
    ("api/link-previews", &["GET"]),
    ("api/link-previews/image", &["GET"]),
    ("api/login-sessions", &["POST"]),
    ("api/login-sessions/phone-code", &["POST"]),
    ("api/login-sessions/refresh", &["POST"]),
    ("api/login-sessions/revoke", &["POST"]),
    ("api/notification-settings", &["GET", "PUT"]),
    ("api/offers/impressions", &["POST"]),
    ("api/offers/{id}/like", &["PUT"]),
    ("api/offers/{id}/reject", &["DELETE", "POST"]),
    ("api/onboarding/complete", &["POST"]),
    ("api/onboarding/consents", &["POST"]),
    ("api/onboarding/invite-code", &["POST"]),
    ("api/onboarding/invite-waitlist", &["POST"]),
    ("api/onboarding/region", &["POST"]),
    ("api/onboarding/region-waitlist", &["POST"]),
    ("api/phone-calls", &["GET"]),
    ("api/plan", &["POST"]),
    ("api/posts", &["DELETE"]),
    ("api/posts/drafts", &["GET", "POST"]),
    ("api/posts/drafts/{id}", &["GET"]),
    ("api/posts/impressions", &["POST"]),
    ("api/posts/{id}", &["DELETE", "PATCH"]),
    ("api/posts/{id}/comments", &["POST"]),
    ("api/posts/{id}/comments/{id}/favorite", &["PUT"]),
    ("api/posts/{id}/comments/{id}/report", &["POST"]),
    ("api/posts/{id}/favorite", &["PUT"]),
    ("api/posts/{id}/report", &["POST"]),
    ("api/posts/{id}/share", &["POST"]),
    ("api/posts/{id}/sold", &["PUT"]),
    ("api/posts/{id}/source", &["GET"]),
    ("api/profile", &["GET", "POST"]),
    ("api/profile/avatar", &["DELETE", "POST"]),
    ("api/profile/handle/check", &["GET"]),
    ("api/profile/links", &["GET", "PUT"]),
    ("api/profile/phone/request", &["POST"]),
    ("api/profile/phone/verify", &["POST"]),
    ("api/push-token/{id}", &["DELETE"]),
    ("api/review-submissions", &["POST"]),
    ("api/rooms", &["POST"]),
    ("api/rooms/{id}/history", &["GET"]),
    ("api/rooms/{id}/history/delta", &["GET"]),
    ("api/rooms/{id}/history/page", &["GET"]),
    ("api/rooms/{id}/leave", &["POST"]),
    ("api/rooms/{id}/messages", &["POST"]),
    ("api/rooms/{id}/notifications", &["PUT"]),
    ("api/rooms/{id}/pin", &["PUT"]),
    ("api/rooms/{id}/reactions", &["POST"]),
    ("api/rooms/{id}/read", &["PUT"]),
    ("api/screens/{id}", &["GET"]),
    ("api/session", &["POST"]),
    ("api/session/establishment", &["GET"]),
    ("api/site-icons", &[]),
    ("api/store/purchases", &["POST"]),
    ("api/sync", &["GET"]),
    ("api/tap-prompts", &[]),
    ("api/telemetry", &["POST"]),
    ("api/users/search", &["GET", "POST"]),
    ("api/users/{id}", &["GET"]),
    ("api/users/{id}/follow", &["PUT"]),
    ("api/users/{id}/report", &["POST"]),
    ("api/vault", &["GET"]),
    ("api/vault/address", &["POST"]),
    ("api/vault/login", &["POST"]),
    ("api/vault/payment_card", &["POST"]),
    ("api/vault/phone", &["POST"]),
    ("api/vault/{id}", &["POST"]),
    ("api/widget-bundle", &["GET"]),
    ("api/widgets/messages", &[]),
];

/// Paths recovered from a second-pass decompile, where the first pass lost the body.
pub const ENDPOINTS_FALLBACK_ONLY: &[(&str, &[&str])] = &[("api/events", &["GET"])];

/// Response keys `api/session` must carry, or the app throws
/// `SessionResponse$DecodeError`.
pub const SESSION_REQUIRED: &[&str] = &[
    "entry",
    "livekit_url",
    "participant_identity",
    "room",
    "session_id",
    "user_id",
];

/// Optional string keys whose default is the empty string.
pub const SESSION_DEFAULT_EMPTY: &[&str] = &[
    "android_min_build",
    "bio",
    "character_avatar_url",
    "character_jaum_color",
    "character_jaum_glyph",
    "character_jaum_material",
    "character_map_visibility",
    "character_name",
    "character_solid_color",
    "character_voice",
    "first_diary_path",
    "first_meeting_greeting",
    "plan",
];

/// Optional string keys whose default is `null`.
pub const SESSION_DEFAULT_NULL: &[&str] = &[
    "consumable_product_ids",
    "first_meeting_greeting_lines",
    "name_suggestions",
    "seen_flags",
    "store_product_ids",
];

/// The app's own error type names.
pub const ERROR_TYPES: &[&str] = &[
    "ChatMirrorStoreError",
    "SecureBridgeError",
    "AccountApiRequestError",
    "AccountAuthorizationError",
    "AttachmentHttpClient$ClientError",
    "SessionBootstrapError",
    "TransportError",
    "SessionResponse$DecodeError",
    "BgmLibraryStore$DownloadError",
    "LocationHttpClientError",
    "RegionLocatorError",
    "PhotoImportException",
    "VideoImportException",
    "VideoTranscodeException",
    "DefinitionBundleStore$LoadFailure",
    "UiDefinitionCompileError",
    "ExpansionError",
    "ExpressionError",
    "UiVocabularyError",
    "GoogleSignInFailure",
    "IdentitySignInFailure",
    "LoginSessionRequestError",
    "PhoneSignInFailure",
];

/// True when `path` matches one the app calls.
///
/// The app's own paths carry placeholders (`api/rooms/{id}/messages`), so a
/// literal comparison would reject the concrete call. Segments are compared
/// position by position, and any `{...}` template segment matches exactly one
/// concrete segment.
pub fn is_known_path(path: &str) -> bool {
    resolve_template(path).is_some()
}

/// Confirmed methods for the template that `path` matches, or `None` when no
/// template matches.
pub fn known_methods(path: &str) -> Option<&'static [&'static str]> {
    resolve_template(path).map(|(_, methods)| methods)
}

/// The template a concrete path resolves to, with its confirmed methods.
pub fn resolve_template(path: &str) -> Option<(&'static str, &'static [&'static str])> {
    let path = path.trim_start_matches('/').trim_end_matches('/');
    let segments: Vec<&str> = path.split('/').collect();
    for (template, methods) in ENDPOINTS.iter().chain(ENDPOINTS_FALLBACK_ONLY.iter()) {
        let pattern: Vec<&str> = template.split('/').collect();
        if pattern.len() != segments.len() {
            continue;
        }
        let mut matched = true;
        for (want, got) in pattern.iter().zip(segments.iter()) {
            let is_placeholder = want.starts_with('{') && want.ends_with('}');
            if is_placeholder {
                // A placeholder stands for one non-empty segment.
                if got.is_empty() {
                    matched = false;
                    break;
                }
            } else if want != got {
                matched = false;
                break;
            }
        }
        if matched {
            return Some((*template, *methods));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_endpoint_has_a_path() {
        for (path, methods) in ENDPOINTS {
            assert!(path.starts_with("api/"), "{path}");
            let _ = methods;
        }
    }

    #[test]
    fn known_paths_resolve() {
        assert!(is_known_path("api/sync"));
        assert!(is_known_path("/api/sync"));
        assert!(is_known_path("api/events"));
        assert!(!is_known_path("api/definitely-not-real"));
        assert_eq!(known_methods("api/session"), Some(&["POST"][..]));
    }

    #[test]
    fn templates_match_concrete_segments() {
        assert_eq!(
            known_methods("api/rooms/abc123/messages"),
            Some(&["POST"][..])
        );
        assert_eq!(
            known_methods("api/rooms/abc/history/delta"),
            Some(&["GET"][..])
        );
        assert!(is_known_path("api/screens/profile"));
        // Same segment count, wrong literal segment: no match.
        assert!(!is_known_path("api/rooms/abc/definitely"));
        // Extra segment: no match.
        assert!(!is_known_path("api/rooms/abc/messages/extra"));
        // Placeholder must not match an empty segment.
        assert!(!is_known_path("api/rooms//messages"));
    }

    #[test]
    fn session_required_keys_are_present() {
        for key in ["entry", "user_id", "room", "livekit_url", "session_id"] {
            assert!(SESSION_REQUIRED.contains(&key), "missing {key}");
        }
    }
}
