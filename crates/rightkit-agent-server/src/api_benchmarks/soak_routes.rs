//! Live runtime seam for the soak harness (FM-189, S:P7, S:P8, S:P10).
//!
//! The harness itself is deliberately passive: it never sleeps, spawns, or
//! invents a duration. This module is the process-level runner the harness
//! doc refers to -- it supplies the real wall-clock window and the real host
//! gauges of the running daemon, so a receipt records observed values rather
//! than an estimate.
//!
//! Receipts produced here are the runtime evidence the canon's `EVIDENCE`
//! atoms require; `render_performance_markdown` turns them into the table in
//! docs/pending/performance.md. A scenario with no observable value is rendered
//! "not measured" -- never a fabricated number.

use axum::{extract::State, response::Json, routing::post, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use rightkit_logs::agent_telemetry::measurement::{HostGauges, MeasurementScenario};

use super::soak_driver::{drive_soak, SoakDriveConfig};
use super::soak_harness::{SoakConfig, SoakHarness};
use crate::api::{now_millis, DaemonState};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SoakRunRequest {
    pub(crate) run_id: String,
    /// Scenario labels ("S0", "S1", "S10", "S50", "S100", "S500"). Omitted or
    /// empty means every scenario the harness knows.
    #[serde(default)]
    pub(crate) scenarios: Vec<String>,
    /// Counters supplied by the real worker runner. Missing values remain
    /// unknown in receipts instead of being inferred by this transport seam.
    #[serde(default)]
    pub(crate) cancelled_workers: Option<u64>,
    #[serde(default)]
    pub(crate) completed_workers: Option<u64>,
    #[serde(default)]
    pub(crate) ownership_violations: Option<u64>,
    /// When set, this route drives a real bounded worker soak (FM-189)
    /// instead of accepting the three counters above verbatim: it spawns,
    /// completes, and cancels real `LiveSession` worker trees through
    /// `soak_driver::drive_soak` and uses the measured counts in every
    /// scenario's receipt. `cancelled_workers`/`completed_workers`/
    /// `ownership_violations` above are ignored when this is present.
    #[serde(default)]
    pub(crate) drive_real_workers: Option<RealWorkerDriveRequest>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RealWorkerDriveRequest {
    pub(crate) worker_count: usize,
    /// Bounded window in seconds. Short (a handful of seconds) for
    /// integration acceptance, long (hours) for a real soak run -- this is
    /// the parameterizable duration the closure requirement calls for.
    pub(crate) duration_secs: u64,
    pub(crate) workspace_root: String,
}

fn scenario_from_label(label: &str) -> Option<MeasurementScenario> {
    MeasurementScenario::ALL
        .into_iter()
        .find(|scenario| scenario.label() == label)
}

/// Read the live process's own resource gauges. Every field is `None` when the
/// platform cannot answer it -- an unknown gauge must stay unknown so the
/// receipt is honest about what was actually observed.
fn observe_host_gauges(state: &DaemonState) -> HostGauges {
    let sessions = crate::api::lock_recover(&state.sessions, "state.sessions");
    let open_sse_streams: u64 = sessions
        .values()
        .map(|session| session.conduit.subscriber_count() as u64)
        .sum();
    HostGauges {
        rss_bytes: process_rss_bytes(),
        cpu_millis: None,
        fd_count: None,
        open_sse_streams: Some(open_sse_streams),
        active_inference: None,
        queued_inference: None,
        active_workers: None,
        parked_workers: None,
    }
}

#[cfg(target_os = "linux")]
fn process_rss_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb * 1024);
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
fn process_rss_bytes() -> Option<u64> {
    // No portable stable API; leave it unmeasured rather than guess.
    None
}

async fn run_soak_handler(
    State(state): State<DaemonState>,
    Json(request): Json<SoakRunRequest>,
) -> Json<Value> {
    let started_at = now_millis() as u64;

    let config = if request.scenarios.is_empty() {
        SoakConfig::new(request.run_id.clone())
    } else {
        let mut scenarios = Vec::new();
        for label in &request.scenarios {
            match scenario_from_label(label) {
                Some(scenario) => scenarios.push(scenario),
                None => {
                    return Json(json!({
                        "error": format!("unknown scenario `{label}`"),
                    }))
                }
            }
        }
        SoakConfig::with_scenarios(request.run_id.clone(), scenarios)
    };

    let config = match config {
        Ok(config) => config,
        Err(error) => return Json(json!({ "error": format!("{error:?}") })),
    };

    // A real worker drive replaces the caller-supplied passthrough counters
    // with values measured from actual runtime state (FM-189). Without it,
    // the three counters remain caller-supplied -- unchanged from before, so
    // existing callers of this route are unaffected.
    let (cancelled_workers, completed_workers, ownership_violations) =
        match &request.drive_real_workers {
            Some(drive) => {
                // The drive awaits the whole bounded window (seconds in tests,
                // hours for a real soak) without occupying a worker thread.
                let state_for_drive = state.clone();
                let drive_config = SoakDriveConfig {
                    worker_count: drive.worker_count,
                    duration: std::time::Duration::from_secs(drive.duration_secs),
                    workspace_root: drive.workspace_root.clone(),
                };
                let outcome = drive_soak(&state_for_drive, &drive_config).await;
                (
                    Some(outcome.cancelled_workers),
                    Some(outcome.completed_workers),
                    Some(outcome.ownership_violations),
                )
            }
            None => (
                request.cancelled_workers,
                request.completed_workers,
                request.ownership_violations,
            ),
        };

    let scenarios = config.scenarios.clone();
    let mut harness = SoakHarness::new(config);
    let host = observe_host_gauges(&state);

    for scenario in scenarios {
        let finished_at = now_millis() as u64;
        if let Err(error) = harness.capture(
            scenario,
            started_at,
            finished_at.max(started_at + 1),
            host.clone(),
            cancelled_workers,
            completed_workers,
            ownership_violations,
        ) {
            return Json(json!({ "error": format!("{error:?}") }));
        }
    }

    let report = harness.finish();
    // Publication accepts only receipts that still verify against retained
    // observations. This keeps both JSON receipts and rendered table rows
    // bound to an integrity-valid digest.
    let valid_receipts: Vec<_> = report
        .receipts
        .iter()
        .filter(|receipt| receipt.has_valid_digest())
        .collect();
    let observed_receipts = valid_receipts
        .iter()
        .filter(|receipt| receipt.observed)
        .count();
    Json(json!({
        "schemaVersion": report.schema_version,
        "runId": report.run_id,
        "status": format!("{:?}", report.status),
        "observedReceipts": observed_receipts,
        "receiptCount": valid_receipts.len(),
        "receipts": valid_receipts,
        "performanceMarkdown":
            rightkit_logs::agent_telemetry::measurement::MeasurementReceipt::render_performance_markdown(
                &report.receipts,
            ),
    }))
}

pub(crate) fn soak_routes() -> Router<DaemonState> {
    Router::new().route("/runtime/soak", post(run_soak_handler))
}
