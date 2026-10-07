//! Declarative scenarios (`*.scenario.toml`) and the step executor.
//!
//! A scenario is a tier, optional requirements, and an ordered list of steps.
//! Steps drive the engine (CLI and framed sidecar), the real app through
//! `rightkit-control`, paid-provider mocks, media validators, and (feature
//! `browser`) Chrome. Each step may `save` its JSON result under a name for
//! later `${name.path}` interpolation and carries `[[step.expect]]` assertions.
use crate::check::{check_all, interpolate, Expect};
use crate::config::QaConfig;
use crate::control::{self, Control, LaunchSpec, Mode};
use crate::engine::{CliOptions, EngineTarget, Sidecar};
use crate::mock::{MockServer, Route};
use crate::page;
use crate::process::Tracker;
use crate::util::{err, sleep_ms, Error, Result};
use crate::workspace::QaWorkspace;
use crate::{media, util};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Deserialize)]
pub struct Scenario {
    pub name: String,
    #[serde(default = "fast")]
    pub tier: String,
    /// `engine`, `ui`, or `browser`: which binary the scenario needs.
    #[serde(default = "engine")]
    pub target: String,
    /// `env:NAME`, `file:path`, `cmd:program`, `platform:darwin|windows`.
    #[serde(default)]
    pub requires: Vec<String>,
    /// A scenario that may legitimately be skipped when `requires` is unmet. Every other
    /// scenario is a mandatory gate: skipping it fails the run.
    #[serde(default)]
    pub optional: bool,
    pub timeout_s: Option<u64>,
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
        Ok(s)
    }
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
/// or a path), `platform:darwin|windows`.
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
            "platform" => (crate::util::platform().ok() != Some(value))
                .then(|| format!("platform is not {value}")),
            other => Some(format!("unknown requirement kind '{other}'")),
        };
        if missing.is_some() {
            return missing;
        }
    }
    None
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
    pub mocks: BTreeMap<String, MockServer>,
    pub ui: Option<(Control, Mode)>,
    #[cfg(feature = "browser")]
    pub browser: Option<crate::browser::BrowserTarget>,
    /// Files this scenario produced; the runner hashes them into the evidence bundle.
    pub artifacts: Vec<(String, PathBuf)>,
    step_no: usize,
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
        let data_dir = ws.scenario_dir(scenario)?;
        let log_dir = ws
            .evidence_dir
            .join(data_dir.file_name().unwrap_or_default());
        std::fs::create_dir_all(&log_dir)?;
        let mut vars = BTreeMap::new();
        vars.insert("data_dir".into(), json!(data_dir.to_string_lossy()));
        vars.insert("evidence_dir".into(), json!(log_dir.to_string_lossy()));
        vars.insert("app_root".into(), json!(cfg.app_root.to_string_lossy()));
        vars.insert(
            "engine".into(),
            json!(engine
                .as_ref()
                .map(|e| e.binary.to_string_lossy().to_string())),
        );
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
            mocks: BTreeMap::new(),
            ui: None,
            #[cfg(feature = "browser")]
            browser: None,
            artifacts: vec![],
            step_no: 0,
        })
    }

    fn engine(&self) -> Result<&EngineTarget> {
        self.engine.as_ref().ok_or_else(|| Error("no engine binary configured (set [engine].binary, RIGHTKIT_QA_ENGINE, or pass --build)".into()))
    }

    fn ui(&self) -> Result<&Control> {
        self.ui
            .as_ref()
            .map(|(c, _)| c)
            .ok_or_else(|| Error("no UI app launched; add a ui_launch step first".into()))
    }

    fn artifact(&mut self, name: &str, path: PathBuf) {
        self.artifacts
            .push((format!("{}/{}", self.scenario, name), path));
    }

    /// Stop everything this scenario started, in reverse dependency order.
    /// Returns problems found while cleaning up (a failed stop is a failed scenario).
    pub fn teardown(&mut self) -> Vec<String> {
        let mut problems = vec![];
        #[cfg(feature = "browser")]
        if let Some(mut b) = self.browser.take() {
            b.stop();
        }
        if let Some((mut c, mode)) = self.ui.take() {
            match c.stop() {
                Ok(r) => {
                    if !r.process_gone || !r.endpoint_closed {
                        problems.push(format!(
                            "UI app pid {} not fully stopped (process_gone={}, endpoint_closed={})",
                            r.pid, r.process_gone, r.endpoint_closed
                        ));
                    }
                    if mode != Mode::Visible && !r.owned_never_frontmost {
                        problems.push(format!(
                            "owned UI process reached foreground: {:?}",
                            r.owned_frontmost_pids
                        ));
                    }
                }
                Err(e) => problems.push(e.0),
            }
        }
        for (_, mut s) in std::mem::take(&mut self.sidecars) {
            s.close();
        }
        self.mocks.clear();
        if let Err(e) = self.tracker.assert_no_orphans() {
            problems.push(e.0);
        }
        problems
    }
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
fn resolve(ctx: &Ctx, s: &str) -> PathBuf {
    let p = Path::new(s);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        ctx.data_dir.join(p)
    }
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

/// Run one step including retry-until-expectations-hold.
pub fn run_step(ctx: &mut Ctx, step: &Step) -> Result<Value> {
    ctx.step_no += 1;
    let started = Instant::now();
    let until = step.until_timeout_s.map(Duration::from_secs);
    let interval = Duration::from_millis(step.interval_ms.unwrap_or(500));
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

fn exec(ctx: &mut Ctx, kind: &str, p: &Map<String, Value>) -> Result<Value> {
    match kind {
        // ------------------------------------------------------------ engine
        "cli" => {
            let args: Vec<String> = match (pv(p, "args"), ps_opt(p, "command")) {
                (Some(Value::Array(a)), _) => a
                    .iter()
                    .map(|v| {
                        v.as_str()
                            .map(String::from)
                            .unwrap_or_else(|| v.to_string())
                    })
                    .collect(),
                (_, Some(c)) => c.split_whitespace().map(String::from).collect(),
                _ => return err("cli step needs 'command' or 'args'"),
            };
            let r = ctx.engine()?.cli(&args, &cli_opts(ctx, p), ctx.tracker)?;
            let log = ctx.log_dir.join(format!("{:02}-cli.log", ctx.step_no));
            std::fs::write(
                &log,
                format!(
                    "$ {}\nexit {:?}\n--- stdout\n{}\n--- stderr\n{}\n",
                    args.join(" "),
                    r.code,
                    r.stdout,
                    r.stderr
                ),
            )?;
            ctx.artifact(&format!("{:02}-cli.log", ctx.step_no), log);
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
            Ok(r.to_value())
        }
        "sidecar_start" => {
            let id = ps_opt(p, "id").unwrap_or_else(|| "sidecar".into());
            let s = ctx
                .engine()?
                .start_sidecar(&cli_opts(ctx, p), ctx.tracker)?;
            let hello = s.hello.clone();
            ctx.sidecars.insert(id, s);
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
            match s.command(&name, input, timeout) {
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
            ctx.tracker.assert_no_orphans()?;
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
            let spec = LaunchSpec {
                binary,
                mode,
                env,
                startup_timeout: Duration::from_secs(ctx.cfg.ui.startup_timeout_s.unwrap_or(90)),
                label: format!("{} app", ctx.cfg.app),
            };
            let c = control::launch(&spec, ctx.ws, ctx.tracker)?;
            let out = json!({"pid": c.pid, "service": c.service, "mode": format!("{mode:?}").to_lowercase(), "health": c.health()?, "log": c.log_path.to_string_lossy()});
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
            let r = c.stop()?;
            if !r.process_gone || !r.endpoint_closed {
                return err(format!(
                    "app pid {} not fully stopped (process_gone={}, endpoint_closed={})",
                    r.pid, r.process_gone, r.endpoint_closed
                ));
            }
            if mode != Mode::Visible && !r.owned_never_frontmost {
                return err(format!(
                    "owned UI process reached foreground: {:?}",
                    r.owned_frontmost_pids
                ));
            }
            Ok(
                json!({"pid": r.pid, "endpoint_closed": r.endpoint_closed, "process_gone": r.process_gone, "frontmost_unchanged": r.frontmost_unchanged, "owned_never_frontmost": r.owned_never_frontmost, "owned_frontmost_pids": r.owned_frontmost_pids}),
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
        "ui_command" => ctx
            .ui()?
            .command(&ps(p, "name")?, pv(p, "args").unwrap_or(&Value::Null)),
        "ui_screenshot" => {
            let name = ps_opt(p, "name").unwrap_or_else(|| format!("shot-{:02}", ctx.step_no));
            let path = ctx.log_dir.join(format!("{name}.png"));
            ctx.ui()?.screenshot_to(&path)?;
            let size = media::png_size(&path)?;
            ctx.artifact(&format!("{name}.png"), path.clone());
            Ok(json!({"path": path.to_string_lossy(), "size": size}))
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
        other => err(format!("unknown step kind '{other}'")),
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
}
