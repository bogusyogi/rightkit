//! Bounded long-run observation harness for FM-189 and the S:P measurement
//! surfaces.
//!
//! This module is intentionally an observation harness, not a fake workload.
//! A caller drives real daemon work and submits timestamps, host gauges, and
//! worker counters. Resource counters are captured from the telemetry crate;
//! values that the caller cannot observe remain absent in the resulting
//! receipt. The module is registered by `api_benchmarks/mod.rs` by the
//! integration owner.

use rightkit_agent_telemetry::{
    MeasurementError, MeasurementObservation, MeasurementReceipt, MeasurementScenario,
};
use serde::{Deserialize, Serialize};

use crate::api_health::MachineStatus;

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
        if run_id.len() > rightkit_agent_telemetry::measurement::MAX_RUN_ID_BYTES {
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
            schema_version: crate::host_identity::schema("long-run-soak.v1"),
            run_id: self.config.run_id,
            status: self.status,
            receipts: self.receipts,
            observed_receipts,
        }
    }
}
