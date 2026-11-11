use rightkit_agent_telemetry::{
    MeasurementObservation, MeasurementReceipt, MeasurementScenario,
};

/// Frozen scenario manifest for FM-189/S:P7. Driving workers, cancellation,
/// restart, and host gauges remains an external runner concern.
pub(crate) fn long_run_scenarios() -> &'static [MeasurementScenario; 6] {
    &MeasurementScenario::ALL
}

/// Issue an integrity-bound receipt from observed daemon counters.
pub(crate) fn measurement_receipt(
    run_id: impl Into<String>,
    observation: MeasurementObservation,
) -> Result<MeasurementReceipt, rightkit_agent_telemetry::MeasurementError> {
    MeasurementReceipt::issue(run_id, observation)
}

/// S:P10 -- render observed long-run measurements into the exact Markdown
/// table shape `docs/pending/performance.md` publishes. This function is the whole
/// mechanism: it never estimates or fabricates a cell. A scenario with no
/// matching observation, or an observation whose gauge is genuinely absent,
/// renders `not measured` -- identical to the placeholder the doc ships with
/// today -- rather than a computed guess. Publishing real numbers still
/// requires a real run; this only prevents the plumbing itself from lying.
pub(crate) fn render_performance_table(observations: &[MeasurementObservation]) -> String {
    const NOT_MEASURED: &str = "not measured";

    let mut lines = vec![
        "| Scenario | RSS | wakeups/s | open SSE streams | turn p50 | turn p95 | round-trips/task |".to_string(),
        "|---|---|---|---|---|---|---|".to_string(),
    ];

    for scenario in MeasurementScenario::ALL {
        let row = observations.iter().find(|o| o.scenario == scenario);
        let cells = match row {
            None => std::array::from_fn(|_| NOT_MEASURED.to_string()),
            Some(obs) => [
                render_gauge(
                    obs.host
                        .rss_bytes
                        .map(|b| format!("{} MiB", b / (1024 * 1024))),
                ),
                render_gauge(obs.resource.wakeups_per_second.map(|w| format!("{w:.1}"))),
                render_gauge(obs.host.open_sse_streams.map(|n| n.to_string())),
                render_gauge(percentile_ms(&obs.resource.latency_samples, 0.50)),
                render_gauge(percentile_ms(&obs.resource.latency_samples, 0.95)),
                render_gauge(round_trips_per_task(&obs.resource.round_trips)),
            ],
        };
        lines.push(format!(
            "| {} | {} | {} | {} | {} | {} | {} |",
            scenario.label(),
            cells[0],
            cells[1],
            cells[2],
            cells[3],
            cells[4],
            cells[5],
        ));
    }
    lines.join("\n")
}

fn render_gauge(value: Option<String>) -> String {
    value.unwrap_or_else(|| "not measured".to_string())
}

/// Nearest-rank percentile over recorded turn latencies, in milliseconds.
/// `None` when no samples were recorded -- an empty vec is a genuine absence
/// of data, not a zero-latency measurement.
fn percentile_ms(samples_ms: &[u64], p: f64) -> Option<String> {
    if samples_ms.is_empty() {
        return None;
    }
    let mut sorted = samples_ms.to_vec();
    sorted.sort_unstable();
    let rank = ((p * sorted.len() as f64).ceil() as usize)
        .saturating_sub(1)
        .min(sorted.len() - 1);
    Some(format!("{}ms", sorted[rank]))
}

/// Mean round-trips per named task. `None` when no round-trip counters were
/// recorded for this scenario.
fn round_trips_per_task(round_trips: &[(String, u64)]) -> Option<String> {
    if round_trips.is_empty() {
        return None;
    }
    let total: u64 = round_trips.iter().map(|(_, count)| count).sum();
    Some(format!("{:.1}", total as f64 / round_trips.len() as f64))
}
