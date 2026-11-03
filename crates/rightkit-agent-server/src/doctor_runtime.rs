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
        report.render_text("RightKit")
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
