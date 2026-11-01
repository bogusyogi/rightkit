use super::types::{
    BakeoffGateConfig, BakeoffGateDecision, BakeoffReport, EvidencePolicy, EvidenceSource,
};

/// Apply caller-owned promotion thresholds to either live or recorded
/// execution evidence. Missing optional measurements fail closed whenever a
/// threshold for that measurement is configured.
pub fn evaluate_bakeoff(report: &BakeoffReport, config: &BakeoffGateConfig) -> BakeoffGateDecision {
    let mut failures = Vec::new();
    if config.evidence_policy == EvidencePolicy::LiveOnly
        && (report.candidate.executor.evidence_source != EvidenceSource::LiveExecution
            || report
                .same_cost_single_agent_baseline
                .executor
                .evidence_source
                != EvidenceSource::LiveExecution)
    {
        failures
            .push("gate requires live execution evidence for candidate and baseline".to_string());
    }
    if config.require_same_cost_comparison && !report.comparison.same_cost_comparable {
        failures.push(format!(
            "single-agent baseline exceeded the same-cost limit on {} task(s)",
            report.comparison.baseline_budget_violations
        ));
    }

    let metrics = &report.candidate.metrics;
    check_minimum(
        &mut failures,
        "verified completion rate",
        Some(metrics.verified_completion_rate),
        config.minimum_verified_completion_rate,
    );
    check_minimum(
        &mut failures,
        "completion-rate delta versus baseline",
        Some(report.comparison.completion_rate_delta),
        config.minimum_completion_rate_delta_vs_baseline,
    );
    check_maximum(
        &mut failures,
        "escalation rate",
        metrics.escalation_rate,
        config.maximum_escalation_rate,
    );
    check_minimum(
        &mut failures,
        "tool-call efficiency",
        metrics.tool_call_efficiency,
        config.minimum_tool_call_efficiency,
    );
    check_maximum(
        &mut failures,
        "review false-positive rate",
        metrics.review_false_positive_rate,
        config.maximum_review_false_positive_rate,
    );
    check_maximum(
        &mut failures,
        "long-context degradation rate",
        metrics.long_context.degradation_rate,
        config.maximum_long_context_degradation_rate,
    );

    BakeoffGateDecision {
        passed: failures.is_empty(),
        failures,
    }
}

fn check_minimum(
    failures: &mut Vec<String>,
    name: &str,
    measured: Option<f64>,
    threshold: Option<f64>,
) {
    let Some(threshold) = threshold else {
        return;
    };
    match measured {
        Some(value) if value >= threshold => {}
        Some(value) => failures.push(format!(
            "{name} {value:.6} is below required minimum {threshold:.6}"
        )),
        None => failures.push(format!("{name} is unavailable")),
    }
}

fn check_maximum(
    failures: &mut Vec<String>,
    name: &str,
    measured: Option<f64>,
    threshold: Option<f64>,
) {
    let Some(threshold) = threshold else {
        return;
    };
    match measured {
        Some(value) if value <= threshold => {}
        Some(value) => failures.push(format!(
            "{name} {value:.6} exceeds allowed maximum {threshold:.6}"
        )),
        None => failures.push(format!("{name} is unavailable")),
    }
}
