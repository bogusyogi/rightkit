// Sibling test file for `doctor_dimensions`. Declared via `#[path]` in
// `doctor.rs` because the parent module already has `mod tests`.

use super::*;

fn all_pass_facts_for_dim() -> Facts {
    Facts {
        provider: ProviderReadiness::OpenRouterFallback { key: true },
        coderight_dir_writable: true,
        evolution_db_ok: true,
        can_bind_local: true,
        workspace_writable: true,
        git_available: true,
        daemon_port_free: true,
        skills_dir_present: true,
        mcp_server_count: Some(1),
        sandbox_tier_env: None,
        sandbox_tier_source: None,
        sandbox_os_supported: true,
    }
}

#[test]
fn json_output_has_every_expected_dimension_key() {
    let report = evaluate(&all_pass_facts_for_dim());
    let json_text = cli_json(&report, false);
    let v: serde_json::Value = serde_json::from_str(&json_text).expect("json parseable");
    let dims = v
        .get("dimensions")
        .and_then(|d| d.as_object())
        .expect("dimensions object present");
    for key in ["policy", "workspace-trust", "hooks", "csprng", "learning"] {
        assert!(
            dims.contains_key(key),
            "missing dimension: {key} in {dims:?}"
        );
        let dim = &dims[key];
        let status = dim.get("status").and_then(|s| s.as_str()).unwrap_or("");
        assert!(
            ["pass", "warn", "block", "unknown"].contains(&status),
            "dimension {key} has unsupported status {status:?}"
        );
        assert!(
            dim.get("detail").and_then(|d| d.as_str()).is_some(),
            "dimension {key} has no string detail"
        );
    }
    assert_eq!(v["schema_version"], "coderight.cli.doctor.v1");
}

#[test]
fn verbose_adds_evidence_field_to_dimensions() {
    let report = evaluate(&all_pass_facts_for_dim());
    let json_text = cli_json(&report, true);
    let v: serde_json::Value = serde_json::from_str(&json_text).expect("json parseable");
    let dims = v["dimensions"].as_object().expect("dimensions object");
    // At least one dimension will have an evidence array in verbose mode;
    // csprng always produces one (either pass with 32-byte sample, or block
    // with all_zero / getrandom error).
    let any_with_evidence = dims
        .values()
        .any(|d| d.get("evidence").and_then(|e| e.as_array()).is_some());
    assert!(
        any_with_evidence,
        "verbose=true produced no dimension with evidence: {dims:?}"
    );
    // And --json alone should never include evidence arrays (the fields are
    // skipped via `skip_serializing_if = "Option::is_none"`).
    let json_default = cli_json(&report, false);
    let v_default: serde_json::Value = serde_json::from_str(&json_default).unwrap();
    let dims_default = v_default["dimensions"].as_object().unwrap();
    assert!(
        dims_default.values().all(|d| d.get("evidence").is_none()),
        "verbose=false leaked evidence: {dims_default:?}"
    );
}

#[test]
fn clipboard_candidate_list_includes_target_os_binary() {
    let bins = candidate_clipboard_bins();
    assert!(
        !bins.is_empty(),
        "clipboard candidate list must not be empty"
    );
    let first_bin = bins[0].0;
    if cfg!(target_os = "macos") {
        assert_eq!(first_bin, "pbcopy");
    } else if cfg!(target_os = "windows") {
        assert_eq!(first_bin, "clip.exe");
    } else {
        assert!(
            ["wl-copy", "xclip", "xsel"].contains(&first_bin),
            "unexpected first linux clipboard candidate: {first_bin}"
        );
    }
}

#[test]
fn probe_csprng_status_is_pass_with_evidence() {
    let dim = probe_csprng(false);
    assert!(
        ["pass", "warn", "block", "unknown"].contains(&dim.status),
        "status={:?}",
        dim.status
    );
    assert!(!dim.detail.is_empty());

    let dim_v = probe_csprng(true);
    let json = serde_json::to_value(&dim_v).unwrap();
    let ev = json.get("evidence").and_then(|e| e.as_array());
    assert!(ev.is_some(), "verbose csprng missing evidence: {json}");
}

#[test]
fn probe_workspace_trust_status_is_known_token() {
    let dim = probe_workspace_trust(false);
    assert!(
        ["pass", "warn", "block", "unknown"].contains(&dim.status),
        "status={:?}",
        dim.status
    );
    assert!(!dim.detail.is_empty());
}

#[test]
fn collect_dimensions_returns_stable_alphabetical_keys() {
    let map = collect_dimensions(false);
    let mut keys: Vec<&str> = map.keys().copied().collect();
    keys.sort();
    assert_eq!(
        keys,
        vec!["csprng", "hooks", "learning", "policy", "workspace-trust"]
    );
}

#[test]
fn directory_writable_probe_does_not_create_missing_target() {
    let temp = tempfile::tempdir().unwrap();
    let missing = temp.path().join(".coderight");

    let _ = directory_is_writable(&missing);
    assert!(!missing.exists(), "doctor probe must not create target");
}

#[test]
fn evolution_db_probe_is_read_only_for_missing_database() {
    let temp = tempfile::tempdir().unwrap();
    let missing = temp.path().join("evolution.db");

    assert!(!evolution_db_is_readable(&missing));
    assert!(!missing.exists(), "doctor probe must not create database");
}
