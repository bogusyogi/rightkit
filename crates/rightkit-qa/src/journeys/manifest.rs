//! `journeys.toml` (D3) and `journeys-map.toml` (D8): types and static validation.
//!
//! [`check`] reads both files from an app root and returns every issue it finds; it does not run
//! anything. Unknown keys are refused, so a misspelled `release_critical` cannot silently turn a
//! journey into a non-critical one.
use super::{is_hex_lower, is_safe_relative};
use crate::util::{err, sha256_hex, Result};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub const MANIFEST_FILE: &str = "journeys.toml";
pub const MAPPING_FILE: &str = "journeys-map.toml";

/// Platforms a journey may declare (receipt `host.os` uses the same words).
pub const PLATFORMS: [&str; 3] = ["macos", "windows", "linux"];
pub const SURFACES: [&str; 5] = ["native-ui", "web", "engine", "cli", "installed"];
pub const RUNNERS: [&str; 2] = ["scenario", "adapter"];
pub const TIERS: [&str; 3] = ["fast", "full", "paid"];
pub const DATA: [&str; 2] = ["generated", "seeded"];
pub const CHANGE_KINDS: [&str; 5] = [
    "removed",
    "downgraded",
    "platform-reduced",
    "weakened",
    "map-shrunk",
];

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    pub app: String,
    #[serde(default)]
    pub coverage_owners: Vec<String>,
    #[serde(default)]
    pub adapter: Vec<AdapterDecl>,
    #[serde(default)]
    pub journey: Vec<Journey>,
    #[serde(default)]
    pub change: Vec<Change>,
}

/// A built artifact named by its role and the cargo package or target that produces it.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRef {
    pub role: String,
    #[serde(default)]
    pub cargo_target: Option<String>,
    #[serde(default)]
    pub cargo_package: Option<String>,
    /// A prebuilt binary path relative to the app root, instead of a cargo target or package.
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterDecl {
    pub name: String,
    pub build: Vec<String>,
    pub executable: ArtifactRef,
    #[serde(default)]
    pub artifacts: Vec<ArtifactRef>,
    #[serde(default)]
    pub args: Vec<String>,
    /// Adapter source paths (relative globs). Carry-forward of an older receipt needs them; with
    /// none declared, carry-forward is refused as stale (the gate, not implemented here).
    #[serde(default)]
    pub sources: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Journey {
    pub id: String,
    pub title: String,
    pub platforms: Vec<String>,
    pub surface: String,
    pub runner: String,
    #[serde(default)]
    pub adapter: Option<String>,
    pub entry: String,
    pub tier: String,
    pub data: String,
    #[serde(default)]
    pub release_critical: bool,
}

/// A D8 tombstone approving a coverage reduction. Its shape is checked here; the approval itself
/// is fetched and verified by the gate, which is not implemented yet.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Change {
    pub id: String,
    pub kind: String,
    pub before_after_sha256: String,
    pub approval: String,
    #[serde(default)]
    pub replacement: Option<String>,
    #[serde(default)]
    pub risk_accepted: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mapping {
    pub version: u32,
    #[serde(default)]
    pub behavior: Vec<Behavior>,
    #[serde(default)]
    pub exempt: Vec<Exempt>,
    #[serde(default)]
    pub global: Option<Global>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Behavior {
    pub id: String,
    pub title: String,
    pub paths: Vec<String>,
    pub journeys: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Exempt {
    pub paths: Vec<String>,
    pub reason: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Global {
    pub paths: Vec<String>,
}

/// Outcome of [`check`]: the parsed files, their digests and every issue found.
#[derive(Debug, Clone)]
pub struct Checked {
    pub manifest: Manifest,
    pub manifest_sha256: String,
    pub mapping: Option<Mapping>,
    pub mapping_sha256: Option<String>,
    pub issues: Vec<String>,
}

impl Checked {
    pub fn ok(&self) -> bool {
        self.issues.is_empty()
    }
}

/// Parse a manifest and hash its exact bytes (the hash the receipt and `selection.json` carry).
pub fn parse_manifest(bytes: &[u8]) -> Result<(Manifest, String)> {
    let text = std::str::from_utf8(bytes)
        .map_err(|e| crate::util::Error(format!("{MANIFEST_FILE} is not UTF-8: {e}")))?;
    Ok((toml::from_str(text)?, sha256_hex(bytes)))
}

/// Parse a mapping file and hash its exact bytes.
pub fn parse_mapping(bytes: &[u8]) -> Result<(Mapping, String)> {
    let text = std::str::from_utf8(bytes)
        .map_err(|e| crate::util::Error(format!("{MAPPING_FILE} is not UTF-8: {e}")))?;
    Ok((toml::from_str(text)?, sha256_hex(bytes)))
}

/// Read `journeys.toml` (required) and `journeys-map.toml` (optional while the gate is advisory)
/// from `root` and validate them. A file that cannot be read or parsed is an `Err`; rule
/// violations are collected in `Checked::issues`.
pub fn check(root: &Path) -> Result<Checked> {
    let mpath = root.join(MANIFEST_FILE);
    let bytes = std::fs::read(&mpath)
        .map_err(|e| crate::util::Error(format!("cannot read {}: {e}", mpath.display())))?;
    let (manifest, manifest_sha256) = parse_manifest(&bytes)
        .map_err(|e| crate::util::Error(format!("{}: {e}", mpath.display())))?;
    let mut issues = check_manifest(&manifest, root);
    let map_path = root.join(MAPPING_FILE);
    let (mapping, mapping_sha256) = if map_path.is_file() {
        let b = std::fs::read(&map_path)
            .map_err(|e| crate::util::Error(format!("cannot read {}: {e}", map_path.display())))?;
        let (m, h) = parse_mapping(&b)
            .map_err(|e| crate::util::Error(format!("{}: {e}", map_path.display())))?;
        issues.extend(check_mapping(&m, &manifest));
        (Some(m), Some(h))
    } else {
        (None, None)
    };
    Ok(Checked {
        manifest,
        manifest_sha256,
        mapping,
        mapping_sha256,
        issues,
    })
}

fn id_ok(id: &str) -> bool {
    let mut chars = id.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphanumeric())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
}

fn has_glob(s: &str) -> bool {
    s.contains(['*', '?', '[', '{'])
}

fn check_artifact_ref(a: &ArtifactRef, what: &str, issues: &mut Vec<String>) {
    if a.role.trim().is_empty() {
        issues.push(format!("{what}: role is empty"));
    }
    let named = |o: &Option<String>| o.as_deref().is_some_and(|s| !s.trim().is_empty());
    if !named(&a.cargo_target) && !named(&a.cargo_package) && !named(&a.path) {
        issues.push(format!(
            "{what}: names none of cargo_target, cargo_package or path, so no built binary is declared"
        ));
    }
    if let Some(p) = a.path.as_deref() {
        if !is_safe_relative(p) {
            issues.push(format!(
                "{what}: path {p:?} must be relative and inside the app root"
            ));
        }
    }
}

/// Validate a parsed manifest against the app root (entries are looked up under `root`).
pub fn check_manifest(m: &Manifest, root: &Path) -> Vec<String> {
    let mut issues = vec![];
    if m.version != 1 {
        issues.push(format!("version must be 1, found {}", m.version));
    }
    if m.app.trim().is_empty() {
        issues.push("app is empty".into());
    }
    if m.coverage_owners.iter().any(|o| o.trim().is_empty()) {
        issues.push("coverage_owners holds an empty login".into());
    }
    if m.journey.is_empty() {
        issues.push("no [[journey]] declared: a manifest with no journeys proves nothing".into());
    }

    // Adapters.
    let mut adapters: BTreeMap<&str, &AdapterDecl> = BTreeMap::new();
    for a in &m.adapter {
        let what = format!("adapter {:?}", a.name);
        if a.name.trim().is_empty() {
            issues.push("adapter with an empty name".into());
        } else if adapters.insert(a.name.as_str(), a).is_some() {
            issues.push(format!("{what}: duplicate adapter name"));
        }
        if a.build.is_empty() || a.build.iter().any(|s| s.trim().is_empty()) {
            issues.push(format!(
                "{what}: build argv is empty or holds an empty word"
            ));
        }
        for src in &a.sources {
            if !is_safe_relative(src) {
                issues.push(format!(
                    "{what}: source {src:?} must be a relative glob inside the app root"
                ));
            }
        }
        check_artifact_ref(&a.executable, &format!("{what} executable"), &mut issues);
        let mut roles = BTreeSet::new();
        roles.insert(a.executable.role.as_str());
        for (i, art) in a.artifacts.iter().enumerate() {
            check_artifact_ref(art, &format!("{what} artifacts[{i}]"), &mut issues);
            if !roles.insert(art.role.as_str()) {
                issues.push(format!("{what}: role {:?} is declared twice", art.role));
            }
        }
    }

    // Journeys.
    let mut ids = BTreeSet::new();
    for j in &m.journey {
        let what = format!("journey {:?}", j.id);
        if !id_ok(&j.id) {
            issues.push(format!(
                "{what}: id must be letters, digits, '.', '_' or '-' and start with a letter or digit"
            ));
        }
        if !ids.insert(j.id.as_str()) {
            issues.push(format!(
                "{what}: duplicate id (ids are immutable and never reused)"
            ));
        }
        if j.title.trim().is_empty() {
            issues.push(format!("{what}: title is empty"));
        }
        if j.platforms.is_empty() {
            issues.push(format!(
                "{what}: platforms is empty, so it would never be required anywhere"
            ));
        }
        let mut seen = BTreeSet::new();
        for p in &j.platforms {
            if !PLATFORMS.contains(&p.as_str()) {
                issues.push(format!(
                    "{what}: unknown platform {p:?} (macos, windows, linux)"
                ));
            } else if !seen.insert(p.as_str()) {
                issues.push(format!("{what}: platform {p:?} listed twice"));
            }
        }
        if !SURFACES.contains(&j.surface.as_str()) {
            issues.push(format!("{what}: unknown surface {:?}", j.surface));
        }
        if !TIERS.contains(&j.tier.as_str()) {
            issues.push(format!(
                "{what}: unknown tier {:?} (fast, full, paid)",
                j.tier
            ));
        }
        if j.data == "real" {
            issues.push(format!(
                "{what}: data = \"real\" is refused; QA never runs on real user data"
            ));
        } else if !DATA.contains(&j.data.as_str()) {
            issues.push(format!(
                "{what}: unknown data {:?} (generated, seeded)",
                j.data
            ));
        }
        match j.runner.as_str() {
            "scenario" => {
                if j.adapter.is_some() {
                    issues.push(format!(
                        "{what}: runner \"scenario\" must not name an adapter"
                    ));
                }
            }
            "adapter" => match j.adapter.as_deref() {
                None => issues.push(format!(
                    "{what}: runner \"adapter\" requires adapter = \"<name>\""
                )),
                Some(name) => match adapters.get(name) {
                    None => issues.push(format!("{what}: adapter {name:?} is not declared")),
                    Some(a) => {
                        if j.surface == "native-ui" {
                            if a.artifacts.is_empty() {
                                issues.push(format!(
                                    "{what}: native-ui needs a real built binary, but adapter {name:?} declares no artifacts"
                                ));
                            }
                            // A bare mocked-IPC dev server is `web`, not proof of the app.
                            if a.build.iter().any(|w| {
                                let w = w.to_ascii_lowercase();
                                w == "vite" || w == "vitest" || w == "storybook"
                            }) {
                                issues.push(format!(
                                    "{what}: native-ui adapter {name:?} builds through a dev server (vite, vitest or storybook); a mocked-IPC server counts as surface \"web\""
                                ));
                            }
                        }
                    }
                },
            },
            other => issues.push(format!(
                "{what}: unknown runner {other:?} (scenario, adapter)"
            )),
        }
        // Entry: relative, inside the app root, and present (a glob needs its fixed prefix directory).
        if !is_safe_relative(&j.entry) {
            issues.push(format!(
                "{what}: entry {:?} must be a relative path inside the app root",
                j.entry
            ));
        } else if has_glob(&j.entry) {
            let prefix: Vec<&str> = j
                .entry
                .split(['/', '\\'])
                .take_while(|c| !has_glob(c))
                .collect();
            let dir = root.join(prefix.join("/"));
            if !dir.is_dir() {
                issues.push(format!(
                    "{what}: entry glob {:?} has no directory {}",
                    j.entry,
                    dir.display()
                ));
            }
        } else if !root.join(&j.entry).exists() {
            issues.push(format!(
                "{what}: entry {:?} does not exist under {}",
                j.entry,
                root.display()
            ));
        }
    }

    // Tombstones: shape only.
    for c in &m.change {
        let what = format!("change {:?}", c.id);
        if !CHANGE_KINDS.contains(&c.kind.as_str()) {
            issues.push(format!("{what}: unknown kind {:?}", c.kind));
        }
        if !is_hex_lower(&c.before_after_sha256, 64) {
            issues.push(format!(
                "{what}: before_after_sha256 must be 64 lowercase hex characters"
            ));
        }
        if !c.approval.starts_with("https://github.com/") || !c.approval.contains("#issuecomment-")
        {
            issues.push(format!(
                "{what}: approval must be a GitHub issue comment URL"
            ));
        }
        match c.replacement.as_deref() {
            Some(r) if !ids.contains(r) => {
                issues.push(format!(
                    "{what}: replacement {r:?} is not a declared journey"
                ));
            }
            None if !c.risk_accepted => {
                issues.push(format!("{what}: no replacement and risk_accepted is false"));
            }
            _ => {}
        }
    }
    issues
}

/// Validate the D8 mapping against the manifest's journeys.
pub fn check_mapping(map: &Mapping, m: &Manifest) -> Vec<String> {
    let mut issues = vec![];
    let ids: BTreeSet<&str> = m.journey.iter().map(|j| j.id.as_str()).collect();
    let paths_ok = |paths: &[String], what: &str, issues: &mut Vec<String>| {
        if paths.is_empty() {
            issues.push(format!("{what}: paths is empty"));
        }
        for p in paths {
            if !is_safe_relative(p) {
                issues.push(format!(
                    "{what}: path {p:?} must be a relative glob inside the app root"
                ));
            }
        }
    };
    if map.version != 1 {
        issues.push(format!(
            "{MAPPING_FILE}: version must be 1, found {}",
            map.version
        ));
    }
    if map.behavior.is_empty() {
        issues.push(format!("{MAPPING_FILE}: no [[behavior]] declared"));
    }
    let mut seen = BTreeSet::new();
    for b in &map.behavior {
        let what = format!("{MAPPING_FILE} behavior {:?}", b.id);
        if !id_ok(&b.id) {
            issues.push(format!("{what}: malformed id"));
        }
        if !seen.insert(b.id.as_str()) {
            issues.push(format!("{what}: duplicate id"));
        }
        if b.title.trim().is_empty() {
            issues.push(format!("{what}: title is empty"));
        }
        paths_ok(&b.paths, &what, &mut issues);
        if b.journeys.is_empty() {
            issues.push(format!(
                "{what}: names no journey, so the behavior is uncovered"
            ));
        }
        for j in &b.journeys {
            if !ids.contains(j.as_str()) {
                issues.push(format!(
                    "{what}: journey {j:?} is not declared in {MANIFEST_FILE}"
                ));
            }
        }
    }
    for (i, e) in map.exempt.iter().enumerate() {
        let what = format!("{MAPPING_FILE} exempt[{i}]");
        paths_ok(&e.paths, &what, &mut issues);
        if e.reason.trim().is_empty() {
            issues.push(format!("{what}: reason is empty"));
        }
    }
    if let Some(g) = &map.global {
        paths_ok(&g.paths, &format!("{MAPPING_FILE} global"), &mut issues);
    }
    issues
}

/// Look up a journey by id.
pub fn find<'a>(m: &'a Manifest, id: &str) -> Option<&'a Journey> {
    m.journey.iter().find(|j| j.id == id)
}

/// Used by callers that need a hard failure rather than a list.
pub fn require_ok(c: &Checked) -> Result<()> {
    if c.ok() {
        Ok(())
    } else {
        err(format!(
            "{} journey manifest issue(s): {}",
            c.issues.len(),
            c.issues.join("; ")
        ))
    }
}

/// Canonical JSON text: object keys sorted, no whitespace.
pub fn canonical_json(v: &serde_json::Value) -> String {
    use serde_json::Value;
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            let parts: Vec<String> = keys
                .into_iter()
                .map(|k| format!("{}:{}", Value::String(k.clone()), canonical_json(&m[k])))
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        Value::Array(a) => format!(
            "[{}]",
            a.iter().map(canonical_json).collect::<Vec<_>>().join(",")
        ),
        other => other.to_string(),
    }
}

/// The `before_after_sha256` of a D8 tombstone: SHA-256 of the canonical JSON of
/// `{id, before, after}`, where `before` and `after` are the normalized journey or map entries
/// (`null` when absent).
pub fn change_hash(id: &str, before: &serde_json::Value, after: &serde_json::Value) -> String {
    let v = serde_json::json!({"id": id, "before": before, "after": after});
    sha256_hex(canonical_json(&v).as_bytes())
}
