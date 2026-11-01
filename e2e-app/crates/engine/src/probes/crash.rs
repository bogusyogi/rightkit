//! rightkit-crash (the engine's feature set: no support bundle): records real crash reports through the crate's
//! non-panicking entry point (`record_report`, the same path the panic hook and hang watchdog take), reads them back
//! from disk and proves they are redacted (pattern scrub plus an app-declared secret), owner-only, schema-valid,
//! pruned by retention, and listed as pending. A supervised helper's death is recorded too.
//!
//! The probe does not install a panic hook: the engine owns the process hook.
use rightkit_crash::{
    list_pending, record_helper_exit, record_report, CrashConfig, CrashReport, Retention,
    CRASH_SCHEMA_VERSION,
};
use serde_json::{json, Value};

const DICTATED: &str = "send the quarterly numbers to the board";
const EMAIL: &str = "ada.lovelace@example.org";
const USER_PATH: &str = "/Users/adalovelace/Documents/private/notes.txt";
const CREDENTIAL: &str = "sk-live-4f9a8b7c6d5e4f3a2b1c";
const APP_SECRET: &str = "e2e-app-held-secret-9981";

fn ensure(cond: bool, what: &str) -> Result<(), String> {
    if cond {
        Ok(())
    } else {
        Err(format!("rightkit-crash: {what}"))
    }
}

pub fn probe(ctx: &super::Ctx) -> Result<Value, String> {
    let dir = ctx.dir("crash")?.join("reports");
    std::fs::create_dir_all(&dir).map_err(|e| format!("rightkit-crash: create crash dir: {e}"))?;
    let mut config =
        CrashConfig::new("RkE2E", "0.1.0", &dir).redact_values(vec![APP_SECRET.to_string()]);
    config.build = Some("e2e-build".into());
    config.retention = Retention {
        max_reports: 3,
        ..Retention::default()
    };

    ensure(
        rightkit_crash::recording_enabled(),
        "crash recording is switched off in this process",
    )?;
    let message = format!(
        "slice failed on `{DICTATED}` for {EMAIL} reading {USER_PATH} with {CREDENTIAL} and {APP_SECRET} from 10.1.2.3"
    );
    let path = record_report(
        &config,
        "panic",
        &message,
        Some("src/probe.rs:1:1"),
        "export-worker",
        "frame 0\nframe 1",
    )
    .ok_or("rightkit-crash: record_report wrote nothing")?;
    ensure(
        path.is_file() && path.starts_with(&dir),
        &format!("report not under the crash dir: {}", path.display()),
    )?;

    let bytes = std::fs::read(&path).map_err(|e| format!("rightkit-crash: read report: {e}"))?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    for forbidden in [
        DICTATED,
        EMAIL,
        "adalovelace",
        CREDENTIAL,
        APP_SECRET,
        "10.1.2.3",
    ] {
        ensure(
            !text.contains(forbidden),
            &format!("the report leaked {forbidden:?}: {text}"),
        )?;
    }
    let report: CrashReport = serde_json::from_slice(&bytes)
        .map_err(|e| format!("rightkit-crash: report is not valid CrashReport JSON: {e}"))?;
    ensure(
        report.schema_version == CRASH_SCHEMA_VERSION && report.kind == "panic",
        "schema_version or kind",
    )?;
    ensure(
        report.app_name == "RkE2E"
            && report.app_version == "0.1.0"
            && report.build.as_deref() == Some("e2e-build"),
        "app identity fields",
    )?;
    ensure(
        report.pid == std::process::id() && report.thread == "export-worker",
        "pid or thread",
    )?;
    ensure(
        report.os == std::env::consts::OS && report.arch == std::env::consts::ARCH,
        "os or arch",
    )?;
    ensure(
        report.message.contains("slice failed"),
        "the non-sensitive part of the message was lost",
    )?;
    ensure(report.backtrace.contains("frame 1"), "backtrace was lost")?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path)
            .map_err(|e| format!("rightkit-crash: stat: {e}"))?
            .permissions()
            .mode()
            & 0o777;
        ensure(
            mode & 0o077 == 0,
            &format!("report mode is {mode:o}, not owner-only"),
        )?;
    }

    // A supervisor records a helper's death with a fixed kind.
    let helper = record_helper_exit(&config, "e2e-sidecar", "signal 9", 2)
        .ok_or("rightkit-crash: record_helper_exit wrote nothing")?;
    let helper_report: CrashReport = serde_json::from_slice(
        &std::fs::read(&helper).map_err(|e| format!("rightkit-crash: read helper report: {e}"))?,
    )
    .map_err(|e| format!("rightkit-crash: helper report JSON: {e}"))?;
    ensure(
        helper_report.kind == "sidecar_crash"
            && helper_report.process_kind.as_deref() == Some("helper"),
        "helper report kind",
    )?;
    ensure(
        helper_report.message.contains("e2e-sidecar")
            && helper_report.message.contains("restarts so far: 2"),
        "helper report message",
    )?;

    // Pending list: both reports; retention keeps only the newest max_reports after more writes.
    let pending = list_pending(&dir);
    ensure(
        pending.len() == 2,
        &format!("expected 2 pending reports, found {}", pending.len()),
    )?;
    for i in 0..4 {
        record_report(&config, "panic", &format!("extra crash {i}"), None, "t", "")
            .ok_or("rightkit-crash: extra record_report wrote nothing")?;
    }
    let kept = list_pending(&dir).len();
    ensure(
        kept == 3,
        &format!("retention max_reports=3 left {kept} reports"),
    )?;

    // Recording can be switched off at runtime (consent withdrawn) and then writes nothing.
    rightkit_crash::set_recording_enabled(false);
    let off = record_report(&config, "panic", "must not be written", None, "t", "");
    rightkit_crash::set_recording_enabled(true);
    ensure(
        off.is_none() && list_pending(&dir).len() == 3,
        "a report was written while recording was off",
    )?;

    Ok(
        json!({ "report": path.file_name().map(|n| n.to_string_lossy().into_owned()), "bytes": bytes.len(), "pending_after_retention": kept }),
    )
}
