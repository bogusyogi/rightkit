//! The runner <-> worker contract (ADR-0001 item 3, rulings D-c, D-e, D-g): data only.
//!
//! The worker is the same `rightkit-qa` executable, started as `rightkit-qa worker <world dir>`.
//! The runner writes `scenario.json` into the world directory before it starts the worker. The
//! worker reads it, runs exactly that one scenario, writes `outcome.json` into the same directory
//! and exits. The runner holds the resource tokens, the Job Object or process group and every
//! deadline; the worker only reports. A worker that exits without an `outcome.json`, or with one
//! that fails [`WorkerOutcome::validate_for`], is `errored` to the runner, never passed.
//!
//! This module has no process spawning, no containment and no scenario execution. It holds the
//! types, their JSON form, the validation both sides apply, and the atomic read/write helpers.
//!
//! Layout of a world, all of it under the run root:
//!
//! ```text
//! <run root>/<world dir>/
//!     scenario.json   written by the runner, read by the worker
//!     outcome.json    written by the worker, read by the runner
//!     <data, home, temp, control socket, control token file>   as named in `World`
//! ```
use crate::evidence::{Artifact, Check, CheckStatus};
use crate::util::{err, plain_path, require_text, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

/// Version of both `scenario.json` and `outcome.json`. A reader refuses any other value before it
/// looks at another field, so a runner and a worker from different releases cannot misread each other.
///
/// Version 2 added the per-step samples, the engine starts per environment set and the launch facts
/// to `outcome.json`. Version 1 is not read: an outcome without them cannot back a truthful receipt.
pub const SCHEMA_VERSION: u32 = 2;
/// File the runner writes into the world directory.
pub const SCENARIO_FILE: &str = "scenario.json";
/// File the worker writes into the world directory.
pub const OUTCOME_FILE: &str = "outcome.json";
/// Lowest port a world may be granted; the range below it is for well-known services.
pub const MIN_WORLD_PORT: u16 = 1024;

/// A machine-wide resource a scenario must declare (ADR-0001 item 2). The runner grants a token only
/// while no other scenario holds it; a scenario that declares none touches none of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Token {
    /// Keyboard and window focus.
    Focus,
    /// The audio device.
    Audio,
    /// The logged-in GUI session (display, accessibility tree).
    GuiSession,
    /// The system clipboard.
    Clipboard,
    /// The real OS credential store. The in-memory store needs no token (ADR-0001 item 2, C2).
    Credentials,
}

/// An environment variable every process of the world carries, so survivors are found by a
/// same-user scan (D-c). `value` is unique per world.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvMarker {
    pub name: String,
    pub value: String,
}

/// The sandbox of one scenario: where its processes keep state and which ports and control
/// endpoints are theirs. Every path is absolute and lies under the run root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct World {
    /// The world directory itself: holds `scenario.json` and `outcome.json`.
    pub dir: PathBuf,
    pub data_dir: PathBuf,
    pub home: PathBuf,
    pub temp: PathBuf,
    /// First port of the range this world may bind (inclusive).
    pub port_start: u16,
    /// Last port of the range this world may bind (inclusive).
    pub port_end: u16,
    /// Control socket (or named pipe path) of this world's control server.
    pub control_socket: PathBuf,
    /// User-only file holding the control token the embedded control server checks (D-e).
    pub control_token_file: PathBuf,
    /// The marker D-c puts in every process environment of the world.
    pub env_marker: EnvMarker,
}

/// Hard limits the runner enforces from outside. The worker may read them to size its own waits;
/// it does not enforce them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Deadlines {
    /// Longest any one step may run before the runner kills the tree.
    pub step_ms: u64,
    /// Longest the whole scenario, setup and teardown included, may run.
    pub scenario_ms: u64,
    /// Longest teardown may run once the steps have ended or failed.
    pub teardown_ms: u64,
}

/// What the runner hands the worker: the resolved scenario, its world and its grants.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScenarioSpec {
    pub schema_version: u32,
    pub run_id: String,
    pub scenario_id: String,
    pub app: String,
    pub platform: String,
    /// Every world path must lie strictly under this directory.
    pub run_root: PathBuf,
    pub world: World,
    /// The resolved scenario (variables and includes already applied). Opaque to this contract; the
    /// worker decodes it with the scenario loader.
    pub scenario: Value,
    /// Resource tokens the runner holds for this scenario, each at most once.
    pub grants: Vec<Token>,
    pub deadlines: Deadlines,
}

/// Wall time spent in one step kind, summed over the scenario.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepTiming {
    pub count: u32,
    pub total_ms: u64,
    pub max_ms: u64,
}

/// The wall time of one executed step, as the worker measured it. The runner recomputes the
/// receipt's p50/p95/slowest and the step budgets from these, so `label` is the step's label (or its
/// name), `kind` its step kind (`cli`, `http`, `engine_log`, `restart`, ...).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepSample {
    pub label: String,
    pub kind: String,
    pub ms: u64,
    /// `passed` when the step ran to its expectations, `failed` or `errored` otherwise.
    pub status: CheckStatus,
}

/// What the worker actually launched, recorded at the launch and not asserted by the runner: the
/// receipt's `world` section is built from it. `env` is the environment the first app or engine
/// process was given; only the part `isolation_env` keeps reaches the receipt (isolation keys,
/// `RIGHTKIT_*` keys and the world marker), and the worker should send no more than that.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerLaunch {
    /// The directory the app was told is its data directory. Must lie under the scenario's world dir.
    pub data_dir: PathBuf,
    /// Variable through which the app was told it (`data_dir_env` of the config).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_dir_env: Option<String>,
    /// Flag through which the app was told it (`data_dir_arg` of the config).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_dir_arg: Option<String>,
    /// Arguments of that launch.
    #[serde(default)]
    pub args: Vec<String>,
    /// The environment of that launch, last value of a repeated name wins.
    #[serde(default)]
    pub env: Vec<(String, String)>,
    /// Every binary the scenario started, as the paths it started them from. The runner maps each to
    /// a role of the build record and refuses a path the record does not hold.
    #[serde(default)]
    pub binaries: Vec<PathBuf>,
    /// `Some(true)` when a token-less control order sent to the running UI app was refused
    /// (`Control::probe_token_required` returned `Ok`), `Some(false)` when the probe ran and the
    /// app answered or failed another way, `None` when no UI app was launched or no probe ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_less_refused: Option<bool>,
}

/// Who a surviving process belongs to (D-c): reached from a registered root, found by the world
/// marker, or neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SurvivorOwner {
    RegisteredRoot,
    WorldMarker,
    Unknown,
}

/// A process still alive after teardown, recorded before cleanup. Any survivor fails the scenario.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Survivor {
    pub pid: u32,
    pub name: String,
    pub owner: SurvivorOwner,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

/// What the worker reports. The runner decides the verdict from it and from what it observed
/// itself (exit status, deadlines, its own survivor scan).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerOutcome {
    pub schema_version: u32,
    pub run_id: String,
    pub scenario_id: String,
    pub status: CheckStatus,
    /// Why the status is not `passed`; empty on a pass.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message: String,
    /// Engine processes started for this scenario (ADR-0001 item 5).
    pub engine_starts: u32,
    /// Starts per environment set (`engine::EngineStarts::by_env_set`: labels such as `#1 default`
    /// or `#2 env KEYS`, never values). When not empty the counts add up to `engine_starts`; when
    /// empty the receipt names one set, `default`.
    #[serde(default)]
    pub engine_starts_by_env: BTreeMap<String, u32>,
    #[serde(default)]
    pub survivors: Vec<Survivor>,
    /// Timing per step kind (`cli`, `http`, `engine_log`, ...). Must agree with `steps`.
    #[serde(default)]
    pub timing: BTreeMap<String, StepTiming>,
    /// Every step the scenario ran, in order, with its wall time and status.
    #[serde(default)]
    pub steps: Vec<StepSample>,
    /// The launch the scenario made; `None` when it launched nothing (the runner refuses a pass
    /// without it: isolation cannot be recorded).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch: Option<WorkerLaunch>,
    /// Checks the scenario recorded.
    #[serde(default)]
    pub checks: Vec<Check>,
    /// Artifacts the scenario hashed, referenced by `checks`.
    #[serde(default)]
    pub artifacts: Vec<Artifact>,
}

// ------------------------------------------------------------------------------------ validation

/// Path components compared by name; on Windows case-insensitively, the way the file system does.
fn key(c: &Component<'_>) -> String {
    let s = c.as_os_str().to_string_lossy().into_owned();
    if cfg!(windows) {
        s.to_lowercase()
    } else {
        s
    }
}

/// `path` is absolute, spells no `.` or `..`, and lies strictly under `root` (a sibling that merely
/// shares a name prefix does not). Pure text: nothing is read from disk, so the paths need not exist.
fn require_under(label: &str, root: &Path, path: &Path) -> Result<()> {
    if !path.is_absolute() {
        return err(format!(
            "{label} must be an absolute path: {}",
            path.display()
        ));
    }
    if path
        .components()
        .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return err(format!(
            "{label} must not contain . or .. components: {}",
            path.display()
        ));
    }
    let root_keys: Vec<String> = plain_path(root).components().map(|c| key(&c)).collect();
    let path_keys: Vec<String> = plain_path(path).components().map(|c| key(&c)).collect();
    if path_keys.len() <= root_keys.len() || path_keys[..root_keys.len()] != root_keys[..] {
        return err(format!(
            "{label} is outside the run root {}: {}",
            root.display(),
            path.display()
        ));
    }
    Ok(())
}

fn version_refusal(found: u64, at: &str) -> String {
    if found == 1 {
        format!(
            "worker contract schema version 1{at} is no longer read; this build reads {SCHEMA_VERSION}. Version 1 has no per-step samples, no engine starts per environment set and no launch facts, so it cannot back a receipt: run the worker and the runner from the same rightkit-qa release"
        )
    } else {
        format!(
            "unsupported worker contract schema version {found}{at}; this build reads {SCHEMA_VERSION}"
        )
    }
}

fn require_version(found: u32) -> Result<()> {
    if found != SCHEMA_VERSION {
        return err(version_refusal(u64::from(found), ""));
    }
    Ok(())
}

impl World {
    /// Every path under `run_root`, a usable port range, a well-formed marker.
    pub fn validate(&self, run_root: &Path) -> Result<()> {
        for (label, p) in [
            ("world dir", &self.dir),
            ("world data dir", &self.data_dir),
            ("world home", &self.home),
            ("world temp", &self.temp),
            ("world control socket", &self.control_socket),
            ("world control token file", &self.control_token_file),
        ] {
            require_under(label, run_root, p)?;
        }
        if self.port_start < MIN_WORLD_PORT || self.port_start > self.port_end {
            return err(format!(
                "world port range {}-{} is invalid: it must start at {MIN_WORLD_PORT} or above and not run backwards",
                self.port_start, self.port_end
            ));
        }
        let name = &self.env_marker.name;
        if name.is_empty() || name.contains(['=', '\0']) {
            return err("world env marker name must be non-empty and hold no '=' or NUL");
        }
        if self.env_marker.value.trim().is_empty() || self.env_marker.value.contains('\0') {
            return err("world env marker value must be non-empty and hold no NUL");
        }
        Ok(())
    }
}

impl Deadlines {
    pub fn validate(&self) -> Result<()> {
        if self.step_ms == 0 || self.scenario_ms == 0 || self.teardown_ms == 0 {
            return err("deadlines must all be above zero");
        }
        if self.step_ms > self.scenario_ms {
            return err(format!(
                "step deadline {} ms exceeds the scenario deadline {} ms",
                self.step_ms, self.scenario_ms
            ));
        }
        Ok(())
    }
}

impl ScenarioSpec {
    /// Refuse a spec the worker must not run: wrong schema version, empty identity, a world path
    /// outside the run root, a repeated token, or deadlines that cannot hold.
    pub fn validate(&self) -> Result<()> {
        require_version(self.schema_version)?;
        for (value, name) in [
            (&self.run_id, "run id"),
            (&self.scenario_id, "scenario id"),
            (&self.app, "app"),
            (&self.platform, "platform"),
        ] {
            require_text(value, name)?;
        }
        if !self.run_root.is_absolute() {
            return err(format!(
                "run root must be an absolute path: {}",
                self.run_root.display()
            ));
        }
        if self
            .run_root
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
        {
            return err(format!(
                "run root must not contain . or .. components: {}",
                self.run_root.display()
            ));
        }
        self.world.validate(&self.run_root)?;
        if !self.scenario.is_object() {
            return err("the resolved scenario must be a JSON object");
        }
        let mut seen = BTreeSet::new();
        for t in &self.grants {
            if !seen.insert(*t) {
                return err(format!("resource token granted twice: {t:?}"));
            }
        }
        self.deadlines.validate()
    }
}

impl WorkerOutcome {
    /// What the runner checks before it believes an outcome: the schema version, that it answers the
    /// scenario it was started for, that timing adds up, and that a pass has no survivor.
    pub fn validate_for(&self, spec: &ScenarioSpec) -> Result<()> {
        require_version(self.schema_version)?;
        if self.run_id != spec.run_id || self.scenario_id != spec.scenario_id {
            return err(format!(
                "outcome is for {}/{}, the worker was started for {}/{}",
                self.run_id, self.scenario_id, spec.run_id, spec.scenario_id
            ));
        }
        for (kind, t) in &self.timing {
            if kind.trim().is_empty() {
                return err("timing has an empty step kind");
            }
            if t.count == 0 || t.max_ms > t.total_ms {
                return err(format!(
                    "timing for step kind {kind} is inconsistent: count {}, total {} ms, max {} ms",
                    t.count, t.total_ms, t.max_ms
                ));
            }
        }
        self.validate_samples()?;
        if self.status == CheckStatus::Passed && !self.survivors.is_empty() {
            return err(format!(
                "outcome is passed but records {} surviving process(es); a survivor fails the scenario",
                self.survivors.len()
            ));
        }
        Ok(())
    }

    /// The step samples, the per-kind timing and the engine starts must say the same thing, so the
    /// receipt built from the samples cannot differ from what the worker reported about itself.
    fn validate_samples(&self) -> Result<()> {
        let mut by_kind: BTreeMap<&str, StepTiming> = BTreeMap::new();
        for s in &self.steps {
            if s.label.trim().is_empty() || s.kind.trim().is_empty() {
                return err("a step sample has an empty label or kind");
            }
            let t = by_kind.entry(s.kind.as_str()).or_insert(StepTiming {
                count: 0,
                total_ms: 0,
                max_ms: 0,
            });
            t.count = t.count.saturating_add(1);
            t.total_ms = t.total_ms.saturating_add(s.ms);
            t.max_ms = t.max_ms.max(s.ms);
        }
        let reported: BTreeMap<&str, StepTiming> =
            self.timing.iter().map(|(k, v)| (k.as_str(), *v)).collect();
        if by_kind != reported {
            return err(format!(
                "outcome timing {reported:?} does not match its step samples {by_kind:?}"
            ));
        }
        if !self.engine_starts_by_env.is_empty() {
            if self
                .engine_starts_by_env
                .keys()
                .any(|l| l.trim().is_empty())
            {
                return err("engine_starts_by_env has an empty environment label");
            }
            let sum = self
                .engine_starts_by_env
                .values()
                .fold(0u32, |a, n| a.saturating_add(*n));
            if sum != self.engine_starts {
                return err(format!(
                    "engine_starts is {} but engine_starts_by_env adds up to {sum}",
                    self.engine_starts
                ));
            }
        }
        if let Some(l) = &self.launch {
            if !l.data_dir.is_absolute() {
                return err(format!(
                    "launch data dir must be an absolute path: {}",
                    l.data_dir.display()
                ));
            }
            if l.env
                .iter()
                .any(|(k, _)| k.is_empty() || k.contains(['=', '\0']))
            {
                return err("launch env holds an invalid variable name");
            }
        }
        Ok(())
    }

    /// Any survivor, or a survivor whose owner is unknown, fails the scenario (ADR-0001 item 3).
    pub fn has_survivors(&self) -> bool {
        !self.survivors.is_empty()
    }
}

// ------------------------------------------------------------------------------------- read/write

/// Write `bytes` so a reader on the other side of the contract sees the whole file or none of it.
fn write_whole(path: &Path, bytes: &[u8]) -> Result<()> {
    rightkit_fs::atomic_write(path, bytes).map_err(|e| {
        Error(format!(
            "cannot write {}: {}",
            path.display(),
            std::io::Error::from(e)
        ))
    })
}

/// Read `path` and check its `schemaVersion` before decoding the rest, so a file from another
/// release is refused for its version and not for whichever field changed.
fn read_versioned<T: serde::de::DeserializeOwned>(path: &Path, what: &str) -> Result<T> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| Error(format!("cannot read {what} {}: {e}", path.display())))?;
    let raw: Value = serde_json::from_str(&text)
        .map_err(|e| Error(format!("{what} {} is not JSON: {e}", path.display())))?;
    let version = raw
        .get("schemaVersion")
        .and_then(Value::as_u64)
        .ok_or_else(|| Error(format!("{what} {} has no schemaVersion", path.display())))?;
    if version != u64::from(SCHEMA_VERSION) {
        return err(version_refusal(version, &format!(" in {}", path.display())));
    }
    serde_json::from_value(raw)
        .map_err(|e| Error(format!("{what} {} is malformed: {e}", path.display())))
}

/// Runner side: validate `spec`, then write `<spec.world.dir>/scenario.json` atomically. The world
/// directory must already exist.
pub fn write_scenario(spec: &ScenarioSpec) -> Result<PathBuf> {
    spec.validate()?;
    let path = spec.world.dir.join(SCENARIO_FILE);
    write_whole(&path, &serde_json::to_vec_pretty(spec)?)?;
    Ok(path)
}

/// Worker side: read and validate `<world_dir>/scenario.json`. The spec must name `world_dir` as its
/// own world, so a worker cannot be pointed at another scenario's directory.
pub fn read_scenario(world_dir: &Path) -> Result<ScenarioSpec> {
    let spec: ScenarioSpec = read_versioned(&world_dir.join(SCENARIO_FILE), SCENARIO_FILE)?;
    spec.validate()?;
    if plain_path(&spec.world.dir) != plain_path(world_dir) {
        return err(format!(
            "{SCENARIO_FILE} describes world {}, the worker was started for {}",
            spec.world.dir.display(),
            world_dir.display()
        ));
    }
    Ok(spec)
}

/// Worker side: write `<world_dir>/outcome.json` atomically, after checking it against `spec`.
pub fn write_outcome(
    world_dir: &Path,
    spec: &ScenarioSpec,
    outcome: &WorkerOutcome,
) -> Result<PathBuf> {
    outcome.validate_for(spec)?;
    let path = world_dir.join(OUTCOME_FILE);
    write_whole(&path, &serde_json::to_vec_pretty(outcome)?)?;
    Ok(path)
}

/// Runner side: read `<world_dir>/outcome.json` and check it against the spec the worker was given.
/// A missing file is its own message: the worker died or was killed before it reported.
pub fn read_outcome(world_dir: &Path, spec: &ScenarioSpec) -> Result<WorkerOutcome> {
    let path = world_dir.join(OUTCOME_FILE);
    if !path.is_file() {
        return err(format!(
            "worker wrote no {OUTCOME_FILE} in {}",
            world_dir.display()
        ));
    }
    let outcome: WorkerOutcome = read_versioned(&path, OUTCOME_FILE)?;
    outcome.validate_for(spec)?;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A fresh run root under the temp directory, created on disk.
    fn run_root(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rkqa-worker-{tag}-{}", crate::util::new_id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn spec_in(root: &Path) -> ScenarioSpec {
        let dir = root.join("world-1");
        std::fs::create_dir_all(&dir).unwrap();
        ScenarioSpec {
            schema_version: SCHEMA_VERSION,
            run_id: "run-1".into(),
            scenario_id: "login-flow".into(),
            app: "postright".into(),
            platform: "windows".into(),
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
                    value: "w-0123".into(),
                },
                dir,
            },
            scenario: json!({"id": "login-flow", "steps": [{"kind": "cli", "args": ["status"]}]}),
            grants: vec![Token::Focus, Token::GuiSession],
            deadlines: Deadlines {
                step_ms: 30_000,
                scenario_ms: 300_000,
                teardown_ms: 20_000,
            },
        }
    }

    fn outcome_for(spec: &ScenarioSpec) -> WorkerOutcome {
        let mut timing = BTreeMap::new();
        timing.insert(
            "cli".to_string(),
            StepTiming {
                count: 4,
                total_ms: 900,
                max_ms: 400,
            },
        );
        WorkerOutcome {
            schema_version: SCHEMA_VERSION,
            run_id: spec.run_id.clone(),
            scenario_id: spec.scenario_id.clone(),
            status: CheckStatus::Passed,
            message: String::new(),
            engine_starts: 1,
            engine_starts_by_env: BTreeMap::from([("#1 default".to_string(), 1)]),
            survivors: vec![],
            timing,
            steps: [100u64, 200, 400, 200]
                .iter()
                .enumerate()
                .map(|(i, ms)| StepSample {
                    label: format!("status {i}"),
                    kind: "cli".into(),
                    ms: *ms,
                    status: CheckStatus::Passed,
                })
                .collect(),
            launch: Some(WorkerLaunch {
                data_dir: spec.world.data_dir.join("login-flow-1a2b3c4d"),
                data_dir_env: Some("POSTRIGHT_DATA".into()),
                data_dir_arg: None,
                args: vec!["status".into()],
                env: vec![("POSTRIGHT_DATA".into(), "x".into())],
                binaries: vec![spec.world.dir.join("engine.exe")],
                token_less_refused: Some(true),
            }),
            checks: vec![Check {
                name: "status ok".into(),
                status: CheckStatus::Passed,
                details: json!({"exit": 0}),
                artifacts: vec!["shot".into()],
            }],
            artifacts: vec![Artifact {
                name: "shot".into(),
                path: spec.world.temp.join("shot.png"),
                sha256: "ab".repeat(32),
                size_bytes: 12,
                recorded_at: "2026-10-23T12:00:00Z".into(),
            }],
        }
    }

    fn same_json<T: Serialize>(a: &T, b: &T) {
        assert_eq!(
            serde_json::to_value(a).unwrap(),
            serde_json::to_value(b).unwrap()
        );
    }

    #[test]
    fn scenario_round_trips_through_the_world_directory() {
        let root = run_root("spec");
        let spec = spec_in(&root);
        let path = write_scenario(&spec).unwrap();
        assert_eq!(path, spec.world.dir.join(SCENARIO_FILE));
        let back = read_scenario(&spec.world.dir).unwrap();
        assert_eq!(back, spec);
        // Wire spellings the worker and runner of one release rely on.
        let wire: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(wire["schemaVersion"], 2);
        assert_eq!(wire["grants"], json!(["focus", "gui-session"]));
        assert_eq!(wire["world"]["portStart"], 41000);
        // No temp file is left beside the finished one.
        let names: Vec<_> = std::fs::read_dir(&spec.world.dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec![SCENARIO_FILE.to_string()]);
    }

    #[test]
    fn outcome_round_trips_and_keeps_the_wire_names_of_new_statuses() {
        let root = run_root("outcome");
        let spec = spec_in(&root);
        let outcome = outcome_for(&spec);
        write_outcome(&spec.world.dir, &spec, &outcome).unwrap();
        let back = read_outcome(&spec.world.dir, &spec).unwrap();
        same_json(&back, &outcome);
        assert_eq!(back.engine_starts, 1);
        assert_eq!(back.timing["cli"].count, 4);

        // D-f: Errored is "errored", NotRun is "missing" on the wire.
        for (status, wire) in [
            (CheckStatus::Errored, "errored"),
            (CheckStatus::NotRun, "missing"),
        ] {
            let mut o = outcome_for(&spec);
            o.status = status;
            o.message = "did not finish".into();
            assert_eq!(serde_json::to_value(&o).unwrap()["status"], wire);
            write_outcome(&spec.world.dir, &spec, &o).unwrap();
            assert_eq!(read_outcome(&spec.world.dir, &spec).unwrap().status, status);
        }
    }

    #[test]
    fn a_world_outside_the_run_root_is_refused() {
        let root = run_root("outside");
        let elsewhere = run_root("elsewhere");
        let mut spec = spec_in(&root);
        spec.validate().unwrap();

        // Each world path, moved out of the run root, is refused by name.
        for field in ["dir", "data", "home", "temp", "socket", "token"] {
            let mut s = spec.clone();
            let out = elsewhere.join("x");
            match field {
                "dir" => s.world.dir = out,
                "data" => s.world.data_dir = out,
                "home" => s.world.home = out,
                "temp" => s.world.temp = out,
                "socket" => s.world.control_socket = out,
                _ => s.world.control_token_file = out,
            }
            let e = s.validate().unwrap_err();
            assert!(e.0.contains("outside the run root"), "{field}: {e}");
            assert!(write_scenario(&s).is_err(), "{field} was written");
        }

        // A sibling that shares the root's name as a prefix is outside, not inside.
        let mut s = spec.clone();
        let mut sibling = root.clone().into_os_string();
        sibling.push("2");
        s.world.temp = PathBuf::from(sibling).join("tmp");
        assert!(s.validate().unwrap_err().0.contains("outside the run root"));

        // `..` cannot walk out, and the root itself is not a world path.
        let mut s = spec.clone();
        s.world.home = root.join("world-1").join("..").join("..").join("escape");
        assert!(s.validate().unwrap_err().0.contains(".."));
        let mut s = spec.clone();
        s.world.data_dir = root.clone();
        assert!(s.validate().unwrap_err().0.contains("outside the run root"));

        // Relative paths are refused.
        spec.world.temp = PathBuf::from("tmp");
        assert!(spec.validate().unwrap_err().0.contains("absolute"));
    }

    #[test]
    fn an_unknown_schema_version_is_refused_on_both_sides() {
        let root = run_root("version");
        let mut spec = spec_in(&root);
        spec.schema_version = SCHEMA_VERSION + 1;
        assert!(spec.validate().unwrap_err().0.contains("schema version"));
        assert!(write_scenario(&spec).is_err());

        // A file from another release is refused for its version even when its other fields differ.
        let good = spec_in(&root);
        std::fs::write(
            good.world.dir.join(SCENARIO_FILE),
            r#"{"schemaVersion": 3, "somethingNew": true}"#,
        )
        .unwrap();
        let e = read_scenario(&good.world.dir).unwrap_err();
        assert!(e.0.contains("schema version 3"), "{e}");
        std::fs::write(good.world.dir.join(SCENARIO_FILE), r#"{"runId": "x"}"#).unwrap();
        assert!(read_scenario(&good.world.dir)
            .unwrap_err()
            .0
            .contains("no schemaVersion"));

        let mut o = outcome_for(&good);
        o.schema_version = 0;
        assert!(o
            .validate_for(&good)
            .unwrap_err()
            .0
            .contains("schema version"));
        std::fs::write(
            good.world.dir.join(OUTCOME_FILE),
            r#"{"schemaVersion": 9, "status": "passed"}"#,
        )
        .unwrap();
        assert!(read_outcome(&good.world.dir, &good)
            .unwrap_err()
            .0
            .contains("schema version 9"));
    }

    #[test]
    fn a_version_1_file_is_refused_with_a_message_that_says_why() {
        let root = run_root("v1");
        let spec = spec_in(&root);
        std::fs::write(
            spec.world.dir.join(OUTCOME_FILE),
            r#"{"schemaVersion": 1, "runId": "run-1", "scenarioId": "login-flow", "status": "passed", "engineStarts": 1}"#,
        )
        .unwrap();
        let e = read_outcome(&spec.world.dir, &spec).unwrap_err();
        assert!(
            e.0.contains("version 1") && e.0.contains("no longer read") && e.0.contains("per-step"),
            "{e}"
        );
        std::fs::write(
            spec.world.dir.join(SCENARIO_FILE),
            r#"{"schemaVersion": 1}"#,
        )
        .unwrap();
        assert!(read_scenario(&spec.world.dir)
            .unwrap_err()
            .0
            .contains("no longer read"));
        let mut o = outcome_for(&spec);
        o.schema_version = 1;
        assert!(o
            .validate_for(&spec)
            .unwrap_err()
            .0
            .contains("no longer read"));
    }

    #[test]
    fn step_samples_timing_and_engine_starts_must_agree() {
        let root = run_root("samples");
        let spec = spec_in(&root);
        outcome_for(&spec).validate_for(&spec).unwrap();

        // A sample the timing does not account for, and timing no sample backs.
        let mut o = outcome_for(&spec);
        o.steps[0].ms = 101;
        assert!(o
            .validate_for(&spec)
            .unwrap_err()
            .0
            .contains("step samples"));
        let mut o = outcome_for(&spec);
        o.steps.pop();
        assert!(o
            .validate_for(&spec)
            .unwrap_err()
            .0
            .contains("step samples"));
        let mut o = outcome_for(&spec);
        o.steps.clear();
        assert!(o
            .validate_for(&spec)
            .unwrap_err()
            .0
            .contains("step samples"));
        let mut o = outcome_for(&spec);
        o.steps[1].kind = "http".into();
        assert!(o
            .validate_for(&spec)
            .unwrap_err()
            .0
            .contains("step samples"));
        let mut o = outcome_for(&spec);
        o.steps[2].label = " ".into();
        assert!(o.validate_for(&spec).unwrap_err().0.contains("empty label"));

        // The per-environment starts add up to the total, or the outcome is refused.
        let mut o = outcome_for(&spec);
        o.engine_starts = 2;
        assert!(o
            .validate_for(&spec)
            .unwrap_err()
            .0
            .contains("adds up to 1"));
        o.engine_starts_by_env.insert("#2 env FOO".into(), 1);
        o.validate_for(&spec).unwrap();
        // No per-environment map: the total alone stands.
        let mut o = outcome_for(&spec);
        o.engine_starts_by_env.clear();
        o.engine_starts = 3;
        o.validate_for(&spec).unwrap();

        // A launch fact with a relative data dir is refused.
        let mut o = outcome_for(&spec);
        o.launch.as_mut().unwrap().data_dir = PathBuf::from("data");
        assert!(o.validate_for(&spec).unwrap_err().0.contains("absolute"));
    }

    #[test]
    fn the_new_fields_round_trip_and_default_when_absent() {
        let root = run_root("v2");
        let spec = spec_in(&root);
        let o = outcome_for(&spec);
        write_outcome(&spec.world.dir, &spec, &o).unwrap();
        let back = read_outcome(&spec.world.dir, &spec).unwrap();
        assert_eq!(back.steps, o.steps);
        assert_eq!(back.engine_starts_by_env, o.engine_starts_by_env);
        assert_eq!(back.launch, o.launch);
        let wire = serde_json::to_value(&o).unwrap();
        assert_eq!(wire["steps"][0]["label"], "status 0");
        assert_eq!(wire["steps"][0]["status"], "passed");
        assert_eq!(wire["engineStartsByEnv"]["#1 default"], 1);
        assert_eq!(wire["launch"]["tokenLessRefused"], true);
        // A minimal v2 outcome (a refusal or a deadline) has no samples and no launch.
        let min: WorkerOutcome = serde_json::from_value(serde_json::json!({
            "schemaVersion": 2, "runId": "run-1", "scenarioId": "login-flow",
            "status": "errored", "message": "deadline", "engineStarts": 0
        }))
        .unwrap();
        assert!(
            min.steps.is_empty() && min.launch.is_none() && min.engine_starts_by_env.is_empty()
        );
        min.validate_for(&spec).unwrap();
    }

    #[test]
    fn tokens_must_be_unique() {
        let root = run_root("tokens");
        let mut spec = spec_in(&root);
        spec.grants = vec![Token::Audio, Token::Clipboard, Token::Audio];
        assert!(spec.validate().unwrap_err().0.contains("twice"));
        spec.grants = vec![
            Token::Focus,
            Token::Audio,
            Token::GuiSession,
            Token::Clipboard,
            Token::Credentials,
        ];
        spec.validate().unwrap();
        spec.grants = vec![];
        spec.validate().unwrap();
    }

    #[test]
    fn deadlines_ports_and_marker_are_checked() {
        let root = run_root("limits");
        let spec = spec_in(&root);

        let mut s = spec.clone();
        s.deadlines.step_ms = 0;
        assert!(s.validate().is_err());
        let mut s = spec.clone();
        s.deadlines.step_ms = s.deadlines.scenario_ms + 1;
        assert!(s.validate().unwrap_err().0.contains("exceeds"));

        let mut s = spec.clone();
        s.world.port_start = 80;
        assert!(s.validate().unwrap_err().0.contains("port range"));
        let mut s = spec.clone();
        s.world.port_end = s.world.port_start - 1;
        assert!(s.validate().unwrap_err().0.contains("port range"));

        let mut s = spec.clone();
        s.world.env_marker.name = "A=B".into();
        assert!(s.validate().is_err());
        let mut s = spec.clone();
        s.world.env_marker.value = " ".into();
        assert!(s.validate().is_err());

        let mut s = spec.clone();
        s.scenario = json!([1]);
        assert!(s.validate().is_err());
        let mut s = spec;
        s.scenario_id = "  ".into();
        assert!(s.validate().is_err());
    }

    #[test]
    fn a_worker_cannot_be_pointed_at_another_scenarios_directory() {
        let root = run_root("pointed");
        let spec = spec_in(&root);
        write_scenario(&spec).unwrap();
        let other = root.join("world-2");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::copy(
            spec.world.dir.join(SCENARIO_FILE),
            other.join(SCENARIO_FILE),
        )
        .unwrap();
        let e = read_scenario(&other).unwrap_err();
        assert!(e.0.contains("started for"), "{e}");
    }

    #[test]
    fn outcome_must_answer_the_scenario_and_be_consistent() {
        let root = run_root("consistent");
        let spec = spec_in(&root);

        let mut o = outcome_for(&spec);
        o.scenario_id = "another".into();
        assert!(o.validate_for(&spec).unwrap_err().0.contains("started for"));

        let mut o = outcome_for(&spec);
        o.timing.insert(
            "http".into(),
            StepTiming {
                count: 1,
                total_ms: 5,
                max_ms: 6,
            },
        );
        assert!(o
            .validate_for(&spec)
            .unwrap_err()
            .0
            .contains("inconsistent"));

        // A passed outcome with a survivor is refused; the same survivor on a failure is reported.
        let survivor = Survivor {
            pid: 4242,
            name: "postright-engine".into(),
            owner: SurvivorOwner::WorldMarker,
            detail: String::new(),
        };
        let mut o = outcome_for(&spec);
        o.survivors.push(survivor.clone());
        assert!(o.validate_for(&spec).unwrap_err().0.contains("survivor"));
        o.status = CheckStatus::Failed;
        o.message = "survivor left behind".into();
        o.validate_for(&spec).unwrap();
        assert!(o.has_survivors());
        write_outcome(&spec.world.dir, &spec, &o).unwrap();
        let back = read_outcome(&spec.world.dir, &spec).unwrap();
        assert_eq!(back.survivors, vec![survivor]);
    }

    #[test]
    fn a_missing_outcome_is_reported_as_no_outcome() {
        let root = run_root("missing");
        let spec = spec_in(&root);
        let e = read_outcome(&spec.world.dir, &spec).unwrap_err();
        assert!(e.0.contains("wrote no outcome.json"), "{e}");
        std::fs::write(spec.world.dir.join(OUTCOME_FILE), "{ not json").unwrap();
        assert!(read_outcome(&spec.world.dir, &spec)
            .unwrap_err()
            .0
            .contains("not JSON"));
    }

    #[test]
    fn rewriting_replaces_the_previous_file() {
        let root = run_root("rewrite");
        let spec = spec_in(&root);
        let mut o = outcome_for(&spec);
        write_outcome(&spec.world.dir, &spec, &o).unwrap();
        o.status = CheckStatus::Failed;
        o.message = "second".into();
        write_outcome(&spec.world.dir, &spec, &o).unwrap();
        let back = read_outcome(&spec.world.dir, &spec).unwrap();
        assert_eq!(back.status, CheckStatus::Failed);
        assert_eq!(back.message, "second");
    }
}
