//! `rightkit-qa` CLI.
//!
//! rightkit-qa doctor  [--config rightkit-qa.toml]
//! rightkit-qa list    [--tier fast|local-models|paid|all[,..]] [--only s]
//! rightkit-qa run     [--tier ..] [--only s] [--engine path] [--ui-binary path]
//!                     [--build] [--build-ui] [--keep-data] [FILE.scenario.toml ...]
//! rightkit-qa guard   [--backend control|wdio]
//! rightkit-qa sweep
//! rightkit-qa verify  <evidence.json>
use rightkit_qa::config::QaConfig;
use rightkit_qa::evidence::Bundle;
use rightkit_qa::guard::{scripts_from_package_json, validate, Backend, ContractInput};
use rightkit_qa::scenario::{discover, unmet_requirement, Scenario};
use rightkit_qa::suite::{run_suite, select, SuiteOptions};
use rightkit_qa::util::{err, Result};
use rightkit_qa::process;
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

fn options(a: &Args) -> SuiteOptions {
    SuiteOptions {
        tier: a.value("--tier"),
        only: a.value("--only"),
        engine: a.value("--engine").map(PathBuf::from),
        ui_binary: a.value("--ui-binary").map(PathBuf::from),
        build_engine: a.has("--build"),
        build_ui: a.has("--build-ui"),
        keep_data: a.has("--keep-data"),
        files: a.positionals().into_iter().map(PathBuf::from).collect(),
    }
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
    let scenarios = select(&cfg, &options(a))?;
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

fn run() -> Result<i32> {
    let a = Args(std::env::args().skip(1).collect());
    match a.0.first().map(String::as_str) {
        Some("doctor") => doctor(&a),
        Some("list") => list(&a),
        Some("run") => {
            let cfg = load_config(&a)?;
            Ok(run_suite(&cfg, &options(&a))?.exit_code)
        }
        Some("guard") => guard(&a),
        Some("sweep") => sweep(&a),
        Some("verify") => verify(&a),
        _ => {
            eprintln!("usage: rightkit-qa <doctor|list|run|guard|sweep|verify> [--config rightkit-qa.toml] ...");
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
