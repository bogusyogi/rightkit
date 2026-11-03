use rightkit_logs::agent_telemetry::{
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
) -> Result<MeasurementReceipt, rightkit_logs::agent_telemetry::MeasurementError> {
    MeasurementReceipt::issue(run_id, observation)
}

#[cfg(test)]
mod measurement_unit_tests {
    use super::*;
    use rightkit_logs::agent_telemetry::{HostGauges, MeasurementError, ResourceMeasurement};

    fn observation(
        scenario: MeasurementScenario,
        start: u64,
        finish: u64,
    ) -> MeasurementObservation {
        MeasurementObservation {
            scenario,
            started_at_unix_ms: start,
            finished_at_unix_ms: finish,
            resource: ResourceMeasurement {
                latency_samples: vec![1, 2, 3],
                round_trips: vec![("provider".to_string(), 4)],
                dropped_rows: 0,
                wakeups_total: 5,
                wakeups_per_second: Some(0.5),
            },
            host: HostGauges::default(),
            cancelled_workers: None,
            completed_workers: None,
            ownership_violations: None,
        }
    }

    #[test]
    fn long_run_scenarios_covers_the_frozen_manifest_in_order() {
        let scenarios = long_run_scenarios();
        assert_eq!(scenarios, &MeasurementScenario::ALL);
        assert_eq!(scenarios.len(), 6);
        assert_eq!(scenarios[0], MeasurementScenario::S0);
        assert_eq!(scenarios[5], MeasurementScenario::S500);
    }

    #[test]
    fn measurement_receipt_issues_for_a_valid_observation() {
        let receipt = measurement_receipt("run-1", observation(MeasurementScenario::S10, 100, 200))
            .expect("valid run id and window must issue a receipt");
        assert_eq!(receipt.run_id, "run-1");
        assert_eq!(receipt.scenario, MeasurementScenario::S10);
        assert!(receipt.receipt_digest.starts_with("sha256:"));
    }

    #[test]
    fn measurement_receipt_rejects_empty_run_id() {
        let result = measurement_receipt("   ", observation(MeasurementScenario::S0, 0, 0));
        assert_eq!(result.unwrap_err(), MeasurementError::EmptyRunId);
    }

    #[test]
    fn measurement_receipt_rejects_inverted_time_window() {
        let result = measurement_receipt("run-2", observation(MeasurementScenario::S0, 200, 100));
        assert_eq!(result.unwrap_err(), MeasurementError::InvalidWindow);
    }
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

#[cfg(test)]
mod render_performance_table_tests {
    use super::*;
    use rightkit_logs::agent_telemetry::{HostGauges, ResourceMeasurement};

    fn observation(scenario: MeasurementScenario) -> MeasurementObservation {
        MeasurementObservation {
            scenario,
            started_at_unix_ms: 0,
            finished_at_unix_ms: 1,
            resource: ResourceMeasurement {
                latency_samples: vec![],
                round_trips: vec![],
                dropped_rows: 0,
                wakeups_total: 0,
                wakeups_per_second: None,
            },
            host: HostGauges::default(),
            cancelled_workers: None,
            completed_workers: None,
            ownership_violations: None,
        }
    }

    #[test]
    fn scenario_with_no_observation_renders_not_measured() {
        let table = render_performance_table(&[]);
        // Every scenario row must appear, all cells not-measured.
        for scenario in MeasurementScenario::ALL {
            let row = table
                .lines()
                .find(|l| l.starts_with(&format!("| {}", scenario.label())))
                .unwrap_or_else(|| panic!("missing row for {}", scenario.label()));
            assert_eq!(
                row.matches("not measured").count(),
                6,
                "row for {} must be fully unmeasured: {row}",
                scenario.label()
            );
        }
    }

    #[test]
    fn observation_with_absent_gauges_renders_not_measured_for_those_cells_only() {
        // rss_bytes present, everything else absent.
        let mut obs = observation(MeasurementScenario::S1);
        obs.host.rss_bytes = Some(512 * 1024 * 1024);
        let table = render_performance_table(&[obs]);
        let row = table.lines().find(|l| l.starts_with("| S1")).unwrap();
        assert!(row.contains("512 MiB"), "row: {row}");
        assert_eq!(
            row.matches("not measured").count(),
            5,
            "only the populated gauge should differ from not-measured: {row}"
        );
    }

    #[test]
    fn latency_percentiles_use_nearest_rank_not_fabricated_interpolation() {
        let mut obs = observation(MeasurementScenario::S10);
        obs.resource.latency_samples = vec![100, 200, 300, 400, 500, 600, 700, 800, 900, 1000];
        let table = render_performance_table(&[obs]);
        let row = table.lines().find(|l| l.starts_with("| S10")).unwrap();
        // p50 of 10 sorted samples (nearest-rank, ceil(0.50*10)-1 = 4) -> 500ms.
        assert!(row.contains("500ms"), "p50 cell: {row}");
        // p95 (ceil(0.95*10)-1 = 9) -> 1000ms.
        assert!(row.contains("1000ms"), "p95 cell: {row}");
    }

    #[test]
    fn round_trips_per_task_averages_across_named_tasks() {
        let mut obs = observation(MeasurementScenario::S50);
        obs.resource.round_trips = vec![("task-a".into(), 4), ("task-b".into(), 6)];
        let table = render_performance_table(&[obs]);
        let row = table.lines().find(|l| l.starts_with("| S50")).unwrap();
        assert!(row.contains(" 5.0 "), "row: {row}");
    }

    #[test]
    fn table_header_and_row_count_match_the_published_shape() {
        let table = render_performance_table(&[]);
        let lines: Vec<&str> = table.lines().collect();
        assert_eq!(
            lines[0],
            "| Scenario | RSS | wakeups/s | open SSE streams | turn p50 | turn p95 | round-trips/task |"
        );
        assert_eq!(lines[1], "|---|---|---|---|---|---|---|");
        assert_eq!(lines.len(), 2 + MeasurementScenario::ALL.len());
    }
}
