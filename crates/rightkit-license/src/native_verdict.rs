//! A native paid-command verdict that says why it decided, for hosts that gate native code
//! (cloud features, an engine switch) on an entitlement.
//!
//! [`ObservedEntitlements`] wraps one [`NativeEngineEntitlements`] and answers one question,
//! [`ObservedEntitlements::require_pro`]: does the signed lifecycle, verified now against the
//! anti-rollback clock, grant access? It reads no renderer value, no pushed flag and no
//! environment. It does the same verification as [`NativeEngineEntitlements::require_pro`] but
//! keeps the lifecycle on a refusal, so a Pro to Free flip can name its cause.
//!
//! What it reports, all of it a fixed vocabulary and none of it a token, key, device id, path or
//! server text:
//!
//! * [`ObservedEntitlements::last_cause`]: the cause of the most recent verdict (`granted`,
//!   `not_entitled`, or a refusal cause below), `None` before the first check.
//! * the host's event sink: `license_pro_refused` with `cause=<cause>` for a refusal, and
//!   `license_pro_verdict` with `verdict=<verdict> lifecycle=<lifecycle>` only when the pair
//!   differs from the previous one, so a check per user action does not flood a log.
//!
//! Refusal causes: `source_unavailable`; for a refused source read the host-supplied code
//! (the owner's `NativeRefusal::as_str`, for example) or `source_refused`; `clock`;
//! `invalid_input`; `not_entitled`; `poisoned`.
//!
//! No thread, timer or static. Verdict checks and observation updates are serialized across
//! the credential-store read. Event sinks run after all observation locks are released;
//! concurrent checks may interleave their event delivery. The refusal-code callback runs
//! during the check and must not re-enter this value.

use crate::{
    ClockStore, NativeEngineEntitlements, NativeEntitlementError, NativeEntitlementSource,
    NativeSourceError,
};
use std::fmt;
use std::sync::Mutex;

/// Host event sink: `(event name, stable detail)`. Receives no secret, no path and no user text.
pub type EventSink = fn(&str, &str);

/// Event name of a refused verdict. Detail: `cause=<cause>`, or the fixed text
/// `entitlement lock poisoned`.
pub const EVENT_REFUSED: &str = "license_pro_refused";
/// Event name of a changed verdict. Detail: `verdict=<verdict> lifecycle=<lifecycle>`.
pub const EVENT_VERDICT: &str = "license_pro_verdict";

/// The host's way to name why a refused source read was refused: the owner's fixed refusal code
/// (`busy`, `unresolved`, `store`, ...), or `None` when it has none. Must return a closed
/// vocabulary, never text from a server or a path.
pub type RefusalCode = Box<dyn Fn() -> Option<&'static str> + Send + Sync>;

/// See the module documentation.
pub struct ObservedEntitlements<N, C: ClockStore> {
    entitlements: Mutex<NativeEngineEntitlements<N, C>>,
    refusal_code: RefusalCode,
    sink: EventSink,
    last_cause: Mutex<Option<&'static str>>,
    last_verdict: Mutex<Option<(&'static str, &'static str)>>,
}

impl<N, C> fmt::Debug for ObservedEntitlements<N, C>
where
    C: ClockStore,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ObservedEntitlements")
            .finish_non_exhaustive()
    }
}

impl<N: NativeEntitlementSource, C: ClockStore> ObservedEntitlements<N, C>
where
    C::Error: fmt::Display,
{
    /// Wrap `entitlements`. `refusal_code` names a refused source read; `sink` receives the
    /// events above.
    pub fn new(
        entitlements: NativeEngineEntitlements<N, C>,
        refusal_code: RefusalCode,
        sink: EventSink,
    ) -> Self {
        Self {
            entitlements: Mutex::new(entitlements),
            refusal_code,
            sink,
            last_cause: Mutex::new(None),
            last_verdict: Mutex::new(None),
        }
    }

    /// Does the verified lifecycle grant access now? `false` on every refusal and on a poisoned
    /// lock: nothing here grants on an error.
    pub fn require_pro(&self) -> bool {
        let entitlements = match self.entitlements.lock() {
            Ok(entitlements) => entitlements,
            Err(poisoned) => {
                drop(poisoned);
                (self.sink)(EVENT_REFUSED, "entitlement lock poisoned");
                return false;
            }
        };
        let (allowed, verdict, lifecycle, refusal) = match entitlements.current() {
            Ok(report) if report.grants_access() => {
                (true, "granted", report.lifecycle.as_str(), None)
            }
            Ok(report) => (false, "not_entitled", report.lifecycle.as_str(), None),
            Err(error) => {
                let cause = self.refusal_cause(&error);
                (false, "refused", cause, Some(cause))
            }
        };
        self.set_cause(refusal.unwrap_or(verdict));
        let changed = self.transition(verdict, lifecycle);
        drop(entitlements);
        if let Some(cause) = refusal {
            (self.sink)(EVENT_REFUSED, &format!("cause={cause}"));
        }
        if changed {
            (self.sink)(
                EVENT_VERDICT,
                &format!("verdict={verdict} lifecycle={lifecycle}"),
            );
        }
        allowed
    }

    /// The cause of the most recent verdict made by [`Self::require_pro`]; `None` before the first.
    pub fn last_cause(&self) -> Option<&'static str> {
        *self
            .last_cause
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The closed-vocabulary reason a native read failed.
    fn refusal_cause(&self, error: &NativeEntitlementError) -> &'static str {
        match error {
            NativeEntitlementError::Source(NativeSourceError::Unavailable) => "source_unavailable",
            NativeEntitlementError::Source(NativeSourceError::Refused) => {
                (self.refusal_code)().unwrap_or("source_refused")
            }
            NativeEntitlementError::Clock(_) => "clock",
            NativeEntitlementError::InvalidInput => "invalid_input",
            NativeEntitlementError::NotEntitled => "not_entitled",
            NativeEntitlementError::Poisoned => "poisoned",
        }
    }

    fn set_cause(&self, cause: &'static str) {
        *self
            .last_cause
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(cause);
    }

    /// Record whether the `(verdict, lifecycle)` pair changed.
    fn transition(&self, verdict: &'static str, lifecycle: &'static str) -> bool {
        let mut last = self
            .last_verdict
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if *last != Some((verdict, lifecycle)) {
            *last = Some((verdict, lifecycle));
            true
        } else {
            false
        }
    }
}
