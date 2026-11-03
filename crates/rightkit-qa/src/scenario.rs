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
    /// Environment this scenario hands the app on `ui_launch`: an `[inputs]` table (or
    /// `inputs = { .. }`) of string values placed before the first `[[step]]`. This is how a
    /// scenario sets a per-app QA input, the `RIGHTKIT_<APP>_<NAME>` variable an app reads itself
    /// behind its own QA feature (the daemon has no `test_selector!` since 0.5.3). [`Scenario::load`] refuses
    /// the file unless each key is `RIGHTKIT_` plus upper-case words (at most 64 bytes), is not a
    /// name the harness or the daemon's mode decision uses (`RESERVED_INPUT_KEYS`) and names no
    /// credential, and each value is at most 1024 bytes with no NUL, CR or LF. Applied after the
    /// config `env` and before the step's own `env` (`ui_launch_env`).
    #[serde(default)]
    pub inputs: BTreeMap<String, String>,
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
    /// The scenario's [`Scenario::inputs`], copied onto each step by [`Scenario::load`] so the step
    /// that launches the app applies them without the runner passing the scenario along. Empty for a
    /// step built by hand.
    #[serde(skip)]
    pub inputs: Arc<BTreeMap<String, String>>,
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
        validate_inputs(path, &s.inputs)?;
        let inputs = Arc::new(s.inputs.clone());
        for step in &mut s.steps {
            step.inputs = Arc::clone(&inputs);
        }
        Ok(s)
    }
}

/// Longest `inputs` key, in bytes.
const INPUT_KEY_MAX: usize = 64;
/// Longest `inputs` value, in bytes.
const INPUT_VALUE_MAX: usize = 1024;

/// Names a scenario may not set through `inputs`, because the harness or the daemon's mode
/// decision sets or reads them itself. Constants where an owner exports one; the rest are spelled
/// where they are set (`workspace::create`, `control::launch`, `suite`, `rightkit_fs::app_dirs`) and listed here by hand, with a test that keeps the workspace
/// and world keys inside the list. A name missing here is still refused at launch when the harness
/// has already given the app a different value for it.
const RESERVED_INPUT_KEYS: &[&str] = &[
    // The control channel and the pool's per-scenario world. The control token file is one
    // variable under two names (`token::ENV_AUTH_FILE`, `pool::ENV_CONTROL_TOKEN_FILE`).
    control::token::ENV_AUTH_FILE,
    crate::pool::WORLD_MARKER_ENV,
    crate::pool::ENV_WORLD_DIR,
    crate::pool::ENV_WORLD_TEMP,
    crate::pool::ENV_PORT_START,
    crate::pool::ENV_PORT_END,
    crate::pool::ENV_CONTROL_SOCKET,
    crate::pool::ENV_TOKENS,
    control::WINDOW_LABEL_ENV,
    "RIGHTKIT_CONTROL_SERVICE",
    "RIGHTKIT_CONTROL_TRACE",
    "RIGHTKIT_CONTROL_STACK_ON_TIMEOUT",
    "RIGHTKIT_QA_HIDDEN",
    "RIGHTKIT_QA_FOCUS",
    "RIGHTKIT_QA_BACKGROUND",
    "RIGHTKIT_QA_PACE_MS",
    // The runner's own roots.
    "RIGHTKIT_QA_HOME",
    rightkit_daemon::ENV_SUITE_ROOT,
    "RIGHTKIT_RUN_ROOT",
    "RIGHTKIT_MANAGED_ROOT",
    // What `workspace::create` puts in every launch environment, and the runner's own settings.
    "RIGHTKIT_QA_NATIVE",
    "RIGHTKIT_QA_RUN_ID",
    "RIGHTKIT_QA_EVIDENCE_DIR",
    "RIGHTKIT_QA_CAPTURE_DIR",
    "RIGHTKIT_QA_MUTE",
    "RIGHTKIT_QA_TIER",
    "RIGHTKIT_QA_ENGINE",
    "RIGHTKIT_QA_UI_BINARY",
    crate::journeys::adapter::ENV_PROTOCOL,
    crate::journeys::adapter::ENV_SELECTION,
    crate::journeys::adapter::ENV_RESULTS,
    crate::journeys::run::QUALIFYING_ENV,
    crate::journeys::run::RUN_STORE_CAP_ENV,
    crate::journeys::run::RUN_TOKEN_ENV,
    crate::storage::HOME_ROOT_ENV,
    crate::storage::HOME_SWEEP_ENV,
    crate::storage::ROOT_CAP_ENV,
    crate::storage::ROOT_CEILING_ENV,
    crate::storage::HOME_MAX_AGE_ENV,
    crate::storage::KEEP_FAILED_HOME_ENV,
];

/// Why `key` is not an acceptable scenario input name, or `None`. The shape is
/// `^RIGHTKIT_[A-Z0-9]+(_[A-Z0-9]+)*$`: the prefix is required because a macOS launch carries
/// only `RIGHTKIT_*` keys (`control::check_env`).
fn input_key_refusal(key: &str) -> Option<String> {
    if key.len() > INPUT_KEY_MAX {
        return Some(format!("key is longer than {INPUT_KEY_MAX} bytes"));
    }
    let words_ok = key.strip_prefix("RIGHTKIT_").is_some_and(|rest| {
        rest.split('_').all(|w| {
            !w.is_empty()
                && w.bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        })
    });
    if !words_ok {
        return Some(
            "key must be RIGHTKIT_ followed by upper-case letters and digits in words joined by single underscores (RIGHTKIT_<APP>_<NAME>); a macOS launch carries only RIGHTKIT_* keys".into(),
        );
    }
    if RESERVED_INPUT_KEYS.contains(&key) {
        return Some("key is set or read by rightkit-qa or rightkit-daemon itself and cannot be set by a scenario".into());
    }
    control::key_names_credential(key).then(|| {
        "key names a credential; secrets are never passed to a launched app through its environment".to_string()
    })
}

/// Why `value` is not an acceptable scenario input value, or `None`.
fn input_value_refusal(value: &str) -> Option<String> {
    if value.len() > INPUT_VALUE_MAX {
        return Some(format!("value is longer than {INPUT_VALUE_MAX} bytes"));
    }
    value
        .contains(['\0', '\r', '\n'])
        .then(|| "value must not contain NUL, CR or LF".into())
}

/// Refuse a scenario file whose `inputs` break the rules above, naming `inputs.<KEY>` and why.
fn validate_inputs(path: &Path, inputs: &BTreeMap<String, String>) -> Result<()> {
    for (key, value) in inputs {
        if let Some(why) = input_key_refusal(key).or_else(|| input_value_refusal(value)) {
            return err(format!("{}: inputs.{key}: {why}", path.display()));
        }
    }
    Ok(())
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

/// A directory name for one scenario of one run: the scenario's name made path-safe, and a short
/// unique suffix.
fn scenario_slug(name: &str) -> String {
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{safe}-{}", &crate::util::new_id()[..8])
}

impl<'a> Ctx<'a> {
    pub fn new(
        cfg: &'a QaConfig,
        engine: Option<EngineTarget>,
        ui_binary: Option<PathBuf>,
        ws: &'a QaWorkspace,
        tracker: &'a Tracker,
        scenario: &str,
    ) -> Result<Self> {
        // The app's real data folder (rightkit#1437): scenarios read its logs and state from here,
        // and the journey run snapshots and restores it. `[snapshot]` is required.
        let data_dir = cfg.real_data_dir()?;
        let slug = scenario_slug(scenario);
        let log_dir = ws.evidence_dir.join(&slug);
        std::fs::create_dir_all(&log_dir)?;
        // Templates passed through ui_command/ui_eval must never name managed storage.
        let app_evidence_dir = ws.home.join("evidence").join(&slug);
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
/// The environment a `ui_launch` hands the app, from the lowest layer to the highest: the
/// config's `[ui] env` and `[env]`, the scenario's
/// `inputs`, the window the in-app control server drives (`[ui] window_label` in
/// [`control::WINDOW_LABEL_ENV`], not the process label of the launch spec), then the step's own
/// `env`. A later layer replaces an earlier value for the same key, so a step can override an
/// input or a config value for one launch. Each key appears once, because `control::launch`
/// refuses a second, different value for a key it has already taken.
fn ui_launch_env(
    cfg: &QaConfig,
    inputs: &BTreeMap<String, String>,
    step_env: Vec<(String, String)>,
) -> Vec<(String, String)> {
    let layers = cfg
        .ui
        .env
        .iter()
        .chain(&cfg.env)
        .map(|(k, v)| (k.clone(), v.clone()))
        .chain(inputs.iter().map(|(k, v)| (k.clone(), v.clone())))
        .chain([(
            control::WINDOW_LABEL_ENV.to_string(),
            cfg.ui.window_label.clone(),
        )])
        .chain(step_env);
    let mut env: Vec<(String, String)> = Vec::new();
    for (key, value) in layers {
        match env.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = value,
            None => env.push((key, value)),
        }
    }
    env
}
fn cli_opts(ctx: &Ctx, p: &Map<String, Value>) -> CliOptions {
    CliOptions {
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
            let result = exec(ctx, &step.kind, &params, &step.inputs)?;
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

fn exec(
    ctx: &mut Ctx,
    kind: &str,
    p: &Map<String, Value>,
    inputs: &BTreeMap<String, String>,
) -> Result<Value> {
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
            let env = ui_launch_env(ctx.cfg, inputs, env_pairs(p));
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
            let c = control::launch_declared(&spec, ctx.ws, ctx.tracker, &ctx.cfg.ui.env_keys)?;
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
