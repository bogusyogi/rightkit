// rightkit-ownership 0.4.1 consumed by exact registry pin, outside the workspace.
//
// Proves the public scanner surface without a git checkout or protected trust file: the embedded catalog,
// ownership_verdict and ownership_cache_key contracts, is_blocking_finding, canonical_repository_identity,
// scanner_policy_digest, and Scanner::with_embedded_catalog scanning a temp directory under an injected
// ScanContext (manual clock, unavailable checkpoint authority, in-memory trust loader).
// Does not prove: evidence over a committed checkout, approval signatures, admission cache, runners.
// State: one temp directory with one small file, removed at the end. No network, no real user data.
use std::path::PathBuf;
use std::sync::Arc;

use rightkit_ownership::keyauth::AuthorityEnv;
use rightkit_ownership::{
    canonical_repository_identity, catalog, catalog_value, codes, is_blocking_finding,
    ownership_cache_key, ownership_verdict, scanner_policy_digest, ManualClock, ScanContext,
    Scanner, SCANNER_VERSION,
};
use serde_json::json;

fn is_hex64(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit())
}

fn context(trust: serde_json::Value) -> ScanContext {
    ScanContext {
        clock: Arc::new(ManualClock::new(1_800_000_000_000)),
        host: "consumer/test".into(),
        authority: AuthorityEnv::unavailable(),
        trust_loader: Arc::new(move || Ok(trust.clone())),
        policy_digest: scanner_policy_digest(),
        before_inventory: None,
    }
}

fn main() -> Result<(), String> {
    // Embedded catalog: text and parsed value agree, and the scanner version is stated.
    assert!(!catalog().is_empty());
    assert!(catalog_value().is_object());
    assert_eq!(
        catalog_value()["version"]
            .as_str()
            .map(|v| v.split('.').count()),
        Some(3)
    );
    assert_eq!(SCANNER_VERSION.split('.').count(), 3);
    assert!(is_hex64(&scanner_policy_digest()));

    // Verdict contract: a malformed report is refused, fail closed.
    let refused = ownership_verdict(&json!({"unexpected": true}), &json!({}));
    assert!(!refused.admit);
    assert_eq!(refused.code, codes::MALFORMED_REPORT);
    assert!(!refused.problems.is_empty());

    // Findings: advise is not blocking; block and banned are.
    assert!(!is_blocking_finding(&json!({"tier": "advise"})));
    assert!(is_blocking_finding(&json!({"tier": "block"})));
    assert!(is_blocking_finding(
        &json!({"tier": "advise", "policy": "banned"})
    ));

    // Cache key: well-formed digests and floors are accepted; an unknown field is refused, typed.
    let digest = "a".repeat(64);
    let inputs = json!({
        "contentDigest": digest, "catalogDigest": digest, "trustDigest": digest,
        "approvalsDigest": digest, "scannerPolicyDigest": digest,
        "protectedFloors": {"minimumScannerVersion": null, "minimumCatalogVersion": null, "minimumCatalogDigest": null},
    });
    let key = ownership_cache_key(&inputs).map_err(|e| e.message)?;
    assert!(!key.is_empty());
    assert_eq!(
        ownership_cache_key(&inputs).map_err(|e| e.message)?,
        key,
        "the key is deterministic"
    );
    let mut extra = inputs.clone();
    extra["surprise"] = json!(1);
    let error = ownership_cache_key(&extra).expect_err("unknown field must be refused");
    assert_eq!(error.code, codes::INVALID_INPUTS);

    // Repository identity: canonical HTTPS form, scp remote accepted, absent and relative refused.
    assert_eq!(
        canonical_repository_identity(&json!("https://github.com/Orthic-Labs/rightkit"))
            .map_err(|e| e)?,
        "https://github.com/orthic-labs/rightkit"
    );
    assert!(
        canonical_repository_identity(&json!("git@github.com:Orthic-Labs/rightkit.git")).is_ok()
    );
    assert!(canonical_repository_identity(&json!("")).is_err());
    assert!(canonical_repository_identity(&json!("relative/path")).is_err());

    // Scanner over a temp directory.
    let dir: PathBuf = std::env::temp_dir().join(format!(
        "rightkit-ownership-consumer-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let outcome = scan(&dir);
    let _ = std::fs::remove_dir_all(&dir);
    outcome?;

    println!("OK rightkit-ownership 0.4.1 (registry consumer)");
    Ok(())
}

fn scan(dir: &std::path::Path) -> Result<(), String> {
    std::fs::write(dir.join("main.rs"), "fn main() {}\n").map_err(|e| e.to_string())?;

    // A trust location that was not verified fails closed with the typed error JSON.
    let unverified = Scanner::with_embedded_catalog(context(json!({})));
    for result in [unverified.inputs(dir), unverified.scan(dir)] {
        assert_eq!(result["code"], codes::INVALID_INPUTS, "{result}");
        assert!(result["error"]
            .as_str()
            .is_some_and(|text| text.contains("trust location")));
    }

    // An absent external trust file is a pending enrolment: inputs are probed and hashed.
    let pending = Scanner::with_embedded_catalog(context(json!({
        "pending": true, "enrolled": false, "trust": null,
        "protection": {"ok": false, "reason": "external trust file is absent"},
    })));
    let probe = pending.inputs(dir);
    assert!(probe.get("error").is_none(), "{probe}");
    assert_eq!(probe["enrollmentPending"], true);
    for field in [
        "contentDigest",
        "catalogDigest",
        "trustDigest",
        "approvalsDigest",
        "scannerPolicyDigest",
    ] {
        assert!(
            probe["inputs"][field].as_str().is_some_and(is_hex64),
            "{field}: {probe}"
        );
    }
    assert_eq!(
        probe["inputs"]["scannerPolicyDigest"],
        scanner_policy_digest()
    );
    assert_eq!(probe["metadata"]["scannerVersion"], SCANNER_VERSION);
    ownership_cache_key(&probe["inputs"]).map_err(|e| e.message)?;
    Ok(())
}
