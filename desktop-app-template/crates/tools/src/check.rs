//! Manifest-consistency gate for the template and every app generated from it. Pure file checks:
//! no install, no network, no build. Fails on drift between manifests, on inexact or non-registry
//! RightKit pins, on stale or bypassed placeholders, and on forbidden Node test harnesses.
use crate::{read, read_json, Result};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

/// Test/UI automation harnesses that must not appear in an app (Rust `rightkit-qa` is the harness).
const FORBIDDEN_HARNESS_DEPS: [&str; 7] = ["puppeteer", "puppeteer-core", "playwright", "@playwright/test", "selenium-webdriver", "webdriverio", "@wdio/cli"];
/// Node files allowed under `scripts/`: gated release-packaging placeholders only.
const ALLOWED_NODE_SCRIPTS: [&str; 1] = ["package-placeholder.mjs"];

fn field(toml: &str, key: &str) -> Option<String> {
    toml.lines().find_map(|l| {
        let l = l.trim();
        let rest = l.strip_prefix(key)?.trim_start().strip_prefix('=')?.trim();
        rest.strip_prefix('"').and_then(|r| r.split('"').next()).map(String::from)
    })
}

fn after<'a>(s: &'a str, marker: &str) -> &'a str {
    s.find(marker).map(|i| &s[i..]).unwrap_or("")
}

fn active_lines(s: &str) -> impl Iterator<Item = &str> {
    s.lines().filter(|l| !l.trim_start().starts_with('#'))
}

fn exact_registry(spec: &str) -> bool {
    let parts: Vec<&str> = spec.split('.').collect();
    parts.len() == 3 && parts.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

pub fn run(root: &Path) -> Result<()> {
    let mut problems: Vec<String> = vec![];
    let mut need = |ok: bool, msg: String| {
        if !ok {
            problems.push(msg)
        }
    };

    let pkg = read_json(root, "package.json")?;
    let tauri = read_json(root, "src-tauri/tauri.conf.json")?;
    let shell_toml = read(root, "src-tauri/Cargo.toml")?;
    let engine_toml = read(root, "crates/engine/Cargo.toml")?;
    let protocol_toml = read(root, "crates/protocol/Cargo.toml")?;
    let placeholders = read_json(root, "placeholders.json")?;
    let name = pkg["name"].as_str().unwrap_or("").to_string();
    let version = pkg["version"].as_str().unwrap_or("").to_string();

    // 1. App identity agrees across manifests.
    need(tauri["version"] == version.as_str(), format!("tauri.conf version {} != package.json {version}", tauri["version"]));
    for (label, toml) in [("src-tauri", &shell_toml), ("engine", &engine_toml), ("protocol", &protocol_toml)] {
        need(field(toml, "version").as_deref() == Some(version.as_str()), format!("{label}/Cargo.toml version != {version}"));
    }
    need(field(&shell_toml, "name").as_deref() == Some(name.as_str()), "src-tauri package name != package.json name".into());
    need(tauri["mainBinaryName"].as_str() == field(after(&shell_toml, "[[bin]]"), "name").as_deref(), "mainBinaryName != [[bin]] name".into());
    need(tauri["productName"].as_str().map(|p| p.to_lowercase()).as_deref() == Some(name.as_str()), "productName lowercased != package name".into());
    need(tauri["identifier"].as_str().map(|i| i.ends_with(&format!(".{name}"))).unwrap_or(false), "identifier must end with .<package name>".into());

    // 2. Sidecar wiring: externalBin <-> engine bin <-> supervisor.
    let engine_bin = field(after(&engine_toml, "[[bin]]"), "name").unwrap_or_default();
    let ext: Vec<&str> = tauri["bundle"]["externalBin"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    need(ext == [format!("binaries/{engine_bin}").as_str()], format!("externalBin must be binaries/{engine_bin}"));
    need(read(root, "src-tauri/src/engine_supervisor.rs")?.contains(&engine_bin), "supervisor does not name the engine binary".into());

    // 3. Dev server port agrees: tauri devUrl, vite, package dev script.
    let dev_url = tauri["build"]["devUrl"].as_str().unwrap_or("");
    let port = dev_url.rsplit(':').next().unwrap_or("").trim_end_matches('/').to_string();
    need(read(root, "vite.config.ts")?.contains(&format!("port: {port}")), "vite port != tauri devUrl port".into());
    need(pkg["scripts"]["dev"].as_str().map(|d| d.contains(&format!("--port {port}"))).unwrap_or(false), "package dev script port != tauri devUrl port".into());
    need(tauri["build"]["frontendDist"] == "../dist" && read(root, "vite.config.ts")?.contains("outDir: \"dist\""), "frontendDist != vite outDir".into());

    // 4. Capabilities cover every registered plugin.
    let lib = read(root, "src-tauri/src/lib.rs")?;
    let perms = read_json(root, "src-tauri/capabilities/default.json")?["permissions"].to_string();
    for (plugin, perm) in [("tauri_plugin_updater", "updater:"), ("tauri_plugin_process", "process:"), ("tauri_plugin_os", "os:")] {
        need(!lib.contains(plugin) || perms.contains(perm), format!("capability missing {perm}* for {plugin}"));
    }

    // 5. RightKit pins: exact, registry-only, equal to the version manifest when reachable.
    let manifest: Option<Value> = std::fs::read_to_string(root.join("..").join("packages").join("release").join("rightkit-versions.json")).ok().and_then(|t| serde_json::from_str(&t).ok());
    let mut deps: BTreeMap<String, String> = BTreeMap::new();
    for section in ["dependencies", "devDependencies"] {
        for (k, v) in pkg[section].as_object().into_iter().flatten() {
            deps.insert(k.clone(), v.as_str().unwrap_or("").to_string());
        }
    }
    let no_placeholders = serde_json::Map::new();
    let ph_npm = placeholders["npm"].as_object().unwrap_or(&no_placeholders);
    let ph_cargo = placeholders["cargo"].as_object().unwrap_or(&no_placeholders);
    for (dep, spec) in deps.iter().filter(|(n, _)| n.starts_with("@rightkit/")) {
        need(exact_registry(spec), format!("{dep} must be an exact registry version, got {spec}"));
        need(!ph_npm.contains_key(dep), format!("{dep} is listed as a placeholder; remove it from placeholders.json when adopting"));
        if let Some(m) = &manifest {
            need(m["npm"][dep].is_string() || m["stagedNpm"][dep].is_string(), format!("{dep} unknown to rightkit-versions.json"));
        }
    }
    for spec in deps.values() {
        let bad = ["file:", "link:", "workspace:", "git:", "github:", "npm:", "http://", "https://"].iter().any(|p| spec.starts_with(p));
        need(!bad, format!("non-registry spec {spec}"));
    }
    let all_cargo: String = [&shell_toml, &engine_toml, &protocol_toml].iter().flat_map(|t| active_lines(t)).collect::<Vec<_>>().join("\n");
    for line in all_cargo.lines() {
        let Some((dep, rest)) = line.split_once('=') else { continue };
        let dep = dep.trim();
        if !dep.starts_with("rightkit-") {
            continue;
        }
        let rest = rest.trim();
        need(!rest.contains("path") && !rest.contains("git"), format!("{dep}: rightkit crates must not use path/git deps"));
        let spec = rest.trim_matches('"');
        let spec = if rest.starts_with('{') { rest.split("version").nth(1).and_then(|v| v.split('"').nth(1)).unwrap_or("") } else { spec };
        need(spec.starts_with('=') && exact_registry(&spec[1..]), format!("{dep} must be pinned with =x.y.z, got {spec}"));
        need(!ph_cargo.contains_key(dep), format!("{dep} is listed as a placeholder; remove it from placeholders.json when adopting"));
        if let Some(m) = &manifest {
            let want = m["stagedCargo"][dep].as_str().or_else(|| m["cargo"][dep].as_str());
            need(want == spec.strip_prefix('='), format!("{dep} pin {spec} != rightkit-versions.json {want:?}"));
        }
    }

    // 6. Placeholders are honest: each stand-in exists while the entry does, and a capability
    // that the version manifest now lists must be adopted instead of stood in for.
    for (section, entries) in [("npm", ph_npm), ("cargo", ph_cargo)] {
        for (dep, entry) in entries {
            if let Some(stand_in) = entry["standIn"].as_str().filter(|s| s.contains('/') || s.contains('.')) {
                if !stand_in.contains(' ') {
                    need(root.join(stand_in).exists(), format!("placeholder {dep}: stand-in {stand_in} is gone; delete the placeholders.json entry"));
                }
            }
            // `npm`/`cargo` are published; `staged*` are built but not yet on the registry, so a
            // staged capability stays a gated placeholder until it moves to the published list.
            if let Some(m) = &manifest {
                let published = if section == "npm" { &m["npm"][dep] } else { &m["cargo"][dep] };
                need(!published.is_string(), format!("{dep} is published in rightkit-versions.json: pin it exactly, delete its stand-in, and drop the placeholder"));
            }
        }
    }

    // 7. Every @rightkit import is a declared dependency.
    let mut files = vec![];
    collect(&root.join("src"), &mut files, &["ts", "tsx"]);
    for f in files {
        let text = std::fs::read_to_string(&f).map_err(|e| e.to_string())?;
        for line in text.lines().filter(|l| !l.trim_start().starts_with("//")) {
            if let Some(rest) = line.split("from \"@rightkit/").nth(1).or_else(|| line.split("from '@rightkit/").nth(1)) {
                let pkg_name = format!("@rightkit/{}", rest.split(['/', '"', '\'']).next().unwrap_or(""));
                need(deps.contains_key(&pkg_name), format!("{} imports undeclared {pkg_name}", f.display()));
            }
        }
    }

    // 8. No Node test harness: Rust `rightkit-qa` is the only e2e path.
    for dep in deps.keys() {
        need(!FORBIDDEN_HARNESS_DEPS.contains(&dep.as_str()), format!("{dep} is a forbidden test harness dependency; drive the app with rightkit-qa"));
    }
    for (key, cmd) in pkg["scripts"].as_object().into_iter().flatten() {
        let cmd = cmd.as_str().unwrap_or("");
        let packaging_placeholder = key.starts_with("rightkit:package:");
        need(packaging_placeholder || !(cmd.starts_with("node ") || cmd.contains("&& node ")), format!("script {key} runs Node ({cmd}); use rightapp-tools or rightkit-qa"));
    }
    let mut node_files = vec![];
    collect(&root.join("scripts"), &mut node_files, &["mjs", "js", "cjs"]);
    collect(&root.join("tests"), &mut node_files, &["mjs", "js", "cjs", "ts"]);
    for f in node_files {
        let n = f.file_name().and_then(|n| n.to_str()).unwrap_or("");
        need(ALLOWED_NODE_SCRIPTS.contains(&n), format!("{} is a Node helper/harness; write it in Rust", f.display()));
    }
    let qa = std::fs::read_to_string(root.join("rightkit-qa.toml")).unwrap_or_default();
    need(field(&qa, "app").as_deref() == Some(name.as_str()), "rightkit-qa.toml app != package name".into());
    let mut scenarios = vec![];
    collect(&root.join("tests").join("scenarios"), &mut scenarios, &["toml"]);
    need(!scenarios.is_empty(), "tests/scenarios has no *.scenario.toml (engine black-box scenarios are required)".into());

    // 9. pnpm release-age exclusions list exactly the pinned freshly published @rightkit versions.
    let ws = read(root, "pnpm-workspace.yaml")?;
    for dep in ["@rightkit/release", "@rightkit/logs", "@rightkit/tauri", "@rightkit/updates"] {
        if let Some(spec) = deps.get(dep) {
            need(ws.contains(&format!("'{dep}@{spec}'")), format!("pnpm-workspace.yaml minimumReleaseAgeExclude missing {dep}@{spec}"));
        }
    }

    // 10. Release config names this app (right-release owns the rest and validates it itself).
    let release = read(root, "right-release.config.mjs")?;
    need(release.contains(&format!("app: \"{name}\"")), format!("right-release.config.mjs app != {name}"));

    if !problems.is_empty() {
        for p in &problems {
            eprintln!("FAIL {p}");
        }
        return Err(format!("{} template check(s) failed", problems.len()));
    }
    println!(
        "template manifests consistent ({} npm pins, {})",
        deps.keys().filter(|n| n.starts_with("@rightkit/")).count(),
        if manifest.is_some() { "checked against rightkit-versions.json" } else { "version manifest not reachable" }
    );
    Ok(())
}

fn collect(dir: &Path, out: &mut Vec<std::path::PathBuf>, exts: &[&str]) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            if !matches!(e.file_name().to_str(), Some("node_modules" | "target" | "gen")) {
                collect(&p, out, exts);
            }
        } else if p.extension().and_then(|x| x.to_str()).map(|x| exts.contains(&x)).unwrap_or(false) {
            out.push(p);
        }
    }
}
