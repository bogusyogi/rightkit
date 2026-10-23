//! `journeys.toml` (D3) and `journeys-map.toml` (D8): types and static validation.
//!
//! [`check`] reads both files from an app root and returns every issue it finds; it does not run
//! anything. Unknown keys are refused, so a misspelled `release_critical` cannot silently turn a
//! journey into a non-critical one.
use super::build_record::{features_of, validate_build_command};
use super::lint;
use super::{is_hex_lower, is_safe_relative};
use crate::budget::{BudgetDecl, Day};
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
    /// The app's one QA build (ADR-0001 D18). Every `runner = "scenario"` journey needs it.
    #[serde(default)]
    pub build: Option<BuildDecl>,
    /// Per-tier budgets and the warn window (`journeys/budget.rs`).
    #[serde(default)]
    pub budget: Option<BudgetDecl>,
    /// The frozen untagged-test count that `journeys check` may only lower (`journeys/lint.rs`).
    #[serde(default)]
    pub tests: Option<TestsDecl>,
    /// One Harness bridge entry per bridged app (`journeys/bridge.rs`).
    #[serde(default)]
    pub bridge: Vec<BridgeDecl>,
    /// The app's step plugin (ADR-0001 D-b): a build-record binary role, started per scenario by
    /// `journeys run`. Needs `[build]`.
    #[serde(default)]
    pub plugin: Option<PluginDecl>,
}

/// Roles the runner launches itself: the engine and the UI app (`journeys run` binds them by these
/// names). A `[plugin]` may never take one of them (N3).
pub const RUNNER_LAUNCHED_ROLES: [&str; 3] = ["engine", "app", "ui"];

/// `[plugin]` in `journeys.toml`: `role` names a `[build]` artifact (its recorded binary is the
/// plugin), `args` are passed to it, `hello_timeout_ms` bounds its `hello` (default 10000, 1 to 60000).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginDecl {
    pub role: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub hello_timeout_ms: Option<u64>,
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

/// One binary the `[build]` section produces. `qa_marker` (default `true`) says whether the binary
/// must carry the QA control marker at launch. Only a binary that is not the app (an adapter's test
/// executable, an engine) sets it to `false`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildArtifact {
    pub role: String,
    #[serde(default)]
    pub cargo_target: Option<String>,
    #[serde(default)]
    pub cargo_package: Option<String>,
    /// Accepted by the schema so that the refusal is a check issue with a reason, not a parse
    /// error: the build lane does not bind a prebuilt path.
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default = "qa_marker_default")]
    pub qa_marker: bool,
}

fn qa_marker_default() -> bool {
    true
}

impl BuildArtifact {
    fn artifact_ref(&self) -> ArtifactRef {
        ArtifactRef {
            role: self.role.clone(),
            cargo_target: self.cargo_target.clone(),
            cargo_package: self.cargo_package.clone(),
            path: self.path.clone(),
        }
    }
}

/// The `[build]` table: the app's ONE QA build (ADR-0001 D18). Every `runner = "scenario"` journey
/// runs against it, so an app has one feature set and one test-mode world.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildDecl {
    /// The broker argv: `rightkit cargo build ...` or `rightkit cargo test ... --no-run ...`.
    #[serde(default)]
    pub command: Vec<String>,
    /// The QA feature set. It must equal the `--features` named in `command`.
    #[serde(default)]
    pub features: Vec<String>,
    #[serde(default)]
    pub artifacts: Vec<BuildArtifact>,
}

/// The `[tests]` table. Its `untagged_baseline` is the frozen count of untagged tests; the count may
/// fall below it but never rise above it.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestsDecl {
    pub untagged_baseline: u32,
}

/// One `[[bridge]]` entry: a bridged app whose already-built libtest binary the Harness bridge runs.
/// The entry is refused after `removal_date`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BridgeDecl {
    pub app: String,
    /// The cargo target of the libtest binary. The bridge never builds it.
    pub binary: String,
    /// Relative source files whose launching tests this entry exempts from the static lint. The
    /// lint keys exemptions by source path, so the entry needs them to exempt anything.
    #[serde(default)]
    pub sources: Vec<String>,
    /// Optional selection: the test names the entry covers. Empty means every test in `sources`.
    #[serde(default)]
    pub tests: Vec<String>,
    /// Ignored tests that must run (`--ignored`). Any other ignored test is `missing`.
    #[serde(default)]
    pub ignored_required: Vec<String>,
    /// `YYYY-MM-DD`, a real calendar day. The entry is refused on any later day.
    pub removal_date: String,
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
    /// Machine-wide resources the journey holds while it runs (`focus`, `audio`, `gui-session`,
    /// `clipboard`, `credentials`; ADR-0001 item 2). A journey that declares none touches none of
    /// them. An unknown or repeated name is a check issue, never silently ignored.
    #[serde(default)]
    pub tokens: Vec<String>,
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
    /// Failing findings of the static source lint (ADR-0001 item 9). `journeys check` fails on them;
    /// `journeys run` reports them as warnings until guardrails are enforced (ADR order step 4).
    pub guardrail_issues: Vec<String>,
    /// The static source lint, including the report-only findings (untagged tests, exempt launches).
    pub lint: lint::Report,
}

impl Checked {
    /// Manifest and guardrail findings together: what `journeys check` fails on.
    pub fn ok(&self) -> bool {
        self.issues.is_empty() && self.guardrail_issues.is_empty()
    }

    /// Manifest findings only: what a run cannot proceed past.
    pub fn ok_for_run(&self) -> bool {
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
    let (guardrail_issues, lint_report) = check_lint(&manifest, root, &today())?;
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
        guardrail_issues,
        lint: lint_report,
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
        if let Err(e) = crate::pool::parse_tokens(&j.tokens) {
            issues.push(format!("{what}: {}", e.0));
        }
        match j.runner.as_str() {
            "scenario" => {
                if j.adapter.is_some() {
                    issues.push(format!(
                        "{what}: runner \"scenario\" must not name an adapter"
                    ));
                }
                if m.build.is_none() {
                    issues.push(format!(
                        "{what}: runner \"scenario\" needs a [build] section, the app's one QA build"
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

    // `[build]`: the one QA build (command, feature set, artifacts).
    if let Some(b) = &m.build {
        check_build(b, &mut issues);
    }

    // `[plugin]`: the role must be a `[build]` artifact, so the plugin binary is a recorded binary.
    if let Some(p) = &m.plugin {
        let roles: BTreeSet<&str> = m
            .build
            .as_ref()
            .map(|b| b.artifacts.iter().map(|a| a.role.as_str()).collect())
            .unwrap_or_default();
        if m.build.is_none() {
            issues.push(
                "[plugin] needs a [build] section: the plugin is a build-record binary".into(),
            );
        } else if !roles.contains(p.role.as_str()) {
            issues.push(format!(
                "[plugin] role {:?} is not a [build] artifact role",
                p.role
            ));
        }
        if !role_name_ok(&p.role) {
            issues.push(format!(
                "[plugin] role {:?} is not a valid role name",
                p.role
            ));
        }
        // N3: the plugin is a separate binary. The engine, the UI app and any binary the runner
        // launches itself (a QA-marked artifact) are refused, so a plugin can neither run an engine
        // outside the engine count nor a UI outside the guard.
        if RUNNER_LAUNCHED_ROLES.contains(&p.role.as_str()) {
            issues.push(format!(
                "[plugin] role {:?} is launched by the runner (engine or UI); a plugin must be its own binary",
                p.role
            ));
        } else if let Some(art) = m
            .build
            .as_ref()
            .and_then(|b| b.artifacts.iter().find(|a| a.role == p.role))
        {
            if art.qa_marker {
                issues.push(format!(
                    "[plugin] role {:?} is QA-marked (launched by the runner); set qa_marker = false on the plugin artifact",
                    p.role
                ));
            }
        }
        if let Some(ms) = p.hello_timeout_ms {
            if !(1..=60_000).contains(&ms) {
                issues.push("[plugin] hello_timeout_ms must be between 1 and 60000".into());
            }
        }
    }

    // `[tests]`: without a frozen baseline the untagged-test count has no ratchet, so an app that
    // runs scenario journeys must declare one.
    if m.tests.is_none() && m.journey.iter().any(|j| j.runner == "scenario") {
        issues.push(
            "[tests] untagged_baseline is required for an app with runner \"scenario\" journeys (the frozen untagged-test count)".into(),
        );
    }

    // `[budget]`: the warn window and the per-tier budgets.
    if let Some(b) = &m.budget {
        issues.extend(b.issues());
    }

    // `[[bridge]]`: one entry per app, with a real removal date and usable test names.
    let mut bridged = BTreeSet::new();
    for b in &m.bridge {
        let what = format!("[[bridge]] app {:?}", b.app);
        if b.app.trim().is_empty() {
            issues.push("[[bridge]] with an empty app name".into());
        } else if !bridged.insert(b.app.as_str()) {
            issues.push(format!("{what}: duplicate app (one bridge entry per app)"));
        }
        if !id_ok(&b.binary) {
            issues.push(format!(
                "{what}: binary {:?} must be a cargo target name",
                b.binary
            ));
        }
        if let Err(e) = Day::parse(&b.removal_date) {
            issues.push(format!(
                "{what}: removal_date {:?} is not a real YYYY-MM-DD day: {}",
                b.removal_date, e.0
            ));
        }
        for n in b.tests.iter().chain(&b.ignored_required) {
            if !test_name_ok(n) {
                issues.push(format!("{what}: {n:?} is not a usable test name"));
            }
        }
        for s in &b.sources {
            if !is_safe_relative(s) {
                issues.push(format!(
                    "{what}: source {s:?} must be a relative path inside the app root"
                ));
            } else if has_glob(s) {
                issues.push(format!(
                    "{what}: source {s:?} must be a literal file path (the lint matches exact paths)"
                ));
            } else if !root.join(s).is_file() {
                issues.push(format!(
                    "{what}: source {s:?} does not exist under {}",
                    root.display()
                ));
            }
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

/// `[build]`: the broker command, the feature set and the artifacts. The command is checked by the
/// build lane's own validator, so `journeys check` and `journeys run` refuse the same commands.
fn check_build(b: &BuildDecl, issues: &mut Vec<String>) {
    match validate_build_command(&b.command) {
        Err(e) => issues.push(format!("[build] command: {}", e.0)),
        Ok(_) => {
            let asked = features_of(&b.command);
            if asked.all_features || asked.no_default_features {
                issues.push(
                    "[build] command names --all-features or --no-default-features; name the QA features with --features so the manifest can list them".into(),
                );
            }
            let mut declared: Vec<&str> = b.features.iter().map(String::as_str).collect();
            declared.sort_unstable();
            declared.dedup();
            let asked: Vec<&str> = asked.requested.iter().map(String::as_str).collect();
            if declared != asked {
                issues.push(format!(
                    "[build] features {declared:?} must equal the --features of the command {asked:?}: one QA feature set per app"
                ));
            }
        }
    }
    if b.artifacts.is_empty() {
        issues.push("[build] declares no artifacts, so no binary is bound to the build".into());
    }
    let mut roles = BTreeSet::new();
    for (i, a) in b.artifacts.iter().enumerate() {
        let what = format!("[build] artifacts[{i}]");
        if !role_name_ok(&a.role) {
            issues.push(format!(
                "{what}: role {:?} must be letters, digits, '.', '_' or '-' (at most 64) and not start with '.'",
                a.role
            ));
        }
        if !roles.insert(a.role.as_str()) {
            issues.push(format!("{what}: role {:?} is declared twice", a.role));
        }
        check_artifact_ref(&a.artifact_ref(), &what, issues);
        let built = has_name(&a.cargo_target) || has_name(&a.cargo_package);
        if !built && has_name(&a.path) {
            issues.push(format!(
                "{what}: names only a path, but the build lane binds a binary only by cargo_target or cargo_package; a prebuilt path is not produced by this build"
            ));
        }
    }
}

fn has_name(o: &Option<String>) -> bool {
    o.as_deref().is_some_and(|s| !s.trim().is_empty())
}

/// Mirrors `build_record`'s artifact role rule (that function is private to its module).
fn role_name_ok(role: &str) -> bool {
    !role.is_empty()
        && role.len() <= 64
        && !role.starts_with('.')
        && role
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
}

/// A test name the bridge can pass as a libtest filter (mirrors `journeys/bridge.rs`).
fn test_name_ok(name: &str) -> bool {
    !name.is_empty() && name.trim() == name && !name.starts_with('-') && !name.contains('\0')
}

/// Today's UTC date as `YYYY-MM-DD`.
fn today() -> String {
    crate::util::now_iso().chars().take(10).collect()
}

/// Runs the static source lint (`journeys/lint.rs`) over every `.rs` file under `root`, with the
/// manifest's frozen baseline, bridge entries and scenario token declarations. Each `Fail` finding
/// becomes an issue; report-only findings (untagged tests, exempt launches) stay in the report.
/// `today` is `YYYY-MM-DD`, passed in so that bridge expiry can be tested on fixed dates.
pub fn check_lint(m: &Manifest, root: &Path, today: &str) -> Result<(Vec<String>, lint::Report)> {
    let mut issues = Vec::new();
    let mut files = Vec::new();
    collect_rust_sources(root, root, &mut files)?;
    let mut bridge = Vec::new();
    for b in &m.bridge {
        for src in &b.sources {
            if b.tests.is_empty() {
                bridge.push(lint::BridgeEntry {
                    path: src.clone(),
                    test: None,
                    removal: b.removal_date.clone(),
                });
            }
            for t in &b.tests {
                bridge.push(lint::BridgeEntry {
                    path: src.clone(),
                    test: Some(t.clone()),
                    removal: b.removal_date.clone(),
                });
            }
        }
    }
    let opts = lint::LintOptions {
        today: today.to_string(),
        baseline_untagged: m.tests.as_ref().map(|t| t.untagged_baseline as usize),
        bridge,
        scenarios: scenario_declarations(m, root, &mut issues),
    };
    let report = lint::lint_sources(&files, &opts);
    for f in report
        .findings
        .iter()
        .filter(|f| f.severity == lint::Severity::Fail)
    {
        let at = if f.line == 0 {
            f.path.clone()
        } else {
            format!("{}:{}", f.path, f.line)
        };
        let subject = f
            .subject
            .as_deref()
            .map(|s| format!(" `{s}`"))
            .unwrap_or_default();
        issues.push(format!("lint {} at {at}{subject}: {}", f.rule, f.message));
    }
    Ok((issues, report))
}

/// Every `.rs` file under `dir` as a `root`-relative path with its text. Directories named
/// `target` or `node_modules` and hidden directories are skipped, and so are symlinks.
fn collect_rust_sources(root: &Path, dir: &Path, out: &mut Vec<lint::SourceFile>) -> Result<()> {
    let read_err =
        |e: std::io::Error| crate::util::Error(format!("cannot read {}: {e}", dir.display()));
    let mut entries = std::fs::read_dir(dir)
        .map_err(read_err)?
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(read_err)?;
    entries.sort_by_key(|e| e.path());
    for e in entries {
        let path = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        let kind = e
            .file_type()
            .map_err(|err| crate::util::Error(format!("cannot stat {}: {err}", path.display())))?;
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            if name.starts_with('.') || name == "target" || name == "node_modules" {
                continue;
            }
            collect_rust_sources(root, &path, out)?;
        } else if name.ends_with(".rs") {
            if let Ok(text) = std::fs::read_to_string(&path) {
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push(lint::SourceFile { path: rel, text });
            }
        }
    }
    Ok(())
}

/// The fields of a scenario file the token lint reads. Read directly, not through the scenario
/// loader, so an undeclared or unknown token is a lint finding rather than a load error.
#[derive(Deserialize)]
struct RawScenario {
    name: String,
    #[serde(default)]
    tokens: Vec<String>,
    #[serde(default, rename = "step")]
    steps: Vec<RawStep>,
}

#[derive(Deserialize)]
struct RawStep {
    kind: String,
}

/// The token declarations and step kinds of every scenario file that a `runner = "scenario"`
/// journey names.
fn scenario_declarations(
    m: &Manifest,
    root: &Path,
    issues: &mut Vec<String>,
) -> Vec<lint::ScenarioTokens> {
    let mut out = Vec::new();
    for j in m.journey.iter().filter(|j| j.runner == "scenario") {
        let what = format!("journey {:?}", j.id);
        for rel in scenario_files(root, &j.entry, &what, issues) {
            let text = match std::fs::read_to_string(root.join(&rel)) {
                Ok(t) => t,
                Err(e) => {
                    issues.push(format!("{what}: cannot read {rel}: {e}"));
                    continue;
                }
            };
            match toml::from_str::<RawScenario>(&text) {
                Ok(s) => out.push(lint::ScenarioTokens {
                    path: rel,
                    id: s.name,
                    declared: s.tokens,
                    steps: s.steps.into_iter().map(|s| s.kind).collect(),
                }),
                Err(e) => issues.push(format!("{what}: {rel} is not a scenario file: {e}")),
            }
        }
    }
    out
}

/// The scenario files one journey entry names, relative to `root`: a literal file, or the files
/// that a glob matches in its last component (`tests/scenarios/*.scenario.toml`). A missing literal
/// file is already a manifest issue, so it yields nothing here.
fn scenario_files(root: &Path, entry: &str, what: &str, issues: &mut Vec<String>) -> Vec<String> {
    if !is_safe_relative(entry) {
        return vec![];
    }
    if !has_glob(entry) {
        return if root.join(entry).is_file() {
            vec![entry.to_string()]
        } else {
            vec![]
        };
    }
    let parts: Vec<&str> = entry.split(['/', '\\']).collect();
    let fixed = parts.iter().take_while(|c| !has_glob(c)).count();
    let (dir_parts, rest) = parts.split_at(fixed);
    let pattern = match rest {
        [last] if !last.contains(['[', '{']) => *last,
        _ => {
            issues.push(format!(
                "{what}: entry glob {entry:?} is too complex for the scenario lint; use one '*' or '?' in its last component"
            ));
            return vec![];
        }
    };
    let prefix = dir_parts.join("/");
    let Ok(read) = std::fs::read_dir(root.join(&prefix)) else {
        return vec![];
    };
    let mut names: Vec<String> = read
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| wild_match(pattern.as_bytes(), n.as_bytes()))
        .collect();
    names.sort();
    if names.is_empty() {
        issues.push(format!(
            "{what}: entry glob {entry:?} matches no scenario file"
        ));
    }
    names
        .into_iter()
        .map(|n| {
            if prefix.is_empty() {
                n
            } else {
                format!("{prefix}/{n}")
            }
        })
        .collect()
}

/// `*` (any run of characters) and `?` (one character), matched over the whole name.
fn wild_match(pat: &[u8], s: &[u8]) -> bool {
    match (pat.first().copied(), s.first().copied()) {
        (None, None) => true,
        (Some(b'*'), _) => wild_match(&pat[1..], s) || (!s.is_empty() && wild_match(pat, &s[1..])),
        (Some(b'?'), Some(_)) => wild_match(&pat[1..], &s[1..]),
        (Some(p), Some(c)) if p == c => wild_match(&pat[1..], &s[1..]),
        _ => false,
    }
}

/// Look up a journey by id.
pub fn find<'a>(m: &'a Manifest, id: &str) -> Option<&'a Journey> {
    m.journey.iter().find(|j| j.id == id)
}

/// Used by callers that need a hard failure rather than a list: manifest and guardrail findings.
pub fn require_ok(c: &Checked) -> Result<()> {
    if c.ok() {
        Ok(())
    } else {
        let all: Vec<&str> = c
            .issues
            .iter()
            .chain(c.guardrail_issues.iter())
            .map(String::as_str)
            .collect();
        err(format!(
            "{} journey manifest issue(s): {}",
            all.len(),
            all.join("; ")
        ))
    }
}

/// A run stops on manifest findings only. Guardrail findings come back as warnings for the run to
/// print and record until guardrails are enforced (ADR-0001 order step 4).
pub fn require_ok_for_run(c: &Checked) -> Result<Vec<String>> {
    if c.ok_for_run() {
        Ok(c.guardrail_issues.clone())
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

#[cfg(test)]
mod token_tests {
    use super::*;

    fn issues_for(tokens: &str) -> Vec<String> {
        let text = format!(
            "version = 1\napp = \"demo\"\n[[journey]]\nid = \"J-1\"\ntitle = \"t\"\nplatforms = [\"windows\"]\nsurface = \"engine\"\nrunner = \"scenario\"\nentry = \"tests/j.scenario.toml\"\ntier = \"fast\"\ndata = \"generated\"\n{tokens}\n"
        );
        let (m, _) = parse_manifest(text.as_bytes()).unwrap();
        check_manifest(&m, Path::new("/nonexistent-app-root"))
    }

    #[test]
    fn a_journey_declares_known_tokens_and_unknown_or_repeated_ones_are_issues() {
        let mentions = |issues: &[String]| issues.iter().any(|i| i.contains("token"));
        assert!(!mentions(&issues_for("")));
        assert!(!mentions(&issues_for(
            "tokens = [\"focus\", \"gui-session\"]"
        )));
        let bad = issues_for("tokens = [\"fcus\"]");
        assert!(
            bad.iter()
                .any(|i| i.contains("J-1") && i.contains("unknown resource token 'fcus'")),
            "{bad:?}"
        );
        let twice = issues_for("tokens = [\"audio\", \"audio\"]");
        assert!(
            twice.iter().any(|i| i.contains("declared twice")),
            "{twice:?}"
        );
    }
}

#[cfg(test)]
mod plugin_tests {
    use super::*;

    fn issues_for(extra: &str) -> Vec<String> {
        let text = format!("version = 1\napp = \"demo\"\n{extra}\n");
        let (m, _) = parse_manifest(text.as_bytes()).unwrap();
        check_manifest(&m, Path::new("/nonexistent-app-root"))
    }

    /// A build with the app (QA-marked) and a plugin artifact that is not QA-marked.
    const BUILD_WITH_PLUGIN: &str = "[build]\ncommand = [\"rightkit\", \"cargo\", \"build\", \"-p\", \"fxapp\", \"--features\", \"qa\"]\nfeatures = [\"qa\"]\nartifacts = [{ role = \"app\", cargo_package = \"fxapp\" }, { role = \"plugin\", cargo_package = \"fxplug\", qa_marker = false }]\n";

    #[test]
    fn a_plugin_names_a_build_record_role_and_a_bad_declaration_is_an_issue() {
        let ok = issues_for(&format!(
            "{BUILD_WITH_PLUGIN}[plugin]\nrole = \"plugin\"\nargs = [\"--qa\"]\nhello_timeout_ms = 5000\n"
        ));
        assert!(!ok.iter().any(|i| i.contains("[plugin]")), "{ok:?}");
        let not_built = issues_for(&format!("{BUILD_WITH_PLUGIN}[plugin]\nrole = \"ghost\"\n"));
        assert!(
            not_built
                .iter()
                .any(|i| i.contains("[plugin] role \"ghost\"")),
            "{not_built:?}"
        );
        let no_build = issues_for("[plugin]\nrole = \"plugin\"\n");
        assert!(
            no_build.iter().any(|i| i.contains("needs a [build]")),
            "{no_build:?}"
        );
        let slow = issues_for(&format!(
            "{BUILD_WITH_PLUGIN}[plugin]\nrole = \"plugin\"\nhello_timeout_ms = 99999\n"
        ));
        assert!(
            slow.iter().any(|i| i.contains("hello_timeout_ms")),
            "{slow:?}"
        );
        // An unknown key is refused by the parser, never silently ignored.
        assert!(parse_manifest(
            format!("version = 1\napp = \"demo\"\n{BUILD_WITH_PLUGIN}[plugin]\nrole = \"plugin\"\nargv = []\n")
                .as_bytes()
        )
        .is_err());
    }

    #[test]
    fn a_plugin_may_not_be_the_engine_the_ui_app_or_a_runner_launched_binary() {
        // N3: `engine` and `app` are launched by the runner by name, and a QA-marked artifact (the
        // default) is launched by it too; each is an issue, and a separate unmarked binary is not.
        let engine = issues_for(&format!("{BUILD_WITH_PLUGIN}[plugin]\nrole = \"engine\"\n"));
        assert!(
            engine
                .iter()
                .any(|i| i.contains("[plugin] role \"engine\"")),
            "{engine:?}"
        );
        let app = issues_for(&format!("{BUILD_WITH_PLUGIN}[plugin]\nrole = \"app\"\n"));
        assert!(
            app.iter().any(|i| i.contains("[plugin] role \"app\"")),
            "{app:?}"
        );
        let marked = issues_for(
            "[build]\ncommand = [\"rightkit\", \"cargo\", \"build\"]\nartifacts = [{ role = \"helper\", cargo_package = \"h\" }]\n[plugin]\nrole = \"helper\"\n",
        );
        assert!(marked.iter().any(|i| i.contains("QA-marked")), "{marked:?}");
    }
}
