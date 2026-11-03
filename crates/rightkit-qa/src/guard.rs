//! The guard that keeps the in-app QA control surface out of builds that must not carry it.
//!
//! Two backends are understood: `control` (the in-app `rightkit-qa` `control` server (or the legacy `rightkit-qa-driver` shim),
//! the default) and `wdio` (the legacy `tauri-plugin-wdio-webdriver`, kept so an
//! app mid-migration still validates).
//!
//! The rule: the server is compiled only behind an explicit `qa-native` cargo feature.
//! `qa-native` is off by default and only dev lanes enable it. A dev build may contain the
//! server (it may use a release-derived profile).
//! A release or update build must not contain it; `right-release` refuses to seal one that does.
//!
//! [`validate`] judges a [`ContractInput`] (Cargo.toml, Rust source, package scripts).
//! [`validate_app`] adds what needs the app's other files: `tauri.conf.json` (the dev server, the
//! hub window label) and `switches.json` (declared env keys). `rightkit-qa guard` runs it, with or
//! without a config.
use crate::util::{err, Error, Result};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Control,
    Wdio,
}

impl Backend {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "control" => Some(Backend::Control),
            "wdio" => Some(Backend::Wdio),
            _ => None,
        }
    }
    fn dependency(self) -> &'static str {
        match self {
            Backend::Control => "rightkit-qa-driver",
            Backend::Wdio => "tauri-plugin-wdio-webdriver",
        }
    }
    fn registration(self) -> &'static str {
        match self {
            Backend::Control => "rightkit_qa_driver::embedded::Control",
            Backend::Wdio => "tauri_plugin_wdio_webdriver::init",
        }
    }
    fn runtime_flag(self) -> &'static str {
        match self {
            Backend::Control => "RIGHTKIT_CONTROL_SERVICE",
            Backend::Wdio => "RIGHTKIT_QA_NATIVE",
        }
    }
}

pub struct ContractInput<'a> {
    pub cargo_toml: &'a str,
    pub rust_source: &'a str,
    pub package_scripts: &'a BTreeMap<String, String>,
    pub backend: Backend,
}

/// Compiled-in gate that accepts a registration: the `qa-native` feature alone. Matched against
/// whitespace-free source text.
const QA_NATIVE_GATES: [&str; 1] = [r#"#[cfg(feature="qa-native")]"#];

/// Feature items that switch on rightkit-tauri-shell developer inspection. None may be reachable from
/// `default`: a release build must never contain it (`packages/release` refuses it as well).
const DEV_INSPECT_FEATURES: [&str; 6] = [
    "dev-inspect",
    "rightkit-tauri-shell/dev-inspect",
    "rightkit-tauri-shell?/dev-inspect",
    // Apps still on the old published name (rightkit-shell) stay covered until they migrate.
    "rightkit-shell/dev-inspect",
    "rightkit-shell?/dev-inspect",
    "dev-native",
];

fn compact(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Text of `name = { ... }` (inline table) or the `[dependencies.name]` table.
fn dependency_body(cargo: &str, name: &str) -> Option<String> {
    let mut search = 0;
    while let Some(i) = cargo[search..].find(name) {
        let at = search + i;
        search = at + name.len();
        let prev = if at == 0 {
            b' '
        } else {
            cargo.as_bytes()[at - 1]
        };
        let before_ok = !prev.is_ascii_alphanumeric() && prev != b'-' && prev != b'_';
        let rest = cargo[at + name.len()..].trim_start();
        if before_ok && rest.starts_with('=') {
            let rest = rest[1..].trim_start();
            if let Some(body) = rest.strip_prefix('{') {
                return body.find('}').map(|e| body[..e].to_string());
            }
            return Some(String::new());
        }
    }
    cargo.find(&format!("[dependencies.{name}]")).map(|i| {
        let tail = &cargo[i..];
        let end = tail[1..].find("\n[").map(|e| e + 1).unwrap_or(tail.len());
        tail[..end].to_string()
    })
}

/// Text of the `[features]` table, up to the next table header.
fn features_table(cargo: &str) -> &str {
    let Some(start) = cargo.find("[features]") else {
        return "";
    };
    let body = &cargo[start + "[features]".len()..];
    let end = body.find("\n[").unwrap_or(body.len());
    &body[..end]
}

/// Every `name = [ ... ]` entry of the `[features]` table, as (name, list body). A list may
/// continue over several lines.
fn feature_entries(cargo: &str) -> Vec<(String, String)> {
    let table = features_table(cargo);
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < table.len() {
        let line_end = table[pos..].find('\n').map_or(table.len(), |i| pos + i + 1);
        let trimmed = table[pos..line_end].trim();
        if let Some((key, value)) = trimmed.split_once('=') {
            let key = key.trim();
            if let Some(list) = value.trim_start().strip_prefix('[') {
                if !key.is_empty() && !key.starts_with('#') {
                    let mut body = list.to_string();
                    let mut end = line_end;
                    while !body.contains(']') && end < table.len() {
                        let next = table[end..].find('\n').map_or(table.len(), |i| end + i + 1);
                        body.push_str(&table[end..next]);
                        end = next;
                    }
                    let close = body.find(']').unwrap_or(body.len());
                    out.push((key.to_string(), body[..close].to_string()));
                    pos = end;
                    continue;
                }
            }
        }
        pos = line_end;
    }
    out
}

/// Items of a feature list body such as `"dep:rightkit-qa-driver", "rightkit-qa-driver/qa-control"`.
fn list_items(list: &str) -> Vec<String> {
    list.split(',')
        .map(|s| s.trim().trim_matches('"').trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Every item the feature `start` enables, directly or through other features: the items of its
/// list, plus the items of each listed feature, and so on. `start` itself appears only when a
/// cycle leads back to it.
fn feature_closure(entries: &[(String, String)], start: &str) -> BTreeSet<String> {
    let mut stack: Vec<String> = entries
        .iter()
        .filter(|(k, _)| k.as_str() == start)
        .flat_map(|(_, v)| list_items(v))
        .collect();
    let mut seen = BTreeSet::new();
    while let Some(item) = stack.pop() {
        if !seen.insert(item.clone()) {
            continue;
        }
        if let Some((_, list)) = entries.iter().find(|(k, _)| k.as_str() == item.as_str()) {
            stack.extend(list_items(list));
        }
    }
    seen
}

/// Whether feature `start`, directly or through other features, enables `target`.
fn feature_reaches(entries: &[(String, String)], start: &str, target: &str) -> bool {
    feature_closure(entries, start).contains(target)
}

/// Whether `default`, directly or through other features, enables `target`.
fn default_reaches(entries: &[(String, String)], target: &str) -> bool {
    feature_reaches(entries, "default", target)
}

/// False for a `use` line or a comment line. Those name the registration path but do not
/// register anything, so they need no gate.
fn is_code_at(src: &str, at: usize) -> bool {
    let line_start = src[..at].rfind('\n').map_or(0, |p| p + 1);
    let line = src[line_start..].lines().next().unwrap_or("").trim_start();
    !(line.starts_with("use ")
        || line.starts_with("pub use ")
        || line.starts_with("pub(crate) use ")
        || line.starts_with("//"))
}

/// Byte offset of the `{` that opens the innermost block enclosing `end`, skipping the balanced
/// `{ ... }` groups of earlier sibling statements. `None` at top level.
fn enclosing_open_brace(src: &str, end: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, b) in src.as_bytes()[..end].iter().enumerate().rev() {
        match b {
            b'}' => depth += 1,
            b'{' if depth == 0 => return Some(i),
            b'{' => depth -= 1,
            _ => {}
        }
    }
    None
}

/// Whether the code at `at` sits under an accepted qa-native gate. The gate may sit on the
/// statement itself, or on an enclosing function or block, so the walk checks the statement head
/// (back to the previous `;`, `{` or `}`) and then moves outward to the enclosing `{`, past any
/// statements that precede the call inside the same block, up to a bounded nesting depth.
fn is_gated(src: &str, at: usize) -> bool {
    let mut end = at;
    for _ in 0..8 {
        let head_start = src[..end].rfind([';', '{', '}']).map_or(0, |p| p + 1);
        let head = compact(&src[head_start..end]);
        if QA_NATIVE_GATES.iter().any(|gate| head.contains(*gate)) {
            return true;
        }
        match enclosing_open_brace(src, end) {
            Some(p) => end = p,
            None => return false,
        }
    }
    false
}

/// The control-server dependency an app uses: `(dependency, feature, registration path)`.
/// An app on `rightkit-qa` (feature `control`) is validated against that shape; the legacy
/// `rightkit-qa-driver` (feature `qa-control`) shape stays accepted for apps not yet migrated.
fn control_target(cargo: &str) -> (&'static str, &'static str, &'static str) {
    if dependency_body(cargo, "rightkit-qa-driver").is_none()
        && dependency_body(cargo, "rightkit-qa").is_some()
    {
        ("rightkit-qa", "control", "rightkit_qa::embedded::Control")
    } else {
        (
            "rightkit-qa-driver",
            "qa-control",
            "rightkit_qa_driver::embedded::Control",
        )
    }
}

pub fn validate(input: &ContractInput) -> Vec<String> {
    let mut issues = vec![];
    let control = input.backend == Backend::Control;
    let (dep, qa_feat, registration) = if control {
        control_target(input.cargo_toml)
    } else {
        (
            input.backend.dependency(),
            "qa-control",
            input.backend.registration(),
        )
    };
    let dep_body = dependency_body(input.cargo_toml, dep).map(|b| compact(&b));
    let dep_enables_qa_control = dep_body
        .as_deref()
        .is_some_and(|b| b.contains(&format!("\"{qa_feat}\"")));
    match &dep_body {
        None => issues.push(format!(
            "Cargo.toml must declare {dep} under [dependencies] as an optional dependency: \
             `{dep} = {{ version = \"...\", optional = true, default-features = false }}`"
        )),
        Some(body) => {
            if !body.contains("optional=true") {
                issues.push(format!(
                    "{dep} must be optional: declare it with `optional = true` so that only \
                     the qa-native feature compiles the QA server into the app"
                ));
                if control && dep_enables_qa_control {
                    issues.push(format!(
                        "{dep} enables the qa-control feature without optional = true, so the \
                         QA server compiles into every build: make the dependency optional and \
                         enable qa-control only from qa-native"
                    ));
                }
            }
        }
    }

    let entries = feature_entries(input.cargo_toml);
    let dep_feature = format!("dep:{dep}");
    let qa_control_feature = format!("{dep}/{qa_feat}");
    match entries.iter().find(|(k, _)| k.as_str() == "qa-native") {
        None => issues.push(format!(
            "Cargo.toml must define the qa-native feature under [features], for example \
             `qa-native = [\"dep:{dep}\", \"{dep}/{qa_feat}\"]`"
        )),
        Some((_, list)) => {
            let items = list_items(list);
            if !items.iter().any(|i| i.as_str() == dep_feature.as_str()) {
                issues.push(format!("qa-native must enable {dep_feature}"));
            }
            let enables_control_qa = items
                .iter()
                .any(|i| i.as_str() == qa_control_feature.as_str());
            if control && !enables_control_qa && !dep_enables_qa_control {
                issues.push(format!(
                    "qa-native must enable {qa_control_feature} (add it to the qa-native \
                     list, or list {qa_feat} in the {dep} dependency's features); \
                     without it the embedded server is not compiled"
                ));
            }
        }
    }
    if default_reaches(&entries, "qa-native") {
        issues.push(
            "qa-native must not be enabled by default: remove it from `default` (directly or \
             through another default feature) so that only `--features qa-native` builds \
             contain the QA server"
                .into(),
        );
    }
    for target in DEV_INSPECT_FEATURES {
        if default_reaches(&entries, target) {
            issues.push(format!(
                "{target} must not be enabled by default: it compiles developer inspection \
                 (rightkit-tauri-shell dev-inspect) into the app, and right-release refuses to seal \
                 or upload such a build. Remove it from `default` (directly or through another \
                 default feature) so that only an explicit dev build contains it"
            ));
        }
    }
    // The crate was renamed from rightkit-shell; apps still on the old published name stay covered.
    for shell_crate in ["rightkit-tauri-shell", "rightkit-shell"] {
        if let Some(body) = dependency_body(input.cargo_toml, shell_crate).map(|b| compact(&b)) {
            if body.contains("\"dev-inspect\"") && !body.contains("optional=true") {
                issues.push(format!(
                    "{shell_crate} lists the dev-inspect feature without optional = true, so \
                     developer inspection compiles into every build: enable it only from an \
                     optional dev feature such as `dev-native = [\"{shell_crate}/dev-inspect\"]`"
                ));
            }
        }
    }
    for (name, list) in &entries {
        if name.as_str() == "qa-native" {
            continue;
        }
        let items = list_items(list);
        if items.iter().any(|i| {
            i.as_str() == dep_feature.as_str() || i.as_str() == qa_control_feature.as_str()
        }) {
            issues.push(format!(
                "feature {name} enables {dep}; only qa-native may enable the QA server \
                 dependency, so remove it from {name}"
            ));
        }
    }

    let sites: Vec<usize> = input
        .rust_source
        .match_indices(registration)
        .map(|(at, _)| at)
        .filter(|&at| is_code_at(input.rust_source, at))
        .collect();
    if sites.is_empty() {
        issues.push(format!(
            "the app must register {dep} in its Rust source: `{registration}` inside \
             `#[cfg(feature = \"qa-native\")]`"
        ));
    }
    let ungated: Vec<usize> = sites
        .iter()
        .copied()
        .filter(|&at| !is_gated(input.rust_source, at))
        .collect();
    if let Some(&first) = ungated.first() {
        let line = input.rust_source[..first].matches('\n').count() + 1;
        issues.push(format!(
            "{dep} registration `{registration}` is unconditional ({} place(s), first at line \
             {line} of the Rust sources): gate each registration statement, or the function or \
             block that contains it, with `#[cfg(feature = \"qa-native\")]` so that a build \
             without the qa-native feature does not contain the QA server",
            ungated.len()
        ));
    }

    let rust_compact = compact(input.rust_source);
    let runtime_gated = rust_compact.contains(input.backend.runtime_flag())
        || (control && rust_compact.contains("build_if_enabled"));
    if !runtime_gated {
        issues.push(format!(
            "registration must also require the {} runtime flag",
            input.backend.runtime_flag()
        ));
    }

    for (name, command) in input.package_scripts {
        if command.contains("dev-inspect") || command.contains("dev-native") {
            let lower = name.to_lowercase();
            let dev_lane = ["qa", "test", "dev"]
                .iter()
                .any(|p| name == p || name.starts_with(&format!("{p}:")));
            if !dev_lane
                && ["release", "publish", "dmg", "installer", "package"]
                    .iter()
                    .any(|w| lower.contains(w))
            {
                issues.push(format!(
                    "package script {name} must not enable dev-inspect or dev-native; only \
                     qa, test or dev scripts may"
                ));
            }
        }
        if !command.contains("qa-native") {
            continue;
        }
        if ["qa", "test", "dev"]
            .iter()
            .any(|p| name == p || name.starts_with(&format!("{p}:")))
        {
            continue;
        }
        let lower = name.to_lowercase();
        if ["release", "publish", "build", "dmg", "installer", "package"]
            .iter()
            .any(|w| lower.contains(w))
            || command.contains("tauri build")
        {
            issues.push(format!(
                "package script {name} must not enable qa-native; only qa, test or dev scripts may"
            ));
        }
    }
    issues
}

pub fn assert_contract(input: &ContractInput) -> Result<()> {
    let issues = validate(input);
    if issues.is_empty() {
        Ok(())
    } else {
        err(format!(
            "native QA contract failed:\n- {}",
            issues.join("\n- ")
        ))
    }
}

/// Everything the guard knows about one app: the [`ContractInput`] plus the files and
/// declarations that only some apps have. `rightkit-qa guard` builds it; a caller that holds only
/// a [`ContractInput`] keeps using [`validate`], whose result is unchanged.
pub struct AppContract<'a> {
    pub contract: ContractInput<'a>,
    /// Text of the app's `tauri.conf.json`, when it has one.
    pub tauri_conf: Option<&'a str>,
    /// `[ui] window_label` when the config names one other than the default `main`.
    pub declared_window_label: Option<&'a str>,
    /// `[ui] env_keys`: the app env keys a launch may carry.
    pub declared_env_keys: &'a [String],
    /// Text of the app's `switches.json`, when it has one.
    pub switches: Option<&'a str>,
}

/// Selector classes of `switches.json` that a release build must not contain
/// (`packages/release/release-switch-scan.mjs`: S1 test activation, S2 authority or identity,
/// S3 executable, library or model, S4 state or data root). The release binary scan proves a
/// name of these classes absent from shipped binaries; S5 and S6 names are allowed to ship.
const RELEASE_FORBIDDEN_CLASSES: [&str; 4] = ["S1", "S2", "S3", "S4"];

/// Feature items that turn on tauri's `custom-protocol` feature, which makes a build load the
/// bundled frontend instead of `build.devUrl`.
const CUSTOM_PROTOCOL_ITEMS: [&str; 2] = ["tauri/custom-protocol", "tauri?/custom-protocol"];

/// `build.devUrl` of a Tauri 2 config, when it is a non-empty string.
fn dev_url_of(conf: &Value) -> Option<String> {
    conf.pointer("/build/devUrl")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .map(str::to_string)
}

/// A `qa-native` build of an app with `build.devUrl` loads that dev server unless tauri's
/// `custom-protocol` feature is on. It is on when the `qa-native` feature closure reaches it, or
/// when the tauri dependency lists it in its own features. Applies only when `qa-native` exists
/// (its absence is a finding of [`validate`]).
fn custom_protocol_issue(cargo: &str, dev_url: &str) -> Option<String> {
    let entries = feature_entries(cargo);
    if !entries.iter().any(|(k, _)| k.as_str() == "qa-native") {
        return None;
    }
    let closure = feature_closure(&entries, "qa-native");
    let from_feature = CUSTOM_PROTOCOL_ITEMS.iter().any(|i| closure.contains(*i));
    let from_dependency = dependency_body(cargo, "tauri")
        .is_some_and(|body| compact(&body).contains("\"custom-protocol\""));
    if from_feature || from_dependency {
        return None;
    }
    Some(format!(
        "QA would load the dev server at {dev_url}: tauri.conf.json declares build.devUrl, and \
         a qa-native build only loads the bundled frontend when tauri's custom-protocol feature \
         is on. Add \"tauri/custom-protocol\" to the qa-native list, or to a feature it enables \
         (`custom-protocol = [\"tauri/custom-protocol\"]` listed in qa-native)"
    ))
}

/// `qa`, `test`, `e2e` and their `name:` variants: the package scripts that run QA.
fn is_qa_lane_script(name: &str) -> bool {
    ["qa", "test", "e2e"]
        .iter()
        .any(|p| name == *p || name.strip_prefix(p).is_some_and(|r| r.starts_with(':')))
}

/// The `&&`/`;`-separated segments of a shell command line.
fn command_segments(command: &str) -> Vec<String> {
    command
        .replace("&&", ";")
        .split(';')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Words of a command segment without its leading `NAME=value` environment assignments.
fn command_words(segment: &str) -> Vec<&str> {
    segment
        .split_whitespace()
        .skip_while(|w| !w.starts_with('-') && w.contains('='))
        .collect()
}

/// A segment that runs `cargo build` or `cargo run`, also through `rustright cargo`.
fn runs_cargo_build_or_run(segment: &str) -> bool {
    let words = command_words(segment);
    let rest = match words.as_slice() {
        ["cargo", rest @ ..] | ["rustright", "cargo", rest @ ..] => rest,
        _ => return false,
    };
    let subcommand = rest
        .iter()
        .copied()
        .find(|w| !w.starts_with('-') && !w.starts_with('+'));
    matches!(subcommand, Some("build" | "run"))
}

/// A segment that builds the frontend: a package manager (pnpm, npm, yarn or bun) invocation with
/// a `build` or `build:<name>` word (`pnpm build`, `pnpm run build:web`, `pnpm -C web build`,
/// `pnpm --filter x build`), or `vite build`.
fn builds_frontend(segment: &str) -> bool {
    let words = command_words(segment);
    let package_manager = words
        .first()
        .is_some_and(|w| ["pnpm", "npm", "yarn", "bun"].contains(w));
    let build_word = words
        .iter()
        .any(|w| *w == "build" || w.starts_with("build:"));
    (package_manager && build_word) || words.windows(2).any(|p| p == ["vite", "build"])
}

/// The QA package scripts (`qa`, `test`, `e2e` and `name:` variants) that name `qa-native` must
/// drive the built app, not the dev server (extends preflight G18):
/// - a script that runs `tauri dev` drives the dev server;
/// - with `build.devUrl` declared, a script that runs `cargo build` or `cargo run` must build the
///   frontend first. The heuristic is narrow on purpose: some `&&`/`;` segment before the first
///   cargo segment is a frontend build ([`builds_frontend`]). `tauri build` anywhere in the
///   command satisfies it, because it runs `beforeBuildCommand`.
fn qa_script_issues(scripts: &BTreeMap<String, String>, dev_url: Option<&str>) -> Vec<String> {
    let mut issues = vec![];
    for (name, command) in scripts {
        if !is_qa_lane_script(name) || !command.contains("qa-native") {
            continue;
        }
        let command = command.split_whitespace().collect::<Vec<_>>().join(" ");
        if command.contains("tauri dev") {
            issues.push(format!(
                "package script {name} runs `tauri dev` with qa-native: QA would drive the dev \
                 server, not the built app. Build the app instead, for example \
                 `tauri build --features qa-native`"
            ));
            continue;
        }
        let Some(url) = dev_url else { continue };
        if command.contains("tauri build") {
            continue;
        }
        let segments = command_segments(&command);
        let Some(first_cargo) = segments.iter().position(|s| runs_cargo_build_or_run(s)) else {
            continue;
        };
        if !segments[..first_cargo].iter().any(|s| builds_frontend(s)) {
            issues.push(format!(
                "package script {name} compiles the app with cargo without building the frontend \
                 first, and tauri.conf.json declares build.devUrl ({url}): QA would run a missing \
                 or stale frontend, or the dev server. Build the frontend before cargo \
                 (`pnpm build && cargo build --features qa-native`) or use \
                 `tauri build --features qa-native`"
            ));
        }
    }
    issues
}

/// `source` without its comment lines, whitespace removed.
fn compact_code(source: &str) -> String {
    let code: Vec<&str> = source
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect();
    compact(&code.join("\n"))
}

/// The control server drives the window labelled `main` unless the app registers it with
/// `.window("<label>")` or the config sets `[ui] window_label`. The hub window is the first of
/// `app.windows` (label `main` when it has none); when that label is not `main`, one of the two
/// must name it. A declared label must be the label of some window.
fn window_label_issues(conf: &Value, rust_source: &str, declared: Option<&str>) -> Vec<String> {
    let labels: Vec<&str> = conf
        .pointer("/app/windows")
        .and_then(Value::as_array)
        .map(|windows| {
            windows
                .iter()
                .map(|w| w.get("label").and_then(Value::as_str).unwrap_or("main"))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let Some(&first) = labels.first() else {
        return vec![];
    };
    let mut issues = vec![];
    // `.window("hub")`, or the same call split over lines by rustfmt, which adds a trailing comma.
    let registered = {
        let code = compact_code(rust_source);
        [
            format!(".window(\"{first}\")"),
            format!(".window(\"{first}\",)"),
        ]
        .iter()
        .any(|call| code.contains(call.as_str()))
    };
    if first != "main" && declared != Some(first) && !registered {
        issues.push(format!(
            "the control server targets the window \"main\" but the first window in \
             tauri.conf.json is \"{first}\": register Control with `.window(\"{first}\")`, or set \
             `window_label = \"{first}\"` under [ui] in rightkit-qa.toml (the config-driven \
             `ui_launch` step exports it to the app as RIGHTKIT_QA_WINDOW_LABEL; a Rust test that \
             calls `control::launch` must put RIGHTKIT_QA_WINDOW_LABEL in its `LaunchSpec::env`)"
        ));
    }
    if let Some(label) = declared {
        if !labels.contains(&label) {
            issues.push(format!(
                "[ui] window_label \"{label}\" is not the label of any window in tauri.conf.json \
                 (windows: {}): set it to one of them",
                labels.join(", ")
            ));
        }
    }
    issues
}

/// Each declared env key (`[ui] env_keys`) must be listed in `switches.json` with a release
/// forbidden class (S1 to S4), so the release binary scan proves the key absent from shipped
/// binaries. Nothing is checked when no key is declared.
fn env_key_issues(keys: &[String], switches: Option<&str>) -> Vec<String> {
    if keys.is_empty() {
        return vec![];
    }
    let Some(text) = switches else {
        return vec![format!(
            "[ui] env_keys declares {} but no switches.json was found (expected at the app root \
             or in src-tauri/): list each declared key there with class S1, S2, S3 or S4 so the \
             release binary scan proves it absent from shipped binaries",
            keys.join(", ")
        )];
    };
    let parsed: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => {
            return vec![format!(
                "switches.json is not valid JSON ({e}), so the declared env keys ({}) cannot be \
                 checked",
                keys.join(", ")
            )]
        }
    };
    let listed = parsed
        .get("switches")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut issues = vec![];
    for key in keys {
        let classes: Vec<&str> = listed
            .iter()
            .filter(|e| e.get("name").and_then(Value::as_str) == Some(key.as_str()))
            .map(|e| e.get("class").and_then(Value::as_str).unwrap_or(""))
            .collect();
        if classes
            .iter()
            .any(|c| RELEASE_FORBIDDEN_CLASSES.contains(c))
        {
            continue;
        }
        match classes.first() {
            None => issues.push(format!(
                "env key {key} is declared in [ui] env_keys but not listed in switches.json: add \
                 an entry with name \"{key}\" and class S1, S2, S3 or S4 so the release binary \
                 scan proves it absent from shipped binaries"
            )),
            Some(class) => issues.push(format!(
                "env key {key} is listed in switches.json with class {}, but a declared env key \
                 must be class S1, S2, S3 or S4 so the release binary scan proves it absent from \
                 shipped binaries",
                if class.is_empty() {
                    "(none)".to_string()
                } else {
                    format!("\"{class}\"")
                }
            )),
        }
    }
    issues
}

/// [`validate`] plus the checks that need more than a [`ContractInput`]: the custom-protocol
/// feature and QA scripts against `build.devUrl`, the window label, and the declared env keys
/// against `switches.json`. For an input with none of those, the result equals [`validate`].
pub fn validate_app(input: &AppContract) -> Vec<String> {
    let mut issues = validate(&input.contract);
    let conf: Option<Value> = input.tauri_conf.and_then(|t| serde_json::from_str(t).ok());
    let dev_url = conf.as_ref().and_then(dev_url_of);
    if let Some(url) = &dev_url {
        issues.extend(custom_protocol_issue(input.contract.cargo_toml, url));
    }
    issues.extend(qa_script_issues(
        input.contract.package_scripts,
        dev_url.as_deref(),
    ));
    if let Some(conf) = &conf {
        issues.extend(window_label_issues(
            conf,
            input.contract.rust_source,
            input.declared_window_label,
        ));
    }
    issues.extend(env_key_issues(input.declared_env_keys, input.switches));
    issues
}

pub fn assert_app_contract(input: &AppContract) -> Result<()> {
    let issues = validate_app(input);
    if issues.is_empty() {
        Ok(())
    } else {
        err(format!(
            "native QA contract failed:\n- {}",
            issues.join("\n- ")
        ))
    }
}

/// The directory of the app's Tauri crate under `app_root`: `<app_root>/src-tauri` when it holds
/// a `Cargo.toml`, else `app_root` itself when that does. The error names both paths.
pub fn tauri_crate_dir(app_root: &Path) -> Result<PathBuf> {
    let nested = app_root.join("src-tauri");
    if nested.join("Cargo.toml").is_file() {
        return Ok(nested);
    }
    if app_root.join("Cargo.toml").is_file() {
        return Ok(app_root.to_path_buf());
    }
    err(format!(
        "no Tauri crate found: neither {} nor {} exists",
        nested.join("Cargo.toml").display(),
        app_root.join("Cargo.toml").display()
    ))
}

/// Directory names the Rust source walk never enters (build output and dependencies).
const SKIPPED_SOURCE_DIRS: [&str; 2] = ["target", "node_modules"];

/// Paths (relative to `root`, `/`-separated) of every `.rs` file under `dir`.
fn collect_rust_files(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
    let read = std::fs::read_dir(dir)
        .map_err(|e| Error(format!("cannot read directory {}: {e}", dir.display())))?;
    for entry in read {
        let entry =
            entry.map_err(|e| Error(format!("cannot read directory {}: {e}", dir.display())))?;
        let path = entry.path();
        let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
        if is_dir {
            if SKIPPED_SOURCE_DIRS.iter().any(|s| entry.file_name() == *s) {
                continue;
            }
            collect_rust_files(root, &path, out)?;
        } else if path.extension().is_some_and(|e| e == "rs") && path.is_file() {
            let rel = path.strip_prefix(root).unwrap_or(&path);
            let parts: Vec<_> = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect();
            out.push(parts.join("/"));
        }
    }
    Ok(())
}

/// The text of every `.rs` file under `dir`, recursively and sorted by relative path, each
/// preceded by a `// ---- <relative path> ----` line so a finding's line can be found. Does not
/// enter `target/` or `node_modules/`. A file that does not end in a newline gets one, so each
/// separator starts its own line.
pub fn rust_sources_under(dir: &Path) -> Result<String> {
    let mut files = Vec::new();
    collect_rust_files(dir, dir, &mut files)?;
    files.sort();
    let mut out = String::new();
    for rel in &files {
        let path = dir.join(rel);
        let text = std::fs::read_to_string(&path)
            .map_err(|e| Error(format!("cannot read {}: {e}", path.display())))?;
        out.push_str("// ---- ");
        out.push_str(rel);
        out.push_str(" ----\n");
        out.push_str(&text);
        if !text.ends_with('\n') {
            out.push('\n');
        }
    }
    Ok(out)
}

pub fn scripts_from_package_json(text: &str) -> Result<BTreeMap<String, String>> {
    let v: Value = serde_json::from_str(text)?;
    Ok(v.get("scripts")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default())
}
