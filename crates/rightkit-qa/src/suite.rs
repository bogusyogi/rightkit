//! A QA run: select scenarios by tier, take the run lock, create the isolated
//! workspace, run scenarios sequentially, sweep orphans, and write hashed evidence.
use crate::audio::SystemMute;
use crate::config::{validate_tier_list, QaConfig};
use crate::engine::{build_engine, EngineTarget};
use crate::evidence::{Bundle, CheckStatus, RunStatus};
use crate::lock::{self, LockOptions};
use crate::process::{sweep_registries, Tracker};
use crate::scenario::{
    discover, run_step, tier_selected, unmet_requirement, Ctx, Scenario, StepRecord,
};
use crate::util::{err, now_iso, platform, Result};
use crate::workspace;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Default)]
pub struct SuiteOptions {
    pub tier: Option<String>,
    pub only: Option<String>,
    pub engine: Option<PathBuf>,
    pub ui_binary: Option<PathBuf>,
    pub build_engine: bool,
    pub build_ui: bool,
    pub keep_data: bool,
    /// Explicit scenario files (override discovery).
    pub files: Vec<PathBuf>,
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
}

impl Outcome {
    fn to_value(&self) -> Value {
        json!({
            "name": self.name, "file": self.file.to_string_lossy(), "tier": self.tier,
            "status": format!("{:?}", self.status).to_lowercase(), "reason": self.reason, "ms": self.ms,
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
            .map(|o| s.name.contains(o.as_str()) || f.to_string_lossy().contains(o.as_str()))
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
    let mut env: Vec<(String, String)> = cfg
        .env
        .iter()
        .chain(cfg.engine.env.iter())
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    env.push(("RIGHTKIT_QA_TIER".into(), tier.into()));
    env.push(("RIGHTKIT_QA_MUTE".into(), "1".into()));
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
    Ok(binary.map(|binary| EngineTarget {
        binary,
        env,
        data_dir_env: cfg.engine.data_dir_env.clone(),
    }))
}

fn ui_binary(cfg: &QaConfig, opts: &SuiteOptions, tracker: &Tracker) -> Result<Option<PathBuf>> {
    if let Some(p) = &opts.ui_binary {
        return Ok(Some(p.clone()));
    }
    if let Ok(p) = std::env::var("RIGHTKIT_QA_UI_BINARY") {
        return Ok(Some(PathBuf::from(p)));
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
        return Ok(Some(build_engine(
            &cfg.app_root,
            &cfg.ui.build,
            &pkg,
            &env,
            tracker,
        )?));
    }
    cfg.ui_binary()
}

pub fn run_suite(cfg: &QaConfig, opts: &SuiteOptions) -> Result<SuiteReport> {
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
    std::fs::create_dir_all(&cfg.evidence_root)?;
    let lock_opts = LockOptions {
        timeout: Duration::from_secs(cfg.lock.timeout_s.unwrap_or(120)),
        stale: Duration::from_secs(cfg.lock.stale_s.unwrap_or(60)),
        ..Default::default()
    };
    let mut ownership = lock::acquire(
        &cfg.evidence_root.join("native.lock"),
        &format!("{}:{tier}", cfg.app),
        &lock_opts,
    )?;
    let run_id = crate::util::new_id();
    // Earlier crashed runs: kill survivors by recorded pid + command line, then drop their dirs.
    let swept = sweep_registries(&cfg.evidence_root.join("runs"), &run_id);
    workspace::prune_abandoned(
        &cfg.evidence_root,
        Duration::from_secs(6 * 3600),
        std::slice::from_ref(&run_id),
    );
    workspace::prune_abandoned_homes(
        &cfg.app,
        Duration::from_secs(6 * 3600),
        std::slice::from_ref(&run_id),
    );
    let ws = workspace::create(&cfg.evidence_root, Some(&run_id), &cfg.app)?;
    let tracker = Tracker::with_registry(ws.root.join("pids.json"));
    let mut bundle = Bundle::new(&cfg.app, platform, &run_id)?;
    let _mute = cfg.audio.system_mute.then(SystemMute::engage);

    let needs_engine = scenarios
        .iter()
        .any(|s| s.target == "engine" && unmet_requirement(&s.requires, &cfg.app_root).is_none());
    let needs_ui = scenarios.iter().any(|s| s.target == "ui");
    let engine = engine_target(cfg, opts, &tracker, &tier)?;
    let ui = if needs_ui {
        ui_binary(cfg, opts, &tracker)?
    } else {
        None
    };
    if needs_engine && engine.is_none() {
        ownership.release();
        return err("selected scenarios need the engine but no binary is configured (use --engine, RIGHTKIT_QA_ENGINE, [engine].binary, or --build)");
    }

    let identity = match run_identity(
        cfg,
        &scenarios,
        engine.as_ref().map(|e| e.binary.as_path()),
        ui.as_deref(),
        needs_engine,
        false,
    ) {
        Ok(i) => i,
        Err(e) => {
            ownership.release();
            return Err(e);
        }
    };
    bundle.set_identity(identity)?;
    println!(
        "[qa] app={} platform={platform} tier={tier} scenarios={} run={run_id}",
        cfg.app,
        scenarios.len()
    );
    if let Some(e) = &engine {
        println!("[qa] engine={}", e.binary.display());
    }
    if !swept.is_empty() {
        println!("[qa] swept orphan pids from earlier runs: {swept:?}");
    }

    let mut outcomes = vec![];
    for sc in &scenarios {
        let started = Instant::now();
        let name = sc.name.clone();
        if let Some(reason) = unmet_requirement(&sc.requires, &cfg.app_root) {
            if sc.optional {
                println!("[qa] SKIP  {name}: {reason} (optional)");
                bundle.add_check(
                    &name,
                    CheckStatus::Skipped,
                    json!({"reason": reason, "optional": true}),
                    &[],
                )?;
                outcomes.push(Outcome {
                    name,
                    file: sc.file.clone(),
                    tier: sc.tier.clone(),
                    status: CheckStatus::Skipped,
                    reason: Some(reason),
                    ms: 0,
                    steps: vec![],
                });
            } else {
                let reason = format!("mandatory scenario could not run: {reason} (mark it `optional = true` only if skipping is acceptable)");
                println!("[qa] FAIL  {name}: {reason}");
                bundle.add_check(
                    &name,
                    CheckStatus::Failed,
                    json!({"error": reason, "mandatory": true}),
                    &[],
                )?;
                outcomes.push(Outcome {
                    name,
                    file: sc.file.clone(),
                    tier: sc.tier.clone(),
                    status: CheckStatus::Failed,
                    reason: Some(reason),
                    ms: 0,
                    steps: vec![],
                });
            }
            continue;
        }
        if sc.target == "ui" && ui.is_none() {
            let reason =
                "no UI binary for this platform ([ui].binaries / --ui-binary / --build-ui)"
                    .to_string();
            println!("[qa] FAIL  {name}: {reason}");
            bundle.add_check(&name, CheckStatus::Failed, json!({"error": reason}), &[])?;
            outcomes.push(Outcome {
                name,
                file: sc.file.clone(),
                tier: sc.tier.clone(),
                status: CheckStatus::Failed,
                reason: Some(reason),
                ms: 0,
                steps: vec![],
            });
            continue;
        }
        let mut ctx = Ctx::new(cfg, engine.clone(), ui.clone(), &ws, &tracker, &name)?;
        let mut steps = vec![];
        let mut failure: Option<String> = None;
        let limit = Duration::from_secs(sc.timeout_s.unwrap_or(600));
        for (i, step) in sc.steps.iter().enumerate() {
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
                    failure = Some(e.0);
                    break;
                }
            }
        }
        if failure.is_some() {
            if let Some((c, _)) = &ctx.ui {
                let p = ctx.log_dir.join("failure.png");
                if c.screenshot_to(&p).is_ok() {
                    ctx.artifacts.push((format!("{name}/failure.png"), p));
                }
            }
        }
        let problems = ctx.teardown();
        if failure.is_none() && !problems.is_empty() {
            failure = Some(problems.join("; "));
        }
        // Hash every artifact the scenario produced, then bind the check to them.
        let mut names = vec![];
        let mut artifact_errors = vec![];
        for (n, path) in &ctx.artifacts {
            if names.contains(n) {
                continue;
            }
            match bundle.add_artifact(n, path) {
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
        let status = if failure.is_none() {
            CheckStatus::Passed
        } else {
            CheckStatus::Failed
        };
        let details = json!({"file": sc.file.to_string_lossy(), "tier": sc.tier, "steps": steps.len(), "error": failure});
        bundle.add_check(&name, status, details, &names)?;
        println!(
            "[qa] {}  {name} ({} ms){}",
            if failure.is_none() { "PASS" } else { "FAIL" },
            started.elapsed().as_millis(),
            failure
                .as_ref()
                .map(|f| format!("\n       {f}"))
                .unwrap_or_default()
        );
        outcomes.push(Outcome {
            name,
            file: sc.file.clone(),
            tier: sc.tier.clone(),
            status,
            reason: failure,
            ms: started.elapsed().as_millis(),
            steps,
        });
    }

    tracker.kill_all();
    let failed = outcomes
        .iter()
        .filter(|o| o.status == CheckStatus::Failed)
        .count();
    let passed = outcomes
        .iter()
        .filter(|o| o.status == CheckStatus::Passed)
        .count();
    // Skips are only ever optional scenarios; a run whose scenarios were all skipped proves nothing.
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
            "app": cfg.app, "platform": platform, "runId": run_id, "tier": tier, "finishedAt": now_iso(),
            "passed": passed, "failed": failed, "skipped": outcomes.len() - passed - failed,
            "scenarios": outcomes.iter().map(Outcome::to_value).collect::<Vec<_>>(),
        }))?,
    )?;
    bundle.write(&evidence_path)?;
    let _ = std::fs::write(
        cfg.evidence_root.join("latest.json"),
        serde_json::to_vec_pretty(
            &json!({"runId": run_id, "evidence": evidence_path.to_string_lossy(), "results": results_path.to_string_lossy()}),
        )?,
    );
    if !opts.keep_data {
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
    ownership.release();
    Ok(SuiteReport {
        exit_code: if failed == 0 && passed > 0 { 0 } else { 1 },
        run_id,
        run_dir: ws.root.clone(),
        evidence_path,
        results_path,
        outcomes,
    })
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
        std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .ok()
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
) -> Result<Value> {
    let mut scenario_ids = vec![];
    for sc in scenarios {
        scenario_ids.push(json!({"name": sc.name, "file": sc.file.to_string_lossy(), "sha256": file_sha(&sc.file)?}));
    }
    let binary = |p: Option<&std::path::Path>, what: &str, needed: bool| -> Result<Value> {
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
    };
    Ok(json!({
        "config": {"path": cfg.config_path.to_string_lossy(), "sha256": file_sha(&cfg.config_path)?},
        "scenarios": scenario_ids,
        "engine": binary(engine, "engine", needs_engine)?,
        "ui": binary(ui, "UI", needs_ui)?,
        "source": git_revision(&cfg.app_root),
    }))
}
