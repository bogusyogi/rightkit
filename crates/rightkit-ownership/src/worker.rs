//! The scan worker, in process. Port of `ownership-scan-worker.mjs`: the inputs probe
//! (`--inputs`) and the protected scan. Both return the worker's JSON line as a value:
//! the result object, or `{error, code}`.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use crate::approval::{canonical_repository_identity, sha256_value};
use crate::error::{Res, ScanError};
use crate::evidence::create_evidence;
use crate::gitio;
use crate::inventory::content_inventory;
use crate::json::{field, items, sha256_hex, str_of, truthy};
use crate::keyauth::read_committed_key_authority;
use crate::paths;
use crate::runner::{ScanRequest, ScanRunner};
use crate::scan::{catalog_digest, catalog_version, ScanContext, SCANNER_VERSION};
use crate::verdict::{codes, ownership_cache_key, ownership_verdict, Decision};

/// Source files whose bytes make up the scanner policy digest.
const POLICY_SOURCES: [(&str, &str); 14] = [
    ("approval.rs", include_str!("approval.rs")),
    ("committed.rs", include_str!("committed.rs")),
    ("deps.rs", include_str!("deps.rs")),
    ("evidence.rs", include_str!("evidence.rs")),
    ("inventory.rs", include_str!("inventory.rs")),
    ("json.rs", include_str!("json.rs")),
    ("keyauth.rs", include_str!("keyauth.rs")),
    ("keylog.rs", include_str!("keylog.rs")),
    ("lockfiles.rs", include_str!("lockfiles.rs")),
    ("paths.rs", include_str!("paths.rs")),
    ("scan.rs", include_str!("scan.rs")),
    ("source.rs", include_str!("source.rs")),
    ("verdict.rs", include_str!("verdict.rs")),
    ("worker.rs", include_str!("worker.rs")),
];

/// Digest of the scanner's own code and version. Any change to the scanner changes
/// every cache key.
pub fn scanner_policy_digest() -> String {
    let mut files: Vec<Value> = POLICY_SOURCES
        .iter()
        .map(|(path, text)| json!({"path": path, "digest": sha256_hex(text.as_bytes())}))
        .collect();
    files.sort_by(|a, b| str_of(field(a, "path")).cmp(str_of(field(b, "path"))));
    sha256_value(
        &json!({"version": SCANNER_VERSION, "files": files, "crate": env!("CARGO_PKG_VERSION")}),
    )
}

/// `<hostname>/<platform>` with Node's platform names.
pub fn host_string() -> String {
    let platform = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    };
    let host = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .filter(|h| !h.is_empty())
        .or_else(|| {
            gitio::run(
                "hostname",
                &[] as &[&str],
                None,
                None,
                Duration::from_secs(5),
                4096,
            )
            .ok()
            .filter(|o| o.success())
            .map(|o| o.text().trim().to_string())
        })
        .unwrap_or_else(|| "localhost".into());
    format!("{host}/{platform}")
}

fn decision_json(decision: &Decision) -> Value {
    let mut out =
        json!({"admit": decision.admit, "code": decision.code, "problems": decision.problems});
    if !decision.warnings.is_empty() {
        out["warnings"] = json!(decision.warnings);
    }
    out
}

fn error_json(error: &ScanError) -> Value {
    json!({"error": error.message, "code": error.reported_code()})
}

/// The scanner a broker links: a catalog plus the host context.
pub struct Scanner {
    pub ctx: ScanContext,
    pub catalog: Value,
}

struct Probe {
    inputs: Value,
    trust: Value,
    complete: bool,
    enrollment_pending: bool,
    metadata: Value,
}

impl Probe {
    fn to_json(&self) -> Value {
        json!({"inputs": self.inputs, "trust": self.trust, "complete": self.complete, "enrollmentPending": self.enrollment_pending, "metadata": self.metadata})
    }
}

impl Scanner {
    pub fn new(ctx: ScanContext, catalog: Value) -> Self {
        Self { ctx, catalog }
    }

    /// A scanner over the catalog embedded in this crate ([`crate::catalog`]).
    pub fn with_embedded_catalog(ctx: ScanContext) -> Self {
        Self::new(ctx, crate::catalog_value().clone())
    }

    fn probe(&self, app_root: &Path, counter: &std::sync::atomic::AtomicUsize) -> Res<Probe> {
        let root = paths::resolve(app_root);
        let state = (self.ctx.trust_loader)()?;
        let absent = field(&state, "pending").as_bool() == Some(true)
            && field(&state, "enrolled").as_bool() == Some(false)
            && field(&state, "trust").is_null()
            && field(field(&state, "protection"), "reason").as_str()
                == Some("external trust file is absent");
        if field(field(&state, "protection"), "ok").as_bool() != Some(true) && !absent {
            return Err(ScanError::invalid_inputs(
                "protected ownership trust location was not verified",
            ));
        }
        let mut trust = field(&state, "trust").clone();
        let mut key_log_digest = Value::Null;
        if truthy(field(&trust, "keyAuthority")) {
            let resolved = read_committed_key_authority(&trust, &self.ctx.authority)
                .map_err(|p| ScanError::invalid_inputs(p.join("; ")))?;
            trust = resolved.resolved_trust.clone();
            key_log_digest = json!(resolved.digest);
        }
        let origin = gitio::plain_git(
            &root,
            &["remote", "get-url", "origin"],
            Duration::from_millis(2500),
        );
        let repository_identity: Option<String> = origin
            .ok()
            .filter(|o| o.success())
            .and_then(|o| canonical_repository_identity(&json!(o.text().trim())).ok());
        let call = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        if let Some(hook) = &self.ctx.before_inventory {
            hook(&root, call);
        }
        let source = content_inventory(&root);
        let roots: Vec<String> = items(field(&trust, "repositories"))
            .iter()
            .find(|e| {
                repository_identity
                    .as_deref()
                    .is_some_and(|i| str_of(field(e, "repositoryIdentity")) == i)
            })
            .map(|e| {
                items(field(e, "roots"))
                    .iter()
                    .map(|r| str_of(r).to_string())
                    .collect()
            })
            .unwrap_or_else(|| vec![".".to_string()]);
        let mut approvals = Vec::new();
        for registered in &roots {
            let mut file = root.clone();
            if registered != "." {
                for part in registered.split('/') {
                    file.push(part);
                }
            }
            file.push(".rightkit");
            file.push("ownership-exceptions.json");
            let digest = match std::fs::read(&file) {
                Ok(bytes) => sha256_hex(&bytes),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    sha256_value(&json!({"absent": true}))
                }
                Err(error) => return Err(ScanError::new(error.to_string())),
            };
            approvals.push(json!({"root": registered, "digest": digest}));
        }
        let catalog_digest = catalog_digest(&self.catalog);
        let floor = |key: &str| {
            if truthy(field(&trust, key)) {
                field(&trust, key).clone()
            } else {
                Value::Null
            }
        };
        let inputs = json!({
            "contentDigest": source.digest,
            "catalogDigest": catalog_digest,
            "trustDigest": sha256_value(&json!({"trust": trust, "repositoryIdentity": repository_identity, "enrolled": field(&state, "enrolled"), "pending": field(&state, "pending"), "keyLogDigest": key_log_digest})),
            "approvalsDigest": sha256_value(&Value::Array(approvals)),
            "scannerPolicyDigest": self.ctx.policy_digest,
            "protectedFloors": {"minimumScannerVersion": floor("minimumScannerVersion"), "minimumCatalogVersion": floor("minimumCatalogVersion"), "minimumCatalogDigest": floor("minimumCatalogDigest")},
        });
        ownership_cache_key(&inputs).map_err(|e| ScanError::with_code(e.code, e.message))?;
        let enrollment_pending = field(&trust, "enrollmentComplete").as_bool() == Some(false)
            || field(&state, "enrolled").as_bool() != Some(true)
            || field(&state, "pending").as_bool() == Some(true);
        let coverage = crate::types::coverage_json(&source.coverage);
        let metadata = json!({
            "repositoryIdentity": repository_identity, "scannerVersion": SCANNER_VERSION, "catalogVersion": catalog_version(&self.catalog),
            "catalogDigest": catalog_digest, "coverage": coverage, "roots": roots.iter().map(|r| json!({"root": r})).collect::<Vec<_>>(),
        });
        Ok(Probe {
            inputs,
            trust,
            complete: source.complete,
            enrollment_pending,
            metadata,
        })
    }

    /// `--inputs`: authenticate and hash inputs only.
    pub fn inputs(&self, app_root: &Path) -> Value {
        let counter = std::sync::atomic::AtomicUsize::new(0);
        match self.probe(app_root, &counter) {
            Ok(probe) => probe.to_json(),
            Err(error) => error_json(&error),
        }
    }

    /// The protected scan: probe, preflight, evidence, re-probe.
    pub fn scan(&self, app_root: &Path) -> Value {
        match self.scan_protected(app_root) {
            Ok(value) => value,
            Err(error) => error_json(&error),
        }
    }

    fn scan_protected(&self, app_root: &Path) -> Res<Value> {
        let root = paths::resolve(app_root);
        let counter = std::sync::atomic::AtomicUsize::new(0);
        let before = self.probe(&root, &counter)?;
        let preliminary = json!({"ok": true, "enrollmentPending": before.enrollment_pending, "violations": [], "problems": [], "coverage": field(&before.metadata, "coverage"), "metadata": before.metadata});
        let preflight = ownership_verdict(&preliminary, &before.trust);
        let with_before = |report: Value, verdict: &Decision| {
            let mut out = before.to_json();
            out["report"] = report;
            out["verdict"] = decision_json(verdict);
            out
        };
        if !preflight.admit {
            let mut report = preliminary.clone();
            report["ok"] = json!(false);
            return Ok(with_before(report, &preflight));
        }
        let checked = match create_evidence(&self.ctx, &root, &self.catalog) {
            Ok(checked) => checked,
            Err(error) => {
                if !before.enrollment_pending {
                    return Err(error);
                }
                let mut report = preliminary.clone();
                report["ok"] = json!(false);
                report["problems"] = json!([error.message]);
                let verdict = ownership_verdict(&report, &before.trust);
                return Ok(with_before(report, &verdict));
            }
        };
        let mut report = checked.result.clone();
        report["ok"] = json!(checked.ok);
        report["enrollmentPending"] = json!(checked.enrollment_pending);
        let mut metadata = field(&report, "metadata").clone();
        metadata["scannerVersion"] = field(&checked.evidence, "scannerVersion").clone();
        metadata["catalogVersion"] = field(&checked.evidence, "catalogVersion").clone();
        report["metadata"] = metadata;
        let verdict = ownership_verdict(&report, &before.trust);
        let after = self.probe(&root, &counter)?;
        let before_key = ownership_cache_key(&before.inputs)
            .map_err(|e| ScanError::with_code(e.code, e.message))?;
        let after_key = ownership_cache_key(&after.inputs)
            .map_err(|e| ScanError::with_code(e.code, e.message))?;
        if before_key != after_key
            || before.complete != after.complete
            || sha256_value(field(&before.metadata, "coverage"))
                != sha256_value(field(&after.metadata, "coverage"))
        {
            return Err(ScanError::with_code(
                codes::INPUTS_CHANGED,
                "ownership inputs changed during protected evidence scan",
            ));
        }
        Ok(
            json!({"inputs": after.inputs, "trust": after.trust, "report": report, "verdict": decision_json(&verdict), "evidence": checked.evidence}),
        )
    }
}

/// Runs the in-process scanner as a [`ScanRunner`].
pub struct NativeScanRunner(pub Arc<Scanner>);

impl ScanRunner for NativeScanRunner {
    fn run(&self, request: &ScanRequest<'_>) -> Value {
        let root = Path::new(request.scan_root);
        if request.inputs_only {
            self.0.inputs(root)
        } else {
            self.0.scan(root)
        }
    }
    /// The in-process scanner is part of the binary: always staged.
    fn staged(&self) -> bool {
        true
    }
}
