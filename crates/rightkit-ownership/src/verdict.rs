//! Protected verdict contract. Port of `release/ownership-verdict.mjs`: a fail-closed
//! classification of one scan report against the protected trust registry, plus the
//! authenticated cache key over the six scan bindings.
//!
//! The cache key deliberately covers the content inventory (which includes dirty and
//! untracked content), trust, approvals, scanner policy and protected floors. A HEAD
//! tree hash is never an input, so a clean HEAD cannot reuse a verdict for edited
//! files.

use serde_json::Value;

use crate::approval::{validate_ownership_floor, verify_repository_registration};
use crate::json::{
    any_string, boolean, digest, field, fields, items, nullable_boolean, nullable_text,
    safe_ordinal, sha256_json, str_of, strings, text, truthy, version, Check,
};
use crate::OwnershipError;

/// Refusal and admission codes, identical to Node.
pub mod codes {
    pub const CLEAN: &str = "RK_OWNERSHIP_CLEAN";
    pub const ADVISORY: &str = "RK_OWNERSHIP_ADVISORY";
    pub const VIOLATION: &str = "RK_OWNERSHIP_VIOLATION";
    pub const PENDING_ENROLLMENT: &str = "RK_OWNERSHIP_PENDING_ENROLLMENT";
    pub const MALFORMED_REPORT: &str = "RK_OWNERSHIP_MALFORMED_REPORT";
    pub const INCOMPLETE_COVERAGE: &str = "RK_OWNERSHIP_INCOMPLETE_COVERAGE";
    pub const INVALID_REGISTRATION: &str = "RK_OWNERSHIP_INVALID_REGISTRATION";
    pub const POLICY_DOWNGRADE: &str = "RK_OWNERSHIP_POLICY_DOWNGRADE";
    pub const REPORT_FAILED: &str = "RK_OWNERSHIP_REPORT_FAILED";
    pub const UNVERIFIED: &str = "RK_OWNERSHIP_UNVERIFIED";
    pub const INVALID_INPUTS: &str = "RK_OWNERSHIP_INVALID_INPUTS";
    pub const INPUTS_CHANGED: &str = "RK_OWNERSHIP_INPUTS_CHANGED";
}

/// Result of the verdict contract (`{admit, code, problems}` in Node).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub admit: bool,
    pub code: &'static str,
    pub problems: Vec<String>,
    /// Reported warnings on an admitted verdict (pending approval enrollment); never a refusal (#262).
    pub warnings: Vec<String>,
}

fn refuse(code: &'static str, problems: Vec<String>) -> Decision {
    Decision {
        admit: false,
        code,
        problems,
        warnings: Vec::new(),
    }
}

/// Only `block`-tier or banned findings fail a scan; `advise` findings are reported.
pub fn is_blocking_finding(finding: &Value) -> bool {
    field(finding, "policy").as_str() == Some("banned")
        || field(finding, "tier").as_str() != Some("advise")
}

// ---- schema validation (Node `validReport` and its helpers) -------------------------

fn kind_ok(value: &Value) -> bool {
    matches!(
        value.as_str(),
        Some("source" | "dependency" | "transitive-dependency")
    )
}

fn tier_ok(value: &Value) -> bool {
    matches!(value.as_str(), Some("block" | "advise"))
}

fn dependency_path(value: &Value) -> bool {
    strings(value) && items(value).len() >= 3
}

fn nullable_version(value: &Value) -> bool {
    value.is_null() || version(value)
}

fn nullable_digest(value: &Value) -> bool {
    value.is_null() || digest(value)
}

fn finding_with(value: &Value, covered: bool) -> bool {
    const REQUIRED: &[(&str, Check)] = &[
        ("fingerprint", text),
        ("capability", text),
        ("package", text),
        ("file", text),
        ("line", safe_ordinal),
    ];
    let mut optional: Vec<(&str, Check)> = vec![
        ("kind", kind_ok),
        ("lineText", any_string),
        ("ordinal", safe_ordinal),
        ("text", any_string),
        ("occurrenceHash", digest),
        ("lockfile", text),
        ("dependencyPath", dependency_path),
        ("ecosystem", text),
        ("name", text),
        ("alias", nullable_text),
        ("section", text),
        ("requirement", nullable_text),
        ("features", strings),
        ("defaultFeatures", nullable_boolean),
        ("source", nullable_text),
        ("resolvedVersion", nullable_text),
        ("local", boolean),
        ("provenance", nullable_text),
        ("tier", tier_ok),
        ("policy", text),
    ];
    if covered {
        optional.push(("exception", |v: &Value| v.is_object()));
    }
    if !fields(value, REQUIRED, &optional) {
        return false;
    }
    if field(value, "kind").as_str() != Some("transitive-dependency") {
        return true;
    }
    let path = items(field(value, "dependencyPath"));
    text(field(value, "name"))
        && text(field(value, "resolvedVersion"))
        && text(field(value, "source"))
        && strings(field(value, "dependencyPath"))
        && path.len() >= 3
        && path.last().and_then(Value::as_str) == field(value, "name").as_str()
        && field(value, "lockfile") == field(value, "file")
}

fn finding(value: &Value) -> bool {
    finding_with(value, false)
}

fn findings(value: &Value) -> bool {
    value
        .as_array()
        .is_some_and(|list| list.iter().all(finding))
}

fn covered_findings(value: &Value) -> bool {
    value
        .as_array()
        .is_some_and(|list| list.iter().all(|entry| finding_with(entry, true)))
}

fn provenance_kind(value: &Value) -> bool {
    matches!(value.as_str(), Some("third-party" | "generated"))
}

fn provenance_plain(value: &Value) -> bool {
    fields(
        value,
        &[
            ("kind", provenance_kind),
            ("requestId", text),
            ("contentDigest", digest),
        ],
        &[],
    )
}

fn provenance_with_paths(value: &Value) -> bool {
    fields(
        value,
        &[
            ("kind", provenance_kind),
            ("requestId", text),
            ("contentDigest", digest),
        ],
        &[("paths", strings)],
    )
}

fn git_digest(value: &Value) -> bool {
    value.as_str().is_some_and(|s| {
        s.strip_prefix("git:").is_some_and(|hex| {
            (40..=64).contains(&hex.len())
                && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        })
    })
}

fn digest_or_git(value: &Value) -> bool {
    digest(value) || git_digest(value)
}

fn content_digests(value: &Value) -> bool {
    value.as_array().is_some_and(|list| {
        list.iter().all(|entry| {
            fields(
                entry,
                &[("path", text), ("digest", digest_or_git)],
                &[("provenance", provenance_plain)],
            )
        })
    })
}

fn rows(value: &Value) -> bool {
    value.as_array().is_some_and(|list| {
        list.iter().all(|entry| {
            fields(
                entry,
                &[
                    ("capability", text),
                    ("owner", text),
                    ("uses", boolean),
                    ("status", text),
                    ("count", safe_ordinal),
                ],
                &[("root", text), ("owners", strings)],
            )
        })
    })
}

fn coverage(value: &Value) -> bool {
    value.as_array().is_some_and(|list| {
        list.iter().all(|entry| {
            fields(
                entry,
                &[("path", text), ("status", text), ("reason", text)],
                &[("detail", text)],
            )
        })
    })
}

fn metadata_provenance(value: &Value) -> bool {
    value
        .as_array()
        .is_some_and(|list| list.iter().all(provenance_with_paths))
}

fn key_checkpoint(value: &Value) -> bool {
    fields(value, &[("sequence", safe_ordinal), ("head", digest)], &[])
}

fn key_authority(value: &Value) -> bool {
    fields(
        value,
        &[
            ("repositoryIdentity", text),
            ("commit", text),
            ("tree", text),
        ],
        &[],
    )
}

fn roots(value: &Value) -> bool {
    value.as_array().is_some_and(|list| {
        list.iter()
            .all(|entry| fields(entry, &[("root", text)], &[("metadata", metadata)]))
    })
}

fn metadata(value: &Value) -> bool {
    fields(
        value,
        &[
            ("repositoryIdentity", nullable_text),
            ("scannerVersion", text),
            ("catalogVersion", text),
            ("catalogDigest", digest),
            ("coverage", coverage),
        ],
        &[
            ("commit", nullable_text),
            ("tree", text),
            ("host", text),
            ("scannedAt", text),
            ("contentDigests", content_digests),
            ("pathBase", text),
            ("pathPrefix", any_string),
            ("provenance", metadata_provenance),
            ("exceptionDigest", digest),
            ("source", text),
            ("keyLogDigest", digest),
            ("keyCheckpoint", key_checkpoint),
            ("rootFingerprint", digest),
            ("keyAuthority", key_authority),
            ("roots", roots),
        ],
    )
}

fn valid_report(value: &Value) -> bool {
    fields(
        value,
        &[
            ("ok", boolean),
            ("enrollmentPending", boolean),
            ("violations", findings),
            ("problems", strings),
            ("coverage", coverage),
            ("metadata", metadata),
        ],
        &[
            ("findings", findings),
            ("covered", covered_findings),
            ("rows", rows),
            ("warnings", strings),
            ("warning", text),
        ],
    )
}

// ---- classification -----------------------------------------------------------------

fn describe(finding: &Value) -> String {
    format!(
        "{} ({} in {}) is owned by {}",
        str_of(field(finding, "capability")),
        str_of(field(finding, "fingerprint")),
        str_of(field(finding, "file")),
        str_of(field(finding, "package")),
    )
}

fn strings_of(value: &Value) -> Vec<String> {
    items(value).iter().map(|s| str_of(s).to_string()).collect()
}

fn strings_at(value: &Value, key: &str) -> Vec<String> {
    items(value)
        .iter()
        .map(|entry| str_of(field(entry, key)).to_string())
        .collect()
}

/// Roots the scan actually covered: `metadata.roots`, or the legacy `pathPrefix`.
fn scanned_roots(metadata: &Value) -> Vec<String> {
    match field(metadata, "roots").as_array() {
        Some(entries) => entries
            .iter()
            .map(|entry| str_of(field(entry, "root")).to_string())
            .collect(),
        None => {
            let prefix = str_of(field(metadata, "pathPrefix"));
            let trimmed = prefix.strip_suffix('/').unwrap_or(prefix);
            vec![if trimmed.is_empty() {
                ".".to_string()
            } else {
                trimmed.to_string()
            }]
        }
    }
}

fn unique(items: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for item in items {
        if !out.contains(&item) {
            out.push(item);
        }
    }
    out
}

/// Node `ownershipVerdict`: one fail-closed contract for scan workers and admission.
pub fn ownership_verdict(report: &Value, trust: &Value) -> Decision {
    if !valid_report(report) {
        return refuse(
            codes::MALFORMED_REPORT,
            vec!["ownership report has missing, unknown, or wrong-typed fields".into()],
        );
    }
    // Pending approval enrollment is a reported warning, never a refusal (#262): every real finding gate below still applies.
    let pending = field(report, "enrollmentPending").as_bool() == Some(true);
    let warnings: Vec<String> = if pending {
        vec!["protected ownership approval enrollment is pending (warning only; it existed only for retired signed approvals)".to_string()]
    } else {
        Vec::new()
    };
    let no_registry = pending && !truthy(trust);
    let metadata = field(report, "metadata");
    let mut files = strings_at(field(report, "violations"), "file");
    files.extend(strings_at(field(report, "findings"), "file"));
    files.extend(strings_at(field(metadata, "contentDigests"), "path"));
    if !no_registry {
        let mut registered = match verify_repository_registration(
            trust,
            field(metadata, "repositoryIdentity"),
            &files,
        ) {
            Ok(roots) => roots,
            Err(problems) => return refuse(codes::INVALID_REGISTRATION, problems),
        };
        let mut scanned = scanned_roots(metadata);
        scanned.sort();
        registered.sort();
        if scanned.windows(2).any(|pair| pair[0] == pair[1]) || scanned != registered {
            return refuse(
                codes::INVALID_REGISTRATION,
                vec!["scan roots do not exactly cover protected repository registration".into()],
            );
        }
        if let Err(problems) = validate_ownership_floor(trust, metadata) {
            return refuse(codes::POLICY_DOWNGRADE, problems);
        }
    }
    let incomplete: Vec<String> = items(field(report, "coverage"))
        .iter()
        .chain(items(field(metadata, "coverage")))
        .filter(|entry| field(entry, "status").as_str() != Some("complete"))
        .map(|entry| {
            format!(
                "{}: {}",
                str_of(field(entry, "path")),
                str_of(field(entry, "reason"))
            )
        })
        .collect();
    let incomplete = unique(incomplete);
    if !incomplete.is_empty() {
        return refuse(codes::INCOMPLETE_COVERAGE, incomplete);
    }
    let violations = items(field(report, "violations"));
    let blocking: Vec<String> = violations
        .iter()
        .filter(|f| is_blocking_finding(f))
        .map(describe)
        .collect();
    if !blocking.is_empty() {
        return refuse(codes::VIOLATION, blocking);
    }
    let problems = strings_of(field(report, "problems"));
    if !problems.is_empty() {
        return refuse(codes::UNVERIFIED, problems);
    }
    if field(report, "ok").as_bool() != Some(true) {
        return refuse(
            codes::REPORT_FAILED,
            vec!["protected ownership scanner reported ok:false".into()],
        );
    }
    let advisory: Vec<String> = violations
        .iter()
        .filter(|f| !is_blocking_finding(f))
        .map(|f| format!("{} [advise]", describe(f)))
        .collect();
    if !advisory.is_empty() {
        return Decision {
            admit: true,
            code: codes::ADVISORY,
            problems: advisory,
            warnings,
        };
    }
    Decision {
        admit: true,
        code: codes::CLEAN,
        problems: Vec::new(),
        warnings,
    }
}

/// Authenticated cache key: SHA-256 over the six required bindings. Missing or
/// malformed bindings are an error, never a key.
pub fn ownership_cache_key(inputs: &Value) -> Result<String, OwnershipError> {
    let floors = |value: &Value| {
        fields(
            value,
            &[
                ("minimumScannerVersion", nullable_version),
                ("minimumCatalogVersion", nullable_version),
                ("minimumCatalogDigest", nullable_digest),
            ],
            &[],
        )
    };
    let valid = fields(
        inputs,
        &[
            ("contentDigest", digest),
            ("catalogDigest", digest),
            ("trustDigest", digest),
            ("approvalsDigest", digest),
            ("scannerPolicyDigest", digest),
            ("protectedFloors", floors),
        ],
        &[],
    );
    if !valid {
        return Err(OwnershipError {
            code: codes::INVALID_INPUTS,
            message:
                "ownership cache inputs have missing, unknown, or malformed digest/floor fields"
                    .into(),
        });
    }
    Ok(sha256_json(inputs))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn blocking_rule_matches_node() {
        assert!(is_blocking_finding(&json!({})));
        assert!(is_blocking_finding(&json!({"tier": "block"})));
        assert!(is_blocking_finding(
            &json!({"tier": "advise", "policy": "banned"})
        ));
        assert!(!is_blocking_finding(&json!({"tier": "advise"})));
    }

    #[test]
    fn cache_key_rejects_missing_binding_and_is_input_sensitive() {
        let digest = |n: u8| format!("{n:064x}");
        let base = json!({
            "contentDigest": digest(1), "catalogDigest": digest(2), "trustDigest": digest(3),
            "approvalsDigest": digest(4), "scannerPolicyDigest": digest(5),
            "protectedFloors": {"minimumScannerVersion": "2.0.0", "minimumCatalogVersion": "1", "minimumCatalogDigest": null}
        });
        let key = ownership_cache_key(&base).unwrap();
        assert_eq!(key, ownership_cache_key(&base).unwrap());
        let mut changed = base.clone();
        changed["contentDigest"] = json!(digest(9));
        assert_ne!(key, ownership_cache_key(&changed).unwrap());
        let mut missing = base.clone();
        missing.as_object_mut().unwrap().remove("trustDigest");
        assert_eq!(
            ownership_cache_key(&missing).unwrap_err().code,
            codes::INVALID_INPUTS
        );
        let mut extra = base;
        extra["headTreeHash"] = json!(digest(7));
        assert_eq!(
            ownership_cache_key(&extra).unwrap_err().code,
            codes::INVALID_INPUTS
        );
    }
}
