//! Scan evidence: the sealed result of a protected scan over exact committed source.
//! Port of `createEvidence` in `ownership-evidence.mjs` (committed, clean-checkout,
//! pending-advisory mode, which is the only mode the worker uses).

use std::path::Path;
use std::time::Duration;

use serde_json::{json, Map, Value};

use crate::approval::{canonical_repository_identity, sha256_value, validate_ownership_floor};
use crate::committed::{acquire, assert_checkout_clean};
use crate::error::{Res, ScanError};
use crate::gitio;
use crate::json::{field, items, locale_compare, str_of, truthy};
use crate::paths;
use crate::scan::{
    catalog_digest, catalog_version, check_app, check_committed, iso_from_ms, CheckOptions,
    ScanContext, SCANNER_VERSION,
};

const WARNING: &str = "WARNING: ownership approval enrollment is pending; findings remain backlog diagnostics and no protected evidence or approvals are asserted";

pub struct Checked {
    pub ok: bool,
    pub enrollment_pending: bool,
    pub evidence: Value,
    pub result: Value,
}

fn root_contains(root: &str, candidate: &str) -> bool {
    root == "." || candidate == root || candidate.starts_with(&format!("{root}/"))
}

fn rebase(entry: &Value, prefix: &str) -> Value {
    let mut out = entry.clone();
    for key in ["path", "file"] {
        if truthy(field(entry, key)) {
            out[key] = json!(format!("{prefix}{}", str_of(field(entry, key))));
        }
    }
    out
}

fn plain_git_text(root: &Path, args: &[&str]) -> Res<String> {
    match gitio::plain_git(root, args, Duration::from_secs(30)) {
        Ok(out) if out.success() => Ok(out.text().trim().to_string()),
        _ => Err(ScanError::new(format!(
            "ownership evidence: git {} failed",
            args.join(" ")
        ))),
    }
}

fn trust_failure_check(state: &Value) -> Res<bool> {
    let pending_without_trust = field(state, "pending").as_bool() == Some(true)
        && field(state, "enrolled").as_bool() == Some(false)
        && field(state, "trust").is_null()
        && field(field(state, "protection"), "ok").as_bool() == Some(false)
        && field(field(state, "protection"), "reason").as_str()
            == Some("external trust file is absent");
    if field(field(state, "protection"), "ok").as_bool() != Some(true) && !pending_without_trust {
        return Err(ScanError::new(
            "protected ownership trust location was not verified",
        ));
    }
    if !truthy(field(state, "trust")) && !pending_without_trust {
        return Err(ScanError::new(
            "protected ownership trust registry is missing",
        ));
    }
    Ok(pending_without_trust)
}

/// Creates scan evidence for `repo_root`, scanning every protected root.
pub fn create_evidence(ctx: &ScanContext, repo_root: &Path, manifest: &Value) -> Res<Checked> {
    let root = paths::resolve(repo_root);
    let mut state = (ctx.trust_loader)()?;
    trust_failure_check(&state)?;
    let mut pending = field(&state, "enrolled").as_bool() != Some(true);
    let trust0 = field(&state, "trust").clone();
    let use_committed = !pending || truthy(field(&trust0, "rootFingerprint"));
    if use_committed {
        assert_checkout_clean(&root, None, None)?;
    }
    let mut required: Vec<String> = items(field(&trust0, "provenance"))
        .iter()
        .flat_map(|r| {
            items(field(r, "paths"))
                .iter()
                .map(|p| str_of(p).to_string())
        })
        .collect();
    if truthy(field(&trust0, "rootFingerprint")) && !truthy(field(&trust0, "keyAuthority")) {
        required.push(".rightkit/ownership-keys.json".into());
    }
    let source = if use_committed {
        Some(acquire(&root, None, None, &required)?)
    } else {
        None
    };
    let mut key_log_evidence: Map<String, Value> = Map::new();
    let trust_now = field(&state, "trust").clone();
    if truthy(field(&trust_now, "keyAuthority")) {
        let resolved = crate::keyauth::read_committed_key_authority(&trust_now, &ctx.authority)
            .map_err(|p| {
                ScanError::new(format!(
                    "protected ownership key authority failed: {}",
                    p.join("; ")
                ))
            })?;
        let complete = field(&resolved.resolved_trust, "enrollmentComplete").clone();
        state["trust"] = resolved.resolved_trust.clone();
        state["enrolled"] = complete.clone();
        state["pending"] = json!(!truthy(&complete));
        pending = !truthy(&complete);
        key_log_evidence.insert("keyLogDigest".into(), json!(resolved.digest));
        key_log_evidence.insert("keyCheckpoint".into(), resolved.key_checkpoint);
        key_log_evidence.insert("rootFingerprint".into(), resolved.root_fingerprint);
        key_log_evidence.insert("keyAuthority".into(), resolved.binding);
        if items(field(&resolved.resolved_trust, "ownershipKeys")).is_empty() {
            return Err(ScanError::new(
                "protected ownership key authority has no active approval keys",
            ));
        }
    } else if truthy(field(&trust_now, "rootFingerprint")) {
        let key_source = match &source {
            Some(s) => acquire_ref(s),
            None => acquire(
                &root,
                None,
                None,
                &[".rightkit/ownership-keys.json".to_string()],
            )?,
        };
        let entry = key_source
            .entry(".rightkit/ownership-keys.json")
            .and_then(|e| e.bytes.clone());
        let resolved = crate::approval::resolve_key_log_trust(
            &trust_now,
            entry.as_deref().map(|b| b.as_slice()),
        )
        .map_err(|p| {
            ScanError::new(format!(
                "protected ownership key log failed: {}",
                p.join("; ")
            ))
        })?;
        let complete = field(&resolved.trust, "enrollmentComplete").clone();
        state["trust"] = resolved.trust.clone();
        state["enrolled"] = complete.clone();
        state["pending"] = json!(!truthy(&complete));
        pending = !truthy(&complete);
        key_log_evidence.insert("keyLogDigest".into(), json!(resolved.key_log_digest));
        key_log_evidence.insert("keyCheckpoint".into(), resolved.checkpoint);
        key_log_evidence.insert(
            "rootFingerprint".into(),
            field(&resolved.trust, "rootFingerprint").clone(),
        );
        if items(field(&resolved.trust, "ownershipKeys")).is_empty() {
            return Err(ScanError::new(
                "protected ownership key authority has no active approval keys",
            ));
        }
    }
    if let (true, Some(source)) = (use_committed, &source) {
        assert_checkout_clean(&root, Some(&source.commit), Some(&source.tree))?;
    }
    let trust = field(&state, "trust").clone();
    let commit = match &source {
        Some(s) => s.commit.clone(),
        None => plain_git_text(&root, &["rev-parse", "HEAD"])?,
    };
    if commit.is_empty() {
        return Err(ScanError::new(
            "ownership evidence: scan source did not produce a HEAD commit",
        ));
    }
    let tree = match &source {
        Some(s) => s.tree.clone(),
        None => plain_git_text(&root, &["rev-parse", &format!("{commit}^{{tree}}")])?,
    };
    let catalog_digest = catalog_digest(manifest);
    let catalog_version = catalog_version(manifest);
    let metadata_floor = json!({"scannerVersion": SCANNER_VERSION, "catalogVersion": catalog_version, "catalogDigest": catalog_digest});
    // No protected trust registry on this host is a reported warning, never a failure (#262).
    let floor: Result<(), Vec<String>> = if truthy(&trust) {
        validate_ownership_floor(&trust, &metadata_floor)
    } else {
        Ok(())
    };
    let floor_ok = floor.is_ok();
    let requested_root = ".".to_string();
    let mut diagnostics: Vec<String> = Vec::new();
    let mut identity: Option<String> = None;
    let mut registration: Option<Value> = None;
    if truthy(&trust) {
        let attempt: Res<(String, Option<Value>)> = (|| {
            let origin: Value = match &source {
                Some(s) => s.origin.as_ref().map_or(Value::Null, |o| json!(o)),
                None => json!(plain_git_text(&root, &["remote", "get-url", "origin"])?),
            };
            let canonical = canonical_repository_identity(&origin).map_err(ScanError::new)?;
            match crate::approval::verify_repository_registration(&trust, &json!(canonical), &[]) {
                Ok(_) => {
                    let entry = items(field(&trust, "repositories"))
                        .iter()
                        .find(|e| str_of(field(e, "repositoryIdentity")) == canonical)
                        .cloned();
                    Ok((canonical, entry))
                }
                Err(problems) => {
                    if !pending {
                        return Err(ScanError::new(problems.join("; ")));
                    }
                    diagnostics.extend(problems);
                    Ok((canonical, None))
                }
            }
        })();
        match attempt {
            Ok((canonical, entry)) => {
                identity = Some(canonical);
                // A failed registration keeps no registration, as `registrationCheck.registration` is absent.
                registration = entry;
            }
            Err(error) => {
                if !pending {
                    return Err(error);
                }
                diagnostics.push(format!(
                    "protected registration unavailable during pending enrollment: {}",
                    error.message
                ));
                identity = None;
                registration = None;
            }
        }
    }
    let registered_roots: Vec<String> = registration
        .as_ref()
        .map(|r| {
            items(field(r, "roots"))
                .iter()
                .map(|x| str_of(x).to_string())
                .collect()
        })
        .unwrap_or_default();
    let requested_registered = registration.is_some()
        && registered_roots
            .iter()
            .any(|r| root_contains(r, &requested_root));
    if registration.is_some() && !requested_registered {
        if !pending {
            return Err(ScanError::new(format!(
                "release app root is outside protected ownership roots: {requested_root}"
            )));
        }
        diagnostics.push(format!("requested scan root is outside protected ownership roots during pending enrollment: {requested_root}"));
    }
    let mut scan_roots: Vec<String> = registered_roots.clone();
    if !requested_registered && !scan_roots.contains(&requested_root) {
        scan_roots.push(requested_root.clone());
    }
    if scan_roots.is_empty() {
        scan_roots.push(requested_root.clone());
    }
    struct One {
        root: String,
        scanned: Value,
        prefix: String,
    }
    let mut scans: Vec<One> = Vec::new();
    for registered in &scan_roots {
        let mut absolute = root.clone();
        if registered != "." {
            for part in registered.split('/') {
                absolute.push(part);
            }
        }
        let absolute = paths::resolve(&absolute);
        let scanned = if let (true, Some(source)) = (use_committed, &source) {
            let options = CheckOptions {
                trust: trust.clone(),
                expected_commit: Some(&commit),
                expected_tree: Some(&tree),
            };
            check_committed(ctx, &absolute, manifest, &options, source)?
        } else {
            let options = CheckOptions {
                trust: json!({"trust": trust, "enrolled": field(&state, "enrolled"), "pending": pending}),
                expected_commit: None,
                expected_tree: None,
            };
            check_app(ctx, &absolute, manifest, &options)?
        };
        let meta = field(&scanned, "metadata");
        if let Some(identity) = &identity {
            let observed = canonical_repository_identity(field(meta, "repositoryIdentity"))
                .map_err(ScanError::new)?;
            if &observed != identity {
                return Err(ScanError::new(format!("ownership evidence: scanner observed a different repository identity under {registered}")));
            }
        }
        if use_committed && field(meta, "commit").as_str() != Some(commit.as_str()) {
            return Err(ScanError::new(format!(
                "ownership evidence: scanner observed a different source commit under {registered}"
            )));
        }
        if field(meta, "catalogDigest").as_str() != Some(catalog_digest.as_str()) {
            return Err(ScanError::new(format!(
                "ownership evidence: scanner observed a different catalog under {registered}"
            )));
        }
        if let Some(authority) = key_log_evidence.get("keyAuthority") {
            if field(meta, "keyAuthority") != authority {
                return Err(ScanError::new(format!("ownership evidence: scanner observed a different shared key authority under {registered}")));
            }
        }
        let repository_paths = field(meta, "pathBase").as_str() == Some("repository");
        let prefix = if registered == "." || repository_paths {
            String::new()
        } else {
            format!("{registered}/")
        };
        scans.push(One {
            root: registered.clone(),
            scanned,
            prefix,
        });
    }
    let content_coverage: Vec<Value> = scans
        .iter()
        .flat_map(|s| {
            items(field(field(&s.scanned, "metadata"), "coverage"))
                .iter()
                .map(|e| rebase(e, &s.prefix))
                .collect::<Vec<_>>()
        })
        .collect();
    let mut content_digests: Vec<Value> = scans
        .iter()
        .flat_map(|s| {
            items(field(field(&s.scanned, "metadata"), "contentDigests"))
                .iter()
                .map(|e| {
                    let mut out = e.clone();
                    out["path"] = json!(format!("{}{}", s.prefix, str_of(field(e, "path"))));
                    out
                })
                .collect::<Vec<_>>()
        })
        .collect();
    content_digests
        .sort_by(|a, b| locale_compare(str_of(field(a, "path")), str_of(field(b, "path"))));
    let content_digest = sha256_value(&Value::Array(content_digests.clone()));
    let mut approvals: Vec<Value> = Vec::new();
    for s in &scans {
        if use_committed {
            let digest = if truthy(field(field(&s.scanned, "metadata"), "exceptionDigest")) {
                field(field(&s.scanned, "metadata"), "exceptionDigest").clone()
            } else {
                json!(sha256_value(&json!([])))
            };
            approvals.push(json!({"root": s.root, "digest": digest}));
        } else {
            let mut file = root.clone();
            if s.root != "." {
                for part in s.root.split('/') {
                    file.push(part);
                }
            }
            file.push(".rightkit");
            file.push("ownership-exceptions.json");
            let digest = match std::fs::read(&file) {
                Ok(bytes) => crate::json::sha256_hex(&bytes),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    sha256_value(&json!([]))
                }
                Err(error) => return Err(ScanError::new(error.to_string())),
            };
            approvals.push(json!({"root": s.root, "digest": digest}));
        }
    }
    let approvals_digest = if pending {
        Value::Null
    } else {
        json!(sha256_value(&Value::Array(approvals)))
    };
    let flat = |key: &str| -> Vec<Value> {
        scans
            .iter()
            .flat_map(|s| {
                items(field(&s.scanned, key))
                    .iter()
                    .map(|e| rebase(e, &s.prefix))
                    .collect::<Vec<_>>()
            })
            .collect()
    };
    let problems: Vec<Value> = scans
        .iter()
        .flat_map(|s| items(field(&s.scanned, "problems")).to_vec())
        .collect();
    // Stale plain exception entries and pending-enrollment diagnostics are warnings, never blockers (#262).
    let mut warnings: Vec<Value> = scans
        .iter()
        .flat_map(|s| {
            items(field(&s.scanned, "warnings"))
                .iter()
                .filter_map(|w| w.as_str())
                .filter(|w| w.starts_with("exception #"))
                .map(|w| {
                    if s.root == "." {
                        json!(w)
                    } else {
                        json!(format!("{}: {w}", s.root))
                    }
                })
                .collect::<Vec<_>>()
        })
        .collect();
    warnings.extend(diagnostics.iter().map(|d| json!(d)));
    let rows: Vec<Value> = scans
        .iter()
        .flat_map(|s| {
            items(field(&s.scanned, "rows"))
                .iter()
                .map(|row| {
                    let mut out = row.clone();
                    out["root"] = json!(s.root);
                    out
                })
                .collect::<Vec<_>>()
        })
        .collect();
    let all_ok = scans
        .iter()
        .all(|s| field(&s.scanned, "ok").as_bool() == Some(true));
    let mut result = json!({
        "ok": all_ok,
        "findings": flat("findings"),
        "violations": flat("violations"),
        "problems": problems,
        "rows": rows,
        "covered": flat("covered"),
        "coverage": content_coverage,
        "metadata": {
            "repositoryIdentity": identity, "commit": commit, "tree": tree, "catalogDigest": catalog_digest,
            "contentDigests": content_digests, "coverage": content_coverage,
            "pathBase": "repository", "pathPrefix": "",
            "roots": scans.iter().map(|s| json!({"root": s.root, "metadata": field(&s.scanned, "metadata")})).collect::<Vec<_>>(),
        },
    });
    if !warnings.is_empty() {
        result["warnings"] = Value::Array(warnings);
    }
    let mut evidence = Map::new();
    evidence.insert("schema".into(), json!(1));
    evidence.insert(
        "repositoryIdentity".into(),
        if identity.is_some() {
            json!(canonical_repository_identity(field(
                field(&result, "metadata"),
                "repositoryIdentity"
            ))
            .map_err(ScanError::new)?)
        } else {
            Value::Null
        },
    );
    evidence.insert(
        "source".into(),
        json!(if pending {
            "enrollment-pending-advisory"
        } else if use_committed {
            "committed-git-tree"
        } else {
            "mutable-checkout-test-seam"
        }),
    );
    evidence.insert("commit".into(), json!(commit));
    if !pending {
        evidence.insert("tree".into(), json!(tree));
    }
    evidence.insert("catalogDigest".into(), json!(catalog_digest));
    evidence.insert("catalogVersion".into(), json!(catalog_version));
    evidence.insert("scannerVersion".into(), json!(SCANNER_VERSION));
    evidence.insert("approvalsDigest".into(), approvals_digest);
    evidence.insert("trustEnrolled".into(), json!(!pending));
    evidence.extend(key_log_evidence);
    evidence.insert("floorOk".into(), json!(floor_ok));
    evidence.insert(
        "visibility".into(),
        registration
            .as_ref()
            .map_or(Value::Null, |r| field(r, "visibility").clone()),
    );
    evidence.insert(
        "registrationDigest".into(),
        registration
            .as_ref()
            .map_or(Value::Null, |r| json!(sha256_value(r))),
    );
    evidence.insert("registeredRoots".into(), json!(registered_roots));
    evidence.insert(
        "contentDigest".into(),
        if pending {
            Value::Null
        } else {
            json!(content_digest)
        },
    );
    evidence.insert("checkedAt".into(), json!(iso_from_ms(ctx.clock.now_ms())));
    let ok = all_ok && floor_ok && content_coverage.is_empty();
    let _ = WARNING;
    Ok(Checked {
        ok,
        enrollment_pending: pending,
        evidence: Value::Object(evidence),
        result,
    })
}

fn acquire_ref(source: &crate::committed::CommittedSource) -> crate::committed::CommittedSource {
    crate::committed::CommittedSource {
        repository_root: source.repository_root.clone(),
        origin: source.origin.clone(),
        commit: source.commit.clone(),
        tree: source.tree.clone(),
        entries: source.entries.clone(),
        coverage: source.coverage.clone(),
    }
}
