/// A daily background analysis task that feeds into the learning harness.
pub fn spawn_daily_analyzer(state: std::sync::Arc<crate::api::DaemonState>) {
    tokio::spawn(async move {
        tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(86_400));
        loop {
            interval.tick().await;
            run_daily_analysis(&state).await;
        }
    });
}

async fn run_daily_analysis(state: &crate::api::DaemonState) {
    let started_at = crate::api::now_iso();
    let source = DaemonAnalyzerSource { state };
    let report = DailyAnalyzer::new().analyze(&source);
    let proposals_minted = mint_review_proposals(&state.learning, &report);
    let (written, error) =
        match consolidate_daily_memory(state.memory_backend.as_ref(), &report).await {
            Ok(written) => (written, None),
            Err(error) => (0, Some(error)),
        };

    // FM-074 roster-status projection: every tick of the consolidation cycle
    // records its outcome so a status/roster surface can report it, instead
    // of the cycle running invisibly in the background. `memory_consolidation_status`
    // is a `rightkit_memory::ConsolidationStatusState` field the orchestrator
    // adds to `DaemonState` (see the A11 wiring doc — this lane cannot edit
    // `DaemonState` itself).
    state
        .memory_consolidation_status
        .record_run(rightkit_memory::ConsolidationRunOutcome {
            started_at,
            completed_at: crate::api::now_iso(),
            review_candidates_seen: report.review_candidates.len() as u64,
            memories_written: written as u64,
            repeated_failure_patterns: report.repeated_failures.len() as u64,
            proposals_minted,
            error,
        });
}

/// Turn a daily report's review candidates into learning proposals, deduped by
/// a stable `daily-analyzer-<session>` id so re-running the analysis is
/// idempotent. Pure over `(harness, report)` so it is unit-testable without a
/// full `DaemonState`.
fn mint_review_proposals(harness: &LearningHarness, report: &DailyReport) -> u64 {
    let mut minted = 0u64;
    for candidate in &report.review_candidates {
        let proposal = review_candidate_to_proposal(candidate);
        let is_new = harness.proposal(&proposal.id).is_none();
        harness.add_proposal_if_absent(proposal);
        if is_new {
            minted = minted.saturating_add(1);
        }
    }
    minted
}

fn review_candidate_to_proposal(candidate: &ReviewCandidate) -> Proposal {
    let kind = match candidate.suggested_proposal_kind.as_str() {
        "HarnessRule" => ProposalKind::HarnessRule,
        "Workflow" => ProposalKind::Workflow,
        _ => ProposalKind::Memory,
    };
    Proposal {
        id: format!("daily-analyzer-{}", candidate.session_id),
        kind,
        summary: format!(
            "[daily-analyzer] {}: {}",
            candidate.session_id, candidate.reason
        ),
        evidence: String::new(),
        score: 0.5,
        status: ProposalStatus::Candidate,
        created_at: crate::api::now_iso(),
        age_sessions: 0,
        rejection_reason: None,
    }
}

/// Consolidate daily analyzer output into durable memory without waiting for a
/// manual proposal approval. This is intentionally deterministic and bounded:
/// memory candidates and repeated failure patterns become stable semantic
/// memories, while workflow/harness-rule candidates remain approval-gated.
async fn consolidate_daily_memory(
    memory: &dyn rightkit_memory::backend::MemoryBackend,
    report: &DailyReport,
) -> Result<usize, String> {
    let mut written = 0usize;
    for candidate in &report.review_candidates {
        if candidate.suggested_proposal_kind != "Memory" {
            continue;
        }
        let content = format!(
            "Daily analyzer memory for session {}: {}. Trajectory: {}",
            candidate.session_id, candidate.reason, candidate.trajectory_excerpt
        );
        let context =
            rightkit_memory::backend::CallContext::after(std::time::Duration::from_secs(5));
        if memory
            .remember_consolidated(
                &context,
                &format!("daily-analyzer-memory-{}", candidate.session_id),
                &content,
                vec![
                    "daily-analyzer".to_string(),
                    "session-memory".to_string(),
                    candidate.session_id.to_lowercase(),
                ],
                0.7,
            )
            .await
            .map_err(|error| error.to_string())?
            .is_some()
        {
            written = written.saturating_add(1);
        }
    }

    for (index, (error, count)) in report.repeated_failures.iter().enumerate() {
        let content = format!(
            "Repeated failure pattern observed by daily analyzer: '{error}' appeared {count} times across recent sessions."
        );
        let context =
            rightkit_memory::backend::CallContext::after(std::time::Duration::from_secs(5));
        if memory
            .remember_consolidated(
                &context,
                &format!(
                    "daily-analyzer-repeated-failure-{}-{}",
                    index,
                    stable_slug(error)
                ),
                &content,
                vec![
                    "daily-analyzer".to_string(),
                    "repeated-failure".to_string(),
                    stable_slug(error),
                ],
                0.85,
            )
            .await
            .map_err(|error| error.to_string())?
            .is_some()
        {
            written = written.saturating_add(1);
        }
    }
    Ok(written)
}

fn stable_slug(value: &str) -> String {
    let slug = value
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .take(8)
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() {
        "unknown".to_string()
    } else {
        slug.chars().take(80).collect()
    }
}
