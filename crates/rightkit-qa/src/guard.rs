//! The guard that keeps the in-app QA control surface out of builds that must not carry it.
//!
//! Two backends are understood: `control` (the in-app `rightkit-control` server,
//! the default) and `wdio` (the legacy `tauri-plugin-wdio-webdriver`, kept so an
//! app mid-migration still validates).
//!
//! The rule: the server is compiled only behind an explicit `qa-native` cargo feature.
//! `qa-native` is off by default and only dev lanes enable it. A dev build may contain the
//! server (it may use a release-derived profile, so `debug_assertions` is not required).
//! A release or update build must not contain it; `right-release` refuses to seal one that does.
use crate::util::{err, Result};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

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
            Backend::Control => "rightkit-control",
            Backend::Wdio => "tauri-plugin-wdio-webdriver",
        }
    }
    fn registration(self) -> &'static str {
        match self {
            Backend::Control => "rightkit_control::embedded::Control",
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

/// Feature items that switch on rightkit-shell developer inspection. None may be reachable from
/// `default`: a release build must never contain it (`packages/release` refuses it as well).
const DEV_INSPECT_FEATURES: [&str; 4] = [
    "dev-inspect",
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

/// Items of a feature list body such as `"dep:rightkit-control", "rightkit-control/qa-control"`.
fn list_items(list: &str) -> Vec<String> {
    list.split(',')
        .map(|s| s.trim().trim_matches('"').trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Whether `default`, directly or through other features, enables `target`.
fn default_reaches(entries: &[(String, String)], target: &str) -> bool {
    let mut stack: Vec<String> = entries
        .iter()
        .filter(|(k, _)| k.as_str() == "default")
        .flat_map(|(_, v)| list_items(v))
        .collect();
    let mut seen = BTreeSet::new();
    while let Some(item) = stack.pop() {
        if item == target {
            return true;
        }
        if !seen.insert(item.clone()) {
            continue;
        }
        if let Some((_, list)) = entries.iter().find(|(k, _)| k.as_str() == item.as_str()) {
            stack.extend(list_items(list));
        }
    }
    false
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

/// Whether the code at `at` sits under an accepted qa-native gate. The gate may sit on the
/// statement itself, or on an enclosing function or block, so the walk moves outward
/// through `{` boundaries and stops at the first `;` or `}`.
fn is_gated(src: &str, at: usize) -> bool {
    let mut end = at;
    for _ in 0..8 {
        let found = src[..end].rfind([';', '{', '}']);
        let head_start = found.map_or(0, |p| p + 1);
        let head = compact(&src[head_start..end]);
        if QA_NATIVE_GATES.iter().any(|gate| head.contains(*gate)) {
            return true;
        }
        match found {
            Some(p) if src.as_bytes()[p] == b'{' => end = p,
            _ => return false,
        }
    }
    false
}

pub fn validate(input: &ContractInput) -> Vec<String> {
    let mut issues = vec![];
    let dep = input.backend.dependency();
    let control = input.backend == Backend::Control;
    let dep_body = dependency_body(input.cargo_toml, dep).map(|b| compact(&b));
    let dep_enables_qa_control = dep_body
        .as_deref()
        .is_some_and(|b| b.contains("\"qa-control\""));
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
    let qa_control_feature = format!("{dep}/qa-control");
    match entries.iter().find(|(k, _)| k.as_str() == "qa-native") {
        None => issues.push(
            "Cargo.toml must define the qa-native feature under [features], for example \
             `qa-native = [\"dep:rightkit-control\", \"rightkit-control/qa-control\"]`"
                .into(),
        ),
        Some((_, list)) => {
            let items = list_items(list);
            if !items.iter().any(|i| i.as_str() == dep_feature.as_str()) {
                issues.push(format!("qa-native must enable {dep_feature}"));
            }
            let enables_control_qa = items
                .iter()
                .any(|i| i.as_str() == qa_control_feature.as_str());
            if control && !enables_control_qa && !dep_enables_qa_control {
                issues.push(
                    "qa-native must enable rightkit-control/qa-control (add it to the qa-native \
                     list, or list qa-control in the rightkit-control dependency's features); \
                     without it the embedded server is not compiled"
                        .into(),
                );
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
                 (rightkit-shell dev-inspect) into the app, and right-release refuses to seal \
                 or upload such a build. Remove it from `default` (directly or through another \
                 default feature) so that only an explicit dev build contains it"
            ));
        }
    }
    if let Some(body) = dependency_body(input.cargo_toml, "rightkit-shell").map(|b| compact(&b)) {
        if body.contains("\"dev-inspect\"") && !body.contains("optional=true") {
            issues.push(
                "rightkit-shell lists the dev-inspect feature without optional = true, so \
                 developer inspection compiles into every build: enable it only from an \
                 optional dev feature such as `dev-native = [\"rightkit-shell/dev-inspect\"]`"
                    .into(),
            );
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

    let registration = input.backend.registration();
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
qa-native = ["dep:rightkit-control", "rightkit-control/qa-control"]
[dependencies]
rightkit-control = { version = "0.3", optional = true, default-features = false }
"##;

    /// The alternative shape: qa-control listed in the optional dependency's own features.
    const CARGO_DEP_FEATURES: &str = r##"
[features]
default = []
qa-native = ["dep:rightkit-control"]
[dependencies]
rightkit-control = { version = "0.3", optional = true, default-features = false, features = ["qa-control"] }
"##;

    /// Registration gated by the feature alone; the registration also appears in a `use` line.
    const RUST_GATED: &str = r##"
use rightkit_control::embedded::Control;
fn run() {
    #[cfg(feature = "qa-native")]
    if let Some(p) = rightkit_control::embedded::Control::<tauri::Wry>::new().build_if_enabled() { b = b.plugin(p); }
}"##;

    /// The older debug-only form, with its compile_error. Still accepted.
    const RUST_DEBUG_ONLY: &str = r##"
#[cfg(all(not(debug_assertions), feature = "qa-native"))]
compile_error!("qa-native is debug only");
fn run() {
    #[cfg(all(debug_assertions, feature = "qa-native"))]
    if let Some(p) = rightkit_control::embedded::Control::<tauri::Wry>::new().build_if_enabled() { b = b.plugin(p); }
}"##;

    /// The gate sits on the function that holds the registration.
    const RUST_FN_GATED: &str = r##"
#[cfg(feature = "qa-native")]
fn register(b: Builder) -> Builder {
    if let Some(p) = rightkit_control::embedded::Control::<tauri::Wry>::new().build_if_enabled() { b.plugin(p) } else { b }
}"##;

    const RUST_UNGATED: &str = r##"
use rightkit_control::embedded::Control;
fn run() {
    if let Some(p) = rightkit_control::embedded::Control::<tauri::Wry>::new().build_if_enabled() { b = b.plugin(p); }
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

    /// The optional dev feature that selects rightkit-shell developer inspection.
    const CARGO_DEV_INSPECT: &str = r##"
[features]
default = []
qa-native = ["dep:rightkit-control", "rightkit-control/qa-control"]
dev-native = ["rightkit-shell/dev-inspect"]
[dependencies]
rightkit-control = { version = "0.3", optional = true, default-features = false }
rightkit-shell = { version = "0.4", default-features = false }
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
            "default = [\"rightkit-shell/dev-inspect\"]",
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
            "rightkit-shell = { version = \"0.4\", default-features = false }",
            "rightkit-shell = { version = \"0.4\", features = [\"dev-inspect\"] }",
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
        let cargo = CARGO.replace(", \"rightkit-control/qa-control\"", "");
        let issues = control(&cargo, RUST_GATED, &[]);
        assert!(
            any_contains(&issues, "rightkit-control/qa-control"),
            "{issues:?}"
        );
    }

    #[test]
    fn rejects_other_feature_that_enables_the_dependency() {
        let cargo = CARGO.replace(
            "[dependencies]",
            "browser-ui = [\"dep:rightkit-control\"]\n[dependencies]",
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
            "qa-native = [\"dep:rightkit-control\", \"rightkit-control/qa-control\"]\n",
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
}
