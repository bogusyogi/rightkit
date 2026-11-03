//! Bounded long-run observation harness for FM-189 and the S:P measurement
//! surfaces.
//!
//! This module is intentionally an observation harness, not a fake workload.
//! A caller drives real daemon work and submits timestamps, host gauges, and
//! worker counters. Resource counters are captured from the telemetry crate;
//! values that the caller cannot observe remain absent in the resulting
//! receipt. The module is registered by `api_benchmarks/mod.rs` by the
//! integration owner.

use rightkit_logs::agent_telemetry::{
    HostGauges, MeasurementError, MeasurementObservation, MeasurementReceipt, MeasurementScenario,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::api_health::{MachineStatus, MACHINE_STATUS_SCHEMA};

const MAX_SOAK_OBSERVATIONS: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SoakError {
    EmptyRunId,
    RunIdTooLong,
    UnknownScenario(MeasurementScenario),
    DuplicateScenario(MeasurementScenario),
    ObservationLimit,
    Measurement(MeasurementError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SoakConfig {
    pub(crate) run_id: String,
    pub(crate) scenarios: Vec<MeasurementScenario>,
    pub(crate) max_observations: usize,
}

impl SoakConfig {
    pub(crate) fn new(run_id: impl Into<String>) -> Result<Self, SoakError> {
        Self::with_scenarios(run_id, MeasurementScenario::ALL.into_iter().collect())
    }

    pub(crate) fn with_scenarios(
        run_id: impl Into<String>,
        scenarios: Vec<MeasurementScenario>,
    ) -> Result<Self, SoakError> {
        let run_id = run_id.into();
        if run_id.trim().is_empty() {
            return Err(SoakError::EmptyRunId);
        }
        if run_id.len() > rightkit_logs::agent_telemetry::measurement::MAX_RUN_ID_BYTES {
            return Err(SoakError::RunIdTooLong);
        }
        let mut accepted = Vec::with_capacity(scenarios.len());
        for scenario in scenarios {
            if !MeasurementScenario::ALL.contains(&scenario) {
                return Err(SoakError::UnknownScenario(scenario));
            }
            if accepted.contains(&scenario) {
                return Err(SoakError::DuplicateScenario(scenario));
            }
            accepted.push(scenario);
        }
        Ok(Self {
            run_id,
            scenarios: accepted,
            max_observations: MAX_SOAK_OBSERVATIONS,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SoakReport {
    pub(crate) schema_version: String,
    pub(crate) run_id: String,
    pub(crate) status: MachineStatus,
    pub(crate) receipts: Vec<MeasurementReceipt>,
    pub(crate) observed_receipts: usize,
}

/// In-memory bounded collector. It does not spawn threads, sleep, or invent a
/// duration; the process-level runner owns scheduling and cancellation.
pub(crate) struct SoakHarness {
    config: SoakConfig,
    status: MachineStatus,
    receipts: Vec<MeasurementReceipt>,
}

impl SoakHarness {
    pub(crate) fn new(config: SoakConfig) -> Self {
        Self {
            config,
            status: MachineStatus::Healthy,
            receipts: Vec::new(),
        }
    }

    pub(crate) fn status(&self) -> MachineStatus {
        self.status
    }

    pub(crate) fn set_status(&mut self, status: MachineStatus) {
        self.status = status;
    }

    /// Capture current process-global resource counters and issue one
    /// integrity-bound receipt. Host values are supplied by the live runner;
    /// no host value is inferred from the resource snapshot.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn capture(
        &mut self,
        scenario: MeasurementScenario,
        started_at_unix_ms: u64,
        finished_at_unix_ms: u64,
        host: HostGauges,
        cancelled_workers: Option<u64>,
        completed_workers: Option<u64>,
        ownership_violations: Option<u64>,
    ) -> Result<(), SoakError> {
        let observation = MeasurementObservation::capture_with_host(
            scenario,
            started_at_unix_ms,
            finished_at_unix_ms,
            host,
            cancelled_workers,
            completed_workers,
            ownership_violations,
        );
        self.record(observation)
    }

    pub(crate) fn record(&mut self, observation: MeasurementObservation) -> Result<(), SoakError> {
        if !self.config.scenarios.contains(&observation.scenario) {
            return Err(SoakError::UnknownScenario(observation.scenario));
        }
        if self
            .receipts
            .iter()
            .any(|receipt| receipt.scenario == observation.scenario)
        {
            return Err(SoakError::DuplicateScenario(observation.scenario));
        }
        if self.receipts.len() >= self.config.max_observations {
            return Err(SoakError::ObservationLimit);
        }
        let receipt = MeasurementReceipt::issue(self.config.run_id.clone(), observation)
            .map_err(SoakError::Measurement)?;
        self.receipts.push(receipt);
        Ok(())
    }

    pub(crate) fn stop(mut self) -> SoakReport {
        self.status = MachineStatus::Stopped;
        self.into_report()
    }

    pub(crate) fn finish(self) -> SoakReport {
        self.into_report()
    }

    fn into_report(self) -> SoakReport {
        let observed_receipts = self
            .receipts
            .iter()
            .filter(|receipt| receipt.observed)
            .count();
        SoakReport {
            schema_version: "coderight.long-run-soak.v1".to_string(),
            run_id: self.config.run_id,
            status: self.status,
            receipts: self.receipts,
            observed_receipts,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MachineConformanceError {
    NotObject,
    MissingSchemaVersion,
    WrongSchemaVersion,
    MissingStatus,
    UnknownStatus,
    MissingOk,
    StatusOkMismatch,
}

/// Validate the common, transport-independent portion of the machine status
/// envelope. This is executable conformance logic for every status class;
/// transport failures still cannot produce a daemon body and are represented
/// by the `unreachable` class at the client seam.
pub(crate) fn validate_machine_status_document(
    document: &Value,
) -> Result<(), MachineConformanceError> {
    let object = document
        .as_object()
        .ok_or(MachineConformanceError::NotObject)?;
    let schema = object
        .get("schemaVersion")
        .and_then(Value::as_str)
        .ok_or(MachineConformanceError::MissingSchemaVersion)?;
    if schema != MACHINE_STATUS_SCHEMA {
        return Err(MachineConformanceError::WrongSchemaVersion);
    }
    let status = object
        .get("status")
        .and_then(Value::as_str)
        .ok_or(MachineConformanceError::MissingStatus)?;
    let expected_ok = match status {
        "healthy" => true,
        "stopped" | "unreachable" | "degraded" => false,
        _ => return Err(MachineConformanceError::UnknownStatus),
    };
    let ok = object
        .get("ok")
        .and_then(Value::as_bool)
        .ok_or(MachineConformanceError::MissingOk)?;
    if ok != expected_ok {
        return Err(MachineConformanceError::StatusOkMismatch);
    }
    Ok(())
}

pub(crate) fn machine_status_document(status: MachineStatus) -> Value {
    serde_json::json!({
        "schemaVersion": MACHINE_STATUS_SCHEMA,
        "status": status,
        "ok": matches!(status, MachineStatus::Healthy),
    })
}
