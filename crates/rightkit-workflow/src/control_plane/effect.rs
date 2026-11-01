//! WP-07 — Effect intent, adapters, receipts, and uncertainty for the
//! autonomous workflow control plane.
//!
//! An *effect* is a journaled intent to perform a single bounded external
//! action on behalf of a workflow node. The workflow plane itself does not
//! execute the effect — a separate adapter (`EffectAdapter`) does — but
//! the workflow plane **owns** the lifecycle:
//!
//! 1. Compile-time: the effect adapter registry ([`EffectAdapterRegistry`])
//!    refuses to register any effect kind outside the V1 allowlist.
//! 2. Dispatch-time: the reducer journals an [`EffectIntent`] row with an
//!    idempotency key so a re-dispatch never performs the action twice.
//! 3. Receipt-time: the adapter returns an [`EffectReceipt`].
//! 4. Reconciliation: the reducer folds the receipt into one of four
//!    [`EffectOutcome`] variants — `Completed`, `Failed`, `OutcomeUnknown`,
//!    or `RequiresReconciliation` — and decides what to do next.
//!
//! ## V1 allowlist
//!
//! The V1 effect surface is deliberately small: only draft-shaped side
//! effects (`DraftDocument`, `DraftReview`, `DraftComment`) are allowed.
//! Anything that looks like publishing, merging, deploying, or otherwise
//! destructive / security-sensitive / unrestricted-messaging is rejected
//! at the registry level. Future work packages (WP-08+) widen the
//! allowlist under tighter governance.
//!
//! ## Why receipts are first-class
//!
//! External systems fail in ways we cannot predict (network partitions,
//! restarts, leases). A receipt — even a `Rejected` or `Unknown` one —
//! is the durable evidence that the action was attempted. The reducer
//! journals every receipt so replay can reconstruct the exact sequence
//! of attempts.

#![allow(clippy::module_name_repetitions)]

use serde::{Deserialize, Serialize};

// ─── EffectIntent ────────────────────────────────────────────────────────────

/// A durable effect intent. One row per dispatch attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectIntent {
    /// `wfe_*` — namespaced id assigned at journal time.
    pub id: String,
    /// `wfr_*` — namespaced run id this intent belongs to.
    pub run_id: String,
    /// `wfn_*` — namespaced node id this intent is registered against.
    pub node_id: String,
    /// Adapter kind — MUST be a member of the V1 allowlist (see
    /// [`is_v1_allowed`]). Examples: `DraftDocument`, `DraftReview`,
    /// `DraftComment`.
    pub adapter_kind: String,
    /// Idempotency key. The adapter MUST treat a repeat of this key as a
    /// no-op (so re-dispatch after a lease loss does not double-apply).
    pub idempotency_key: String,
    /// `sha256:`-prefixed content hash of the canonical-JSON-encoded
    /// effect payload. Recorded so the reducer can verify the dispatch
    /// matches the compiled intent on replay.
    pub payload_hash: String,
    /// Absolute Unix epoch millisecond at which this intent row was
    /// journaled.
    pub requested_at_unix_ms: i64,
    /// Time-to-live in milliseconds. After `requested_at_unix_ms +
    /// ttl_unix_ms`, the adapter MUST refuse the dispatch and the reducer
    /// MUST escalate to [`EffectOutcome::RequiresReconciliation`].
    pub ttl_unix_ms: i64,
}

// ─── EffectReceipt ───────────────────────────────────────────────────────────

/// An adapter's response to one [`EffectIntent`] dispatch. Every variant
/// carries enough metadata for the reducer to journal the receipt verbatim
/// — even a rejection is durable evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EffectReceipt {
    /// The adapter accepted the intent and returned a receipt id. The
    /// reducer treats `Accepted` as **tentatively** completed — the actual
    /// completion is confirmed by a subsequent completion callback (out of
    /// scope for V1; recorded for forward compatibility).
    Accepted {
        /// Adapter-supplied receipt id.
        receipt_id: String,
        /// Absolute Unix epoch millisecond at which the adapter accepted
        /// the intent.
        accepted_at_unix_ms: i64,
    },
    /// The adapter refused the intent. The reducer treats this as a
    /// non-retryable [`EffectOutcome::Failed`] unless the adapter flags it
    /// retryable (out of scope for V1).
    Rejected {
        /// Free-form reason text recorded verbatim for audit.
        reason: String,
        /// Absolute Unix epoch millisecond at which the adapter rejected
        /// the intent.
        rejected_at_unix_ms: i64,
    },
    /// The adapter could not reach a definitive decision (e.g. the call
    /// timed out, the lease was lost mid-flight, the upstream returned a
    /// non-canonical status). The reducer MUST escalate to
    /// [`EffectOutcome::OutcomeUnknown`] or
    /// [`EffectOutcome::RequiresReconciliation`].
    Unknown {
        /// Free-form diagnostic hint recorded verbatim for audit.
        hint: String,
    },
}

// ─── EffectOutcome ───────────────────────────────────────────────────────────

/// The reducer's view of one effect attempt. Exactly one variant is
/// produced per `EffectReceipt` (or per lease-loss event).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EffectOutcome {
    /// Effect completed successfully. The receipt is preserved verbatim
    /// for audit and downstream delivery.
    Completed {
        /// Adapter receipt.
        receipt: EffectReceipt,
    },
    /// Effect failed terminally (or after exhausting retries). The receipt
    /// is preserved verbatim; `retryable` records whether the failure was
    /// theoretically retryable (caller policy decides whether to act on
    /// it).
    Failed {
        /// Adapter receipt.
        receipt: EffectReceipt,
        /// True when the failure class is in the retryable set.
        retryable: bool,
    },
    /// The effect's terminal state is not knowable from the receipt alone
    /// (e.g. the adapter returned [`EffectReceipt::Unknown`]). The reducer
    /// MUST escalate to the reconciliation loop.
    OutcomeUnknown {
        /// Adapter receipt.
        receipt: EffectReceipt,
        /// Absolute Unix epoch millisecond at which the adapter last
        /// signalled activity for this intent. Used to bound the
        /// reconciliation window.
        last_seen_at_unix_ms: i64,
    },
    /// The effect cannot be reconciled automatically (e.g. the intent
    /// expired before the adapter responded, the lease was lost and no
    /// resume arrived). The reducer MUST publish an attention row and
    /// stop dispatching on this node until a human (or a later work
    /// package) intervenes.
    RequiresReconciliation,
}

// ─── EffectAdapterRegistry ───────────────────────────────────────────────────

/// V1 allowlist of adapter kinds. Closed set: only `DraftDocument`,
/// `DraftReview`, and `DraftComment` are permitted. Anything that looks
/// like publishing, merging, deploying, or otherwise destructive /
/// security-sensitive / unrestricted-messaging is rejected even if the
/// caller tries to slip it in via a custom kind name.
const V1_ALLOWED_KINDS: &[&str] = &["DraftDocument", "DraftReview", "DraftComment"];

/// Defensive keywords that the registry rejects anywhere in a kind name.
/// The intent is to make sure future additions cannot quietly widen the
/// surface (e.g. a kind named `DraftDocumentPublish` is rejected even
/// though it starts with a V1 prefix).
const V1_DENIED_KEYWORDS: &[&str] = &[
    "publish",
    "merge",
    "deploy",
    "destructive",
    "security",
    "unrestricted-messaging",
    "unrestricted_messaging",
    "unrestrictedMessaging",
];

/// Pure-data registry of allowed adapter kinds. The registry is the only
/// authority for "is this kind allowed"; the reducer MUST consult it
/// before journaling an [`EffectIntent`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectAdapterRegistry {
    /// Exact-match allowlist. Comparison is case-sensitive.
    allowed: Vec<String>,
}

impl EffectAdapterRegistry {
    /// Build the V1 registry. The set is fixed by `V1_ALLOWED_KINDS`;
    /// callers that need a wider surface must build their own registry
    /// from a future work package's policy.
    pub fn v1_default() -> Self {
        Self {
            allowed: V1_ALLOWED_KINDS.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    /// Build a registry from an explicit allowlist. Mostly useful in tests
    /// and in policy builders that want a narrow surface.
    pub fn from_allowed<I: IntoIterator<Item = String>>(kinds: I) -> Self {
        Self {
            allowed: kinds.into_iter().collect(),
        }
    }

    /// Borrow the underlying allowlist.
    pub fn as_slice(&self) -> &[String] {
        &self.allowed
    }

    /// True when `kind` is permitted by this registry.
    ///
    /// The check is:
    ///
    /// 1. **Defensive keyword scan** — `kind` must not contain any of
    ///    `V1_DENIED_KEYWORDS` (case-insensitive).
    /// 2. **Exact-match allowlist** — `kind` must appear verbatim in the
    ///    registry's `allowed` list.
    pub fn is_allowed(&self, kind: &str) -> bool {
        if contains_any_denied_keyword(kind) {
            return false;
        }
        self.allowed.iter().any(|k| k == kind)
    }
}

impl Default for EffectAdapterRegistry {
    fn default() -> Self {
        Self::v1_default()
    }
}

/// Case-insensitive substring scan over [`V1_DENIED_KEYWORDS`].
fn contains_any_denied_keyword(kind: &str) -> bool {
    let lower = kind.to_ascii_lowercase();
    V1_DENIED_KEYWORDS
        .iter()
        .any(|kw| lower.contains(&kw.to_ascii_lowercase()))
}

/// Free-function form of [`EffectAdapterRegistry::is_allowed`] against the
/// V1 default registry. Callers that do not need to override the registry
/// use this; the reducer's adapter-allow check is the canonical use site.
pub fn is_v1_allowed(kind: &str) -> bool {
    EffectAdapterRegistry::v1_default().is_allowed(kind)
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_v1_allowed_accepts_the_three_v1_kinds() {
        assert!(is_v1_allowed("DraftDocument"));
        assert!(is_v1_allowed("DraftReview"));
        assert!(is_v1_allowed("DraftComment"));
    }

    #[test]
    fn is_v1_allowed_rejects_unknown_kinds() {
        assert!(!is_v1_allowed("PublishDocument"));
        assert!(!is_v1_allowed("MergeRequest"));
        assert!(!is_v1_allowed("DeployService"));
        assert!(!is_v1_allowed("RunShell"));
        assert!(!is_v1_allowed(""));
    }

    #[test]
    fn is_v1_allowed_rejects_kinds_containing_denied_keywords() {
        // The defensive keyword scan catches even names that *start* with
        // a V1 prefix.
        assert!(!is_v1_allowed("DraftDocumentPublish"));
        assert!(!is_v1_allowed("DraftReviewMerge"));
        assert!(!is_v1_allowed("DraftCommentDeploy"));
        assert!(!is_v1_allowed("DestructiveOp"));
        assert!(!is_v1_allowed("SecurityAudit"));
        assert!(!is_v1_allowed("Unrestricted-Messaging"));
    }

    #[test]
    fn registry_default_has_exactly_three_kinds() {
        let r = EffectAdapterRegistry::v1_default();
        assert_eq!(r.as_slice().len(), 3);
        assert!(r.as_slice().contains(&"DraftDocument".to_string()));
        assert!(r.as_slice().contains(&"DraftReview".to_string()));
        assert!(r.as_slice().contains(&"DraftComment".to_string()));
    }

    #[test]
    fn registry_default_matches_v1_free_function() {
        let r = EffectAdapterRegistry::v1_default();
        for kind in ["DraftDocument", "DraftReview", "DraftComment"] {
            assert_eq!(r.is_allowed(kind), is_v1_allowed(kind));
        }
        for kind in ["PublishDocument", "RunShell", ""] {
            assert_eq!(r.is_allowed(kind), is_v1_allowed(kind));
            assert!(!r.is_allowed(kind));
        }
    }

    #[test]
    fn registry_from_allowed_respects_custom_allowlist() {
        let r = EffectAdapterRegistry::from_allowed(vec!["CustomKind".into()]);
        assert!(r.is_allowed("CustomKind"));
        // The default V1 kinds are NOT in a custom allowlist.
        assert!(!r.is_allowed("DraftDocument"));
        // Defensive keyword scan still applies.
        assert!(!r.is_allowed("CustomPublish"));
    }

    #[test]
    fn effect_outcome_serializes_each_variant() {
        let r = EffectReceipt::Accepted {
            receipt_id: "rcp_1".into(),
            accepted_at_unix_ms: 100,
        };
        let o = EffectOutcome::Completed { receipt: r.clone() };
        let s = serde_json::to_string(&o).expect("serialize");
        let back: EffectOutcome = serde_json::from_str(&s).expect("deserialize");
        assert_eq!(back, o);

        let o = EffectOutcome::Failed {
            receipt: r.clone(),
            retryable: true,
        };
        let s = serde_json::to_string(&o).expect("serialize");
        let back: EffectOutcome = serde_json::from_str(&s).expect("deserialize");
        assert_eq!(back, o);

        let o = EffectOutcome::OutcomeUnknown {
            receipt: EffectReceipt::Unknown {
                hint: "lease lost".into(),
            },
            last_seen_at_unix_ms: 50,
        };
        let s = serde_json::to_string(&o).expect("serialize");
        let back: EffectOutcome = serde_json::from_str(&s).expect("deserialize");
        assert_eq!(back, o);

        let o = EffectOutcome::RequiresReconciliation;
        let s = serde_json::to_string(&o).expect("serialize");
        let back: EffectOutcome = serde_json::from_str(&s).expect("deserialize");
        assert_eq!(back, o);
    }

    #[test]
    fn effect_intent_struct_round_trips() {
        let i = EffectIntent {
            id: "wfe_1".into(),
            run_id: "wfr_run".into(),
            node_id: "wfn_node".into(),
            adapter_kind: "DraftDocument".into(),
            idempotency_key: "idem_1".into(),
            payload_hash: "sha256:abc".into(),
            requested_at_unix_ms: 1_000,
            ttl_unix_ms: 30_000,
        };
        let s = serde_json::to_string(&i).expect("serialize");
        let back: EffectIntent = serde_json::from_str(&s).expect("deserialize");
        assert_eq!(back, i);
    }
}
