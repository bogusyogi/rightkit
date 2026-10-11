//! The guard that keeps the in-app QA control surface out of production builds.
//!
//! Two backends are understood: `control` (the in-app `rightkit-control` server,
//! the default) and `wdio` (the legacy `tauri-plugin-wdio-webdriver`, kept so an
//! app mid-migration still validates).
use crate::util::{err, Result};
use serde_json::Value;
use std::collections::BTreeMap;

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

fn feature_list(cargo: &str, feature: &str) -> String {
    let mut search = 0;
    while let Some(i) = cargo[search..].find(feature) {
        let at = search + i;
        search = at + feature.len();
        let rest = cargo[at + feature.len()..].trim_start();
        if let Some(r) = rest.strip_prefix('=') {
            if let Some(body) = r.trim_start().strip_prefix('[') {
                if let Some(e) = body.find(']') {
                    return body[..e].to_string();
                }
            }
        }
    }
    String::new()
}

pub fn validate(input: &ContractInput) -> Vec<String> {
    let mut issues = vec![];
    let dep = input.backend.dependency();
    match dependency_body(input.cargo_toml, dep) {
        None => issues.push(format!("Cargo.toml must declare {dep}")),
        Some(body) => {
            if !body.replace(' ', "").contains("optional=true") {
                issues.push(format!("{dep} must be optional"));
            }
        }
    }
    let feature = feature_list(input.cargo_toml, "qa-native");
    if !feature.contains(&format!("dep:{dep}")) {
        issues.push(format!("qa-native must enable dep:{dep}"));
    }
    let compact: String = input
        .rust_source
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    if !compact.contains(r#"cfg(all(not(debug_assertions),feature="qa-native"))"#)
        || !compact.contains("compile_error!")
    {
        issues
            .push("Rust must compile_error when qa-native is enabled outside debug builds".into());
    }
    if !compact.contains(r#"cfg(all(debug_assertions,feature="qa-native"))"#)
        || !compact.contains(input.backend.registration())
    {
        issues.push(format!(
            "{dep} registration must require debug_assertions and qa-native"
        ));
    }
    let gated = compact.contains(input.backend.runtime_flag())
        || (input.backend == Backend::Control && compact.contains("build_if_enabled"));
    if !gated {
        issues.push(format!(
            "registration must also require the {} runtime flag",
            input.backend.runtime_flag()
        ));
    }
    for (name, command) in input.package_scripts {
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
            issues.push(format!("package script {name} must not enable qa-native"));
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

    const CARGO: &str = r#"
[features]
qa-native = ["dep:rightkit-control"]
[dependencies]
rightkit-control = { version = "0.1", optional = true, features = ["qa-control"] }
"#;
    const RUST: &str = r#"
#[cfg(all(not(debug_assertions), feature = "qa-native"))]
compile_error!("qa-native is debug only");
fn run() {
    #[cfg(all(debug_assertions, feature = "qa-native"))]
    if let Some(p) = rightkit_control::embedded::Control::<tauri::Wry>::new().build_if_enabled() { b = b.plugin(p); }
}"#;

    #[test]
    fn accepts_debug_only_control_registration() {
        let scripts = BTreeMap::from([(
            "qa".to_string(),
            "cargo run --features qa-native".to_string(),
        )]);
        let i = ContractInput {
            cargo_toml: CARGO,
            rust_source: RUST,
            package_scripts: &scripts,
            backend: Backend::Control,
        };
        assert_eq!(validate(&i), Vec::<String>::new());
    }

    #[test]
    fn rejects_release_scripts_and_non_optional_dependency() {
        let cargo = CARGO.replace("optional = true, ", "");
        let scripts = BTreeMap::from([(
            "release".to_string(),
            "tauri build --features qa-native".to_string(),
        )]);
        let i = ContractInput {
            cargo_toml: &cargo,
            rust_source: RUST,
            package_scripts: &scripts,
            backend: Backend::Control,
        };
        let issues = validate(&i);
        assert!(
            issues.iter().any(|s| s.contains("must be optional")),
            "{issues:?}"
        );
        assert!(
            issues.iter().any(|s| s.contains("package script release")),
            "{issues:?}"
        );
    }

    #[test]
    fn rejects_missing_compile_error() {
        let scripts = BTreeMap::new();
        let i = ContractInput {
            cargo_toml: CARGO,
            rust_source: "fn main(){}",
            package_scripts: &scripts,
            backend: Backend::Control,
        };
        assert!(validate(&i).iter().any(|s| s.contains("compile_error")));
    }
}
