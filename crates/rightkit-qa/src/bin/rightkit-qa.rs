//! `rightkit-qa` CLI.
//!
//! rightkit-qa doctor  [--config rightkit-qa.toml]
//! rightkit-qa list    [--tier fast|local-models|paid|all[,..]] [--only s]
//! rightkit-qa run     [--tier ..] [--only s] [--engine path] [--ui-binary path]
//!                     [--build] [--build-ui] [--keep-data] [--kind functional|visual]
//!                     [FILE.scenario.toml ...]
//! rightkit-qa guard   [--backend control|wdio]
//! rightkit-qa sweep
//! rightkit-qa prune-runs [--app <key>] [--root <managed root>] [--dry-run]
//! rightkit-qa verify  <evidence.json>
//! rightkit-qa journeys check [--root <app root>]
//! rightkit-qa journeys validate-receipt <receipt.json> [--tree <40-hex>] [--manifest <path>]
//!                     [--results <results.jsonl>] [--evidence-root <dir>] [--allow-dirty]
use rightkit_qa::config::QaConfig;
use rightkit_qa::evidence::Bundle;
use rightkit_qa::guard::{scripts_from_package_json, validate, Backend, ContractInput};
use rightkit_qa::process;
use rightkit_qa::scenario::{discover, unmet_requirement, Scenario};
use rightkit_qa::storage::RunKind;
use rightkit_qa::suite::{run_suite, select, SuiteOptions};
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
/// `run`, `census` and `gate` are not implemented.
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
            if c.ok() {
                println!(
                    "journeys ok: {} ({} journeys, {} adapters, mapping {})",
                    c.manifest.app,
                    c.manifest.journey.len(),
                    c.manifest.adapter.len(),
                    if c.mapping.is_some() { "present" } else { "absent" }
                );
                Ok(0)
            } else {
                println!("{} issue(s)", c.issues.len());
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
                    p.id.as_deref().map(|i| format!(" [{i}]")).unwrap_or_default(),
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
        _ => {
            eprintln!("usage: rightkit-qa journeys <check|validate-receipt> ...");
            Ok(2)
        }
    }
}

fn run() -> Result<i32> {
    let a = Args(std::env::args().skip(1).collect());
    match a.0.first().map(String::as_str) {
        Some("doctor") => doctor(&a),
        Some("list") => list(&a),
        Some("run") => {
            let cfg = load_config(&a)?;
            Ok(run_suite(&cfg, &options(&a)?)?.exit_code)
        }
        Some("guard") => guard(&a),
        Some("sweep") => sweep(&a),
        Some("prune-runs") => prune_runs(&a),
        Some("homes") => homes(&a),
        Some("clean") => clean(&a),
        Some("verify") => verify(&a),
        Some("journeys") => journeys(&a),
        _ => {
            eprintln!("usage: rightkit-qa <doctor|list|run|guard|sweep|prune-runs|homes|clean|verify|journeys> [--config rightkit-qa.toml] ...");
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
