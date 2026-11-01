//! Bounded long-run measurement records.
//!
//! This module is an observation boundary: it snapshots existing resource
//! counters and carries host-only gauges as optional values. Missing gauges
//! remain missing; this type never manufactures a measurement.

use crate::agent_telemetry::resource::snapshot_resource_telemetry;
use serde::{Deserialize, Serialize};

pub const MEASUREMENT_SCHEMA: &str = "coderight.long-run-measurement.v1";
pub const MAX_RUN_ID_BYTES: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeasurementScenario {
    S0,
    S1,
    S10,
    S50,
    S100,
    S500,
}

impl MeasurementScenario {
    pub const ALL: [Self; 6] = [
        Self::S0,
        Self::S1,
        Self::S10,
        Self::S50,
        Self::S100,
        Self::S500,
    ];
    pub const fn label(self) -> &'static str {
        match self {
            Self::S0 => "S0",
            Self::S1 => "S1",
            Self::S10 => "S10",
            Self::S50 => "S50",
            Self::S100 => "S100",
            Self::S500 => "S500",
        }
    }
}

/// Host gauges are supplied by daemon/runner, never estimated here.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostGauges {
    pub rss_bytes: Option<u64>,
    pub cpu_millis: Option<u64>,
    pub fd_count: Option<u64>,
    pub open_sse_streams: Option<u64>,
    pub active_inference: Option<u64>,
    pub queued_inference: Option<u64>,
    pub active_workers: Option<u64>,
    pub parked_workers: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceMeasurement {
    pub latency_samples: Vec<u64>,
    pub round_trips: Vec<(String, u64)>,
    pub dropped_rows: u64,
    pub wakeups_total: u64,
    pub wakeups_per_second: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MeasurementObservation {
    pub scenario: MeasurementScenario,
    pub started_at_unix_ms: u64,
    pub finished_at_unix_ms: u64,
    pub resource: ResourceMeasurement,
    pub host: HostGauges,
    pub cancelled_workers: Option<u64>,
    pub completed_workers: Option<u64>,
    pub ownership_violations: Option<u64>,
}

impl MeasurementObservation {
    pub fn capture(
        scenario: MeasurementScenario,
        started_at_unix_ms: u64,
        finished_at_unix_ms: u64,
    ) -> Self {
        Self::capture_with_host(
            scenario,
            started_at_unix_ms,
            finished_at_unix_ms,
            HostGauges::default(),
            None,
            None,
            None,
        )
    }

    /// Capture the process-global resource snapshot and attach only gauges
    /// supplied by the live runner. A missing gauge remains `None`; this
    /// constructor never turns an unavailable host value into zero.
    #[allow(clippy::too_many_arguments)]
    pub fn capture_with_host(
        scenario: MeasurementScenario,
        started_at_unix_ms: u64,
        finished_at_unix_ms: u64,
        host: HostGauges,
        cancelled_workers: Option<u64>,
        completed_workers: Option<u64>,
        ownership_violations: Option<u64>,
    ) -> Self {
        let snapshot = snapshot_resource_telemetry();
        Self {
            scenario,
            started_at_unix_ms,
            finished_at_unix_ms,
            resource: ResourceMeasurement {
                latency_samples: snapshot.latency_samples,
                round_trips: snapshot.round_trips,
                dropped_rows: snapshot.dropped_rows,
                wakeups_total: snapshot.wakeups_total,
                wakeups_per_second: snapshot.wakeups_per_second,
            },
            host,
            cancelled_workers,
            completed_workers,
            ownership_violations,
        }
    }

    pub fn has_observed_value(&self) -> bool {
        !self.resource.latency_samples.is_empty()
            || !self.resource.round_trips.is_empty()
            || self.resource.wakeups_total > 0
            || self.host != HostGauges::default()
            || self.cancelled_workers.is_some()
            || self.completed_workers.is_some()
            || self.ownership_violations.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MeasurementReceipt {
    pub schema: String,
    pub run_id: String,
    pub scenario: MeasurementScenario,
    pub observation: MeasurementObservation,
    pub observed: bool,
    pub receipt_digest: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeasurementError {
    EmptyRunId,
    RunIdTooLong,
    InvalidWindow,
}

impl MeasurementReceipt {
    pub fn issue(
        run_id: impl Into<String>,
        observation: MeasurementObservation,
    ) -> Result<Self, MeasurementError> {
        let run_id = run_id.into();
        if run_id.trim().is_empty() {
            return Err(MeasurementError::EmptyRunId);
        }
        if run_id.len() > MAX_RUN_ID_BYTES {
            return Err(MeasurementError::RunIdTooLong);
        }
        if observation.finished_at_unix_ms < observation.started_at_unix_ms {
            return Err(MeasurementError::InvalidWindow);
        }
        let material = format!("schema={MEASUREMENT_SCHEMA};run={run_id};scenario={};start={};finish={};latencies={:?};round_trips={:?};dropped={};wakeups={};host={:?};cancelled={:?};completed={:?};ownership={:?}", observation.scenario.label(), observation.started_at_unix_ms, observation.finished_at_unix_ms, observation.resource.latency_samples, observation.resource.round_trips, observation.resource.dropped_rows, observation.resource.wakeups_total, observation.host, observation.cancelled_workers, observation.completed_workers, observation.ownership_violations);
        let mut digest = rightkit_fs::digest::Sha256Hasher::new();
        digest.update(material.as_bytes());
        let receipt_digest = format!("sha256:{}", digest.finalize());
        let observed = observation.has_observed_value();
        Ok(Self {
            schema: MEASUREMENT_SCHEMA.into(),
            run_id,
            scenario: observation.scenario,
            observation,
            observed,
            receipt_digest,
        })
    }

    /// Recompute the digest from the retained observation. This is a local
    /// integrity check only; it does not establish that the observation came
    /// from a live daemon run.
    pub fn has_valid_digest(&self) -> bool {
        Self::issue(self.run_id.clone(), self.observation.clone())
            .map(|rebuilt| {
                rebuilt.schema == self.schema
                    && rebuilt.scenario == self.scenario
                    && rebuilt.observed == self.observed
                    && rebuilt.receipt_digest == self.receipt_digest
            })
            .unwrap_or(false)
    }

    /// Render only integrity-valid, observed receipts in the published
    /// performance-table shape. Every measured row carries its receipt digest
    /// so each published figure is explicitly bound to its evidence. Missing
    /// scenarios and missing gauges render as `not measured`; this function
    /// never fills a cell with an estimate.
    pub fn render_performance_markdown(receipts: &[Self]) -> String {
        let mut lines = vec![
            "| Scenario | RSS | wakeups/s | open SSE streams | turn p50 | turn p95 | round-trips/task | receipt digest |".to_string(),
            "|---|---|---|---|---|---|---|---|".to_string(),
        ];
        for scenario in MeasurementScenario::ALL {
            let receipt = receipts.iter().find(|receipt| {
                receipt.scenario == scenario && receipt.observed && receipt.has_valid_digest()
            });
            let cells = receipt
                .map(|receipt| {
                    let observation = &receipt.observation;
                    [
                        format_optional(
                            observation
                                .host
                                .rss_bytes
                                .map(|bytes| format!("{} MiB", bytes / (1024 * 1024))),
                        ),
                        format_optional(
                            observation
                                .resource
                                .wakeups_per_second
                                .map(|value| format!("{value:.1}")),
                        ),
                        format_optional(
                            observation
                                .host
                                .open_sse_streams
                                .map(|value| value.to_string()),
                        ),
                        format_optional(nearest_rank_ms(&observation.resource.latency_samples, 50)),
                        format_optional(nearest_rank_ms(&observation.resource.latency_samples, 95)),
                        format_optional(mean_round_trips(&observation.resource.round_trips)),
                        receipt.receipt_digest.clone(),
                    ]
                })
                .unwrap_or_else(|| std::array::from_fn(|_| "not measured".to_string()));
            lines.push(format!(
                "| {} | {} | {} | {} | {} | {} | {} | {} |",
                scenario.label(),
                cells[0],
                cells[1],
                cells[2],
                cells[3],
                cells[4],
                cells[5],
                cells[6]
            ));
        }
        lines.join("\n")
    }
}

fn format_optional(value: Option<String>) -> String {
    value.unwrap_or_else(|| "not measured".to_string())
}

fn nearest_rank_ms(samples: &[u64], percentile: usize) -> Option<String> {
    if samples.is_empty() {
        return None;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let rank = (percentile * sorted.len()).div_ceil(100);
    Some(format!(
        "{}ms",
        sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
    ))
}

fn mean_round_trips(rows: &[(String, u64)]) -> Option<String> {
    if rows.is_empty() {
        return None;
    }
    let total: u64 = rows.iter().map(|(_, count)| *count).sum();
    Some(format!("{:.1}", total as f64 / rows.len() as f64))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation() -> MeasurementObservation {
        MeasurementObservation {
            scenario: MeasurementScenario::S10,
            started_at_unix_ms: 10,
            finished_at_unix_ms: 20,
            resource: ResourceMeasurement {
                latency_samples: vec![4, 8],
                round_trips: vec![("task-a".to_string(), 1)],
                dropped_rows: 0,
                wakeups_total: 2,
                wakeups_per_second: Some(1.0),
            },
            host: HostGauges {
                rss_bytes: Some(1024),
                ..HostGauges::default()
            },
            cancelled_workers: Some(0),
            completed_workers: Some(1),
            ownership_violations: Some(0),
        }
    }

    #[test]
    fn scenario_manifest_is_complete_and_ordered() {
        assert_eq!(MeasurementScenario::ALL[0], MeasurementScenario::S0);
        assert_eq!(MeasurementScenario::ALL[5], MeasurementScenario::S500);
        assert_eq!(MeasurementScenario::S100.label(), "S100");
    }

    #[test]
    fn receipt_digest_recomputes_without_inventing_measurements() {
        let receipt = MeasurementReceipt::issue("run-1", observation()).unwrap();
        assert!(receipt.observed);
        assert!(receipt.has_valid_digest());
        assert!(receipt.receipt_digest.starts_with("sha256:"));
    }

    #[test]
    fn invalid_windows_are_rejected_before_a_receipt_is_issued() {
        let mut invalid = observation();
        invalid.started_at_unix_ms = 21;
        assert_eq!(
            MeasurementReceipt::issue("run-1", invalid).unwrap_err(),
            MeasurementError::InvalidWindow
        );
    }

    #[test]
    fn performance_markdown_uses_only_observed_valid_receipts() {
        let receipt = MeasurementReceipt::issue("run-1", observation()).unwrap();
        let markdown = MeasurementReceipt::render_performance_markdown(&[receipt]);
        let row = markdown
            .lines()
            .find(|line| line.starts_with("| S10"))
            .unwrap();
        assert!(row.contains("4ms"));
        assert!(row.contains("8ms"));
        assert!(row.contains("not measured"));
        assert_eq!(markdown.lines().count(), 2 + MeasurementScenario::ALL.len());
    }
}
