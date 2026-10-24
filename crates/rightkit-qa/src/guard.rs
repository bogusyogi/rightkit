//! The guard that keeps the in-app QA control surface out of builds that must not carry it.
//!
//! Two backends are understood: `control` (the in-app `rightkit-qa` `control` server (or the legacy `rightkit-qa-driver` shim),
//! the default) and `wdio` (the legacy `tauri-plugin-wdio-webdriver`, kept so an
//! app mid-migration still validates).
//!
//! The rule: the server is compiled only behind an explicit `qa-native` cargo feature.
//! `qa-native` is off by default and only dev lanes enable it. A dev build may contain the
//! server (it may use a release-derived profile, so `debug_assertions` is not required).
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

/// Compiled-in gates that accept a registration: the feature alone (the current rule), or
/// the older debug-only form. Matched against whitespace-free source text.
const QA_NATIVE_GATES: [&str; 3] = [
    r#"#[cfg(feature="qa-native")]"#,
    r#"#[cfg(all(debug_assertions,feature="qa-native"))]"#,
    r#"#[cfg(all(feature="qa-native",debug_assertions))]"#,
];

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
                 `tauri build --debug --features qa-native`"
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
                 `tauri build --debug --features qa-native`"
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
             `window_label = \"{first}\"` under [ui] in rightkit-qa.toml (rightkit-qa exports it \
             to the app as RIGHTKIT_QA_WINDOW_LABEL)"
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Shape in the README: the server is pulled in only by `qa-native`.
    const CARGO: &str = r##"
[features]
default = []
qa-native = ["dep:rightkit-qa-driver", "rightkit-qa-driver/qa-control"]
[dependencies]
rightkit-qa-driver = { version = "0.3", optional = true, default-features = false }
"##;

    /// The alternative shape: qa-control listed in the optional dependency's own features.
    const CARGO_DEP_FEATURES: &str = r##"
[features]
default = []
qa-native = ["dep:rightkit-qa-driver"]
[dependencies]
rightkit-qa-driver = { version = "0.3", optional = true, default-features = false, features = ["qa-control"] }
"##;

    /// Registration gated by the feature alone; the registration also appears in a `use` line.
    const RUST_GATED: &str = r##"
use rightkit_qa_driver::embedded::Control;
fn run() {
    #[cfg(feature = "qa-native")]
    if let Some(p) = rightkit_qa_driver::embedded::Control::<tauri::Wry>::new().build_if_enabled() { b = b.plugin(p); }
}"##;

    /// The older debug-only form, with its compile_error. Still accepted.
    const RUST_DEBUG_ONLY: &str = r##"
#[cfg(all(not(debug_assertions), feature = "qa-native"))]
compile_error!("qa-native is debug only");
fn run() {
    #[cfg(all(debug_assertions, feature = "qa-native"))]
    if let Some(p) = rightkit_qa_driver::embedded::Control::<tauri::Wry>::new().build_if_enabled() { b = b.plugin(p); }
}"##;

    /// The gate sits on the function that holds the registration.
    const RUST_FN_GATED: &str = r##"
#[cfg(feature = "qa-native")]
fn register(b: Builder) -> Builder {
    if let Some(p) = rightkit_qa_driver::embedded::Control::<tauri::Wry>::new().build_if_enabled() { b.plugin(p) } else { b }
}"##;

    const RUST_UNGATED: &str = r##"
use rightkit_qa_driver::embedded::Control;
fn run() {
    if let Some(p) = rightkit_qa_driver::embedded::Control::<tauri::Wry>::new().build_if_enabled() { b = b.plugin(p); }
}"##;

    fn check(cargo: &str, rust: &str, scripts: &[(&str, &str)], backend: Backend) -> Vec<String> {
        let scripts: BTreeMap<String, String> = scripts
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        validate(&ContractInput {
            cargo_toml: cargo,
            rust_source: rust,
            package_scripts: &scripts,
            backend,
        })
    }

    fn control(cargo: &str, rust: &str, scripts: &[(&str, &str)]) -> Vec<String> {
        check(cargo, rust, scripts, Backend::Control)
    }

    fn any_contains(issues: &[String], needle: &str) -> bool {
        issues.iter().any(|s| s.contains(needle))
    }

    #[test]
    fn accepts_qa_native_gate_without_debug_assertions() {
        let issues = control(
            CARGO,
            RUST_GATED,
            &[("qa", "cargo run --features qa-native")],
        );
        assert_eq!(issues, Vec::<String>::new());
    }

    #[test]
    fn accepts_older_debug_only_form() {
        let issues = control(
            CARGO,
            RUST_DEBUG_ONLY,
            &[("qa", "cargo run --features qa-native")],
        );
        assert_eq!(issues, Vec::<String>::new());
    }

    #[test]
    fn accepts_gate_on_enclosing_function() {
        assert_eq!(control(CARGO, RUST_FN_GATED, &[]), Vec::<String>::new());
    }

    #[test]
    fn accepts_qa_control_listed_in_dependency_features() {
        assert_eq!(
            control(CARGO_DEP_FEATURES, RUST_GATED, &[]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn accepts_wdio_backend_gated_by_qa_native() {
        let cargo = r##"
[features]
default = []
qa-native = ["dep:tauri-plugin-wdio-webdriver"]
[dependencies]
tauri-plugin-wdio-webdriver = { version = "0.1", optional = true }
"##;
        let rust = r##"
const FLAG: &str = "RIGHTKIT_QA_NATIVE";
#[cfg(feature = "qa-native")]
fn run() { b = b.plugin(tauri_plugin_wdio_webdriver::init()); }
"##;
        let issues = check(cargo, rust, &[], Backend::Wdio);
        assert_eq!(issues, Vec::<String>::new());
    }

    /// The optional dev feature that selects rightkit-tauri-shell developer inspection.
    const CARGO_DEV_INSPECT: &str = r##"
[features]
default = []
qa-native = ["dep:rightkit-qa-driver", "rightkit-qa-driver/qa-control"]
dev-native = ["rightkit-tauri-shell/dev-inspect"]
[dependencies]
rightkit-qa-driver = { version = "0.3", optional = true, default-features = false }
rightkit-tauri-shell = { version = "0.4", default-features = false }
"##;

    #[test]
    fn accepts_dev_inspect_behind_an_off_by_default_dev_feature() {
        assert_eq!(
            control(
                CARGO_DEV_INSPECT,
                RUST_GATED,
                &[("dev", "cargo run --features dev-native")]
            ),
            Vec::<String>::new()
        );
    }

    #[test]
    fn rejects_dev_inspect_in_default_features() {
        for default in [
            "default = [\"dev-native\"]",
            "default = [\"rightkit-tauri-shell/dev-inspect\"]",
            "default = [\"dev-inspect\"]",
            "default = [\"shell\"]\nshell = [\"dev-native\"]",
        ] {
            let cargo = CARGO_DEV_INSPECT.replace("default = []", default);
            let issues = control(&cargo, RUST_GATED, &[]);
            assert!(
                any_contains(&issues, "must not be enabled by default"),
                "{default}: {issues:?}"
            );
        }
    }

    #[test]
    fn rejects_non_optional_dev_inspect_dependency_and_release_scripts() {
        let cargo = CARGO_DEV_INSPECT.replace(
            "rightkit-tauri-shell = { version = \"0.4\", default-features = false }",
            "rightkit-tauri-shell = { version = \"0.4\", features = [\"dev-inspect\"] }",
        );
        let issues = control(&cargo, RUST_GATED, &[]);
        assert!(
            any_contains(&issues, "without optional = true"),
            "{issues:?}"
        );
        let issues = control(
            CARGO_DEV_INSPECT,
            RUST_GATED,
            &[("build:release", "tauri build --features dev-native")],
        );
        assert!(
            any_contains(
                &issues,
                "package script build:release must not enable dev-inspect"
            ),
            "{issues:?}"
        );
    }

    #[test]
    fn accepts_and_checks_the_rightkit_qa_control_shape() {
        let cargo = CARGO
            .replace("rightkit-qa-driver", "rightkit-qa")
            .replace("qa-control", "control");
        let rust = RUST_GATED.replace("rightkit_qa_driver", "rightkit_qa");
        assert_eq!(control(&cargo, &rust, &[]), Vec::<String>::new());
        let without = cargo.replace(", \"rightkit-qa/control\"", "");
        assert!(any_contains(
            &control(&without, &rust, &[]),
            "rightkit-qa/control"
        ));
        let eager = cargo.replace("optional = true, ", "");
        assert!(any_contains(
            &control(&eager, &rust, &[]),
            "must be optional"
        ));
    }

    #[test]
    fn rejects_unconditional_registration() {
        let issues = control(CARGO, RUST_UNGATED, &[]);
        assert!(any_contains(&issues, "is unconditional"), "{issues:?}");
    }

    #[test]
    fn rejects_negated_gate() {
        let negated = RUST_GATED.replace(
            "#[cfg(feature = \"qa-native\")]",
            "#[cfg(not(feature = \"qa-native\"))]",
        );
        let issues = control(CARGO, &negated, &[]);
        assert!(any_contains(&issues, "is unconditional"), "{issues:?}");
    }

    #[test]
    fn rejects_release_scripts_and_non_optional_dependency() {
        let cargo = CARGO.replace("optional = true, ", "");
        let issues = control(
            &cargo,
            RUST_GATED,
            &[("release", "tauri build --features qa-native")],
        );
        assert!(any_contains(&issues, "must be optional"), "{issues:?}");
        assert!(
            any_contains(&issues, "package script release"),
            "{issues:?}"
        );
    }

    #[test]
    fn rejects_non_optional_dependency_that_enables_qa_control() {
        let cargo = CARGO_DEP_FEATURES.replace("optional = true, ", "");
        let issues = control(&cargo, RUST_GATED, &[]);
        assert!(any_contains(&issues, "must be optional"), "{issues:?}");
        assert!(
            any_contains(
                &issues,
                "enables the qa-control feature without optional = true"
            ),
            "{issues:?}"
        );
    }

    #[test]
    fn rejects_qa_native_in_default_features() {
        let cargo = CARGO.replace("default = []", "default = [\"qa-native\"]");
        let issues = control(&cargo, RUST_GATED, &[]);
        assert!(
            any_contains(&issues, "must not be enabled by default"),
            "{issues:?}"
        );
    }

    #[test]
    fn rejects_qa_native_reached_through_a_default_feature() {
        let cargo = CARGO.replace(
            "default = []",
            "default = [\"full\"]\nfull = [\"qa-native\"]",
        );
        let issues = control(&cargo, RUST_GATED, &[]);
        assert!(
            any_contains(&issues, "must not be enabled by default"),
            "{issues:?}"
        );
    }

    #[test]
    fn rejects_qa_native_without_qa_control() {
        let cargo = CARGO.replace(", \"rightkit-qa-driver/qa-control\"", "");
        let issues = control(&cargo, RUST_GATED, &[]);
        assert!(
            any_contains(&issues, "rightkit-qa-driver/qa-control"),
            "{issues:?}"
        );
    }

    #[test]
    fn rejects_other_feature_that_enables_the_dependency() {
        let cargo = CARGO.replace(
            "[dependencies]",
            "browser-ui = [\"dep:rightkit-qa-driver\"]\n[dependencies]",
        );
        let issues = control(&cargo, RUST_GATED, &[]);
        assert!(
            any_contains(&issues, "feature browser-ui enables"),
            "{issues:?}"
        );
    }

    #[test]
    fn rejects_missing_qa_native_feature() {
        let cargo = CARGO.replace(
            "qa-native = [\"dep:rightkit-qa-driver\", \"rightkit-qa-driver/qa-control\"]\n",
            "",
        );
        let issues = control(&cargo, RUST_GATED, &[]);
        assert!(
            any_contains(&issues, "must define the qa-native feature"),
            "{issues:?}"
        );
    }

    #[test]
    fn rejects_missing_registration() {
        let issues = control(CARGO, "fn main(){}", &[]);
        assert!(any_contains(&issues, "must register"), "{issues:?}");
    }

    const REGISTER: &str =
        "rightkit_qa_driver::embedded::Control::<tauri::Wry>::new().build_if_enabled()";

    #[test]
    fn accepts_registration_after_another_statement_in_a_gated_block() {
        let rust = format!(
            "fn run() {{\n    #[cfg(feature = \"qa-native\")]\n    {{\n        let b = Builder::new();\n        b.setup();\n        if let Some(p) = {REGISTER} {{ b.plugin(p); }}\n    }}\n}}"
        );
        assert_eq!(control(CARGO, &rust, &[]), Vec::<String>::new());
    }

    #[test]
    fn accepts_registration_in_a_nested_block_of_a_gated_function() {
        let rust = format!(
            "#[cfg(feature = \"qa-native\")]\nfn register(b: Builder) -> Builder {{\n    let x = 1;\n    if x == 1 {{\n        let y = 2;\n        for _ in 0..y {{\n            let z = 3;\n            if let Some(p) = {REGISTER} {{ return b.plugin(p); }}\n        }}\n    }}\n    b\n}}"
        );
        assert_eq!(control(CARGO, &rust, &[]), Vec::<String>::new());
    }

    #[test]
    fn rejects_registration_after_a_statement_in_an_ungated_block() {
        let rust = format!(
            "fn run() {{\n    {{\n        let b = Builder::new();\n        if let Some(p) = {REGISTER} {{ b.plugin(p); }}\n    }}\n}}"
        );
        let issues = control(CARGO, &rust, &[]);
        assert!(any_contains(&issues, "is unconditional"), "{issues:?}");
    }

    #[test]
    fn a_gate_on_a_sibling_block_or_function_does_not_cover_the_registration() {
        let sibling_block = format!(
            "fn run() {{\n    #[cfg(feature = \"qa-native\")]\n    {{ other(); }}\n    {{\n        let b = Builder::new();\n        if let Some(p) = {REGISTER} {{ b.plugin(p); }}\n    }}\n}}"
        );
        let issues = control(CARGO, &sibling_block, &[]);
        assert!(any_contains(&issues, "is unconditional"), "{issues:?}");
        let sibling_fn = format!(
            "#[cfg(feature = \"qa-native\")]\nfn gated() {{ one(); }}\nfn run() {{\n    let b = Builder::new();\n    if let Some(p) = {REGISTER} {{ b.plugin(p); }}\n}}"
        );
        let issues = control(CARGO, &sibling_fn, &[]);
        assert!(any_contains(&issues, "is unconditional"), "{issues:?}");
    }

    #[test]
    fn a_gate_deeper_than_the_walk_bound_is_not_trusted() {
        let mut rust = String::from("#[cfg(feature = \"qa-native\")]\nfn run() {\n");
        for _ in 0..9 {
            rust.push_str("{ let a = 1;\n");
        }
        rust.push_str(&format!("let r = {REGISTER};\n"));
        for _ in 0..9 {
            rust.push_str("}\n");
        }
        rust.push_str("}\n");
        let issues = control(CARGO, &rust, &[]);
        assert!(any_contains(&issues, "is unconditional"), "{issues:?}");
    }

    // ---- validate_app: custom-protocol, QA scripts, window label, declared env keys ----

    /// A reduced Tauri 2 config: a dev server and one window.
    const CONF_DEV: &str =
        r#"{"build":{"devUrl":"http://localhost:1420"},"app":{"windows":[{"label":"main"}]}}"#;

    /// `CARGO` plus a tauri dependency; `qa_native_extra` is appended to the qa-native list.
    fn tauri_cargo(qa_native_extra: &str, other_features: &str, tauri_dep: &str) -> String {
        format!(
            "[features]\ndefault = []\nqa-native = [\"dep:rightkit-qa-driver\", \
             \"rightkit-qa-driver/qa-control\"{qa_native_extra}]\n{other_features}\n\
             [dependencies]\nrightkit-qa-driver = {{ version = \"0.3\", optional = true, \
             default-features = false }}\n{tauri_dep}\n"
        )
    }

    const TAURI_DEP: &str = "tauri = { version = \"2\" }";

    /// A cargo manifest whose qa-native build carries custom-protocol.
    fn custom_protocol_cargo() -> String {
        tauri_cargo(", \"tauri/custom-protocol\"", "", TAURI_DEP)
    }

    /// What `validate_app` is asked about, with everything optional left out.
    #[derive(Default)]
    struct App<'a> {
        conf: Option<&'a str>,
        window: Option<&'a str>,
        keys: Vec<String>,
        switches: Option<&'a str>,
        scripts: Vec<(&'a str, &'a str)>,
    }

    fn app_check(cargo: &str, rust: &str, app: &App) -> Vec<String> {
        let scripts: BTreeMap<String, String> = app
            .scripts
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        validate_app(&AppContract {
            contract: ContractInput {
                cargo_toml: cargo,
                rust_source: rust,
                package_scripts: &scripts,
                backend: Backend::Control,
            },
            tauri_conf: app.conf,
            declared_window_label: app.window,
            declared_env_keys: &app.keys,
            switches: app.switches,
        })
    }

    fn with_conf(conf: &str) -> App<'_> {
        App {
            conf: Some(conf),
            ..App::default()
        }
    }

    #[test]
    fn dev_url_without_custom_protocol_in_qa_native_is_a_finding() {
        let cargo = tauri_cargo("", "", TAURI_DEP);
        let issues = app_check(&cargo, RUST_GATED, &with_conf(CONF_DEV));
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert!(
            issues[0].contains("dev server at http://localhost:1420"),
            "{issues:?}"
        );
        assert!(issues[0].contains("tauri/custom-protocol"), "{issues:?}");
    }

    #[test]
    fn a_custom_protocol_feature_that_qa_native_does_not_reach_is_still_a_finding() {
        let cargo = tauri_cargo(
            "",
            "custom-protocol = [\"tauri/custom-protocol\"]",
            TAURI_DEP,
        );
        let issues = app_check(&cargo, RUST_GATED, &with_conf(CONF_DEV));
        assert!(any_contains(&issues, "dev server at"), "{issues:?}");
    }

    #[test]
    fn custom_protocol_listed_in_qa_native_passes() {
        for item in ["tauri/custom-protocol", "tauri?/custom-protocol"] {
            let cargo = tauri_cargo(&format!(", \"{item}\""), "", TAURI_DEP);
            let issues = app_check(&cargo, RUST_GATED, &with_conf(CONF_DEV));
            assert_eq!(issues, Vec::<String>::new(), "{item}");
        }
    }

    #[test]
    fn custom_protocol_reached_through_another_feature_passes() {
        let cargo = tauri_cargo(
            ", \"custom-protocol\"",
            "custom-protocol = [\"tauri/custom-protocol\"]",
            TAURI_DEP,
        );
        let issues = app_check(&cargo, RUST_GATED, &with_conf(CONF_DEV));
        assert_eq!(issues, Vec::<String>::new());
        let deeper = tauri_cargo(
            ", \"shipping\"",
            "shipping = [\"custom-protocol\"]\ncustom-protocol = [\"tauri/custom-protocol\"]",
            TAURI_DEP,
        );
        let issues = app_check(&deeper, RUST_GATED, &with_conf(CONF_DEV));
        assert_eq!(issues, Vec::<String>::new());
    }

    #[test]
    fn custom_protocol_in_the_tauri_dependency_features_passes() {
        for dep in [
            "tauri = { version = \"2\", features = [\"custom-protocol\"] }",
            "[dependencies.tauri]\nversion = \"2\"\nfeatures = [\"custom-protocol\"]",
        ] {
            let cargo = tauri_cargo("", "", dep);
            let issues = app_check(&cargo, RUST_GATED, &with_conf(CONF_DEV));
            assert_eq!(issues, Vec::<String>::new(), "{dep}");
        }
    }

    #[test]
    fn no_dev_url_means_no_custom_protocol_finding() {
        let cargo = tauri_cargo("", "", TAURI_DEP);
        for conf in [
            r#"{"build":{"frontendDist":"../dist"}}"#,
            r#"{"build":{"devUrl":""}}"#,
            r#"{"build":{"devUrl":"  "}}"#,
            r#"{"build":{"devUrl":null}}"#,
            "not json",
        ] {
            let issues = app_check(&cargo, RUST_GATED, &with_conf(conf));
            assert_eq!(issues, Vec::<String>::new(), "{conf}");
        }
        let issues = app_check(&cargo, RUST_GATED, &App::default());
        assert_eq!(issues, Vec::<String>::new());
    }

    #[test]
    fn custom_protocol_is_only_judged_when_qa_native_exists() {
        let cargo = tauri_cargo("", "", TAURI_DEP).replace(
            "qa-native = [\"dep:rightkit-qa-driver\", \"rightkit-qa-driver/qa-control\"]\n",
            "",
        );
        let issues = app_check(&cargo, RUST_GATED, &with_conf(CONF_DEV));
        assert!(
            any_contains(&issues, "must define the qa-native feature"),
            "{issues:?}"
        );
        assert!(!any_contains(&issues, "dev server"), "{issues:?}");
    }

    fn script_issues(scripts: &[(&str, &str)], conf: Option<&str>) -> Vec<String> {
        let app = App {
            conf,
            scripts: scripts.to_vec(),
            ..App::default()
        };
        app_check(&custom_protocol_cargo(), RUST_GATED, &app)
    }

    #[test]
    fn a_qa_script_that_runs_tauri_dev_with_qa_native_is_a_finding() {
        for conf in [Some(CONF_DEV), None] {
            let issues = script_issues(&[("qa:native", "tauri dev --features qa-native")], conf);
            assert_eq!(issues.len(), 1, "{issues:?}");
            assert!(issues[0].contains("package script qa:native"), "{issues:?}");
            assert!(issues[0].contains("dev server"), "{issues:?}");
        }
        for name in ["qa", "test", "e2e", "test:native", "e2e:native"] {
            let issues = script_issues(&[(name, "pnpm tauri dev --features qa-native")], None);
            assert!(
                any_contains(&issues, "runs `tauri dev`"),
                "{name}: {issues:?}"
            );
        }
    }

    #[test]
    fn tauri_dev_outside_a_qa_script_or_without_qa_native_is_left_alone() {
        let issues = script_issues(
            &[
                ("dev:native", "tauri dev --features qa-native"),
                ("qa:dev", "tauri dev"),
                ("dev", "tauri dev"),
            ],
            Some(CONF_DEV),
        );
        assert_eq!(issues, Vec::<String>::new());
    }

    #[test]
    fn a_cargo_only_qa_script_with_a_dev_url_is_a_finding() {
        for command in [
            "cargo build --features qa-native",
            "cargo run --features qa-native",
            "rustright cargo build --features qa-native",
            "FOO=1 rustright cargo run --features qa-native",
            "cargo +stable build --locked --features qa-native",
            "cargo build --features qa-native && pnpm build",
        ] {
            let issues = script_issues(&[("qa:native", command)], Some(CONF_DEV));
            assert_eq!(issues.len(), 1, "{command}: {issues:?}");
            assert!(
                issues[0].contains("without building the frontend"),
                "{issues:?}"
            );
            assert!(issues[0].contains("http://localhost:1420"), "{issues:?}");
        }
    }

    #[test]
    fn a_cargo_qa_script_without_a_dev_url_is_left_alone() {
        for conf in [None, Some(r#"{"build":{"frontendDist":"../dist"}}"#)] {
            let issues = script_issues(&[("qa:native", "cargo build --features qa-native")], conf);
            assert_eq!(issues, Vec::<String>::new());
        }
    }

    #[test]
    fn a_qa_script_that_builds_the_frontend_first_passes() {
        for command in [
            "pnpm build && cargo build --features qa-native",
            "pnpm run build && cargo build --features qa-native",
            "pnpm run build:web && rustright cargo run --features qa-native",
            "pnpm -C web build && cargo build --features qa-native",
            "pnpm --filter ui build && cargo build --features qa-native",
            "npm run build; cargo run --features qa-native",
            "yarn build && cargo build --features qa-native",
            "bun run build && cargo build --features qa-native",
            "pnpm exec vite build && cargo build --features qa-native",
            "vite build && cargo build --features qa-native",
            "cd web && pnpm build && cargo build --features qa-native",
            "NODE_ENV=production pnpm build && cargo build --features qa-native",
        ] {
            let issues = script_issues(&[("qa:native", command)], Some(CONF_DEV));
            assert_eq!(issues, Vec::<String>::new(), "{command}");
        }
    }

    #[test]
    fn a_non_build_pnpm_segment_is_not_a_frontend_build() {
        for command in [
            "pnpm install && cargo build --features qa-native",
            "pnpm test:build && cargo build --features qa-native",
            "pnpm run lint && cargo build --features qa-native",
            "echo build && cargo build --features qa-native",
        ] {
            let issues = script_issues(&[("qa:native", command)], Some(CONF_DEV));
            assert_eq!(issues.len(), 1, "{command}: {issues:?}");
        }
    }

    #[test]
    fn tauri_build_anywhere_in_a_qa_script_passes() {
        for command in [
            "tauri build --debug --features qa-native",
            "pnpm tauri build --debug --features qa-native",
            "cargo tauri build --debug -- --features qa-native",
            "tauri build --debug --features qa-native && cargo run --features qa-native",
        ] {
            let issues = script_issues(&[("qa:native", command)], Some(CONF_DEV));
            assert_eq!(issues, Vec::<String>::new(), "{command}");
        }
    }

    const CONF_HUB: &str = r#"{"app":{"windows":[{"label":"hub"},{"label":"settings"}]}}"#;

    /// A dev server and a first window labelled `hub`.
    const CONF_DEV_HUB: &str =
        r#"{"build":{"devUrl":"http://localhost:1420"},"app":{"windows":[{"label":"hub"}]}}"#;

    /// `RUST_GATED` plus a function that names a window label.
    fn rust_with_window(label: &str) -> String {
        format!("{RUST_GATED}\nfn hub(c: Control) {{ c.window( \"{label}\" ); }}\n")
    }

    #[test]
    fn a_first_window_other_than_main_needs_a_window_label() {
        let issues = app_check(CARGO, RUST_GATED, &with_conf(CONF_HUB));
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert!(issues[0].contains("first window"), "{issues:?}");
        assert!(issues[0].contains("\"hub\""), "{issues:?}");
        assert!(issues[0].contains(".window(\"hub\")"), "{issues:?}");
        assert!(issues[0].contains("window_label"), "{issues:?}");
        assert!(issues[0].contains("RIGHTKIT_QA_WINDOW_LABEL"), "{issues:?}");
    }

    #[test]
    fn a_window_registration_naming_the_first_window_passes() {
        let issues = app_check(CARGO, &rust_with_window("hub"), &with_conf(CONF_HUB));
        assert_eq!(issues, Vec::<String>::new());
        let split = format!(
            "{RUST_GATED}\nfn hub(c: Control) {{ c\n    .window(\n        \"hub\",\n    ); }}\n"
        );
        let issues = app_check(CARGO, &split, &with_conf(CONF_HUB));
        assert_eq!(issues, Vec::<String>::new());
    }

    #[test]
    fn a_window_registration_for_another_label_or_in_a_comment_does_not_count() {
        let other = app_check(CARGO, &rust_with_window("settings"), &with_conf(CONF_HUB));
        assert!(any_contains(&other, "first window"), "{other:?}");
        let commented = format!("{RUST_GATED}\n// c.window(\"hub\");\n");
        let issues = app_check(CARGO, &commented, &with_conf(CONF_HUB));
        assert!(any_contains(&issues, "first window"), "{issues:?}");
    }

    #[test]
    fn a_declared_window_label_naming_the_first_window_passes() {
        let app = App {
            conf: Some(CONF_HUB),
            window: Some("hub"),
            ..App::default()
        };
        assert_eq!(app_check(CARGO, RUST_GATED, &app), Vec::<String>::new());
    }

    #[test]
    fn a_first_window_labelled_main_needs_nothing() {
        for conf in [
            r#"{"app":{"windows":[{"label":"main"},{"label":"other"}]}}"#,
            r#"{"app":{"windows":[{"title":"No label means main"}]}}"#,
            r#"{"app":{"windows":[]}}"#,
            r#"{"app":{}}"#,
            r#"{}"#,
        ] {
            let issues = app_check(CARGO, RUST_GATED, &with_conf(conf));
            assert_eq!(issues, Vec::<String>::new(), "{conf}");
        }
    }

    #[test]
    fn a_declared_window_label_that_is_no_window_is_a_finding() {
        let app = App {
            conf: Some(CONF_DEV),
            window: Some("ghost"),
            ..App::default()
        };
        let issues = app_check(&custom_protocol_cargo(), RUST_GATED, &app);
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert!(issues[0].contains("\"ghost\""), "{issues:?}");
        assert!(
            issues[0].contains("not the label of any window"),
            "{issues:?}"
        );
        // Nothing to compare with when the config lists no window.
        let app = App {
            conf: Some(r#"{"app":{"windows":[]}}"#),
            window: Some("ghost"),
            ..App::default()
        };
        assert_eq!(app_check(CARGO, RUST_GATED, &app), Vec::<String>::new());
    }

    const SWITCHES: &str = r#"{"schema":1,"app":"demo","switches":[
        {"name":"DEMO_PROFILE","class":"S1","channel":"env","compiledOut":"feature:test-mode"},
        {"name":"DEMO_TUNING","class":"S6","channel":"env","valueType":"bool"}]}"#;

    fn keys(names: &[&str]) -> Vec<String> {
        names.iter().map(|k| k.to_string()).collect()
    }

    #[test]
    fn a_declared_env_key_listed_in_a_release_forbidden_class_passes() {
        for class in ["S1", "S2", "S3", "S4"] {
            let switches = SWITCHES.replace("\"S1\"", &format!("\"{class}\""));
            let app = App {
                keys: keys(&["DEMO_PROFILE"]),
                switches: Some(&switches),
                ..App::default()
            };
            assert_eq!(
                app_check(CARGO, RUST_GATED, &app),
                Vec::<String>::new(),
                "{class}"
            );
        }
    }

    #[test]
    fn a_declared_env_key_in_a_shippable_class_is_a_finding() {
        let app = App {
            keys: keys(&["DEMO_TUNING"]),
            switches: Some(SWITCHES),
            ..App::default()
        };
        let issues = app_check(CARGO, RUST_GATED, &app);
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert!(issues[0].contains("DEMO_TUNING"), "{issues:?}");
        assert!(issues[0].contains("\"S6\""), "{issues:?}");
        let s5 = SWITCHES.replace("\"S1\"", "\"S5\"");
        let app = App {
            keys: keys(&["DEMO_PROFILE"]),
            switches: Some(&s5),
            ..App::default()
        };
        assert!(any_contains(&app_check(CARGO, RUST_GATED, &app), "\"S5\""));
        let classless = r#"{"switches":[{"name":"DEMO_PROFILE"}]}"#;
        let app = App {
            keys: keys(&["DEMO_PROFILE"]),
            switches: Some(classless),
            ..App::default()
        };
        assert!(any_contains(&app_check(CARGO, RUST_GATED, &app), "(none)"));
    }

    #[test]
    fn a_declared_env_key_missing_from_switches_json_is_a_finding() {
        let app = App {
            keys: keys(&["DEMO_PROFILE", "DEMO_OTHER"]),
            switches: Some(SWITCHES),
            ..App::default()
        };
        let issues = app_check(CARGO, RUST_GATED, &app);
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert!(issues[0].contains("DEMO_OTHER"), "{issues:?}");
        assert!(
            issues[0].contains("not listed in switches.json"),
            "{issues:?}"
        );
    }

    #[test]
    fn declared_env_keys_without_switches_json_are_one_finding_naming_the_paths() {
        let app = App {
            keys: keys(&["DEMO_PROFILE", "DEMO_OTHER"]),
            ..App::default()
        };
        let issues = app_check(CARGO, RUST_GATED, &app);
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert!(
            issues[0].contains("no switches.json was found"),
            "{issues:?}"
        );
        assert!(issues[0].contains("app root"), "{issues:?}");
        assert!(issues[0].contains("src-tauri/"), "{issues:?}");
        assert!(issues[0].contains("DEMO_PROFILE, DEMO_OTHER"), "{issues:?}");
    }

    #[test]
    fn declared_env_keys_with_an_unreadable_switches_json_are_a_finding() {
        let app = App {
            keys: keys(&["DEMO_PROFILE"]),
            switches: Some("{ not json"),
            ..App::default()
        };
        let issues = app_check(CARGO, RUST_GATED, &app);
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert!(issues[0].contains("not valid JSON"), "{issues:?}");
    }

    #[test]
    fn no_declared_env_keys_need_no_switches_json() {
        assert_eq!(
            app_check(CARGO, RUST_GATED, &App::default()),
            Vec::<String>::new()
        );
        let app = App {
            switches: Some("{ not json"),
            ..App::default()
        };
        assert_eq!(app_check(CARGO, RUST_GATED, &app), Vec::<String>::new());
    }

    #[test]
    fn validate_app_adds_nothing_to_a_valid_input_and_keeps_validate_findings() {
        let scripts: BTreeMap<String, String> = [(
            "qa".to_string(),
            "pnpm build && cargo run --features qa-native".to_string(),
        )]
        .into();
        let cargo = custom_protocol_cargo();
        let declared = keys(&["DEMO_PROFILE"]);
        let rust = rust_with_window("hub");
        let valid = AppContract {
            contract: ContractInput {
                cargo_toml: &cargo,
                rust_source: &rust,
                package_scripts: &scripts,
                backend: Backend::Control,
            },
            tauri_conf: Some(CONF_DEV_HUB),
            declared_window_label: None,
            declared_env_keys: &declared,
            switches: Some(SWITCHES),
        };
        assert_eq!(validate(&valid.contract), Vec::<String>::new());
        assert_eq!(validate_app(&valid), validate(&valid.contract));
        assert!(assert_app_contract(&valid).is_ok());

        // Findings of the plain contract come first and are unchanged.
        let broken = AppContract {
            contract: ContractInput {
                cargo_toml: &cargo,
                rust_source: "fn main(){}",
                package_scripts: &scripts,
                backend: Backend::Control,
            },
            ..valid
        };
        let plain = validate(&broken.contract);
        assert!(any_contains(&plain, "must register"), "{plain:?}");
        let all = validate_app(&broken);
        assert!(any_contains(&all, "first window"), "{all:?}");
        assert_eq!(all[..plain.len()], plain[..]);
        let failure = assert_app_contract(&broken).unwrap_err();
        assert!(
            failure.0.starts_with("native QA contract failed:\n- "),
            "{failure}"
        );
    }

    #[test]
    fn feature_closure_follows_listed_features_and_survives_a_cycle() {
        let entries = feature_entries(
            "[features]\na = [\"b\", \"x/y\"]\nb = [\"c\", \"a\"]\nc = [\"dep:z\"]\nd = [\"q\"]\n",
        );
        let closure = feature_closure(&entries, "a");
        for item in ["b", "x/y", "c", "dep:z", "a"] {
            assert!(closure.contains(item), "{item}: {closure:?}");
        }
        assert!(!closure.contains("q"), "{closure:?}");
        assert!(feature_reaches(&entries, "a", "dep:z"));
        assert!(!feature_reaches(&entries, "d", "dep:z"));
        assert!(!feature_reaches(&entries, "missing", "dep:z"));
    }

    // ---- tauri_crate_dir and rust_sources_under ----

    /// A unique scratch directory under the system temp dir, removed on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "rightkit-qa-guard-{tag}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }

        fn write(&self, rel: &str, text: &str) {
            let path = self.0.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn tauri_crate_dir_prefers_src_tauri_over_the_root() {
        let s = Scratch::new("crate-nested");
        s.write("Cargo.toml", "[package]\n");
        s.write("src-tauri/Cargo.toml", "[package]\n");
        assert_eq!(tauri_crate_dir(&s.0).unwrap(), s.0.join("src-tauri"));
    }

    #[test]
    fn tauri_crate_dir_falls_back_to_the_root_crate() {
        let s = Scratch::new("crate-root");
        s.write("Cargo.toml", "[package]\n");
        assert_eq!(tauri_crate_dir(&s.0).unwrap(), s.0);
        // A src-tauri directory without a manifest is not a crate.
        s.write("src-tauri/src/lib.rs", "");
        assert_eq!(tauri_crate_dir(&s.0).unwrap(), s.0);
    }

    #[test]
    fn tauri_crate_dir_names_both_paths_when_there_is_no_crate() {
        let s = Scratch::new("crate-none");
        let e = tauri_crate_dir(&s.0).unwrap_err().0;
        let nested = s.0.join("src-tauri").join("Cargo.toml");
        let root = s.0.join("Cargo.toml");
        assert!(e.contains(&nested.display().to_string()), "{e}");
        assert!(e.contains(&root.display().to_string()), "{e}");
    }

    #[test]
    fn rust_sources_under_reads_nested_modules_sorted_with_separators() {
        let s = Scratch::new("sources");
        s.write("lib.rs", "pub mod commands;\n");
        s.write("main.rs", "fn main() {}");
        s.write("commands/mod.rs", "pub mod deep;\n");
        s.write("commands/deep/x.rs", "pub fn x() {}\n");
        s.write("notes.txt", "not rust\n");
        s.write("target/skipped.rs", "fn skipped() {}\n");
        s.write("node_modules/skipped.rs", "fn skipped() {}\n");
        s.write("commands/target/skipped.rs", "fn skipped() {}\n");
        s.write("commands/node_modules/skipped.rs", "fn skipped() {}\n");
        let text = rust_sources_under(&s.0).unwrap();
        assert_eq!(
            text,
            "// ---- commands/deep/x.rs ----\npub fn x() {}\n\
             // ---- commands/mod.rs ----\npub mod deep;\n\
             // ---- lib.rs ----\npub mod commands;\n\
             // ---- main.rs ----\nfn main() {}\n"
        );
    }

    #[test]
    fn rust_sources_under_a_missing_directory_is_an_error_naming_it() {
        let s = Scratch::new("sources-missing");
        let missing = s.0.join("absent");
        let e = rust_sources_under(&missing).unwrap_err().0;
        assert!(e.contains(&missing.display().to_string()), "{e}");
    }

    #[test]
    fn a_registration_in_a_second_source_file_is_seen_by_the_guard() {
        let s = Scratch::new("second-file");
        s.write("lib.rs", "pub mod qa;\nfn run() {}\n");
        s.write("qa.rs", RUST_GATED);
        let rust = rust_sources_under(&s.0).unwrap();
        assert_eq!(control(CARGO, &rust, &[]), Vec::<String>::new());
        // The same registration without its gate is still caught across files.
        s.write("qa.rs", RUST_UNGATED);
        let rust = rust_sources_under(&s.0).unwrap();
        assert!(any_contains(
            &control(CARGO, &rust, &[]),
            "is unconditional"
        ));
    }
}
