//! License lifecycle classification.
//!
//! The classification the apps' UI keys on, computed once here instead of in every app:
//! given an authenticated [`Entitlement`] and a trusted time, which state is the license
//! in? [`evaluate_lifecycle`] adds the whole native pipeline around it (authenticate,
//! reconcile the anti-rollback clock, skew check, requirement check, classify, commit).
//!
//! The variant set is the union of what the shipping apps' lifecycle snapshots expose
//! (ViewRight `license_lifecycle_snapshot`, HeardRight `license_lifecycle_snapshot`),
//! plus `Grace`/`Expired`/`Revoked`, which the JS client already distinguishes.
//! [`Lifecycle::snapshot_kind`] maps back to the apps' existing wire strings so a UI
//! keyed on `active_trial` / `expired_trial` / `verification_required` keeps working.

use serde::{Deserialize, Serialize};

use crate::clock::{
    AntiRollbackClock, ClockError, ClockReading, ClockStore, CLOCK_ROLLBACK_TOLERANCE_SECS,
};
use crate::{authenticate_token, Entitlement, EntitlementStage, LicenseKind, LicenseRequirement};

/// Where a license stands. `#[non_exhaustive]`: match with a wildcard arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Lifecycle {
    /// No license, or a valid token whose entitlement does not satisfy the required tier.
    Unlicensed,
    /// An older paid record with an activation credential but no signed token; it needs
    /// one signed online refresh.
    MigrationRequired,
    /// Time-bound trial, unexpired.
    Trial,
    /// Trial past its signed `exp`. Trials never get grace.
    ExpiredTrial,
    /// Term or legacy (no lifecycle claims) license, unexpired.
    Active,
    /// Term, legacy or final-lifetime license past `exp` but inside
    /// [`LifecyclePolicy::grace_secs`]. Never produced with the default policy.
    Grace,
    /// Term, legacy or final-lifetime license past `exp` (and past any grace).
    Expired,
    /// Lifetime purchase before its one mandatory confirmation (`exp == confirmation_due_at`).
    LifetimeProvisional,
    /// Provisional lifetime purchase past `confirmation_due_at`; needs the online check.
    VerificationRequired,
    /// Confirmed lifetime license.
    Lifetime,
    /// The device clock is behind the persisted high-water mark; needs an online re-anchor.
    ClockRepairRequired,
    /// The server reported the license no longer active.
    Revoked,
    /// The token failed authentication or its lifecycle claims are inconsistent.
    Invalid,
}

impl Lifecycle {
    /// Whether this state grants paid access.
    pub fn grants_access(self) -> bool {
        matches!(
            self,
            Self::Trial | Self::Active | Self::Grace | Self::LifetimeProvisional | Self::Lifetime
        )
    }

    /// States that the user can resolve by one online refresh
    /// ([`crate::refresh_and_reanchor`]).
    pub fn needs_online_refresh(self) -> bool {
        matches!(
            self,
            Self::MigrationRequired | Self::VerificationRequired | Self::ClockRepairRequired
        )
    }

    /// The serde / JS name (`"expired_trial"`, ...).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unlicensed => "unlicensed",
            Self::MigrationRequired => "migration_required",
            Self::Trial => "trial",
            Self::ExpiredTrial => "expired_trial",
            Self::Active => "active",
            Self::Grace => "grace",
            Self::Expired => "expired",
            Self::LifetimeProvisional => "lifetime_provisional",
            Self::VerificationRequired => "verification_required",
            Self::Lifetime => "lifetime",
            Self::ClockRepairRequired => "clock_repair_required",
            Self::Revoked => "revoked",
            Self::Invalid => "invalid",
        }
    }

    /// The `kind` string the apps' existing lifecycle snapshots use, so a renderer that
    /// keys on them needs no change. `Expired`, `Revoked` and `Invalid` all map to
    /// `"invalid"` and `Grace` to `"active_legacy"`, exactly as those apps report them.
    pub fn snapshot_kind(self) -> &'static str {
        match self {
            Self::Unlicensed => "free",
            Self::MigrationRequired => "migration_required",
            Self::Trial => "active_trial",
            Self::ExpiredTrial => "expired_trial",
            Self::Active | Self::Grace => "active_legacy",
            Self::LifetimeProvisional => "active_lifetime_provisional",
            Self::VerificationRequired => "verification_required",
            Self::Lifetime => "active_lifetime",
            Self::ClockRepairRequired => "clock_repair_required",
            Self::Expired | Self::Revoked | Self::Invalid => "invalid",
        }
    }
}

/// Tunable classification policy. The default is what every shipping app does today.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LifecyclePolicy {
    /// Seconds past `exp` during which a term, legacy or final-lifetime license is
    /// [`Lifecycle::Grace`] instead of [`Lifecycle::Expired`]. Default `0` (hard expiry,
    /// as in all four native apps). Negative values are treated as `0`. Trials and
    /// provisional lifetime licenses never get grace.
    ///
    /// Raising this makes **native authorization accept an expired token**; it is an
    /// opt-in for apps that deliberately mirror the JS client's `graceDays`.
    pub grace_secs: i64,
    /// Let a verified *final* lifetime license keep working, evaluated at the persisted
    /// high-water mark, when the clock is rolled back (HeardRight behaviour). Default
    /// `false`: a rolled-back clock yields [`Lifecycle::ClockRepairRequired`] for everyone.
    pub lifetime_survives_rollback: bool,
}

impl Entitlement {
    /// Classify an **already authenticated** entitlement at `now` (use a trusted time,
    /// not the raw wall clock). Pure; does not check the signature, the clock or the
    /// issue-time skew; [`evaluate_lifecycle`] does all of that.
    pub fn lifecycle(&self, now: i64, policy: &LifecyclePolicy) -> Lifecycle {
        if !self.has_valid_lifecycle() {
            return Lifecycle::Invalid;
        }
        let live = now < self.exp;
        let in_grace = now < self.exp.saturating_add(policy.grace_secs.max(0));
        let open_ended = || {
            if live {
                Lifecycle::Active
            } else if in_grace {
                Lifecycle::Grace
            } else {
                Lifecycle::Expired
            }
        };
        match (self.license_kind, self.entitlement_stage) {
            (None, None) | (Some(LicenseKind::Term), Some(EntitlementStage::TimeBound)) => {
                open_ended()
            }
            (Some(LicenseKind::Trial), Some(EntitlementStage::TimeBound)) => {
                if live {
                    Lifecycle::Trial
                } else {
                    Lifecycle::ExpiredTrial
                }
            }
            (Some(LicenseKind::Lifetime), Some(EntitlementStage::Provisional)) => {
                if live {
                    Lifecycle::LifetimeProvisional
                } else {
                    Lifecycle::VerificationRequired
                }
            }
            (Some(LicenseKind::Lifetime), Some(EntitlementStage::Final)) => {
                if live {
                    Lifecycle::Lifetime
                } else if in_grace {
                    Lifecycle::Grace
                } else {
                    Lifecycle::Expired
                }
            }
            _ => Lifecycle::Invalid,
        }
    }
}

/// What the host has stored for this license, as far as lifecycle is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoredCredential<'a> {
    /// No license record at all.
    Absent,
    /// The server previously reported the license inactive.
    Revoked,
    /// Active record with an activation credential but no signed token.
    LegacyUnsigned,
    /// A record with neither a signed token nor an activation credential.
    Unsigned,
    /// A signed entitlement token. `clock_initialized` is the record's "anti-rollback
    /// clock initialised" flag (see [`AntiRollbackClock::reconcile`]).
    Signed {
        token: &'a str,
        clock_initialized: bool,
    },
}

/// Static inputs to [`evaluate_lifecycle`].
#[derive(Debug, Clone, Copy)]
pub struct LifecycleContext<'a> {
    pub pubkey: &'a [u8; 32],
    pub expected_app: &'a str,
    pub device_id: &'a str,
    pub policy: LifecyclePolicy,
    /// If set, an authentic, unexpired entitlement that does not satisfy it is reported as
    /// [`Lifecycle::Unlicensed`] (the apps' "free", e.g. a non-Pro token).
    pub requirement: Option<&'a LicenseRequirement>,
}

impl<'a> LifecycleContext<'a> {
    pub fn new(pubkey: &'a [u8; 32], expected_app: &'a str, device_id: &'a str) -> Self {
        Self {
            pubkey,
            expected_app,
            device_id,
            policy: LifecyclePolicy::default(),
            requirement: None,
        }
    }

    pub fn with_policy(mut self, policy: LifecyclePolicy) -> Self {
        self.policy = policy;
        self
    }

    pub fn with_requirement(mut self, requirement: &'a LicenseRequirement) -> Self {
        self.requirement = Some(requirement);
        self
    }
}

/// The verdict of [`evaluate_lifecycle`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleReport {
    pub lifecycle: Lifecycle,
    /// The authenticated entitlement, present whenever the token authenticated (even if
    /// expired or rejected afterwards). It is *authentic*, not necessarily *valid*:
    /// grant access only if [`LifecycleReport::grants_access`].
    pub entitlement: Option<Entitlement>,
    pub expires_at: Option<i64>,
    pub confirmation_due_at: Option<i64>,
    /// The trusted time the verdict was computed at.
    pub effective_now: Option<i64>,
    pub message: Option<String>,
    /// The anti-rollback ledger is now persisted. If the record's flag is unset, the host
    /// must set it now (and persist it) so that deleting the ledger later is detected.
    pub clock_initialized: bool,
}

impl LifecycleReport {
    pub fn grants_access(&self) -> bool {
        self.lifecycle.grants_access()
    }

    fn bare(lifecycle: Lifecycle, message: Option<&str>, clock_initialized: bool) -> Self {
        Self {
            lifecycle,
            entitlement: None,
            expires_at: None,
            confirmation_due_at: None,
            effective_now: None,
            message: message.map(str::to_string),
            clock_initialized,
        }
    }

    fn for_entitlement(
        lifecycle: Lifecycle,
        entitlement: Entitlement,
        effective_now: Option<i64>,
        message: Option<String>,
        clock_initialized: bool,
    ) -> Self {
        Self {
            lifecycle,
            expires_at: Some(entitlement.exp),
            confirmation_due_at: entitlement.confirmation_due_at,
            entitlement: Some(entitlement),
            effective_now,
            message,
            clock_initialized,
        }
    }
}

/// The full native lifecycle verdict for one stored license.
///
/// Pipeline for a [`StoredCredential::Signed`] token:
/// 1. [`authenticate_token`] (signature, app, device). Failure is [`Lifecycle::Invalid`].
/// 2. Reconcile the anti-rollback clock. A missing ledger on an initialised install is
///    `Err(ClockError::Missing)`. A rolled-back clock is
///    [`Lifecycle::ClockRepairRequired`], except that with
///    [`LifecyclePolicy::lifetime_survives_rollback`] a *final lifetime* token is evaluated
///    at the persisted high-water mark instead.
/// 3. **Issue-time skew check on every path, including already-expired tokens.**
///    If `effective_now + 300s < iat` the result is [`Lifecycle::Invalid`], never
///    `Expired*`. (The previous per-app code re-verified expired tokens at `exp - 1`,
///    which skipped `verify_token`'s own skew test; this check is therefore load-bearing.)
/// 4. Requirement check (`ctx.requirement`) -> [`Lifecycle::Unlicensed`].
/// 5. Classify with [`Entitlement::lifecycle`] and commit the clock (throttled).
///    The clock is only written on paths that reach classification.
pub fn evaluate_lifecycle<S: ClockStore>(
    ctx: &LifecycleContext<'_>,
    clock: &AntiRollbackClock<S>,
    credential: StoredCredential<'_>,
    wall_now: i64,
) -> Result<LifecycleReport, ClockError<S::Error>> {
    let (token, was_initialized) = match credential {
        StoredCredential::Absent => {
            return Ok(LifecycleReport::bare(Lifecycle::Unlicensed, None, false))
        }
        StoredCredential::Revoked => {
            return Ok(LifecycleReport::bare(
                Lifecycle::Revoked,
                Some("The server no longer considers this license active."),
                false,
            ))
        }
        StoredCredential::LegacyUnsigned => {
            return Ok(LifecycleReport::bare(
                Lifecycle::MigrationRequired,
                Some("This older paid record needs one signed online refresh."),
                false,
            ))
        }
        StoredCredential::Unsigned => {
            return Ok(LifecycleReport::bare(
                Lifecycle::Invalid,
                Some("A current signed license is required."),
                false,
            ))
        }
        StoredCredential::Signed {
            token,
            clock_initialized,
        } => (token, clock_initialized),
    };

    let entitlement = match authenticate_token(token, ctx.pubkey, ctx.expected_app, ctx.device_id) {
        Ok(entitlement) => entitlement,
        Err(error) => {
            return Ok(LifecycleReport::bare(
                Lifecycle::Invalid,
                Some(&error.to_string()),
                was_initialized,
            ))
        }
    };

    let reading = clock.reconcile(wall_now, was_initialized)?;
    let (effective_now, trusted) = match &reading {
        ClockReading::Trusted { effective_now, .. } => (*effective_now, true),
        ClockReading::Rollback { highest_seen } => {
            if ctx.policy.lifetime_survives_rollback && entitlement.is_final_lifetime() {
                (*highest_seen, false)
            } else {
                let message = format!(
                    "device clock moved backwards or stopped advancing after restart (last trusted time {highest_seen})"
                );
                return Ok(LifecycleReport::for_entitlement(
                    Lifecycle::ClockRepairRequired,
                    entitlement,
                    None,
                    Some(message),
                    was_initialized,
                ));
            }
        }
    };

    if effective_now.saturating_add(CLOCK_ROLLBACK_TOLERANCE_SECS) < entitlement.iat {
        return Ok(LifecycleReport::for_entitlement(
            Lifecycle::Invalid,
            entitlement,
            Some(effective_now),
            Some("The license issue time is later than the device clock.".to_string()),
            was_initialized,
        ));
    }

    if let Some(requirement) = ctx.requirement {
        if !entitlement.satisfies(requirement) {
            return Ok(LifecycleReport::for_entitlement(
                Lifecycle::Unlicensed,
                entitlement,
                Some(effective_now),
                None,
                was_initialized,
            ));
        }
    }

    let lifecycle = entitlement.lifecycle(effective_now, &ctx.policy);
    if trusted {
        clock.commit(&reading)?;
    }
    let message = match lifecycle {
        Lifecycle::Expired => Some("The signed license has expired.".to_string()),
        Lifecycle::Invalid => Some("The signed license lifecycle is invalid.".to_string()),
        _ => None,
    };
    Ok(LifecycleReport::for_entitlement(
        lifecycle,
        entitlement,
        Some(effective_now),
        message,
        was_initialized || trusted,
    ))
}
