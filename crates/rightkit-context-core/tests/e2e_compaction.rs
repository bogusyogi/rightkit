//! End-to-end: host compaction through the public pipeline. A session is
//! compacted under a token budget (obligations protected, narrative reduced,
//! optional residual summary), and the resulting federation decision is
//! committed exactly once, within its liveness window and restart epoch.

use rightkit_context_core::compaction::{
    assemble, check_federation_liveness, compact, CompactionConfig, CompactionFederationDecision,
    CompactionInput, CompactionItem, FederationCommitLedger, FederationLivenessError,
    ResidualNarrativeSummarizer, CATEGORY_NARRATIVE, CATEGORY_OBLIGATION,
    COMPACTION_FEDERATION_V1_SCHEMA,
};
use rightkit_context_core::compaction_receipt::CompactionSourceCursor;

const GOLDEN_FIXTURE: &str = include_str!("fixtures/compaction-federation-v1.golden.json");

fn golden_decision() -> CompactionFederationDecision {
    serde_json::from_str(GOLDEN_FIXTURE).expect("golden fixture parses as a decision")
}

fn input() -> CompactionInput {
    CompactionInput {
        source_cursor: CompactionSourceCursor {
            session_id: "session-1".into(),
            last_seq: 7,
        },
        obligations: vec![CompactionItem {
            id: "obligation-1".into(),
            category: CATEGORY_OBLIGATION.into(),
            content: "must preserve exact identifier ABC-123".into(),
            priority: 100,
            source_seq: 1,
            protected: true,
        }],
        narrative: vec![CompactionItem::new(
            "narrative-1",
            CATEGORY_NARRATIVE,
            "old exploratory detail may be reduced",
        )],
        ..Default::default()
    }
}

struct Summarizer(std::cell::Cell<usize>);

impl ResidualNarrativeSummarizer for Summarizer {
    fn summarize(&self, _residual: &[CompactionItem], _budget_tokens: u32) -> Option<String> {
        self.0.set(self.0.get() + 1);
        Some("residual summary".into())
    }
    fn provider(&self) -> &str {
        "fixture-provider"
    }
}

#[test]
fn compaction_protects_obligations_under_any_budget_and_summarizes_only_the_residue() {
    // Roomy budget: deterministic, input untouched, obligations retained.
    let source = input();
    let before = source.clone();
    let roomy = compact(
        &source,
        &CompactionConfig {
            budget_tokens: 64,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(source, before, "input is never mutated");
    assert_eq!(roomy.projection.source_cursor.last_seq, 7);
    assert_eq!(roomy.receipt.retained_obligations, ["obligation-1"]);
    assert!(roomy.receipt.budget_met && !roomy.receipt.fallback_used);

    // Tight budget: the optional summarizer runs exactly once, for residue only.
    let summarizer = Summarizer(Default::default());
    let tight = assemble(
        &input(),
        &CompactionConfig {
            budget_tokens: 11,
            ..Default::default()
        },
        Some(&summarizer),
    )
    .unwrap();
    assert_eq!(summarizer.0.get(), 1);
    assert!(tight.receipt.fallback_used);
    assert_eq!(
        tight.receipt.fallback_provider.as_deref(),
        Some("fixture-provider")
    );
    assert!(tight
        .projection
        .retained
        .iter()
        .any(|item| item.category == "residual_narrative"));

    // Impossible budget: protected material still survives and the miss is receipted.
    let mut protected = input();
    protected.obligations[0].content = "identifier ABC-123 constraint must never be elided".into();
    let overflow = compact(
        &protected,
        &CompactionConfig {
            budget_tokens: 1,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!overflow.receipt.budget_met);
    assert!(overflow.projection.rendered_text.contains("ABC-123"));
    assert!(overflow
        .receipt
        .omitted_categories
        .contains(&CATEGORY_NARRATIVE.to_string()));

    // Ambiguous input (duplicate ids) fails closed.
    let mut duplicate = input();
    duplicate
        .session
        .push(CompactionItem::new("obligation-1", "session", "duplicate"));
    assert!(compact(&duplicate, &CompactionConfig::default()).is_err());
}

#[test]
fn a_federation_decision_is_one_use_expiring_and_bound_to_its_restart_epoch() {
    let decision = golden_decision();
    assert_eq!(decision.schema, COMPACTION_FEDERATION_V1_SCHEMA);
    assert_eq!(
        decision.selection_receipt.selected_item_ids,
        ["ctx-item-0001", "ctx-item-0002", "ctx-item-0017"]
    );
    // Wire round trip is lossless.
    let reparsed: CompactionFederationDecision =
        serde_json::from_str(&serde_json::to_string_pretty(&decision).unwrap()).unwrap();
    assert_eq!(decision, reparsed);

    // Replay one millisecond past expiry is refused.
    let late = decision.issued_monotonic_elapsed_ms + decision.expires_after_elapsed_ms + 1;
    assert_eq!(
        check_federation_liveness(&decision, decision.restart_epoch, late),
        Err(FederationLivenessError::Expired {
            elapsed_ms: decision.expires_after_elapsed_ms + 1,
            allowed_ms: decision.expires_after_elapsed_ms,
        })
    );

    // First commit lands; the second commit of the same decision is refused.
    let mut ledger = FederationCommitLedger::new(decision.restart_epoch);
    let now = decision.issued_monotonic_elapsed_ms + 1;
    ledger.commit(&decision, now).expect("first commit");
    assert_eq!(
        ledger.commit(&decision, now),
        Err(FederationLivenessError::AlreadyCommitted(
            decision.identity.clone()
        ))
    );

    // A process restart invalidates every pending decision of the old epoch.
    let mut restarted = FederationCommitLedger::new(decision.restart_epoch);
    restarted.restart(decision.restart_epoch + 1);
    assert_eq!(
        restarted.commit(&decision, now),
        Err(FederationLivenessError::RestartEpochStale {
            decision_epoch: decision.restart_epoch,
            current_epoch: decision.restart_epoch + 1,
        })
    );
}
