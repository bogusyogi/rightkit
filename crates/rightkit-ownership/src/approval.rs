//! Protected trust-registry helpers used by the verdict contract. Port of the subset
//! of `release/ownership-approval.mjs` that `ownership-verdict.mjs` calls: canonical
//! repository identity, registry validation, scan-root registration and version
//! floors. Approval signatures, the key log, key authority and external trust loading
//! are not ported; the trust document arrives from the scanner worker as it does in
//! Node.

use serde_json::Value;
use url::Url;

use crate::json::{
    canonical_json, digest, field, has, is_dotted_version, is_hex64, items, sha256_hex,
    sha256_json, str_of, truthy,
};
use crate::keylog::{self, verify_key_log};
use serde_json::{json, Map};

/// Canonical `https://host[:port]/path` identity, or the Node error message.
///
/// Parsing is WHATWG (the `url` crate), as Node's `URL`: IDNA hosts become punycode for
/// `https` but stay opaque and percent-encoded for `ssh`, dot segments resolve before the
/// path checks, backslashes in `https` paths become `/`, and percent-encoding is kept
/// as written.
pub fn canonical_repository_identity(value: &Value) -> Result<String, String> {
    let Some(text) = value.as_str() else {
        return Err("repository identity is required".into());
    };
    let trimmed = js_trim(text);
    if trimmed.is_empty() {
        return Err("repository identity is required".into());
    }
    let raw = match scp_remote(trimmed) {
        Some((host, path)) => format!("ssh://{host}/{path}"),
        None => trimmed.to_string(),
    };
    let url = Url::parse(&raw).map_err(|_| {
        "repository identity must be an absolute HTTPS/SSH URL or SSH scp remote".to_string()
    })?;
    let unsupported = || "unsupported repository identity URL".to_string();
    let host = url.host_str().unwrap_or("");
    if !matches!(url.scheme(), "https" | "ssh") || host.is_empty() {
        return Err(unsupported());
    }
    if (url.scheme() == "https" && !url.username().is_empty())
        || url.password().is_some_and(|p| !p.is_empty())
    {
        return Err(unsupported());
    }
    // JavaScript `search` and `hash` are empty for a bare `?` or `#`.
    if url.query().is_some_and(|q| !q.is_empty()) || url.fragment().is_some_and(|f| !f.is_empty()) {
        return Err(unsupported());
    }
    let hostname = host.to_lowercase();
    let mut repo = url
        .path()
        .trim_start_matches('/')
        .trim_end_matches('/')
        .to_string();
    if repo.len() >= 4
        && repo.is_char_boundary(repo.len() - 4)
        && repo[repo.len() - 4..].eq_ignore_ascii_case(".git")
    {
        repo.truncate(repo.len() - 4);
    }
    if hostname == "github.com" {
        repo = repo.to_lowercase();
    }
    if repo.is_empty()
        || repo
            .split('/')
            .any(|seg| seg.is_empty() || seg == "." || seg == "..")
    {
        return Err("invalid repository identity path".into());
    }
    let port = url
        .port()
        .map(|port| format!(":{port}"))
        .unwrap_or_default();
    Ok(format!("https://{hostname}{port}/{repo}"))
}

/// JavaScript `String.prototype.trim`: Unicode White_Space plus the BOM.
pub(crate) fn js_trim(text: &str) -> &str {
    text.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
}

/// `^([^@/:]+)@([^:]+):(.+)$`, returning `(host, path)`.
fn scp_remote(raw: &str) -> Option<(&str, &str)> {
    let (user, rest) = raw.split_once('@')?;
    if user.is_empty() || user.contains(['/', ':']) {
        return None;
    }
    let (host, path) = rest.split_once(':')?;
    // `.` does not match line terminators in JavaScript, and `[^:]+` may not be empty.
    if host.is_empty() || path.is_empty() || path.contains(['\n', '\r', '\u{2028}', '\u{2029}']) {
        return None;
    }
    Some((host, path))
}

/// Whether `path` is an exact repository-relative path (no absolute, backslash,
/// empty, dot or glob segments).
pub fn exact_repository_path(path: &str) -> bool {
    path == "."
        || (!path.starts_with('/')
            && !path.contains('\\')
            && path
                .split('/')
                .all(|seg| !seg.is_empty() && seg != "." && seg != "..")
            && !path.contains(['*', '?', '[', ']', '{', '}']))
}

fn within_root(file: &str, root: &str) -> bool {
    root == "."
        || file == root
        || file
            .strip_prefix(root)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn is_absolute_trust_path(path: &str) -> bool {
    if cfg!(windows) {
        let bytes = path.as_bytes();
        path.starts_with("\\\\")
            || path.starts_with('/')
            || (bytes.len() >= 3
                && bytes[0].is_ascii_alphabetic()
                && bytes[1] == b':'
                && matches!(bytes[2], b'/' | b'\\'))
    } else {
        path.starts_with('/')
    }
}

/// Problems with the protected trust registry (Node `validateTrustRegistry`).
/// Empty means valid.
pub fn validate_trust_registry(trust: &Value) -> Vec<String> {
    let mut problems = Vec::new();
    if field(trust, "schema").as_f64() != Some(1.0) {
        problems.push("protected trust schema must be 1".into());
    }
    let revoked_ok = field(trust, "revokedKeyIds").is_array();
    let evidence = field(trust, "verifiedTrackingEvidence");
    let evidence_ok = evidence
        .as_array()
        .is_some_and(|list| list.iter().all(|x| x.as_str().is_some_and(is_hex64)));
    if !revoked_ok || !evidence_ok {
        problems.push("trust revocation or tracking evidence lists are malformed".into());
    }
    let floors_ok = field(trust, "minimumScannerVersion")
        .as_str()
        .is_some_and(is_dotted_version)
        && field(trust, "minimumCatalogVersion")
            .as_str()
            .is_some_and(is_dotted_version);
    if !floors_ok {
        problems.push("scanner/catalog floors are missing or malformed".into());
    }
    if has(trust, "minimumCatalogDigest") && !digest(field(trust, "minimumCatalogDigest")) {
        problems.push("minimum catalog digest is malformed".into());
    }
    if has(trust, "rootFingerprint") || has(trust, "keyCheckpoint") {
        let checkpoint = field(trust, "keyCheckpoint");
        let checkpoint_ok = truthy(checkpoint)
            && crate::json::safe_ordinal(field(checkpoint, "sequence"))
            && digest(field(checkpoint, "head"));
        if !digest(field(trust, "rootFingerprint")) || !checkpoint_ok {
            problems.push("protected key-log root pin or checkpoint is malformed".into());
        }
    }
    if has(trust, "keyAuthority") {
        let authority = field(trust, "keyAuthority");
        let keys_ok = authority.as_object().is_some_and(|map| {
            map.len() == 2
                && map.contains_key("checkoutPath")
                && map.contains_key("repositoryIdentity")
        }) && field(authority, "checkoutPath")
            .as_str()
            .is_some_and(is_absolute_trust_path);
        if !keys_ok {
            problems.push("protected key authority is malformed".into());
        }
        match canonical_repository_identity(field(authority, "repositoryIdentity")) {
            Ok(identity)
                if Some(identity.as_str()) == field(authority, "repositoryIdentity").as_str() => {}
            Ok(_) => problems.push("protected key authority identity is noncanonical".into()),
            Err(_) => problems.push("protected key authority identity is invalid".into()),
        }
    }
    let Some(repositories) = field(trust, "repositories").as_array() else {
        return vec!["protected repository registry missing".into()];
    };
    let mut seen = std::collections::HashSet::new();
    for entry in repositories {
        let identity = match canonical_repository_identity(field(entry, "repositoryIdentity")) {
            Ok(identity) => identity,
            Err(_) => {
                problems.push("repository registry contains invalid identity".into());
                continue;
            }
        };
        if Some(identity.as_str()) != field(entry, "repositoryIdentity").as_str()
            || !seen.insert(identity.clone())
        {
            problems.push(format!(
                "repository registry identity is noncanonical or duplicated: {}",
                str_of(field(entry, "repositoryIdentity"))
            ));
        }
        let roots = field(entry, "roots");
        let roots_ok = roots.as_array().is_some_and(|list| {
            !list.is_empty()
                && list
                    .iter()
                    .all(|root| root.as_str().is_some_and(exact_repository_path))
        });
        if !roots_ok {
            problems.push(format!(
                "repository registry roots must be exact paths: {identity}"
            ));
        }
        if !matches!(
            field(entry, "visibility").as_str(),
            Some("public" | "private")
        ) {
            problems.push(format!("repository visibility is invalid: {identity}"));
        }
    }
    let provenance = field(trust, "provenance");
    if !provenance.is_null() && !provenance.is_array() {
        problems.push("protected provenance registry is malformed".into());
    }
    for record in items(provenance) {
        let paths_ok = record
            .get("paths")
            .and_then(Value::as_array)
            .is_some_and(|paths| {
                !paths.is_empty()
                    && paths
                        .iter()
                        .all(|p| p.as_str().is_some_and(exact_repository_path))
            });
        let kind_ok = matches!(
            field(record, "kind").as_str(),
            Some("third-party" | "generated")
        );
        if !truthy(field(record, "repositoryIdentity"))
            || !paths_ok
            || !digest(field(record, "contentDigest"))
            || !kind_ok
            || !truthy(field(record, "requestId"))
        {
            problems.push("protected provenance entry is incomplete or malformed".into());
        }
        match canonical_repository_identity(field(record, "repositoryIdentity")) {
            Ok(identity)
                if Some(identity.as_str()) == field(record, "repositoryIdentity").as_str() => {}
            Ok(_) => problems.push("provenance repository identity is noncanonical".into()),
            Err(_) => problems.push("provenance repository identity is invalid".into()),
        }
    }
    problems
}

/// Verify that `repository_identity` is registered and that `files` lie inside its
/// registered roots. `Ok` carries the registered roots; `Err` carries the problems.
pub fn verify_repository_registration(
    trust: &Value,
    repository_identity: &Value,
    files: &[String],
) -> Result<Vec<String>, Vec<String>> {
    let validation = validate_trust_registry(trust);
    if !validation.is_empty() {
        return Err(validation);
    }
    // Node reports every identity failure as the same problem, whatever the parse error.
    let identity = canonical_repository_identity(repository_identity)
        .map_err(|_| vec!["invalid repository identity".to_string()])?;
    let registration = items(field(trust, "repositories"))
        .iter()
        .find(|entry| field(entry, "repositoryIdentity").as_str() == Some(identity.as_str()))
        .ok_or_else(|| vec!["repository is not registered in protected trust".to_string()])?;
    let roots: Vec<String> = items(field(registration, "roots"))
        .iter()
        .map(|r| str_of(r).to_string())
        .collect();
    let problems: Vec<String> = files
        .iter()
        .filter(|file| {
            !exact_repository_path(file) || !roots.iter().any(|root| within_root(file, root))
        })
        .map(|file| format!("path outside registered repository roots or malformed: {file}"))
        .collect();
    if problems.is_empty() {
        Ok(roots)
    } else {
        Err(problems)
    }
}

/// `a >= b` for dotted numeric versions (Node `versionAtLeast`). Missing or malformed
/// input is never at least anything.
pub(crate) fn version_at_least(actual: Option<&str>, minimum: Option<&str>) -> bool {
    fn parse(version: Option<&str>) -> Option<Vec<f64>> {
        version
            .unwrap_or("")
            .split('.')
            .map(|part| {
                if !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()) {
                    part.parse::<f64>().ok()
                } else {
                    None
                }
            })
            .collect()
    }
    let (Some(a), Some(b)) = (parse(actual), parse(minimum)) else {
        return false;
    };
    for index in 0..a.len().max(b.len()) {
        let left = a.get(index).copied().unwrap_or(0.0);
        let right = b.get(index).copied().unwrap_or(0.0);
        if left > right {
            return true;
        }
        if left < right {
            return false;
        }
    }
    true
}

/// Warning for catalog digest drift; must equal the Node `CATALOG_DIGEST_DRIFT_WARNING` byte for byte.
pub const CATALOG_DIGEST_DRIFT_WARNING: &str = "ownership catalog digest differs from the digest pinned in protected trust (warning only; the catalog version still meets the protected minimum; trust-sync is retired)";

/// Result of the ownership floor check: `problems` refuse, `warnings` are reported only.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OwnershipFloor {
    pub problems: Vec<String>,
    pub warnings: Vec<String>,
}

impl OwnershipFloor {
    pub fn ok(&self) -> bool {
        self.problems.is_empty()
    }
}

/// Scanner and catalog version floors are hard; a catalog digest that differs from the pinned
/// digest is a warning (trust-sync is retired, decisions.md 2026-10-15). Node `validateOwnershipFloor`.
pub fn validate_ownership_floor(trust: &Value, metadata: &Value) -> OwnershipFloor {
    if !truthy(trust) {
        return OwnershipFloor {
            problems: vec!["external ownership trust unavailable".into()],
            warnings: Vec::new(),
        };
    }
    let mut floor = OwnershipFloor::default();
    if !version_at_least(
        field(metadata, "scannerVersion").as_str(),
        field(trust, "minimumScannerVersion").as_str(),
    ) {
        floor
            .problems
            .push("scanner version missing, malformed, or below protected minimum".into());
    }
    if !version_at_least(
        field(metadata, "catalogVersion").as_str(),
        field(trust, "minimumCatalogVersion").as_str(),
    ) {
        floor
            .problems
            .push("catalog version missing, malformed, or below protected minimum".into());
    }
    let pinned = field(trust, "minimumCatalogDigest");
    if truthy(pinned) && pinned != field(metadata, "catalogDigest") {
        floor.warnings.push(CATALOG_DIGEST_DRIFT_WARNING.into());
    }
    floor
}

// ---- approval records ------------------------------------------------------------

pub const APPROVAL_SCHEMA: i64 = 2;
pub const REQUIRED_HOSTS: [&str; 2] = ["windows", "macos"];

fn exact_keys(value: &Value, keys: &[&str]) -> bool {
    value
        .as_object()
        .is_some_and(|map| map.len() == keys.len() && keys.iter().all(|k| map.contains_key(*k)))
}

/// `/^[a-z0-9][a-z0-9._-]{1,127}$/` and not a SHA-256 hex digest.
fn valid_fingerprint(value: &Value) -> bool {
    value.as_str().is_some_and(|s| {
        (2..=128).contains(&s.len())
            && s.as_bytes()[0].is_ascii_lowercase_or_digit()
            && s.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
            })
            && !is_hex64(s)
    })
}

trait AsciiExt {
    fn is_ascii_lowercase_or_digit(&self) -> bool;
}

impl AsciiExt for u8 {
    fn is_ascii_lowercase_or_digit(&self) -> bool {
        self.is_ascii_lowercase() || self.is_ascii_digit()
    }
}

fn occurrence_path(value: &Value) -> bool {
    value.as_str().is_some_and(|s| {
        !s.is_empty()
            && s != "."
            && exact_repository_path(s)
            && !(s.len() >= 2 && s.as_bytes()[0].is_ascii_alphabetic() && s.as_bytes()[1] == b':')
    })
}

/// `null` or a non-empty string that is present. Absent is not nullable.
fn nullable_string_present(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Null) => true,
        Some(Value::String(s)) => !s.is_empty(),
        _ => false,
    }
}

fn dependency_source_ok(source: &str) -> bool {
    source == "registry"
        || (source.starts_with("git+")
            && source.len() > 4
            && !source[4..].chars().any(char::is_whitespace))
        || (source.starts_with("path:") && source.len() > 5)
}

/// Stable identity of one finding occurrence (Node `occurrenceHash`).
pub fn occurrence_hash(finding: &Value) -> Result<String, String> {
    const INVALID: &str = "invalid occurrence fingerprint or path";
    if !finding.is_object()
        || !valid_fingerprint(field(finding, "fingerprint"))
        || !occurrence_path(field(finding, "file"))
    {
        return Err(INVALID.into());
    }
    let kind = match field(finding, "kind") {
        Value::Null => {
            if truthy(field(finding, "ecosystem")) {
                "dependency".to_string()
            } else {
                "source".to_string()
            }
        }
        other => other.as_str().unwrap_or("?").to_string(),
    };
    let fingerprint = field(finding, "fingerprint");
    let file = field(finding, "file");
    if kind == "source" {
        let line_text = field(finding, "lineText").as_str();
        if line_text.is_none_or(|t| t.contains(['\r', '\n']))
            || !crate::json::safe_ordinal(field(finding, "ordinal"))
        {
            return Err("source occurrence requires full lineText and nonnegative ordinal".into());
        }
        return Ok(sha256_json(
            &json!({"v": 2, "kind": kind, "fingerprint": fingerprint, "path": file, "lineText": field(finding, "lineText"), "ordinal": field(finding, "ordinal")}),
        ));
    }
    let non_empty = |key: &str| field(finding, key).as_str().is_some_and(|s| !s.is_empty());
    if kind == "transitive-dependency" {
        let path = field(finding, "dependencyPath").as_array();
        let ecosystem = field(finding, "ecosystem").as_str().unwrap_or("");
        let path_ok = path.is_some_and(|p| {
            p.len() >= 3
                && p.iter().all(|n| n.as_str().is_some_and(|s| !s.is_empty()))
                && p.last() == Some(field(finding, "name"))
        });
        if !matches!(ecosystem, "cargo" | "npm")
            || !non_empty("name")
            || !non_empty("resolvedVersion")
            || !non_empty("source")
            || !path_ok
        {
            return Err("malformed transitive dependency occurrence identity".into());
        }
        return Ok(sha256_json(
            &json!({"v": 2, "kind": kind, "fingerprint": fingerprint, "path": file, "ecosystem": ecosystem, "name": field(finding, "name"), "resolvedVersion": field(finding, "resolvedVersion"), "source": field(finding, "source"), "dependencyPath": field(finding, "dependencyPath")}),
        ));
    }
    if kind != "dependency" {
        return Err("unknown occurrence kind".into());
    }
    let raw_ecosystem = field(finding, "ecosystem").as_str().unwrap_or("");
    let ecosystem = if raw_ecosystem == "swiftpm" {
        "swift"
    } else {
        raw_ecosystem
    };
    let features = finding.get("features").and_then(Value::as_array);
    let features_ok =
        features.is_some_and(|f| f.iter().all(|x| x.as_str().is_some_and(|s| !s.is_empty())));
    let default_features_ok = matches!(
        finding.get("defaultFeatures"),
        Some(Value::Bool(_) | Value::Null)
    );
    let source_ok = field(finding, "source")
        .as_str()
        .is_some_and(dependency_source_ok);
    if !matches!(ecosystem, "cargo" | "npm" | "swift")
        || !non_empty("name")
        || !non_empty("section")
        || !nullable_string_present(finding.get("alias"))
        || !nullable_string_present(finding.get("requirement"))
        || !nullable_string_present(finding.get("resolvedVersion"))
        || !features_ok
        || !default_features_ok
        || !source_ok
    {
        return Err("malformed dependency occurrence identity".into());
    }
    let mut unique: Vec<String> = features
        .expect("checked")
        .iter()
        .map(|f| str_of(f).to_string())
        .collect();
    unique.sort();
    unique.dedup();
    Ok(sha256_json(&json!({
        "v": 2, "kind": kind, "fingerprint": fingerprint, "path": file, "ecosystem": ecosystem, "name": field(finding, "name"),
        "alias": field(finding, "alias"), "section": field(finding, "section"), "requirement": field(finding, "requirement"),
        "features": unique, "defaultFeatures": field(finding, "defaultFeatures"), "source": field(finding, "source"),
        "resolvedVersion": field(finding, "resolvedVersion"),
    })))
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `^\d{4}-\d{2}-\d{2}$` naming a real calendar date.
fn parse_date(value: &str) -> Option<i64> {
    let bytes = value.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    if !bytes
        .iter()
        .enumerate()
        .all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit())
    {
        return None;
    }
    let (y, m, d): (i64, i64, i64) = (
        value[..4].parse().ok()?,
        value[5..7].parse().ok()?,
        value[8..].parse().ok()?,
    );
    if !(1..=12).contains(&m) || d < 1 {
        return None;
    }
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let dim = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ][(m - 1) as usize];
    (d <= dim).then(|| days_from_civil(y, m, d))
}

/// A real calendar date in `YYYY-MM-DD` form.
pub fn is_valid_date(value: &str) -> bool {
    parse_date(value).is_some()
}

/// SHA-256 over canonical JSON (Node `sha256(value)`).
pub fn sha256_value(value: &Value) -> String {
    sha256_json(value)
}

fn valid_date(value: &Value) -> bool {
    value.as_str().is_some_and(|s| parse_date(s).is_some())
}

fn is_request_id(value: &str) -> bool {
    // ^[A-Za-z0-9][A-Za-z0-9._:-]{2,127}$
    (3..=128).contains(&value.len())
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

fn is_canonical_base64(value: &str) -> bool {
    use base64::Engine;
    !value.is_empty()
        && base64::engine::general_purpose::STANDARD
            .decode(value)
            .is_ok_and(|b| base64::engine::general_purpose::STANDARD.encode(b) == value)
}

/// Schema problems of one approval envelope (Node `validateRecord`).
pub fn validate_record(record: &Value) -> Vec<String> {
    if !exact_keys(record, &["schema", "payload", "signature"])
        || field(record, "schema").as_f64() != Some(APPROVAL_SCHEMA as f64)
    {
        return vec![
            "approval envelope must use schema 2 and exactly schema, payload, signature".into(),
        ];
    }
    let r = field(record, "payload");
    if !exact_keys(
        r,
        &[
            "repositoryIdentity",
            "fingerprint",
            "policyDigest",
            "minimumScannerVersion",
            "minimumCatalogVersion",
            "occurrences",
            "reason",
            "owner",
            "issued",
            "expires",
            "requestId",
            "trackingRef",
        ],
    ) {
        return vec!["approval payload has missing or unknown keys".into()];
    }
    let mut p: Vec<String> = Vec::new();
    let identity = field(r, "repositoryIdentity");
    if !identity
        .as_str()
        .is_some_and(|s| s.encode_utf16().count() <= 512)
    {
        p.push("invalid repository identity".into());
    }
    match canonical_repository_identity(identity) {
        Ok(canonical) if Some(canonical.as_str()) == identity.as_str() => {}
        Ok(_) => p.push("approval repository identity is not canonical".into()),
        Err(_) => p.push("invalid repository identity".into()),
    }
    if !valid_fingerprint(field(r, "fingerprint")) {
        p.push("invalid catalog fingerprint id".into());
    }
    if !digest(field(r, "policyDigest")) {
        p.push("invalid policy digest".into());
    }
    for key in ["minimumScannerVersion", "minimumCatalogVersion"] {
        if !field(r, key).as_str().is_some_and(is_dotted_version) {
            p.push(format!("invalid {key}"));
        }
    }
    let occurrences = field(r, "occurrences").as_array();
    match occurrences {
        Some(list) if !list.is_empty() && list.len() <= 256 => {
            let mut pairs = std::collections::HashSet::new();
            for item in list {
                let ok = exact_keys(item, &["path", "kind", "hash"])
                    && occurrence_path(field(item, "path"))
                    && matches!(field(item, "kind").as_str(), Some("source" | "dependency"))
                    && digest(field(item, "hash"));
                if !ok {
                    p.push("occurrences require exact repository-relative path, known kind, and hex hash".into());
                    continue;
                }
                if !pairs.insert(format!(
                    "{}\0{}",
                    str_of(field(item, "path")),
                    str_of(field(item, "hash"))
                )) {
                    p.push("duplicate occurrence path/hash pair".into());
                }
            }
        }
        _ => p.push("occurrences must contain 1 to 256 entries".into()),
    }
    if !valid_date(field(r, "issued")) {
        p.push("invalid issued date".into());
    }
    if !valid_date(field(r, "expires")) {
        p.push("invalid expiry date".into());
    }
    if !field(r, "requestId").as_str().is_some_and(is_request_id) {
        p.push("invalid request id".into());
    }
    let tracking = field(r, "trackingRef");
    let tracking_url = field(tracking, "url")
        .as_str()
        .and_then(|u| Url::parse(u).ok());
    let tracking_ok = exact_keys(tracking, &["id", "url", "verified"])
        && tracking_url.as_ref().is_some_and(|u| {
            u.scheme() == "https"
                && u.host_str().is_some_and(|h| !h.is_empty())
                && u.username().is_empty()
                && u.password().is_none()
        })
        && field(tracking, "id")
            .as_str()
            .is_some_and(|s| !s.is_empty())
        && field(tracking, "verified").as_bool() == Some(true);
    if !tracking_ok {
        p.push("tracking reference must be verified and https-backed".into());
    }
    if !field(r, "reason")
        .as_str()
        .is_some_and(|s| !s.trim().is_empty() && s.encode_utf16().count() <= 4000)
    {
        p.push("invalid approval reason".into());
    }
    if !field(r, "owner")
        .as_str()
        .is_some_and(|s| !s.trim().is_empty() && s.encode_utf16().count() <= 256)
    {
        p.push("invalid owner".into());
    }
    let signature = field(record, "signature");
    if !signature.is_null() {
        let host = str_of(field(signature, "host"));
        let prefix = if host == "windows" { "win-" } else { "mac-" };
        let key_id = str_of(field(signature, "keyId"));
        let key_ok = key_id.strip_prefix(prefix).is_some_and(is_hex64);
        let ok = exact_keys(signature, &["keyId", "host", "value"])
            && REQUIRED_HOSTS.contains(&host)
            && key_ok
            && field(signature, "value")
                .as_str()
                .is_some_and(is_canonical_base64);
        if !ok {
            p.push("malformed signature envelope".into());
        }
    }
    p
}

/// Key id for a host key: `win-` or `mac-` plus the SPKI digest.
pub fn key_id_for_public_key(host: &str, public_key_pem: &str) -> Result<String, String> {
    if !REQUIRED_HOSTS.contains(&host) {
        return Err("unknown ownership approval host".into());
    }
    let key =
        keylog::parse_public_key(public_key_pem).ok_or_else(|| "invalid public key".to_string())?;
    let der = key.der().ok_or_else(|| "invalid public key".to_string())?;
    Ok(format!(
        "{}{}",
        if host == "windows" { "win-" } else { "mac-" },
        sha256_hex(&der)
    ))
}

/// Catalog policy digest for one fingerprint (Node `policyEntryDigest`).
pub fn policy_entry_digest(manifest: &Value, fingerprint: &Value) -> Result<String, String> {
    let label = || str_of(fingerprint).to_string();
    let capabilities = field(manifest, "capabilities").as_array();
    if !valid_fingerprint(fingerprint)
        || capabilities.is_none_or(|list| list.iter().any(|c| !field(c, "fingerprints").is_array()))
    {
        return Err(format!(
            "missing or malformed catalog policy entry: {}",
            label()
        ));
    }
    let mut matches: Vec<Value> = Vec::new();
    for capability in capabilities.expect("checked") {
        for entry in items(field(capability, "fingerprints")) {
            if field(entry, "id") == fingerprint {
                let mut merged = entry.as_object().cloned().unwrap_or_default();
                merged.insert("capability".into(), field(capability, "capability").clone());
                merged.insert("package".into(), field(capability, "package").clone());
                let tier = if truthy(field(entry, "tier")) {
                    field(entry, "tier").clone()
                } else {
                    field(capability, "tier").clone()
                };
                merged.insert("tier".into(), tier);
                matches.push(Value::Object(merged));
            }
        }
    }
    let ok = matches.len() == 1
        && ["capability", "package", "tier"]
            .iter()
            .all(|k| truthy(field(&matches[0], k)));
    if !ok {
        return Err(format!(
            "missing, ambiguous, or malformed catalog policy entry: {}",
            label()
        ));
    }
    Ok(sha256_json(&matches[0]))
}

/// Resolve protected N-device keys against the supplied immutable log bytes.
/// `Ok` carries the resolved trust, the key-log digest and the verified checkpoint.
pub struct ResolvedKeyLog {
    pub trust: Value,
    pub key_log_digest: String,
    pub checkpoint: Value,
}

pub fn resolve_key_log_trust(
    trust: &Value,
    log_bytes: Option<&[u8]>,
) -> Result<ResolvedKeyLog, Vec<String>> {
    if !truthy(field(trust, "rootFingerprint")) {
        return Err(vec!["protected key-log root pin is missing".into()]);
    }
    let checkpoint = field(trust, "keyCheckpoint");
    let pin_ok = digest(field(trust, "rootFingerprint"))
        && truthy(checkpoint)
        && crate::json::safe_ordinal(field(checkpoint, "sequence"))
        && digest(field(checkpoint, "head"));
    if !pin_ok {
        return Err(vec![
            "protected key-log root pin or checkpoint is malformed".into(),
        ]);
    }
    let bytes = log_bytes.unwrap_or(b"undefined");
    let log: Value = serde_json::from_slice(bytes)
        .map_err(|_| vec!["committed ownership key log is missing or invalid JSON".to_string()])?;
    let log_digest = sha256_hex(bytes);
    let verified = verify_key_log(&log, field(trust, "rootFingerprint"), Some(checkpoint));
    if !verified.ok {
        return Err(verified.problems);
    }
    let platforms: std::collections::HashSet<&str> = verified
        .all_keys
        .iter()
        .map(|k| str_of(field(k, "platform")))
        .collect();
    let enrollment_complete = REQUIRED_HOSTS.iter().all(|p| platforms.contains(p));
    let mut revoked: Vec<Value> = Vec::new();
    for id in items(field(trust, "revokedKeyIds"))
        .iter()
        .cloned()
        .chain(verified.revoked_key_ids.iter().map(|i| json!(i)))
    {
        if !revoked.contains(&id) {
            revoked.push(id);
        }
    }
    let mut resolved = trust.as_object().cloned().unwrap_or_default();
    resolved.insert("ownershipKeys".into(), Value::Array(verified.keys.clone()));
    resolved.insert(
        "ownershipAllKeys".into(),
        Value::Array(verified.all_keys.clone()),
    );
    resolved.insert("enrollmentComplete".into(), json!(enrollment_complete));
    resolved.insert("revokedKeyIds".into(), Value::Array(revoked));
    let (sequence, head) = verified.checkpoint.unwrap_or((-1, None));
    Ok(ResolvedKeyLog {
        trust: Value::Object(resolved),
        key_log_digest: log_digest,
        checkpoint: json!({"sequence": sequence, "head": head}),
    })
}

/// Digest over file paths and contents of a provenance record.
pub fn provenance_content_digest(files: &[(String, Vec<u8>)]) -> String {
    let mut list: Vec<(String, String)> = files
        .iter()
        .map(|(path, content)| (path.clone(), sha256_hex(content)))
        .collect();
    list.sort_by(|a, b| crate::json::locale_compare(&a.0, &b.0));
    let value: Vec<Value> = list
        .into_iter()
        .map(|(path, sha)| json!({"path": path, "sha256": sha}))
        .collect();
    sha256_json(&Value::Array(value))
}

/// Verifies a protected provenance record against the files it declares.
pub fn verify_protected_provenance(
    trust: &Value,
    repository_identity: &Value,
    paths: &[String],
    files: &[(String, Vec<u8>)],
    kind: &str,
    request_id: &str,
) -> Result<(), Vec<String>> {
    let validation = validate_trust_registry(trust);
    if !validation.is_empty() {
        return Err(validation);
    }
    let identity = canonical_repository_identity(repository_identity)
        .map_err(|_| vec!["invalid repository identity".to_string()])?;
    if paths.is_empty() || paths.iter().any(|p| !exact_repository_path(p)) {
        return Err(vec![
            "provenance paths must be exact repository-relative paths".into(),
        ]);
    }
    let mut sorted: Vec<String> = paths.to_vec();
    sorted.sort();
    sorted.dedup();
    if sorted.len() != paths.len() {
        return Err(vec!["duplicate provenance paths".into()]);
    }
    let mut file_paths: Vec<&String> = files.iter().map(|f| &f.0).collect();
    file_paths.sort();
    if files.len() != sorted.len() || file_paths.iter().zip(&sorted).any(|(a, b)| *a != b) {
        return Err(vec![
            "provenance content must exactly cover every declared path".into(),
        ]);
    }
    verify_repository_registration(trust, &json!(identity), &sorted)?;
    let content_digest = provenance_content_digest(files);
    let found = items(field(trust, "provenance")).iter().any(|record| {
        let mut record_paths: Vec<&str> =
            items(field(record, "paths")).iter().map(str_of).collect();
        record_paths.sort();
        str_of(field(record, "repositoryIdentity")) == identity
            && str_of(field(record, "kind")) == kind
            && str_of(field(record, "requestId")) == request_id
            && str_of(field(record, "contentDigest")) == content_digest
            && canonical_json(&json!(record_paths)) == canonical_json(&json!(sorted))
    });
    if found {
        Ok(())
    } else {
        Err(vec![
            "no protected provenance record matches paths and content".into(),
        ])
    }
}

// Exception records are plain, git-reviewed entries (Orthic-Labs/rightkit#262): `verify_exception`, `ExceptionContext` and the
// exception signature check are retired. Key-log, trust-registry, floor and provenance code in this module is unchanged.

/// Map helper used by callers that assemble JSON objects field by field.
pub fn object(pairs: Vec<(&str, Value)>) -> Value {
    let mut map = Map::new();
    for (key, value) in pairs {
        map.insert(key.into(), value);
    }
    Value::Object(map)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const IDENTITY: &str = "https://github.com/test/repo";

    #[test]
    fn canonical_identity_normalises_scp_port_and_git_suffix() {
        assert_eq!(
            canonical_repository_identity(&json!("git@GitHub.com:Test/Repo.git")).unwrap(),
            IDENTITY
        );
        assert_eq!(
            canonical_repository_identity(&json!("https://github.com:443/test/repo/")).unwrap(),
            IDENTITY
        );
        assert_eq!(
            canonical_repository_identity(&json!("ssh://git.example.com:2222/Team/App")).unwrap(),
            "https://git.example.com:2222/Team/App"
        );
    }

    #[test]
    fn canonical_identity_refuses_credentials_queries_and_traversal() {
        for bad in [
            "https://user:pw@github.com/test/repo",
            "https://github.com/test/repo?x=1",
            "https://github.com/test/../../",
            "file:///tmp/repo",
            "",
        ] {
            assert!(canonical_repository_identity(&json!(bad)).is_err(), "{bad}");
        }
        assert!(canonical_repository_identity(&json!(null)).is_err());
        // WHATWG resolves dot segments before Node sees the path, so this is accepted.
        assert_eq!(
            canonical_repository_identity(&json!("https://github.com/test/../x")).unwrap(),
            "https://github.com/x"
        );
    }

    #[test]
    fn exact_repository_path_rejects_absolute_glob_and_dot_segments() {
        assert!(exact_repository_path("."));
        assert!(exact_repository_path("engine/app"));
        for bad in ["/abs", "a\\b", "a//b", "./a", "a/../b", "a*", "x{y}"] {
            assert!(!exact_repository_path(bad), "{bad}");
        }
    }

    #[test]
    fn version_floor_compares_numerically() {
        assert!(version_at_least(Some("2.4.0"), Some("2.0.0")));
        assert!(version_at_least(Some("10.0"), Some("9.9.9")));
        assert!(!version_at_least(Some("1.0.0"), Some("2.0.0")));
        assert!(!version_at_least(None, Some("1")));
        assert!(!version_at_least(Some("1.x"), Some("1")));
    }
}
