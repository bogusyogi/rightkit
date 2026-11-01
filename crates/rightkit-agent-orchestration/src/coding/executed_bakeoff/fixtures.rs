use std::collections::BTreeMap;

use async_trait::async_trait;

use super::runner::BakeoffExecutor;
use super::types::{
    BakeoffError, EvidenceSource, ExecutionRecord, ExecutionRequest, ExecutionTopology,
    ExecutorDescriptor, StableTaskId,
};

#[derive(Debug, Clone, PartialEq)]
pub struct RecordedExecution {
    pub record: ExecutionRecord,
    pub recorded_e2e_latency_ms: u64,
}

/// Offline executor for replaying execution records captured elsewhere. Its
/// reports remain explicitly marked `recorded_fixture`, never live evidence.
#[derive(Debug, Clone)]
pub struct RecordedFixtureExecutor {
    descriptor: ExecutorDescriptor,
    fixtures: BTreeMap<StableTaskId, RecordedExecution>,
}

impl RecordedFixtureExecutor {
    pub fn new(
        name: impl Into<String>,
        topology: ExecutionTopology,
        fixtures: impl IntoIterator<Item = (StableTaskId, RecordedExecution)>,
    ) -> Self {
        Self {
            descriptor: ExecutorDescriptor {
                name: name.into(),
                topology,
                evidence_source: EvidenceSource::RecordedFixture,
            },
            fixtures: fixtures.into_iter().collect(),
        }
    }
}

#[async_trait]
impl BakeoffExecutor for RecordedFixtureExecutor {
    fn descriptor(&self) -> &ExecutorDescriptor {
        &self.descriptor
    }

    async fn execute(&self, request: &ExecutionRequest) -> Result<ExecutionRecord, BakeoffError> {
        self.fixtures
            .get(&request.task_id)
            .map(|fixture| fixture.record.clone())
            .ok_or_else(|| BakeoffError::MissingFixture(request.task_id.as_str().to_string()))
    }

    fn recorded_latency_ms(&self, task_id: &StableTaskId) -> Option<u64> {
        self.fixtures
            .get(task_id)
            .map(|fixture| fixture.recorded_e2e_latency_ms)
    }
}
