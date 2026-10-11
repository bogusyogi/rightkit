//! Refresh-and-reanchor orchestration.
//!
//! One online refresh does three things: ask the server whether the license is still
//! active, verify any freshly minted token locally, and re-anchor the anti-rollback
//! clock on the server's signed issue time. This module is transport-agnostic: the host
//! supplies a [`RefreshTransport`] (its HTTP client) and a clock
//! ([`AntiRollbackClock`]), and gets a typed [`RefreshOutcome`] back.
//!
//! ## Outcomes
//! | Server / transport result | [`RefreshOutcome`] | Clock |
//! |---|---|---|
//! | transport failure (offline, 5xx, malformed body) | `Stale` | untouched |
//! | `status != "active"` | `Revoked` | untouched |
//! | active + new verified token | `TokenReplaced` | re-anchored and persisted |
//! | active + token omitted or identical, cached token still valid | `TokenKept` | untouched |
//! | active + token omitted or identical, cached token absent or lapsed | `TokenDropped` | untouched |
//! | active + token unusable (bad signature, replayed anchor, not active, ...) | `Rejected` | untouched |
//!
//! ## Pure judgement
//! [`judge_refresh_response`] makes the response-level decision with no I/O (status,
//! authentication, lifecycle at the token's own `iat`, requirement, ledger replay/missing via
//! [`crate::plan_reanchor`]); [`refresh_and_reanchor`] is that judgement plus the transport
//! call, the ledger write and the cached-token verdict, so the two cannot drift.
//!
//! ## Ordering contract
//! The clock is persisted **before** this function returns `TokenReplaced`; the caller
//! must then persist the new license record. If the second write is interrupted, the
//! state can only fail closed (fresh anchor, old token). Never write the license first.
//!
//! ## What the caller does with each outcome
//! * `Stale`: keep the cached record unchanged; store `stale_since` on it (it is the
//!   first failure of this episode, carried in via [`RefreshParams::stale_since`]) and
//!   clear it on the next non-stale outcome.
//! * `Revoked`: the server is authoritative. Treat as [`crate::Lifecycle::Revoked`];
//!   delete the signed token.
//! * `TokenReplaced`: persist `token` and the app-specific fields in `response.extra`.
//! * `TokenKept`: leave the record as is.
//! * `TokenDropped`: the server says active but the device holds no usable token; drop the
//!   cached token and ask the user to re-activate online.
//! * `Rejected`: keep the cached record unchanged and surface the reason; do not mark stale.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::clock::{
    plan_reanchor, AnchorRefusal, AntiRollbackClock, ClockError, ClockState, ClockStore,
    ReanchorPlan, Reanchored,
};
use crate::lifecycle::{
    evaluate_lifecycle, Lifecycle, LifecycleContext, LifecyclePolicy, StoredCredential,
};
use crate::{authenticate_token, Entitlement, LicenseRequirement, VerifyError};

/// The body of a refresh call (JSON field names match the RightApps backend).
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshRequest<'a> {
    pub app: &'a str,
    pub activation_token: &'a str,
    pub device_id: &'a str,
}

/// A decoded refresh response. Only the fields the orchestration needs are typed;
/// everything else the server sent (entitlements, `validUntil`, lifecycle hints, ...)
/// stays in [`RefreshResponse::extra`] for the app to persist as it already does.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshResponse {
    pub status: String,
    #[serde(default)]
    pub signed_token: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl RefreshResponse {
    pub fn from_json(body: &str) -> Result<Self, TransportError> {
        serde_json::from_str(body)
            .map_err(|error| TransportError::new(format!("refresh response is malformed: {error}")))
    }
}

/// The server could not be reached or its answer is not a usable refresh response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportError(pub String);

impl TransportError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for TransportError {}

/// The host's HTTP client for the refresh endpoint.
///
/// Contract: return `Ok` for any response that carries an authoritative answer, including
/// an HTTP 4xx whose JSON body says `ok:false` with a non-`"active"` status (map it to a
/// `RefreshResponse` with that status). Return `Err` for everything else: no network,
/// timeouts, 5xx, proxy HTML, unparsable bodies. Those become `Stale`, never `Revoked`.
pub trait RefreshTransport {
    fn refresh(&self, request: &RefreshRequest<'_>) -> Result<RefreshResponse, TransportError>;
}

/// Inputs of one refresh.
#[derive(Debug, Clone, Copy)]
pub struct RefreshParams<'a> {
    pub pubkey: &'a [u8; 32],
    pub app: &'a str,
    pub device_id: &'a str,
    pub activation_token: &'a str,
    /// The token currently stored, if any.
    pub cached_token: Option<&'a str>,
    /// The record's "anti-rollback clock initialised" flag.
    pub clock_initialized: bool,
    /// `stale_since` already stored on the record, if the previous refresh was stale.
    pub stale_since: Option<i64>,
    /// Current wall clock. Used as the `stale_since` stamp and to seed a brand-new ledger.
    pub wall_now: i64,
    pub policy: LifecyclePolicy,
    /// If set, a fresh or cached token must satisfy it (e.g. `LicenseRequirement::pro()`).
    pub requirement: Option<&'a LicenseRequirement>,
}

impl<'a> RefreshParams<'a> {
    pub fn new(
        pubkey: &'a [u8; 32],
        app: &'a str,
        device_id: &'a str,
        activation_token: &'a str,
        wall_now: i64,
    ) -> Self {
        Self {
            pubkey,
            app,
            device_id,
            activation_token,
            cached_token: None,
            clock_initialized: false,
            stale_since: None,
            wall_now,
            policy: LifecyclePolicy::default(),
            requirement: None,
        }
    }
}

/// Why the server's "active" answer was nevertheless not accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshRejection {
    /// The new token failed authentication (signature, app, device).
    InvalidToken(VerifyError),
    /// The new token authenticates but does not grant access at its own issue time
    /// (expired on arrival, inconsistent lifecycle claims, ...).
    NotActive(Lifecycle),
    /// The new token does not satisfy the required tier/feature.
    RequirementNotMet,
    /// The new token's `iat` is not newer than the last anchor (replay).
    ReplayedServerTime { last_server_iat: i64, got: i64 },
}

/// Why the cached token was kept when no new token arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKeptReason {
    /// The server said active and sent no token.
    ServerOmittedToken,
    /// The server sent back the token we already hold.
    ServerReturnedSameToken,
}

/// Why no usable token remains when no new token arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenDropReason {
    /// There was no cached token to keep.
    NoCachedToken,
    /// The cached token is expired, rolled-back-clock, inconsistent or otherwise not
    /// valid now; the carried lifecycle says which.
    CachedTokenLapsed(Lifecycle),
}

/// Typed result of [`refresh_and_reanchor`]. See the module docs for the caller's duties.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum RefreshOutcome {
    TokenReplaced {
        token: String,
        entitlement: Box<Entitlement>,
        lifecycle: Lifecycle,
        reanchored: Reanchored,
        response: RefreshResponse,
    },
    TokenKept {
        reason: TokenKeptReason,
        lifecycle: Lifecycle,
        /// `exp` of the kept token: the instant it stops being valid.
        valid_until: i64,
        response: RefreshResponse,
    },
    TokenDropped {
        reason: TokenDropReason,
        response: RefreshResponse,
    },
    Stale {
        /// First failure of this episode (`params.stale_since` if set, else `wall_now`).
        stale_since: i64,
        error: String,
    },
    Revoked {
        message: Option<String>,
    },
    Rejected(RefreshRejection),
}

impl RefreshOutcome {
    /// The JS-facing name (`tokenReplaced`, `tokenKept`, `tokenDropped`, `stale`,
    /// `revoked`, `rejected`).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::TokenReplaced { .. } => "tokenReplaced",
            Self::TokenKept { .. } => "tokenKept",
            Self::TokenDropped { .. } => "tokenDropped",
            Self::Stale { .. } => "stale",
            Self::Revoked { .. } => "revoked",
            Self::Rejected(_) => "rejected",
        }
    }
}

/// The claims of a fresh token that was authentic but not accepted, for messages and
/// diagnostics (for example "this token expired at ..., issued at ...").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct RejectedToken {
    /// The token's `iat`.
    pub iat: i64,
    /// The token's `exp`.
    pub exp: i64,
    /// How the token classifies at its own issue time.
    pub lifecycle: Lifecycle,
}

/// The pure verdict on a refresh response; see [`judge_refresh_response`].
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum RefreshJudgement {
    /// `status != "active"`: the server is authoritative.
    Revoked { message: Option<String> },
    /// The server said active but its token cannot be used. `token` carries the claims
    /// when the token authenticated (so `NotActive` and `RequirementNotMet` and
    /// `ReplayedServerTime` have exp/iat; `InvalidToken` has none).
    Rejected {
        rejection: RefreshRejection,
        token: Option<RejectedToken>,
    },
    /// The token is acceptable and the ledger should move to `anchor`
    /// ([`ReanchorPlan::into_state`] gives the record). Writing it is the caller's job
    /// (or [`refresh_and_reanchor`]'s), and is re-checked against the ledger at write time.
    ReplaceToken {
        token: String,
        entitlement: Box<Entitlement>,
        lifecycle: Lifecycle,
        anchor: ReanchorPlan,
    },
    /// Active, and no different token arrived. Whether the cached token still grants
    /// access depends on the clock (a stateful judgement), so it is decided by
    /// [`evaluate_lifecycle`], not here.
    NoNewToken { reason: TokenKeptReason },
    /// A new token would be accepted but the ledger of an initialised install is missing:
    /// fail closed ([`ClockError::Missing`]).
    LedgerMissing,
}

/// The fresh token worth judging: present, non-blank, and different from the cached one.
fn replacement_token<'r>(
    params: &RefreshParams<'_>,
    response: &'r RefreshResponse,
) -> Option<&'r str> {
    response
        .signed_token
        .as_deref()
        .filter(|token| !token.trim().is_empty())
        .filter(|token| params.cached_token != Some(*token))
}

/// Judge a refresh response with **no I/O**: the same decision [`refresh_and_reanchor`]
/// makes, which calls this function internally so the two cannot drift.
///
/// `prior` is the ledger as currently stored (`ClockStore::load`), used only for the
/// replay check and the missing-ledger check on a replacement token; pass `None` when
/// there is none. The order is fixed: status, then (for a different fresh token)
/// authentication, lifecycle at the token's own `iat`, requirement, and finally the
/// ledger (missing, then replay via [`plan_reanchor`]).
///
/// What it does not decide: the transport result (`Stale` is decided before a response
/// exists) and the keep-or-drop verdict on the cached token, which needs the live clock
/// reading. Those are [`RefreshJudgement::NoNewToken`] here and are completed by
/// [`refresh_and_reanchor`] with `evaluate_lifecycle`.
pub fn judge_refresh_response(
    params: &RefreshParams<'_>,
    response: &RefreshResponse,
    prior: Option<&ClockState>,
) -> RefreshJudgement {
    if response.status != "active" {
        return RefreshJudgement::Revoked {
            message: response.message.clone(),
        };
    }
    let Some(token) = replacement_token(params, response) else {
        let has_token = response
            .signed_token
            .as_deref()
            .is_some_and(|token| !token.trim().is_empty());
        return RefreshJudgement::NoNewToken {
            reason: if has_token {
                TokenKeptReason::ServerReturnedSameToken
            } else {
                TokenKeptReason::ServerOmittedToken
            },
        };
    };

    let entitlement = match authenticate_token(token, params.pubkey, params.app, params.device_id) {
        Ok(entitlement) => entitlement,
        Err(error) => {
            return RefreshJudgement::Rejected {
                rejection: RefreshRejection::InvalidToken(error),
                token: None,
            }
        }
    };
    // Judge the fresh token at its own issue time: it is the server's statement of "now".
    let lifecycle = entitlement.lifecycle(entitlement.iat, &params.policy);
    let claims = RejectedToken {
        iat: entitlement.iat,
        exp: entitlement.exp,
        lifecycle,
    };
    if !lifecycle.grants_access() {
        return RefreshJudgement::Rejected {
            rejection: RefreshRejection::NotActive(lifecycle),
            token: Some(claims),
        };
    }
    if let Some(requirement) = params.requirement {
        if !entitlement.satisfies(requirement) {
            return RefreshJudgement::Rejected {
                rejection: RefreshRejection::RequirementNotMet,
                token: Some(claims),
            };
        }
    }
    match plan_reanchor(
        prior,
        entitlement.iat,
        params.wall_now,
        params.clock_initialized,
    ) {
        Ok(anchor) => RefreshJudgement::ReplaceToken {
            token: token.to_string(),
            entitlement: Box::new(entitlement),
            lifecycle,
            anchor,
        },
        Err(AnchorRefusal::Missing) => RefreshJudgement::LedgerMissing,
        Err(AnchorRefusal::Replayed {
            last_server_iat,
            got,
        }) => RefreshJudgement::Rejected {
            rejection: RefreshRejection::ReplayedServerTime {
                last_server_iat,
                got,
            },
            token: Some(claims),
        },
    }
}

/// Refresh the license from the server and re-anchor the clock on the server time.
///
/// `Err` is reserved for the local clock store failing or the ledger of an initialised
/// install being missing ([`ClockError::Missing`], checked *before* any network call);
/// every server-side situation is an `Ok(RefreshOutcome)`.
///
/// The decision is [`judge_refresh_response`]; this function adds the transport call, the
/// ledger load/write and the cached-token verdict. A `Rejected(NotActive(..))` outcome
/// carries only the lifecycle; the token's `exp`/`iat` for a message are in
/// [`RefreshJudgement::Rejected`], obtained by judging the same response again (it is pure).
pub fn refresh_and_reanchor<S, T>(
    params: &RefreshParams<'_>,
    transport: &T,
    clock: &AntiRollbackClock<S>,
) -> Result<RefreshOutcome, ClockError<S::Error>>
where
    S: ClockStore,
    T: RefreshTransport,
{
    if params.clock_initialized && clock.load()?.is_none() {
        return Err(ClockError::Missing);
    }

    let request = RefreshRequest {
        app: params.app,
        activation_token: params.activation_token,
        device_id: params.device_id,
    };
    let response = match transport.refresh(&request) {
        Ok(response) => response,
        Err(error) => {
            return Ok(RefreshOutcome::Stale {
                stale_since: params.stale_since.unwrap_or(params.wall_now),
                error: error.to_string(),
            })
        }
    };

    // The ledger is read only when a different fresh token is in play.
    let prior = if response.status == "active" && replacement_token(params, &response).is_some() {
        clock.load()?
    } else {
        None
    };
    match judge_refresh_response(params, &response, prior.as_ref()) {
        RefreshJudgement::Revoked { message } => Ok(RefreshOutcome::Revoked { message }),
        RefreshJudgement::Rejected { rejection, .. } => Ok(RefreshOutcome::Rejected(rejection)),
        RefreshJudgement::LedgerMissing => Err(ClockError::Missing),
        RefreshJudgement::NoNewToken { reason } => keep_or_drop(params, clock, reason, response),
        RefreshJudgement::ReplaceToken {
            token,
            entitlement,
            lifecycle,
            ..
        } => {
            // The write re-plans against the ledger as it is at write time.
            let reanchored =
                match clock.reanchor(entitlement.iat, params.wall_now, params.clock_initialized) {
                    Ok(reanchored) => reanchored,
                    Err(ClockError::ReplayedAnchor {
                        last_server_iat,
                        got,
                    }) => {
                        return Ok(RefreshOutcome::Rejected(
                            RefreshRejection::ReplayedServerTime {
                                last_server_iat,
                                got,
                            },
                        ))
                    }
                    Err(other) => return Err(other),
                };
            Ok(RefreshOutcome::TokenReplaced {
                token,
                entitlement,
                lifecycle,
                reanchored,
                response,
            })
        }
    }
}

fn keep_or_drop<S: ClockStore>(
    params: &RefreshParams<'_>,
    clock: &AntiRollbackClock<S>,
    kept_reason: TokenKeptReason,
    response: RefreshResponse,
) -> Result<RefreshOutcome, ClockError<S::Error>> {
    let Some(cached) = params.cached_token else {
        return Ok(RefreshOutcome::TokenDropped {
            reason: TokenDropReason::NoCachedToken,
            response,
        });
    };
    let mut ctx = LifecycleContext::new(params.pubkey, params.app, params.device_id)
        .with_policy(params.policy);
    if let Some(requirement) = params.requirement {
        ctx = ctx.with_requirement(requirement);
    }
    let report = evaluate_lifecycle(
        &ctx,
        clock,
        StoredCredential::Signed {
            token: cached,
            clock_initialized: params.clock_initialized,
        },
        params.wall_now,
    )?;
    if report.grants_access() {
        let valid_until = report.expires_at.unwrap_or_default();
        Ok(RefreshOutcome::TokenKept {
            reason: kept_reason,
            lifecycle: report.lifecycle,
            valid_until,
            response,
        })
    } else {
        Ok(RefreshOutcome::TokenDropped {
            reason: TokenDropReason::CachedTokenLapsed(report.lifecycle),
            response,
        })
    }
}
