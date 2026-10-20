//! The fixed-schema values the native client hands to a renderer, and its typed error.
//!
//! Everything here is a closed enum or a plain number/string that cannot carry a credential:
//! no token-bearing type is `Serialize`, and no field name matches `token`, `key` or `secret`.
//! Times are `i64` Unix milliseconds (converted from the crate's seconds). The server's own text
//! is never forwarded; it is mapped to a [`MessageCode`], and the JS client holds the fixed
//! English text per code ([`MessageCode::english`] is the same table).

use std::fmt;

use serde::Serialize;

use crate::{ClockRefusal, Lifecycle};

/// Whether this build has licensing at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LicenseMode {
    /// No licence product, or no provisioned key.
    Disabled,
    /// A real public key and host are compiled in.
    Configured,
}

/// Where the licence stands, coarsely, for the UI. The authority is [`Lifecycle`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ViewStatus {
    None,
    Active,
    Grace,
    Expired,
    Revoked,
    Invalid,
    /// The state cannot be established now (unresolved write, clock refusal, record unreadable).
    Unverifiable,
}

/// `licenseKind` of the verified token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewKind {
    Trial,
    Lifetime,
    Term,
}

/// `entitlementStage` of the verified token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewStage {
    TimeBound,
    Provisional,
    Final,
}

/// What kind of envelope the stored record is in. Never the revision value itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RevisionState {
    Active,
    Tombstone,
    None,
}

/// Why the server declined, mapped natively from its non-active `status` (or HTTP 429). The
/// server's message is never forwarded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RejectReason {
    /// `invalid` or `not_found`.
    KeyNotAccepted,
    Revoked,
    Expired,
    /// `device_limit` or `limit_reached` (the exact server names are unverified).
    DeviceLimit,
    /// `rate_limited` or HTTP 429.
    RateLimited,
    Other,
}

impl RejectReason {
    /// Map the server's non-active `status` string and the HTTP status. Anything unmatched is
    /// [`RejectReason::Other`].
    pub fn from_server(status: Option<&str>, http_status: u16) -> Self {
        if http_status == 429 {
            return Self::RateLimited;
        }
        match status.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
            Some("invalid" | "not_found") => Self::KeyNotAccepted,
            Some("revoked" | "removed") => Self::Revoked,
            Some("expired") => Self::Expired,
            Some("device_limit" | "limit_reached") => Self::DeviceLimit,
            Some("rate_limited") => Self::RateLimited,
            _ => Self::Other,
        }
    }

    /// The default message for this reason.
    pub fn message(self) -> MessageCode {
        match self {
            Self::KeyNotAccepted => MessageCode::KeyNotAccepted,
            Self::Revoked => MessageCode::Revoked,
            Self::Expired => MessageCode::Expired,
            Self::DeviceLimit => MessageCode::DeviceLimit,
            Self::RateLimited => MessageCode::RateLimited,
            Self::Other => MessageCode::RejectedOther,
        }
    }
}

/// A fixed message. The JS client maps each code to fixed English text; an unknown code shows
/// the `unknown` text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MessageCode {
    None,
    Unconfigured,
    Busy,
    Unresolved,
    NetworkUnavailable,
    KeyNotAccepted,
    Revoked,
    Expired,
    DeviceLimit,
    RateLimited,
    RejectedOther,
    StoreFailed,
    ClockRollback,
    ClockUnavailable,
    VerifyFailed,
    InvalidToken,
    IssuedInTheFuture,
    InvalidLifecycle,
    MigrationRequired,
    ClockRepairRequired,
    Forbidden,
    Unsupported,
    TrialRequested,
    TrialRefused,
    UpdateRefused,
    UpdateFailed,
    Unknown,
}

impl MessageCode {
    /// The fixed English text of this code.
    pub fn english(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Unconfigured => "Licensing is not available in this build.",
            Self::Busy => "Another licence operation is in progress. Try again in a moment.",
            Self::Unresolved => "The last licence change did not finish. Try again to complete it.",
            Self::NetworkUnavailable => {
                "Could not reach the licence server. Check your connection and try again."
            }
            Self::KeyNotAccepted => "That licence key was not accepted.",
            Self::Revoked => "This licence has been revoked.",
            Self::Expired => "This licence has expired.",
            Self::DeviceLimit => {
                "This licence is already active on the maximum number of devices."
            }
            Self::RateLimited => "Too many attempts. Wait a few minutes and try again.",
            Self::RejectedOther => "The licence server declined the request.",
            Self::StoreFailed => "The licence could not be saved on this device.",
            Self::ClockRollback => {
                "Your system clock appears to be set earlier than before. Correct it and try again."
            }
            Self::ClockUnavailable => "The system clock could not be read.",
            Self::VerifyFailed => "The licence could not be verified.",
            Self::InvalidToken => "The stored licence could not be verified.",
            Self::IssuedInTheFuture => {
                "The licence was issued later than your system clock allows. Check the date and time."
            }
            Self::InvalidLifecycle => "The licence data is inconsistent.",
            Self::MigrationRequired => "Your licence needs a one-time online refresh.",
            Self::ClockRepairRequired => {
                "Your system clock needs correcting before the licence can be used."
            }
            Self::Forbidden => "This window cannot manage the licence.",
            Self::Unsupported => "This feature is not available in this app.",
            Self::TrialRequested => "Check your email to start the trial.",
            Self::TrialRefused => "A trial could not be started for this email.",
            Self::UpdateRefused => "This update cannot be installed with the current licence.",
            Self::UpdateFailed => "The update could not be downloaded or installed.",
            Self::Unknown => "Something went wrong.",
        }
    }
}

/// The licence as the renderer may see it. No token, key or record content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LicenseView {
    pub mode: LicenseMode,
    pub status: ViewStatus,
    /// The existing [`Lifecycle`] enum, snake_case.
    pub lifecycle: Lifecycle,
    pub tier: Option<String>,
    pub features: Vec<String>,
    pub kind: Option<ViewKind>,
    pub stage: Option<ViewStage>,
    /// Expiry of the verified token, Unix ms.
    pub valid_until: Option<i64>,
    /// Unix ms.
    pub confirmation_due_at: Option<i64>,
    /// First failed refresh of the current offline episode, Unix ms.
    pub stale_since: Option<i64>,
    /// Last four characters of the licence id.
    pub license_id_suffix: Option<String>,
    /// Display only: the lock was held, so this is the last committed view. Authorization never
    /// uses a view with this set.
    pub busy: bool,
    pub revision_state: RevisionState,
    /// A write did not finish or a rotation marker exists: every entitlement or update use
    /// refuses until it is resolved.
    pub unresolved: bool,
    pub message: MessageCode,
}

impl LicenseView {
    /// The view of a build with licensing disabled.
    pub fn disabled() -> Self {
        Self {
            mode: LicenseMode::Disabled,
            ..Self::none(LicenseMode::Disabled)
        }
    }

    /// No licence at all.
    pub(crate) fn none(mode: LicenseMode) -> Self {
        Self {
            mode,
            status: ViewStatus::None,
            lifecycle: Lifecycle::Unlicensed,
            tier: None,
            features: Vec::new(),
            kind: None,
            stage: None,
            valid_until: None,
            confirmation_due_at: None,
            stale_since: None,
            license_id_suffix: None,
            busy: false,
            revision_state: RevisionState::None,
            unresolved: false,
            message: MessageCode::None,
        }
    }

    /// The state cannot be established: shown instead of a guess.
    pub(crate) fn unverifiable(message: MessageCode, unresolved: bool) -> Self {
        Self {
            status: ViewStatus::Unverifiable,
            unresolved,
            message,
            ..Self::none(LicenseMode::Configured)
        }
    }

    /// This view, marked as the display-only last committed one.
    pub(crate) fn with_busy(mut self) -> Self {
        self.busy = true;
        self.message = MessageCode::Busy;
        self
    }
}

/// What a refresh did, in the closed vocabulary of the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RefreshViewOutcome {
    /// Nothing to refresh with; nothing was sent.
    NoCredential,
    /// The server confirmed the licence (a new token, or the held one kept).
    Refreshed,
    /// The server could not be reached, or its answer was unusable; the record stands.
    Stale,
    /// The server said the licence is no longer active.
    Revoked,
    /// A new token moved the clock BACKWARDS to server time (the clock-repair path).
    Reanchored,
    /// The clock refused the new token (rollback, replay, missing ledger).
    ClockRefused,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshView {
    pub view: LicenseView,
    pub outcome: RefreshViewOutcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrialView {
    pub accepted: bool,
    pub message: MessageCode,
}

/// `CommandError.code` on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CommandCode {
    Unconfigured,
    Busy,
    Unresolved,
    Network,
    Rejected,
    Store,
    Clock,
    Verify,
    Forbidden,
    Unsupported,
}

/// The serialized object an `invoke` rejects with (never a string).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandError {
    pub code: CommandCode,
    /// Only when `code` is `rejected`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<RejectReason>,
    pub message: MessageCode,
}

/// Why a licence operation did not complete. `Display` is fixed text: no raw server body, no
/// echoed input, no path. Nothing here carries a credential.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum LicenseError {
    /// Disabled mode, or no usable configuration. No network was used.
    Unconfigured,
    /// The licence operation lock (or the credential fence) is held; try again.
    Busy,
    /// An earlier write did not finish. Authorization refuses until it is resolved.
    Unresolved,
    /// The server could not be reached, or answered with something unusable.
    Network,
    /// The server declined.
    Rejected { reason: RejectReason },
    /// The record could not be read or saved. The previous verified record stands.
    Store,
    /// The anti-rollback clock refused.
    Clock(ClockRefusal),
    /// A token, envelope or record failed verification and was not stored.
    Verify,
    /// The caller may not manage the licence (used by the shell adapter's caller gate).
    Forbidden,
    /// The feature is not available in this app (used by the shell adapter).
    Unsupported,
}

impl LicenseError {
    /// The wire code.
    pub fn code(&self) -> CommandCode {
        match self {
            Self::Unconfigured => CommandCode::Unconfigured,
            Self::Busy => CommandCode::Busy,
            Self::Unresolved => CommandCode::Unresolved,
            Self::Network => CommandCode::Network,
            Self::Rejected { .. } => CommandCode::Rejected,
            Self::Store => CommandCode::Store,
            Self::Clock(_) => CommandCode::Clock,
            Self::Verify => CommandCode::Verify,
            Self::Forbidden => CommandCode::Forbidden,
            Self::Unsupported => CommandCode::Unsupported,
        }
    }

    /// The default message for this error.
    pub fn message_code(&self) -> MessageCode {
        match self {
            Self::Unconfigured => MessageCode::Unconfigured,
            Self::Busy => MessageCode::Busy,
            Self::Unresolved => MessageCode::Unresolved,
            Self::Network => MessageCode::NetworkUnavailable,
            Self::Rejected { reason } => reason.message(),
            Self::Store => MessageCode::StoreFailed,
            Self::Clock(ClockRefusal::Rollback { .. } | ClockRefusal::Missing) => {
                MessageCode::ClockRollback
            }
            Self::Clock(_) => MessageCode::ClockUnavailable,
            Self::Verify => MessageCode::VerifyFailed,
            Self::Forbidden => MessageCode::Forbidden,
            Self::Unsupported => MessageCode::Unsupported,
        }
    }

    /// The reason, only for a rejection.
    pub fn reason(&self) -> Option<RejectReason> {
        match self {
            Self::Rejected { reason } => Some(*reason),
            _ => None,
        }
    }

    /// The object a command rejects with.
    pub fn to_command_error(&self) -> CommandError {
        CommandError {
            code: self.code(),
            reason: self.reason(),
            message: self.message_code(),
        }
    }
}

impl fmt::Display for LicenseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message_code().english())
    }
}

impl std::error::Error for LicenseError {}

impl From<&LicenseError> for CommandError {
    fn from(error: &LicenseError) -> Self {
        error.to_command_error()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    /// Every key of `value`, at every depth.
    fn keys(value: &Value, out: &mut Vec<String>) {
        match value {
            Value::Object(map) => {
                for (key, inner) in map {
                    out.push(key.clone());
                    keys(inner, out);
                }
            }
            Value::Array(items) => items.iter().for_each(|item| keys(item, out)),
            _ => {}
        }
    }

    fn sentinel_view() -> LicenseView {
        LicenseView {
            mode: LicenseMode::Configured,
            status: ViewStatus::Active,
            lifecycle: Lifecycle::Active,
            tier: Some("pro".into()),
            features: vec!["wake_word".into()],
            kind: Some(ViewKind::Term),
            stage: Some(ViewStage::TimeBound),
            valid_until: Some(1_792_108_800_000),
            confirmation_due_at: Some(1),
            stale_since: Some(2),
            license_id_suffix: Some("abcd".into()),
            busy: true,
            revision_state: RevisionState::Active,
            unresolved: true,
            message: MessageCode::Busy,
        }
    }

    #[test]
    fn no_view_key_matches_token_key_or_secret() {
        let view = sentinel_view();
        let values = [
            serde_json::to_value(&view).unwrap(),
            serde_json::to_value(RefreshView {
                view: view.clone(),
                outcome: RefreshViewOutcome::Reanchored,
            })
            .unwrap(),
            serde_json::to_value(TrialView {
                accepted: true,
                message: MessageCode::TrialRequested,
            })
            .unwrap(),
            serde_json::to_value(LicenseView::disabled()).unwrap(),
            serde_json::to_value(
                LicenseError::Rejected {
                    reason: RejectReason::DeviceLimit,
                }
                .to_command_error(),
            )
            .unwrap(),
        ];
        let mut all = Vec::new();
        for value in &values {
            keys(value, &mut all);
        }
        assert!(all.len() > 20, "the walk found the keys: {all:?}");
        for key in &all {
            let lower = key.to_ascii_lowercase();
            assert!(
                !lower.contains("token") && !lower.contains("key") && !lower.contains("secret"),
                "key `{key}` names a credential"
            );
        }
    }

    #[test]
    fn serialization_never_contains_a_sentinel_credential() {
        // The views have no field a credential could flow into; a sentinel placed in every
        // free-text field a view has (tier, features, suffix) is the only text that appears.
        let mut view = sentinel_view();
        view.tier = Some("TIER_VALUE".into());
        view.features = vec!["FEATURE_VALUE".into()];
        let text = serde_json::to_string(&view).unwrap();
        assert!(text.contains("TIER_VALUE") && text.contains("FEATURE_VALUE"));
        for sentinel in [
            "SENTINEL_ACTIVATION_TOKEN",
            "SENTINEL_SIGNED_TOKEN",
            "SENTINEL_KEY",
        ] {
            assert!(!text.contains(sentinel));
        }
        // Times are numbers (Unix ms), not strings.
        let value = serde_json::to_value(&view).unwrap();
        assert!(value["validUntil"].is_i64());
        assert_eq!(value["lifecycle"], "active");
        assert_eq!(value["revisionState"], "active");
    }

    #[test]
    fn wire_names_are_the_frozen_ones() {
        let text = |value: &dyn erased::Ser| value.json();
        assert_eq!(
            text(&MessageCode::NetworkUnavailable),
            "\"networkUnavailable\""
        );
        assert_eq!(
            text(&MessageCode::IssuedInTheFuture),
            "\"issuedInTheFuture\""
        );
        assert_eq!(text(&MessageCode::None), "\"none\"");
        assert_eq!(text(&RejectReason::KeyNotAccepted), "\"keyNotAccepted\"");
        assert_eq!(text(&RefreshViewOutcome::ClockRefused), "\"clockRefused\"");
        assert_eq!(text(&RefreshViewOutcome::NoCredential), "\"noCredential\"");
        assert_eq!(text(&ViewKind::Lifetime), "\"lifetime\"");
        assert_eq!(text(&ViewStage::TimeBound), "\"time_bound\"");
        assert_eq!(
            text(&Lifecycle::VerificationRequired),
            "\"verification_required\""
        );
        assert_eq!(text(&CommandCode::Unresolved), "\"unresolved\"");
        assert_eq!(text(&LicenseMode::Disabled), "\"disabled\"");
        assert_eq!(text(&RevisionState::Tombstone), "\"tombstone\"");
    }

    mod erased {
        pub trait Ser {
            fn json(&self) -> String;
        }
        impl<T: serde::Serialize> Ser for T {
            fn json(&self) -> String {
                serde_json::to_string(self).unwrap()
            }
        }
    }

    #[test]
    fn command_error_code_maps_to_the_frozen_message_codes() {
        let cases: [(LicenseError, CommandCode, MessageCode); 9] = [
            (
                LicenseError::Unconfigured,
                CommandCode::Unconfigured,
                MessageCode::Unconfigured,
            ),
            (LicenseError::Busy, CommandCode::Busy, MessageCode::Busy),
            (
                LicenseError::Unresolved,
                CommandCode::Unresolved,
                MessageCode::Unresolved,
            ),
            (
                LicenseError::Network,
                CommandCode::Network,
                MessageCode::NetworkUnavailable,
            ),
            (
                LicenseError::Store,
                CommandCode::Store,
                MessageCode::StoreFailed,
            ),
            (
                LicenseError::Verify,
                CommandCode::Verify,
                MessageCode::VerifyFailed,
            ),
            (
                LicenseError::Forbidden,
                CommandCode::Forbidden,
                MessageCode::Forbidden,
            ),
            (
                LicenseError::Unsupported,
                CommandCode::Unsupported,
                MessageCode::Unsupported,
            ),
            (
                LicenseError::Clock(ClockRefusal::Rollback { highest_seen: 1 }),
                CommandCode::Clock,
                MessageCode::ClockRollback,
            ),
        ];
        for (error, code, message) in cases {
            assert_eq!(error.code(), code);
            assert_eq!(error.message_code(), message);
            assert_eq!(error.reason(), None);
            assert_eq!(error.to_string(), message.english());
        }
        let unavailable = LicenseError::Clock(ClockRefusal::Unavailable("x".into()));
        assert_eq!(unavailable.message_code(), MessageCode::ClockUnavailable);
        let rejected = LicenseError::Rejected {
            reason: RejectReason::RateLimited,
        };
        assert_eq!(rejected.code(), CommandCode::Rejected);
        assert_eq!(rejected.message_code(), MessageCode::RateLimited);
        assert_eq!(rejected.reason(), Some(RejectReason::RateLimited));
        let wire = serde_json::to_value(rejected.to_command_error()).unwrap();
        assert_eq!(wire["code"], "rejected");
        assert_eq!(wire["reason"], "rateLimited");
        assert_eq!(wire["message"], "rateLimited");
        // `reason` is absent for every other code.
        let wire = serde_json::to_value(LicenseError::Busy.to_command_error()).unwrap();
        assert!(wire.get("reason").is_none());
    }

    #[test]
    fn reject_reasons_map_from_the_server_status() {
        let cases = [
            (Some("invalid"), 400, RejectReason::KeyNotAccepted),
            (Some("not_found"), 404, RejectReason::KeyNotAccepted),
            (Some("revoked"), 400, RejectReason::Revoked),
            (Some("expired"), 400, RejectReason::Expired),
            (Some("device_limit"), 409, RejectReason::DeviceLimit),
            (Some("limit_reached"), 409, RejectReason::DeviceLimit),
            (Some("rate_limited"), 400, RejectReason::RateLimited),
            (Some("something_new"), 400, RejectReason::Other),
            (None, 400, RejectReason::Other),
            (Some("invalid"), 429, RejectReason::RateLimited),
            (None, 429, RejectReason::RateLimited),
        ];
        for (status, http, want) in cases {
            assert_eq!(
                RejectReason::from_server(status, http),
                want,
                "{status:?} {http}"
            );
        }
    }

    #[test]
    fn every_message_code_has_text_except_none() {
        use MessageCode::*;
        for code in [
            Unconfigured,
            Busy,
            Unresolved,
            NetworkUnavailable,
            KeyNotAccepted,
            Revoked,
            Expired,
            DeviceLimit,
            RateLimited,
            RejectedOther,
            StoreFailed,
            ClockRollback,
            ClockUnavailable,
            VerifyFailed,
            InvalidToken,
            IssuedInTheFuture,
            InvalidLifecycle,
            MigrationRequired,
            ClockRepairRequired,
            Forbidden,
            Unsupported,
            TrialRequested,
            TrialRefused,
            UpdateRefused,
            UpdateFailed,
            Unknown,
        ] {
            assert!(!code.english().is_empty(), "{code:?}");
        }
        assert_eq!(MessageCode::None.english(), "");
    }
}
