//! The ownership scan. Port of `ownership.mjs`: findings from direct and transitive
//! dependencies and from source, exception evaluation, the capability table, and the
//! working-tree and committed-tree check entry points.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Map, Value};

use crate::approval::{
    canonical_repository_identity, occurrence_hash, resolve_key_log_trust, sha256_value,
    verify_protected_provenance,
};
use crate::clock::Clock;
use crate::committed::CommittedSource;
use crate::deps::{dependencies_from_inventory, CommittedInfo, Dep, RepoFile, ScanInv};
use crate::error::{Res, ScanError};
use crate::gitio;
use crate::inventory::{git_listed_inventory, listed_coverage, File};
use crate::json::{field, items, locale_compare, sha256_hex, str_of, truthy};
use crate::keyauth::{read_committed_key_authority, AuthorityEnv};
use crate::lockfiles::{transitive_dependencies, ContentDigest, LockContext, OWNERSHIP_LOCKFILES};
use crate::paths::{self, posix_basename};
use crate::source::{extname, finding_policy, source_findings, source_matchers};
use crate::types::{coverage_json, Cov};
use crate::verdict::is_blocking_finding;

pub const MANIFEST_FILE: &str = "rightkit-ownership.json";
pub const EXCEPTIONS_FILE: &str = ".rightkit/ownership-exceptions.json";
pub const SCANNER_VERSION: &str = "2.6.0";
const CODE_EXT: [&str; 19] = [
    ".rs", ".ts", ".tsx", ".js", ".mjs", ".cjs", ".jsx", ".swift", ".m", ".mm", ".c", ".cc",
    ".cpp", ".h", ".py", ".go", ".kt", ".java", ".cs",
];
const TWO_MIB: u64 = 2 * 1024 * 1024;

/// Called before each content inventory with the call number.
pub type InventoryHook = Arc<dyn Fn(&Path, usize) + Send + Sync>;

pub type TrustLoaderFn = Arc<dyn Fn() -> Res<Value> + Send + Sync>;

/// Everything a scan needs from its host: time, identity strings, the protected trust
/// source and the protected checkpoint location. Tests pin each of them.
pub struct ScanContext {
    pub clock: Arc<dyn Clock>,
    /// `<hostname>/<platform>`, recorded in report metadata.
    pub host: String,
    pub authority: AuthorityEnv,
    pub trust_loader: TrustLoaderFn,
    /// Digest of the scanner's own code, bound into every cache key.
    pub policy_digest: String,
    /// Test seam: called before each content inventory with the call number (1-based).
    pub before_inventory: Option<InventoryHook>,
}

pub fn catalog_digest(manifest: &Value) -> String {
    sha256_value(manifest)
}

pub fn catalog_version(manifest: &Value) -> String {
    match field(manifest, "version") {
        Value::Null => match field(manifest, "schema") {
            Value::Null => "1".into(),
            Value::String(s) => s.clone(),
            other => other.to_string(),
        },
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// ISO-8601 UTC with milliseconds (`Date.prototype.toISOString`).
pub fn iso_from_ms(ms: u64) -> String {
    let days = (ms / 86_400_000) as i64;
    let rem = ms % 86_400_000;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3_600_000,
        rem / 60_000 % 60,
        rem / 1000 % 60,
        rem % 1000
    )
}

fn has_code_ext(rel: &str) -> bool {
    let ext = extname(rel);
    CODE_EXT.contains(&ext.as_str())
}

fn is_manifest_or_lock(rel: &str) -> bool {
    let base = posix_basename(rel);
    [
        "Cargo.toml",
        "package.json",
        "Package.swift",
        "pnpm-workspace.yaml",
    ]
    .contains(&base)
        || OWNERSHIP_LOCKFILES.contains(&base)
}

fn digest_of(file: &File) -> String {
    file.digest
        .clone()
        .unwrap_or_else(|| sha256_hex(file.bytes.as_deref().map_or(&[][..], |b| b.as_slice())))
}

// ---- repository location ----------------------------------------------------------

pub struct Location {
    pub root: PathBuf,
    pub prefix: String,
    pub path_base: &'static str,
    pub commit: Option<String>,
}

pub fn scan_repository_root(app_root: &Path) -> Location {
    let output = gitio::plain_git(
        app_root,
        &["rev-parse", "--show-toplevel", "HEAD"],
        Duration::from_secs(30),
    );
    let text = output.map(|o| o.text()).unwrap_or_default();
    let mut lines = text
        .trim_end()
        .split('\n')
        .map(|l| l.trim_end_matches('\r'));
    let root_path = lines.next().unwrap_or("");
    let head = lines.next().unwrap_or("");
    let commit = ((head.len() == 40 || head.len() == 64)
        && head.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
    .then(|| head.to_string());
    if root_path.is_empty() || !Path::new(root_path).is_absolute() {
        return Location {
            root: paths::resolve(app_root),
            prefix: String::new(),
            path_base: "scan-root",
            commit,
        };
    }
    let root = paths::real_or_self(Path::new(root_path));
    let app = paths::real_or_self(app_root);
    match paths::relative_posix(&root, &app) {
        Some(relative) if !relative.starts_with("..") && !Path::new(&relative).is_absolute() => {
            Location {
                root,
                prefix: if relative.is_empty() {
                    String::new()
                } else {
                    format!("{relative}/")
                },
                path_base: "repository",
                commit,
            }
        }
        _ => Location {
            root: app,
            prefix: String::new(),
            path_base: "scan-root",
            commit,
        },
    }
}

pub fn repository_identity_for(root: &Path) -> Option<String> {
    let out = gitio::plain_git(
        root,
        &["config", "--get", "remote.origin.url"],
        Duration::from_secs(30),
    )
    .ok()?;
    if !out.success() {
        return None;
    }
    canonical_repository_identity(&json!(out.text().trim())).ok()
}

// ---- inventories ------------------------------------------------------------------

fn file_inventory(
    app_root: &Path,
    trust: &Value,
    path_prefix: &str,
    repository_root: &Path,
) -> ScanInv {
    let protected: Vec<String> = items(field(trust, "provenance"))
        .iter()
        .flat_map(|record| {
            items(field(record, "paths"))
                .iter()
                .map(|p| str_of(p).to_string())
        })
        .map(|p| {
            if !path_prefix.is_empty() && p.starts_with(path_prefix) {
                p[path_prefix.len()..].to_string()
            } else {
                p
            }
        })
        .collect();
    let protected_set: HashSet<&str> = protected.iter().map(String::as_str).collect();
    let relevant = |rel: &str| is_manifest_or_lock(rel) || has_code_ext(rel);
    let max = |rel: &str| {
        if has_code_ext(rel) || protected_set.contains(rel) {
            TWO_MIB
        } else {
            64 * 1024 * 1024
        }
    };
    let mut inventory =
        git_listed_inventory(app_root, &relevant, &protected, &max, repository_root);
    for item in &mut inventory.coverage {
        if item.reason == "file-over-size-limit" {
            item.reason = if protected_set.contains(item.path.as_str()) {
                "protected-provenance-file-over-2MiB"
            } else if has_code_ext(&item.path) {
                "source-file-over-2MiB"
            } else {
                "manifest-file-over-64MiB"
            }
            .into();
        }
    }
    listed_coverage(&mut inventory);
    ScanInv {
        files: inventory.files,
        coverage: inventory.coverage,
        committed: None,
    }
}

fn committed_file_inventory(
    app_root: &Path,
    trust: &Value,
    path_prefix: &str,
    committed: &CommittedSource,
) -> ScanInv {
    let prefix = if path_prefix.is_empty() {
        String::new()
    } else {
        format!("{}/", path_prefix.trim_end_matches('/'))
    };
    let repository_files: Vec<RepoFile> = committed
        .entries
        .iter()
        .filter(|e| e.kind == "blob")
        .map(|e| RepoFile {
            rel: e.rel.clone(),
            bytes: e.bytes.clone(),
        })
        .collect();
    let mut files: Vec<File> = Vec::new();
    let mut coverage: Vec<Cov> = committed
        .coverage
        .iter()
        .filter(|(path, _)| prefix.is_empty() || path.starts_with(&prefix))
        .map(|(path, reason)| {
            Cov::new(
                if prefix.is_empty() {
                    path.clone()
                } else {
                    path[prefix.len()..].to_string()
                },
                reason.clone(),
            )
        })
        .collect();
    let protected: HashSet<String> = items(field(trust, "provenance"))
        .iter()
        .flat_map(|r| {
            items(field(r, "paths"))
                .iter()
                .map(|p| str_of(p).to_string())
        })
        .collect();
    let (mut reported_opaque, mut reported_node_modules): (HashSet<String>, HashSet<String>) =
        (HashSet::new(), HashSet::new());
    for entry in &committed.entries {
        if !prefix.is_empty() && !entry.rel.starts_with(&prefix) {
            continue;
        }
        let rel = if prefix.is_empty() {
            entry.rel.clone()
        } else {
            entry.rel[prefix.len()..].to_string()
        };
        if rel.is_empty() {
            continue;
        }
        if entry.mode == "120000" {
            coverage.push(Cov::new(rel, "committed-symlink"));
            continue;
        }
        if entry.mode == "160000" || entry.kind == "commit" {
            coverage.push(Cov::new(rel, "committed-gitlink"));
            continue;
        }
        if entry.kind != "blob" {
            coverage.push(Cov::new(rel, "committed-unsupported-entry"));
            continue;
        }
        let base = posix_basename(&rel);
        let relevant = protected.contains(&entry.rel)
            || is_manifest_or_lock(&rel)
            || base == "ownership-exceptions.json"
            || has_code_ext(&rel);
        let bytes = if relevant { entry.bytes.clone() } else { None };
        if relevant && bytes.is_none() {
            coverage.push(Cov::new(rel.clone(), "committed-blob-unavailable"));
        }
        let is_exception = base == "ownership-exceptions.json";
        let limit = if has_code_ext(&rel) || protected.contains(&entry.rel) || is_exception {
            TWO_MIB
        } else {
            64 * 1024 * 1024
        };
        let size = entry.size.unwrap_or(0);
        if relevant && entry.size.is_some_and(|s| s > limit) {
            coverage.push(Cov::new(
                rel.clone(),
                if is_exception {
                    "committed-exceptions-over-2MiB"
                } else if protected.contains(&entry.rel) {
                    "protected-provenance-file-over-2MiB"
                } else {
                    "source-file-over-2MiB"
                },
            ));
        }
        let parts: Vec<&str> = rel.split('/').collect();
        if let Some(at) = parts.iter().position(|p| *p == "node_modules") {
            let dir = parts[..=at].join("/");
            if reported_node_modules.insert(dir.clone()) {
                coverage.push(Cov::new(dir, "node-modules-provenance-not-verified"));
            }
        }
        if let Some(at) = parts
            .iter()
            .position(|p| ["target", ".build", "DerivedData", "Pods"].contains(p))
        {
            let dir = parts[..=at].join("/");
            if reported_opaque.insert(dir.clone()) {
                coverage.push(Cov::new(dir, "installed-or-build-cache"));
            }
        }
        let mut abs = app_root.to_path_buf();
        for part in rel.split('/') {
            abs.push(part);
        }
        files.push(File {
            rel: rel.clone(),
            abs,
            size: entry.size,
            bytes: if size <= limit { bytes } else { None },
            oid: Some(entry.oid.clone()),
            digest: Some(format!("git:{}", entry.oid)),
            cache_key: Some(format!("gitblob:{}", entry.oid)),
            in_node_modules: rel.split('/').any(|p| p == "node_modules"),
        });
    }
    ScanInv {
        files,
        coverage,
        committed: Some(CommittedInfo {
            path_prefix: path_prefix.to_string(),
            repository_files,
        }),
    }
}

// ---- provenance -------------------------------------------------------------------

struct Provenance {
    excluded: HashMap<String, Value>,
    classifications: Vec<Value>,
}

fn trusted_provenance(
    inventory: &ScanInv,
    trust: &Value,
    repository_identity: Option<&str>,
    coverage: &mut Vec<Cov>,
    repository_root: &Path,
    path_prefix: &str,
) -> Provenance {
    let mut out = Provenance {
        excluded: HashMap::new(),
        classifications: Vec::new(),
    };
    let Some(identity) = repository_identity.filter(|_| truthy(trust)) else {
        return out;
    };
    let scanned: HashMap<&str, &File> = inventory
        .files
        .iter()
        .map(|f| (f.rel.as_str(), f))
        .collect();
    let strip = |rel: &str| {
        if path_prefix.is_empty() {
            rel.to_string()
        } else {
            rel[path_prefix.len()..].to_string()
        }
    };
    for record in items(field(trust, "provenance")) {
        if str_of(field(record, "repositoryIdentity")) != identity {
            continue;
        }
        let record_paths: Vec<&str> = items(field(record, "paths")).iter().map(str_of).collect();
        if !record_paths
            .iter()
            .any(|rel| path_prefix.is_empty() || rel.starts_with(path_prefix))
        {
            continue;
        }
        let mut files: Vec<(String, Vec<u8>)> = Vec::new();
        let mut safe = true;
        for rel in &record_paths {
            if let Some(committed) = &inventory.committed {
                let found = committed.repository_files.iter().find(|f| f.rel == *rel);
                match found.and_then(|f| f.bytes.as_ref()) {
                    Some(bytes) if bytes.len() as u64 <= TWO_MIB => {
                        files.push((rel.to_string(), bytes.to_vec()))
                    }
                    _ => {
                        safe = false;
                        break;
                    }
                }
                continue;
            }
            let Ok(root_real) = paths::real(repository_root) else {
                safe = false;
                break;
            };
            let mut current = repository_root.to_path_buf();
            let mut ok = true;
            let mut regular = false;
            for part in rel.split('/') {
                current.push(part);
                match std::fs::symlink_metadata(&current) {
                    Ok(meta) if meta.file_type().is_symlink() => {
                        ok = false;
                        break;
                    }
                    Ok(meta) => regular = meta.is_file(),
                    Err(_) => {
                        ok = false;
                        break;
                    }
                }
            }
            let mut file_path = root_real.clone();
            for part in rel.split('/') {
                file_path.push(part);
            }
            if !ok || !regular || !paths::resolve(&file_path).starts_with(&root_real) {
                safe = false;
                break;
            }
            let content = if path_prefix.is_empty() || rel.starts_with(path_prefix) {
                match scanned
                    .get(strip(rel).as_str())
                    .and_then(|f| f.bytes.as_ref().map(|b| (b.clone(), f.size)))
                {
                    Some((bytes, size)) if size.unwrap_or(0) <= TWO_MIB => Some(bytes.to_vec()),
                    _ => None,
                }
            } else {
                match std::fs::metadata(&current) {
                    Ok(meta) if meta.len() <= TWO_MIB => std::fs::read(&current).ok(),
                    _ => None,
                }
            };
            match content {
                Some(bytes) => files.push((rel.to_string(), bytes)),
                None => {
                    safe = false;
                    break;
                }
            }
        }
        let paths_owned: Vec<String> = record_paths.iter().map(|p| p.to_string()).collect();
        let verified = safe
            && verify_protected_provenance(
                trust,
                &json!(identity),
                &paths_owned,
                &files,
                str_of(field(record, "kind")),
                str_of(field(record, "requestId")),
            )
            .is_ok();
        if !verified {
            for rel in &record_paths {
                if path_prefix.is_empty() || rel.starts_with(path_prefix) {
                    coverage.push(Cov::new(
                        strip(rel),
                        "protected-provenance-stale-or-unreadable",
                    ));
                }
            }
            continue;
        }
        let classification = json!({"kind": field(record, "kind"), "requestId": field(record, "requestId"), "contentDigest": field(record, "contentDigest")});
        let mut sorted: Vec<&str> = record_paths.clone();
        sorted.sort();
        let mut with_paths = classification.clone();
        with_paths["paths"] = json!(sorted);
        out.classifications.push(with_paths);
        for rel in &record_paths {
            if path_prefix.is_empty() || rel.starts_with(path_prefix) {
                out.excluded.insert(strip(rel), classification.clone());
            }
        }
    }
    out
}

// ---- findings ---------------------------------------------------------------------

pub struct Scanned {
    pub findings: Vec<Value>,
    pub coverage: Vec<Cov>,
    pub deps: Vec<Dep>,
    pub content_digests: Vec<ContentDigest>,
    pub provenance: Vec<Value>,
}

fn set_policy(map: &mut Map<String, Value>, capability: &Value, fingerprint: &Value) {
    let (tier, policy) = finding_policy(capability, fingerprint);
    map.insert("tier".into(), json!(tier));
    if let Some(policy) = policy {
        map.insert("policy".into(), json!(policy));
    }
}

fn dependency_finding(
    capability: &Value,
    fingerprint: &Value,
    d: &Dep,
    transitive: Option<&crate::lockfiles::Transitive>,
) -> Value {
    let mut m = Map::new();
    m.insert("fingerprint".into(), field(fingerprint, "id").clone());
    m.insert("capability".into(), field(capability, "capability").clone());
    m.insert("package".into(), field(capability, "package").clone());
    set_policy(&mut m, capability, fingerprint);
    m.insert(
        "file".into(),
        json!(transitive.map_or(d.file.as_str(), |t| t.file.as_str())),
    );
    m.insert("line".into(), json!(0));
    m.insert(
        "kind".into(),
        json!(if transitive.is_some() {
            "transitive-dependency"
        } else {
            "dependency"
        }),
    );
    let ecosystem = transitive.map_or(d.ecosystem, |t| t.ecosystem);
    let name = transitive.map_or(d.name.as_str(), |t| t.name.as_str());
    m.insert("ecosystem".into(), json!(ecosystem));
    m.insert("name".into(), json!(name));
    match transitive {
        None => {
            m.insert("alias".into(), json!(d.alias));
            m.insert("section".into(), json!(d.section));
            m.insert("requirement".into(), json!(d.requirement));
            m.insert("features".into(), json!(d.features));
            m.insert("defaultFeatures".into(), d.default_features.clone());
            m.insert("local".into(), json!(d.local));
        }
        Some(t) => {
            m.insert("lockfile".into(), json!(t.lockfile()));
            m.insert("dependencyPath".into(), json!(t.dependency_path));
        }
    }
    let source = transitive.map_or(d.source.as_str(), |t| t.source.as_str());
    let resolved = transitive
        .map(|t| t.resolved_version.clone())
        .or_else(|| d.resolved_version.clone());
    m.insert("source".into(), json!(source));
    m.insert("resolvedVersion".into(), json!(resolved));
    let alias = if transitive.is_none() {
        d.alias.as_deref()
    } else {
        None
    };
    m.insert(
        "text".into(),
        json!(format!(
            "{ecosystem} dependency {}{name}",
            alias.map(|a| format!("{a} aliases ")).unwrap_or_default()
        )),
    );
    m.insert("provenance".into(), json!(source));
    Value::Object(m)
}

#[allow(clippy::too_many_arguments)]
fn scan_internal(
    app_root: &Path,
    manifest: &Value,
    inventory: ScanInv,
    trust: &Value,
    repository_identity: Option<&str>,
    repository_root: &Path,
    path_prefix: &str,
) -> Res<Scanned> {
    let mut findings: Vec<Value> = Vec::new();
    let mut coverage: Vec<Cov> = inventory.coverage.clone();
    let deps_result = dependencies_from_inventory(app_root, &inventory)?;
    let mut content_digests: Vec<ContentDigest> = deps_result.content_digests;
    let mut deps = deps_result.deps;
    let capabilities = items(field(manifest, "capabilities"));
    let catalog_owners: HashSet<String> = capabilities
        .iter()
        .map(|c| str_of(field(c, "package")).to_string())
        .collect();
    coverage.extend(deps_result.coverage);
    let provenance = trusted_provenance(
        &inventory,
        trust,
        repository_identity,
        &mut coverage,
        repository_root,
        path_prefix,
    );
    for c in capabilities {
        for f in items(field(c, "fingerprints"))
            .iter()
            .filter(|f| str_of(field(f, "type")) == "dependency")
        {
            for d in &deps {
                let fname = str_of(field(f, "name"));
                if d.ecosystem == str_of(field(f, "ecosystem"))
                    && (d.name == fname || d.alias.as_deref() == Some(fname))
                    && !catalog_owners.contains(&d.name)
                {
                    findings.push(dependency_finding(c, f, d, None));
                }
            }
        }
    }
    let mut block_rules: Vec<(&Value, &Value)> = Vec::new();
    for c in capabilities {
        for f in items(field(c, "fingerprints"))
            .iter()
            .filter(|f| str_of(field(f, "type")) == "dependency")
        {
            if finding_policy(c, f).0 == "block" {
                block_rules.push((c, f));
            }
        }
    }
    let mut transitive_owners = catalog_owners.clone();
    for owner in items(field(manifest, "internalOwners")) {
        let name = if owner.is_string() {
            str_of(owner)
        } else {
            str_of(field(owner, "package"))
        };
        transitive_owners.insert(name.to_string());
    }
    let targets: HashSet<String> = block_rules
        .iter()
        .map(|(_, f)| {
            format!(
                "{}:{}",
                str_of(field(f, "ecosystem")),
                str_of(field(f, "name"))
            )
        })
        .collect();
    let manifests: Vec<&File> = deps_result.manifests.iter().collect();
    let lock_ctx = LockContext {
        app_root,
        files: &inventory.files,
        committed: inventory.committed.is_some(),
    };
    let transitive = transitive_dependencies(
        &lock_ctx,
        &manifests,
        &transitive_owners,
        &mut coverage,
        &mut content_digests,
        &targets,
    );
    for (c, f) in &block_rules {
        for t in &transitive {
            if t.ecosystem == str_of(field(f, "ecosystem")) && t.name == str_of(field(f, "name")) {
                let shim = Dep {
                    ecosystem: t.ecosystem,
                    name: t.name.clone(),
                    file: t.file.clone(),
                    alias: None,
                    section: String::new(),
                    requirement: None,
                    features: Vec::new(),
                    default_features: Value::Null,
                    resolved_version: None,
                    local: false,
                    source: String::new(),
                };
                findings.push(dependency_finding(c, f, &shim, Some(t)));
            }
        }
    }
    let matchers = source_matchers(manifest)?;
    for file in &inventory.files {
        let Some(bytes) = &file.bytes else { continue };
        if !has_code_ext(&file.rel) {
            continue;
        }
        if let Some(class) = provenance.excluded.get(&file.rel) {
            content_digests.push(ContentDigest {
                path: file.rel.clone(),
                digest: digest_of(file),
                provenance: Some(class.clone()),
            });
            continue;
        }
        if file.size.unwrap_or(0) > TWO_MIB {
            coverage.push(Cov::new(file.rel.clone(), "source-file-over-2MiB"));
            continue;
        }
        content_digests.push(ContentDigest {
            path: file.rel.clone(),
            digest: digest_of(file),
            provenance: None,
        });
        for mut finding in source_findings(&file.rel, bytes, &matchers) {
            finding.insert("file".into(), json!(file.rel));
            findings.push(Value::Object(finding));
        }
    }
    if !path_prefix.is_empty() {
        let prefix = |p: &str| {
            if p == "." {
                path_prefix[..path_prefix.len() - 1].to_string()
            } else {
                format!("{path_prefix}{p}")
            }
        };
        for f in &mut findings {
            let file = prefix(str_of(field(f, "file")));
            f["file"] = json!(file);
            if let Some(lock) = f.get("lockfile").and_then(Value::as_str).map(prefix) {
                f["lockfile"] = json!(lock);
            }
        }
        for c in &mut coverage {
            c.path = prefix(&c.path);
        }
        for d in &mut deps {
            d.file = prefix(&d.file);
        }
        for d in &mut content_digests {
            d.path = prefix(&d.path);
        }
    }
    for f in &mut findings {
        let hash = occurrence_hash(f).map_err(ScanError::new)?;
        f["occurrenceHash"] = json!(hash);
    }
    content_digests.sort_by(|a, b| locale_compare(&a.path, &b.path));
    Ok(Scanned {
        findings,
        coverage,
        deps,
        content_digests,
        provenance: provenance.classifications,
    })
}

pub fn content_digests_json(list: &[ContentDigest]) -> Value {
    Value::Array(
        list.iter()
            .map(|d| {
                let mut m = Map::new();
                m.insert("path".into(), json!(d.path));
                m.insert("digest".into(), json!(d.digest));
                if let Some(p) = &d.provenance {
                    m.insert("provenance".into(), p.clone());
                }
                Value::Object(m)
            })
            .collect(),
    )
}

// ---- exceptions -------------------------------------------------------------------

fn enrollment_pending(trust: &Value, state: &Value) -> bool {
    if let Some(pending) = state.get("pending") {
        return truthy(pending);
    }
    if let Some(enrolled) = state.get("enrolled") {
        return !truthy(enrolled);
    }
    if truthy(field(trust, "rootFingerprint")) {
        let keys = field(trust, "ownershipKeys");
        return !keys.as_array().is_some_and(|k| !k.is_empty());
    }
    let hosts = field(trust, "hosts");
    let keys = [field(hosts, "windows"), field(hosts, "macos")];
    let revoked = |id: &Value| items(field(trust, "revokedKeyIds")).contains(id);
    !keys.iter().all(|k| {
        truthy(field(k, "keyId")) && truthy(field(k, "publicKeyPem")) && !revoked(field(k, "keyId"))
    }) || field(keys[0], "keyId") == field(keys[1], "keyId")
        || field(keys[0], "publicKeyPem") == field(keys[1], "publicKeyPem")
}

struct EvalResult {
    violations: Vec<Value>,
    covered: Vec<Value>,
    problems: Vec<String>,
    warnings: Vec<String>,
    /// Informational only: pending enrollment is a warning and never affects `ok`.
    enrollment_pending: bool,
}

/// Problems with one plain exception entry (Orthic-Labs/rightkit#262). Same rules as `validateExceptionEntry` in `ownership.mjs`.
fn exception_entry_problems(e: &Value) -> Vec<String> {
    let Some(map) = e.as_object() else {
        return vec!["not an object".into()];
    };
    if map.contains_key("payload") || map.contains_key("signature") || map.contains_key("schema") {
        return vec!["legacy signed exception record; replace it with a plain entry {capability, package, reason, owner, date}".into()];
    }
    let mut problems = Vec::new();
    let non_empty = |key: &str| {
        field(e, key)
            .as_str()
            .is_some_and(|text| !text.trim().is_empty())
    };
    for key in ["capability", "reason", "owner"] {
        if !non_empty(key) {
            problems.push(format!("missing {key}"));
        }
    }
    if !non_empty("package") && !non_empty("crate") {
        problems.push("missing package (crate or package)".into());
    }
    if !crate::approval::is_valid_date(str_of(field(e, "date"))) {
        problems.push("date must be YYYY-MM-DD".into());
    }
    for key in ["fingerprint", "path"] {
        if let Some(value) = map.get(key) {
            if !value.as_str().is_some_and(|text| !text.is_empty()) {
                problems.push(format!("{key} must be a non-empty string when present"));
            }
        }
    }
    problems
}

/// Does a plain exception entry cover this finding? Same rules as `exceptionMatches` in `ownership.mjs`.
fn exception_matches(e: &Value, finding: &Value) -> bool {
    let owner = if field(e, "package").is_null() {
        field(e, "crate")
    } else {
        field(e, "package")
    };
    if field(finding, "capability") != field(e, "capability") || field(finding, "package") != owner
    {
        return false;
    }
    if map_has(e, "fingerprint") && field(finding, "fingerprint") != field(e, "fingerprint") {
        return false;
    }
    if map_has(e, "path") {
        let prefix = str_of(field(e, "path")).trim_end_matches('/');
        let file = str_of(field(finding, "file"));
        if !(file == str_of(field(e, "path")) || file.starts_with(&format!("{prefix}/"))) {
            return false;
        }
    }
    true
}

fn map_has(value: &Value, key: &str) -> bool {
    value.get(key).is_some_and(|v| !v.is_null())
}

/// Plain exception entries, reviewed through git (Orthic-Labs/rightkit#262): a matching entry marks a finding excepted,
/// an entry that matches nothing is a stale warning, a malformed or legacy signed entry is a problem. Enrollment state
/// no longer affects the verdict; pending enrollment is only a warning.
fn evaluate(
    findings: &[Value],
    exceptions: &[Value],
    trust: &Value,
    enrollment: &Value,
) -> EvalResult {
    let (mut problems, mut covered, mut violations, mut warnings) = (
        Vec::new(),
        Vec::<Value>::new(),
        Vec::<Value>::new(),
        Vec::<String>::new(),
    );
    for e in exceptions {
        let where_ = format!(
            "exception #{} ({}/{})",
            js_scalar(field(e, "index")),
            if field(e, "capability").is_null() {
                "?".to_string()
            } else {
                js_scalar(field(e, "capability"))
            },
            if !field(e, "package").is_null() {
                js_scalar(field(e, "package"))
            } else if !field(e, "crate").is_null() {
                js_scalar(field(e, "crate"))
            } else {
                "?".to_string()
            }
        );
        let invalid = exception_entry_problems(e);
        if !invalid.is_empty() {
            problems.push(format!("{where_}: {}", invalid.join("; ")));
            continue;
        }
        let matching: Vec<&Value> = findings
            .iter()
            .filter(|f| exception_matches(e, f))
            .collect();
        if matching.is_empty() {
            warnings.push(format!(
                "{where_}: stale, matches no current finding (remove it from the exceptions file)"
            ));
            continue;
        }
        for f in matching {
            let hash = occurrence_hash(f).unwrap_or_default();
            if covered
                .iter()
                .any(|x| str_of(field(x, "occurrenceHash")) == hash)
            {
                continue;
            }
            let mut entry = f.clone();
            entry["occurrenceHash"] = json!(hash);
            entry["exception"] = e.clone();
            covered.push(entry);
        }
    }
    let pending = enrollment_pending(trust, enrollment);
    if pending {
        warnings.push("approval enrollment pending (warning only): exceptions are plain git-reviewed entries and no scan result depends on enrolled approval keys".into());
    }
    for f in findings {
        let hash = occurrence_hash(f).unwrap_or_default();
        if !covered
            .iter()
            .any(|x| str_of(field(x, "occurrenceHash")) == hash)
        {
            let mut entry = f.clone();
            entry["occurrenceHash"] = json!(hash);
            violations.push(entry);
        }
    }
    EvalResult {
        violations,
        covered,
        problems,
        warnings,
        enrollment_pending: pending,
    }
}

fn js_scalar(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => "undefined".into(),
        other => other.to_string(),
    }
}

fn capability_table(
    manifest: &Value,
    deps: &[Dep],
    violations: &[Value],
    covered: &[Value],
    has_coverage: bool,
) -> Value {
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    for c in items(field(manifest, "capabilities")) {
        let name = str_of(field(c, "capability")).to_string();
        let package = str_of(field(c, "package")).to_string();
        match groups.iter_mut().find(|(n, _)| *n == name) {
            Some(group) => group.1.push(package),
            None => groups.push((name, vec![package])),
        }
    }
    Value::Array(
        groups
            .into_iter()
            .map(|(capability, owners)| {
                let declared = deps.iter().any(|d| {
                    owners.contains(&d.name)
                        || owners
                            .iter()
                            .any(|o| d.name == o.replacen("rightkit-", "@rightkit/", 1))
                });
                let own = violations
                    .iter()
                    .filter(|x| str_of(field(x, "capability")) == capability)
                    .count();
                let cov = covered
                    .iter()
                    .filter(|x| str_of(field(x, "capability")) == capability)
                    .count();
                let status = if own > 0 {
                    "blocked"
                } else if cov > 0 {
                    "excepted"
                } else if declared {
                    "declared-dependency"
                } else if has_coverage {
                    "unscanned"
                } else {
                    "not-detected"
                };
                let mut row = Map::new();
                row.insert("capability".into(), json!(capability));
                row.insert("owner".into(), json!(owners[0]));
                if owners.len() > 1 {
                    row.insert("owners".into(), json!(owners));
                }
                row.insert("uses".into(), json!(declared));
                row.insert("status".into(), json!(status));
                row.insert("count".into(), json!(own + cov));
                Value::Object(row)
            })
            .collect(),
    )
}

/// Dedup coverage by `path\0reason`, keeping first position and last value.
fn dedup_coverage(list: &[Cov]) -> Vec<Cov> {
    let mut out: Vec<Cov> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for c in list {
        let key = format!("{}\0{}", c.path, c.reason);
        match index.get(&key) {
            Some(at) => out[*at] = c.clone(),
            None => {
                index.insert(key, out.len());
                out.push(c.clone());
            }
        }
    }
    out
}

// ---- check entry points ------------------------------------------------------------

pub struct CheckOptions<'a> {
    /// The trust object itself, or a `{trust, enrolled, pending}` state wrapper.
    pub trust: Value,
    pub expected_commit: Option<&'a str>,
    pub expected_tree: Option<&'a str>,
}

fn trust_of(state: &Value) -> Value {
    if state.get("trust").is_some() {
        field(state, "trust").clone()
    } else {
        state.clone()
    }
}

fn read_exceptions_json(text: &str) -> Result<Vec<Value>, ()> {
    let parsed: Value = serde_json::from_str(text).map_err(|_| ())?;
    Ok(items(field(&parsed, "exceptions"))
        .iter()
        .enumerate()
        .map(|(index, e)| {
            let mut entry = e.as_object().cloned().unwrap_or_default();
            entry.insert("index".into(), json!(index));
            Value::Object(entry)
        })
        .collect())
}

#[allow(clippy::too_many_arguments)]
fn finish(
    manifest: &Value,
    scanned: Scanned,
    metadata_base: Map<String, Value>,
    exceptions: Vec<Value>,
    trust: &Value,
    enrollment: &Value,
) -> Value {
    let mut metadata = metadata_base;
    metadata.insert(
        "contentDigests".into(),
        content_digests_json(&scanned.content_digests),
    );
    metadata.insert("coverage".into(), coverage_json(&scanned.coverage));
    metadata.insert(
        "provenance".into(),
        Value::Array(scanned.provenance.clone()),
    );
    let result = evaluate(&scanned.findings, &exceptions, trust, enrollment);
    let coverage = dedup_coverage(&scanned.coverage);
    let rows = capability_table(
        manifest,
        &scanned.deps,
        &result.violations,
        &result.covered,
        !coverage.is_empty(),
    );
    let ok = !result.violations.iter().any(is_blocking_finding)
        && result.problems.is_empty()
        && coverage.is_empty();
    json!({
        "ok": ok, "rows": rows, "violations": result.violations, "covered": result.covered, "problems": result.problems,
        "warnings": result.warnings, "enrollmentPending": result.enrollment_pending, "findings": scanned.findings,
        "coverage": coverage_json(&coverage), "metadata": Value::Object(metadata),
    })
}

fn scan_metadata_base(
    ctx: &ScanContext,
    manifest: &Value,
    identity: Option<&str>,
    commit: Option<&str>,
) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("repositoryIdentity".into(), json!(identity));
    m.insert("commit".into(), json!(commit));
    m.insert("host".into(), json!(ctx.host));
    m.insert("scannedAt".into(), json!(iso_from_ms(ctx.clock.now_ms())));
    m.insert("catalogDigest".into(), json!(catalog_digest(manifest)));
    m.insert("catalogVersion".into(), json!(catalog_version(manifest)));
    m.insert("scannerVersion".into(), json!(SCANNER_VERSION));
    m
}

fn enrollment_of(trust: &Value, state: &Value) -> Value {
    if truthy(field(trust, "rootFingerprint")) {
        let complete = field(trust, "enrollmentComplete").as_bool() == Some(true);
        json!({"enrolled": complete, "pending": !complete})
    } else if state.get("pending").is_some() || state.get("enrolled").is_some() {
        state.clone()
    } else {
        Value::Null
    }
}

/// Working-tree check (`checkApp`). Used for pending-enrolment advisory scans.
pub fn check_app(
    ctx: &ScanContext,
    app_root: &Path,
    manifest: &Value,
    options: &CheckOptions<'_>,
) -> Res<Value> {
    let trust_state = options.trust.clone();
    let mut trust = trust_of(&trust_state);
    let repository_location = scan_repository_root(app_root);
    let identity = repository_identity_for(app_root);
    let mut authority_meta: Map<String, Value> = Map::new();
    if truthy(field(&trust, "keyAuthority")) {
        let authority = read_committed_key_authority(&trust, &ctx.authority).map_err(|p| {
            ScanError::new(format!(
                "protected ownership key authority failed: {}",
                p.join("; ")
            ))
        })?;
        trust = authority.resolved_trust.clone();
        if items(field(&trust, "ownershipKeys")).is_empty() {
            return Err(ScanError::new(
                "protected ownership key authority has no active approval keys",
            ));
        }
        authority_meta.insert("keyLogDigest".into(), json!(authority.digest));
        authority_meta.insert("keyCheckpoint".into(), authority.key_checkpoint);
        authority_meta.insert("rootFingerprint".into(), authority.root_fingerprint);
        authority_meta.insert("keyAuthority".into(), authority.binding);
    } else if truthy(field(&trust, "rootFingerprint")) {
        let mut with_authority = trust.clone();
        with_authority["keyAuthority"] = json!({"repositoryIdentity": identity, "checkoutPath": repository_location.root.to_string_lossy()});
        let authority =
            read_committed_key_authority(&with_authority, &ctx.authority).map_err(|p| {
                ScanError::new(format!(
                    "protected ownership key log failed: {}",
                    p.join("; ")
                ))
            })?;
        trust = authority.resolved_trust.clone();
        if items(field(&trust, "ownershipKeys")).is_empty() {
            return Err(ScanError::new(
                "protected ownership key log has no active approval keys",
            ));
        }
        authority_meta.insert("keyLogDigest".into(), json!(authority.digest));
        authority_meta.insert("keyCheckpoint".into(), authority.key_checkpoint);
        authority_meta.insert("rootFingerprint".into(), authority.root_fingerprint);
    }
    let inventory = file_inventory(
        app_root,
        &trust,
        &repository_location.prefix,
        &repository_location.root,
    );
    let scanned = scan_internal(
        app_root,
        manifest,
        inventory,
        &trust,
        identity.as_deref(),
        &repository_location.root,
        &repository_location.prefix,
    )?;
    let mut metadata = scan_metadata_base(
        ctx,
        manifest,
        identity.as_deref(),
        repository_location.commit.as_deref(),
    );
    metadata.insert("pathBase".into(), json!(repository_location.path_base));
    metadata.insert("pathPrefix".into(), json!(repository_location.prefix));
    metadata.extend(authority_meta);
    let enrollment = enrollment_of(&trust, &trust_state);
    let exceptions =
        match std::fs::read_to_string(app_root.join(".rightkit").join("ownership-exceptions.json"))
        {
            Ok(text) => read_exceptions_json(&text)
                .map_err(|_| ScanError::new("Unexpected end of JSON input"))?,
            Err(_) => Vec::new(),
        };
    Ok(finish(
        manifest,
        scanned,
        metadata,
        exceptions,
        &trust,
        &enrollment,
    ))
}

/// Committed-tree check (`checkCommitted`): exact HEAD blobs, no checkout contents.
pub fn check_committed(
    ctx: &ScanContext,
    app_root: &Path,
    manifest: &Value,
    options: &CheckOptions<'_>,
    source: &CommittedSource,
) -> Res<Value> {
    let trust_state = options.trust.clone();
    let mut trust = trust_of(&trust_state);
    if options.expected_commit.is_some_and(|c| c != source.commit)
        || options.expected_tree.is_some_and(|t| t != source.tree)
    {
        return Err(ScanError::new(
            "committed ownership: source handle does not match expected commit/tree",
        ));
    }
    let relative = paths::relative_posix(
        &paths::real_or_self(&source.repository_root),
        &paths::real_or_self(app_root),
    );
    let relative = match relative {
        Some(r) if !r.starts_with("..") && !Path::new(&r).is_absolute() => r,
        _ => {
            return Err(ScanError::new(
                "committed ownership: scan root is outside committed repository",
            ))
        }
    };
    let path_prefix = if relative.is_empty() {
        String::new()
    } else {
        format!("{relative}/")
    };
    let identity = source
        .origin
        .as_deref()
        .filter(|o| !o.is_empty())
        .map(|o| canonical_repository_identity(&json!(o)))
        .transpose()
        .map_err(ScanError::new)?;
    let Some(identity) = identity else {
        return Err(ScanError::new(
            "committed ownership: repository origin is missing or invalid",
        ));
    };
    let mut key_meta: Map<String, Value> = Map::new();
    if truthy(field(&trust, "keyAuthority")) {
        let authority = read_committed_key_authority(&trust, &ctx.authority).map_err(|p| {
            ScanError::new(format!(
                "committed ownership key authority failed: {}",
                p.join("; ")
            ))
        })?;
        trust = authority.resolved_trust.clone();
        if items(field(&trust, "ownershipKeys")).is_empty() {
            return Err(ScanError::new(
                "protected ownership key authority has no active approval keys",
            ));
        }
        key_meta.insert("keyLogDigest".into(), json!(authority.digest));
        key_meta.insert("keyCheckpoint".into(), authority.key_checkpoint);
        key_meta.insert(
            "rootFingerprint".into(),
            field(&trust, "rootFingerprint").clone(),
        );
        key_meta.insert("keyAuthority".into(), authority.binding);
    } else if truthy(field(&trust, "rootFingerprint")) {
        let log = source
            .entry(".rightkit/ownership-keys.json")
            .and_then(|e| e.bytes.clone());
        let resolved = resolve_key_log_trust(&trust, log.as_deref().map(|b| b.as_slice()))
            .map_err(|p| {
                ScanError::new(format!(
                    "committed ownership key log failed: {}",
                    p.join("; ")
                ))
            })?;
        trust = resolved.trust;
        if items(field(&trust, "ownershipKeys")).is_empty() {
            return Err(ScanError::new(
                "protected ownership key log has no active approval keys",
            ));
        }
        key_meta.insert("keyLogDigest".into(), json!(resolved.key_log_digest));
        key_meta.insert("keyCheckpoint".into(), resolved.checkpoint);
        key_meta.insert(
            "rootFingerprint".into(),
            field(&trust, "rootFingerprint").clone(),
        );
    }
    let mut inventory = committed_file_inventory(app_root, &trust, &path_prefix, source);
    if !path_prefix.is_empty()
        && !source
            .entries
            .iter()
            .any(|e| e.rel.starts_with(&path_prefix))
    {
        inventory
            .coverage
            .push(Cov::new(".", "registered-root-missing-from-committed-tree"));
    }
    let mut scanned = scan_internal(
        app_root,
        manifest,
        inventory,
        &trust,
        Some(&identity),
        &source.repository_root,
        &path_prefix,
    )?;
    let exception_path = [path_prefix.trim_end_matches('/'), EXCEPTIONS_FILE]
        .iter()
        .filter(|p| !p.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join("/");
    let exception_object = source.entry(&exception_path);
    let exception_bytes = exception_object.and_then(|e| e.bytes.clone());
    let mut exceptions: Vec<Value> = Vec::new();
    if let (Some(bytes), Some(object)) = (&exception_bytes, exception_object) {
        if object.size.unwrap_or(0) <= TWO_MIB {
            match read_exceptions_json(&String::from_utf8_lossy(bytes)) {
                Ok(list) => exceptions = list,
                Err(()) => scanned.coverage.push(Cov::new(
                    EXCEPTIONS_FILE,
                    "invalid-committed-exceptions-json",
                )),
            }
        }
    }
    let exception_digest = match (&exception_bytes, exception_object) {
        (Some(bytes), _) => sha256_hex(bytes),
        (None, Some(object)) => {
            sha256_value(&json!({"unavailableBlob": object.oid, "size": object.size.unwrap_or(0)}))
        }
        (None, None) => sha256_value(&json!([])),
    };
    let mut metadata = scan_metadata_base(ctx, manifest, Some(&identity), Some(&source.commit));
    metadata.insert("tree".into(), json!(source.tree));
    metadata.insert("pathBase".into(), json!("repository"));
    metadata.insert("pathPrefix".into(), json!(path_prefix));
    metadata.insert("exceptionDigest".into(), json!(exception_digest));
    metadata.insert("source".into(), json!("committed-git-tree"));
    metadata.extend(key_meta);
    let enrollment = enrollment_of(&trust, &trust_state);
    Ok(finish(
        manifest,
        scanned,
        metadata,
        exceptions,
        &trust,
        &enrollment,
    ))
}

#[cfg(test)]
mod exception_tests {
    use super::*;

    fn finding(file: &str, fingerprint: &str, capability: &str, package: &str) -> Value {
        json!({"fingerprint": fingerprint, "capability": capability, "package": package, "kind": "source",
               "file": file, "line": 1, "lineText": "marker", "ordinal": 0, "text": "marker"})
    }

    fn entry() -> Value {
        json!({"capability": "oauth-sign-in", "package": "rightkit-oauth", "reason": "migration",
               "owner": "app", "date": "2026-10-17", "index": 0})
    }

    fn run(findings: &[Value], exceptions: &[Value]) -> EvalResult {
        evaluate(
            findings,
            exceptions,
            &Value::Null,
            &json!({"enrolled": true}),
        )
    }

    fn stale(result: &EvalResult) -> usize {
        result
            .warnings
            .iter()
            .filter(|w| w.contains("stale"))
            .count()
    }

    #[test]
    fn plain_entry_marks_matching_findings_excepted_without_trust_or_signature() {
        let github = finding(
            "src/a.rs",
            "oauth.endpoint.github",
            "oauth-sign-in",
            "rightkit-oauth",
        );
        let other = finding(
            "src/b.rs",
            "browser.x",
            "browser-automation",
            "rightkit-browser",
        );
        let result = run(&[github, other], &[entry()]);
        assert!(result.problems.is_empty(), "{:?}", result.problems);
        assert_eq!(result.covered.len(), 1);
        assert_eq!(result.violations.len(), 1);
        assert_eq!(str_of(field(&result.violations[0], "file")), "src/b.rs");
        assert_eq!(stale(&result), 0);
    }

    #[test]
    fn crate_alias_fingerprint_and_path_narrow_the_match() {
        let f = finding(
            "engine/src/a.rs",
            "oauth.endpoint.github",
            "oauth-sign-in",
            "rightkit-oauth",
        );
        let mut aliased = entry();
        aliased.as_object_mut().unwrap().remove("package");
        aliased["crate"] = json!("rightkit-oauth");
        assert_eq!(run(std::slice::from_ref(&f), &[aliased]).covered.len(), 1);
        let mut narrowed = entry();
        narrowed["fingerprint"] = json!("oauth.endpoint.github");
        narrowed["path"] = json!("engine/src");
        assert_eq!(run(std::slice::from_ref(&f), &[narrowed]).covered.len(), 1);
        let mut wrong_fingerprint = entry();
        wrong_fingerprint["fingerprint"] = json!("oauth.endpoint.google");
        let result = run(std::slice::from_ref(&f), &[wrong_fingerprint]);
        assert_eq!(
            (
                result.covered.len(),
                result.violations.len(),
                stale(&result)
            ),
            (0, 1, 1)
        );
        let mut wrong_path = entry();
        wrong_path["path"] = json!("apps/src");
        let result = run(&[f], &[wrong_path]);
        assert_eq!((result.covered.len(), stale(&result)), (0, 1));
    }

    #[test]
    fn entry_matching_nothing_is_a_stale_warning_not_a_problem() {
        let f = finding(
            "src/a.rs",
            "browser.x",
            "browser-automation",
            "rightkit-browser",
        );
        let result = run(&[f], &[entry()]);
        assert!(result.problems.is_empty());
        assert_eq!(result.covered.len(), 0);
        assert_eq!(result.violations.len(), 1);
        assert!(result.warnings.iter().any(|w| w.starts_with(
            "exception #0 (oauth-sign-in/rightkit-oauth): stale, matches no current finding"
        )));
    }

    #[test]
    fn malformed_and_legacy_signed_entries_are_problems_and_never_cover() {
        let f = finding(
            "src/a.rs",
            "oauth.endpoint.github",
            "oauth-sign-in",
            "rightkit-oauth",
        );
        for (key, expected) in [
            ("reason", "missing reason"),
            ("owner", "missing owner"),
            ("capability", "missing capability"),
            ("package", "missing package"),
            ("date", "date must be YYYY-MM-DD"),
        ] {
            let mut broken = entry();
            broken.as_object_mut().unwrap().remove(key);
            let result = run(std::slice::from_ref(&f), &[broken]);
            assert_eq!(result.covered.len(), 0, "{key}");
            assert!(
                result.problems[0].contains(expected),
                "{:?}",
                result.problems
            );
        }
        let mut bad_date = entry();
        bad_date["date"] = json!("2026-99-99");
        let result = run(std::slice::from_ref(&f), &[bad_date]);
        assert!(result.problems[0].contains("date must be YYYY-MM-DD"));
        let legacy = json!({"schema": 2, "payload": {"fingerprint": "oauth.endpoint.github"}, "signature": null, "index": 0});
        let result = run(&[f], &[legacy]);
        assert_eq!(result.covered.len(), 0);
        assert!(result.problems[0].contains("legacy signed exception record"));
    }

    #[test]
    fn two_entries_for_one_finding_cover_it_once_and_pending_enrollment_only_warns() {
        let f = finding(
            "src/a.rs",
            "oauth.endpoint.github",
            "oauth-sign-in",
            "rightkit-oauth",
        );
        let result = evaluate(
            &[f],
            &[entry(), entry()],
            &Value::Null,
            &json!({"pending": true}),
        );
        assert_eq!((result.covered.len(), result.violations.len()), (1, 0));
        assert!(result.problems.is_empty());
        assert!(result
            .warnings
            .iter()
            .any(|w| w.contains("enrollment pending")));
    }
}
