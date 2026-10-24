//! A QA run: select scenarios by tier, create the isolated workspace, run the scenarios through the
//! pool (one world, one tracker and, where declared, resource tokens per scenario; see [`crate::pool`]),
//! sweep orphans, and write hashed evidence. A qualifying run has no app-wide run lock: a scenario that
//! uses a machine-wide resource declares its token instead. The in-process path holds `native.lock`
//! for the whole run and runs one scenario at a time.
use crate::audio::SystemMute;
use crate::config::{validate_tier_list, QaConfig};
use crate::engine::{build_engine, EngineTarget};
use crate::evidence::{Bundle, Check, CheckStatus, RunStatus};
use crate::lock::{self, LockOptions};
use crate::plugin::PluginSpec;
use crate::pool::{self, HomeTemplate, Job, Pool, SpecInputs, WorldPlan};
use crate::process::{sweep_registries, Tracker};
use crate::scenario::{
    discover, requirement_gate, resolve_run_kind, run_step, scenario_kind, tier_selected,
    unmet_requirement, Ctx, Gate, Scenario, SkipKind, StepHandlers, StepRecord,
};
use crate::steps::{self, StepState};
use crate::storage::{self, RunKind, RunOutcome};
use crate::supervise;
use crate::util::{err, now_iso, platform, Error, Result};
use crate::worker::{
    ScenarioSpec, StepSample, StepTiming, Survivor, Token, WorkerLaunch, WorkerOutcome, World,
    SCHEMA_VERSION,
};
use crate::workspace::{self, QaWorkspace};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Per-scenario callback: gets the scenario's [`Ctx`] (already built, run kind set) and the scenario.
pub type ScenarioHook = Arc<dyn for<'a> Fn(&mut Ctx<'a>, &Scenario) -> Result<()> + Send + Sync>;

/// App-defined behaviour for a suite run (rightkit#456). `SuiteHooks::default()` changes nothing.
#[derive(Clone, Default)]
pub struct SuiteHooks {
    /// App-defined step kinds, see [`StepHandlers`].
    pub steps: StepHandlers,
    /// Runs once per scenario, after its [`Ctx`] exists and before its first step: start the app's
    /// mock, set `ctx.vars` entries (`mock_url`, ...), register cleanup. An `Err` fails the scenario
    /// without running its steps; [`SuiteHooks::teardown`] still runs.
    pub setup: Option<ScenarioHook>,
    /// Runs once per scenario that reached setup, after its steps (pass or fail) and before the
    /// built-in teardown (UI, sidecars, mocks) and the run's orphan sweep, while `ctx` is still
    /// live. An `Err` is a teardown problem and fails an otherwise passing scenario.
    pub teardown: Option<ScenarioHook>,
}

impl SuiteHooks {
    /// Register a custom step kind (builder style).
    pub fn step(
        mut self,
        kind: &str,
        handler: impl for<'a> Fn(&mut Ctx<'a>, &serde_json::Map<String, Value>) -> Result<Value>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        self.steps.register(kind, handler);
        self
    }
    pub fn setup(
        mut self,
        f: impl for<'a> Fn(&mut Ctx<'a>, &Scenario) -> Result<()> + Send + Sync + 'static,
    ) -> Self {
        self.setup = Some(Arc::new(f));
        self
    }
    pub fn teardown(
        mut self,
        f: impl for<'a> Fn(&mut Ctx<'a>, &Scenario) -> Result<()> + Send + Sync + 'static,
    ) -> Self {
        self.teardown = Some(Arc::new(f));
        self
    }
}

impl std::fmt::Debug for SuiteHooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SuiteHooks")
            .field("steps", &self.steps)
            .field("setup", &self.setup.is_some())
            .field("teardown", &self.teardown.is_some())
            .finish()
    }
}

#[derive(Debug, Clone, Default)]
pub struct SuiteOptions {
    pub tier: Option<String>,
    pub only: Option<String>,
    pub engine: Option<PathBuf>,
    /// An explicit UI executable. Skips the resolver's validation; still refused when it is an
    /// installed app (see [`explicit_ui_binary`]).
    pub ui_binary: Option<PathBuf>,
    pub build_engine: bool,
    pub build_ui: bool,
    pub keep_data: bool,
    /// Run kind: `functional` (clicks, state, text; no screenshots or recordings) or `visual`.
    /// `None` lets the scenarios decide ([`resolve_run_kind`]); with none declaring `visual` the
    /// run is functional.
    pub kind: Option<RunKind>,
    /// Explicit scenario files (override discovery).
    pub files: Vec<PathBuf>,
    /// App-defined step kinds and per-scenario setup/teardown.
    pub hooks: SuiteHooks,
    /// Kill tracked processes still alive after each scenario's teardown, without failing the
    /// scenario, so one scenario's stray process cannot fail the ones after it. A scenario can
    /// also set `kill_orphans = true`. Off by default: survivors fail the scenario.
    pub kill_orphans: bool,
    /// The app's step plugin (protocol v1, see [`crate::plugin`]): started per scenario by
    /// [`steps::setup`], its kinds run through [`steps::dispatch`]. Set from `[plugin]` in
    /// `rightkit-qa.toml` (`QaConfig::plugin_spec`) or by a library caller. A qualifying run hands it
    /// to each worker.
    pub plugin: Option<PluginSpec>,
}

#[derive(Debug, Clone)]
pub struct Outcome {
    pub name: String,
    pub file: PathBuf,
    pub tier: String,
    pub status: CheckStatus,
    pub reason: Option<String>,
    pub ms: u128,
    pub steps: Vec<StepRecord>,
    /// Why a `Skipped` outcome was skipped (Sage C5); `None` for every other status. The pass rule
    /// reads this, never the text of `reason`.
    pub skip: Option<SkipKind>,
    /// Resource tokens the scenario declared and the pool held for it.
    pub tokens: Vec<Token>,
    /// The scenario's world directory; `None` for a scenario that never ran (skipped, refused).
    pub world: Option<PathBuf>,
    /// Processes found alive at teardown, read before any cleanup (`Ctx::finish`, or the worker's
    /// outcome). Empty for a scenario that never ran.
    pub survivors: Vec<Survivor>,
    /// Engine processes the scenario started (the receipt's `engine_starts`); 0 when it never ran.
    pub engine_starts: u32,
}

impl Outcome {
    /// Whether this outcome fails the run. Only `Passed` passes. A `Skipped` outcome is tolerated only
    /// when its kind is ([`SkipKind::tolerated`]): a qualifying run tolerates only `OtherPlatform`.
    /// `Failed`, `Errored`, `NotRun` and a skip with no kind always fail.
    pub fn counts_as_failure(&self, qualifying: bool) -> bool {
        match self.status {
            CheckStatus::Passed => false,
            CheckStatus::Skipped => !self.skip.is_some_and(|k| k.tolerated(qualifying)),
            _ => true,
        }
    }

    fn to_value(&self) -> Value {
        json!({
            "name": self.name, "file": self.file.to_string_lossy(), "tier": self.tier,
            "status": serde_json::to_value(self.status).unwrap_or(Value::Null), "reason": self.reason, "ms": self.ms,
            "skipKind": self.skip.map(SkipKind::as_str),
            "tokens": self.tokens.iter().map(|t| pool::token_name(*t)).collect::<Vec<_>>(),
            "world": self.world.as_ref().map(|w| w.to_string_lossy().into_owned()),
            "engineStarts": self.engine_starts,
            "survivors": serde_json::to_value(&self.survivors).unwrap_or(Value::Null),
            "steps": self.steps.iter().map(|s| json!({"index": s.index, "kind": s.kind, "name": s.name, "ok": s.ok, "ms": s.ms, "error": s.error})).collect::<Vec<_>>(),
        })
    }
}

#[derive(Debug)]
pub struct SuiteReport {
    pub exit_code: i32,
    pub run_id: String,
    pub run_dir: PathBuf,
    pub evidence_path: PathBuf,
    pub results_path: PathBuf,
    pub outcomes: Vec<Outcome>,
}

/// Whether `only` selects the scenario: `only` is one name or a comma-separated list; a name
/// matches when it is a substring of the scenario name or file path, or equals the file stem (the
/// file name without `.scenario.toml`) exactly.
pub fn only_matches(only: &str, name: &str, file: &Path) -> bool {
    let fname = file
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let stem = fname
        .strip_suffix(".scenario.toml")
        .map(str::to_string)
        .unwrap_or_else(|| {
            file.file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default()
        });
    let path = file.to_string_lossy();
    only.split(',')
        .map(str::trim)
        .filter(|o| !o.is_empty())
        .any(|o| name.contains(o) || path.contains(o) || stem == o)
}

pub fn select(cfg: &QaConfig, opts: &SuiteOptions) -> Result<Vec<Scenario>> {
    let tier = opts
        .tier
        .clone()
        .unwrap_or_else(|| cfg.default_tier.clone());
    validate_tier_list(&tier)?;
    let files = if opts.files.is_empty() {
        discover(&cfg.scenario_dirs)?
    } else {
        opts.files.clone()
    };
    let mut out = vec![];
    for f in files {
        let s = Scenario::load(&f)?;
        let matches_only = opts
            .only
            .as_ref()
            .map(|o| only_matches(o, &s.name, &f))
            .unwrap_or(true);
        if matches_only && (!opts.files.is_empty() || tier_selected(&s.tier, &tier)) {
            out.push(s);
        }
    }
    Ok(out)
}

fn engine_target(
    cfg: &QaConfig,
    opts: &SuiteOptions,
    tracker: &Tracker,
    tier: &str,
) -> Result<Option<EngineTarget>> {
    let binary = if let Some(p) = &opts.engine {
        Some(p.clone())
    } else if let Ok(p) = std::env::var("RIGHTKIT_QA_ENGINE") {
        Some(PathBuf::from(p))
    } else if opts.build_engine {
        if cfg.engine.build.is_empty() {
            return err("--build needs [engine].build in the config");
        }
        let pkg = cfg.engine.package.clone().ok_or_else(|| {
            crate::util::Error("--build needs [engine].package (the cargo target name)".into())
        })?;
        let build_env: Vec<(String, String)> = cfg
            .env
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        Some(build_engine(
            &cfg.app_root,
            &cfg.engine.build,
            &pkg,
            &build_env,
            tracker,
        )?)
    } else {
        cfg.engine_binary()?
    };
    Ok(binary.map(|binary| engine_target_for(cfg, binary, tier)))
}

/// The engine target for `binary` as the run builds it: the config's env, the run's tier and mute
/// markers, and the config's data-dir and forbidden-arg rules. The worker (see [`run_spec`]) builds
/// its copy the same way from the binary path the runner resolved.
fn engine_target_for(cfg: &QaConfig, binary: PathBuf, tier: &str) -> EngineTarget {
    let mut env: Vec<(String, String)> = cfg
        .env
        .iter()
        .chain(cfg.engine.env.iter())
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    env.push(("RIGHTKIT_QA_TIER".into(), tier.into()));
    env.push(("RIGHTKIT_QA_MUTE".into(), "1".into()));
    EngineTarget {
        env,
        data_dir_env: cfg.engine.data_dir_env.clone(),
        data_dir_arg: cfg.engine.data_dir_arg.clone(),
        forbidden_args: cfg.engine.forbidden_args.clone(),
        ..EngineTarget::new(binary)
    }
}

/// A UI binary the caller named (`SuiteOptions::ui_binary`, `--ui-binary`, `RIGHTKIT_QA_UI_BINARY`).
/// It skips the resolver's checks (name, release directory, QA marker: the caller said which file),
/// but the installed-app refusal stays in force (Orthic-Labs/rightkit#235), so the suite fails before
/// any scenario starts instead of at the first `ui_launch`.
pub fn explicit_ui_binary(path: &Path) -> Result<PathBuf> {
    crate::installed::refuse_installed(path, "UI app binary")?;
    Ok(path.to_path_buf())
}

/// An explicitly named UI binary, launched from the fixed stash like every other source (macOS keys privacy
/// grants to the launch path, so the path must not follow the file the caller happened to name). The
/// control-marker check stays skipped for it, and an app bundle (`.app`) is staged at its fixed path by
/// `control::launch`.
fn stashed_explicit_ui_binary(cfg: &QaConfig, path: &Path) -> Result<PathBuf> {
    let named = explicit_ui_binary(path)?;
    if named.is_dir() || crate::binary::is_stashed(&named, &cfg.app_root) {
        return Ok(named);
    }
    crate::binary::stash_binary(&named, &cfg.app_root, None, false)
}

fn ui_binary(cfg: &QaConfig, opts: &SuiteOptions, tracker: &Tracker) -> Result<Option<PathBuf>> {
    if let Some(p) = &opts.ui_binary {
        return stashed_explicit_ui_binary(cfg, p).map(Some);
    }
    if let Ok(p) = std::env::var("RIGHTKIT_QA_UI_BINARY") {
        return stashed_explicit_ui_binary(cfg, Path::new(&p)).map(Some);
    }
    if opts.build_ui {
        if cfg.ui.build.is_empty() {
            return err("--build-ui needs [ui].build in the config");
        }
        let pkg = cfg.ui.package.clone().ok_or_else(|| {
            crate::util::Error("--build-ui needs [ui].package (the cargo target name)".into())
        })?;
        let env: Vec<(String, String)> = cfg
            .env
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let built = build_engine(&cfg.app_root, &cfg.ui.build, &pkg, &env, tracker)?;
        // Keep a stable QA-only copy so a later plain dev build cannot displace it.
        let kept =
            crate::binary::stash_qa_build(&built, &cfg.app_root, cfg.ui.pointer_file.as_deref())?;
        return Ok(Some(kept));
    }
    // No override and no build requested: the dev-build resolver (pointer file, broker target root,
    // then `ui.binaries`). A config that names no executable keeps the old "none configured" answer.
    match cfg.resolve_ui_binary() {
        Ok(found) => {
            eprintln!(
                "[qa] ui binary ({:?}): {}",
                found.source,
                found.path.display()
            );
            Ok(Some(found.path))
        }
        // A found-but-not-QA build fails closed with its instructive message.
        Err(err) if err.0.contains("test-mode/control build") => Err(err),
        Err(err) => {
            eprintln!("[qa] dev-build resolver: {err}; falling back to ui.binaries");
            // Same rule as every other source: launch from the fixed stash, not from `target/`.
            match cfg.ui_binary()? {
                Some(p) => stashed_explicit_ui_binary(cfg, &p).map(Some),
                None => Ok(None),
            }
        }
    }
}

pub fn run_suite(cfg: &QaConfig, opts: &SuiteOptions) -> Result<SuiteReport> {
    run_suite_at(cfg, opts, None, RunPolicy::default())
}

/// [`run_suite`] judged as a qualifying run (Sage C5): the run passes only when every scenario passed
/// or was skipped because it requires another OS. An `optional` skip, an unmet requirement and a
/// scenario left out by tier all fail it.
///
/// A qualifying run also runs each scenario in a supervised worker (`<this executable> worker <world
/// dir>`, see [`crate::supervise`] and [`crate::worker_main`]), so this executable must be the
/// `rightkit-qa` CLI, or an embedding binary that routes `worker <dir>` to
/// [`crate::worker_main::main`] with [`run_spec`]. A worker cannot carry in-process
/// [`SuiteHooks`]: a qualifying run with hooks is refused (use the step plugin, [`SuiteOptions::plugin`]).
pub fn run_suite_qualifying(cfg: &QaConfig, opts: &SuiteOptions) -> Result<SuiteReport> {
    run_suite_at(cfg, opts, None, RunPolicy::qualifying_with_workers()?)
}

/// Test/embedding entry point with an explicit managed root.
pub fn run_suite_in_managed_root(
    cfg: &QaConfig,
    opts: &SuiteOptions,
    managed_root: &Path,
) -> Result<SuiteReport> {
    run_suite_at(cfg, opts, Some(managed_root), RunPolicy::default())
}

/// [`run_suite_in_managed_root`] judged as a qualifying run, see [`run_suite_qualifying`].
pub fn run_suite_qualifying_in_managed_root(
    cfg: &QaConfig,
    opts: &SuiteOptions,
    managed_root: &Path,
) -> Result<SuiteReport> {
    run_suite_at(
        cfg,
        opts,
        Some(managed_root),
        RunPolicy::qualifying_with_workers()?,
    )
}

/// How a run's outcomes are judged and where its scenarios run.
#[derive(Debug, Clone, Default)]
struct RunPolicy {
    /// The strict rule of [`run_suite_qualifying`].
    qualifying: bool,
    /// The executable that runs each scenario as `<exe> worker <world dir>`. `Some` for a qualifying
    /// run; `None` runs scenarios in this process (an ordinary run, and the unit tests that judge a
    /// qualifying run without spawning a worker).
    worker_exe: Option<PathBuf>,
}

impl RunPolicy {
    fn qualifying_with_workers() -> Result<Self> {
        let exe = std::env::current_exe().map_err(|e| {
            Error(format!(
                "cannot find this executable to start scenario workers: {e}"
            ))
        })?;
        Ok(Self {
            qualifying: true,
            worker_exe: Some(crate::util::plain_path(&exe)),
        })
    }
}

/// Why a macOS run with UI scenarios stays serial unless `[host] workers` says otherwise. macOS keys
/// privacy grants to one fixed bundle path and id per app (`control::stage_bundle`), replaced in place
/// on each launch, so two scenarios launching the UI would stage the same bundle, in this process or
/// in separate workers.
const MAC_UI_SERIAL_REASON: &str =
    "macOS UI scenarios share one fixed QA app bundle per app (set [host] workers to override)";

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn run_suite_at(
    cfg: &QaConfig,
    opts: &SuiteOptions,
    managed_override: Option<&Path>,
    policy: RunPolicy,
) -> Result<SuiteReport> {
    let platform = platform()?;
    let tier = opts
        .tier
        .clone()
        .unwrap_or_else(|| cfg.default_tier.clone());
    let scenarios = select(cfg, opts)?;
    if scenarios.is_empty() {
        return err(format!(
            "no scenarios matched tier '{tier}'{}",
            opts.only
                .as_ref()
                .map(|o| format!(" --only {o}"))
                .unwrap_or_default()
        ));
    }
    if policy.worker_exe.is_some()
        && (!opts.hooks.steps.is_empty()
            || opts.hooks.setup.is_some()
            || opts.hooks.teardown.is_some())
    {
        return err("a qualifying run runs each scenario in a worker process, which cannot carry in-process SuiteHooks (custom steps, setup, teardown); declare the app's step kinds in its step plugin instead (SuiteOptions::plugin)");
    }
    let run_kind = resolve_run_kind(opts.kind, &scenarios);
    let managed_root = managed_override
        .map(Path::to_path_buf)
        .unwrap_or_else(|| crate::storage::managed_root(None));
    let managed_app_root = crate::storage::prepare_app_root(&managed_root, &cfg.app)?;
    // The in-process path (no worker executable) runs every scenario in this process, on this
    // process's shared UI, engine and fixed app paths. It keeps the app-wide `native.lock` (ADR-0001
    // D-a), so two runs of one app never overlap here, and the pool runs one worker (see below).
    // A qualifying run (`worker_exe` set) takes no app lock: scenarios that share a machine-wide
    // resource declare a token, and the pool never runs two holders of one token at once.
    let lock_opts = LockOptions {
        timeout: Duration::from_secs(cfg.lock.timeout_s.unwrap_or(120)),
        stale: Duration::from_secs(cfg.lock.stale_s.unwrap_or(60)),
        ..Default::default()
    };
    let _native_lock = match policy.worker_exe {
        Some(_) => None,
        None => Some(lock::acquire(
            &managed_app_root.join("native.lock"),
            &format!("{}:{tier}", cfg.app),
            &lock_opts,
        )?),
    };
    let run_id = crate::util::new_id();
    // Earlier crashed runs: kill survivors by recorded pid + command line, then drop their dirs.
    let mut swept = sweep_registries(&managed_app_root, &run_id);
    swept.extend(pool::sweep_world_registries(&managed_app_root, &run_id));
    let ws = workspace::create(&managed_root, Some(&run_id), &cfg.app)?;
    ws.set_kind(run_kind)?;
    // Run-level tracker: engine and UI builds, and the final sweep. Each scenario has its own.
    let tracker = Tracker::with_registry(ws.root.join("pids.json"));
    let mut bundle = Bundle::new(&cfg.app, platform, &run_id)?;
    let _mute = cfg.audio.system_mute.then(SystemMute::engage);

    let needs_engine = scenarios
        .iter()
        .any(|s| s.target == "engine" && unmet_requirement(&s.requires, &cfg.app_root).is_none());
    let needs_ui = scenarios.iter().any(|s| s.target == "ui");
    // A `ui` scenario that seeds engine state before launch (`engine_seed`) needs the engine too.
    let needs_engine = needs_engine
        || scenarios.iter().any(|s| {
            s.target == "ui"
                && s.steps.iter().any(|st| st.kind == "engine_seed")
                && unmet_requirement(&s.requires, &cfg.app_root).is_none()
        });
    let engine = engine_target(cfg, opts, &tracker, &tier)?;
    let ui = if needs_ui {
        ui_binary(cfg, opts, &tracker)?
    } else {
        None
    };
    if needs_engine && engine.is_none() {
        return err("selected scenarios need the engine but no binary is configured (use --engine, RIGHTKIT_QA_ENGINE, [engine].binary, or --build)");
    }

    let identity = {
        let mut i = run_identity(
            cfg,
            &scenarios,
            engine.as_ref().map(|e| e.binary.as_path()),
            ui.as_deref(),
            needs_engine,
            false,
            // N3: the plugin binary is bound like the other launched binaries, so a swapped plugin
            // changes the run identity.
            opts.plugin.as_ref().map(|p| p.binary.as_path()),
        )?;
        if let Some(o) = i.as_object_mut() {
            o.insert("runKind".into(), json!(run_kind.as_str()));
        }
        i
    };
    bundle.set_identity(identity)?;
    println!(
        "[qa] app={} platform={platform} tier={tier} kind={run_kind} scenarios={} run={run_id}",
        cfg.app,
        scenarios.len()
    );
    if let Some(e) = &engine {
        println!("[qa] engine={}", e.binary.display());
    }
    if !swept.is_empty() {
        println!("[qa] swept orphan pids from earlier runs: {swept:?}");
    }

    // Settle everything that needs no run first, in selection order: another OS, an unmet
    // requirement, no UI binary, a kind conflict. What is left goes to the pool.
    let bundle = Mutex::new(bundle);
    let mut slots: Vec<Option<Outcome>> = vec![None; scenarios.len()];
    let mut runnable: Vec<(usize, RunKind)> = vec![];
    for (idx, sc) in scenarios.iter().enumerate() {
        let name = &sc.name;
        let settle = |status: CheckStatus,
                      skip: Option<SkipKind>,
                      reason: String,
                      details: Value|
         -> Result<Outcome> {
            lock(&bundle).add_check(name, status, details, &[])?;
            Ok(Outcome {
                name: name.clone(),
                file: sc.file.clone(),
                tier: sc.tier.clone(),
                status,
                reason: Some(reason),
                ms: 0,
                steps: vec![],
                skip,
                tokens: sc.grants.clone(),
                world: None,
                survivors: vec![],
                engine_starts: 0,
            })
        };
        match requirement_gate(sc, &cfg.app_root) {
            Gate::Skip { kind, reason } => {
                println!(
                    "[qa] SKIP  {name}: {reason} ({})",
                    match kind {
                        SkipKind::OtherPlatform => "other platform",
                        SkipKind::Optional => "optional",
                        SkipKind::UnmetRequirement => "unmet requirement",
                        SkipKind::TierNotSelected => "tier not selected",
                    }
                );
                let details = json!({"reason": reason, "skipKind": kind.as_str(), "optional": sc.optional, "platform": kind == SkipKind::OtherPlatform});
                slots[idx] = Some(settle(CheckStatus::Skipped, Some(kind), reason, details)?);
                continue;
            }
            Gate::Fail { reason } => {
                println!("[qa] FAIL  {name}: {reason}");
                let details = json!({"error": reason, "mandatory": true});
                slots[idx] = Some(settle(CheckStatus::Failed, None, reason, details)?);
                continue;
            }
            Gate::Run => {}
        }
        if sc.target == "ui" && ui.is_none() {
            let reason =
                "no UI binary for this platform ([ui].binaries / --ui-binary / --build-ui)"
                    .to_string();
            println!("[qa] FAIL  {name}: {reason}");
            let details = json!({"error": reason});
            slots[idx] = Some(settle(CheckStatus::Failed, None, reason, details)?);
            continue;
        }
        match scenario_kind(run_kind, sc.kind, name) {
            Ok(k) => runnable.push((idx, k)),
            Err(e) => {
                println!("[qa] FAIL  {name}: {e}");
                let details = json!({"error": e.0});
                slots[idx] = Some(settle(CheckStatus::Failed, None, e.0, details)?);
            }
        }
    }

    // The pool: one world and one tracker per scenario, tokens as locks, N workers.
    if !runnable.is_empty() {
        let jobs: Vec<Job> = runnable
            .iter()
            .map(|(idx, _)| Job {
                id: scenarios[*idx].name.clone(),
                tokens: scenarios[*idx].grants.clone(),
            })
            .collect();
        let serial = (platform == "darwin"
            && runnable
                .iter()
                .any(|(idx, _)| scenarios[*idx].target == "ui"))
        .then_some(MAC_UI_SERIAL_REASON);
        // In process, one worker whatever `[host] workers` says: the native lock is a serial run.
        let size = if policy.worker_exe.is_none() {
            pool::PoolSize {
                workers: 1,
                reason: "in-process run: native.lock serializes scenarios".into(),
            }
        } else {
            pool::host_pool_size(&cfg.host, jobs.len(), serial)
        };
        let plan = WorldPlan::new(run_id.clone(), ws.home.clone(), &cfg.host, jobs.len())?;
        let template = HomeTemplate::capture(&ws.home)?;
        println!("[qa] pool: {} worker(s) ({})", size.workers, size.reason);
        let env = RunEnv {
            cfg,
            hooks: &opts.hooks,
            kill_orphans: opts.kill_orphans,
            engine: engine.as_ref(),
            ui: ui.as_deref(),
            ws: &ws,
            template: Some(&template),
            bundle: &bundle,
            tier: &tier,
            plugin: opts.plugin.as_ref(),
            build: &Value::Null,
            // A qualifying run: each scenario runs as `<exe> worker <world dir>` under the
            // supervisor (`supervise::run_worker`); any other run stays in this process.
            worker_exe: policy.worker_exe.as_deref(),
        };
        let results = Pool::new(size.workers).run(&jobs, &plan, |k, _job, world| {
            let (idx, kind) = runnable[k];
            let sc = &scenarios[idx];
            let spec = pool::build_spec(
                &plan,
                world,
                SpecInputs {
                    app: &cfg.app,
                    platform,
                    scenario_id: &sc.name,
                    // The worker (W2-D) decodes the resolved scenario from this; the in-process
                    // runner has the loaded `Scenario` and reads only the identity.
                    scenario: json!({"name": sc.name, "file": sc.file.to_string_lossy(), "target": sc.target}),
                    tokens: &sc.grants,
                    scenario_ms: sc.timeout_s.unwrap_or(DEFAULT_SCENARIO_S).saturating_mul(1000),
                },
            );
            run_scenario(&env, sc, kind, &spec, world)
        })?;
        for (k, res) in results.into_iter().enumerate() {
            let idx = runnable[k].0;
            let sc = &scenarios[idx];
            slots[idx] = Some(match res {
                Ok(outcome) => outcome,
                // The pool could not run it (its world could not be built, or it panicked outside the
                // scenario): an errored scenario, never a pass.
                Err(why) => {
                    let reason = format!("scenario could not run: {why}");
                    println!("[qa] ERROR {}: {reason}", sc.name);
                    let _ = lock(&bundle).add_check(
                        &sc.name,
                        CheckStatus::Errored,
                        json!({"error": reason}),
                        &[],
                    );
                    Outcome {
                        name: sc.name.clone(),
                        file: sc.file.clone(),
                        tier: sc.tier.clone(),
                        status: CheckStatus::Errored,
                        reason: Some(reason),
                        ms: 0,
                        steps: vec![],
                        skip: None,
                        tokens: sc.grants.clone(),
                        world: None,
                        survivors: vec![],
                        engine_starts: 0,
                    }
                }
            });
        }
    }
    let outcomes: Vec<Outcome> = slots.into_iter().flatten().collect();
    let mut bundle = bundle.into_inner().unwrap_or_else(|e| e.into_inner());
    // Workers finish in any order; the evidence lists checks in selection order.
    let order: HashMap<&str, usize> = scenarios
        .iter()
        .enumerate()
        .map(|(i, s)| (s.name.as_str(), i))
        .collect();
    bundle
        .checks
        .sort_by_key(|c| order.get(c.name.as_str()).copied().unwrap_or(usize::MAX));

    tracker.kill_all();
    // Only Passed passes. A skip is tolerated by kind (SkipKind::tolerated); failed, errored and
    // never-run outcomes always fail.
    let failed = outcomes
        .iter()
        .filter(|o| o.counts_as_failure(policy.qualifying))
        .count();
    let passed = outcomes
        .iter()
        .filter(|o| o.status == CheckStatus::Passed)
        .count();
    // A run whose scenarios were all skipped proves nothing.
    bundle.finish(if failed == 0 && passed > 0 {
        RunStatus::Passed
    } else {
        RunStatus::Failed
    });
    let evidence_path = ws.evidence_dir.join("evidence.json");
    let results_path = ws.evidence_dir.join("results.json");
    std::fs::write(
        &results_path,
        serde_json::to_vec_pretty(&json!({
            "app": cfg.app, "platform": platform, "runId": run_id, "tier": tier, "kind": run_kind.as_str(), "finishedAt": now_iso(),
            "qualifying": policy.qualifying,
            "passed": passed, "failed": failed, "skipped": outcomes.len() - passed - failed,
            "scenarios": outcomes.iter().map(Outcome::to_value).collect::<Vec<_>>(),
        }))?,
    )?;
    bundle.write(&evidence_path)?;
    let _ = std::fs::write(
        ws.root.parent().unwrap_or(&ws.root).join("latest.json"),
        serde_json::to_vec_pretty(
            &json!({"runId": run_id, "evidence": evidence_path.to_string_lossy(), "results": results_path.to_string_lossy()}),
        )?,
    );
    // `keep_data` says the caller reads this run's home and worlds afterwards. Retention (this run's
    // and any concurrent run's of the same app) would otherwise remove it as an older passing run,
    // so the request is recorded before the outcome makes the run eligible.
    if opts.keep_data {
        if let Err(e) = crate::retention::mark_keep_data(&ws.root) {
            println!("[qa] WARN could not mark the run as kept (retention may remove it): {e}");
        }
    }
    // Record pass or fail in the run marker before it is completed: retention reads only this.
    let _ = ws.set_outcome(if failed == 0 && passed > 0 {
        RunOutcome::Passed
    } else {
        RunOutcome::Failed
    });
    ws.finish()?;
    if !opts.keep_data && failed > 0 && storage::keep_failed_home_requested() {
        println!(
            "[qa] kept QA home of the failed run: {}",
            storage::describe_kept_home(&ws.home)
        );
    } else if !opts.keep_data {
        let report = workspace::dispose(&ws, true, 5, 200);
        for (p, why) in report.retained {
            println!("[qa] WARN could not remove {}: {why}", p.display());
        }
    }
    println!(
        "[qa] {passed} passed, {failed} failed, {} skipped; evidence {}",
        outcomes.len() - passed - failed,
        evidence_path.display()
    );
    // Retention right after the job: failed runs and the newest pass per kind stay, the rest of
    // the passing runs of this app go. It never fails the job.
    let pruned = crate::retention::prune_runs_after_job(&managed_root, &cfg.app);
    println!("[qa] run retention: {}", pruned.summary());
    for (path, why) in pruned
        .kept
        .iter()
        .filter(|(_, why)| why.starts_with("not removed"))
    {
        println!("[qa] WARN {}: {why}", path.display());
    }
    Ok(SuiteReport {
        exit_code: if failed == 0 && passed > 0 { 0 } else { 1 },
        run_id,
        run_dir: ws.root.clone(),
        evidence_path,
        results_path,
        outcomes,
    })
}

/// Default wall-time budget of one scenario, in seconds, when it declares no `timeout_s`.
const DEFAULT_SCENARIO_S: u64 = 600;

/// What every scenario of one run shares. All of it is read-only or behind a lock, so the pool's
/// workers hold it by reference.
struct RunEnv<'a> {
    cfg: &'a QaConfig,
    hooks: &'a SuiteHooks,
    kill_orphans: bool,
    engine: Option<&'a EngineTarget>,
    ui: Option<&'a Path>,
    ws: &'a QaWorkspace,
    /// How a world home is built. `Some` in the runner; `None` in a worker, which is handed a world
    /// the runner already built ([`run_spec`]).
    template: Option<&'a HomeTemplate>,
    bundle: &'a Mutex<Bundle>,
    /// The run's tier, in the engine's environment ([`engine_target_for`]).
    tier: &'a str,
    /// The app's step plugin, started per scenario by [`steps::setup`].
    plugin: Option<&'a PluginSpec>,
    /// The build record's binaries by role, exposed to steps as `${build.<role>}`. `Null` outside a
    /// journeys worker.
    build: &'a Value,
    /// `Some` in a qualifying run: the executable started as `worker <world dir>` for each scenario.
    /// Always `None` inside a worker.
    worker_exe: Option<&'a Path>,
}

/// What the steps of one scenario produced, before the verdict is recorded.
struct Executed {
    failure: Option<String>,
    steps: Vec<StepRecord>,
    /// Names of the artifacts hashed into the bundle for this scenario.
    names: Vec<String>,
    /// The scenario could not be set up or finished abnormally (a panic, a step plugin that crashed,
    /// an engine that failed to start or died, a worker that did not report): `errored`, not `failed`.
    errored: bool,
    /// Processes alive at teardown, read before any cleanup.
    survivors: Vec<Survivor>,
    /// Engine processes the scenario started.
    engine_starts: u32,
    /// Starts per environment set (W2-R); empty when the per-set counts do not add up to the total.
    engine_starts_by_env: BTreeMap<String, u32>,
    /// What the scenario launched, read at the launch (W2-R); `None` when it launched nothing.
    launch: Option<WorkerLaunch>,
}

impl Executed {
    /// A scenario that could not run at all: `errored` with `why`, nothing recorded.
    fn refused(why: String) -> Self {
        Self {
            failure: Some(why),
            steps: vec![],
            names: vec![],
            errored: true,
            survivors: vec![],
            engine_starts: 0,
            engine_starts_by_env: BTreeMap::new(),
            launch: None,
        }
    }
}

/// Run one scenario to a recorded [`Outcome`]. This is the single call the pool makes per scenario.
/// An ordinary run executes it in this process, in `world`. A qualifying run (`env.worker_exe`)
/// writes `scenario.json`, starts `<exe> worker <world.dir>` under the supervisor ([`supervise`]: the
/// Job Object or process group, the deadlines, the survivor record) and reads `outcome.json` back
/// ([`run_spec`] is the worker's side); the queue, the worlds and the tokens around it do not change.
fn run_scenario(
    env: &RunEnv<'_>,
    sc: &Scenario,
    kind: RunKind,
    spec: &ScenarioSpec,
    world: &World,
) -> Outcome {
    let started = Instant::now();
    let executed = match env.worker_exe {
        Some(exe) => run_in_worker(env, sc, kind, spec, world, exe),
        None => run_in_process(env, sc, kind, spec, world, started),
    };
    conclude(env, sc, world, started, executed)
}

/// Build the world's workspace (its directories, binding and environment). The runner's step.
fn prepare_world(env: &RunEnv<'_>, spec: &ScenarioSpec, world: &World) -> Result<QaWorkspace> {
    let template = env
        .template
        .ok_or_else(|| Error("no home template: only the runner builds a scenario world".into()))?;
    pool::world_workspace(env.ws, template, world, &spec.grants)
}

/// [`execute`] behind a panic guard: a panic kills what the scenario's tracker holds and is
/// `errored`, never a pass.
#[allow(clippy::too_many_arguments)]
fn execute_guarded(
    env: &RunEnv<'_>,
    sc: &Scenario,
    kind: RunKind,
    ws: &QaWorkspace,
    world: &World,
    grants: &[Token],
    tracker: &Tracker,
    started: Instant,
) -> Executed {
    let attempt = catch_unwind(AssertUnwindSafe(|| {
        execute(env, sc, kind, ws, world, grants, tracker, started)
    }));
    match attempt {
        Ok(done) => done,
        Err(p) => {
            tracker.kill_all();
            let text = p
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| p.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "no message".into());
            Executed::refused(format!("scenario panicked: {text}"))
        }
    }
}

/// The ordinary run: build the world, then execute the scenario in this process.
fn run_in_process(
    env: &RunEnv<'_>,
    sc: &Scenario,
    kind: RunKind,
    spec: &ScenarioSpec,
    world: &World,
    started: Instant,
) -> Executed {
    if let Err(e) = spec.validate() {
        return Executed::refused(format!("scenario spec refused: {}", e.0));
    }
    let wsw = match prepare_world(env, spec, world) {
        Ok(w) => w,
        Err(e) => {
            return Executed::refused(format!("scenario world could not be built: {}", e.0));
        }
    };
    // One tracker per scenario: its teardown can only see, and only kill, its own processes.
    let tracker = Tracker::with_registry(pool::registry_path(&env.ws.root, world));
    execute_guarded(env, sc, kind, &wsw, world, &spec.grants, &tracker, started)
}

/// The step plugin's description in `scenario.json`. The plugin's `env` is written to the world
/// directory, so it must not carry secrets.
fn plugin_json(p: &PluginSpec) -> Value {
    json!({
        "binary": p.binary.to_string_lossy(),
        "args": p.args,
        "env": p.env.iter().map(|(k, v)| json!([k, v])).collect::<Vec<_>>(),
        "helloTimeoutMs": u64::try_from(p.hello_timeout.as_millis()).unwrap_or(u64::MAX),
    })
}

pub(crate) fn plugin_from_json(v: &Value) -> Result<PluginSpec> {
    let binary = v
        .get("binary")
        .and_then(Value::as_str)
        .ok_or_else(|| Error("scenario.json: plugin has no binary".into()))?;
    let mut spec = PluginSpec::new(binary);
    spec.args = v
        .get("args")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    spec.env = v
        .get("env")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|p| {
                    let p = p.as_array()?;
                    Some((
                        p.first()?.as_str()?.to_string(),
                        p.get(1)?.as_str()?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    if let Some(ms) = v.get("helloTimeoutMs").and_then(Value::as_u64) {
        spec.hello_timeout = Duration::from_millis(ms);
    }
    Ok(spec)
}

/// What a worker needs beyond the contract's own fields to rebuild this scenario's environment:
/// the config path, the binaries the runner resolved, the run kind, the plugin, the file digest of
/// the scenario it was selected with, and the world's workspace (the dirs and environment
/// [`pool::world_workspace`] built). Added to the spec's opaque `scenario` object as `runtime`.
fn worker_runtime(
    env: &RunEnv<'_>,
    sc: &Scenario,
    kind: RunKind,
    wsw: &QaWorkspace,
) -> Result<Value> {
    Ok(json!({
        "config": env.cfg.config_path.to_string_lossy(),
        "tier": env.tier,
        "kind": kind.as_str(),
        "killOrphans": env.kill_orphans,
        "engine": env.engine.map(|e| e.binary.to_string_lossy().into_owned()),
        "ui": env.ui.map(|p| p.to_string_lossy().into_owned()),
        "scenarioSha256": file_sha(&sc.file)?,
        "plugin": env.plugin.map(plugin_json),
        "workspace": {
            "root": wsw.root.to_string_lossy(),
            "evidenceDir": wsw.evidence_dir.to_string_lossy(),
            "captureDir": wsw.capture_dir.to_string_lossy(),
            "env": wsw.env,
        },
    }))
}

/// The qualifying run: build the world, hand the worker its `runtime`, run it under the supervisor
/// and adopt what it reports. The runner re-hashes every artifact the worker names, so evidence in
/// the bundle is what the runner read from disk, never what the worker claimed.
fn run_in_worker(
    env: &RunEnv<'_>,
    sc: &Scenario,
    kind: RunKind,
    spec: &ScenarioSpec,
    world: &World,
    exe: &Path,
) -> Executed {
    if let Err(e) = spec.validate() {
        return Executed::refused(format!("scenario spec refused: {}", e.0));
    }
    let wsw = match prepare_world(env, spec, world) {
        Ok(w) => w,
        Err(e) => {
            return Executed::refused(format!("scenario world could not be built: {}", e.0));
        }
    };
    let runtime = match worker_runtime(env, sc, kind, &wsw) {
        Ok(r) => r,
        Err(e) => {
            return Executed::refused(format!("scenario worker could not be prepared: {}", e.0));
        }
    };
    let mut spec = spec.clone();
    match spec.scenario.as_object_mut() {
        Some(o) => {
            o.insert("runtime".into(), runtime);
        }
        None => {
            return Executed::refused("scenario spec refused: scenario is not an object".into())
        }
    }
    let reported = match supervise::run_worker(&spec, exe) {
        Ok(o) => o,
        Err(e) => {
            return Executed::refused(format!("scenario worker could not be run: {}", e.0));
        }
    };
    let mut names = vec![];
    let mut problems = vec![];
    for a in &reported.artifacts {
        match lock(env.bundle).add_artifact(&a.name, &a.path) {
            Ok(rec) if rec.sha256 == a.sha256 => names.push(rec.name),
            Ok(rec) => problems.push(format!(
                "artifact {} changed after the worker hashed it (sha256 {} != {})",
                a.name, rec.sha256, a.sha256
            )),
            Err(e) => problems.push(e.0),
        }
    }
    let said = |fallback: &str| {
        if reported.message.is_empty() {
            fallback.to_string()
        } else {
            reported.message.clone()
        }
    };
    let (mut failure, mut errored) = match reported.status {
        CheckStatus::Passed => (None, false),
        CheckStatus::Failed => (Some(said("the scenario failed")), false),
        CheckStatus::Errored => (Some(said("the worker reported errored")), true),
        other => (
            Some(format!(
                "the worker reported {}: {}",
                serde_json::to_value(other)
                    .ok()
                    .and_then(|v| v.as_str().map(String::from))
                    .unwrap_or_default(),
                reported.message
            )),
            true,
        ),
    };
    if failure.is_none() && !reported.survivors.is_empty() {
        failure = Some(format!(
            "{} process(es) survived teardown: {}",
            reported.survivors.len(),
            reported
                .survivors
                .iter()
                .map(|s| format!("{} (pid {})", s.name, s.pid))
                .collect::<Vec<_>>()
                .join(", ")
        ));
        errored = true;
    }
    if !problems.is_empty() {
        let msg = format!(
            "required evidence could not be recorded: {}",
            problems.join("; ")
        );
        failure = Some(match failure.take() {
            Some(f) => format!("{f}; {msg}"),
            None => msg,
        });
        errored = true;
    }
    Executed {
        failure,
        steps: vec![],
        names,
        errored,
        survivors: reported.survivors,
        engine_starts: reported.engine_starts,
        engine_starts_by_env: reported.engine_starts_by_env,
        launch: reported.launch,
    }
}

/// Record the verdict of one scenario in the evidence and report it. Both paths end here.
fn conclude(
    env: &RunEnv<'_>,
    sc: &Scenario,
    world: &World,
    started: Instant,
    executed: Executed,
) -> Outcome {
    let Executed {
        failure,
        steps,
        names,
        errored,
        survivors,
        engine_starts,
        engine_starts_by_env: _,
        launch: _,
    } = executed;
    let mut status = match (&failure, errored) {
        (None, _) => CheckStatus::Passed,
        (Some(_), true) => CheckStatus::Errored,
        (Some(_), false) => CheckStatus::Failed,
    };
    let mut failure = failure;
    let details = json!({"file": sc.file.to_string_lossy(), "tier": sc.tier, "steps": steps.len(), "error": failure, "world": world.dir.to_string_lossy(), "tokens": sc.tokens, "engineStarts": engine_starts, "survivors": survivors.len(), "worker": env.worker_exe.is_some()});
    if let Err(e) = lock(env.bundle).add_check(&sc.name, status, details, &names) {
        // The verdict could not be written into the evidence: the scenario did not finish cleanly.
        status = CheckStatus::Errored;
        failure = Some(match failure.take() {
            Some(f) => format!("{f}; evidence check could not be recorded: {}", e.0),
            None => format!("evidence check could not be recorded: {}", e.0),
        });
    }
    let word = match status {
        CheckStatus::Passed => "PASS",
        CheckStatus::Errored => "ERROR",
        _ => "FAIL",
    };
    println!(
        "[qa] {word}  {} ({} ms){}",
        sc.name,
        started.elapsed().as_millis(),
        failure
            .as_ref()
            .map(|f| format!("\n       {f}"))
            .unwrap_or_default()
    );
    Outcome {
        name: sc.name.clone(),
        file: sc.file.clone(),
        tier: sc.tier.clone(),
        status,
        reason: failure,
        ms: started.elapsed().as_millis(),
        steps,
        skip: None,
        tokens: sc.grants.clone(),
        world: Some(world.dir.clone()),
        survivors,
        engine_starts,
    }
}

/// Wall time per step kind, summed over the scenario, in the worker contract's form.
fn step_timing(steps: &[StepRecord]) -> BTreeMap<String, StepTiming> {
    let mut out: BTreeMap<String, StepTiming> = BTreeMap::new();
    for s in steps {
        let ms = u64::try_from(s.ms).unwrap_or(u64::MAX);
        let t = out.entry(s.kind.clone()).or_insert(StepTiming {
            count: 0,
            total_ms: 0,
            max_ms: 0,
        });
        t.count += 1;
        t.total_ms = t.total_ms.saturating_add(ms);
        t.max_ms = t.max_ms.max(ms);
    }
    out
}

fn runtime_str<'a>(rt: &'a serde_json::Map<String, Value>, key: &str) -> Result<&'a str> {
    rt.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| Error(format!("scenario.json runtime has no `{key}`")))
}

fn runtime_opt_path(rt: &serde_json::Map<String, Value>, key: &str) -> Option<PathBuf> {
    rt.get(key).and_then(Value::as_str).map(PathBuf::from)
}

/// The worker's side of a qualifying run: `rightkit-qa worker <world dir>` calls this (through
/// [`crate::worker_main::main`]) with the spec the runner wrote. It rebuilds the scenario's
/// environment from the spec's `runtime` section (`worker_runtime`), loads the scenario from the
/// file the runner selected (refused when it is not the same bytes), runs it in the world the runner
/// built, and reports the verdict, the engine starts, the survivors, the timing per step kind and the
/// artifacts it hashed. The verdict a worker reports is advice: the runner decides from this and from
/// what it observed itself.
///
/// A scenario that passes but leaves survivors (`kill_orphans`) is reported `errored`: the contract
/// refuses a pass with survivors.
pub fn run_spec(spec: &ScenarioSpec) -> Result<WorkerOutcome> {
    let rt = spec
        .scenario
        .get("runtime")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            Error(
                "scenario.json has no runtime section: it was not written by a qualifying run"
                    .into(),
            )
        })?;
    let cfg = QaConfig::load(Path::new(runtime_str(rt, "config")?))?;
    let file = spec
        .scenario
        .get("file")
        .and_then(Value::as_str)
        .ok_or_else(|| Error("scenario.json: scenario has no file".into()))?;
    let sc = Scenario::load(Path::new(file))?;
    if sc.name != spec.scenario_id {
        return err(format!(
            "scenario file {file} holds '{}', the runner selected '{}'",
            sc.name, spec.scenario_id
        ));
    }
    let selected = runtime_str(rt, "scenarioSha256")?;
    let found = file_sha(&sc.file)?;
    if found != selected {
        return err(format!(
            "scenario file {file} changed after the runner selected it (sha256 {found} != {selected})"
        ));
    }
    let kind = RunKind::parse(runtime_str(rt, "kind")?)?;
    let tier = runtime_str(rt, "tier")?.to_string();
    let wsv = rt
        .get("workspace")
        .and_then(Value::as_object)
        .ok_or_else(|| Error("scenario.json runtime has no `workspace`".into()))?;
    let wpath = |key: &str| -> Result<PathBuf> { Ok(PathBuf::from(runtime_str(wsv, key)?)) };
    let ws = QaWorkspace {
        run_id: spec.run_id.clone(),
        root: wpath("root")?,
        home: spec.world.home.clone(),
        data_dir: spec.world.data_dir.clone(),
        evidence_dir: wpath("evidenceDir")?,
        capture_dir: wpath("captureDir")?,
        env: wsv
            .get("env")
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string())))
                    .collect()
            })
            .unwrap_or_default(),
    };
    let engine = runtime_opt_path(rt, "engine").map(|b| engine_target_for(&cfg, b, &tier));
    let ui = runtime_opt_path(rt, "ui");
    let plugin = match rt.get("plugin").filter(|v| !v.is_null()) {
        Some(v) => Some(plugin_from_json(v)?),
        None => None,
    };
    let hooks = SuiteHooks::default();
    let bundle = Mutex::new(Bundle::new(&spec.app, &spec.platform, &spec.run_id)?);
    let build = spec
        .scenario
        .get("binaries")
        .cloned()
        .unwrap_or(Value::Null);
    let env = RunEnv {
        cfg: &cfg,
        hooks: &hooks,
        kill_orphans: rt
            .get("killOrphans")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || sc.kill_orphans,
        engine: engine.as_ref(),
        ui: ui.as_deref(),
        ws: &ws,
        template: None,
        bundle: &bundle,
        tier: &tier,
        plugin: plugin.as_ref(),
        build: &build,
        worker_exe: None,
    };
    let started = Instant::now();
    let tracker = Tracker::with_registry(pool::registry_path(&ws.root, &spec.world));
    let done = execute_guarded(
        &env,
        &sc,
        kind,
        &ws,
        &spec.world,
        &spec.grants,
        &tracker,
        started,
    );
    let mut status = match (&done.failure, done.errored) {
        (None, _) => CheckStatus::Passed,
        (Some(_), true) => CheckStatus::Errored,
        (Some(_), false) => CheckStatus::Failed,
    };
    let mut message = done.failure.clone().unwrap_or_default();
    if status == CheckStatus::Passed && !done.survivors.is_empty() {
        status = CheckStatus::Errored;
        message = format!(
            "{} process(es) survived teardown ({}); a survivor fails a worker scenario even when kill_orphans is set",
            done.survivors.len(),
            done.survivors
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let artifacts: Vec<_> = lock(&bundle)
        .artifacts
        .iter()
        .filter(|a| done.names.contains(&a.name))
        .cloned()
        .collect();
    let check = Check {
        name: sc.name.clone(),
        status,
        details: json!({"file": sc.file.to_string_lossy(), "tier": sc.tier, "steps": done.steps.len(), "error": done.failure, "world": spec.world.dir.to_string_lossy(), "tokens": sc.tokens, "engineStarts": done.engine_starts, "survivors": done.survivors.len()}),
        artifacts: done.names.clone(),
    };
    let samples: Vec<StepSample> = done
        .steps
        .iter()
        .map(|s| StepSample {
            label: s.name.clone(),
            kind: s.kind.clone(),
            ms: u64::try_from(s.ms).unwrap_or(u64::MAX),
            status: if s.ok {
                CheckStatus::Passed
            } else {
                CheckStatus::Failed
            },
        })
        .collect();
    Ok(WorkerOutcome {
        schema_version: SCHEMA_VERSION,
        run_id: spec.run_id.clone(),
        scenario_id: spec.scenario_id.clone(),
        status,
        message,
        engine_starts: done.engine_starts,
        engine_starts_by_env: done.engine_starts_by_env,
        survivors: done.survivors,
        timing: step_timing(&done.steps),
        steps: samples,
        launch: done.launch,
        checks: vec![check],
        artifacts,
    })
}

/// Run the setup hook, the steps, the teardown hooks and the artifact hashing of one scenario in its
/// own workspace and tracker.
#[allow(clippy::too_many_arguments)]
fn execute(
    env: &RunEnv<'_>,
    sc: &Scenario,
    kind: RunKind,
    ws: &QaWorkspace,
    world: &World,
    grants: &[Token],
    tracker: &Tracker,
    started: Instant,
) -> Executed {
    let name = &sc.name;
    let mut ctx = match Ctx::new(
        env.cfg,
        env.engine.cloned(),
        env.ui.map(Path::to_path_buf),
        ws,
        tracker,
        name,
    ) {
        Ok(c) => c.with_world(world.clone(), grants.to_vec()),
        Err(e) => {
            return Executed::refused(format!("scenario context could not be created: {}", e.0));
        }
    };
    ctx.kind = kind;
    ctx.handlers = env.hooks.steps.clone();
    ctx.kill_orphans = env.kill_orphans || sc.kill_orphans;
    if let Some(p) = env.plugin {
        ctx.steps = StepState::with_plugin(p.clone());
    }
    if env.build.is_object() {
        ctx.vars.insert("build".into(), env.build.clone());
    }
    let mut steps = vec![];
    let mut failure: Option<String> = None;
    let mut errored = false;
    let limit = Duration::from_secs(sc.timeout_s.unwrap_or(DEFAULT_SCENARIO_S));
    let mut run_steps = true;
    // The step plugin starts first, so the app's setup hook can already use its kinds; teardown runs
    // in the opposite order.
    if let Err(e) = steps::setup(&mut ctx, sc) {
        errored |= steps::is_errored(&e);
        failure = Some(format!("scenario setup: {}", e.0));
        run_steps = false;
    }
    if run_steps {
        if let Some(setup) = &env.hooks.setup {
            if let Err(e) = setup(&mut ctx, sc) {
                failure = Some(format!("scenario setup: {}", e.0));
                run_steps = false;
            }
        }
    }
    for (i, step) in sc.steps.iter().enumerate() {
        if !run_steps {
            break;
        }
        let t = Instant::now();
        if started.elapsed() > limit {
            failure = Some(format!(
                "scenario exceeded its {}s budget before step {}",
                limit.as_secs(),
                i + 1
            ));
            break;
        }
        let label = step.label.clone().unwrap_or_else(|| step.kind.clone());
        match run_step(&mut ctx, step) {
            Ok(_) => steps.push(StepRecord {
                index: i + 1,
                kind: step.kind.clone(),
                name: label,
                ok: true,
                ms: t.elapsed().as_millis(),
                error: None,
            }),
            Err(e) => {
                steps.push(StepRecord {
                    index: i + 1,
                    kind: step.kind.clone(),
                    name: label,
                    ok: false,
                    ms: t.elapsed().as_millis(),
                    error: Some(e.0.clone()),
                });
                // A plugin that crashed or broke the protocol is the infrastructure's failure.
                errored |= steps::is_errored(&e);
                failure = Some(e.0);
                break;
            }
        }
    }
    // The failure capture is a screenshot: only a visual scenario takes one.
    if failure.is_some() && ctx.kind == RunKind::Visual {
        if let Some((c, _)) = &ctx.ui {
            let p = ctx.log_dir.join("failure.png");
            if c.screenshot_to(&p).is_ok() {
                ctx.artifacts.push((format!("{name}/failure.png"), p));
            }
        }
    }
    let mut problems = vec![];
    if let Some(td) = &env.hooks.teardown {
        if let Err(e) = td(&mut ctx, sc) {
            problems.push(format!("scenario teardown: {}", e.0));
        }
    }
    if let Err(e) = steps::teardown(&mut ctx, sc) {
        errored |= steps::is_errored(&e);
        problems.push(format!("scenario teardown: {}", e.0));
    }
    // `finish` keeps what `teardown` would merge apart: engine problems make the scenario `errored`,
    // and the survivors and engine starts are read before any cleanup.
    // W2-R: what this scenario launched, read before teardown stops the app. The token probe needs
    // the running app, so it goes first; the launch facts need the engine start count, read after.
    let ui_launched = steps.iter().any(|s| s.kind == "ui_launch" && s.ok);
    let token_less_refused = match &ctx.ui {
        Some((c, _)) => Some(c.token_attached() && c.probe_token_required().is_ok()),
        None => ui_launched.then_some(false),
    };
    let mut launch_env = match &ctx.engine {
        Some(e) => e.env_for(Some(&ctx.data_dir), &[]),
        None => {
            let mut v: Vec<(String, String)> = ctx
                .ws
                .env
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            if let Some(var) = &ctx.cfg.engine.data_dir_env {
                v.push((var.clone(), ctx.data_dir.to_string_lossy().into()));
            }
            v
        }
    };
    // N2: the receipt records the token file the app was actually handed (the launch picked it),
    // not the world's copy in the workspace environment. One value only.
    if let Some((c, _)) = &ctx.ui {
        launch_env.retain(|(k, _)| k != crate::control::token::ENV_AUTH_FILE);
        launch_env.push((
            crate::control::token::ENV_AUTH_FILE.to_string(),
            c.control_token_file().to_string_lossy().into_owned(),
        ));
    }
    let engine_path = ctx.engine.as_ref().map(|e| e.binary.clone());
    let ui_path = ctx.ui_binary.clone();
    let (data_dir, data_dir_env, data_dir_arg) = (
        ctx.data_dir.clone(),
        ctx.cfg.engine.data_dir_env.clone(),
        ctx.cfg.engine.data_dir_arg.clone(),
    );
    let torn = ctx.finish();
    let total_starts = torn.engine_starts.total();
    let by_env = {
        let m = torn.engine_starts.by_env_set.clone();
        let sum = m.values().fold(0u32, |a, n| a.saturating_add(*n));
        if sum == total_starts {
            m
        } else {
            BTreeMap::new()
        }
    };
    let mut binaries = vec![];
    if total_starts > 0 {
        binaries.extend(engine_path);
    }
    if ui_launched {
        binaries.extend(ui_path);
    }
    let launch = (!binaries.is_empty()).then(|| WorkerLaunch {
        data_dir,
        data_dir_env,
        data_dir_arg,
        args: vec![],
        env: crate::journeys::receipt::isolation_env(&launch_env, &world.env_marker.name)
            .into_iter()
            .collect(),
        binaries,
        token_less_refused,
    });
    problems.extend(torn.problems);
    let engine_errors = torn.errored;
    if !engine_errors.is_empty() {
        errored = true;
    }
    if failure.is_none() {
        problems.extend(engine_errors);
        if !problems.is_empty() {
            failure = Some(problems.join("; "));
        }
    } else if !engine_errors.is_empty() {
        failure = failure.map(|f| format!("{f}; {}", engine_errors.join("; ")));
    }
    // Hash every artifact the scenario produced, then bind the check to them.
    let mut names = vec![];
    let mut artifact_errors = vec![];
    for (n, path) in &ctx.artifacts {
        if names.contains(n) {
            continue;
        }
        match ws
            .preserve_artifact(path)
            .and_then(|kept| lock(env.bundle).add_artifact(n, &kept))
        {
            Ok(_) => names.push(n.clone()),
            Err(e) => artifact_errors.push(e.0),
        }
    }
    if !artifact_errors.is_empty() {
        let msg = format!(
            "required evidence could not be recorded: {}",
            artifact_errors.join("; ")
        );
        failure = Some(match failure.take() {
            Some(f) => format!("{f}; {msg}"),
            None => msg,
        });
    }
    Executed {
        failure,
        steps,
        names,
        errored,
        survivors: torn.survivors,
        engine_starts: total_starts,
        engine_starts_by_env: by_env,
        launch,
    }
}

fn file_sha(path: &std::path::Path) -> Result<String> {
    let bytes = std::fs::read(path).map_err(|e| {
        crate::util::Error(format!(
            "cannot hash {} for the evidence identity: {e}",
            path.display()
        ))
    })?;
    Ok(crate::util::sha256_hex(&bytes))
}

fn git_revision(root: &std::path::Path) -> Value {
    let git = |args: &[&str]| {
        let mut git = std::process::Command::new("git");
        git.arg("-C").arg(root).args(args);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            git.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: never flash a console from a GUI host
        }
        crate::process::output_bounded(&mut git, Duration::from_secs(30))
            .ok()
            .flatten()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    match git(&["rev-parse", "HEAD"]) {
        Some(head) => {
            json!({"head": head, "dirty": git(&["status", "--porcelain"]).map(|s| !s.is_empty())})
        }
        None => Value::Null,
    }
}

/// What this run exercises: config, scenario files, engine/UI binaries and the source revision,
/// all by digest, so a receipt cannot be reused for different bytes.
fn run_identity(
    cfg: &QaConfig,
    scenarios: &[Scenario],
    engine: Option<&std::path::Path>,
    ui: Option<&std::path::Path>,
    needs_engine: bool,
    needs_ui: bool,
    plugin: Option<&std::path::Path>,
) -> Result<Value> {
    let mut scenario_ids = vec![];
    for sc in scenarios {
        scenario_ids.push(json!({"name": sc.name, "file": sc.file.to_string_lossy(), "sha256": file_sha(&sc.file)?}));
    }
    Ok(json!({
        "config": {"path": cfg.config_path.to_string_lossy(), "sha256": file_sha(&cfg.config_path)?},
        "scenarios": scenario_ids,
        "engine": bound_binary(engine, "engine", needs_engine)?,
        "ui": bound_binary(ui, "UI", needs_ui)?,
        "plugin": bound_binary(plugin, "plugin", false)?,
        "source": git_revision(&cfg.app_root),
    }))
}

/// One launched binary of the evidence identity: its path and digest, or every executable of a
/// `.app` bundle. `needed` makes an unresolved binary an error; otherwise it is `null`.
fn bound_binary(p: Option<&std::path::Path>, what: &str, needed: bool) -> Result<Value> {
    match p {
        Some(p) => {
            let target = if p.is_dir() { None } else { Some(p) };
            match target {
                Some(t) => Ok(json!({"path": t.to_string_lossy(), "sha256": file_sha(t)?})),
                None => {
                    // A `.app` bundle: bind to every executable inside Contents/MacOS.
                    let mut exes = vec![];
                    if let Ok(rd) = std::fs::read_dir(p.join("Contents").join("MacOS")) {
                        for e in rd.flatten() {
                            exes.push(json!({"path": e.path().to_string_lossy(), "sha256": file_sha(&e.path())?}));
                        }
                    }
                    if exes.is_empty() {
                        return err(format!(
                            "{} has no executable to bind into the evidence identity",
                            p.display()
                        ));
                    }
                    Ok(json!({"path": p.to_string_lossy(), "executables": exes}))
                }
            }
        }
        None if needed => err(format!(
            "evidence identity needs the {what} binary but none resolved"
        )),
        None => Ok(Value::Null),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_swapped_plugin_binary_changes_the_bound_identity() {
        // N3: the plugin is bound by path and digest; the same path with other bytes is other evidence.
        let dir = std::env::temp_dir().join(format!("rk-plugin-id-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("plugin.bin");
        std::fs::write(&p, b"plugin v1").unwrap();
        let before = bound_binary(Some(&p), "plugin", false).unwrap();
        std::fs::write(&p, b"plugin v2").unwrap();
        let after = bound_binary(Some(&p), "plugin", false).unwrap();
        assert_ne!(before["sha256"], after["sha256"]);
        assert_eq!(before["path"], after["path"]);
        // Not configured: the identity records null, and a missing plugin file is an error.
        assert_eq!(bound_binary(None, "plugin", false).unwrap(), Value::Null);
        assert!(bound_binary(Some(&dir.join("absent.bin")), "plugin", false).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The step plugin fixture (`examples/step_plugin_fixture.rs`), which `cargo test` builds into the
    /// examples directory beside the test binary's `deps` directory.
    fn step_plugin_fixture_exe() -> PathBuf {
        let deps = std::env::current_exe().unwrap();
        let debug = deps.parent().unwrap().parent().unwrap();
        let exe = debug.join("examples").join(format!(
            "step_plugin_fixture{}",
            std::env::consts::EXE_SUFFIX
        ));
        assert!(
            exe.exists(),
            "the step plugin fixture is not built at {} (cargo test builds the examples)",
            exe.display()
        );
        exe
    }

    /// E2 (ADR-0001 item 4): a plugin round-trip through the worker's step path, with a real plugin
    /// process on every platform. `start` runs `hello`; then `setup`, one `fixture_echo` step with a
    /// held secret, a plugin error that is an ordinary failure, `teardown` and `shutdown`. A missing
    /// role binary is errored, not failed. The worker reaches these calls through `steps::setup`,
    /// `steps::dispatch` and `steps::teardown` (see `StepState::with_plugin`); this drives the same
    /// [`Plugin`] those calls use, on the real process.
    #[test]
    fn a_real_step_plugin_process_completes_hello_step_hold_teardown_and_shutdown() {
        let exe = step_plugin_fixture_exe();
        let dir = std::env::temp_dir().join(format!("rk-plugin-e2-{}", crate::util::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tracker = Tracker::new();
        let env = crate::steps::spawn::SpawnEnv {
            tracker: &tracker,
            engine: None,
            data_dir: &dir,
            log_dir: &dir,
            extra_env: &[],
            ui_binary: None,
        };
        let spec = PluginSpec::new(&exe);
        let timeout = Duration::from_secs(20);
        let mut p = crate::plugin::Plugin::start(&spec, &env, "e2").expect("hello answered");
        assert_eq!(p.name(), "step-plugin-fixture");
        assert_eq!(p.version(), "1");
        assert!(p.kinds().iter().any(|k| k == "fixture_echo"));

        let body = json!({
            "scenario": {"name": "e2", "tier": "fast", "target": "engine", "requires": [], "tokens": []},
            "data_dir": dir.to_string_lossy(),
            "log_dir": dir.to_string_lossy(),
            "vars": {},
        });
        p.setup(&env, body.as_object().cloned().unwrap(), timeout)
            .expect("setup answered");

        let params = json!({"x": 1}).as_object().cloned().unwrap();
        let vars = BTreeMap::new();
        let out = p
            .step(&env, "fixture_echo", &params, &vars, timeout)
            .expect("the step answered");
        assert_eq!(out.value, json!({"x": 1}));
        assert_eq!(
            out.hold,
            vec![(
                "fixture secret".to_string(),
                "fixture-secret-value".to_string()
            )]
        );

        // The plugin refusing a step is an ordinary failed step, never an infrastructure error.
        let refused = p
            .step(&env, "fixture_missing", &params, &vars, timeout)
            .unwrap_err();
        assert!(!steps::is_errored(&refused), "{}", refused.0);

        p.teardown(&env, &vars, timeout).expect("teardown answered");
        assert!(p.shutdown(&env).is_empty(), "shutdown is clean");

        // A missing role binary never starts: that is errored.
        let absent = PluginSpec::new(dir.join("absent-plugin.exe"));
        match crate::plugin::Plugin::start(&absent, &env, "e2") {
            Ok(_) => panic!("a missing plugin binary must not start"),
            Err(e) => assert!(steps::is_errored(&e), "{}", e.0),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_matches_substring_exact_stem_and_comma_lists() {
        let f = Path::new("/x/scenarios/login-flow.scenario.toml");
        assert!(only_matches("login-flow", "Some Name", f));
        assert!(only_matches("login", "Some Name", f));
        assert!(only_matches("Some", "Some Name", f));
        assert!(only_matches("nope, login-flow", "Some Name", f));
        assert!(only_matches("nope,Some Name", "Some Name", f));
        assert!(!only_matches("nope,other", "Some Name", f));
        assert!(!only_matches(" , ", "Some Name", f));
        // Stem equality, even when the stem is not a substring of anything else matched.
        let g = Path::new("/x/a/b.scenario.toml");
        assert!(only_matches("b", "zzz", g));
    }

    #[test]
    fn an_explicit_ui_binary_skips_the_resolver_checks() {
        // Not named after the app, not a file, not a QA build: the caller named it, so it stands.
        let p = std::env::temp_dir()
            .join("rkqa-explicit")
            .join("release")
            .join("anything.bin");
        assert_eq!(explicit_ui_binary(&p).unwrap(), p);
        assert!(
            crate::binary::validate(&p, "app").is_err(),
            "the resolver path would have refused it"
        );
    }

    #[cfg(any(target_os = "macos", windows))]
    #[test]
    fn an_explicit_ui_binary_in_an_installed_location_is_still_refused() {
        if crate::installed::installed_mode() {
            return;
        }
        #[cfg(target_os = "macos")]
        let p = PathBuf::from("/Applications/CutRight.app/Contents/MacOS/CutRight");
        #[cfg(windows)]
        let Some(p) = std::env::var_os("ProgramFiles")
            .map(|d| PathBuf::from(d).join("CutRight").join("cutright.exe"))
        else {
            return;
        };
        let e = explicit_ui_binary(&p).unwrap_err();
        assert!(e.0.contains("RIGHT_RELEASE_QA_STAGE=installed"), "{e}");
    }
}

#[cfg(test)]
mod pool_tests {
    use super::*;
    use crate::scenario::SkipKind;

    fn tmp(tag: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("rkqa-suitepool-{tag}-{}", crate::util::new_id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// An app with the given scenario files and `extra` appended to its config (`[host]` and so on).
    fn app(dir: &Path, extra: &str, scenarios: &[(&str, String)]) -> QaConfig {
        let sdir = dir.join("scenarios");
        std::fs::create_dir_all(&sdir).unwrap();
        for (n, body) in scenarios {
            std::fs::write(sdir.join(format!("{n}.scenario.toml")), body).unwrap();
        }
        let path = dir.join("rightkit-qa.toml");
        std::fs::write(
            &path,
            format!("app = \"pooltest\"\nscenarios = [\"scenarios\"]\n{extra}\n"),
        )
        .unwrap();
        QaConfig::load(&path).unwrap()
    }

    /// A scenario that needs no binary: it writes a marker file naming itself into its data dir.
    fn noop(name: &str, header: &str) -> (String, String) {
        (
            name.to_string(),
            format!(
                "name = \"{name}\"\ntarget = \"browser\"\n{header}\n[[step]]\nkind = \"write_file\"\npath = \"${{data_dir}}/marker.txt\"\ncontent = \"{name}\"\n"
            ),
        )
    }

    fn files(list: &[(String, String)]) -> Vec<(&str, String)> {
        list.iter().map(|(n, b)| (n.as_str(), b.clone())).collect()
    }

    fn outcome(status: CheckStatus, skip: Option<SkipKind>) -> Outcome {
        Outcome {
            name: "x".into(),
            file: PathBuf::from("x.scenario.toml"),
            tier: "fast".into(),
            status,
            reason: None,
            ms: 0,
            steps: vec![],
            skip,
            tokens: vec![],
            world: None,
            survivors: vec![],
            engine_starts: 0,
        }
    }

    #[test]
    fn typed_skip_counting_matches_the_qualifying_rule() {
        // (outcome, fails an ordinary run, fails a qualifying run)
        for (o, ordinary, qualifying) in [
            (outcome(CheckStatus::Passed, None), false, false),
            (
                outcome(CheckStatus::Skipped, Some(SkipKind::OtherPlatform)),
                false,
                false,
            ),
            (
                outcome(CheckStatus::Skipped, Some(SkipKind::Optional)),
                false,
                true,
            ),
            (
                outcome(CheckStatus::Skipped, Some(SkipKind::UnmetRequirement)),
                true,
                true,
            ),
            (
                outcome(CheckStatus::Skipped, Some(SkipKind::TierNotSelected)),
                true,
                true,
            ),
            // A skip with no kind cannot be justified, so it fails either way.
            (outcome(CheckStatus::Skipped, None), true, true),
            (outcome(CheckStatus::Failed, None), true, true),
            (outcome(CheckStatus::Errored, None), true, true),
            (outcome(CheckStatus::NotRun, None), true, true),
        ] {
            assert_eq!(
                o.counts_as_failure(false),
                ordinary,
                "{:?}/{:?} ordinary",
                o.status,
                o.skip
            );
            assert_eq!(
                o.counts_as_failure(true),
                qualifying,
                "{:?}/{:?} qualifying",
                o.status,
                o.skip
            );
        }
        // The wire name of a never-run outcome is the receipt's, not the variant's.
        assert_eq!(
            outcome(CheckStatus::NotRun, None).to_value()["status"],
            "missing"
        );
        assert_eq!(
            outcome(CheckStatus::Skipped, Some(SkipKind::Optional)).to_value()["skipKind"],
            "optional"
        );
    }

    #[test]
    fn eight_no_op_scenarios_run_in_distinct_worlds_and_leave_distinct_state() {
        let d = tmp("eight");
        let list: Vec<_> = (0..8).map(|i| noop(&format!("s-{i}"), "")).collect();
        let cfg = app(&d, "[host]\nworkers = 4", &files(&list));
        let opts = SuiteOptions {
            keep_data: true,
            ..Default::default()
        };
        let r = run_suite_in_managed_root(&cfg, &opts, &d.join("managed")).unwrap();
        assert_eq!(r.exit_code, 0, "{:?}", r.outcomes);
        assert_eq!(r.outcomes.len(), 8);

        let worlds: Vec<PathBuf> = r
            .outcomes
            .iter()
            .map(|o| o.world.clone().unwrap())
            .collect();
        let unique: std::collections::BTreeSet<_> = worlds.iter().collect();
        assert_eq!(unique.len(), 8, "scenarios shared a world: {worlds:?}");
        for (o, w) in r.outcomes.iter().zip(&worlds) {
            assert_eq!(o.status, CheckStatus::Passed, "{o:?}");
            // The scenario's data dir is inside its own world, and holds only its own marker.
            let data = w.join("home").join("data");
            let mut markers = vec![];
            for e in std::fs::read_dir(&data).unwrap().flatten() {
                let m = e.path().join("marker.txt");
                if m.is_file() {
                    markers.push(std::fs::read_to_string(m).unwrap());
                }
            }
            assert_eq!(
                markers,
                vec![o.name.clone()],
                "{} kept another scenario's state",
                o.name
            );
        }
        // Evidence lists the checks in selection order whatever order the workers finished in.
        let evidence: Value =
            serde_json::from_slice(&std::fs::read(&r.evidence_path).unwrap()).unwrap();
        let names: Vec<String> = evidence["checks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["name"].as_str().unwrap().to_string())
            .collect();
        let expected: Vec<String> = (0..8).map(|i| format!("s-{i}")).collect();
        assert_eq!(names, expected);
        if let Some(home) = worlds[0].parent().and_then(Path::parent) {
            let _ = std::fs::remove_dir_all(home);
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn two_focus_scenarios_never_overlap_while_others_run_alongside() {
        let d = tmp("focus");
        let list = vec![
            noop("focus-a", "tokens = [\"focus\"]"),
            noop("plain-a", ""),
            noop("focus-b", "tokens = [\"focus\"]"),
            noop("plain-b", ""),
        ];
        let cfg = app(&d, "[host]\nworkers = 4", &files(&list));
        // The setup hook runs first in a scenario and the teardown hook last, so they bracket it.
        let marks: Arc<Mutex<Vec<(String, bool, Instant)>>> = Arc::default();
        let hooks = SuiteHooks::default()
            .setup({
                let m = marks.clone();
                move |_ctx, sc| {
                    m.lock()
                        .unwrap()
                        .push((sc.name.clone(), true, Instant::now()));
                    std::thread::sleep(Duration::from_millis(200));
                    Ok(())
                }
            })
            .teardown({
                let m = marks.clone();
                move |_ctx, sc| {
                    m.lock()
                        .unwrap()
                        .push((sc.name.clone(), false, Instant::now()));
                    Ok(())
                }
            });
        let opts = SuiteOptions {
            hooks,
            ..Default::default()
        };
        let r = run_suite_in_managed_root(&cfg, &opts, &d.join("managed")).unwrap();
        assert_eq!(r.exit_code, 0, "{:?}", r.outcomes);
        let marks = marks.lock().unwrap().clone();
        let at = |name: &str, start: bool| {
            marks
                .iter()
                .find(|(n, s, _)| n == name && *s == start)
                .map(|(_, _, t)| *t)
                .unwrap()
        };
        let (a0, a1) = (at("focus-a", true), at("focus-a", false));
        let (b0, b1) = (at("focus-b", true), at("focus-b", false));
        assert!(a1 <= b0 || b1 <= a0, "the two focus scenarios overlapped");
        // The in-process path is serial (one worker under `native.lock`), so the ungrouped scenario
        // cannot overlap a focus scenario here either. The token rule that lets it run alongside is
        // checked on the pool itself (`pool::tests`, the token rule), where a worker runs the scenario.
        let (p0, p1) = (at("plain-a", true), at("plain-a", false));
        for (x0, x1) in [(a0, a1), (b0, b1)] {
            assert!(p1 <= x0 || x1 <= p0, "in-process scenarios overlapped");
        }
        assert_eq!(
            r.outcomes
                .iter()
                .find(|o| o.name == "focus-a")
                .unwrap()
                .tokens,
            vec![Token::Focus]
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Hooks that record when each scenario starts (setup) and ends (teardown), and hold each one
    /// open for 200 ms, so two scenarios running at once would show as overlapping windows.
    type Marks = Arc<Mutex<Vec<(String, bool, Instant)>>>;

    fn timed_hooks(marks: &Marks) -> SuiteHooks {
        SuiteHooks::default()
            .setup({
                let m = marks.clone();
                move |_ctx, sc| {
                    m.lock()
                        .unwrap()
                        .push((sc.name.clone(), true, Instant::now()));
                    std::thread::sleep(Duration::from_millis(200));
                    Ok(())
                }
            })
            .teardown({
                let m = marks.clone();
                move |_ctx, sc| {
                    m.lock()
                        .unwrap()
                        .push((sc.name.clone(), false, Instant::now()));
                    Ok(())
                }
            })
    }

    /// Start and end events in time order must alternate (start, end, start, end, ...): a second
    /// start before the first end is an overlap.
    fn assert_serial(marks: &[(String, bool, Instant)]) {
        let mut events: Vec<(Instant, bool)> = marks.iter().map(|(_, s, t)| (*t, *s)).collect();
        events.sort_by_key(|(t, _)| *t);
        let flags: Vec<bool> = events.iter().map(|(_, s)| *s).collect();
        let alternating: Vec<bool> = (0..flags.len()).map(|i| i % 2 == 0).collect();
        assert_eq!(flags, alternating, "in-process scenario windows overlapped");
    }

    #[test]
    fn in_process_scenarios_never_overlap_even_when_workers_are_configured() {
        // ADR-0001 item 3: `[host] workers = 4` cannot widen the in-process path past one worker.
        let d = tmp("serial");
        let list = vec![noop("one", ""), noop("two", "")];
        let cfg = app(&d, "[host]\nworkers = 4", &files(&list));
        let marks: Arc<Mutex<Vec<(String, bool, Instant)>>> = Arc::default();
        let opts = SuiteOptions {
            hooks: timed_hooks(&marks),
            ..Default::default()
        };
        let r = run_suite_in_managed_root(&cfg, &opts, &d.join("managed")).unwrap();
        assert_eq!(r.exit_code, 0, "{:?}", r.outcomes);
        let marks = marks.lock().unwrap().clone();
        assert_eq!(marks.len(), 4, "each scenario starts and ends once");
        assert_serial(&marks);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn two_in_process_runs_of_one_app_never_overlap() {
        // ADR-0001 item 3: the app-wide `native.lock` makes the second run wait for the first.
        let d = tmp("tworuns");
        let list = vec![noop("solo", "")];
        let _ = app(&d, "", &files(&list));
        let managed = d.join("managed");
        let marks: Arc<Mutex<Vec<(String, bool, Instant)>>> = Arc::default();
        std::thread::scope(|s| {
            for _ in 0..2 {
                let (d, managed, marks) = (d.clone(), managed.clone(), marks.clone());
                s.spawn(move || {
                    let cfg = QaConfig::load(&d.join("rightkit-qa.toml")).unwrap();
                    let opts = SuiteOptions {
                        hooks: timed_hooks(&marks),
                        ..Default::default()
                    };
                    let r = run_suite_in_managed_root(&cfg, &opts, &managed).unwrap();
                    assert_eq!(r.exit_code, 0, "{:?}", r.outcomes);
                });
            }
        });
        let marks = marks.lock().unwrap().clone();
        assert_eq!(marks.len(), 4, "two runs, one scenario each, start and end");
        assert_serial(&marks);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn an_unknown_or_repeated_token_in_a_scenario_file_refuses_the_run() {
        for header in ["tokens = [\"fcus\"]", "tokens = [\"focus\", \"focus\"]"] {
            let d = tmp("badtoken");
            let list = vec![noop("a", header)];
            let cfg = app(&d, "", &files(&list));
            let e = run_suite_in_managed_root(&cfg, &SuiteOptions::default(), &d.join("managed"))
                .unwrap_err();
            assert!(e.0.contains("token"), "{header}: {e}");
            let _ = std::fs::remove_dir_all(&d);
        }
    }

    #[test]
    fn other_platform_skips_never_fail_but_optional_skips_fail_a_qualifying_run() {
        let other = if cfg!(windows) { "darwin" } else { "windows" };
        let d = tmp("skips");
        let list = vec![
            noop("runs", ""),
            noop("elsewhere", &format!("requires = [\"platform:{other}\"]")),
            noop(
                "optional",
                "optional = true\nrequires = [\"env:RKQA_SUITE_DEFINITELY_UNSET\"]",
            ),
        ];
        let cfg = app(&d, "", &files(&list));
        let managed = d.join("managed");
        let kind_of =
            |r: &SuiteReport, n: &str| r.outcomes.iter().find(|o| o.name == n).unwrap().skip;

        let ordinary = run_suite_in_managed_root(&cfg, &SuiteOptions::default(), &managed).unwrap();
        assert_eq!(ordinary.exit_code, 0, "{:?}", ordinary.outcomes);
        assert_eq!(
            kind_of(&ordinary, "elsewhere"),
            Some(SkipKind::OtherPlatform)
        );
        assert_eq!(kind_of(&ordinary, "optional"), Some(SkipKind::Optional));
        assert_eq!(kind_of(&ordinary, "runs"), None);

        // The judging rule, in this process: a unit-test binary cannot be a worker (the qualifying
        // entry points start `<this exe> worker <dir>`; see `a_qualifying_run_refuses_in_process_hooks`).
        let judged = || RunPolicy {
            qualifying: true,
            worker_exe: None,
        };
        let q = run_suite_at(&cfg, &SuiteOptions::default(), Some(&managed), judged()).unwrap();
        assert_eq!(
            q.exit_code, 1,
            "an optional skip must fail a qualifying run"
        );

        // Without the optional scenario the qualifying run passes: another OS's scenario is the only
        // skip a qualifying run tolerates.
        let only_other = tmp("skips-other");
        let list = vec![
            noop("runs", ""),
            noop("elsewhere", &format!("requires = [\"platform:{other}\"]")),
        ];
        let cfg = app(&only_other, "", &files(&list));
        let q = run_suite_at(
            &cfg,
            &SuiteOptions::default(),
            Some(&only_other.join("managed")),
            judged(),
        )
        .unwrap();
        assert_eq!(q.exit_code, 0, "{:?}", q.outcomes);
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&only_other);
    }

    #[test]
    fn a_qualifying_run_refuses_in_process_hooks() {
        let d = tmp("qhooks");
        let list = vec![noop("a", "")];
        let cfg = app(&d, "", &files(&list));
        let opts = SuiteOptions {
            hooks: SuiteHooks::default().setup(|_, _| Ok(())),
            ..Default::default()
        };
        let e = run_suite_qualifying_in_managed_root(&cfg, &opts, &d.join("managed")).unwrap_err();
        assert!(e.0.contains("cannot carry in-process SuiteHooks"), "{e}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_panicking_hook_is_errored_not_passed_and_costs_the_others_nothing() {
        let d = tmp("panic");
        let list = vec![noop("fine", ""), noop("boom", ""), noop("also-fine", "")];
        let cfg = app(&d, "[host]\nworkers = 3", &files(&list));
        let hooks = SuiteHooks::default().setup(|_ctx, sc| {
            if sc.name == "boom" {
                panic!("hook exploded");
            }
            Ok(())
        });
        let opts = SuiteOptions {
            hooks,
            ..Default::default()
        };
        let r = run_suite_in_managed_root(&cfg, &opts, &d.join("managed")).unwrap();
        assert_eq!(r.exit_code, 1);
        let status = |n: &str| r.outcomes.iter().find(|o| o.name == n).unwrap().status;
        assert_eq!(status("fine"), CheckStatus::Passed);
        assert_eq!(status("also-fine"), CheckStatus::Passed);
        assert_eq!(status("boom"), CheckStatus::Errored);
        let boom = r.outcomes.iter().find(|o| o.name == "boom").unwrap();
        assert!(
            boom.reason.as_deref().unwrap().contains("hook exploded"),
            "{boom:?}"
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}
