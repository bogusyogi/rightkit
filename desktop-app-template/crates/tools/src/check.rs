//! Manifest-consistency gate for the template and every app generated from it. Pure file checks:
//! no install, no network, no build. Fails on drift between manifests, on inexact or non-registry
//! RightKit pins, on stale or bypassed placeholders, and on forbidden Node test harnesses.
use crate::{read, read_json, Result};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

/// Test/UI automation harnesses that must not appear in an app (Rust `rightkit-qa` is the harness).
const FORBIDDEN_HARNESS_DEPS: [&str; 7] = [
    "puppeteer",
    "puppeteer-core",
    "playwright",
    "@playwright/test",
    "selenium-webdriver",
    "webdriverio",
    "@wdio/cli",
];
/// Node files allowed under `scripts/`: gated release-packaging placeholders only.
const ALLOWED_NODE_SCRIPTS: [&str; 2] = ["package-placeholder.mjs", "check-release-pin.mjs"];

fn field(toml: &str, key: &str) -> Option<String> {
    toml.lines().find_map(|l| {
        let l = l.trim();
        let rest = l.strip_prefix(key)?.trim_start().strip_prefix('=')?.trim();
        rest.strip_prefix('"')
            .and_then(|r| r.split('"').next())
            .map(String::from)
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
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

/// The version of `dep` that rightkit-versions.json lists in `section` (`npm` or `cargo`), searched in
/// the registry snapshot first, then the published list, then the staged list. Staged means built but
/// not on the registry, so a staged-only version is listed (a placeholder must yield to it) but is never
/// the version a pin may name: pin checks pass `include_staged = false`, placeholder checks pass `true`.
fn listed_version(m: &Value, section: &str, dep: &str, include_staged: bool) -> Option<String> {
    let staged = format!("staged{}", section[..1].to_uppercase() + &section[1..]);
    let staged_hit = if include_staged {
        m[staged.as_str()][dep].as_str()
    } else {
        None
    };
    [
        m["registry"][section][dep].as_str(),
        m[section][dep].as_str(),
        staged_hit,
    ]
    .into_iter()
    .flatten()
    .next()
    .map(String::from)
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
    need(
        tauri["version"] == version.as_str(),
        format!(
            "tauri.conf version {} != package.json {version}",
            tauri["version"]
        ),
    );
    for (label, toml) in [
        ("src-tauri", &shell_toml),
        ("engine", &engine_toml),
        ("protocol", &protocol_toml),
    ] {
        need(
            field(toml, "version").as_deref() == Some(version.as_str()),
            format!("{label}/Cargo.toml version != {version}"),
        );
    }
    need(
        field(&shell_toml, "name").as_deref() == Some(name.as_str()),
        "src-tauri package name != package.json name".into(),
    );
    need(
        tauri["mainBinaryName"].as_str() == field(after(&shell_toml, "[[bin]]"), "name").as_deref(),
        "mainBinaryName != [[bin]] name".into(),
    );
    need(
        tauri["productName"]
            .as_str()
            .map(|p| p.to_lowercase())
            .as_deref()
            == Some(name.as_str()),
        "productName lowercased != package name".into(),
    );
    need(
        tauri["identifier"]
            .as_str()
            .map(|i| i.ends_with(&format!(".{name}")))
            .unwrap_or(false),
        "identifier must end with .<package name>".into(),
    );

    // 2. Sidecar wiring: externalBin <-> engine bin <-> the shell that spawns it (src-tauri/src/lib.rs).
    let engine_bin = field(after(&engine_toml, "[[bin]]"), "name").unwrap_or_default();
    let ext: Vec<&str> = tauri["bundle"]["externalBin"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    need(
        ext == [format!("binaries/{engine_bin}").as_str()],
        format!("externalBin must be binaries/{engine_bin}"),
    );
    need(
        read(root, "src-tauri/src/lib.rs")?.contains(&engine_bin),
        "src-tauri/src/lib.rs does not name the engine binary (the supervisor spawns it)".into(),
    );

    // 2b. Engine staging is two top-level managed calls per profile: build, then stage. A nested broker
    // call is refused (exit 125), so the scripts must be exactly this shape and nothing else may name the
    // retired one-call builder or a raw rustc query.
    let tools_pkg = field(&read(root, "crates/tools/Cargo.toml")?, "name").unwrap_or_default();
    let engine_pkg = field(&engine_toml, "name").unwrap_or_default();
    need(
        read(root, "Cargo.toml")?.contains("[profile.release-iterate]"),
        "Cargo.toml has no [profile.release-iterate] (the dev engine profile)".into(),
    );
    for (script, flags) in [
        ("build:engine", "--release"),
        ("build:engine:dev", "--profile release-iterate"),
    ] {
        let want = engine_script(&engine_pkg, &tools_pkg, flags);
        need(
            pkg["scripts"][script].as_str() == Some(want.as_str()),
            format!("scripts.{script} must be exactly: {want}"),
        );
    }
    for (key, cmd) in pkg["scripts"].as_object().into_iter().flatten() {
        let cmd = cmd.as_str().unwrap_or("");
        need(
            !cmd.contains("build-engine") && !cmd.contains("rustright rustc"),
            format!("script {key} nests a retired or broker-nesting engine call ({cmd})"),
        );
    }
    need(
        tauri["build"]["beforeDevCommand"]
            .as_str()
            .is_some_and(|c| c.contains("build:engine")),
        "tauri build.beforeDevCommand must stage the engine through build:engine".into(),
    );
    need(
        tauri["build"]["beforeBuildCommand"]
            .as_str()
            .is_some_and(|c| c.contains("build:engine")),
        "tauri build.beforeBuildCommand must stage the engine through build:engine".into(),
    );

    // 3. Dev server port agrees: tauri devUrl, vite, package dev script.
    let dev_url = tauri["build"]["devUrl"].as_str().unwrap_or("");
    let port = dev_url
        .rsplit(':')
        .next()
        .unwrap_or("")
        .trim_end_matches('/')
        .to_string();
    need(
        read(root, "vite.config.ts")?.contains(&format!("port: {port}")),
        "vite port != tauri devUrl port".into(),
    );
    need(
        pkg["scripts"]["dev"]
            .as_str()
            .map(|d| d.contains(&format!("--port {port}")))
            .unwrap_or(false),
        "package dev script port != tauri devUrl port".into(),
    );
    need(
        tauri["build"]["frontendDist"] == "../dist"
            && read(root, "vite.config.ts")?.contains("outDir: \"dist\""),
        "frontendDist != vite outDir".into(),
    );

    // 4. Capabilities cover every registered plugin.
    let lib = read(root, "src-tauri/src/lib.rs")?;
    let perms = read_json(root, "src-tauri/capabilities/default.json")?["permissions"].to_string();
    for (plugin, perm) in [
        ("tauri_plugin_updater", "updater:"),
        ("tauri_plugin_process", "process:"),
        ("tauri_plugin_os", "os:"),
    ] {
        need(
            !lib.contains(plugin) || perms.contains(perm),
            format!("capability missing {perm}* for {plugin}"),
        );
    }

    // 5. RightKit pins: exact, registry-only, equal to the published version when the manifest is reachable.
    let manifest: Option<Value> = std::fs::read_to_string(
        root.join("..")
            .join("packages")
            .join("release")
            .join("rightkit-versions.json"),
    )
    .ok()
    .and_then(|t| serde_json::from_str(&t).ok());
    let mut deps: BTreeMap<String, String> = BTreeMap::new();
    for section in ["dependencies", "devDependencies"] {
        for (k, v) in pkg[section].as_object().into_iter().flatten() {
            deps.insert(k.clone(), v.as_str().unwrap_or("").to_string());
        }
    }
    let no_placeholders = serde_json::Map::new();
    let ph_npm = placeholders["npm"].as_object().unwrap_or(&no_placeholders);
    let ph_cargo = placeholders["cargo"]
        .as_object()
        .unwrap_or(&no_placeholders);
    for (dep, spec) in deps.iter().filter(|(n, _)| n.starts_with("@rightkit/")) {
        need(
            exact_registry(spec),
            format!("{dep} must be an exact registry version, got {spec}"),
        );
        need(
            !ph_npm.contains_key(dep),
            format!(
                "{dep} is listed as a placeholder; remove it from placeholders.json when adopting"
            ),
        );
        if let Some(m) = &manifest {
            match listed_version(m, "npm", dep, false) {
                None => need(false, format!("{dep} unknown to rightkit-versions.json")),
                Some(want) => need(
                    want == *spec,
                    format!("{dep} pin {spec} != published {want} in rightkit-versions.json"),
                ),
            }
        }
    }
    for spec in deps.values() {
        let bad = [
            "file:",
            "link:",
            "workspace:",
            "git:",
            "github:",
            "npm:",
            "http://",
            "https://",
        ]
        .iter()
        .any(|p| spec.starts_with(p));
        need(!bad, format!("non-registry spec {spec}"));
    }
    let all_cargo: String = [&shell_toml, &engine_toml, &protocol_toml]
        .iter()
        .flat_map(|t| active_lines(t))
        .collect::<Vec<_>>()
        .join("\n");
    for line in all_cargo.lines() {
        let Some((dep, rest)) = line.split_once('=') else {
            continue;
        };
        let dep = dep.trim();
        if !dep.starts_with("rightkit-") {
            continue;
        }
        let rest = rest.trim();
        need(
            !rest.contains("path") && !rest.contains("git"),
            format!("{dep}: rightkit crates must not use path/git deps"),
        );
        let spec = rest.trim_matches('"');
        let spec = if rest.starts_with('{') {
            rest.split("version")
                .nth(1)
                .and_then(|v| v.split('"').nth(1))
                .unwrap_or("")
        } else {
            spec
        };
        need(
            spec.starts_with('=') && exact_registry(&spec[1..]),
            format!("{dep} must be pinned with =x.y.z, got {spec}"),
        );
        need(
            !ph_cargo.contains_key(dep),
            format!(
                "{dep} is listed as a placeholder; remove it from placeholders.json when adopting"
            ),
        );
        if let Some(m) = &manifest {
            let want = listed_version(m, "cargo", dep, false);
            need(
                want.as_deref() == spec.strip_prefix('='),
                format!("{dep} pin {spec} != published {want:?} in rightkit-versions.json"),
            );
        }
    }

    // 5b. Tauri plugin crates pin exactly to their npm counterparts (one Tauri plugin generation per app).
    for line in active_lines(&shell_toml) {
        let Some((dep, rest)) = line.split_once('=') else {
            continue;
        };
        let dep = dep.trim();
        let Some(plugin) = dep.strip_prefix("tauri-plugin-") else {
            continue;
        };
        let npm_name = format!("@tauri-apps/plugin-{plugin}");
        let crate_spec = rest
            .trim()
            .trim_start_matches('{')
            .split('"')
            .nth(1)
            .unwrap_or("");
        let npm_spec = deps.get(&npm_name).map(String::as_str).unwrap_or("");
        need(exact_registry(npm_spec), format!("{npm_name} must be declared at an exact version (the counterpart of {dep}), got {npm_spec:?}"));
        need(
            crate_spec.strip_prefix('=') == Some(npm_spec) && crate_spec.starts_with('='),
            format!(
                "{dep} must be pinned =x.y.z equal to {npm_name} {npm_spec}, got {crate_spec:?}"
            ),
        );
    }

    // 6. Placeholders are honest. A package that rightkit-versions.json lists at all (published or staged)
    // has shipped: adopt it (pin exactly, delete its stand-in, delete the entry). Each stand-in file that
    // is named must still exist while its entry does.
    for (section, entries) in [("npm", ph_npm), ("cargo", ph_cargo)] {
        for (dep, entry) in entries {
            if let Some(stand_in) = entry["standIn"]
                .as_str()
                .filter(|s| s.contains('/') || s.contains('.'))
            {
                if !stand_in.contains(' ') {
                    need(root.join(stand_in).exists(), format!("placeholder {dep}: stand-in {stand_in} is gone; delete the placeholders.json entry"));
                }
            }
            if let Some(m) = &manifest {
                need(listed_version(m, section, dep, true).is_none(), format!("{dep} is listed in rightkit-versions.json: pin it exactly, delete its stand-in, and drop the placeholder"));
            }
        }
    }

    // 7. Every @rightkit import is a declared dependency.
    let mut files = vec![];
    collect(&root.join("src"), &mut files, &["ts", "tsx"]);
    for f in files {
        let text = std::fs::read_to_string(&f).map_err(|e| e.to_string())?;
        for line in text.lines().filter(|l| !l.trim_start().starts_with("//")) {
            if let Some(rest) = line
                .split("from \"@rightkit/")
                .nth(1)
                .or_else(|| line.split("from '@rightkit/").nth(1))
            {
                let pkg_name = format!(
                    "@rightkit/{}",
                    rest.split(['/', '"', '\'']).next().unwrap_or("")
                );
                need(
                    deps.contains_key(&pkg_name),
                    format!("{} imports undeclared {pkg_name}", f.display()),
                );
            }
        }
    }

    // 8. No Node test harness: Rust `rightkit-qa` is the only e2e path.
    for dep in deps.keys() {
        need(
            !FORBIDDEN_HARNESS_DEPS.contains(&dep.as_str()),
            format!("{dep} is a forbidden test harness dependency; drive the app with rightkit-qa"),
        );
    }
    for (key, cmd) in pkg["scripts"].as_object().into_iter().flatten() {
        let cmd = cmd.as_str().unwrap_or("");
        let packaging_placeholder =
            key.starts_with("rightkit:package:") || key == "check:release-pin";
        need(
            packaging_placeholder || !(cmd.starts_with("node ") || cmd.contains("&& node ")),
            format!("script {key} runs Node ({cmd}); use rightapp-tools or rightkit-qa"),
        );
    }
    let mut node_files = vec![];
    collect(
        &root.join("scripts"),
        &mut node_files,
        &["mjs", "js", "cjs"],
    );
    collect(
        &root.join("tests"),
        &mut node_files,
        &["mjs", "js", "cjs", "ts"],
    );
    for f in node_files {
        let n = f.file_name().and_then(|n| n.to_str()).unwrap_or("");
        need(
            ALLOWED_NODE_SCRIPTS.contains(&n),
            format!("{} is a Node helper/harness; write it in Rust", f.display()),
        );
    }
    let qa = std::fs::read_to_string(root.join("rightkit-qa.toml")).unwrap_or_default();
    need(
        field(&qa, "app").as_deref() == Some(name.as_str()),
        "rightkit-qa.toml app != package name".into(),
    );
    let mut scenarios = vec![];
    collect(
        &root.join("tests").join("scenarios"),
        &mut scenarios,
        &["toml"],
    );
    need(
        !scenarios.is_empty(),
        "tests/scenarios has no *.scenario.toml (engine black-box scenarios are required)".into(),
    );

    // 9. pnpm release-age exclusions list exactly the pinned freshly published @rightkit versions.
    let ws = read(root, "pnpm-workspace.yaml")?;
    for dep in [
        "@rightkit/release",
        "@rightkit/app-shell",
        "@rightkit/logs",
        "@rightkit/platform-ui",
        "@rightkit/tauri",
        "@rightkit/updates",
    ] {
        if let Some(spec) = deps.get(dep) {
            need(
                ws.contains(&format!("'{dep}@{spec}'")),
                format!("pnpm-workspace.yaml minimumReleaseAgeExclude missing {dep}@{spec}"),
            );
        }
    }

    // 9b. Warn (not fail) when an exclusion names an exact version the lockfile no longer contains: once
    // the install rewrote the lockfile with the new pin, the old exclusion is stale (README step 4).
    if let Ok(lock) = read(root, "pnpm-lock.yaml") {
        let excl = ws.split("minimumReleaseAgeExclude:").nth(1).unwrap_or("");
        for line in excl
            .lines()
            .take_while(|l| l.trim().is_empty() || l.starts_with("  -"))
        {
            let entry = line
                .trim()
                .trim_start_matches('-')
                .trim()
                .trim_matches('\'')
                .trim_matches('"');
            // Only exact `name@version`; ranges (`||`) and blanks are not checked.
            if entry.is_empty()
                || entry.contains(' ')
                || entry.contains('|')
                || entry.rfind('@').map_or(true, |i| i == 0)
            {
                continue;
            }
            if !lock.contains(&format!("{entry}'")) && !lock.contains(&format!("{entry}:")) {
                eprintln!("WARN minimumReleaseAgeExclude {entry} is not in pnpm-lock.yaml; remove it once the install has rewritten the lockfile with the new pin");
            }
        }
    }

    // 10a. right-release dev builds the release-iterate engine: its dev build overrides beforeBuildCommand.
    need(read(root, "right-release.config.mjs")?.contains("pnpm build:engine:dev"), "right-release.config.mjs dev builds must override beforeBuildCommand to use pnpm build:engine:dev".into());
    // 10. Release config names this app (right-release owns the rest and validates it itself).
    let release = read(root, "right-release.config.mjs")?;
    need(
        release.contains(&format!("app: \"{name}\"")),
        format!("right-release.config.mjs app != {name}"),
    );

    if !problems.is_empty() {
        for p in &problems {
            eprintln!("FAIL {p}");
        }
        return Err(format!("{} template check(s) failed", problems.len()));
    }
    println!(
        "template manifests consistent ({} npm pins, {})",
        deps.keys().filter(|n| n.starts_with("@rightkit/")).count(),
        if manifest.is_some() {
            "checked against rightkit-versions.json"
        } else {
            "version manifest not reachable"
        }
    );
    Ok(())
}

/// The one script shape for an engine profile: a managed build, then a managed stage. Two top-level calls.
fn engine_script(engine: &str, tools: &str, flags: &str) -> String {
    format!("rustright cargo build --locked --manifest-path Cargo.toml -p {engine} {flags} && rustright cargo run -q --manifest-path Cargo.toml -p {tools} -- stage-engine {flags}")
}

fn collect(dir: &Path, out: &mut Vec<std::path::PathBuf>, exts: &[&str]) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            if !matches!(
                e.file_name().to_str(),
                Some("node_modules" | "target" | "gen")
            ) {
                collect(&p, out, exts);
            }
        } else if p
            .extension()
            .and_then(|x| x.to_str())
            .map(|x| exts.contains(&x))
            .unwrap_or(false)
        {
            out.push(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_staged_only_version_is_never_a_pinnable_version() {
        let m = json!({"registry": {"npm": {}}, "npm": {"a": "1.0.0"}, "stagedNpm": {"a": "2.0.0", "b": "3.0.0"}});
        assert_eq!(
            listed_version(&m, "npm", "a", false).as_deref(),
            Some("1.0.0")
        );
        assert_eq!(listed_version(&m, "npm", "b", false), None);
    }

    #[test]
    fn a_staged_only_version_still_counts_as_listed_for_placeholders() {
        let m = json!({"stagedCargo": {"rightkit-x": "0.2.0"}});
        assert_eq!(
            listed_version(&m, "cargo", "rightkit-x", true).as_deref(),
            Some("0.2.0")
        );
    }
}
