//! Engine-side entitlement gate (feature `engine-gate`).
//!
//! An app engine (CutRight's `cutright-engine`, GenRight's, ...) can be started directly from a
//! command line, an agent plugin or another app. That is by design and stays supported. This
//! module is the check such an engine runs at startup so that an unlicensed engine does not
//! simply work: it resolves the caller's entitlement and returns a typed verdict the engine maps
//! to an exit code and a message (Orthic-Labs/rightkit#182, consumer bogusyogi/CutRight#86).
//!
//! ## What it is not
//! There is no parent-process check, no launch token and no handshake with the shell app, so
//! headless use is never blocked by *how* the engine was started. The licence comes from the
//! caller's Google account (a [`crate::BearerTokenProvider`]) or from the local entitlement cache the
//! shell app persisted ([`crate::SubjectCacheStore`]); the device and account are the caller's. Whether
//! a refusal stops the engine is a release-time choice, [`Enforcement`], set by the build.
//!
//! ## Resolution order ([`EngineGate::check`])
//! 1. [`SubjectEntitlementSource::entitlement`]: cache inside the 24 hour re-check interval, else
//!    the entitlement service, else the cache inside the 7 day offline grace window.
//! 2. If no bearer token can be produced (signed out, headless with no credentials) the service
//!    cannot be asked, so the gate falls back to the local cache alone
//!    ([`SubjectEntitlementSource::cached_decision`]): still inside the offline grace window it
//!    yields [`Verdict::Grace`], otherwise [`DenyReason::SignedOut`].
//!
//! [`EngineGate::check_offline`] does step 2 only and never touches the network or the token
//! provider (for engines that must not make calls at startup).
//!
//! ## Never silent
//! Every verdict has a message ([`GateOutcome::message`]). [`GateOutcome::must_report`] is true
//! for everything except a plain [`Verdict::Allowed`] under [`Enforcement::Enforce`]; a grace
//! admission and a denial that [`Enforcement::Advisory`] let through are both reported, and
//! [`GateOutcome::emit`] writes them. No error is turned into success: every [`DenyReason`] is
//! a refusal and the only way past one is [`Enforcement::Advisory`], which the caller must
//! choose explicitly and which still reports it.
//!
//! ## Trust
//! Same as [`crate::SubjectEntitlementSource`]: the answer is server JSON over TLS and the
//! persisted cache is not tamper-proof. This gate stops the accidental and the casual
//! unlicensed run; it is not offline cryptographic enforcement (use [`crate::EngineEntitlements`]
//! with a signed token where that is required). Messages carry no token, key or response body.
//!
//! ```text
//! let source = SubjectEntitlementSource::new(config, provider).with_store(shared_cache);
//! let gate = EngineGate::new(source, Enforcement::Enforce);
//! let outcome = gate.check();
//! outcome.emit(&mut std::io::stderr())?;
//! if !outcome.permits() {
//!     std::process::exit(outcome.exit_code());
//! }
//! ```

use std::fmt;
use std::io;

use crate::{
    Freshness, SubjectDecision, SubjectEntitlementError, SubjectEntitlementSource, SubjectSource,
};

/// Exit code: the engine may run (`Allowed` or `Grace`, or any verdict under `Advisory`).
pub const EXIT_ALLOWED: i32 = 0;
/// Exit code: the account is not entitled, is expired or was rejected (`EX_NOPERM`).
pub const EXIT_DENIED: i32 = 77;
/// Exit code: the entitlement could not be established and no grace covers it (`EX_UNAVAILABLE`).
pub const EXIT_UNAVAILABLE: i32 = 69;
/// Exit code: the gate is misconfigured (unknown app, bad URL, address policy) (`EX_CONFIG`).
pub const EXIT_CONFIG: i32 = 78;

/// Whether a refusal stops the engine. A release-time switch: the build decides, not the
/// environment (there is deliberately no environment or flag override in this crate).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Enforcement {
    /// A [`Verdict::Denied`] stops the engine ([`GateOutcome::permits`] is `false`).
    Enforce,
    /// A [`Verdict::Denied`] is reported but does not stop the engine. For development and
    /// pre-launch builds only; still never silent.
    Advisory,
}

impl Enforcement {
    /// `true` maps to [`Enforcement::Enforce`], `false` to [`Enforcement::Advisory`]. For a
    /// compile-time or release-profile switch.
    pub fn from_switch(enforce: bool) -> Self {
        if enforce {
            Self::Enforce
        } else {
            Self::Advisory
        }
    }
}

/// Why the engine is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DenyReason {
    /// The service answered and the account does not hold Pro for this app.
    NotEntitled,
    /// The entitlement lapsed (`expires_at` reached, unix seconds). Grace never extends it.
    Expired { expired_at: i64 },
    /// The entitlement service rejected the account token (401 after refresh, or 403).
    Unauthorized,
    /// No bearer token could be produced and the local cache cannot cover the run (none, or
    /// past the offline grace window). Sign in with the app. Carries the provider's message.
    SignedOut(String),
    /// The service could not be reached or answered unusably, and the cache cannot cover the
    /// run (none, or past the offline grace window). Carries the error text.
    Unavailable(String),
    /// The gate cannot ask as configured (unknown app, bad URL, address policy, a rejected
    /// request). Not transient; carries the error text.
    Misconfigured(String),
}

/// The gate's decision before enforcement is applied.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Verdict {
    /// Pro, from a fresh answer or a cache inside the re-check interval.
    Allowed {
        source: SubjectSource,
        /// Unix seconds; `None` means no expiry.
        expires_at: Option<i64>,
    },
    /// Pro on the strength of a cached answer because the service could not be asked
    /// (offline, signed out, 5xx). Ends at `grace_ends_at` (unix seconds, local clock).
    Grace {
        source: SubjectSource,
        expires_at: Option<i64>,
        grace_ends_at: i64,
    },
    /// Refused.
    Denied(DenyReason),
}

impl Verdict {
    /// Map a source result to a verdict. Pure; no clock needed (the decision already folded
    /// `expires_at` into `pro`). Every error is a [`Verdict::Denied`]; none grants.
    pub fn from_result(result: Result<SubjectDecision, SubjectEntitlementError>) -> Self {
        match result {
            Ok(decision) => Self::from_decision(&decision),
            Err(error) => Self::from_error(&error),
        }
    }

    /// Map a decision to a verdict.
    pub fn from_decision(decision: &SubjectDecision) -> Self {
        let e = &decision.entitlement;
        if !decision.pro {
            return Self::Denied(match e.expires_at {
                Some(expired_at) if e.pro => DenyReason::Expired { expired_at },
                _ => DenyReason::NotEntitled,
            });
        }
        match decision.freshness {
            Freshness::OfflineGrace { grace_ends_at } => Self::Grace {
                source: e.source,
                expires_at: e.expires_at,
                grace_ends_at,
            },
            _ => Self::Allowed {
                source: e.source,
                expires_at: e.expires_at,
            },
        }
    }

    /// Map a source error (with no usable cache) to a refusal.
    pub fn from_error(error: &SubjectEntitlementError) -> Self {
        Self::Denied(match error {
            SubjectEntitlementError::Token(e) => DenyReason::SignedOut(e.to_string()),
            SubjectEntitlementError::Unauthorized => DenyReason::Unauthorized,
            SubjectEntitlementError::Config(_) | SubjectEntitlementError::Rejected { .. } => {
                DenyReason::Misconfigured(error.to_string())
            }
            // Offline, RateLimited, Server, BadResponse, and any variant added later: fail
            // closed as "could not establish".
            other => DenyReason::Unavailable(other.to_string()),
        })
    }

    /// The exit code this verdict maps to, ignoring enforcement. `Allowed` and `Grace` are 0.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Allowed { .. } | Self::Grace { .. } => EXIT_ALLOWED,
            Self::Denied(reason) => match reason {
                DenyReason::NotEntitled
                | DenyReason::Expired { .. }
                | DenyReason::Unauthorized
                | DenyReason::SignedOut(_) => EXIT_DENIED,
                DenyReason::Misconfigured(_) => EXIT_CONFIG,
                // Unavailable and any variant added later.
                _ => EXIT_UNAVAILABLE,
            },
        }
    }

    /// A one-line, human-readable explanation. Never empty, never carries a secret.
    pub fn message(&self) -> String {
        match self {
            Self::Allowed { source, expires_at } => format!(
                "entitlement verified (source {}, {})",
                source_name(*source),
                expiry_text(*expires_at)
            ),
            Self::Grace {
                source,
                expires_at,
                grace_ends_at,
            } => format!(
                "entitlement not re-checked online; running on the cached answer (source {}, {}) \
                 until {} - connect and sign in to refresh it",
                source_name(*source),
                expiry_text(*expires_at),
                format_unix(*grace_ends_at)
            ),
            Self::Denied(reason) => match reason {
                DenyReason::NotEntitled => {
                    "this account does not hold a Pro entitlement for this app".to_string()
                }
                DenyReason::Expired { expired_at } => format!(
                    "the entitlement for this app expired at {}",
                    format_unix(*expired_at)
                ),
                DenyReason::Unauthorized => {
                    "the entitlement service rejected this account's sign-in; sign in again with \
                     the app"
                        .to_string()
                }
                DenyReason::SignedOut(m) => format!(
                    "not signed in ({m}) and no cached entitlement covers this run; sign in with \
                     the app"
                ),
                DenyReason::Unavailable(m) => format!(
                    "could not establish the entitlement ({m}) and no cached entitlement covers \
                     this run; connect and retry"
                ),
                DenyReason::Misconfigured(m) => {
                    format!("entitlement check is misconfigured ({m})")
                }
                #[allow(unreachable_patterns)]
                _ => "entitlement refused".to_string(),
            },
        }
    }
}

/// A verdict with the release-time enforcement applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateOutcome {
    pub verdict: Verdict,
    pub enforcement: Enforcement,
}

impl GateOutcome {
    pub fn new(verdict: Verdict, enforcement: Enforcement) -> Self {
        Self {
            verdict,
            enforcement,
        }
    }

    /// May the engine run: `Allowed`, `Grace`, or a refusal under [`Enforcement::Advisory`].
    pub fn permits(&self) -> bool {
        !matches!(self.verdict, Verdict::Denied(_)) || self.enforcement == Enforcement::Advisory
    }

    /// The process exit code: 0 when [`permits`](Self::permits), otherwise the verdict's code.
    pub fn exit_code(&self) -> i32 {
        if self.permits() {
            EXIT_ALLOWED
        } else {
            self.verdict.exit_code()
        }
    }

    /// True for everything except a plain `Allowed` under `Enforce`. The engine must show
    /// these to the user (stderr, and its own log).
    pub fn must_report(&self) -> bool {
        !matches!(self.verdict, Verdict::Allowed { .. })
    }

    /// The message for the user. Never empty. A refusal let through by `Advisory` says so.
    pub fn message(&self) -> String {
        let base = self.verdict.message();
        match (&self.verdict, self.enforcement) {
            (Verdict::Denied(_), Enforcement::Advisory) => {
                format!("{base} (not enforced in this build; the engine will run)")
            }
            (Verdict::Denied(_), Enforcement::Enforce) => {
                format!("{base}; the engine will not run")
            }
            _ => base,
        }
    }

    /// Write [`message`](Self::message) as one line to `out` when
    /// [`must_report`](Self::must_report); a plain `Allowed` writes nothing.
    pub fn emit(&self, out: &mut dyn io::Write) -> io::Result<()> {
        if self.must_report() {
            writeln!(out, "{}", self.message())?;
        }
        Ok(())
    }
}

impl fmt::Display for GateOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message())
    }
}

/// The engine's startup gate: a [`SubjectEntitlementSource`] plus the release-time
/// [`Enforcement`].
pub struct EngineGate {
    source: SubjectEntitlementSource,
    enforcement: Enforcement,
}

impl EngineGate {
    /// Build the source with the caller's token provider, persisted cache
    /// ([`SubjectEntitlementSource::with_store`]) and clock, then wrap it.
    pub fn new(source: SubjectEntitlementSource, enforcement: Enforcement) -> Self {
        Self {
            source,
            enforcement,
        }
    }

    pub fn enforcement(&self) -> Enforcement {
        self.enforcement
    }

    /// Resolve the entitlement (cache, service, offline grace; see the module docs) and apply
    /// enforcement. Blocks for at most the HTTP client's timeouts.
    pub fn check(&self) -> GateOutcome {
        let verdict = match self.source.entitlement() {
            Ok(decision) => Verdict::from_decision(&decision),
            Err(SubjectEntitlementError::Token(token_error)) => {
                // Cannot ask the service. The local cache may still cover the run.
                match self.source.cached_decision() {
                    Some(decision) => Verdict::from_decision(&decision),
                    None => Verdict::from_error(&SubjectEntitlementError::Token(token_error)),
                }
            }
            Err(error) => Verdict::from_error(&error),
        };
        GateOutcome::new(verdict, self.enforcement)
    }

    /// Decide from the local cache alone: no token request, no network. No usable cache is
    /// [`DenyReason::SignedOut`].
    pub fn check_offline(&self) -> GateOutcome {
        let verdict = match self.source.cached_decision() {
            Some(decision) => Verdict::from_decision(&decision),
            None => Verdict::Denied(DenyReason::SignedOut(
                "no usable local entitlement".to_string(),
            )),
        };
        GateOutcome::new(verdict, self.enforcement)
    }
}

impl fmt::Debug for EngineGate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EngineGate")
            .field("enforcement", &self.enforcement)
            .finish_non_exhaustive()
    }
}

fn source_name(source: SubjectSource) -> &'static str {
    match source {
        SubjectSource::Play => "play",
        SubjectSource::Storekit => "storekit",
        SubjectSource::License => "license",
        SubjectSource::Absent => "none",
        _ => "unknown",
    }
}

fn expiry_text(expires_at: Option<i64>) -> String {
    match expires_at {
        None => "no expiry".to_string(),
        Some(t) => format!("expires {}", format_unix(t)),
    }
}

/// Unix seconds as `YYYY-MM-DDTHH:MM:SSZ` (UTC).
fn format_unix(t: i64) -> String {
    let days = t.div_euclid(86_400);
    let secs = t.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SubjectEntitlement, TokenError};
    use std::time::Duration;

    fn ent(pro: bool, expires_at: Option<i64>) -> SubjectEntitlement {
        SubjectEntitlement {
            app: "cutright".into(),
            pro,
            source: SubjectSource::License,
            expires_at,
            checked_at: 0,
        }
    }

    fn decision(e: SubjectEntitlement, freshness: Freshness, pro: bool) -> SubjectDecision {
        SubjectDecision {
            entitlement: e,
            freshness,
            pro,
        }
    }

    #[test]
    fn formats_unix_time() {
        assert_eq!(format_unix(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_unix(1_792_108_800), "2026-10-16T00:00:00Z");
        assert_eq!(format_unix(1_709_210_096), "2024-02-29T12:34:56Z");
        assert_eq!(format_unix(-1), "1969-12-31T23:59:59Z");
    }

    #[test]
    fn decisions_map_to_verdicts() {
        let fresh = decision(ent(true, None), Freshness::Fresh, true);
        assert!(matches!(
            Verdict::from_decision(&fresh),
            Verdict::Allowed { .. }
        ));
        let cached = decision(ent(true, None), Freshness::Cached, true);
        assert!(matches!(
            Verdict::from_decision(&cached),
            Verdict::Allowed { .. }
        ));
        let grace = decision(
            ent(true, None),
            Freshness::OfflineGrace { grace_ends_at: 99 },
            true,
        );
        assert_eq!(
            Verdict::from_decision(&grace),
            Verdict::Grace {
                source: SubjectSource::License,
                expires_at: None,
                grace_ends_at: 99
            }
        );
        let free = decision(ent(false, None), Freshness::Fresh, false);
        assert_eq!(
            Verdict::from_decision(&free),
            Verdict::Denied(DenyReason::NotEntitled)
        );
        let lapsed = decision(ent(true, Some(50)), Freshness::Cached, false);
        assert_eq!(
            Verdict::from_decision(&lapsed),
            Verdict::Denied(DenyReason::Expired { expired_at: 50 })
        );
        // A grace window never turns a non-Pro answer into a grant.
        let grace_free = decision(
            ent(false, None),
            Freshness::OfflineGrace { grace_ends_at: 99 },
            false,
        );
        assert_eq!(
            Verdict::from_decision(&grace_free),
            Verdict::Denied(DenyReason::NotEntitled)
        );
    }

    #[test]
    fn every_error_is_a_refusal_with_its_exit_code() {
        let cases: Vec<(SubjectEntitlementError, i32)> = vec![
            (
                SubjectEntitlementError::Token(TokenError::new("signed out")),
                EXIT_DENIED,
            ),
            (SubjectEntitlementError::Unauthorized, EXIT_DENIED),
            (
                SubjectEntitlementError::Offline("down".into()),
                EXIT_UNAVAILABLE,
            ),
            (
                SubjectEntitlementError::RateLimited {
                    retry_after: Some(Duration::from_secs(5)),
                },
                EXIT_UNAVAILABLE,
            ),
            (
                SubjectEntitlementError::Server { status: 503 },
                EXIT_UNAVAILABLE,
            ),
            (
                SubjectEntitlementError::BadResponse("x".into()),
                EXIT_UNAVAILABLE,
            ),
            (
                SubjectEntitlementError::Rejected { status: 404 },
                EXIT_CONFIG,
            ),
            (
                SubjectEntitlementError::Config("bad url".into()),
                EXIT_CONFIG,
            ),
        ];
        for (error, code) in cases {
            let verdict = Verdict::from_error(&error);
            assert!(matches!(verdict, Verdict::Denied(_)), "{error}");
            assert_eq!(verdict.exit_code(), code, "{error}");
            assert!(!verdict.message().is_empty());
        }
    }

    #[test]
    fn enforcement_applies_only_to_refusals_and_is_never_silent() {
        let denied = Verdict::Denied(DenyReason::NotEntitled);
        let enforce = GateOutcome::new(denied.clone(), Enforcement::Enforce);
        assert!(!enforce.permits());
        assert_eq!(enforce.exit_code(), EXIT_DENIED);
        assert!(enforce.must_report());

        let advisory = GateOutcome::new(denied, Enforcement::Advisory);
        assert!(advisory.permits());
        assert_eq!(advisory.exit_code(), EXIT_ALLOWED);
        assert!(advisory.must_report());
        assert!(advisory.message().contains("not enforced"));
        let mut out = Vec::new();
        advisory.emit(&mut out).unwrap();
        assert!(!out.is_empty());

        let allowed = GateOutcome::new(
            Verdict::Allowed {
                source: SubjectSource::Play,
                expires_at: None,
            },
            Enforcement::Enforce,
        );
        assert!(allowed.permits() && !allowed.must_report());
        let mut out = Vec::new();
        allowed.emit(&mut out).unwrap();
        assert!(out.is_empty());

        let grace = GateOutcome::new(
            Verdict::Grace {
                source: SubjectSource::Play,
                expires_at: None,
                grace_ends_at: 1_792_108_800,
            },
            Enforcement::Enforce,
        );
        assert!(grace.permits() && grace.must_report());
        assert!(grace.message().contains("2026-10-16T00:00:00Z"));
    }

    #[test]
    fn from_switch_maps_both_ways() {
        assert_eq!(Enforcement::from_switch(true), Enforcement::Enforce);
        assert_eq!(Enforcement::from_switch(false), Enforcement::Advisory);
    }
}
