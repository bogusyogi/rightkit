//! `rightkit-qa` CLI.
//!
//! rightkit-qa doctor  [--config rightkit-qa.toml]
//! rightkit-qa list    [--tier fast|local-models|paid|all[,..]] [--only s]
//! rightkit-qa run     [--tier ..] [--only s] [--engine path] [--ui-binary path]
//!                     [--build] [--build-ui] [--keep-data] [--kind functional|visual]
//!                     [--qualifying] [FILE.scenario.toml ...]
//!                     (a qualifying run, `--qualifying` or RIGHTKIT_QA_QUALIFYING=1, refuses
//!                     --build, --build-ui and --kill-orphans)
//! rightkit-qa worker  <world dir>     (started by a qualifying run, never by hand: runs the one
//!                     scenario in scenario.json inside that world and writes outcome.json)
//! rightkit-qa guard   [--backend control|wdio]
//! rightkit-qa sweep
//! rightkit-qa prune-runs [--app <key>] [--root <managed root>] [--dry-run]
//! rightkit-qa verify  <evidence.json>
//! rightkit-qa run-tests [cargo test args] [--parallel N] [--binary-timeout-s N] [--total-timeout-s N]
//!                     [--from-cargo-json <file>] [--env K=V]... [--cwd package|workspace|<dir>] [-- <libtest args>]
//!                     (compile the test binaries in the broker, run them outside it, write one receipt)
//! rightkit-qa journeys check [--root <app root>]
//! rightkit-qa journeys validate-receipt <receipt.json> [--tree <40-hex>] [--manifest <path>]
//!                     [--results <results.jsonl>] [--evidence-root <dir>] [--allow-dirty]
//! rightkit-qa journeys run [--root <app root>] [--id J-001]... [--platform P] [--tier fast[,full,paid]]
//!                     [--run-store-cap-bytes N] [--prepare-only]
use rightkit_qa::config::QaConfig;
use rightkit_qa::evidence::Bundle;
use rightkit_qa::guard::{scripts_from_package_json, validate, Backend, ContractInput};
use rightkit_qa::process;
use rightkit_qa::scenario::{discover, unmet_requirement, Scenario};
use rightkit_qa::storage::RunKind;
use rightkit_qa::suite::{run_suite, run_suite_qualifying, select, SuiteOptions};
use rightkit_qa::util::{err, Result};
use std::path::{Path, PathBuf};

struct Args(Vec<String>);

impl Args {
    fn value(&self, flag: &str) -> Option<String> {
        self.0
            .iter()
            .position(|a| a == flag)
            .and_then(|i| self.0.get(i + 1))
            .cloned()
    }
    /// Every value of a repeatable flag (`--id a --id b`), in order.
    fn values(&self, flag: &str) -> Vec<String> {
        let mut out = vec![];
        let mut i = 0;
        while i < self.0.len() {
            if self.0[i] == flag {
                if let Some(v) = self.0.get(i + 1) {
                    out.push(v.clone());
                }
                i += 2;
            } else {
                i += 1;
            }
        }
        out
    }
    fn has(&self, flag: &str) -> bool {
        self.0.iter().any(|a| a == flag)
    }
    /// Positionals: everything that is neither a flag nor a flag's value.
    fn positionals(&self) -> Vec<String> {
        let value_flags = [
            "--config",
            "--tier",
            "--only",
            "--engine",
            "--ui-binary",
            "--backend",
            "--dir",
            "--max-gb",
            "--young-hours",
            "--kind",
            "--app",
            "--root",
            "--tree",
            "--manifest",
            "--results",
            "--evidence-root",
            "--id",
            "--platform",
            "--run-store-cap-bytes",
        ];
        let mut out = vec![];
        let mut skip = false;
        for a in self.0.iter().skip(1) {
            if skip {
                skip = false;
            } else if value_flags.contains(&a.as_str()) {
                skip = true;
            } else if !a.starts_with("--") {
                out.push(a.clone());
            }
        }
        out
    }
}

fn load_config(a: &Args) -> Result<QaConfig> {
    let path = a
        .value("--config")
        .or_else(|| std::env::var("RIGHTKIT_QA_CONFIG").ok())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("rightkit-qa.toml"));
    QaConfig::load(&path)
}

fn options(a: &Args) -> Result<SuiteOptions> {
    Ok(SuiteOptions {
        tier: a.value("--tier"),
        only: a.value("--only"),
        engine: a.value("--engine").map(PathBuf::from),
        ui_binary: a.value("--ui-binary").map(PathBuf::from),
        build_engine: a.has("--build"),
        build_ui: a.has("--build-ui"),
        keep_data: a.has("--keep-data"),
        kind: a.value("--kind").map(|k| RunKind::parse(&k)).transpose()?,
        files: a.positionals().into_iter().map(PathBuf::from).collect(),
        hooks: Default::default(),
        kill_orphans: a.has("--kill-orphans"),
        plugin: None,
    })
}

fn doctor(a: &Args) -> Result<i32> {
    let cfg = load_config(a)?;
    println!(
        "rightkit-qa config valid: {} (root {})",
        cfg.app,
        cfg.app_root.display()
    );
    println!("platform: {}", rightkit_qa::util::platform()?);
    let show = |label: &str, p: Option<PathBuf>| match p {
        Some(p) => println!(
            "{label}: {} ({})",
            p.display(),
            if p.exists() {
                "present"
            } else {
                "missing; build it or pass a path"
            }
        ),
        None => println!("{label}: not configured for this platform"),
    };
    show("engine binary", cfg.engine_binary()?);
    show("ui binary", cfg.ui_binary()?);
    let files = discover(&cfg.scenario_dirs)?;
    let mut bad = 0;
    for f in &files {
        match Scenario::load(f) {
            Ok(s) => {
                let unmet = unmet_requirement(&s.requires, &cfg.app_root)
                    .map(|r| format!(" [would skip: {r}]"))
                    .unwrap_or_default();
                println!("  ok   {:<14} {:<7} {}{unmet}", s.tier, s.target, s.name);
            }
            Err(e) => {
                bad += 1;
                println!("  FAIL {}: {e}", f.display());
            }
        }
    }
    println!("{} scenario files, {bad} invalid", files.len());
    Ok(if bad == 0 { 0 } else { 1 })
}

fn list(a: &Args) -> Result<i32> {
    let cfg = load_config(a)?;
    let scenarios = select(&cfg, &options(a)?)?;
    for s in &scenarios {
        println!(
            "{:<14} {:<7} {}  ({})",
            s.tier,
            s.target,
            s.name,
            s.file.display()
        );
    }
    println!("{} scenarios", scenarios.len());
    Ok(0)
}

fn guard(a: &Args) -> Result<i32> {
    let cfg = load_config(a)?;
    let backend = a
        .value("--backend")
        .map(|b| {
            Backend::parse(&b).ok_or_else(|| rightkit_qa::Error(format!("unknown backend {b}")))
        })
        .transpose()?
        .unwrap_or(Backend::Control);
    let read = |rel: &Option<String>, default: &str| -> Result<String> {
        let p = cfg.app_root.join(rel.as_deref().unwrap_or(default));
        std::fs::read_to_string(&p)
            .map_err(|e| rightkit_qa::Error(format!("cannot read {}: {e}", p.display())))
    };
    let cargo = read(&cfg.ui.cargo_toml, "src-tauri/Cargo.toml")?;
    let rust = if cfg.ui.rust_sources.is_empty() {
        read(&None, "src-tauri/src/lib.rs")?
    } else {
        cfg.ui
            .rust_sources
            .iter()
            .map(|s| read(&Some(s.clone()), ""))
            .collect::<Result<Vec<_>>>()?
            .join("\n")
    };
    let scripts = scripts_from_package_json(
        &read(&cfg.ui.package_json, "package.json").unwrap_or_else(|_| "{}".into()),
    )?;
    let issues = validate(&ContractInput {
        cargo_toml: &cargo,
        rust_source: &rust,
        package_scripts: &scripts,
        backend,
    });
    if issues.is_empty() {
        println!("native QA contract ok: {}", cfg.app);
        Ok(0)
    } else {
        for i in &issues {
            println!("- {i}");
        }
        Ok(1)
    }
}

fn sweep(a: &Args) -> Result<i32> {
    let cfg = load_config(a)?;
    let managed_root = rightkit_qa::storage::managed_root(None);
    let killed = process::sweep_registries(&managed_root.join("runs").join(&cfg.app), "");
    let r = rightkit_qa::storage::prune_with_keep(&managed_root, &cfg.app, None, &[])?;
    println!(
        "killed {} orphan pid(s) {killed:?}; removed {} abandoned run dir(s), retained {}",
        killed.len(),
        r.removed.len(),
        r.retained.len()
    );
    Ok(0)
}

/// `rightkit-qa prune-runs [--app <key>] [--root <managed root>] [--dry-run]`: apply the run
/// retention rule to `<managed root>/runs`. Only directories that carry the `.rightkit-run.json`
/// marker are considered; every failed run and the newest passing run per app and kind are kept,
/// the other passing runs are removed. Without `--app` every app is pruned; without `--root` the
/// managed root is `RIGHTKIT_MANAGED_ROOT` or the platform default. Suited to a schedule; the suite
/// and the harness already run it right after each job.
fn prune_runs(a: &Args) -> Result<i32> {
    let root = rightkit_qa::storage::managed_root(a.value("--root").map(PathBuf::from).as_deref());
    let dry = a.has("--dry-run");
    let r = rightkit_qa::retention::prune_runs(&root, a.value("--app").as_deref(), dry)?;
    for (path, bytes, why) in &r.removed {
        println!(
            "{} {} ({bytes} bytes): {why}",
            if dry { "would remove" } else { "removed" },
            path.display()
        );
    }
    println!("{} under {}", r.summary(), root.join("runs").display());
    Ok(0)
}

/// `rightkit-qa clean [--dry-run] [--dir <.../rightkit-qa>] [--max-age-hours N]`: remove the QA
/// homes of dead runs (owner process gone, older than the window; default 2 h or
/// `RIGHTKIT_QA_HOME_MAX_AGE_SECS`). A home whose owner is alive is never touched. A home written
/// before the owner marker existed goes when it is named like a QA home, no pid its records name is
/// alive, and nothing in it changed within the window (`storage::clean_homes` states the rule).
fn clean(a: &Args) -> Result<i32> {
    use rightkit_qa::storage::{clean_homes, home_max_age, qa_root};
    let dir = a
        .value("--dir")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(qa_root);
    let max_age = a
        .value("--max-age-hours")
        .and_then(|v| v.parse::<f64>().ok())
        .map(|h| std::time::Duration::from_secs_f64(h * 3600.0))
        .unwrap_or_else(home_max_age);
    let dry = a.has("--dry-run");
    let r = clean_homes(&dir, max_age, dry);
    for (path, bytes, why) in &r.removed {
        println!(
            "{} {} ({bytes} bytes): {why}",
            if dry { "would remove" } else { "removed" },
            path.display()
        );
    }
    println!(
        "{} {} home(s), {} bytes of {} bytes found; {} retained",
        if dry { "would remove" } else { "removed" },
        r.removed.len(),
        r.removed_bytes,
        r.total_bytes,
        r.retained.len()
    );
    Ok(0)
}

/// `rightkit-qa homes [--apply] [--dir <.../rightkit-qa>] [--max-gb N] [--young-hours N]`:
/// report (default) or remove (`--apply`) app homes the retention rule no longer protects.
fn homes(a: &Args) -> Result<i32> {
    use rightkit_qa::storage::{sweep_homes, HomePolicy, HOME_ROOT_ENV};
    let dir = match a.value("--dir") {
        Some(d) => std::path::PathBuf::from(d),
        None => std::env::var_os(HOME_ROOT_ENV)
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::PathBuf::from(if cfg!(target_os = "macos") {
                    "/private/tmp"
                } else {
                    ""
                })
            })
            .join("rightkit-qa"),
    };
    let mut policy = HomePolicy::default();
    if let Some(gb) = a.value("--max-gb").and_then(|v| v.parse::<f64>().ok()) {
        policy.max_bytes = (gb * 1e9) as u64;
    }
    if let Some(h) = a.value("--young-hours").and_then(|v| v.parse::<u64>().ok()) {
        policy.young = std::time::Duration::from_secs(h * 3600);
    }
    let apply = a.has("--apply");
    let r = sweep_homes(&dir, policy, !apply);
    for (path, bytes, why) in &r.removed {
        println!(
            "{} {} ({bytes} bytes): {why}",
            if apply { "removed" } else { "would remove" },
            path.display()
        );
    }
    println!(
        "{} {} home(s), {} bytes of {} bytes found; {} retained",
        if apply { "removed" } else { "would remove" },
        r.removed.len(),
        r.removed_bytes,
        r.total_bytes,
        r.retained.len()
    );
    Ok(0)
}

fn verify(a: &Args) -> Result<i32> {
    let Some(path) = a.positionals().first().cloned() else {
        return err("usage: rightkit-qa verify <evidence.json>");
    };
    let bundle: Bundle = serde_json::from_slice(&std::fs::read(Path::new(&path))?)?;
    let drift = bundle.verify();
    if drift.is_empty() {
        println!(
            "evidence intact: {} artifacts, status {:?}",
            bundle.artifacts.len(),
            bundle.status
        );
        Ok(0)
    } else {
        for (n, why) in &drift {
            println!("DRIFT {n}: {why}");
        }
        Ok(1)
    }
}

/// `rightkit-qa journeys check [--root <app root>]` and
/// `rightkit-qa journeys validate-receipt <receipt.json> [--tree <40-hex>] [--manifest <path>]
/// [--results <results.jsonl>] [--evidence-root <dir>] [--allow-dirty]`.
///
/// `check` exits 0 only when `journeys.toml` (and `journeys-map.toml` when present) has no issue.
/// `validate-receipt` exits 0 only when the receipt is a pass after every D5 rule is re-checked;
/// any problem, an unreadable receipt or a missing results file exits non-zero. `--results`
/// defaults to `results.jsonl` beside the receipt and `--evidence-root` to the receipt's directory.
///
/// `rightkit-qa journeys run [--root <app root>] [--id J-001]... [--platform P] [--tier fast[,full,paid]]
/// [--run-store-cap-bytes N] [--prepare-only]` is always a qualifying run: it selects the journeys
/// for this host up front, records the source identity, builds through the broker
/// (`rustright cargo ... --no-run --message-format=json`) and writes `build-record.json` into the
/// managed run directory. It refuses `--build`, `--build-ui` and `--kill-orphans`. Without
/// `--prepare-only` it then launches each scenario in a supervised worker outside the broker and
/// collects the outcomes into a validated receipt; with `--prepare-only` it stops at the written
/// record (exit 0) and closes the run without a recorded outcome. `census` and `gate` are not
/// implemented.
fn journeys(a: &Args) -> Result<i32> {
    use rightkit_qa::journeys::manifest::parse_manifest;
    use rightkit_qa::journeys::{check, parse_receipt, validate_receipt, ValidateOptions};
    let sub = a.0.get(1).map(String::as_str);
    match sub {
        Some("check") => {
            let root = PathBuf::from(a.value("--root").unwrap_or_else(|| ".".into()));
            let c = check(&root)?;
            for i in &c.issues {
                println!("- {i}");
            }
            for i in &c.guardrail_issues {
                println!("- guardrail: {i}");
            }
            if c.ok() {
                println!(
                    "journeys ok: {} ({} journeys, {} adapters, mapping {})",
                    c.manifest.app,
                    c.manifest.journey.len(),
                    c.manifest.adapter.len(),
                    if c.mapping.is_some() {
                        "present"
                    } else {
                        "absent"
                    }
                );
                Ok(0)
            } else {
                println!("{} issue(s)", c.issues.len() + c.guardrail_issues.len());
                Ok(1)
            }
        }
        Some("validate-receipt") => {
            let pos = a.positionals();
            let Some(file) = pos.get(1).map(PathBuf::from) else {
                return err("usage: rightkit-qa journeys validate-receipt <receipt.json> [--tree <40-hex>] [--manifest <path>] [--results <file>] [--evidence-root <dir>] [--allow-dirty]");
            };
            let receipt = parse_receipt(&std::fs::read(&file)?)?;
            let dir = file
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."));
            let results_path = a
                .value("--results")
                .map(PathBuf::from)
                .unwrap_or_else(|| dir.join("results.jsonl"));
            let results = std::fs::read(&results_path).ok();
            let evidence_root = a
                .value("--evidence-root")
                .map(PathBuf::from)
                .unwrap_or_else(|| dir.clone());
            // Without --manifest the path the receipt records is tried; a manifest that cannot be
            // read leaves the receipt incomplete (validate_receipt refuses it), never accepted.
            let manifest_path = a
                .value("--manifest")
                .unwrap_or_else(|| receipt.manifest.path.clone());
            let manifest = std::fs::read(&manifest_path)
                .ok()
                .and_then(|b| parse_manifest(&b).ok());
            let tree = a.value("--tree");
            let v = validate_receipt(
                &receipt,
                &ValidateOptions {
                    expected_tree: tree.as_deref(),
                    manifest: manifest.as_ref().map(|(m, s)| (m, s.as_str())),
                    results: results.as_deref(),
                    evidence_root: Some(evidence_root.as_path()),
                    allow_dirty: a.has("--allow-dirty"),
                },
            );
            for p in &v.problems {
                println!(
                    "- {}{}: {}",
                    p.code,
                    p.id.as_deref()
                        .map(|i| format!(" [{i}]"))
                        .unwrap_or_default(),
                    p.detail
                );
            }
            for u in &v.unchecked {
                println!("unchecked: {u}");
            }
            println!(
                "receipt claims {}, recomputed {}: {}",
                v.claimed.as_str(),
                v.verdict.as_str(),
                if v.passed() { "ACCEPTED" } else { "REFUSED" }
            );
            Ok(if v.passed() { 0 } else { 1 })
        }
        Some("run") => journeys_run(a),
        _ => {
            eprintln!("usage: rightkit-qa journeys <check|validate-receipt|run> ...");
            Ok(2)
        }
    }
}

/// `journeys run`: select, source identity, broker build and build record (steps 1 to 3), then
/// launch, collect and the validated receipt (steps 4 and 5). `--prepare-only` stops after step 3.
fn journeys_run(a: &Args) -> Result<i32> {
    use rightkit_qa::journeys::run::{
        close_prepared, prepare, refuse_flags_in_qualifying, run_journeys, JourneyRunOptions,
        PrepareOptions, RunMode, StoreCap,
    };
    // `journeys run` is always a qualifying run: no build flags, no quiet orphan kills.
    refuse_flags_in_qualifying(&a.0)?;
    let env = |k: &str| std::env::var(k).ok();
    let cap = StoreCap::resolve(a.value("--run-store-cap-bytes").as_deref(), &env)?;
    let options = PrepareOptions {
        app_root: PathBuf::from(a.value("--root").unwrap_or_else(|| ".".into())),
        ids: a.values("--id"),
        platform: a.value("--platform"),
        tier: a.value("--tier"),
        managed_root: None,
        store_cap: cap,
    };
    if a.has("--prepare-only") {
        let prepared = prepare(&options)?;
        println!("{}", prepared.describe());
        // Nothing was launched: close the run without an outcome and bring the store under its cap.
        let report = close_prepared(&prepared.run, cap);
        println!("run store: {}", report.summary());
        for e in &report.errors {
            eprintln!("rightkit-qa: cleanup: {e}");
        }
        return Ok(0);
    }
    let report = run_journeys(&JourneyRunOptions {
        prepare: options,
        exe: std::env::current_exe()?,
        mode: RunMode::Qualifying,
    })?;
    println!("{}", report.describe());
    Ok(report.exit_code())
}

const GENERAL_USAGE: &str = "usage: rightkit-qa <doctor|list|run|guard|sweep|prune-runs|homes|clean|verify|run-tests|journeys> [--config rightkit-qa.toml] ...\nrun `rightkit-qa <command> --help` for one command's usage";

/// The usage a `--help` or `-h` request prints, or `None` when the command line asks for no help.
/// Help is decided before anything is selected, loaded or run: the caller prints it and exits 0.
/// `worker` is the runner's own child command (its argument is a world directory), so it never asks.
fn help_request(args: &[String]) -> Option<String> {
    let is_help = |s: &String| s == "--help" || s == "-h";
    // Arguments after a `--` belong to a test binary (`run-tests -- --help`), never to this command.
    let args = args.split(|a| a == "--").next().unwrap_or(args);
    if !args.iter().any(is_help) {
        return None;
    }
    let cmd = args.first().filter(|s| !is_help(s)).map(String::as_str);
    let usage = match cmd {
        None => GENERAL_USAGE.to_string(),
        Some("worker") => return None,
        Some("journeys") => {
            let sub = args.get(1).filter(|s| !is_help(s)).map(String::as_str);
            journeys_usage(sub)
        }
        Some(c) => command_usage(c),
    };
    Some(usage)
}

fn command_usage(cmd: &str) -> String {
    let line = match cmd {
        "doctor" => "usage: rightkit-qa doctor [--config rightkit-qa.toml]",
        "list" => "usage: rightkit-qa list [--config rightkit-qa.toml]",
        "run" => "usage: rightkit-qa run [--config rightkit-qa.toml] [--qualifying] [--build] [--build-ui] [--kill-orphans] ...",
        "guard" => "usage: rightkit-qa guard [--config rightkit-qa.toml] ...",
        "sweep" => "usage: rightkit-qa sweep [--config rightkit-qa.toml] ...",
        "prune-runs" => "usage: rightkit-qa prune-runs [--config rightkit-qa.toml] ...",
        "homes" => "usage: rightkit-qa homes [--config rightkit-qa.toml] ...",
        "clean" => "usage: rightkit-qa clean [--config rightkit-qa.toml] ...",
        "verify" => "usage: rightkit-qa verify <evidence.json>",
        "run-tests" => return rightkit_qa::run_tests::cli::USAGE.to_string(),
        _ => return GENERAL_USAGE.to_string(),
    };
    line.to_string()
}

fn journeys_usage(sub: Option<&str>) -> String {
    let line = match sub {
        Some("check") => "usage: rightkit-qa journeys check [--root <dir>]",
        Some("validate-receipt") => "usage: rightkit-qa journeys validate-receipt <receipt.json> [--tree <40-hex>] [--manifest <path>] [--results <file>] [--evidence-root <dir>] [--allow-dirty]",
        Some("run") => "usage: rightkit-qa journeys run [--qualifying] [--prepare-only] [--config rightkit-qa.toml] ...",
        _ => "usage: rightkit-qa journeys <check|validate-receipt|run> ...",
    };
    line.to_string()
}

fn run() -> Result<i32> {
    let a = Args(std::env::args().skip(1).collect());
    if let Some(usage) = help_request(&a.0) {
        println!("{usage}");
        return Ok(0);
    }
    match a.0.first().map(String::as_str) {
        Some("doctor") => doctor(&a),
        Some("list") => list(&a),
        Some("run") => {
            let mut opts = options(&a)?;
            // A qualifying run never compiles and never kills survivors quietly.
            if rightkit_qa::journeys::run::is_qualifying(&a.0, &|k| std::env::var(k).ok()) {
                rightkit_qa::journeys::run::refuse_flags_in_qualifying(&a.0)?;
                rightkit_qa::journeys::run::refuse_options_in_qualifying(&opts)?;
            }
            let cfg = load_config(&a)?;
            // The app's step plugin, declared in rightkit-qa.toml `[plugin]`, reaches both paths.
            opts.plugin = cfg.plugin_spec()?;
            // A qualifying run is judged by the strict rule and runs each scenario in a supervised
            // worker (`<this exe> worker <world dir>`); any other run stays in this process.
            if rightkit_qa::journeys::run::is_qualifying(&a.0, &|k| std::env::var(k).ok()) {
                return Ok(run_suite_qualifying(&cfg, &opts)?.exit_code);
            }
            Ok(run_suite(&cfg, &opts)?.exit_code)
        }
        Some(rightkit_qa::worker_main::WORKER_SUBCOMMAND) => {
            let Some(dir) = a.0.get(1) else {
                return err("usage: rightkit-qa worker <world dir>");
            };
            Ok(rightkit_qa::worker_main::main(
                Path::new(dir),
                rightkit_qa::suite::run_spec,
            ))
        }
        Some("guard") => guard(&a),
        Some("sweep") => sweep(&a),
        Some("prune-runs") => prune_runs(&a),
        Some("homes") => homes(&a),
        Some("clean") => clean(&a),
        Some("verify") => verify(&a),
        Some("journeys") => journeys(&a),
        Some("run-tests") => rightkit_qa::run_tests::cli::main(&a.0[1..]),
        _ => {
            eprintln!("usage: rightkit-qa <doctor|list|run|worker|guard|sweep|prune-runs|homes|clean|verify|run-tests|journeys> [--config rightkit-qa.toml] ...");
            Ok(2)
        }
    }
}

fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("rightkit-qa: {e}");
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod help_tests {
    use super::help_request;

    fn argv(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn help_for_every_subcommand_prints_usage_and_selects_nothing() {
        for cmd in [
            "doctor",
            "list",
            "run",
            "guard",
            "sweep",
            "prune-runs",
            "homes",
            "clean",
            "verify",
            "run-tests",
        ] {
            for flag in ["--help", "-h"] {
                let text = help_request(&argv(&format!("{cmd} {flag}")))
                    .unwrap_or_else(|| panic!("{cmd} {flag} asks for help"));
                assert!(
                    text.starts_with(&format!("usage: rightkit-qa {cmd}")),
                    "{text}"
                );
            }
        }
    }

    #[test]
    fn journeys_run_help_is_its_own_usage() {
        let text = help_request(&argv("journeys run --help")).expect("help");
        assert!(
            text.starts_with("usage: rightkit-qa journeys run"),
            "{text}"
        );
        let text = help_request(&argv("journeys validate-receipt -h")).expect("help");
        assert!(
            text.starts_with("usage: rightkit-qa journeys validate-receipt"),
            "{text}"
        );
        let text = help_request(&argv("journeys --help")).expect("help");
        assert!(text.starts_with("usage: rightkit-qa journeys <"), "{text}");
    }

    #[test]
    fn help_with_no_command_is_the_general_usage() {
        assert!(help_request(&argv("--help"))
            .expect("help")
            .starts_with("usage: rightkit-qa <"));
    }

    #[test]
    fn a_command_line_without_help_asks_for_nothing() {
        assert!(help_request(&argv("run --config rightkit-qa.toml")).is_none());
        assert!(help_request(&argv("journeys run --qualifying")).is_none());
        assert!(help_request(&argv("")).is_none());
    }

    #[test]
    fn run_tests_help_is_only_asked_before_the_libtest_arguments() {
        let text = help_request(&argv("run-tests -p fx --help")).expect("help");
        assert!(text.contains("--from-cargo-json"), "{text}");
        // After `--` the flag belongs to the test binary.
        assert!(help_request(&argv("run-tests -p fx -- --help")).is_none());
        assert!(help_request(&argv("run-tests -- -h")).is_none());
        assert!(help_request(&argv("run-tests --help -- --nocapture")).is_some());
    }

    #[test]
    fn the_worker_child_command_never_takes_help() {
        assert!(help_request(&argv(r"worker D:\worlds\w1")).is_none());
    }
}
