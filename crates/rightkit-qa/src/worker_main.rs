//! Worker side of the runner <-> worker contract (ADR-0001 item 3, ruling D-g).
//!
//! The runner starts this executable as `rightkit-qa worker <world dir>` inside its Job Object or
//! process group (see [`crate::supervise`]). The worker waits for the runner's start line on stdin,
//! reads `scenario.json`, runs exactly that one scenario, writes `outcome.json` and exits. The
//! verdict lives in the outcome: a failed or errored scenario still exits 0. A non-zero exit means
//! the worker could not do its job at all (refused spec, no start line, no outcome written), and
//! the runner reads that as `errored`.
//!
//! The scenario itself is a closure the caller passes in (the pool's per-scenario function), so this
//! module depends on nothing in `suite.rs` or `scenario.rs`.
use crate::evidence::CheckStatus;
use crate::util::Result;
use crate::worker::{self, ScenarioSpec, WorkerOutcome, SCHEMA_VERSION};
use std::collections::BTreeMap;
use std::io::BufRead;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;

/// The `rightkit-qa` subcommand that selects worker mode.
pub const WORKER_SUBCOMMAND: &str = "worker";
/// The one line the runner writes to the worker's stdin once the worker is inside its containment.
/// The worker does nothing, and so creates no process, before it reads it; that closes the window
/// between spawn and Job Object assignment. End of input without it means the runner is gone.
pub const START_LINE: &str = "go";
/// The worker's stdout and stderr, written by the runner into the world directory.
pub const LOG_FILE: &str = "worker.log";

/// The worker finished its part: an outcome (of any status) is on disk.
pub const EXIT_OK: i32 = 0;
/// `scenario.json` was refused (wrong version, invalid, or it names another world directory).
pub const EXIT_REFUSED: i32 = 2;
/// The runner's start line never came.
pub const EXIT_NO_START: i32 = 3;
/// The scenario ran but no outcome could be written.
pub const EXIT_NO_OUTCOME: i32 = 4;

/// An outcome that reports `message` as an `errored` scenario: no checks, no timing, no survivors.
pub(crate) fn errored_outcome(spec: &ScenarioSpec, message: impl Into<String>) -> WorkerOutcome {
    WorkerOutcome {
        schema_version: SCHEMA_VERSION,
        run_id: spec.run_id.clone(),
        scenario_id: spec.scenario_id.clone(),
        status: CheckStatus::Errored,
        message: message.into(),
        engine_starts: 0,
        engine_starts_by_env: BTreeMap::new(),
        survivors: Vec::new(),
        timing: BTreeMap::new(),
        steps: Vec::new(),
        launch: None,
        checks: Vec::new(),
        artifacts: Vec::new(),
    }
}

/// Whether the first line of `input` is the runner's start line.
fn wait_for_start(mut input: impl BufRead) -> bool {
    let mut line = String::new();
    matches!(input.read_line(&mut line), Ok(n) if n > 0) && line.trim() == START_LINE
}

fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-text panic payload".to_string()
    }
}

/// Entry point of `rightkit-qa worker <world dir>`: wait for the runner's start line on stdin, then
/// [`run_in_world`]. Returns the process exit code.
pub fn main<F>(world_dir: &Path, run: F) -> i32
where
    F: FnOnce(&ScenarioSpec) -> Result<WorkerOutcome>,
{
    if !wait_for_start(std::io::stdin().lock()) {
        eprintln!(
            "rightkit-qa worker: no start line from the runner; exiting without running anything"
        );
        return EXIT_NO_START;
    }
    run_in_world(world_dir, run)
}

/// Read `scenario.json` from `world_dir`, run it through `run`, write `outcome.json`.
///
/// A spec that fails validation, or whose `world.dir` differs from `world_dir`, is refused before
/// `run` is called and nothing is written. An `Err` or a panic from `run` becomes an `errored`
/// outcome. An outcome the contract refuses (another run or scenario id, a pass with survivors,
/// inconsistent timing) is replaced by an `errored` one that says so, so a worker never reports a
/// verdict the runner would have to throw away silently.
pub fn run_in_world<F>(world_dir: &Path, run: F) -> i32
where
    F: FnOnce(&ScenarioSpec) -> Result<WorkerOutcome>,
{
    let spec = match worker::read_scenario(world_dir) {
        Ok(spec) => spec,
        Err(e) => {
            eprintln!("rightkit-qa worker: refused: {e}");
            return EXIT_REFUSED;
        }
    };
    let outcome = match catch_unwind(AssertUnwindSafe(|| run(&spec))) {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(e)) => errored_outcome(&spec, format!("the scenario could not run: {e}")),
        Err(payload) => errored_outcome(
            &spec,
            format!("the scenario panicked: {}", panic_text(payload.as_ref())),
        ),
    };
    match worker::write_outcome(world_dir, &spec, &outcome) {
        Ok(_) => EXIT_OK,
        Err(first) => {
            let fallback = errored_outcome(
                &spec,
                format!("the scenario's outcome was refused by the contract: {first}"),
            );
            match worker::write_outcome(world_dir, &spec, &fallback) {
                Ok(_) => EXIT_OK,
                Err(second) => {
                    eprintln!("rightkit-qa worker: cannot write the outcome: {second}");
                    EXIT_NO_OUTCOME
                }
            }
        }
    }
}
