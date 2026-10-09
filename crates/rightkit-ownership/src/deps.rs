//! Direct dependency extraction from manifests (Cargo.toml, package.json,
//! Package.swift) and lock-file version resolution. Port of
//! `dependenciesFromInventory`, `parseCargo`, `resolveLocalPath` and
//! `resolveDependencyVersions` in `ownership.mjs`.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use fancy_regex::Regex;
use serde_json::Value;

use crate::committed::Bytes;
use crate::error::{Res, ScanError};
use crate::inventory::File;
use crate::json::{field, items, sha256_hex, str_of, truthy};
use crate::lockfiles::{parse_toml, ContentDigest, OWNERSHIP_LOCKFILES};
use crate::paths::{self, posix_basename, posix_dirname, posix_join, posix_normalize};
use crate::types::Cov;

/// Committed-tree extras: the full repository listing, for local path checks and lock reads.
pub struct CommittedInfo {
    pub path_prefix: String,
    pub repository_files: Vec<RepoFile>,
}

pub struct RepoFile {
    pub rel: String,
    pub bytes: Option<Bytes>,
}

/// The inventory a scan runs over: working-tree or committed.
pub struct ScanInv {
    pub files: Vec<File>,
    pub coverage: Vec<Cov>,
    pub committed: Option<CommittedInfo>,
}

#[derive(Debug, Clone)]
pub struct Dep {
    pub ecosystem: &'static str,
    pub name: String,
    pub file: String,
    pub alias: Option<String>,
    pub section: String,
    pub requirement: Option<String>,
    pub features: Vec<String>,
    pub default_features: Value,
    pub resolved_version: Option<String>,
    pub local: bool,
    pub source: String,
}

pub struct DepsResult {
    pub deps: Vec<Dep>,
    pub coverage: Vec<Cov>,
    pub content_digests: Vec<ContentDigest>,
    pub manifests: Vec<File>,
}

const MANIFEST_NAMES: [&str; 3] = ["Cargo.toml", "package.json", "Package.swift"];
const NPM_FIELDS: [&str; 4] = [
    "dependencies",
    "devDependencies",
    "optionalDependencies",
    "peerDependencies",
];

fn input_digest(file: &File) -> String {
    file.digest
        .clone()
        .unwrap_or_else(|| sha256_hex(file.bytes.as_deref().map_or(&[][..], |b| b.as_slice())))
}

fn win32_absolute(path: &str) -> bool {
    let b = path.as_bytes();
    path.starts_with('/')
        || path.starts_with('\\')
        || (b.len() >= 3
            && b[0].is_ascii_alphabetic()
            && b[1] == b':'
            && matches!(b[2], b'/' | b'\\'))
}

struct Resolve<'a> {
    root: &'a Path,
    inventory: &'a ScanInv,
}

impl Resolve<'_> {
    #[allow(clippy::too_many_arguments)]
    fn local_path(
        &self,
        source_file: &str,
        path_value: &str,
        coverage: &mut Vec<Cov>,
        display: &str,
        reason_prefix: &str,
        detail: &str,
    ) {
        if path_value.is_empty() {
            return;
        }
        let suffix = if detail.is_empty() {
            String::new()
        } else {
            format!(":{detail}")
        };
        let outside = |coverage: &mut Vec<Cov>| {
            coverage.push(Cov::new(
                display,
                format!("{reason_prefix}-outside-repository{suffix}"),
            ))
        };
        let unresolved = |coverage: &mut Vec<Cov>| {
            coverage.push(Cov::new(
                display,
                format!("{reason_prefix}-path-unresolved{suffix}"),
            ))
        };
        if let Some(committed) = &self.inventory.committed {
            if path_value.starts_with('/')
                || win32_absolute(path_value)
                || path_value.contains('\\')
            {
                return outside(coverage);
            }
            let relative = posix_normalize(&posix_join(&[&posix_dirname(source_file), path_value]));
            if relative == ".." || relative.starts_with("../") || relative.starts_with('/') {
                return outside(coverage);
            }
            let tail = if relative == "." {
                String::new()
            } else {
                format!("{relative}/")
            };
            let rel = format!("{}{}", committed.path_prefix, tail);
            let rel = rel.strip_suffix('/').unwrap_or(&rel).to_string();
            let exists = relative == "."
                || committed
                    .repository_files
                    .iter()
                    .any(|f| f.rel == rel || f.rel.starts_with(&format!("{rel}/")));
            if !exists {
                unresolved(coverage);
            }
            return;
        }
        let resolved = paths::resolve(&self.root.join(posix_dirname(source_file)).join(path_value));
        match (paths::real(self.root), paths::real(&resolved)) {
            (Ok(root), Ok(target)) => {
                if target != root && !target.starts_with(&root) {
                    outside(coverage);
                }
            }
            _ => unresolved(coverage),
        }
    }
}

fn js_str(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

fn first<'a>(values: &[&'a Value]) -> &'a Value {
    static NULL: Value = Value::Null;
    values
        .iter()
        .copied()
        .find(|v| !v.is_null())
        .unwrap_or(&NULL)
}

struct CargoWorkspace {
    dir: String,
    deps: Value,
}

#[allow(clippy::too_many_arguments)]
fn parse_cargo(
    parsed: &Value,
    file: &str,
    out: &mut Vec<Dep>,
    workspace: &CargoWorkspace,
    resolver: &Resolve<'_>,
    coverage: &mut Vec<Cov>,
) -> Res<()> {
    fn add_table(
        table: &Value,
        prefix: &str,
        file: &str,
        out: &mut Vec<Dep>,
        workspace: &CargoWorkspace,
        resolver: &Resolve<'_>,
        coverage: &mut Vec<Cov>,
    ) -> Res<()> {
        if !table.is_object() {
            return Ok(());
        }
        for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
            let Some(entries) = field(table, section).as_object() else {
                continue;
            };
            for (alias, raw) in entries {
                let spec_value = if raw.is_object() {
                    raw.clone()
                } else {
                    Value::Null
                };
                let spec = &spec_value;
                let workspace_true = field(spec, "workspace").as_bool() == Some(true);
                let inherited_raw = if workspace_true {
                    workspace.deps.get(alias.as_str())
                } else {
                    None
                };
                if workspace_true && inherited_raw.is_none() {
                    return Err(ScanError::new(format!(
                        "missing workspace dependency: {alias}"
                    )));
                }
                let inherited_owned = match inherited_raw {
                    Some(Value::String(version)) => serde_json::json!({"version": version}),
                    Some(other) if truthy(other) => other.clone(),
                    _ => Value::Null,
                };
                let inherited = &inherited_owned;
                let name = js_str(first(&[
                    field(spec, "package"),
                    field(inherited, "package"),
                ]))
                .unwrap_or_else(|| alias.clone());
                let path_value = first(&[field(spec, "path"), field(inherited, "path")]);
                let git = first(&[field(spec, "git"), field(inherited, "git")]);
                let selector = first(&[
                    field(spec, "rev"),
                    field(inherited, "rev"),
                    field(spec, "branch"),
                    field(inherited, "branch"),
                    field(spec, "tag"),
                    field(inherited, "tag"),
                ]);
                let path_text = if truthy(path_value) {
                    Some(js_text(path_value))
                } else {
                    None
                };
                let git_text = if truthy(git) {
                    Some(js_text(git))
                } else {
                    None
                };
                let source = if let Some(p) = &path_text {
                    format!("path:{p}")
                } else if let Some(g) = &git_text {
                    format!(
                        "git+{g}{}",
                        if truthy(selector) {
                            format!("#{}", js_text(selector))
                        } else {
                            String::new()
                        }
                    )
                } else {
                    "registry".to_string()
                };
                let requirement_value = if raw.is_string() {
                    raw.clone()
                } else {
                    first(&[field(spec, "version"), field(inherited, "version")]).clone()
                };
                let features_bad = (!field(spec, "features").is_null()
                    && !field(spec, "features").is_array())
                    || (!field(inherited, "features").is_null()
                        && !field(inherited, "features").is_array());
                if features_bad {
                    return Err(ScanError::new(format!(
                        "malformed dependency features: {alias}"
                    )));
                }
                let mut feature_values: Vec<Value> = Vec::new();
                for v in items(field(inherited, "features"))
                    .iter()
                    .chain(items(field(spec, "features")))
                {
                    if !feature_values.contains(v) {
                        feature_values.push(v.clone());
                    }
                }
                feature_values.sort_by_key(js_text);
                let raw_ok = truthy(raw) && (raw.is_string() || (raw.is_object()));
                let features_strings = feature_values.iter().all(Value::is_string);
                let requirement_ok = requirement_value.is_null() || requirement_value.is_string();
                if !raw_ok || !features_strings || !requirement_ok {
                    return Err(ScanError::new(format!(
                        "malformed dependency declaration: {alias}"
                    )));
                }
                if !name.is_empty() {
                    out.push(Dep {
                        ecosystem: "cargo",
                        name: name.clone(),
                        file: file.into(),
                        alias: if name == *alias {
                            None
                        } else {
                            Some(alias.clone())
                        },
                        section: format!("{prefix}{section}"),
                        requirement: js_str(&requirement_value),
                        features: feature_values
                            .iter()
                            .map(|v| str_of(v).to_string())
                            .collect(),
                        default_features: first(&[
                            field(spec, "default-features"),
                            field(inherited, "default-features"),
                        ])
                        .clone(),
                        resolved_version: None,
                        local: path_text.is_some() || git_text.is_some(),
                        source,
                    });
                }
                if let Some(p) = &path_text {
                    let source_file = if truthy(field(spec, "path")) {
                        file.to_string()
                    } else {
                        posix_join(&[&workspace.dir, "Cargo.toml"])
                    };
                    resolver.local_path(&source_file, p, coverage, file, "local-dependency", "");
                }
                if git_text.is_some() {
                    coverage.push(Cov::new(file, format!("git-dependency-source:{name}")));
                }
            }
        }
        if let Some(targets) = field(table, "target").as_object() {
            for (target, value) in targets {
                add_table(
                    value,
                    &format!("{prefix}target.'{target}'."),
                    file,
                    out,
                    workspace,
                    resolver,
                    coverage,
                )?;
            }
        }
        Ok(())
    }
    add_table(parsed, "", file, out, workspace, resolver, coverage)
}

/// JavaScript `String(value)` for scalars.
fn js_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => "null".into(),
        Value::Object(_) => "[object Object]".into(),
        other => other.to_string(),
    }
}

fn swift_package_arguments(text: &str) -> Res<Vec<String>> {
    let re = Regex::new(r"\.package\s*\(").expect("static regex");
    let mut args = Vec::new();
    for found in re.find_iter(text) {
        let Ok(found) = found else { break };
        let start = found.end();
        let bytes = text.as_bytes();
        let (mut at, mut depth, mut quote) = (start, 1i32, false);
        while at < bytes.len() && depth > 0 {
            if bytes[at] == b'\\' && quote {
                at += 2;
                continue;
            }
            if bytes[at] == b'"' {
                quote = !quote;
            }
            if !quote && bytes[at] == b'(' {
                depth += 1;
            }
            if !quote && bytes[at] == b')' {
                depth -= 1;
            }
            at += 1;
        }
        if depth > 0 {
            return Err(ScanError::new("unterminated Swift package declaration"));
        }
        args.push(text[start..at - 1].to_string());
    }
    Ok(args)
}

/// `^(?:npm:)(@[^/]+\/[^@]+|[^@]+)(?:@|$)` capture.
fn npm_alias(spec: &str) -> Option<String> {
    let rest = spec.strip_prefix("npm:")?;
    if let Some(scoped) = rest.strip_prefix('@') {
        if let Some((scope, tail)) = scoped.split_once('/') {
            let name_tail: &str = tail.split('@').next().unwrap_or("");
            if !scope.is_empty() && !name_tail.is_empty() {
                return Some(format!("@{scope}/{name_tail}"));
            }
        }
        return None;
    }
    let name = rest.split('@').next().unwrap_or("");
    (!name.is_empty()).then(|| name.to_string())
}

fn is_git_spec(spec: &str) -> bool {
    let lower = spec.to_lowercase();
    lower.starts_with("git+")
        || ((lower.starts_with("http:") || lower.starts_with("https:"))
            && lower
                .split_once(':')
                .is_some_and(|(_, rest)| rest.contains(".git")))
}

pub fn dependencies_from_inventory(app_root: &Path, inventory: &ScanInv) -> Res<DepsResult> {
    let mut deps: Vec<Dep> = Vec::new();
    let mut coverage: Vec<Cov> = Vec::new();
    let mut content_digests: Vec<ContentDigest> = Vec::new();
    let resolver = Resolve {
        root: app_root,
        inventory,
    };
    let manifests: Vec<&File> = inventory
        .files
        .iter()
        .filter(|x| MANIFEST_NAMES.contains(&posix_basename(&x.rel)))
        .collect();
    let primary: Vec<&File> = manifests
        .iter()
        .copied()
        .filter(|x| !x.in_node_modules)
        .collect();
    let mut selected: HashSet<&str> = primary.iter().map(|x| x.rel.as_str()).collect();
    let mut visited: HashSet<String> = HashSet::new();
    let mut queue: Vec<&File> = primary
        .iter()
        .copied()
        .filter(|x| posix_basename(&x.rel) == "package.json")
        .collect();
    let mut cursor = 0;
    while cursor < queue.len() {
        let manifest = queue[cursor];
        cursor += 1;
        let Some(bytes) = &manifest.bytes else {
            continue;
        };
        let Ok(pkg) = serde_json::from_slice::<Value>(bytes) else {
            continue;
        };
        for field_name in NPM_FIELDS {
            let Some(map) = field(&pkg, field_name).as_object() else {
                continue;
            };
            for spec in map.values() {
                let Some(spec) = spec.as_str() else { continue };
                let local = spec
                    .strip_prefix("file:")
                    .or_else(|| spec.strip_prefix("link:"))
                    .filter(|l| !l.is_empty());
                let Some(local) = local else { continue };
                let target =
                    paths::resolve(&app_root.join(posix_dirname(&manifest.rel)).join(local));
                let relative = paths::relative_posix(app_root, &target).unwrap_or_default();
                let relative = relative.trim_end_matches('/').to_string();
                if !visited.insert(relative.clone()) {
                    continue;
                }
                for x in &manifests {
                    let base = posix_basename(&x.rel);
                    if x.rel == format!("{relative}/package.json")
                        || (x.rel.starts_with(&format!("{relative}/"))
                            && MANIFEST_NAMES.contains(&base))
                    {
                        selected.insert(x.rel.as_str());
                        if base == "package.json" {
                            queue.push(x);
                        }
                    }
                }
            }
        }
    }
    let files: Vec<&File> = manifests
        .iter()
        .copied()
        .filter(|x| selected.contains(x.rel.as_str()))
        .collect();
    let mut workspaces: Vec<CargoWorkspace> = Vec::new();
    for x in files
        .iter()
        .filter(|x| posix_basename(&x.rel) == "Cargo.toml")
    {
        let parsed = x
            .bytes
            .as_ref()
            .ok_or(())
            .and_then(|b| parse_toml(&String::from_utf8_lossy(b)).map_err(|_| ()));
        match parsed {
            Ok(parsed) => {
                let deps_value = field(field(&parsed, "workspace"), "dependencies");
                if deps_value.as_object().is_some_and(|m| !m.is_empty()) {
                    workspaces.push(CargoWorkspace {
                        dir: posix_dirname(&x.rel),
                        deps: deps_value.clone(),
                    });
                }
            }
            Err(()) => coverage.push(Cov::new(x.rel.clone(), "invalid-cargo-toml")),
        }
    }
    for x in &files {
        let Some(bytes) = &x.bytes else { continue };
        let rel = &x.rel;
        let base = posix_basename(rel);
        let text = String::from_utf8_lossy(bytes).into_owned();
        content_digests.push(ContentDigest {
            path: rel.clone(),
            digest: input_digest(x),
            provenance: None,
        });
        if base == "Cargo.toml" {
            let dir = posix_dirname(rel);
            let mut candidates: Vec<&CargoWorkspace> = workspaces
                .iter()
                .filter(|w| w.dir == "." || dir == w.dir || dir.starts_with(&format!("{}/", w.dir)))
                .collect();
            candidates.sort_by_key(|w| std::cmp::Reverse(w.dir.len()));
            let fallback = CargoWorkspace {
                dir: ".".into(),
                deps: Value::Object(Default::default()),
            };
            let workspace = candidates.first().copied().unwrap_or(&fallback);
            let outcome = parse_toml(&text).map_err(|_| ()).and_then(|parsed| {
                parse_cargo(&parsed, rel, &mut deps, workspace, &resolver, &mut coverage)
                    .map_err(|_| ())
            });
            if outcome.is_err() {
                coverage.push(Cov::new(rel.clone(), "invalid-cargo-toml"));
            }
        } else if base == "package.json" {
            let Ok(pkg) = serde_json::from_str::<Value>(&text) else {
                coverage.push(Cov::new(rel.clone(), "invalid-json"));
                continue;
            };
            for field_name in NPM_FIELDS {
                let Some(map) = field(&pkg, field_name).as_object() else {
                    continue;
                };
                for (name, spec) in map {
                    let Some(spec) = spec.as_str() else {
                        coverage.push(Cov::new(
                            rel.clone(),
                            format!("invalid-npm-dependency:{name}"),
                        ));
                        continue;
                    };
                    let alias = npm_alias(spec);
                    let local_path = spec
                        .strip_prefix("file:")
                        .or_else(|| spec.strip_prefix("link:"))
                        .filter(|l| !l.is_empty());
                    let is_git = is_git_spec(spec);
                    let source = if let Some(local) = local_path {
                        format!("path:{local}")
                    } else if is_git {
                        if spec.starts_with("git+") {
                            spec.to_string()
                        } else {
                            format!("git+{spec}")
                        }
                    } else if spec.starts_with("workspace:") {
                        format!("path:{spec}")
                    } else {
                        "registry".to_string()
                    };
                    deps.push(Dep {
                        ecosystem: "npm",
                        name: alias.clone().unwrap_or_else(|| name.clone()),
                        file: rel.clone(),
                        alias: alias.as_ref().map(|_| name.clone()),
                        section: field_name.into(),
                        requirement: Some(spec.to_string()),
                        features: Vec::new(),
                        default_features: Value::Null,
                        resolved_version: None,
                        local: local_path.is_some(),
                        source,
                    });
                    if let Some(local) = local_path {
                        resolver.local_path(
                            rel,
                            local,
                            &mut coverage,
                            rel,
                            "local-npm-dependency",
                            name,
                        );
                    } else if is_git {
                        coverage.push(Cov::new(
                            rel.clone(),
                            format!("git-npm-dependency-source:{name}"),
                        ));
                    }
                }
            }
        } else {
            let url_re = Regex::new(r#"url:\s*["']([^"']+)["']"#).expect("static regex");
            let path_re = Regex::new(r#"path:\s*["']([^"']+)["']"#).expect("static regex");
            let id_re = Regex::new(r#"id:\s*["']([^"']+)["']"#).expect("static regex");
            let name_re = Regex::new(r#"name:\s*["']([^"']+)["']"#).expect("static regex");
            let selector_re =
                Regex::new(r#"(?:branch|revision):\s*["']([^"']+)["']"#).expect("static regex");
            let strip_re = Regex::new(r#"(?:name|url|path|id):\s*"(?:\\.|[^"\\])*"\s*,?\s*"#)
                .expect("static regex");
            let capture = |re: &Regex, text: &str| {
                re.captures(text)
                    .ok()
                    .flatten()
                    .and_then(|c| c.get(1))
                    .map(|m| m.as_str().to_string())
            };
            for arg in swift_package_arguments(&text)? {
                let url = capture(&url_re, &arg);
                let path_dep = capture(&path_re, &arg);
                let registry = capture(&id_re, &arg);
                let explicit = capture(&name_re, &arg);
                let derived_url = url.as_ref().map(|u| {
                    let trimmed = if u.len() >= 4
                        && u.is_char_boundary(u.len() - 4)
                        && u[u.len() - 4..].eq_ignore_ascii_case(".git")
                    {
                        &u[..u.len() - 4]
                    } else {
                        u.as_str()
                    };
                    trimmed.rsplit('/').next().unwrap_or("").to_lowercase()
                });
                let derived_path = path_dep
                    .as_ref()
                    .map(|p| p.rsplit(['/', '\\']).next().unwrap_or("").to_string());
                let id = explicit
                    .or_else(|| registry.clone())
                    .or(derived_url)
                    .or(derived_path);
                let Some(id) = id
                    .filter(|i| !i.is_empty())
                    .filter(|_| url.is_some() || path_dep.is_some() || registry.is_some())
                else {
                    return Err(ScanError::new(
                        "unsupported Swift package identity or source",
                    ));
                };
                let selector = capture(&selector_re, &arg);
                let requirement = strip_re.replace_all(&arg, "").trim().to_string();
                let source = if let Some(p) = &path_dep {
                    format!("path:{p}")
                } else if registry.is_some() {
                    "registry".into()
                } else {
                    format!(
                        "git+{}{}",
                        url.clone().unwrap_or_default(),
                        selector.map(|s| format!("#{s}")).unwrap_or_default()
                    )
                };
                deps.push(Dep {
                    ecosystem: "swiftpm",
                    name: id.clone(),
                    file: rel.clone(),
                    alias: None,
                    section: "dependencies".into(),
                    requirement: if requirement.is_empty() {
                        None
                    } else {
                        Some(requirement)
                    },
                    features: Vec::new(),
                    default_features: Value::Null,
                    resolved_version: None,
                    local: path_dep.is_some(),
                    source,
                });
                if let Some(p) = &path_dep {
                    resolver.local_path(rel, p, &mut coverage, rel, "local-swift-dependency", &id);
                }
            }
        }
    }
    resolve_versions(
        app_root,
        inventory,
        &mut deps,
        &mut coverage,
        &mut content_digests,
    );
    Ok(DepsResult {
        deps,
        coverage,
        content_digests,
        manifests: files.into_iter().cloned().collect(),
    })
}

fn resolve_versions(
    app_root: &Path,
    inventory: &ScanInv,
    deps: &mut [Dep],
    coverage: &mut Vec<Cov>,
    content_digests: &mut Vec<ContentDigest>,
) {
    let by_rel: HashMap<&str, &File> = inventory
        .files
        .iter()
        .map(|f| (f.rel.as_str(), f))
        .collect();
    let snapshot: Option<HashMap<String, &RepoFile>> = inventory.committed.as_ref().map(|c| {
        c.repository_files
            .iter()
            .map(|f| (f.rel.clone(), f))
            .collect()
    });
    let mut cache: HashMap<String, Option<String>> = HashMap::new();
    let mut adjacent = |file: &str,
                        name: &str,
                        content_digests: &mut Vec<ContentDigest>|
     -> Result<Option<String>, String> {
        let rel = posix_join(&[&posix_dirname(file), name]);
        if let Some(hit) = cache.get(&rel) {
            return Ok(hit.clone());
        }
        let bytes: Option<Bytes> =
            if let (Some(snapshot), Some(committed)) = (&snapshot, &inventory.committed) {
                match snapshot.get(&format!("{}{}", committed.path_prefix, rel)) {
                    Some(item) if item.bytes.is_none() => {
                        return Err(format!("lockfile bytes unavailable: {rel}"))
                    }
                    Some(item) => item.bytes.clone(),
                    None => None,
                }
            } else {
                let inventoried = if OWNERSHIP_LOCKFILES.contains(&name) {
                    by_rel.get(rel.as_str()).copied()
                } else {
                    None
                };
                if inventoried.is_some_and(|i| i.bytes.is_none()) {
                    return Err(format!("lockfile bytes unavailable: {rel}"));
                }
                let absolute = paths::resolve(&app_root.join(&rel));
                if let Some(item) = inventoried {
                    item.bytes.clone()
                } else if absolute.exists() {
                    let root = paths::real(app_root).map_err(|e| e.to_string())?;
                    let real = paths::real(&absolute).map_err(|e| e.to_string())?;
                    if !real.starts_with(&root) || real == root {
                        return Err(format!("lockfile outside scan root: {rel}"));
                    }
                    let meta = std::fs::symlink_metadata(&absolute).map_err(|e| e.to_string())?;
                    if !meta.is_file() || meta.len() > 64 * 1024 * 1024 {
                        return Err(format!("lockfile unsupported or too large: {rel}"));
                    }
                    Some(std::sync::Arc::new(
                        std::fs::read(&absolute).map_err(|e| e.to_string())?,
                    ))
                } else {
                    None
                }
            };
        let text = bytes
            .as_ref()
            .map(|b| String::from_utf8_lossy(b).into_owned());
        cache.insert(rel.clone(), text.clone());
        if let Some(bytes) = &bytes {
            content_digests.push(ContentDigest {
                path: rel,
                digest: sha256_hex(bytes),
                provenance: None,
            });
        }
        Ok(text)
    };
    for d in deps.iter_mut() {
        let outcome: Result<(), String> = (|| {
            match d.ecosystem {
                "cargo" => {
                    let Some(text) = adjacent(&d.file, "Cargo.lock", content_digests)? else {
                        return Ok(());
                    };
                    let lock = parse_toml(&text)?;
                    let Some(packages) = field(&lock, "package").as_array() else {
                        return Err("Cargo.lock package list missing".into());
                    };
                    let matches_source = |p: &Value| {
                        let source = field(p, "source").as_str().unwrap_or("");
                        if d.source == "registry" {
                            source.starts_with("registry+")
                        } else if d.source.starts_with("git+") {
                            source
                                .split('#')
                                .next()
                                .unwrap_or("")
                                .split('?')
                                .next()
                                .unwrap_or("")
                                == d.source.split('#').next().unwrap_or("")
                        } else {
                            !truthy(field(p, "source"))
                        }
                    };
                    let mut candidates: Vec<&Value> = packages
                        .iter()
                        .filter(|p| str_of(field(p, "name")) == d.name && matches_source(p))
                        .collect();
                    if candidates.len() > 1 {
                        let manifest_text = by_rel
                            .get(d.file.as_str())
                            .and_then(|f| f.bytes.as_ref())
                            .map(|b| String::from_utf8_lossy(b).into_owned());
                        let owner = manifest_text
                            .and_then(|t| {
                                if t.is_empty() {
                                    None
                                } else {
                                    Some(parse_toml(&t))
                                }
                            })
                            .transpose()?
                            .and_then(|m| {
                                field(field(&m, "package"), "name")
                                    .as_str()
                                    .map(String::from)
                            });
                        let refs: Vec<&str> = packages
                            .iter()
                            .filter(|p| {
                                owner
                                    .as_deref()
                                    .is_some_and(|o| str_of(field(p, "name")) == o)
                            })
                            .flat_map(|p| items(field(p, "dependencies")))
                            .filter_map(Value::as_str)
                            .collect();
                        candidates.retain(|p| {
                            let version = str_of(field(p, "version"));
                            refs.iter().any(|r| {
                                *r == format!("{} {}", d.name, version)
                                    || r.starts_with(&format!("{} {} (", d.name, version))
                            })
                        });
                    }
                    if candidates.len() != 1 || !field(candidates[0], "version").is_string() {
                        return Err(format!(
                            "Cargo.lock resolution missing or ambiguous: {}",
                            d.name
                        ));
                    }
                    d.resolved_version = Some(str_of(field(candidates[0], "version")).to_string());
                }
                "npm" => {
                    let text = match adjacent(&d.file, "npm-shrinkwrap.json", content_digests)? {
                        Some(t) => Some(t),
                        None => adjacent(&d.file, "package-lock.json", content_digests)?,
                    };
                    if let Some(text) = text {
                        let lock: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
                        let name = d.alias.clone().unwrap_or_else(|| d.name.clone());
                        let item = field(field(&lock, "packages"), &format!("node_modules/{name}"));
                        let item = if item.is_null() {
                            field(field(&lock, "dependencies"), &name)
                        } else {
                            item
                        };
                        let Some(version) = field(item, "version").as_str() else {
                            return Err(format!("npm lock resolution missing: {name}"));
                        };
                        d.resolved_version = Some(version.to_string());
                    } else {
                        let pnpm = adjacent(&d.file, "pnpm-lock.yaml", content_digests)?;
                        let yarn = if pnpm.is_none() {
                            adjacent(&d.file, "yarn.lock", content_digests)?
                        } else {
                            None
                        };
                        if let Some(pnpm) = pnpm {
                            resolve_pnpm(d, &pnpm)?;
                        } else if let Some(yarn) = yarn {
                            resolve_yarn(d, &yarn)?;
                        }
                    }
                }
                _ => {
                    let Some(text) = adjacent(&d.file, "Package.resolved", content_digests)? else {
                        return Ok(());
                    };
                    if d.local {
                        return Ok(());
                    }
                    let lock: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
                    let pins = if !field(&lock, "pins").is_null() {
                        field(&lock, "pins")
                    } else {
                        field(field(&lock, "object"), "pins")
                    };
                    let Some(pins) = pins.as_array() else {
                        return Err("Swift lock pins missing".into());
                    };
                    let location = d
                        .source
                        .get(4..)
                        .unwrap_or("")
                        .split('#')
                        .next()
                        .unwrap_or("")
                        .to_string();
                    let pin: Vec<&Value> = pins
                        .iter()
                        .filter(|p| {
                            let identity = if !field(p, "identity").is_null() {
                                field(p, "identity")
                            } else {
                                field(p, "package")
                            };
                            identity
                                .as_str()
                                .is_some_and(|i| i.to_lowercase() == d.name.to_lowercase())
                                || field(p, "location").as_str() == Some(location.as_str())
                                || field(p, "repositoryURL").as_str() == Some(location.as_str())
                        })
                        .collect();
                    if pin.len() != 1 {
                        return Err(format!(
                            "Swift lock resolution missing or ambiguous: {}",
                            d.name
                        ));
                    }
                    let version = field(field(pin[0], "state"), "version");
                    match version {
                        Value::Null => d.resolved_version = None,
                        Value::String(v) => d.resolved_version = Some(v.clone()),
                        _ => return Err(format!("invalid Swift resolved version: {}", d.name)),
                    }
                }
            }
            Ok(())
        })();
        if let Err(message) = outcome {
            coverage.push(Cov::new(
                d.file.clone(),
                format!("dependency-lock-resolution:{message}"),
            ));
        }
    }
}

fn unquote(value: &str) -> String {
    let v = value.strip_prefix(['\'', '"']).unwrap_or(value);
    v.strip_suffix(['\'', '"']).unwrap_or(v).to_string()
}

fn resolve_pnpm(d: &mut Dep, text: &str) -> Result<(), String> {
    let entry_re =
        Regex::new(r#"^(\s*)(?:'([^']+)'|"([^"]+)"|([^:]+)):\s*(.*?)\s*$"#).expect("static regex");
    let importers_re = Regex::new(r"(?m)^importers:").expect("static regex");
    let (mut section, mut name, mut dependency_indent): (
        Option<String>,
        Option<String>,
        Option<usize>,
    ) = (None, None, None);
    let mut in_root = !importers_re.is_match(text).unwrap_or(false);
    let mut root_indent: Option<usize> = None;
    let target = d.alias.clone().unwrap_or_else(|| d.name.clone());
    let version_tail = Regex::new(r"^.*@(?=\d)").expect("static regex");
    for line in text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)) {
        let Some(caps) = entry_re.captures(line).ok().flatten() else {
            continue;
        };
        let indent = caps.get(1).map_or(0, |m| m.as_str().len());
        let key = caps
            .get(2)
            .or_else(|| caps.get(3))
            .or_else(|| caps.get(4))
            .map_or("", |m| m.as_str());
        let value = caps.get(5).map_or("", |m| m.as_str());
        if key == "." && indent == 2 {
            in_root = true;
            root_indent = Some(2);
            continue;
        }
        if root_indent.is_some_and(|r| indent <= r) {
            in_root = false;
        }
        if !in_root {
            continue;
        }
        if matches!(
            key,
            "dependencies" | "devDependencies" | "optionalDependencies" | "peerDependencies"
        ) {
            section = Some(key.into());
            dependency_indent = Some(indent + 2);
            name = None;
            continue;
        }
        if dependency_indent == Some(indent) {
            name = Some(key.to_string());
            if name.as_deref() == Some(target.as_str())
                && section.as_deref() == Some(d.section.as_str())
                && !value.is_empty()
            {
                d.resolved_version =
                    Some(unquote(value).split('(').next().unwrap_or("").to_string());
            }
        }
        if key == "version"
            && name.as_deref() == Some(target.as_str())
            && section.as_deref() == Some(d.section.as_str())
            && dependency_indent.is_some_and(|di| indent == di + 2)
        {
            let cleaned = unquote(value).split('(').next().unwrap_or("").to_string();
            d.resolved_version = Some(version_tail.replace(&cleaned, "").into_owned());
        }
    }
    if d.resolved_version.is_none() {
        return Err(format!("pnpm lock resolution missing: {target}"));
    }
    Ok(())
}

fn resolve_yarn(d: &mut Dep, text: &str) -> Result<(), String> {
    let target = d.alias.clone().unwrap_or_else(|| d.name.clone());
    let requirement = d.requirement.clone().unwrap_or_else(|| "undefined".into());
    let version_re = Regex::new(r#"^\s+version(?::|\s)\s*["']?([^\s"']+)"#).expect("static regex");
    let mut selected = false;
    for line in text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)) {
        if !line.is_empty() && !line.starts_with(char::is_whitespace) && line.ends_with(':') {
            selected = line.contains(&format!("{target}@{requirement}"))
                || line.contains(&format!("{target}@npm:{requirement}"));
        }
        if let Some(caps) = version_re.captures(line).ok().flatten() {
            if selected {
                d.resolved_version = caps.get(1).map(|m| m.as_str().to_string());
            }
        }
    }
    if d.resolved_version.is_none() {
        return Err(format!("yarn lock resolution missing: {target}"));
    }
    Ok(())
}
