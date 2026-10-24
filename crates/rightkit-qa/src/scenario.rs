//! Declarative scenarios (`*.scenario.toml`) and the step executor.
//!
//! A scenario is a tier, optional requirements, and an ordered list of steps.
//! Steps drive the engine (CLI and framed sidecar), the real app through
//! the in-app control server, paid-provider mocks, media validators, and (feature
//! `browser`) Chrome. Each step may `save` its JSON result under a name for
//! later `${name.path}` interpolation and carries `[[step.expect]]` assertions.
use crate::a11y;
use crate::check::{check_all, interpolate, Expect};
use crate::config::QaConfig;
use crate::control::{self, Control, LaunchSpec, Mode};
use crate::engine::{
    parse_cold_reason, CliCall, CliOptions, EngineSession, EngineStarts, EngineTarget,
    ParityReport, Route as EngineRoute, Sidecar, TargetHost, COLD_REASONS,
};
use crate::mock::{MockServer, Route};
use crate::page;
use crate::process::Tracker;
use crate::storage::RunKind;
use crate::util::{err, sleep_ms, Error, Result};
use crate::worker::{Survivor, SurvivorOwner, Token, World};
use crate::workspace::QaWorkspace;
use crate::{media, util};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Deserialize)]
pub struct Scenario {
    pub name: String,
    #[serde(default = "fast")]
    pub tier: String,
    /// `engine`, `ui`, or `browser`: which binary the scenario needs.
    #[serde(default = "engine")]
    pub target: String,
    /// `env:NAME`, `file:path`, `cmd:program`, `platform:darwin|macos|windows|linux`.
    #[serde(default)]
    pub requires: Vec<String>,
    /// A scenario that may legitimately be skipped when `requires` is unmet. Every other
    /// scenario is a mandatory gate: skipping it fails the run.
    #[serde(default)]
    pub optional: bool,
    /// Kill any tracked process still alive after this scenario's teardown, without failing the
    /// scenario (see `SuiteOptions::kill_orphans`).
    #[serde(default)]
    pub kill_orphans: bool,
    pub timeout_s: Option<u64>,
    /// Machine-wide resources the scenario holds while it runs: `focus`, `audio`, `gui-session`,
    /// `clipboard`, `credentials` (ADR-0001 item 2). The pool never runs two scenarios that name the
    /// same token at once; a scenario that names none takes no lock. An unknown or repeated name is
    /// refused when the file is loaded.
    #[serde(default)]
    pub tokens: Vec<String>,
    /// `tokens` parsed and checked by [`Scenario::load`], in declared order.
    #[serde(skip)]
    pub grants: Vec<Token>,
    /// `functional` or `visual`. Absent: the scenario takes the run's kind (see
    /// [`resolve_run_kind`]). A `functional` scenario takes no screenshots and no recordings; a
    /// `visual` scenario may, and makes an otherwise unconstrained run `visual`.
    #[serde(default)]
    pub kind: Option<RunKind>,
    #[serde(default, rename = "step")]
    pub steps: Vec<Step>,
    #[serde(skip)]
    pub file: PathBuf,
}

fn fast() -> String {
    "fast".into()
}
fn engine() -> String {
    "engine".into()
}

#[derive(Debug, Clone, Deserialize)]
pub struct Step {
    pub kind: String,
    /// Human label for reports; `name` stays free for step parameters.
    pub label: Option<String>,
    pub save: Option<String>,
    #[serde(default)]
    pub expect: Vec<Expect>,
    /// Re-run the step until its expectations hold, for up to this many seconds.
    pub until_timeout_s: Option<u64>,
    pub interval_ms: Option<u64>,
    #[serde(flatten)]
    pub params: BTreeMap<String, Value>,
}

impl Scenario {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error(format!("cannot read {}: {e}", path.display())))?;
        let mut s: Scenario =
            toml::from_str(&text).map_err(|e| Error(format!("{}: {e}", path.display())))?;
        s.file = path.to_path_buf();
        crate::config::validate_tier_list(&s.tier)?;
        if !["engine", "ui", "browser"].contains(&s.target.as_str()) {
            return err(format!(
                "{}: target must be engine|ui|browser",
                path.display()
            ));
        }
        if s.steps.is_empty() {
            return err(format!("{}: scenario has no steps", path.display()));
        }
        s.grants = crate::pool::parse_tokens(&s.tokens)
            .map_err(|e| Error(format!("{}: {}", path.display(), e.0)))?;
        Ok(s)
    }
}

/// The kind of a suite run. An explicit request (`rightkit-qa run --kind`, [`crate::suite::SuiteOptions::kind`])
/// wins. Otherwise the run is `visual` when any selected scenario declares `kind = "visual"`, and
/// `functional` (the default, no screenshots) when none does.
pub fn resolve_run_kind(requested: Option<RunKind>, scenarios: &[Scenario]) -> RunKind {
    requested.unwrap_or_else(|| {
        if scenarios.iter().any(|s| s.kind == Some(RunKind::Visual)) {
            RunKind::Visual
        } else {
            RunKind::Functional
        }
    })
}

/// The kind in force for one scenario inside a run of kind `run`. A scenario that declares
/// nothing takes the run's kind; one that declares `functional` stays functional even in a visual
/// run (it takes no captures); one that declares `visual` in a `functional` run is a conflict
/// with the run that was asked for, and is refused so evidence never changes silently.
pub fn scenario_kind(run: RunKind, declared: Option<RunKind>, scenario: &str) -> Result<RunKind> {
    match (run, declared) {
        (run, None) => Ok(run),
        (RunKind::Functional, Some(RunKind::Visual)) => err(format!(
            "scenario '{scenario}' is `visual` but this is a functional run, which takes no screenshots or screen recordings; run with --kind visual (or drop the explicit functional kind)"
        )),
        (_, Some(declared)) => Ok(declared),
    }
}

/// Refuse a capture (screenshot or screen recording) unless the scenario's kind is `visual`.
/// `what` names the step or method. The error says how to change the kind.
pub fn ensure_capture_allowed(kind: RunKind, scenario: &str, what: &str) -> Result<()> {
    if kind == RunKind::Visual {
        return Ok(());
    }
    err(format!(
        "{what} is refused in functional scenario '{scenario}': a functional run takes no screenshots or screen recordings; mark the scenario `kind = \"visual\"` or run with --kind visual"
    ))
}

/// Driver methods that capture the screen. A `ui_batch` may not smuggle one into a functional run.
pub fn is_capture_method(method: &str) -> bool {
    method == "screenshot"
        || method.starts_with("record")
        || method.starts_with("capture")
        || method.starts_with("video")
}

pub fn discover(dirs: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut out = vec![];
    for d in dirs {
        if d.is_file() {
            out.push(d.clone());
            continue;
        }
        let Ok(rd) = std::fs::read_dir(d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.ends_with(".scenario.toml"))
                .unwrap_or(false)
            {
                out.push(p);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Whether a scenario runs under `selected` ("fast", "fast,paid", "all").
pub fn tier_selected(scenario_tier: &str, selected: &str) -> bool {
    let sel: Vec<&str> = selected.split(',').map(str::trim).collect();
    sel.contains(&"all")
        || scenario_tier
            .split(',')
            .map(str::trim)
            .any(|t| sel.contains(&t))
}

/// Canonical name of a `platform:<value>` requirement value, or `None` for an unknown one.
/// `macos` (the journey manifest's vocabulary) is an alias of `darwin` (what `util::platform()`
/// returns), so both spellings select the same host.
fn canonical_platform(value: &str) -> Option<&'static str> {
    match value {
        "darwin" | "macos" => Some("darwin"),
        "windows" => Some("windows"),
        "linux" => Some("linux"),
        _ => None,
    }
}

/// `Some(reason)` when a `platform:<os>` requirement names a known OS other than the host's. The
/// suite skips such a scenario even when it is mandatory: it can never run here. An UNKNOWN platform
/// value is not a mismatch (it is a typo, not another OS): it is left to [`unmet_requirement_with`],
/// which reports it as an unmet requirement so it can never skip silently as `OtherPlatform`.
pub fn platform_mismatch(requires: &[String]) -> Option<String> {
    let host = crate::util::platform().ok();
    requires.iter().find_map(|r| {
        let (kind, value) = r.split_once(':')?;
        if kind != "platform" {
            return None;
        }
        let wanted = canonical_platform(value)?;
        (host != Some(wanted)).then(|| format!("platform is not {value}"))
    })
}

/// `Some(reason)` when a requirement is not met (the scenario is skipped). `env:X` reads the
/// process environment; see [`unmet_requirement_with`] for a settings source.
pub fn unmet_requirement(requires: &[String], app_root: &Path) -> Option<String> {
    unmet_requirement_with(requires, app_root, &|k| {
        std::env::var(k).ok().filter(|v| !v.is_empty())
    })
}

/// Like [`unmet_requirement`], with `env:X` answered by `lookup` (for example
/// `|k| settings.get(k)` over a [`crate::settings::Settings`], which survives managed cargo).
/// Kinds: `env:NAME`, `file:path` (relative to `app_root` or absolute), `cmd:program` (on `PATH`
/// or a path), `platform:darwin|macos|windows|linux` (`macos` is an alias of `darwin`; an unknown
/// value is an unmet requirement, like an unknown kind).
pub fn unmet_requirement_with(
    requires: &[String],
    app_root: &Path,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Option<String> {
    for r in requires {
        let (kind, value) = r.split_once(':').unwrap_or((r.as_str(), ""));
        let missing = match kind {
            "env" => lookup(value)
                .is_none()
                .then(|| format!("missing env {value}")),
            "file" => (!app_root.join(value).exists() && !Path::new(value).exists())
                .then(|| format!("missing file {value}")),
            "cmd" => (!command_exists(value)).then(|| format!("missing command {value}")),
            "platform" => match canonical_platform(value) {
                Some(wanted) => (crate::util::platform().ok() != Some(wanted))
                    .then(|| format!("platform is not {value}")),
                None => Some(format!(
                    "unknown platform '{value}' (expected darwin, macos, windows or linux)"
                )),
            },
            other => Some(format!("unknown requirement kind '{other}'")),
        };
        if missing.is_some() {
            return missing;
        }
    }
    None
}

/// Why a scenario was skipped (Sage C5). A skip is a decision the run made, not a verdict, so each one
/// carries its reason as a kind, and the pass rule reads the kind, never the text of the reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SkipKind {
    /// The scenario requires another OS than the host's (`platform:<os>`). It can never run here, so
    /// it never counts against any run.
    OtherPlatform,
    /// A requirement other than the platform is unmet (a missing env var, file or command).
    UnmetRequirement,
    /// The run's tier selection left the scenario out.
    TierNotSelected,
    /// The scenario is declared `optional` and its requirements are unmet.
    Optional,
}

impl SkipKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SkipKind::OtherPlatform => "other-platform",
            SkipKind::UnmetRequirement => "unmet-requirement",
            SkipKind::TierNotSelected => "tier-not-selected",
            SkipKind::Optional => "optional",
        }
    }

    /// Whether a skip of this kind leaves the run passing. A qualifying run tolerates only
    /// `OtherPlatform`; an ordinary run also tolerates `Optional` (the behaviour before typed skips).
    /// Every other kind fails either way.
    pub fn tolerated(self, qualifying: bool) -> bool {
        match self {
            SkipKind::OtherPlatform => true,
            SkipKind::Optional => !qualifying,
            SkipKind::UnmetRequirement | SkipKind::TierNotSelected => false,
        }
    }
}

/// What the requirement check decides for one scenario before any step runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gate {
    /// Requirements are met (or none are declared): run it.
    Run,
    /// Do not run it; record a skip of this kind.
    Skip { kind: SkipKind, reason: String },
    /// Do not run it and fail: a mandatory scenario that cannot run is a failed gate.
    Fail { reason: String },
}

/// Decide whether `sc` runs on this host. A `platform:<os>` mismatch is an `OtherPlatform` skip even
/// for a mandatory scenario (it can never run here). Any other unmet requirement is an `Optional`
/// skip for an `optional` scenario and a failure for a mandatory one.
pub fn requirement_gate(sc: &Scenario, app_root: &Path) -> Gate {
    if let Some(reason) = platform_mismatch(&sc.requires) {
        return Gate::Skip {
            kind: SkipKind::OtherPlatform,
            reason,
        };
    }
    match unmet_requirement(&sc.requires, app_root) {
        None => Gate::Run,
        Some(reason) if sc.optional => Gate::Skip {
            kind: SkipKind::Optional,
            reason,
        },
        Some(reason) => Gate::Fail {
            reason: format!("mandatory scenario could not run: {reason} (mark it `optional = true` only if skipping is acceptable)"),
        },
    }
}

fn command_exists(name: &str) -> bool {
    if name.contains(std::path::MAIN_SEPARATOR) {
        return Path::new(name).exists();
    }
    let exts: Vec<String> = if cfg!(windows) {
        vec![".exe".into(), ".cmd".into(), ".bat".into(), String::new()]
    } else {
        vec![String::new()]
    };
    std::env::var_os("PATH")
        .map(|p| {
            std::env::split_paths(&p)
                .any(|d| exts.iter().any(|e| d.join(format!("{name}{e}")).is_file()))
        })
        .unwrap_or(false)
}

// ------------------------------------------------------------------- context

/// A handler for an app-defined step kind: gets the live [`Ctx`] (variables, the `save` map is
/// `ctx.vars`, data and log dirs, tracker, mocks) and the step's parameters with `${...}` already
/// interpolated. The value it returns is what the step's `[[step.expect]]` checks run on and what
/// `save` stores, exactly like a built-in kind.
pub type StepHandler =
    Arc<dyn for<'a> Fn(&mut Ctx<'a>, &Map<String, Value>) -> Result<Value> + Send + Sync>;

/// The app-defined step kinds of a run, by kind name. A registered kind takes precedence over a
/// built-in of the same name.
#[derive(Clone, Default)]
pub struct StepHandlers(BTreeMap<String, StepHandler>);

impl StepHandlers {
    pub fn new() -> Self {
        Self::default()
    }
    /// Register `handler` for `kind` (replacing an earlier one).
    pub fn register(
        &mut self,
        kind: &str,
        handler: impl for<'a> Fn(&mut Ctx<'a>, &Map<String, Value>) -> Result<Value>
            + Send
            + Sync
            + 'static,
    ) -> &mut Self {
        self.0.insert(kind.to_string(), Arc::new(handler));
        self
    }
    pub fn get(&self, kind: &str) -> Option<StepHandler> {
        self.0.get(kind).cloned()
    }
    pub fn kinds(&self) -> Vec<&str> {
        self.0.keys().map(String::as_str).collect()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for StepHandlers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.0.keys()).finish()
    }
}

pub struct Ctx<'a> {
    pub cfg: &'a QaConfig,
    pub engine: Option<EngineTarget>,
    pub ui_binary: Option<PathBuf>,
    pub ws: &'a QaWorkspace,
    pub tracker: &'a Tracker,
    pub scenario: String,
    pub data_dir: PathBuf,
    pub log_dir: PathBuf,
    pub vars: BTreeMap<String, Value>,
    pub sidecars: BTreeMap<String, Sidecar>,
    /// The scenario's engine: one `--stdio` sidecar per distinct env set that `cli` steps use, and the
    /// count of every engine process the scenario started (ADR-0001 item 5). [`Ctx::finish`] stops it
    /// before survivors are read. Sidecars of `sidecar_start` steps are in `sidecars`, not here.
    pub engine_session: EngineSession,
    pub mocks: BTreeMap<String, MockServer>,
    pub ui: Option<(Control, Mode)>,
    #[cfg(feature = "browser")]
    pub browser: Option<crate::browser::BrowserTarget>,
    /// The scenario's run kind. [`Ctx::new`] starts `Functional`; the suite sets the kind in
    /// force ([`scenario_kind`]). Only `Visual` allows screenshots and recordings.
    pub kind: RunKind,
    /// Files this scenario produced; the runner hashes them into the evidence bundle.
    pub artifacts: Vec<(String, PathBuf)>,
    /// App-defined step kinds; the suite fills it from `SuiteOptions::hooks`. Empty by default.
    pub handlers: StepHandlers,
    /// When set, [`Ctx::teardown`] kills tracked survivors instead of reporting them as orphans.
    pub kill_orphans: bool,
    /// The pool's world for this scenario (ports, env marker, control files); `None` outside a pool
    /// run (a harness test, a hand-built `Ctx`). See [`Ctx::with_world`].
    pub world: Option<World>,
    /// Resource tokens the pool holds for this scenario, see [`Scenario::tokens`].
    pub granted: Vec<Token>,
    /// What the built-in steps and the app's step plugin hold for this scenario ([`crate::steps`]).
    pub steps: crate::steps::StepState,
    step_no: usize,
}

impl<'a> Ctx<'a> {
    pub fn new(
        cfg: &'a QaConfig,
        mut engine: Option<EngineTarget>,
        ui_binary: Option<PathBuf>,
        ws: &'a QaWorkspace,
        tracker: &'a Tracker,
        scenario: &str,
    ) -> Result<Self> {
        let data_dir = ws.scenario_dir(scenario)?;
        // This scenario's copy of the engine target carries the test-mode pair after the ordinary
        // config env, so the cli, the sidecar and any custom step that clones `ctx.engine.env` all
        // hand the engine the bound scenario directory (rightkit-fs checks the binding at the root).
        if let Some(e) = engine.as_mut() {
            e.bind_test_mode(&data_dir);
        }
        let log_dir = ws
            .evidence_dir
            .join(data_dir.file_name().unwrap_or_default());
        std::fs::create_dir_all(&log_dir)?;
        // Templates passed through ui_command/ui_eval must never name managed storage.
        let app_evidence_dir = ws
            .home
            .join("evidence")
            .join(data_dir.file_name().unwrap_or_default());
        std::fs::create_dir_all(&app_evidence_dir)?;
        let mut vars = BTreeMap::new();
        // Every path a scenario can expand goes through `scenario_path_text`, so text such as
        // `${data_dir}/proj/README.txt` is a path Windows accepts.
        vars.insert("data_dir".into(), json!(scenario_path_text(&data_dir)));
        vars.insert(
            "evidence_dir".into(),
            json!(scenario_path_text(&app_evidence_dir)),
        );
        vars.insert("app_root".into(), json!(scenario_path_text(&cfg.app_root)));
        vars.insert(
            "engine".into(),
            json!(engine.as_ref().map(|e| scenario_path_text(&e.binary))),
        );
        let engine_session = EngineSession::new(Some(log_dir.clone()));
        Ok(Self {
            cfg,
            engine,
            ui_binary,
            ws,
            tracker,
            scenario: scenario.into(),
            data_dir,
            log_dir,
            vars,
            sidecars: BTreeMap::new(),
            engine_session,
            mocks: BTreeMap::new(),
            ui: None,
            #[cfg(feature = "browser")]
            browser: None,
            kind: RunKind::Functional,
            artifacts: vec![],
            handlers: StepHandlers::default(),
            kill_orphans: false,
            world: None,
            granted: vec![],
            steps: crate::steps::StepState::default(),
            step_no: 0,
        })
    }

    /// The 1-based number of the step being run (0 before the first).
    pub fn step_no(&self) -> usize {
        self.step_no
    }

    /// Bind the scenario to the world the pool built for it and the tokens the pool holds for it.
    /// The engine this scenario starts carries the world's environment (marker, port range, control
    /// paths, tokens), so a process finds its world in its own env and not in shared state.
    pub fn with_world(mut self, world: World, granted: Vec<Token>) -> Self {
        let pairs = crate::pool::world_env(&world, &granted);
        if let Some(e) = self.engine.as_mut() {
            e.env.retain(|(k, _)| !pairs.iter().any(|(wk, _)| wk == k));
            e.env.extend(pairs);
        }
        self.world = Some(world);
        self.granted = granted;
        self
    }

    /// The `offset`-th port of this scenario's world range (0 is the first). Refused outside a pool
    /// run and past the end of the range, so a scenario cannot bind into another world's ports.
    pub fn world_port(&self, offset: u16) -> Result<u16> {
        let w = self
            .world
            .as_ref()
            .ok_or_else(|| Error("this scenario has no world, so it has no port range".into()))?;
        let port = u32::from(w.port_start) + u32::from(offset);
        if port > u32::from(w.port_end) {
            return err(format!(
                "port offset {offset} is outside this scenario's range {}-{}",
                w.port_start, w.port_end
            ));
        }
        Ok(port as u16)
    }

    /// Refuse use of a machine-wide resource the scenario did not declare. Outside a pool run
    /// (`world` is `None`) nothing is held or checked, so existing direct users are unchanged.
    pub fn require_token(&self, token: Token, what: &str) -> Result<()> {
        if self.world.is_none() || self.granted.contains(&token) {
            return Ok(());
        }
        let name = crate::pool::token_name(token);
        err(format!(
            "{what} uses the {name} resource but scenario '{}' does not declare it; add tokens = [\"{name}\"] to the scenario",
            self.scenario
        ))
    }

    fn engine(&self) -> Result<&EngineTarget> {
        self.engine.as_ref().ok_or_else(no_engine)
    }

    fn ui(&self) -> Result<&Control> {
        self.ui
            .as_ref()
            .map(|(c, _)| c)
            .ok_or_else(|| Error("no UI app launched; add a ui_launch step first".into()))
    }

    /// Record a file this scenario produced as evidence (hashed into the bundle).
    pub fn artifact(&mut self, name: &str, path: PathBuf) {
        self.artifacts
            .push((format!("{}/{}", self.scenario, name), path));
    }

    /// Stop everything this scenario started, in reverse dependency order.
    /// Returns problems found while cleaning up (a failed stop is a failed scenario), then the
    /// engine problems of [`Ctx::finish`] (`errored` there). A caller that reports a scenario as
    /// `errored` or records survivors calls [`Ctx::finish`] instead.
    pub fn teardown(&mut self) -> Vec<String> {
        let t = self.finish();
        let mut problems = t.problems;
        problems.extend(t.errored);
        problems
    }

    /// [`Ctx::teardown`] with everything it learned kept apart. The order is the point:
    /// 1. the browser, the UI app, the scenario's own sidecars and the `cli` steps' engine
    ///    sidecars are stopped (stdin closed, a moment to exit, then the tree is killed);
    /// 2. the tracker's live processes are read into `survivors`;
    /// 3. only then is anything killed (`kill_orphans`) or reported as an orphan.
    ///
    /// So a survivor is always recorded before cleanup removes the evidence of it, and a sidecar that
    /// is stopped here can never be mistaken for one. Also writes `engine-starts.json` (the counts,
    /// per env set) and the sidecars' full stderr files as artifacts.
    pub fn finish(&mut self) -> Teardown {
        let mut problems = vec![];
        #[cfg(feature = "browser")]
        if let Some(mut b) = self.browser.take() {
            b.stop();
        }
        if let Some((mut c, mode)) = self.ui.take() {
            let focused = c.focus_requested();
            match c.stop() {
                Ok(r) => {
                    if !r.process_gone || !r.endpoint_closed {
                        problems.push(format!(
                            "UI app pid {} not fully stopped (process_gone={}, endpoint_closed={})",
                            r.pid, r.process_gone, r.endpoint_closed
                        ));
                    }
                    if mode != Mode::Visible && !focused && !r.owned_never_frontmost {
                        problems.push(format!(
                            "background proof failed: {} (states {:?}, owned frontmost pids {:?}, pid unreadable in {} sample(s))",
                            r.owned_never_frontmost_failure.as_deref().unwrap_or("unknown"),
                            r.frontmost_states,
                            r.owned_frontmost_pids,
                            r.frontmost_pid_unavailable_samples
                        ));
                    }
                }
                Err(e) => problems.push(e.0),
            }
        }
        for (_, mut s) in std::mem::take(&mut self.sidecars) {
            s.close();
        }
        self.engine_session.close_all();
        self.mocks.clear();
        // Read before any kill: `assert_no_orphans` and `kill_all` remove what they find.
        let survivors: Vec<Survivor> = self
            .tracker
            .alive()
            .into_iter()
            .map(|(pid, label)| Survivor {
                pid,
                name: label,
                owner: SurvivorOwner::RegisteredRoot,
                detail: String::new(),
            })
            .collect();
        if self.kill_orphans {
            self.tracker.kill_all();
        } else if let Err(e) = self.tracker.assert_no_orphans() {
            problems.push(e.0);
        }
        let engine_starts = self.engine_session.starts().clone();
        if engine_starts.total() > 0 {
            let path = self.log_dir.join("engine-starts.json");
            let text = serde_json::to_string_pretty(&engine_starts.to_value()).unwrap_or_default();
            match std::fs::write(&path, text) {
                Ok(()) => self.artifact("engine-starts.json", path),
                Err(e) => problems.push(format!("engine-starts.json could not be written: {e}")),
            }
        }
        for file in self.engine_session.stderr_files().to_vec() {
            if let Some(name) = file.file_name().map(|n| n.to_string_lossy().into_owned()) {
                self.artifact(&name, file);
            }
        }
        Teardown {
            problems,
            errored: engine_starts.errored(),
            survivors,
            engine_starts,
        }
    }
}

/// What stopping a scenario found (see [`Ctx::finish`]).
#[derive(Debug, Clone)]
pub struct Teardown {
    /// Failures: a UI app not fully stopped, a tracked process left running (unless `kill_orphans`).
    pub problems: Vec<String>,
    /// Why the scenario is `errored` on account of its engines: sidecars that failed to start, died or
    /// went silent, then `engine starts N > allowed M` (Sage C3(b)). Empty for a clean scenario.
    pub errored: Vec<String>,
    /// Tracked processes still alive once everything above was stopped, read before any was killed.
    pub survivors: Vec<Survivor>,
    /// The engine-start counts, total and per env set; `engine_starts.total()` is the receipt's
    /// `engine_starts`.
    pub engine_starts: EngineStarts,
}

// --------------------------------------------------------------------- params

pub(crate) fn pv<'p>(p: &'p Map<String, Value>, k: &str) -> Option<&'p Value> {
    p.get(k).filter(|v| !v.is_null())
}
pub(crate) fn ps(p: &Map<String, Value>, k: &str) -> Result<String> {
    pv(p, k)
        .and_then(Value::as_str)
        .map(String::from)
        .ok_or_else(|| Error(format!("step needs string '{k}'")))
}
pub(crate) fn ps_opt(p: &Map<String, Value>, k: &str) -> Option<String> {
    pv(p, k).and_then(Value::as_str).map(String::from)
}
pub(crate) fn pf(p: &Map<String, Value>, k: &str) -> Result<f64> {
    pv(p, k)
        .and_then(Value::as_f64)
        .ok_or_else(|| Error(format!("step needs number '{k}'")))
}
pub(crate) fn pf_or(p: &Map<String, Value>, k: &str, d: f64) -> f64 {
    pv(p, k).and_then(Value::as_f64).unwrap_or(d)
}
pub(crate) fn pb(p: &Map<String, Value>, k: &str) -> bool {
    pv(p, k).and_then(Value::as_bool).unwrap_or(false)
}
pub(crate) fn dur(p: &Map<String, Value>, k: &str, default_s: f64) -> Duration {
    Duration::from_millis((pf_or(p, k, default_s) * 1000.0) as u64)
}
/// A Windows verbatim path (`\\?\C:\dir`, `\\?\UNC\server\share`) in its ordinary form
/// (`C:\dir`, `\\server\share`), when it fits the ordinary length limit. Anything else is
/// returned unchanged, including a verbatim path too long to be written without the prefix.
///
/// A canonicalised path on Windows is verbatim, and under that prefix Windows does no
/// normalisation: `/` is a file-name character, not a separator. Scenario text joins with `/`.
pub fn strip_verbatim(text: &str) -> String {
    // Also keeps the path verbatim for a reserved device name or a component Windows would
    // rewrite (see `util::plain_path_text`).
    crate::util::plain_path_text(text)
}

/// The text a scenario variable holds for `path`: the path itself, in ordinary (non-verbatim)
/// form on Windows where it fits.
fn scenario_path_text(path: &Path) -> String {
    let text = path.to_string_lossy();
    if cfg!(windows) {
        strip_verbatim(&text)
    } else {
        text.into_owned()
    }
}

/// `s` as a path of this host. On Windows every `/` becomes `\`, which is the same path for an
/// ordinary one and the only correct reading for a verbatim one.
fn host_path(s: &str) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(s.replace('/', "\\"))
    } else {
        PathBuf::from(s)
    }
}

/// A scenario path: absolute as it is, relative inside `data_dir`. Separators are normalised
/// after the join, so a relative `a/b` under a verbatim `data_dir` is still two components.
fn resolve_in(data_dir: &Path, s: &str) -> PathBuf {
    let p = host_path(s);
    if p.is_absolute() {
        p
    } else {
        data_dir.join(p)
    }
}

fn resolve(ctx: &Ctx, s: &str) -> PathBuf {
    resolve_in(&ctx.data_dir, s)
}
fn env_pairs(p: &Map<String, Value>) -> Vec<(String, String)> {
    pv(p, "env")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        v.as_str()
                            .map(String::from)
                            .unwrap_or_else(|| v.to_string()),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}
fn cli_opts(ctx: &Ctx, p: &Map<String, Value>) -> CliOptions {
    CliOptions {
        data_dir: Some(ctx.data_dir.clone()),
        env: env_pairs(p),
        input: pv(p, "input").cloned(),
        timeout: Some(dur(p, "timeout_s", 120.0)),
    }
}

// ------------------------------------------------------------------- execution

#[derive(Debug, Clone)]
pub struct StepRecord {
    pub index: usize,
    pub kind: String,
    pub name: String,
    pub ok: bool,
    pub ms: u128,
    pub error: Option<String>,
}

/// Default gap between `until` retries. The retried check is local to the harness (a UI read or an
/// engine call), so a long gap is dead time; override per step with `interval_ms`.
pub const DEFAULT_UNTIL_INTERVAL_MS: u64 = 25;

/// Run one step including retry-until-expectations-hold.
pub fn run_step(ctx: &mut Ctx, step: &Step) -> Result<Value> {
    ctx.step_no += 1;
    if let Some(name) = step
        .save
        .as_deref()
        .filter(|n| crate::plugin::is_build_var(n))
    {
        // The runner's build record: a scenario that saves over it would repoint `${build.<role>}`.
        return Err(Error(format!(
            "{} step {} ({}): `save = \"{name}\"` names a variable the runner owns (`build`)",
            crate::plugin::ERRORED_MARK,
            ctx.step_no,
            step.kind
        )));
    }
    let started = Instant::now();
    let until = step.until_timeout_s.map(Duration::from_secs);
    let interval = Duration::from_millis(step.interval_ms.unwrap_or(DEFAULT_UNTIL_INTERVAL_MS));
    loop {
        let attempt: Result<Value> = (|| {
            let resolved = interpolate(
                &Value::Object(step.params.clone().into_iter().collect()),
                &ctx.vars,
            )?;
            let params = resolved.as_object().cloned().unwrap_or_default();
            let result = exec(ctx, &step.kind, &params)?;
            let expects: Vec<Expect> = step
                .expect
                .iter()
                .map(|e| resolve_expect(e, &ctx.vars))
                .collect::<Result<_>>()?;
            check_all(&result, &expects)?;
            Ok(result)
        })();
        match attempt {
            Ok(v) => {
                if let Some(name) = &step.save {
                    ctx.vars.insert(name.clone(), v.clone());
                }
                return Ok(v);
            }
            Err(e) => match until {
                Some(limit) if started.elapsed() < limit => std::thread::sleep(interval),
                _ => {
                    return Err(Error(format!(
                        "step {} ({}): {}",
                        ctx.step_no, step.kind, e.0
                    )))
                }
            },
        }
    }
}

fn resolve_expect(e: &Expect, vars: &BTreeMap<String, Value>) -> Result<Expect> {
    let mut out = e.clone();
    if let Some(v) = &e.eq {
        out.eq = Some(interpolate(v, vars)?);
    }
    if let Some(v) = &e.ne {
        out.ne = Some(interpolate(v, vars)?);
    }
    if let Some(v) = &e.contains {
        out.contains = Some(interpolate(v, vars)?);
    }
    Ok(out)
}

/// The `commands` of an `engine_seed` step: a non-empty array of tables, each a `cli` step's parameters.
pub(crate) fn seed_commands(p: &Map<String, Value>) -> Result<Vec<Map<String, Value>>> {
    let list = pv(p, "commands").and_then(Value::as_array).ok_or_else(|| {
        Error("engine_seed needs a `commands` array of cli parameter tables".into())
    })?;
    if list.is_empty() {
        return err("engine_seed `commands` is empty");
    }
    list.iter()
        .enumerate()
        .map(|(i, c)| {
            let m = c
                .as_object()
                .cloned()
                .ok_or_else(|| Error(format!("engine_seed command {} is not a table", i + 1)))?;
            if pv(&m, "command").is_none() && pv(&m, "args").is_none() {
                return err(format!(
                    "engine_seed command {} needs 'command' or 'args'",
                    i + 1
                ));
            }
            Ok(m)
        })
        .collect()
}

fn no_engine() -> Error {
    Error(
        "no engine binary configured (set [engine].binary, RIGHTKIT_QA_ENGINE, or pass --build)"
            .into(),
    )
}

/// The words of a `cli`-family step: `args` (a list) or `command` (split on whitespace).
fn cli_args(p: &Map<String, Value>) -> Result<Vec<String>> {
    match (pv(p, "args"), ps_opt(p, "command")) {
        (Some(Value::Array(a)), _) => Ok(a
            .iter()
            .map(|v| {
                v.as_str()
                    .map(String::from)
                    .unwrap_or_else(|| v.to_string())
            })
            .collect()),
        (_, Some(c)) => Ok(c.split_whitespace().map(String::from).collect()),
        _ => err("cli step needs 'command' or 'args'"),
    }
}

/// Which step kind is calling the engine; it decides how the call may be routed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Lane {
    /// `cli`: the scenario's sidecar, unless the step declares `lifecycle = "cold"` and a `cold_reason`.
    Session,
    /// `cold_cli`: always a cold process; `cold_reason` (or `reason`) is required.
    ColdStep,
    /// `engine_seed`: each command is a cold process, declared as `startup` (it runs before the UI app,
    /// which opens the store next, so no sidecar of the scenario may hold it).
    Seed,
}

fn cold_reason_of(reason: Option<String>) -> Result<&'static str> {
    match reason {
        Some(r) => parse_cold_reason(&r),
        None => err(format!(
            "a cold launch names its reason: cold_reason = one of {}",
            COLD_REASONS.join(", ")
        )),
    }
}

/// Where a `cli`-family step goes (Sage C3(a)): the sidecar by default, a declared cold launch with
/// `lifecycle = "cold"` and exactly one `cold_reason` from [`COLD_REASONS`].
fn cli_route(p: &Map<String, Value>, lane: Lane) -> Result<EngineRoute> {
    let lifecycle = ps_opt(p, "lifecycle");
    let reason = ps_opt(p, "cold_reason");
    match lane {
        Lane::Seed => Ok(EngineRoute::Cold("startup")),
        Lane::ColdStep => {
            if let Some(l) = lifecycle.as_deref().filter(|l| *l != "cold") {
                return err(format!(
                    "cold_cli is always cold: lifecycle `{l}` is not allowed"
                ));
            }
            Ok(EngineRoute::Cold(cold_reason_of(
                reason.or_else(|| ps_opt(p, "reason")),
            )?))
        }
        Lane::Session => match lifecycle.as_deref() {
            None | Some("session") => {
                if reason.is_some() {
                    return err("cold_reason needs lifecycle = \"cold\"");
                }
                Ok(EngineRoute::Session)
            }
            Some("cold") => Ok(EngineRoute::Cold(cold_reason_of(reason)?)),
            Some(other) => err(format!(
                "lifecycle must be `session` or `cold`, not `{other}`"
            )),
        },
    }
}

/// One engine CLI call: the body of the `cli` and `cold_cli` steps and of each `engine_seed`
/// command. The call's transcript is written to `log_name` in the scenario's evidence dir.
///
/// A `cli` step is served by the scenario's `--stdio` sidecar for its `env` set (started on first use),
/// and by a cold process only when the step declares it. The result has the shape a cold call returns
/// plus `via`, `env_set`, `exit_code_source` (and `cold_reason`); see [`crate::engine::Served::to_value`].
fn run_cli(ctx: &mut Ctx, p: &Map<String, Value>, log_name: &str, lane: Lane) -> Result<Value> {
    let args = cli_args(p)?;
    let route = cli_route(p, lane)?;
    let opts = cli_opts(ctx, p);
    let label = format!("step {} `{}`", ctx.step_no, args.join(" "));
    let target = ctx.engine.as_ref().ok_or_else(no_engine)?;
    let host = TargetHost {
        target,
        tracker: ctx.tracker,
    };
    let served = ctx.engine_session.cli(
        &host,
        &CliCall {
            args: &args,
            opts: &opts,
            label: &label,
        },
        route,
    )?;
    let r = &served.result;
    let log = ctx.log_dir.join(log_name);
    std::fs::write(
        &log,
        format!(
            "$ {}\nvia {:?} (env set {}{}), exit code from {}\nexit {:?}\n--- stdout\n{}\n--- stderr\n{}\n",
            args.join(" "),
            served.via,
            served.env_set,
            served
                .cold_reason
                .map(|c| format!(", cold: {c}"))
                .unwrap_or_default(),
            served.exit_code_source,
            r.code,
            r.stdout,
            r.stderr
        ),
    )?;
    ctx.steps.note_cli_log(log.clone());
    ctx.artifact(log_name, log);
    let ok = r.code == Some(0) && r.json.get("error").map(Value::is_null).unwrap_or(true);
    if !ok && !pb(p, "allow_error") {
        return err(format!(
            "engine `{}` failed (exit {:?}{}): stdout {} stderr tail {}",
            args.join(" "),
            r.code,
            if r.timed_out { ", timed out" } else { "" },
            crate::util::tail(&r.stdout, 600),
            crate::util::tail(&r.stderr, 600)
        ));
    }
    Ok(served.to_value())
}

impl Ctx<'_> {
    /// Run `command` (one served method) with `input` cold and in-session and compare them: JSON body,
    /// error class and exit code (Sage C3(c)). The stdio protocol carries the exit code only when a
    /// response has `exit_code`; otherwise the report says only body and error class were compared and
    /// records `exit_code_source: "derived"`. The cold launch is declared as `parity`. The command runs
    /// twice: use a read-only or idempotent one, and not a `store-lock` command. The transcript of both
    /// answers is the step's `NN-parity.log`. `env` picks the sidecar, as for a `cli` step.
    pub fn parity(
        &mut self,
        command: &str,
        input: Option<Value>,
        env: Vec<(String, String)>,
        timeout: Duration,
    ) -> Result<ParityReport> {
        let args: Vec<String> = command.split_whitespace().map(String::from).collect();
        let opts = CliOptions {
            data_dir: Some(self.data_dir.clone()),
            env,
            input,
            timeout: Some(timeout),
        };
        let label = format!("step {} parity `{command}`", self.step_no);
        let target = self.engine.as_ref().ok_or_else(no_engine)?;
        let host = TargetHost {
            target,
            tracker: self.tracker,
        };
        let report = self.engine_session.parity(
            &host,
            &CliCall {
                args: &args,
                opts: &opts,
                label: &label,
            },
        )?;
        let name = format!("{:02}-parity.log", self.step_no);
        let log = self.log_dir.join(&name);
        let side = |title: &str, r: &crate::engine::CliResult| {
            format!(
                "--- {title}: exit {:?}\n{}\n--- {title} stderr\n{}\n",
                r.code, r.stdout, r.stderr
            )
        };
        std::fs::write(
            &log,
            format!(
                "$ {command}\nequal {}\ncompares {}\ndifferences {:?}\n{}{}",
                report.equal(),
                report.to_value()["compares"].as_str().unwrap_or(""),
                report.differences,
                side("cold", &report.cold),
                side("in-session", &report.session)
            ),
        )?;
        self.steps.note_cli_log(log.clone());
        self.artifact(&name, log);
        Ok(report)
    }
}

fn exec(ctx: &mut Ctx, kind: &str, p: &Map<String, Value>) -> Result<Value> {
    if let Some(handler) = ctx.handlers.get(kind) {
        return handler(ctx, p);
    }
    match kind {
        // ------------------------------------------------------------ engine
        // Served by the scenario's `--stdio` sidecar for the step's `env` set. `lifecycle = "cold"` with
        // one `cold_reason` (startup, restart, recovery, role-change, store-lock, cli-entry,
        // non-method) launches a cold process instead.
        "cli" => {
            let log = format!("{:02}-cli.log", ctx.step_no);
            run_cli(ctx, p, &log, Lane::Session)
        }
        // A cold process on purpose: the `cli` parameters plus `cold_reason` (or `reason`), required.
        "cold_cli" => {
            let log = format!("{:02}-cold-cli.log", ctx.step_no);
            run_cli(ctx, p, &log, Lane::ColdStep)
        }
        // Close the `env` set's sidecar and open a new one now (a declared, counted restart).
        "restart" => {
            let opts = cli_opts(ctx, p);
            let target = ctx.engine.as_ref().ok_or_else(no_engine)?;
            let host = TargetHost {
                target,
                tracker: ctx.tracker,
            };
            ctx.engine_session.restart(&host, &opts)
        }
        // Kill the `env` set's sidecar; the engine must recover on the next call to that set (a declared,
        // counted restart).
        "kill" => {
            let opts = cli_opts(ctx, p);
            ctx.engine_session.kill(&opts)
        }
        // `command` (one served method), `input`, `env`, `timeout_s`: run it cold and in-session and fail
        // on any difference in body, error class or (when the protocol carries it) exit code.
        "parity" => {
            let command = match (ps_opt(p, "command"), pv(p, "args")) {
                (Some(c), _) => c,
                (None, Some(_)) => cli_args(p)?.join(" "),
                _ => return err("parity step needs 'command'"),
            };
            let report = ctx.parity(
                &command,
                pv(p, "input").cloned(),
                env_pairs(p),
                dur(p, "timeout_s", 120.0),
            )?;
            if !report.equal() {
                return err(format!(
                    "parity failed for `{}`: {}",
                    report.command,
                    report.differences.join("; ")
                ));
            }
            Ok(report.to_value())
        }
        // Seed the engine's state before the UI starts: each entry of `commands` is a `cli` step's
        // parameters (`command` or `args`, `input`, `env`, `allow_error`, `timeout_s`), run in order
        // against the scenario's data dir, the one `ui_launch` then hands the app. A `ui` scenario
        // needs the engine binary only when it uses this step.
        "engine_seed" => {
            if ctx.ui.is_some() {
                return err("engine_seed must run before ui_launch: the app is already running and may hold the state it would seed");
            }
            let commands = seed_commands(p)?;
            let mut results = Vec::new();
            for (i, one) in commands.iter().enumerate() {
                let log = format!("{:02}-seed-{:02}.log", ctx.step_no, i + 1);
                results.push(
                    run_cli(ctx, one, &log, Lane::Seed)
                        .map_err(|e| Error(format!("engine_seed command {}: {}", i + 1, e.0)))?,
                );
            }
            Ok(json!({"seeded": results.len(), "results": results}))
        }
        "sidecar_start" => {
            let id = ps_opt(p, "id").unwrap_or_else(|| "sidecar".into());
            let s = ctx
                .engine()?
                .start_sidecar(&cli_opts(ctx, p), ctx.tracker)?;
            let mut s = s;
            s.set_frame_shape(pv(p, "legacy_status_frames").and_then(Value::as_bool));
            let hello = s.hello.clone();
            ctx.sidecars.insert(id, s);
            // An engine process the scenario asked for by name: counted, and its own declaration.
            ctx.engine_session.note_scenario_sidecar();
            Ok(hello)
        }
        "sidecar_command" => {
            let id = ps_opt(p, "id").unwrap_or_else(|| "sidecar".into());
            let name = ps(p, "name")?;
            let input = pv(p, "input").cloned().unwrap_or_else(|| json!({}));
            let timeout = dur(p, "timeout_s", 120.0);
            let s = ctx
                .sidecars
                .get_mut(&id)
                .ok_or_else(|| Error(format!("no sidecar '{id}'")))?;
            let legacy = pv(p, "legacy_status_frames").and_then(Value::as_bool);
            match s.command_with(&name, input, timeout, legacy) {
                Ok(v) => Ok(v),
                Err(e) if pb(p, "allow_error") => Ok(json!({"error": e.0})),
                Err(e) => Err(e),
            }
        }
        "sidecar_raw" => {
            let id = ps_opt(p, "id").unwrap_or_else(|| "sidecar".into());
            let frame = pv(p, "frame")
                .cloned()
                .ok_or_else(|| Error("sidecar_raw needs 'frame'".into()))?;
            let timeout = dur(p, "timeout_s", 60.0);
            ctx.sidecars
                .get_mut(&id)
                .ok_or_else(|| Error(format!("no sidecar '{id}'")))?
                .raw(frame, timeout)
        }
        "sidecar_close" => {
            let id = ps_opt(p, "id").unwrap_or_else(|| "sidecar".into());
            let mut s = ctx
                .sidecars
                .remove(&id)
                .ok_or_else(|| Error(format!("no sidecar '{id}'")))?;
            let code = s.close();
            Ok(json!({"exit_code": code, "stderr_tail": s.stderr_tail()}))
        }
        // -------------------------------------------------------------- mocks
        "mock_start" => {
            let id = ps_opt(p, "id").unwrap_or_else(|| "mock".into());
            let routes = pv(p, "routes")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .map(|r| Route {
                            method: r["method"].as_str().map(String::from),
                            path: r["path"].as_str().unwrap_or("/").to_string(),
                            status: r["status"].as_u64().unwrap_or(200) as u16,
                            body: r
                                .get("json")
                                .or_else(|| r.get("body"))
                                .cloned()
                                .unwrap_or(Value::Null),
                            content_type: r["content_type"].as_str().map(String::from),
                        })
                        .collect()
                })
                .unwrap_or_default();
            let m = MockServer::start(routes)?;
            let base = m.base.clone();
            ctx.mocks.insert(id, m);
            Ok(json!({"base": base}))
        }
        "mock_requests" => {
            let id = ps_opt(p, "id").unwrap_or_else(|| "mock".into());
            let m = ctx
                .mocks
                .get(&id)
                .ok_or_else(|| Error(format!("no mock '{id}'")))?;
            Ok(json!({"requests": m.seen().iter().map(|s| s.to_value()).collect::<Vec<_>>()}))
        }
        "mock_stop" => {
            let id = ps_opt(p, "id").unwrap_or_else(|| "mock".into());
            ctx.mocks.remove(&id);
            Ok(json!({"stopped": true}))
        }
        // -------------------------------------------------------------- files
        "write_wav" => {
            let path = resolve(ctx, &ps(p, "path")?);
            media::write_reference_wav(&path, pf_or(p, "seconds", 6.0))?;
            Ok(json!({"path": path.to_string_lossy()}))
        }
        "write_file" => {
            let path = resolve(ctx, &ps(p, "path")?);
            if let Some(d) = path.parent() {
                std::fs::create_dir_all(d)?;
            }
            std::fs::write(&path, ps_opt(p, "content").unwrap_or_default())?;
            Ok(json!({"path": path.to_string_lossy()}))
        }
        "file_stat" => Ok(media::file_stat(&resolve(ctx, &ps(p, "path")?))),
        "wav_stats" => media::wav_stats(&resolve(ctx, &ps(p, "path")?)),
        "png_size" => media::png_size(&resolve(ctx, &ps(p, "path")?)),
        "ffprobe" => match media::ffprobe(&resolve(ctx, &ps(p, "path")?), ctx.tracker) {
            Ok(v) => Ok(v),
            Err(e) if pb(p, "optional") => Ok(json!({"skipped": e.0})),
            Err(e) => Err(e),
        },
        // --------------------------------------------------------------- misc
        "set" => Ok(pv(p, "value").cloned().unwrap_or(Value::Null)),
        "sleep" => {
            sleep_ms(pf_or(p, "ms", 100.0) as u64);
            Ok(json!({"slept_ms": pf_or(p, "ms", 100.0)}))
        }
        "assert_no_orphans" => {
            // Mid-scenario: the runner's own live sidecars and the step plugin are alive on purpose
            // (teardown retires them and checks them strictly), everything else is reported.
            ctx.tracker.assert_no_foreign_orphans()?;
            Ok(json!({"alive": 0}))
        }
        // ------------------------------------------------------------- UI app
        "ui_launch" => {
            if ctx.ui.is_some() {
                return err("a UI app is already running in this scenario");
            }
            let binary = match ps_opt(p, "binary") {
                Some(b) => ctx.cfg.app_root.join(b),
                None => ctx.ui_binary.clone().ok_or_else(|| Error("no UI binary configured: set [ui].binaries, pass --ui-binary, or `binary` on the step".into()))?,
            };
            let mode = Mode::parse(&ps_opt(p, "mode").unwrap_or_else(|| ctx.cfg.ui.mode.clone()))?;
            let mut env: Vec<(String, String)> = ctx
                .cfg
                .ui
                .env
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            env.extend(ctx.cfg.env.iter().map(|(k, v)| (k.clone(), v.clone())));
            if let Some(var) = &ctx.cfg.engine.data_dir_env {
                env.push((var.clone(), ctx.data_dir.to_string_lossy().into()));
            }
            env.extend(env_pairs(p));
            let focus = pv(p, "focus")
                .and_then(Value::as_bool)
                .unwrap_or(ctx.cfg.ui.focus);
            // Keyboard and window focus is one per machine: a scenario that takes it declares it.
            if focus {
                ctx.require_token(Token::Focus, "ui_launch with focus")?;
            }
            let spec = LaunchSpec {
                binary,
                mode,
                env,
                startup_timeout: Duration::from_secs(ctx.cfg.ui.startup_timeout_s.unwrap_or(90)),
                label: format!("{} app", ctx.cfg.app),
                focus,
            };
            let c = control::launch(&spec, ctx.ws, ctx.tracker)?;
            let out = json!({"pid": c.pid, "service": c.service, "focus": focus, "mode": format!("{mode:?}").to_lowercase(), "health": c.health()?, "log": c.log_path.to_string_lossy()});
            let log = c.log_path.clone();
            ctx.ui = Some((c, mode));
            ctx.artifact("app.log", log);
            Ok(out)
        }
        "ui_stop" => {
            let (mut c, mode) = ctx
                .ui
                .take()
                .ok_or_else(|| Error("no UI app running".into()))?;
            let focused = c.focus_requested();
            let r = c.stop()?;
            if !r.process_gone || !r.endpoint_closed {
                return err(format!(
                    "app pid {} not fully stopped (process_gone={}, endpoint_closed={})",
                    r.pid, r.process_gone, r.endpoint_closed
                ));
            }
            if mode != Mode::Visible && !focused && !r.owned_never_frontmost {
                return err(format!(
                    "background proof failed: {} (states {:?}, owned frontmost pids {:?}, pid unreadable in {} sample(s))",
                    r.owned_never_frontmost_failure.as_deref().unwrap_or("unknown"),
                    r.frontmost_states,
                    r.owned_frontmost_pids,
                    r.frontmost_pid_unavailable_samples
                ));
            }
            Ok(
                json!({"pid": r.pid, "endpoint_closed": r.endpoint_closed, "process_gone": r.process_gone, "frontmost_unchanged": r.frontmost_unchanged, "owned_never_frontmost": r.owned_never_frontmost, "owned_frontmost_pids": r.owned_frontmost_pids, "frontmost_pid_unavailable_samples": r.frontmost_pid_unavailable_samples, "owned_never_frontmost_failure": r.owned_never_frontmost_failure}),
            )
        }
        "ui_auth_probe" => {
            let c = ctx.ui()?;
            Ok(json!({"unlisted_app_error": c.unlisted_app_probe()?}))
        }
        "ui_click" => {
            let c = ctx.ui()?;
            let button = ps_opt(p, "button").unwrap_or_else(|| "left".into());
            let count = pf_or(p, "count", 1.0) as u32;
            let (x, y) = match ps_opt(p, "selector") {
                Some(sel) => c.center_of(&sel)?,
                None => (pf(p, "x")?, pf(p, "y")?),
            };
            c.click(x, y, &button, count)?;
            Ok(json!({"x": x, "y": y}))
        }
        "ui_move" => {
            let c = ctx.ui()?;
            let (x, y) = match ps_opt(p, "selector") {
                Some(sel) => c.center_of(&sel)?,
                None => (pf(p, "x")?, pf(p, "y")?),
            };
            c.move_to(x, y)?;
            Ok(json!({"x": x, "y": y}))
        }
        "ui_drag" => {
            let c = ctx.ui()?;
            let from = match ps_opt(p, "from_selector") {
                Some(s) => c.center_of(&s)?,
                None => (pf(p, "x1")?, pf(p, "y1")?),
            };
            let to = match ps_opt(p, "to_selector") {
                Some(s) => c.center_of(&s)?,
                None => (pf(p, "x2")?, pf(p, "y2")?),
            };
            c.drag(from, to, pf_or(p, "steps", 12.0) as u32)?;
            Ok(json!({"from": [from.0, from.1], "to": [to.0, to.1]}))
        }
        "ui_wheel" => {
            let c = ctx.ui()?;
            let (x, y) = match ps_opt(p, "selector") {
                Some(sel) => c.center_of(&sel)?,
                None => (pf_or(p, "x", 100.0), pf_or(p, "y", 100.0)),
            };
            c.wheel(x, y, pf_or(p, "dx", 0.0), pf_or(p, "dy", 0.0))?;
            Ok(json!({"x": x, "y": y}))
        }
        "ui_key" => {
            ctx.ui()?.key(&ps(p, "key")?)?;
            Ok(json!({"ok": true}))
        }
        "ui_type" => {
            ctx.ui()?.type_text(&ps(p, "text")?)?;
            Ok(json!({"ok": true}))
        }
        "ui_eval" => ctx.ui()?.eval(&ps(p, "js")?),
        "ui_wait_eval" => ctx
            .ui()?
            .wait_eval(&ps(p, "js")?, dur(p, "timeout_s", 20.0)),
        "ui_dom" => Ok(json!({"elements": ctx.ui()?.dom(&ps(p, "selector")?)?})),
        "ui_ax" => Ok(json!({"nodes": ctx.ui()?.ax()?})),
        // Role/name assertion over the accessibility nodes: `role`, `name`, `name_contains`, `named`,
        // `disabled`, `min_count` (default 1), `max_count`. Fails listing the roles and names found.
        "ui_assert_ax" => {
            let q = a11y::AxQuery::from_params(p)?;
            a11y::ax_assert(&ctx.ui()?.ax()?, &q)
        }
        // Text contrast of `selector` against its effective background (WCAG 2.x). Fails below the
        // minimum: `min`, else `level` (`AA` default, `AAA`) for the text size (`large` overrides the
        // size read from the page). `report_only = true` reports the ratio and never fails.
        "ui_contrast" => {
            let selector = ps(p, "selector")?;
            let probe = ctx.ui()?.eval(&a11y::contrast_probe_js(&selector))?;
            let opts = a11y::ContrastOptions {
                level: ps_opt(p, "level"),
                min: pv(p, "min").and_then(Value::as_f64),
                large: pv(p, "large").and_then(Value::as_bool),
                report_only: pb(p, "report_only"),
            };
            a11y::contrast_report(&selector, &probe, &opts)
        }
        // Keyboard-focus evidence: whether the focused element shows a visible indicator (computed
        // outline or box-shadow), its identity, and `:focus-visible`. Reports `visible`; assert it
        // with `[[step.expect]]`. `selector` adds `matches` (is the focused element that one).
        // `screenshot = true` also saves a capture as evidence (a visual scenario only).
        "ui_focus_ring" => {
            let selector = ps_opt(p, "selector");
            let probe = ctx.ui()?.eval(&a11y::focus_probe_js(selector.as_deref()))?;
            let mut out = a11y::focus_ring_verdict(&probe);
            if pb(p, "screenshot") {
                ensure_capture_allowed(ctx.kind, &ctx.scenario, "ui_focus_ring screenshot")?;
                let name = ps_opt(p, "name").unwrap_or_else(|| format!("focus-{:02}", ctx.step_no));
                let path = ctx.log_dir.join(format!("{name}.png"));
                let png = ctx.ui()?.screenshot_png()?;
                media::png_size_bytes(&png, &path)?;
                std::fs::write(&path, &png)?;
                ctx.artifact(&format!("{name}.png"), path.clone());
                out["screenshot"] = json!(path.to_string_lossy());
            }
            Ok(out)
        }
        "ui_command" => ctx
            .ui()?
            .command(&ps(p, "name")?, pv(p, "args").unwrap_or(&Value::Null)),
        "ui_screenshot" => {
            ensure_capture_allowed(ctx.kind, &ctx.scenario, "ui_screenshot")?;
            let name = ps_opt(p, "name").unwrap_or_else(|| format!("shot-{:02}", ctx.step_no));
            let path = ctx.log_dir.join(format!("{name}.png"));
            let png = ctx.ui()?.screenshot_png()?;
            let size = media::png_size_bytes(&png, &path)?;
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&path, &png)?;
            // Only evidence is hashed into the bundle; `evidence = false` keeps a working capture out of it.
            if pv(p, "evidence").and_then(Value::as_bool) != Some(false) {
                ctx.artifact(&format!("{name}.png"), path.clone());
            }
            Ok(json!({"path": path.to_string_lossy(), "size": size}))
        }
        "ui_batch" => {
            // `steps = [{method = "click", params = {x = 1, y = 2}}, ...]`: one round trip.
            let steps: Vec<(String, Value)> = pv(p, "steps")
                .and_then(Value::as_array)
                .ok_or_else(|| Error("ui_batch needs a `steps` array".into()))?
                .iter()
                .map(|s| {
                    let m = s
                        .get("method")
                        .and_then(Value::as_str)
                        .ok_or_else(|| Error("ui_batch step needs a `method`".into()))?;
                    Ok((
                        m.to_string(),
                        s.get("params").cloned().unwrap_or_else(|| json!({})),
                    ))
                })
                .collect::<Result<_>>()?;
            for (m, _) in &steps {
                if is_capture_method(m) {
                    ensure_capture_allowed(
                        ctx.kind,
                        &ctx.scenario,
                        &format!("ui_batch method '{m}'"),
                    )?;
                }
            }
            let refs: Vec<(&str, Value)> =
                steps.iter().map(|(m, p)| (m.as_str(), p.clone())).collect();
            let results = ctx.ui()?.batch(&refs)?;
            Ok(json!({"results": results}))
        }
        "ui_measure_paint" => {
            let c = ctx.ui()?;
            let trigger_sel = ps(p, "click_selector")?;
            let id = format!("paint-{}", util::new_id());
            let m = page::measure_paint(
                c,
                &mut || c.click_selector(&trigger_sel),
                &id,
                ps_opt(p, "selector").as_deref(),
                pf_or(p, "settle_ms", page::DEFAULT_SETTLE_MS as f64) as u64,
                pf_or(p, "timeout_ms", page::DEFAULT_PAINT_TIMEOUT_MS as f64) as u64,
            )?;
            if let Some(budget) = pv(p, "budget_ms").and_then(Value::as_f64) {
                page::assert_paint_within(&m, budget, &trigger_sel)?;
            }
            Ok(
                json!({"duration_ms": m.duration_ms, "settle_ms": m.settle_ms, "mutations": m.mutations, "animation_frames": m.animation_frames}),
            )
        }
        "ui_assert_pane" => {
            let g = page::read_pane_geometry(ctx.ui()?, &ps(p, "selector")?)?;
            page::assert_active_pane(&g)?;
            Ok(
                json!({"x": g.x, "y": g.y, "width": g.width, "height": g.height, "matches": g.matches}),
            )
        }
        "ui_resolve_frame" => {
            let path = page::resolve_unique_frame(
                ctx.ui()?,
                &ps(p, "selector")?,
                pf_or(p, "max_depth", 4.0) as usize,
            )?;
            Ok(json!({"frame": path, "described": page::describe_frame(&path)}))
        }
        "ui_eval_in_frame" => {
            let frame: Vec<usize> = pv(p, "frame")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|i| i.as_u64().map(|i| i as usize))
                        .collect()
                })
                .unwrap_or_default();
            page::eval_in_frame(ctx.ui()?, &frame, &ps(p, "js")?)
        }
        #[cfg(feature = "browser")]
        k if k.starts_with("browser_") => crate::browser::exec(ctx, k, p),
        other => crate::steps::dispatch(ctx, other, p),
    }
}

#[cfg(feature = "browser")]
pub(crate) mod params {
    pub(crate) use super::{dur, pb, pf_or, ps, ps_opt, pv};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_scenario_toml_with_flattened_params_and_expects() {
        let text = r#"
name = "engine doctor"
tier = "fast"
[[step]]
kind = "cli"
command = "doctor"
save = "doc"
  [[step.expect]]
  path = "json.contract_version"
  gte = 2
[[step]]
kind = "cli"
args = ["spend", "status"]
input = { a = 1 }
"#;
        let path = std::env::temp_dir().join(format!("rkqa-{}.scenario.toml", util::new_id()));
        std::fs::write(&path, text).unwrap();
        let s = Scenario::load(&path).unwrap();
        assert_eq!(s.steps.len(), 2);
        assert_eq!(s.steps[0].params["command"], json!("doctor"));
        assert_eq!(s.steps[0].expect[0].gte, Some(2.0));
        assert_eq!(s.steps[1].params["input"], json!({"a": 1}));
        let _ = std::fs::remove_file(path);
    }

    /// `focus` on `ui_launch` is a plain boolean step parameter; absent means the default (off).
    #[test]
    fn ui_launch_focus_param_parses() {
        let text = r#"
name = "focus ring"
target = "ui"
[[step]]
kind = "ui_launch"
focus = true
[[step]]
kind = "ui_key"
key = "Tab"
[[step]]
kind = "ui_stop"
"#;
        let path =
            std::env::temp_dir().join(format!("rkqa-focus-{}.scenario.toml", util::new_id()));
        std::fs::write(&path, text).unwrap();
        let s = Scenario::load(&path);
        let _ = std::fs::remove_file(&path);
        let s = s.unwrap();
        assert_eq!(
            pv(&s.steps[0].params.clone().into_iter().collect(), "focus"),
            Some(&json!(true))
        );
        assert!(!pb(
            &s.steps[2].params.clone().into_iter().collect(),
            "focus"
        ));
    }

    #[test]
    fn tier_selection_and_requirements() {
        assert!(tier_selected("fast", "fast"));
        assert!(!tier_selected("paid", "fast"));
        assert!(tier_selected("paid", "all"));
        assert!(tier_selected("local-models", "fast,local-models"));
        assert_eq!(
            unmet_requirement(&["env:RKQA_SURELY_UNSET_VAR".into()], Path::new("/")).as_deref(),
            Some("missing env RKQA_SURELY_UNSET_VAR")
        );
        assert_eq!(
            unmet_requirement(&["file:/definitely/not/here".into()], Path::new("/")).as_deref(),
            Some("missing file /definitely/not/here")
        );
        assert!(unmet_requirement(&[], Path::new("/")).is_none());
    }

    #[test]
    fn a_verbatim_path_is_given_to_scenarios_in_its_ordinary_form() {
        assert_eq!(strip_verbatim(r"\\?\C:\qa\run-1"), r"C:\qa\run-1");
        assert_eq!(
            strip_verbatim(r"\\?\UNC\server\share\qa"),
            r"\\server\share\qa"
        );
        // Already ordinary, not Windows at all, or a verbatim form with no ordinary spelling.
        for unchanged in [
            r"C:\qa\run-1",
            "/tmp/qa/run-1",
            r"\\?\Volume{4c1b02c1-d990-11dc-99ae-806e6f6e6963}\qa",
            r"\\?\GLOBALROOT\Device\x",
        ] {
            assert_eq!(strip_verbatim(unchanged), unchanged);
        }
        // Too long to be written without the prefix: left verbatim.
        let long = format!(r"\\?\C:\{}", "d".repeat(260));
        assert_eq!(strip_verbatim(&long), long);
    }

    #[cfg(not(windows))]
    #[test]
    fn scenario_paths_are_untouched_off_windows() {
        assert_eq!(
            resolve_in(Path::new("/data"), "proj/a\\b.txt"),
            Path::new("/data/proj/a\\b.txt")
        );
        assert_eq!(
            resolve_in(Path::new("/data"), "/abs/x"),
            Path::new("/abs/x")
        );
        assert_eq!(scenario_path_text(Path::new("/data/run")), "/data/run");
    }

    fn scenario_with_kind(kind: Option<&str>) -> Scenario {
        let header = kind
            .map(|k| {
                format!(
                    "kind = \"{k}\"
"
                )
            })
            .unwrap_or_default();
        let text = format!(
            "name = \"k\"
{header}[[step]]
kind = \"cli\"
command = \"x\"
"
        );
        let path = std::env::temp_dir().join(format!("rkqa-kind-{}.scenario.toml", util::new_id()));
        std::fs::write(&path, text).unwrap();
        let s = Scenario::load(&path);
        let _ = std::fs::remove_file(&path);
        s.unwrap()
    }

    #[test]
    fn a_scenario_declares_its_kind_or_leaves_it_to_the_run() {
        assert_eq!(scenario_with_kind(None).kind, None);
        assert_eq!(
            scenario_with_kind(Some("visual")).kind,
            Some(RunKind::Visual)
        );
        assert_eq!(
            scenario_with_kind(Some("functional")).kind,
            Some(RunKind::Functional)
        );
        let path = std::env::temp_dir().join(format!("rkqa-kind-{}.scenario.toml", util::new_id()));
        std::fs::write(
            &path,
            "name = \"k\"
kind = \"cosmetic\"
[[step]]
kind = \"cli\"
command = \"x\"
",
        )
        .unwrap();
        let e = Scenario::load(&path).unwrap_err();
        let _ = std::fs::remove_file(&path);
        assert!(e.0.contains("functional") && e.0.contains("visual"), "{e}");
    }

    #[test]
    fn run_kind_defaults_to_functional_and_follows_scenarios_unless_requested() {
        let none = [
            scenario_with_kind(None),
            scenario_with_kind(Some("functional")),
        ];
        assert_eq!(resolve_run_kind(None, &none), RunKind::Functional);
        let some = [scenario_with_kind(None), scenario_with_kind(Some("visual"))];
        assert_eq!(resolve_run_kind(None, &some), RunKind::Visual);
        // An explicit request wins over what the scenarios declare, both ways.
        assert_eq!(
            resolve_run_kind(Some(RunKind::Functional), &some),
            RunKind::Functional
        );
        assert_eq!(
            resolve_run_kind(Some(RunKind::Visual), &none),
            RunKind::Visual
        );
        assert_eq!(resolve_run_kind(None, &[]), RunKind::Functional);
        assert_eq!(RunKind::parse("visual").unwrap(), RunKind::Visual);
        assert_eq!(RunKind::parse(" functional ").unwrap(), RunKind::Functional);
        assert!(RunKind::parse("both").is_err());
    }

    #[test]
    fn a_scenario_kind_may_narrow_a_run_but_never_widen_a_functional_one() {
        let f = RunKind::Functional;
        let v = RunKind::Visual;
        assert_eq!(scenario_kind(f, None, "s").unwrap(), f);
        assert_eq!(scenario_kind(v, None, "s").unwrap(), v);
        assert_eq!(scenario_kind(v, Some(f), "s").unwrap(), f);
        assert_eq!(scenario_kind(v, Some(v), "s").unwrap(), v);
        assert_eq!(scenario_kind(f, Some(f), "s").unwrap(), f);
        let e = scenario_kind(f, Some(v), "login").unwrap_err();
        assert!(
            e.0.contains("login") && e.0.contains("--kind visual"),
            "{e}"
        );
    }

    #[test]
    fn captures_are_refused_in_a_functional_scenario_and_allowed_in_a_visual_one() {
        let e = ensure_capture_allowed(RunKind::Functional, "login", "ui_screenshot").unwrap_err();
        assert!(
            e.0.contains("ui_screenshot")
                && e.0.contains("login")
                && e.0.contains("functional")
                && e.0.contains("kind = \"visual\""),
            "{e}"
        );
        ensure_capture_allowed(RunKind::Visual, "login", "ui_screenshot").unwrap();
        assert!(is_capture_method("screenshot"));
        assert!(is_capture_method("record_start"));
        assert!(is_capture_method("capture"));
        assert!(!is_capture_method("click"));
        assert!(!is_capture_method("batch"));
    }

    fn bare_step(kind: &str, params: Value) -> Step {
        Step {
            kind: kind.into(),
            label: None,
            save: None,
            expect: vec![],
            until_timeout_s: None,
            interval_ms: None,
            params: params
                .as_object()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .collect(),
        }
    }

    /// The step-level rule, through the real executor: a functional scenario's screenshot step
    /// is refused before the app is touched, and a visual one gets past the gate (and then stops
    /// for want of a launched app, which proves the refusal was the kind and nothing else).
    #[test]
    fn a_custom_step_kind_runs_with_ctx_and_params_and_feeds_expect_and_save() {
        let root = std::env::temp_dir().join(format!("rkqa-custom-ws-{}", util::new_id()));
        std::fs::create_dir_all(&root).unwrap();
        let cfg =
            QaConfig::parse("app = \"customtest\"\n", &root.join("rightkit-qa.toml")).unwrap();
        let ws = crate::workspace::create(&root.join("managed"), None, "customtest").unwrap();
        let tracker = Tracker::new();
        let mut ctx = Ctx::new(&cfg, None, None, &ws, &tracker, "s").unwrap();
        ctx.vars.insert("mock_url".into(), json!("http://m"));
        let e = run_step(&mut ctx, &bare_step("custom_probe", json!({}))).unwrap_err();
        assert!(e.0.contains("unknown step kind 'custom_probe'"), "{e}");
        ctx.handlers.register("custom_probe", |ctx, p| {
            let url = ctx.vars["mock_url"].as_str().unwrap().to_string();
            ctx.vars.insert("touched".into(), json!(true));
            Ok(json!({"url": url, "echo": p.get("what").cloned()}))
        });
        assert_eq!(ctx.handlers.kinds(), vec!["custom_probe"]);
        // Parameters arrive interpolated; the result is what `expect` checks and `save` stores.
        let mut step = bare_step("custom_probe", json!({"what": "${mock_url}/x"}));
        step.save = Some("admin".into());
        step.expect =
            vec![serde_json::from_value(json!({"path": "echo", "eq": "http://m/x"})).unwrap()];
        let v = run_step(&mut ctx, &step).unwrap();
        assert_eq!(v["url"], "http://m");
        assert_eq!(ctx.vars["admin"]["echo"], "http://m/x");
        assert_eq!(ctx.vars["touched"], json!(true));
        step.expect = vec![serde_json::from_value(json!({"path": "echo", "eq": "nope"})).unwrap()];
        let e = run_step(&mut ctx, &step).unwrap_err();
        assert!(e.0.contains("custom_probe"), "{e}");
        // `build` and `build.*` are the runner's record of what it built: a `save` over them is
        // errored before the step runs.
        for name in ["build", "build.app"] {
            let mut step = bare_step("custom_probe", json!({"what": "x"}));
            step.save = Some(name.into());
            let touched_before = ctx.vars.get("touched").cloned();
            ctx.vars.remove("touched");
            let e = run_step(&mut ctx, &step).unwrap_err();
            assert!(crate::plugin::is_errored(&e), "{e}");
            assert!(e.0.contains(name), "{e}");
            assert!(
                !ctx.vars.contains_key("touched"),
                "the step must not have run"
            );
            assert!(!ctx.vars.contains_key(name));
            if let Some(v) = touched_before {
                ctx.vars.insert("touched".into(), v);
            }
        }
        // A handler error is the step's error.
        ctx.handlers.register("boom", |_, _| err("bad thing"));
        assert!(run_step(&mut ctx, &bare_step("boom", json!({})))
            .unwrap_err()
            .0
            .contains("bad thing"));
        drop(ctx);
        let _ = crate::workspace::dispose(&ws, false, 3, 10);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_functional_scenario_refuses_screenshot_steps_and_a_visual_one_does_not() {
        let root = std::env::temp_dir().join(format!("rkqa-kind-ws-{}", util::new_id()));
        std::fs::create_dir_all(&root).unwrap();
        let cfg = QaConfig::parse(
            "app = \"kindtest\"
",
            &root.join("rightkit-qa.toml"),
        )
        .unwrap();
        let ws = crate::workspace::create(&root.join("managed"), None, "kindtest").unwrap();
        let tracker = Tracker::new();
        let mut ctx = Ctx::new(&cfg, None, None, &ws, &tracker, "s").unwrap();
        assert_eq!(
            ctx.kind,
            RunKind::Functional,
            "a scenario is functional unless told otherwise"
        );

        let shot = bare_step("ui_screenshot", json!({}));
        let batch = bare_step(
            "ui_batch",
            json!({"steps": [{"method": "click", "params": {"x": 1, "y": 2}}, {"method": "screenshot"}]}),
        );
        let e = run_step(&mut ctx, &shot).unwrap_err();
        assert!(e.0.contains("refused in functional scenario"), "{e}");
        let e = run_step(&mut ctx, &batch).unwrap_err();
        assert!(
            e.0.contains("ui_batch method 'screenshot'") && e.0.contains("functional"),
            "{e}"
        );

        ctx.kind = RunKind::Visual;
        let e = run_step(&mut ctx, &shot).unwrap_err();
        assert!(e.0.contains("no UI app launched"), "{e}");
        let e = run_step(&mut ctx, &batch).unwrap_err();
        assert!(e.0.contains("no UI app launched"), "{e}");

        drop(ctx);
        let _ = crate::workspace::dispose(&ws, false, 3, 10);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn parses_the_seed_and_accessibility_steps() {
        let text = r##"
name = "settings a11y"
target = "ui"
kind = "visual"
[[step]]
kind = "engine_seed"
commands = [
  { command = "project create demo" },
  { args = ["settings", "set", "theme", "dark"], allow_error = true },
]
[[step]]
kind = "ui_launch"
[[step]]
kind = "ui_assert_ax"
role = "button"
name = "Save"
min_count = 1
[[step]]
kind = "ui_contrast"
selector = "#status"
level = "AA"
  [[step.expect]]
  path = "passes"
  eq = true
[[step]]
kind = "ui_focus_ring"
selector = "#btn"
screenshot = true
  [[step.expect]]
  path = "visible"
  eq = true
"##;
        let path = std::env::temp_dir().join(format!("rkqa-a11y-{}.scenario.toml", util::new_id()));
        std::fs::write(&path, text).unwrap();
        let s = Scenario::load(&path);
        let _ = std::fs::remove_file(&path);
        let s = s.unwrap();
        assert_eq!(s.target, "ui");
        let kinds: Vec<&str> = s.steps.iter().map(|s| s.kind.as_str()).collect();
        assert_eq!(
            kinds,
            [
                "engine_seed",
                "ui_launch",
                "ui_assert_ax",
                "ui_contrast",
                "ui_focus_ring"
            ]
        );
        let seeds = seed_commands(&s.steps[0].params.clone().into_iter().collect()).unwrap();
        assert_eq!(seeds.len(), 2);
        assert_eq!(
            seeds[1]["args"],
            json!(["settings", "set", "theme", "dark"])
        );
        assert_eq!(s.steps[3].params["selector"], json!("#status"));
        assert_eq!(s.steps[4].expect[0].eq, Some(json!(true)));
        let q =
            a11y::AxQuery::from_params(&s.steps[2].params.clone().into_iter().collect()).unwrap();
        assert_eq!(
            (q.role.as_deref(), q.name.as_deref(), q.min_count),
            (Some("button"), Some("Save"), Some(1))
        );
    }

    #[test]
    fn seed_commands_are_validated() {
        let get = |v: Value| seed_commands(v.as_object().unwrap());
        assert!(get(json!({})).is_err());
        assert!(get(json!({"commands": []})).is_err());
        assert!(get(json!({"commands": ["doctor"]})).is_err());
        assert!(get(json!({"commands": [{"input": {"a": 1}}]})).is_err());
        assert_eq!(
            get(json!({"commands": [{"command": "doctor"}]}))
                .unwrap()
                .len(),
            1
        );
    }

    /// `engine_seed` through the executor: it needs the engine (a `ui` scenario with no engine
    /// binary gets a clear error, not a skip) and it refuses to run once the app is up.
    #[test]
    fn engine_seed_needs_the_engine_and_runs_before_the_app() {
        let root = std::env::temp_dir().join(format!("rkqa-seed-ws-{}", util::new_id()));
        std::fs::create_dir_all(&root).unwrap();
        let cfg = QaConfig::parse(
            "app = \"seedtest\"
",
            &root.join("rightkit-qa.toml"),
        )
        .unwrap();
        let ws = crate::workspace::create(&root.join("managed"), None, "seedtest").unwrap();
        let tracker = Tracker::new();
        let mut ctx = Ctx::new(&cfg, None, None, &ws, &tracker, "s").unwrap();
        let seed = bare_step("engine_seed", json!({"commands": [{"command": "doctor"}]}));
        let e = run_step(&mut ctx, &seed).unwrap_err();
        assert!(e.0.contains("no engine binary configured"), "{e}");
        let bad = bare_step("engine_seed", json!({"commands": []}));
        assert!(run_step(&mut ctx, &bad).unwrap_err().0.contains("empty"));
        // The accessibility steps need a launched app.
        for kind in ["ui_assert_ax", "ui_contrast", "ui_focus_ring"] {
            let step = bare_step(kind, json!({"role": "button", "selector": "#a"}));
            let e = run_step(&mut ctx, &step).unwrap_err();
            assert!(e.0.contains("no UI app launched"), "{kind}: {e}");
        }
        drop(ctx);
        let _ = crate::workspace::dispose(&ws, false, 3, 10);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The defect: `${data_dir}/proj/README.txt` under a canonicalised (verbatim) data
    /// directory reached the file system with a `/` Windows does not split on (os error 123).
    #[cfg(windows)]
    #[test]
    fn a_scenario_path_mixing_separators_reaches_the_file_system_on_windows() {
        let root = std::env::temp_dir().join(format!("rkqa-paths-{}", util::new_id()));
        std::fs::create_dir_all(&root).unwrap();
        let data_dir = std::fs::canonicalize(&root).unwrap();
        assert!(
            data_dir.to_string_lossy().starts_with(r"\\?\"),
            "{data_dir:?}"
        );

        // The variable a scenario expands is ordinary, so text built on it is a valid path.
        let var = scenario_path_text(&data_dir);
        assert!(!var.starts_with(r"\\?\"), "{var}");
        let expanded = resolve_in(&data_dir, &format!("{var}/proj/README.txt"));
        std::fs::create_dir_all(expanded.parent().unwrap()).unwrap();
        std::fs::write(&expanded, "one").unwrap();

        // A relative path with `/`, joined to the verbatim directory itself.
        let relative = resolve_in(&data_dir, "proj/nested/b.txt");
        std::fs::create_dir_all(relative.parent().unwrap()).unwrap();
        std::fs::write(&relative, "two").unwrap();

        // Text built on the verbatim form, as a scenario written before this fix would hold.
        let verbatim = resolve_in(
            &data_dir,
            &format!("{}/proj/c.txt", data_dir.to_string_lossy()),
        );
        std::fs::write(&verbatim, "three").unwrap();

        // All three are real files in the same directory tree.
        assert_eq!(
            std::fs::read_to_string(root.join("proj").join("README.txt")).unwrap(),
            "one"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("proj").join("nested").join("b.txt")).unwrap(),
            "two"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("proj").join("c.txt")).unwrap(),
            "three"
        );
        assert!(std::fs::metadata(&expanded).unwrap().is_file());
        std::fs::remove_dir_all(root).unwrap();
    }

    /// The last value a launch environment gives `key` (the launch guard and `Command` both let a
    /// later pair win).
    fn last_env(env: &[(String, String)], key: &str) -> Option<String> {
        env.iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    }

    /// Every scenario gets its own private directory with a runner binding for the run, and the
    /// engine target of that scenario carries the test-mode pair pointing at it. The cli and the
    /// sidecar read the launch environment from `EngineTarget::env_for`; a custom step that clones
    /// `ctx.engine.env` reads the same pairs.
    #[test]
    fn each_scenario_gets_a_bound_private_root_and_its_engine_env_names_it() {
        use rightkit_fs::test_mode::{decide, Decision, Request, BINDING_FILE};
        let root = std::env::temp_dir().join(format!("rkqa-bind-ws-{}", util::new_id()));
        std::fs::create_dir_all(&root).unwrap();
        let cfg = QaConfig::parse("app = \"bindtest\"\n", &root.join("rightkit-qa.toml")).unwrap();
        let ws = crate::workspace::create(&root.join("managed"), None, "bindtest").unwrap();
        let tracker = Tracker::new();
        let target = EngineTarget::new(root.join("engine")).with_data_dir_env("BINDTEST_DATA");
        let protected = vec![root.join("real-user-data")];
        let home_binding: Value =
            serde_json::from_slice(&std::fs::read(ws.home.join(BINDING_FILE)).unwrap()).unwrap();

        let mut first = Ctx::new(&cfg, Some(target.clone()), None, &ws, &tracker, "alpha").unwrap();
        let second = Ctx::new(&cfg, Some(target.clone()), None, &ws, &tracker, "beta").unwrap();
        assert_ne!(first.data_dir, second.data_dir, "two scenarios, two roots");
        for (ctx, name) in [(&first, "alpha"), (&second, "beta")] {
            assert!(ctx.data_dir.starts_with(&ws.data_dir));
            let binding: Value =
                serde_json::from_slice(&std::fs::read(ctx.data_dir.join(BINDING_FILE)).unwrap())
                    .unwrap();
            assert_eq!(binding["app"], "bindtest");
            assert_eq!(binding["runRoot"], home_binding["runRoot"]);
            assert_eq!(binding["runId"], json!(ws.run_id));
            assert_eq!(binding["scenario"], name);
            // The same decision a test-capable app makes accepts the scenario root.
            let request = Request {
                app_id: "bindtest",
                activated: true,
                root: Some(&ctx.data_dir),
                protected_paths: &protected,
            };
            let Decision::Test(validated) = decide(&request).unwrap() else {
                panic!("a bound scenario root must activate test mode")
            };
            assert_eq!(
                validated.path(),
                std::fs::canonicalize(&ctx.data_dir).unwrap()
            );
            // cli and sidecar launch environment.
            let dir = ctx.data_dir.to_string_lossy().into_owned();
            let env = ctx
                .engine
                .as_ref()
                .unwrap()
                .env_for(Some(&ctx.data_dir), &[]);
            assert_eq!(last_env(&env, "RIGHTKIT_UNDER_TEST").as_deref(), Some("1"));
            assert_eq!(
                last_env(&env, "RIGHTKIT_QA_DATA_DIR").as_deref(),
                Some(dir.as_str())
            );
            // The app's own data-dir variable behaves as before.
            assert_eq!(
                last_env(&env, "BINDTEST_DATA").as_deref(),
                Some(dir.as_str())
            );
        }
        // A custom step that clones the engine env (a daemon step, for one) gets the pair.
        first.handlers.register("engine_env", |ctx, _| {
            Ok(json!(ctx.engine.as_ref().unwrap().env.clone()))
        });
        let env = run_step(&mut first, &bare_step("engine_env", json!({}))).unwrap();
        let env: Vec<(String, String)> = serde_json::from_value(env).unwrap();
        assert_eq!(last_env(&env, "RIGHTKIT_UNDER_TEST").as_deref(), Some("1"));
        let first_dir = first.data_dir.to_string_lossy().into_owned();
        assert_eq!(
            last_env(&env, "RIGHTKIT_QA_DATA_DIR").as_deref(),
            Some(first_dir.as_str())
        );
        let (a, b) = (first.data_dir.clone(), second.data_dir.clone());
        drop(first);
        drop(second);
        // Cleanup is unchanged: the home goes, and the scenario directories with it. A scenario
        // directory is not a home, so only the home passes the ownership test.
        assert!(!crate::storage::is_owned_home(&a));
        let report = crate::workspace::dispose(&ws, false, 1, 0);
        assert!(report.retained.is_empty(), "{report:?}");
        assert!(!ws.home.exists() && !a.exists() && !b.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The refusals stay reachable: the config env cannot unbind the pair, a step's own `env` can
    /// still set or override it, a refusal-probe target is left unbound, and the decision still
    /// refuses a half pair, a root with no binding and a binding for another app.
    #[test]
    fn negative_test_overrides_and_refusals_survive_the_binding() {
        use rightkit_fs::test_mode::{decide, Refusal, Request, BINDING_FILE};
        let root = std::env::temp_dir().join(format!("rkqa-bind-neg-{}", util::new_id()));
        std::fs::create_dir_all(&root).unwrap();
        let cfg = QaConfig::parse("app = \"negtest\"\n", &root.join("rightkit-qa.toml")).unwrap();
        let ws = crate::workspace::create(&root.join("managed"), None, "negtest").unwrap();
        let tracker = Tracker::new();
        let mut target = EngineTarget::new(root.join("engine")).with_data_dir_env("NEGTEST_DATA");
        // Config-level values come before the binding and cannot unbind it.
        target.env.push(("RIGHTKIT_UNDER_TEST".into(), "0".into()));
        target
            .env
            .push(("RIGHTKIT_QA_DATA_DIR".into(), "elsewhere".into()));
        let ctx = Ctx::new(&cfg, Some(target.clone()), None, &ws, &tracker, "neg").unwrap();
        let engine = ctx.engine.as_ref().unwrap();
        assert_eq!(
            engine
                .env
                .iter()
                .filter(|(k, _)| k == "RIGHTKIT_UNDER_TEST" || k == "RIGHTKIT_QA_DATA_DIR")
                .count(),
            2,
            "each variable appears once: {:?}",
            engine.env
        );
        assert_eq!(
            last_env(&engine.env, "RIGHTKIT_UNDER_TEST").as_deref(),
            Some("1")
        );
        // A step's `env` (CliOptions::env) is applied after the binding and wins.
        let dir = Some(ctx.data_dir.as_path());
        let step_env = vec![
            ("RIGHTKIT_UNDER_TEST".to_string(), "0".to_string()),
            ("RIGHTKIT_QA_DATA_DIR".to_string(), "".to_string()),
        ];
        let env = engine.env_for(dir, &step_env);
        assert_eq!(last_env(&env, "RIGHTKIT_UNDER_TEST").as_deref(), Some("0"));
        assert_eq!(last_env(&env, "RIGHTKIT_QA_DATA_DIR").as_deref(), Some(""));
        // The refusal-probe target keeps its config env untouched.
        let probe = target.clone().allow_no_data_dir_for_refusal_probe();
        let probed = Ctx::new(&cfg, Some(probe), None, &ws, &tracker, "probe").unwrap();
        assert_eq!(
            last_env(&probed.engine.as_ref().unwrap().env, "RIGHTKIT_UNDER_TEST").as_deref(),
            Some("0")
        );
        // The app-side decision: halves alone, no binding, another app's binding.
        let protected = vec![root.join("real-user-data")];
        let bound = Request {
            app_id: "negtest",
            activated: true,
            root: Some(&ctx.data_dir),
            protected_paths: &protected,
        };
        assert!(decide(&bound).is_ok());
        assert_eq!(
            decide(&Request {
                activated: false,
                ..bound.clone()
            }),
            Err(Refusal::RootWithoutActivation)
        );
        assert_eq!(
            decide(&Request {
                root: None,
                ..bound.clone()
            }),
            Err(Refusal::ActivationWithoutRoot)
        );
        // A private directory with no binding: the shape of the old failure.
        let unbound = ws.home.join("unbound");
        rightkit_fs::private::ensure_private_dir(&unbound).unwrap();
        assert_eq!(
            decide(&Request {
                root: Some(&unbound),
                ..bound.clone()
            }),
            Err(Refusal::RootNotCreatedByRunner)
        );
        assert_eq!(
            decide(&Request {
                app_id: "other",
                ..bound.clone()
            }),
            Err(Refusal::BindingAppMismatch)
        );
        // Rewriting the binding's `app` is enough to change the verdict: the check is app-id
        // equality, not proof of who wrote the file.
        let path = ctx.data_dir.join(BINDING_FILE);
        std::fs::write(&path, r#"{"app":"other"}"#).unwrap();
        assert_eq!(decide(&bound), Err(Refusal::BindingAppMismatch));
        drop(ctx);
        drop(probed);
        let _ = crate::workspace::dispose(&ws, false, 3, 10);
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod token_and_skip_tests {
    use super::*;

    fn load(header: &str) -> Result<Scenario> {
        let text = format!("name = \"t\"\n{header}\n[[step]]\nkind = \"set\"\nvalue = 1\n");
        let path = std::env::temp_dir().join(format!("rkqa-tok-{}.scenario.toml", util::new_id()));
        std::fs::write(&path, text).unwrap();
        let s = Scenario::load(&path);
        let _ = std::fs::remove_file(&path);
        s
    }

    #[test]
    fn scenario_tokens_parse_in_declared_order_and_default_to_none() {
        assert!(load("").unwrap().grants.is_empty());
        let s = load("tokens = [\"gui-session\", \"focus\"]").unwrap();
        assert_eq!(s.grants, vec![Token::GuiSession, Token::Focus]);
        assert_eq!(s.tokens, vec!["gui-session", "focus"]);
    }

    #[test]
    fn an_unknown_empty_or_repeated_token_is_refused_at_load() {
        for bad in [
            "tokens = [\"fcus\"]",
            "tokens = [\"\"]",
            "tokens = [\"Focus\"]",
            "tokens = [\"focus\", \"focus\"]",
        ] {
            let e = load(bad).unwrap_err();
            assert!(e.0.contains("token"), "{bad}: {e}");
        }
    }

    #[test]
    fn ctx_refuses_an_undeclared_token_only_inside_a_world() {
        let root = std::env::temp_dir().join(format!("rkqa-ctxtok-{}", util::new_id()));
        std::fs::create_dir_all(&root).unwrap();
        let cfg = crate::config::QaConfig::parse(
            "app = \"tok\"\n[engine]\ndata_dir_env = \"TOK_DATA\"\n",
            &root.join("rightkit-qa.toml"),
        )
        .unwrap();
        let ws = crate::workspace::create(&root.join("managed"), None, "tok").unwrap();
        let tracker = Tracker::new();
        let ctx = Ctx::new(&cfg, None, None, &ws, &tracker, "s").unwrap();
        // No world: nothing is held or checked.
        ctx.require_token(Token::Focus, "x").unwrap();
        let world = crate::pool::WorldPlan::new(
            "run".into(),
            ws.home.clone(),
            &crate::config::HostConfig::default(),
            1,
        )
        .unwrap()
        .world(0)
        .unwrap();
        let ctx = ctx.with_world(world.clone(), vec![Token::Audio]);
        ctx.require_token(Token::Audio, "x").unwrap();
        let e = ctx
            .require_token(Token::Focus, "ui_launch with focus")
            .unwrap_err();
        assert!(e.0.contains("tokens = [\"focus\"]"), "{e}");
        // The port range is the world's, and nothing past it.
        assert_eq!(ctx.world_port(0).unwrap(), world.port_start);
        assert_eq!(ctx.world_port(1).unwrap(), world.port_start + 1);
        assert!(ctx
            .world_port(world.port_end - world.port_start + 1)
            .is_err());
        let _ = crate::workspace::dispose(&ws, false, 3, 10);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn skip_kinds_count_by_kind_and_a_qualifying_run_tolerates_only_other_platform() {
        for (kind, ordinary, qualifying) in [
            (SkipKind::OtherPlatform, true, true),
            (SkipKind::Optional, true, false),
            (SkipKind::UnmetRequirement, false, false),
            (SkipKind::TierNotSelected, false, false),
        ] {
            assert_eq!(kind.tolerated(false), ordinary, "{kind:?} ordinary");
            assert_eq!(kind.tolerated(true), qualifying, "{kind:?} qualifying");
        }
    }

    #[test]
    fn requirement_gate_types_each_outcome() {
        let other = if cfg!(windows) { "darwin" } else { "windows" };
        let host = if cfg!(windows) { "windows" } else { "darwin" };
        let root = std::env::temp_dir();
        let gate = |header: &str| requirement_gate(&load(header).unwrap(), &root);

        assert_eq!(gate(""), Gate::Run);
        assert_eq!(
            gate(&format!("requires = [\"platform:{host}\"]")),
            Gate::Run
        );
        // Another OS is a skip even when the scenario is mandatory.
        assert!(matches!(
            gate(&format!("requires = [\"platform:{other}\"]")),
            Gate::Skip {
                kind: SkipKind::OtherPlatform,
                ..
            }
        ));
        assert!(matches!(
            gate(&format!(
                "optional = true\nrequires = [\"platform:{other}\"]"
            )),
            Gate::Skip {
                kind: SkipKind::OtherPlatform,
                ..
            }
        ));
        // Another unmet requirement: optional skips, mandatory fails.
        assert!(matches!(
            gate("optional = true\nrequires = [\"env:RKQA_DEFINITELY_UNSET_X\"]"),
            Gate::Skip {
                kind: SkipKind::Optional,
                ..
            }
        ));
        match gate("requires = [\"env:RKQA_DEFINITELY_UNSET_X\"]") {
            Gate::Fail { reason } => assert!(reason.contains("mandatory"), "{reason}"),
            other => panic!("expected a failed gate, got {other:?}"),
        }
    }

    #[test]
    fn platform_macos_is_an_alias_of_darwin() {
        assert_eq!(canonical_platform("macos"), Some("darwin"));
        assert_eq!(canonical_platform("darwin"), Some("darwin"));
        assert_eq!(canonical_platform("windows"), Some("windows"));
        assert_eq!(canonical_platform("linux"), Some("linux"));
        assert_eq!(canonical_platform("macosx"), None);

        let root = std::env::temp_dir();
        let req = |v: &str| vec![format!("platform:{v}")];
        // Both spellings decide identically on every host.
        assert_eq!(
            platform_mismatch(&req("macos")).is_some(),
            platform_mismatch(&req("darwin")).is_some()
        );
        assert_eq!(
            unmet_requirement(&req("macos"), &root).is_some(),
            unmet_requirement(&req("darwin"), &root).is_some()
        );
        let gate = |v: &str| {
            requirement_gate(
                &load(&format!("requires = [\"platform:{v}\"]")).unwrap(),
                &root,
            )
        };
        if cfg!(target_os = "macos") {
            // The false green this fixes: `platform:macos` must RUN on a Mac, not skip forever.
            assert_eq!(gate("macos"), Gate::Run);
            assert_eq!(gate("darwin"), Gate::Run);
            assert!(matches!(
                gate("windows"),
                Gate::Skip {
                    kind: SkipKind::OtherPlatform,
                    ..
                }
            ));
        } else {
            // Another OS is still an OtherPlatform skip for the alias.
            for v in ["macos", "darwin"] {
                assert!(
                    matches!(
                        gate(v),
                        Gate::Skip {
                            kind: SkipKind::OtherPlatform,
                            ..
                        }
                    ),
                    "{v}: {:?}",
                    gate(v)
                );
            }
        }
        if cfg!(windows) {
            assert_eq!(gate("windows"), Gate::Run);
        }
    }

    #[test]
    fn an_unknown_platform_value_is_an_unmet_requirement_never_a_silent_skip() {
        let root = std::env::temp_dir();
        let reqs = vec!["platform:macosx".to_string()];
        // Not another OS, so not an OtherPlatform skip (which every run tolerates).
        assert_eq!(platform_mismatch(&reqs), None);
        let reason = unmet_requirement(&reqs, &root).expect("unknown platform must be unmet");
        assert!(reason.contains("unknown platform 'macosx'"), "{reason}");
        // Same handling as an unknown requirement kind: mandatory fails, optional skips as Optional.
        let gate = |header: &str| requirement_gate(&load(header).unwrap(), &root);
        match gate("requires = [\"platform:macosx\"]") {
            Gate::Fail { reason } => assert!(reason.contains("unknown platform"), "{reason}"),
            other => panic!("expected a failed gate, got {other:?}"),
        }
        assert!(matches!(
            gate("optional = true\nrequires = [\"platform:macosx\"]"),
            Gate::Skip {
                kind: SkipKind::Optional,
                ..
            }
        ));
        // An unknown value after a valid host-or-other platform requirement is still reported.
        let mixed = vec!["platform:macos".to_string(), "platform:typo".to_string()];
        assert_eq!(
            platform_mismatch(&mixed).is_some(),
            !cfg!(target_os = "macos")
        );
        assert!(unmet_requirement(&mixed, &root).is_some());
    }
}

/// How `cli`, `cold_cli` and `engine_seed` steps declare their lifecycle (Sage C3(a)); the counting
/// and routing themselves are tested in `engine::session_tests`.
#[cfg(test)]
mod cli_route_tests {
    use super::*;

    fn params(v: Value) -> Map<String, Value> {
        v.as_object().cloned().unwrap()
    }

    #[test]
    fn a_cli_step_is_in_session_unless_it_declares_a_cold_reason() {
        let p = params(json!({"command": "a.b"}));
        assert!(matches!(
            cli_route(&p, Lane::Session),
            Ok(EngineRoute::Session)
        ));
        let p = params(json!({"command": "a.b", "lifecycle": "session"}));
        assert!(matches!(
            cli_route(&p, Lane::Session),
            Ok(EngineRoute::Session)
        ));
        let p = params(
            json!({"command": "store.restore", "lifecycle": "cold", "cold_reason": "store-lock"}),
        );
        assert!(matches!(
            cli_route(&p, Lane::Session),
            Ok(EngineRoute::Cold("store-lock"))
        ));
    }

    #[test]
    fn a_cold_declaration_names_exactly_one_reason_from_the_closed_set() {
        let missing = params(json!({"command": "a.b", "lifecycle": "cold"}));
        let e = cli_route(&missing, Lane::Session).err().unwrap().0;
        assert!(
            e.contains("names its reason") && e.contains("non-method"),
            "{e}"
        );
        let unknown = params(json!({"command": "a.b", "lifecycle": "cold", "cold_reason": "slow"}));
        assert!(cli_route(&unknown, Lane::Session).is_err());
        // A reason without `lifecycle = "cold"` is a mistake, never a quiet cold launch.
        let stray = params(json!({"command": "a.b", "cold_reason": "startup"}));
        assert!(cli_route(&stray, Lane::Session).is_err());
        let bad_lifecycle = params(json!({"command": "a.b", "lifecycle": "warm"}));
        assert!(cli_route(&bad_lifecycle, Lane::Session).is_err());
    }

    #[test]
    fn cold_cli_is_always_cold_and_needs_its_reason() {
        let p = params(json!({"command": "doctor", "reason": "non-method"}));
        assert!(matches!(
            cli_route(&p, Lane::ColdStep),
            Ok(EngineRoute::Cold("non-method"))
        ));
        let p = params(json!({"command": "doctor", "cold_reason": "cli-entry"}));
        assert!(matches!(
            cli_route(&p, Lane::ColdStep),
            Ok(EngineRoute::Cold("cli-entry"))
        ));
        assert!(cli_route(&params(json!({"command": "doctor"})), Lane::ColdStep).is_err());
        let p = params(json!({"command": "doctor", "reason": "startup", "lifecycle": "session"}));
        assert!(cli_route(&p, Lane::ColdStep).is_err());
    }

    #[test]
    fn engine_seed_commands_are_declared_startup_launches() {
        let p = params(json!({"command": "a.b"}));
        assert!(matches!(
            cli_route(&p, Lane::Seed),
            Ok(EngineRoute::Cold("startup"))
        ));
    }

    #[test]
    fn the_cli_words_come_from_command_or_args() {
        let words = |v: Value| cli_args(&params(v)).unwrap();
        assert_eq!(
            words(json!({"command": "job status"})),
            vec!["job", "status"]
        );
        assert_eq!(words(json!({"args": ["job.status"]})), vec!["job.status"]);
        assert!(cli_args(&params(json!({"input": {}}))).is_err());
    }
}
