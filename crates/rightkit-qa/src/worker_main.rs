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

/// Shared by this module's tests and `supervise`'s.
#[cfg(test)]
pub(crate) mod test_support {
    use crate::evidence::CheckStatus;
    use crate::worker::{
        Deadlines, EnvMarker, ScenarioSpec, Token, WorkerOutcome, World, SCHEMA_VERSION,
    };
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    /// A fresh run root under the temp directory, created on disk.
    pub fn run_root(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rkqa-wmain-{tag}-{}", crate::util::new_id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A valid spec whose world is `<root>/<name>`, created on disk.
    pub fn spec_in(root: &Path, name: &str, deadlines: Deadlines) -> ScenarioSpec {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        ScenarioSpec {
            schema_version: SCHEMA_VERSION,
            run_id: "run-1".into(),
            scenario_id: "supervised".into(),
            app: "fixture".into(),
            platform: "test".into(),
            run_root: root.to_path_buf(),
            world: World {
                data_dir: dir.join("data"),
                home: dir.join("home"),
                temp: dir.join("tmp"),
                port_start: 41000,
                port_end: 41015,
                control_socket: dir.join("control.sock"),
                control_token_file: dir.join("control.token"),
                env_marker: EnvMarker {
                    name: "RIGHTKIT_QA_WORLD".into(),
                    value: format!("w-{name}"),
                },
                dir,
            },
            scenario: json!({"id": "supervised", "steps": []}),
            grants: vec![Token::Focus],
            deadlines,
        }
    }

    pub fn deadlines(step_ms: u64, scenario_ms: u64, teardown_ms: u64) -> Deadlines {
        Deadlines {
            step_ms,
            scenario_ms,
            teardown_ms,
        }
    }

    pub fn passed_outcome(spec: &ScenarioSpec) -> WorkerOutcome {
        WorkerOutcome {
            schema_version: SCHEMA_VERSION,
            run_id: spec.run_id.clone(),
            scenario_id: spec.scenario_id.clone(),
            status: CheckStatus::Passed,
            message: String::new(),
            engine_starts: 1,
            engine_starts_by_env: BTreeMap::new(),
            survivors: vec![],
            timing: BTreeMap::new(),
            steps: vec![],
            launch: None,
            checks: vec![],
            artifacts: vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use crate::util::err;
    use crate::worker::{read_outcome, OUTCOME_FILE, SCENARIO_FILE};
    use std::cell::Cell;
    use std::io::Cursor;

    #[test]
    fn the_start_line_gates_the_worker() {
        assert!(wait_for_start(Cursor::new("go\n")));
        assert!(wait_for_start(Cursor::new("go\r\nmore")));
        assert!(
            !wait_for_start(Cursor::new("")),
            "end of input is not a start"
        );
        assert!(!wait_for_start(Cursor::new("stop\n")));
        assert!(!wait_for_start(Cursor::new("\n")));
    }

    #[test]
    fn a_scenario_runs_and_its_outcome_is_written_with_exit_zero() {
        let root = run_root("ok");
        let spec = spec_in(&root, "world-1", deadlines(1_000, 5_000, 1_000));
        worker::write_scenario(&spec).unwrap();
        let code = run_in_world(&spec.world.dir, |s| {
            assert_eq!(s, &spec);
            Ok(passed_outcome(s))
        });
        assert_eq!(code, EXIT_OK);
        let back = read_outcome(&spec.world.dir, &spec).unwrap();
        assert_eq!(back.status, CheckStatus::Passed);
    }

    #[test]
    fn a_failed_scenario_still_exits_zero_and_keeps_its_verdict() {
        let root = run_root("failed");
        let spec = spec_in(&root, "world-1", deadlines(1_000, 5_000, 1_000));
        worker::write_scenario(&spec).unwrap();
        let code = run_in_world(&spec.world.dir, |s| {
            let mut o = passed_outcome(s);
            o.status = CheckStatus::Failed;
            o.message = "a check failed".into();
            Ok(o)
        });
        assert_eq!(code, EXIT_OK);
        let back = read_outcome(&spec.world.dir, &spec).unwrap();
        assert_eq!(back.status, CheckStatus::Failed);
        assert_eq!(back.message, "a check failed");
    }

    #[test]
    fn an_error_or_a_panic_becomes_an_errored_outcome() {
        let root = run_root("errored");
        let spec = spec_in(&root, "world-1", deadlines(1_000, 5_000, 1_000));
        worker::write_scenario(&spec).unwrap();

        assert_eq!(run_in_world(&spec.world.dir, |_| err("boom")), EXIT_OK);
        let back = read_outcome(&spec.world.dir, &spec).unwrap();
        assert_eq!(back.status, CheckStatus::Errored);
        assert!(back.message.contains("boom"), "{}", back.message);

        std::fs::remove_file(spec.world.dir.join(OUTCOME_FILE)).unwrap();
        assert_eq!(
            run_in_world(&spec.world.dir, |_| -> Result<WorkerOutcome> {
                panic!("kaput")
            }),
            EXIT_OK
        );
        let back = read_outcome(&spec.world.dir, &spec).unwrap();
        assert_eq!(back.status, CheckStatus::Errored);
        assert!(back.message.contains("panicked: kaput"), "{}", back.message);
    }

    #[test]
    fn an_outcome_the_contract_refuses_is_replaced_by_an_errored_one() {
        let root = run_root("refused-outcome");
        let spec = spec_in(&root, "world-1", deadlines(1_000, 5_000, 1_000));
        worker::write_scenario(&spec).unwrap();
        let code = run_in_world(&spec.world.dir, |s| {
            let mut o = passed_outcome(s);
            o.scenario_id = "someone-else".into();
            Ok(o)
        });
        assert_eq!(code, EXIT_OK);
        let back = read_outcome(&spec.world.dir, &spec).unwrap();
        assert_eq!(back.status, CheckStatus::Errored);
        assert!(
            back.message.contains("refused by the contract"),
            "{}",
            back.message
        );
    }

    /// Fixture: a worker run with a world directory that does not match the spec is refused, the
    /// scenario is never run and no outcome is written.
    #[test]
    fn a_world_dir_that_mismatches_the_spec_is_refused_without_running_anything() {
        let root = run_root("mismatch");
        let spec = spec_in(&root, "world-1", deadlines(1_000, 5_000, 1_000));
        worker::write_scenario(&spec).unwrap();
        let other = root.join("world-2");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::copy(
            spec.world.dir.join(SCENARIO_FILE),
            other.join(SCENARIO_FILE),
        )
        .unwrap();

        let ran = Cell::new(false);
        let code = run_in_world(&other, |s| {
            ran.set(true);
            Ok(passed_outcome(s))
        });
        assert_eq!(code, EXIT_REFUSED);
        assert!(!ran.get(), "the scenario must not run");
        assert!(!other.join(OUTCOME_FILE).exists());
        assert!(!spec.world.dir.join(OUTCOME_FILE).exists());

        // A directory with no scenario at all is refused the same way.
        let empty = root.join("world-3");
        std::fs::create_dir_all(&empty).unwrap();
        assert_eq!(
            run_in_world(&empty, |s| Ok(passed_outcome(s))),
            EXIT_REFUSED
        );
    }
}
