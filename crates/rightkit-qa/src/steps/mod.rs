//! Built-in steps and the step-plugin glue (ADR-0001 item 4).
//!
//! Generic steps are rightkit-qa built-ins, so every app gets them and none re-implements them:
//!
//! | kind | file | what it does |
//! |---|---|---|
//! | `http` | `http.rs` | one plain-HTTP request to a daemon's public listener (loopback) |
//! | `engine_log` | `engine_log.rs` | wait for an event in a JSON-lines log, forbid text in it |
//! | `start_daemon`, `stop_daemon` | `daemon.rs` | run the engine as a long-lived listener, tracked, stopped bounded |
//! | `mock_admin` | `mock_admin.rs` | one request to an `/__admin/` path of the scenario's mock |
//! | `secret_absent` | `secret_absent.rs` | prove a secret reached no result, no file and no `cli` step log |
//!
//! Everything app-specific is a step kind of the app's step plugin ([`crate::plugin`]). Setup and
//! teardown run through the plugin too ([`setup`], [`teardown`]).
//!
//! # Wiring (the executor hook)
//!
//! Each built-in has the shape of an app step handler: `fn(&mut Ctx, &Map<String, Value>) ->
//! Result<Value>`, where the map is the step's parameters with `${...}` already resolved by
//! `run_step` (a [`crate::scenario::Step`] holds them unresolved). In `scenario.rs` `exec`, replace
//! the last arm
//!
//! ```text
//! other => err(format!("unknown step kind '{other}'")),
//! ```
//!
//! with `other => crate::steps::dispatch(ctx, other, p),`. Core kinds and an app's registered
//! `ctx.handlers` are tried first, then the built-ins, then the plugin's kinds; [`dispatch`] keeps the
//! old `unknown step kind` error for the rest. `Ctx` needs one field, `pub steps:
//! crate::steps::StepState` (starts `StepState::default()`; the suite sets the plugin with
//! [`StepState::with_plugin`] from the manifest). The suite runs [`setup`] before the first step and
//! [`teardown`] after the last (in `execute`, before `ctx.teardown()`), and the `cli` arm calls
//! [`StepState::note_cli_log`] with each step log it writes, so `secret_absent` can prove it saw
//! every one.
use crate::plugin::{Plugin, PluginSpec, DEFAULT_CALL_TIMEOUT};
use crate::scenario::{pv, Ctx, Scenario};
use crate::steps::spawn::SpawnEnv;
use crate::util::{err, Error, Result};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

mod daemon;
mod engine_log;
mod http;
mod mock_admin;
mod secret_absent;
pub mod spawn;

pub use crate::plugin::is_errored;

/// The step kinds the runner implements itself. A plugin may not declare any of them.
pub const BUILTIN_KINDS: [&str; 6] = [
    "http",
    "engine_log",
    "start_daemon",
    "stop_daemon",
    "mock_admin",
    "secret_absent",
];

pub fn is_builtin(kind: &str) -> bool {
    BUILTIN_KINDS.contains(&kind)
}

/// What the built-in steps and the plugin hold for one scenario. Lives in `Ctx::steps`.
#[derive(Default)]
pub struct StepState {
    /// The app's plugin, set by the suite from the manifest. `None`: the app has no plugin.
    pub plugin_spec: Option<PluginSpec>,
    plugin: Option<Plugin>,
    daemons: BTreeMap<String, daemon::Running>,
    /// Secrets the scenario has seen or made (label, value); `engine_log` can prove none reaches
    /// the log. Values are never printed.
    held: Vec<(String, String)>,
    /// Step logs the `cli` steps wrote, each holding that call's whole stdout and stderr.
    cli_logs: Vec<PathBuf>,
}

impl StepState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_plugin(spec: PluginSpec) -> Self {
        Self {
            plugin_spec: Some(spec),
            ..Self::default()
        }
    }

    /// Install a plugin that is already running (a host that embeds one, or a fixture); the suite
    /// normally starts it from [`StepState::plugin_spec`] in [`setup`].
    pub fn attach_plugin(&mut self, plugin: Plugin) {
        self.plugin = Some(plugin);
    }

    /// Remember a secret so `engine_log` (`forbid_held`) can prove it never reached the log.
    pub fn hold(&mut self, label: &str, value: &str) {
        if !value.is_empty() && !self.held.iter().any(|(_, v)| v == value) {
            self.held.push((label.to_string(), value.to_string()));
        }
    }

    pub fn held(&self) -> &[(String, String)] {
        &self.held
    }

    /// Record the log of one `cli` step (served by the sidecar or cold). `secret_absent` requires
    /// every recorded log to exist and be whole, so a call that left no log cannot hide a leak.
    pub fn note_cli_log(&mut self, path: PathBuf) {
        if !self.cli_logs.contains(&path) {
            self.cli_logs.push(path);
        }
    }

    pub fn cli_logs(&self) -> &[PathBuf] {
        &self.cli_logs
    }

    /// The kinds the running plugin declared (empty without a plugin).
    pub fn plugin_kinds(&self) -> Vec<String> {
        self.plugin.as_ref().map(Plugin::kinds).unwrap_or_default()
    }
}

// ------------------------------------------------------------------------------- dispatch

/// Run one step kind the core executor does not know: a built-in, then the plugin's kinds. An
/// error from a plugin crash or protocol violation satisfies [`is_errored`].
pub fn dispatch(ctx: &mut Ctx, kind: &str, p: &Map<String, Value>) -> Result<Value> {
    match kind {
        "http" => http::run(ctx, p),
        "engine_log" => engine_log::run(ctx, p),
        "start_daemon" => daemon::start(ctx, p),
        "stop_daemon" => daemon::stop(ctx, p),
        "mock_admin" => mock_admin::run(ctx, p),
        "secret_absent" => secret_absent::run(ctx, p),
        other => plugin_step(ctx, other, p),
    }
}

/// The wall-clock limit of one plugin request: the step's `timeout_s`, else two minutes.
fn call_timeout(p: &Map<String, Value>) -> Duration {
    pv(p, "timeout_s")
        .and_then(Value::as_f64)
        .filter(|s| s.is_finite() && *s > 0.0)
        .map(|s| Duration::from_secs_f64(s.min(3600.0)))
        .unwrap_or(DEFAULT_CALL_TIMEOUT)
}

/// The environment every process of the plugin starts with: the world workspace's isolation
/// (HOME, TMP/TEMP, APPDATA/LOCALAPPDATA, XDG dirs, the app's data-dir variable: the same set the UI
/// launch gets), then the world's own variables (marker, ports, tokens). A plugin and the children
/// it spawns never run on the real user profile.
fn world_env(ctx: &Ctx) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = ctx
        .ws
        .env
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if let Some(world) = ctx.world.as_ref() {
        for (k, v) in crate::pool::world_env(world, &ctx.granted) {
            env.retain(|(have, _)| *have != k);
            env.push((k, v));
        }
    }
    env
}

/// Call `f` with the running plugin and the scenario's spawn environment. The plugin and the
/// environment borrow different fields of `ctx`, so `f` can use both.
fn with_plugin<R>(ctx: &mut Ctx, f: impl FnOnce(&mut Plugin, &SpawnEnv) -> R) -> Result<R> {
    let world = world_env(ctx);
    let (out, launches) = {
        let env = SpawnEnv {
            tracker: ctx.tracker,
            engine: ctx.engine.as_ref(),
            data_dir: &ctx.data_dir,
            log_dir: &ctx.log_dir,
            extra_env: &world,
            ui_binary: ctx.ui_binary.as_deref(),
        };
        let plugin = ctx
            .steps
            .plugin
            .as_mut()
            .ok_or_else(|| Error("the step plugin is not running".into()))?;
        let out = f(plugin, &env);
        (out, plugin.take_engine_launches())
    };
    let step = Some(ctx.step_no()).filter(|n| *n > 0);
    // Every engine process the plugin started with `spawn` `engine: true` during that call counts
    // as an (undeclared) engine start of the scenario.
    for note in launches {
        match note {
            spawn::EngineNote::Started { what, cold_reason } => ctx
                .engine_session
                .note_spawned_engine(&what, cold_reason, step),
            spawn::EngineNote::Refused(why) => ctx.engine_session.note_engine_refusal(&why),
        }
    }
    Ok(out)
}

/// Register what a plugin reply asked to hold (`hold`), so `engine_log` `forbid_held` checks it.
fn hold_reply(ctx: &mut Ctx, reply: &crate::plugin::Reply) {
    for (label, secret) in &reply.hold {
        ctx.steps.hold(label, secret);
    }
}

fn merge_vars(ctx: &mut Ctx, vars: BTreeMap<String, Value>) {
    for (name, value) in vars {
        ctx.vars.insert(name, value);
    }
}

fn plugin_step(ctx: &mut Ctx, kind: &str, p: &Map<String, Value>) -> Result<Value> {
    let handled = ctx.steps.plugin.as_ref().map(|pl| pl.handles(kind));
    match handled {
        Some(true) => {}
        Some(false) => {
            let kinds = ctx.steps.plugin_kinds().join(", ");
            return err(format!(
                "unknown step kind '{kind}' (the step plugin implements: {kinds})"
            ));
        }
        None if ctx.steps.plugin_spec.is_some() => {
            return err(format!(
                "unknown step kind '{kind}': the step plugin is declared but was not started (run `steps::setup` before the first step)"
            ));
        }
        None => return err(format!("unknown step kind '{kind}'")),
    }
    let vars = ctx.vars.clone();
    let timeout = call_timeout(p);
    let reply = with_plugin(ctx, |plugin, env| plugin.step(env, kind, p, &vars, timeout))??;
    hold_reply(ctx, &reply);
    merge_vars(ctx, reply.set_vars);
    Ok(reply.value)
}

// ------------------------------------------------------------------------- setup / teardown

/// Per-scenario setup: start the app's step plugin, once, through the scenario's tracker, and run its
/// `setup` (the synthetic project, scratch variables, the app's mock: whatever the app prepares,
/// asking the runner to start any process). A scenario of an app without a plugin does nothing.
/// Run this where the suite runs the app's setup hook; teardown still runs after an `Err`.
pub fn setup(ctx: &mut Ctx, sc: &Scenario) -> Result<()> {
    let Some(spec) = ctx.steps.plugin_spec.clone() else {
        return Ok(());
    };
    if ctx.steps.plugin.is_some() {
        return err("the step plugin is already running for this scenario");
    }
    let world = world_env(ctx);
    let started = {
        let env = SpawnEnv {
            tracker: ctx.tracker,
            engine: ctx.engine.as_ref(),
            data_dir: &ctx.data_dir,
            log_dir: &ctx.log_dir,
            extra_env: &world,
            ui_binary: ctx.ui_binary.as_deref(),
        };
        Plugin::start(&spec, &env, &sc.name)?
    };
    ctx.steps.plugin = Some(started);
    let mut body = Map::new();
    body.insert(
        "scenario".into(),
        json!({
            "name": sc.name,
            "tier": sc.tier,
            "target": sc.target,
            "requires": sc.requires,
            "tokens": sc.tokens,
        }),
    );
    body.insert("data_dir".into(), json!(ctx.data_dir.to_string_lossy()));
    body.insert("log_dir".into(), json!(ctx.log_dir.to_string_lossy()));
    body.insert("vars".into(), json!(ctx.vars));
    let reply = with_plugin(ctx, |plugin, env| {
        plugin.setup(env, body, DEFAULT_CALL_TIMEOUT)
    })??;
    hold_reply(ctx, &reply);
    merge_vars(ctx, reply.set_vars);
    Ok(())
}

/// Per-scenario teardown, in this order: the plugin's `teardown` (the app's own cleanup, while the
/// daemons still run), every daemon still running (stdin closed, waited, killed if it will not exit),
/// then the plugin itself (`shutdown`, bounded, killed if it will not exit) and every child it left
/// running. Each problem found is reported; a scenario that fails any of them fails.
pub fn teardown(ctx: &mut Ctx, _sc: &Scenario) -> Result<()> {
    let mut problems: Vec<String> = Vec::new();
    let alive = ctx
        .steps
        .plugin
        .as_ref()
        .is_some_and(|p| p.dead_reason().is_none());
    if alive {
        let vars = ctx.vars.clone();
        match with_plugin(ctx, |plugin, env| {
            plugin.teardown(env, &vars, DEFAULT_CALL_TIMEOUT)
        }) {
            Ok(Ok(_)) => {}
            Ok(Err(e)) | Err(e) => problems.push(format!("step plugin teardown: {}", e.0)),
        }
    }
    problems.extend(daemon::stop_all(ctx));
    if ctx.steps.plugin.is_some() {
        match with_plugin(ctx, |plugin, env| plugin.shutdown(env)) {
            Ok(found) => problems.extend(found),
            Err(e) => problems.push(e.0),
        }
        ctx.steps.plugin = None;
    }
    if problems.is_empty() {
        Ok(())
    } else {
        err(problems.join("; "))
    }
}

// ----------------------------------------------------------------------------- parameters

/// Reject a field the step does not know, so a typo cannot silently weaken a check.
pub(crate) fn only(p: &Map<String, Value>, kind: &str, allowed: &[&str]) -> Result<()> {
    match p
        .iter()
        .find(|(k, v)| !v.is_null() && !allowed.contains(&k.as_str()))
    {
        Some((k, _)) => err(format!(
            "{kind} has no field `{k}` (fields: {})",
            allowed.join(", ")
        )),
        None => Ok(()),
    }
}

pub(crate) fn text(p: &Map<String, Value>, key: &str) -> Result<Option<String>> {
    match pv(p, key) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => err(format!("`{key}` must be a string")),
    }
}

pub(crate) fn required(p: &Map<String, Value>, kind: &str, key: &str) -> Result<String> {
    text(p, key)?.ok_or_else(|| Error(format!("{kind} needs string `{key}`")))
}

pub(crate) fn flag(p: &Map<String, Value>, key: &str) -> Result<bool> {
    match pv(p, key) {
        None => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => err(format!("`{key}` must be true or false")),
    }
}

pub(crate) fn uint(p: &Map<String, Value>, key: &str) -> Result<Option<u64>> {
    match pv(p, key) {
        None => Ok(None),
        Some(v) => v
            .as_u64()
            .map(Some)
            .ok_or_else(|| Error(format!("`{key}` must be a non-negative whole number"))),
    }
}

pub(crate) fn strings(p: &Map<String, Value>, key: &str) -> Result<Vec<String>> {
    match pv(p, key) {
        None => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| Error(format!("`{key}` must be an array of strings")))
            })
            .collect(),
        Some(_) => err(format!("`{key}` must be an array of strings")),
    }
}

/// A table whose values are all strings, in key order.
pub(crate) fn string_table(p: &Map<String, Value>, key: &str) -> Result<Vec<(String, String)>> {
    match pv(p, key) {
        None => Ok(Vec::new()),
        Some(Value::Object(map)) => map
            .iter()
            .map(|(k, v)| {
                v.as_str()
                    .map(|s| (k.clone(), s.to_string()))
                    .ok_or_else(|| Error(format!("`{key}.{k}` must be a string")))
            })
            .collect(),
        Some(_) => err(format!("`{key}` must be a table")),
    }
}

/// `s` as a path of this host: on Windows every `/` becomes `\`.
pub(crate) fn host_path(s: &str) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(s.replace('/', "\\"))
    } else {
        PathBuf::from(s)
    }
}

/// Whether `path` is `dir` or below it, with no `..` in it (a lexical check; verbatim Windows
/// prefixes are normalised first).
pub(crate) fn within(dir: &Path, path: &Path) -> bool {
    let dir = crate::util::plain_path(dir);
    let path = crate::util::plain_path(path);
    !path.components().any(|c| matches!(c, Component::ParentDir)) && path.starts_with(&dir)
}

/// A scenario path: relative to the scenario's data directory, or absolute and inside the data or
/// log directory. A step cannot read outside its scenario.
pub(crate) fn scenario_path(data_dir: &Path, log_dir: &Path, text: &str) -> Result<PathBuf> {
    let path = host_path(text);
    let full = if path.is_absolute() {
        path
    } else {
        data_dir.join(path)
    };
    if within(data_dir, &full) || within(log_dir, &full) {
        Ok(full)
    } else {
        err(format!(
            "`{text}` is outside this scenario's data and log directories"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::{serve, PluginCx, StepOutput, StepPlugin};

    /// Two kinds: `echo` sets a variable and returns its params, `explode` crashes the plugin.
    struct Echo;

    impl StepPlugin for Echo {
        fn name(&self) -> String {
            "echo-plugin".into()
        }
        fn kinds(&self) -> Vec<String> {
            vec!["echo".into(), "explode".into()]
        }
        fn step(
            &mut self,
            _cx: &mut PluginCx<'_>,
            kind: &str,
            params: &Map<String, Value>,
            vars: &Map<String, Value>,
        ) -> std::result::Result<StepOutput, String> {
            match kind {
                "echo" => Ok(
                    StepOutput::value(json!({"params": params, "saw": vars.get("seed")}))
                        .with_var("from_plugin", json!("yes"))
                        .with_hold("plugin api key", "plugin-secret-0123456789abcdef"),
                ),
                _ => panic!("the plugin crashes on purpose"),
            }
        }
    }

    fn running_echo() -> Plugin {
        let (from_runner, to_plugin) = std::io::pipe().unwrap();
        let (from_plugin, to_runner) = std::io::pipe().unwrap();
        std::thread::spawn(move || {
            let mut plugin = Echo;
            let _ = serve(&mut plugin, from_runner, to_runner);
        });
        Plugin::attach(
            Box::new(from_plugin),
            Box::new(to_plugin),
            "s",
            Duration::from_secs(20),
        )
        .unwrap()
    }

    fn with_ctx<R>(f: impl FnOnce(&mut Ctx) -> R) -> R {
        let root = std::env::temp_dir().join(format!("rkqa-steps-{}", crate::util::new_id()));
        std::fs::create_dir_all(&root).unwrap();
        let cfg =
            crate::config::QaConfig::parse("app = \"stepstest\"\n", &root.join("rightkit-qa.toml"))
                .unwrap();
        let ws = crate::workspace::create(&root.join("managed"), None, "stepstest").unwrap();
        let tracker = crate::process::Tracker::new();
        let mut ctx = Ctx::new(&cfg, None, None, &ws, &tracker, "s").unwrap();
        f(&mut ctx)
    }

    fn scenario(ctx: &Ctx) -> Scenario {
        let path = ctx.data_dir.join("x.scenario.toml");
        std::fs::write(&path, "name = \"x\"\n[[step]]\nkind = \"set\"\n").unwrap();
        Scenario::load(&path).unwrap()
    }

    fn params(v: Value) -> Map<String, Value> {
        v.as_object().cloned().unwrap()
    }

    /// N4: what the plugin and its spawned programs start with is the world workspace's isolation
    /// (profile, temp and app-data directories), not the real user profile.
    #[test]
    fn the_plugin_environment_carries_the_workspace_isolation() {
        with_ctx(|ctx| {
            let env = world_env(ctx);
            let get = |k: &str| {
                env.iter()
                    .find(|(name, _)| name == k)
                    .map(|(_, v)| v.clone())
            };
            let home = ctx.ws.home.to_string_lossy().into_owned();
            assert_eq!(get("HOME").as_deref(), Some(home.as_str()));
            assert_eq!(get("USERPROFILE").as_deref(), Some(home.as_str()));
            for key in ["TMP", "TEMP", "APPDATA", "LOCALAPPDATA", "XDG_DATA_HOME"] {
                let value = get(key).unwrap_or_else(|| panic!("{key} is not in the plugin env"));
                assert!(
                    std::path::Path::new(&value).starts_with(&ctx.ws.home),
                    "{key} = {value} is outside the world home {home}"
                );
            }
            // Every workspace variable is there, once.
            for (k, v) in &ctx.ws.env {
                assert_eq!(get(k).as_ref(), Some(v), "{k}");
            }
            let mut names: Vec<&String> = env.iter().map(|(k, _)| k).collect();
            names.sort();
            let before = names.len();
            names.dedup();
            assert_eq!(before, names.len(), "a variable is set twice");
        });
    }

    #[test]
    fn a_plugin_kind_runs_through_dispatch_with_vars_in_and_vars_out() {
        with_ctx(|ctx| {
            ctx.vars.insert("seed".into(), json!(7));
            ctx.steps.attach_plugin(running_echo());
            assert_eq!(ctx.steps.plugin_kinds(), vec!["echo", "explode"]);
            let v = dispatch(ctx, "echo", &params(json!({"a": 1}))).unwrap();
            assert_eq!(v["params"], json!({"a": 1}));
            assert_eq!(v["saw"], 7);
            assert_eq!(ctx.vars.get("from_plugin"), Some(&json!("yes")));
            // PostRight gap: a secret the plugin step registered is held for `engine_log`
            // `forbid_held`, the same set `start_daemon` `secret_files` and `hold_files` feed.
            assert!(
                ctx.steps
                    .held()
                    .iter()
                    .any(|(label, value)| label == "plugin api key"
                        && value == "plugin-secret-0123456789abcdef"),
                "{:?}",
                ctx.steps.held().iter().map(|(l, _)| l).collect::<Vec<_>>()
            );

            let e = dispatch(ctx, "nope", &params(json!({}))).unwrap_err();
            assert!(
                e.0.contains("unknown step kind 'nope'") && e.0.contains("echo, explode"),
                "{}",
                e.0
            );
            assert!(!is_errored(&e));
        });
    }

    #[test]
    fn a_plugin_crash_through_dispatch_is_errored_and_teardown_still_completes() {
        with_ctx(|ctx| {
            let sc = scenario(ctx);
            ctx.steps.attach_plugin(running_echo());
            let e = dispatch(ctx, "explode", &params(json!({}))).unwrap_err();
            assert!(is_errored(&e), "{}", e.0);
            let again = dispatch(ctx, "echo", &params(json!({}))).unwrap_err();
            assert!(is_errored(&again), "{}", again.0);
            assert!(teardown(ctx, &sc).is_ok());
            // The plugin is gone after teardown; its kinds are unknown kinds again.
            let gone = dispatch(ctx, "echo", &params(json!({}))).unwrap_err();
            assert!(gone.0.contains("unknown step kind 'echo'"), "{}", gone.0);
        });
    }

    #[test]
    fn a_teardown_shuts_a_healthy_plugin_down_cleanly() {
        with_ctx(|ctx| {
            let sc = scenario(ctx);
            ctx.steps.attach_plugin(running_echo());
            assert!(dispatch(ctx, "echo", &params(json!({}))).is_ok());
            assert!(teardown(ctx, &sc).is_ok());
            assert!(ctx.steps.plugin_kinds().is_empty());
        });
    }

    /// Oracle item 9: a declared plugin that cannot start makes the scenario's setup `errored`.
    #[test]
    fn a_declared_plugin_that_cannot_start_makes_setup_errored() {
        with_ctx(|ctx| {
            let sc = scenario(ctx);
            let missing = ctx.data_dir.join("no-such-plugin");
            ctx.steps = StepState::with_plugin(PluginSpec::new(missing));
            let e = setup(ctx, &sc).unwrap_err();
            assert!(is_errored(&e), "{}", e.0);
            assert!(ctx.steps.plugin_kinds().is_empty());
        });
    }

    #[test]
    fn without_a_plugin_an_unknown_kind_keeps_its_old_error() {
        with_ctx(|ctx| {
            let e = dispatch(ctx, "daemon_enrol", &params(json!({}))).unwrap_err();
            assert_eq!(e.0, "unknown step kind 'daemon_enrol'");
            let sc = scenario(ctx);
            assert!(setup(ctx, &sc).is_ok());
        });
    }

    #[test]
    fn the_builtins_are_reached_before_the_plugin_and_secret_absent_sees_every_cli_log() {
        with_ctx(|ctx| {
            assert!(BUILTIN_KINDS.iter().all(|k| is_builtin(k)));
            std::fs::write(ctx.data_dir.join("store"), "state").unwrap();
            let log = ctx.log_dir.join("01-cli.log");
            std::fs::write(&log, "$ doctor\n--- stdout\n{}\n--- stderr\nfine\n").unwrap();
            ctx.steps.note_cli_log(log.clone());
            let ok = dispatch(
                ctx,
                "secret_absent",
                &params(json!({"needle": "never-printed-0123456789", "min_cli_logs": 1})),
            )
            .unwrap();
            assert_eq!(ok["cli_logs_scanned"], 1);
            // A call that registered a log and left none cannot hide its output.
            ctx.steps.note_cli_log(ctx.log_dir.join("02-cli.log"));
            let e = dispatch(
                ctx,
                "secret_absent",
                &params(json!({"needle": "never-printed-0123456789"})),
            )
            .unwrap_err();
            assert!(e.0.contains("is missing"), "{}", e.0);
        });
    }

    #[test]
    fn engine_log_reads_only_inside_the_scenario() {
        with_ctx(|ctx| {
            std::fs::write(ctx.data_dir.join("engine.jsonl"), "{\"event\":\"up\"}\n").unwrap();
            let ok = dispatch(
                ctx,
                "engine_log",
                &params(json!({"path": "engine.jsonl", "event": "up", "wait_s": 0})),
            )
            .unwrap();
            assert_eq!(ok["matched"], true);
            let mut refused = vec!["../outside.jsonl", "/etc/passwd"];
            if cfg!(windows) {
                refused.push("C:\\Windows\\win.ini");
            }
            for bad in refused {
                let e = dispatch(
                    ctx,
                    "engine_log",
                    &params(json!({"path": bad, "forbid": ["x"]})),
                )
                .unwrap_err();
                assert!(e.0.contains("outside this scenario"), "{bad}: {}", e.0);
            }
        });
    }

    /// The step plugin fixture (`examples/step_plugin_fixture.rs`), built by `cargo test` into the
    /// examples directory beside the test binary's `deps` directory (the same locator as the E2 test
    /// in `suite.rs`, copied because that one is private to its module).
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

    /// C1 (ADR-0001 item 8): the journeys hand-off and the worker's step path run together with a
    /// real plugin process. The spec is parsed from the JSON journeys writes into `scenario.json`
    /// `runtime.plugin` (`journeys/run.rs`, `plugin_json`), then `setup`, a `fixture_echo` step and
    /// `teardown` run on the `StepState` the worker builds. Nothing is left running afterwards: the
    /// plugin's pid is gone and the tracker has no survivor, registered or recorded.
    #[test]
    fn the_journeys_plugin_json_runs_setup_step_and_teardown_with_no_survivor() {
        let exe = step_plugin_fixture_exe();
        // The shape journeys writes: the role's binary path, its args, env and hello timeout.
        let journeys_plugin = json!({
            "binary": exe.to_string_lossy(),
            "args": [],
            "env": [],
            "helloTimeoutMs": 10_000,
        });
        let spec = crate::suite::plugin_from_json(&journeys_plugin)
            .expect("the journeys plugin JSON parses");
        with_ctx(|ctx| {
            let sc = scenario(ctx);
            ctx.steps = StepState::with_plugin(spec);
            let setup_result = setup(ctx, &sc);
            let plugin_pids: Vec<u32> = ctx
                .tracker
                .alive()
                .into_iter()
                .filter(|(_, label)| label.as_str() == "step plugin")
                .map(|(pid, _)| pid)
                .collect();

            let echoed = dispatch(ctx, "fixture_echo", &params(json!({"x": 1})));
            let torn_down = teardown(ctx, &sc);

            if let Err(e) = &setup_result {
                panic!("setup must start the plugin: {}", e.0);
            }
            assert!(
                !plugin_pids.is_empty(),
                "setup registered no plugin pid with the tracker"
            );
            let echoed = match echoed {
                Ok(v) => v,
                Err(e) => panic!("the fixture_echo step failed: {}", e.0),
            };
            assert_eq!(echoed, json!({"x": 1}));
            assert!(
                ctx.steps
                    .held()
                    .iter()
                    .any(|(label, value)| label == "fixture secret"
                        && value == "fixture-secret-value"),
                "{:?}",
                ctx.steps.held()
            );
            if let Err(e) = &torn_down {
                panic!("teardown must complete cleanly: {}", e.0);
            }

            for pid in &plugin_pids {
                assert!(
                    !crate::process::is_alive(*pid),
                    "plugin pid {pid} is still alive"
                );
            }
            assert!(
                ctx.tracker.alive().is_empty(),
                "survivors: {:?}",
                ctx.tracker.alive()
            );
            assert!(
                ctx.tracker.recorded().is_empty(),
                "recorded: {:?}",
                ctx.tracker.recorded()
            );
        });
    }
}
