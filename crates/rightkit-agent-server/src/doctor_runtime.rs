// Runtime helpers + CLI entry point + JSON serialisation. Included from
// `doctor.rs`.

/// Runtime-only subset for the daemon's `/health/diagnostics` (no bind check —
/// the daemon is already serving).
pub fn gather_runtime() -> DiagnosticsReport {
    let mut f = gather();
    f.can_bind_local = true;
    evaluate(&f)
}

pub fn gather_runtime_with_sandbox_tier(tier: Option<String>, source: &str) -> DiagnosticsReport {
    let mut f = gather();
    if let Some(tier) = tier
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        f.sandbox_tier_env = Some(tier);
        f.sandbox_tier_source = Some(source.to_string());
    }
    f.can_bind_local = true;
    evaluate(&f)
}

/// CLI entry point. Returns the process exit code.
///
/// `verbose` adds per-dimension evidence; `clipboard` additionally copies the
/// rendered report to the OS clipboard via the local selection tool.
///
/// The clipboard copy is best-effort: if no platform clipboard binary is on
/// PATH, `eprintln!` notes it but the exit code comes solely from the verdict.
pub fn run(json: bool, verbose: bool, clipboard: bool) -> i32 {
    let report = evaluate(&gather());
    let rendered = if json {
        cli_json(&report, verbose)
    } else {
        report.render_text()
    };
    if clipboard {
        match try_copy_to_clipboard(&rendered) {
            Ok(tool) => eprintln!("[doctor] copied report to clipboard via {tool}"),
            Err(err) => eprintln!(
                "[doctor] --clipboard: clipboard copy failed ({err}); report left on stdout"
            ),
        }
    }
    if json {
        println!("{}", rendered);
    } else {
        print!("{}", rendered);
    }
    report.exit_code()
}

/// Serialise a `DiagnosticsReport` to JSON, additionally adding the
/// `coderight.cli.doctor.v1` schema tag and the security/runtime dimension
/// probes (`policy`, `workspace-trust`, `hooks`, `csprng`, `learning`).
///
/// With `verbose` the dimensions include their `evidence` sub-checks.
pub fn cli_json(report: &DiagnosticsReport, verbose: bool) -> String {
    let mut value = serde_json::to_value(report).unwrap_or_else(|_| serde_json::json!({}));
    let dimensions_value =
        serde_json::to_value(collect_dimensions(verbose)).unwrap_or_else(|_| serde_json::json!({}));
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "schema_version".to_string(),
            serde_json::json!("coderight.cli.doctor.v1"),
        );
        object.insert("dimensions".to_string(), dimensions_value);
        object.insert(
            "extension_execution".to_string(),
            serde_json::to_value(rightkit_agent_plugins::extension_trust_catalog())
                .unwrap_or_else(|_| serde_json::json!([])),
        );
    }
    serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".into())
}

#[cfg(test)]
mod doctor_runtime_tests {
    use super::*;

    fn check_of<'a>(report: &'a DiagnosticsReport, id: &str) -> &'a Check {
        report
            .checks
            .iter()
            .find(|c| c.id == id)
            .unwrap_or_else(|| panic!("missing check {id}"))
    }

    // The runtime subset is served from an already-listening daemon, so the
    // bind probe must never be re-run and must always report success.
    #[test]
    fn gather_runtime_reports_bind_as_satisfied() {
        let report = gather_runtime();
        assert_eq!(check_of(&report, "network").status, Status::Pass);
    }

    #[test]
    fn sandbox_tier_override_is_trimmed_and_attributed() {
        let report = gather_runtime_with_sandbox_tier(Some("  strict  ".into()), "daemon-config");
        let sandbox = check_of(&report, "core_sandbox");
        assert!(
            sandbox.detail.contains("daemon-config"),
            "override source must be attributed: {}",
            sandbox.detail
        );
        assert_eq!(check_of(&report, "network").status, Status::Pass);
    }

    // A blank or whitespace-only override carries no information; it must not
    // displace the probed default or invent a source attribution.
    #[test]
    fn blank_sandbox_tier_override_is_ignored() {
        for blank in ["", "   ", "	"] {
            let report = gather_runtime_with_sandbox_tier(Some(blank.into()), "daemon-config");
            let sandbox = check_of(&report, "core_sandbox");
            assert!(
                !sandbox.detail.contains("daemon-config"),
                "blank tier {blank:?} must not be attributed to a source: {}",
                sandbox.detail
            );
        }
    }

    #[test]
    fn absent_sandbox_tier_override_matches_plain_runtime_gather() {
        let overridden = gather_runtime_with_sandbox_tier(None, "daemon-config");
        let plain = gather_runtime();
        assert_eq!(
            check_of(&overridden, "core_sandbox").detail,
            check_of(&plain, "core_sandbox").detail
        );
    }

    // The CLI envelope is a published contract: consumers key off the schema
    // tag and expect the dimension probes alongside the base report.
    #[test]
    fn cli_json_carries_schema_tag_and_dimensions() {
        let report = evaluate(&gather());
        let value: serde_json::Value = serde_json::from_str(&cli_json(&report, false)).unwrap();
        assert_eq!(value["schema_version"], "coderight.cli.doctor.v1");
        assert!(value["dimensions"].is_object());
        assert!(value["checks"].is_array());
    }

    #[test]
    fn cli_json_reports_honest_extension_execution_classes() {
        let report = evaluate(&gather());
        let value: serde_json::Value = serde_json::from_str(&cli_json(&report, false)).unwrap();
        let classes = value["extension_execution"]
            .as_array()
            .expect("extension_execution array");
        assert_eq!(classes.len(), 4);
        let native = classes
            .iter()
            .find(|entry| entry["class"] == "trusted_native_in_process")
            .expect("native trust class");
        assert_eq!(native["process_authority"], true);
        assert_eq!(native["registration_is_sandbox"], false);
        let isolated = classes
            .iter()
            .find(|entry| entry["class"] == "future_isolated")
            .expect("future isolated class");
        assert_eq!(isolated["available"], false);
        assert_eq!(isolated["registration_is_sandbox"], false);
    }

    // `verbose` only adds evidence; it must not change the envelope shape.
    #[test]
    fn cli_json_verbose_keeps_envelope_shape() {
        let report = evaluate(&gather());
        let plain: serde_json::Value = serde_json::from_str(&cli_json(&report, false)).unwrap();
        let verbose: serde_json::Value = serde_json::from_str(&cli_json(&report, true)).unwrap();
        assert_eq!(verbose["schema_version"], plain["schema_version"]);
        assert_eq!(
            verbose["dimensions"]
                .as_object()
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            plain["dimensions"]
                .as_object()
                .unwrap()
                .keys()
                .collect::<Vec<_>>()
        );
    }
}
