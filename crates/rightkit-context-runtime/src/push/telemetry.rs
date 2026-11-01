//! Content-free, unit-bearing Push observations. This optional sink is not
//! Cortex knowledge and never invents provider billing or task outcomes.
use serde::Serialize;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex, OnceLock,
};
static WRITER: OnceLock<Mutex<()>> = OnceLock::new();
static OBSERVED: AtomicU64 = AtomicU64::new(0);
static LOST: AtomicU64 = AtomicU64::new(0);
const MAX_LOG_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PushObservation<'a> {
    pub schema_version: u8,
    pub axis: &'static str,
    pub verb: &'a str,
    pub before: usize,
    pub after: usize,
    pub measurement_unit: &'static str,
    pub estimator_basis: &'static str,
    pub observed_at: u64,
    pub task_outcome: &'static str,
    pub provider_billed_tokens: Option<u64>,
    pub opportunity_digest: Option<String>,
    pub metadata: Option<&'a str>,
}
fn unit(verb: &str) -> (&'static str, &'static str) {
    match verb {
        "packet-measure" => ("tokens", "o200k_base/1:serialized_packet"),
        "skel" | "compress" => ("unicode_scalars", "unicode_scalar_count_v1"),
        "prep:compress" => ("lexical_units", "whitespace_v1"),
        "runc"
        | "prepare"
        | "restore"
        | "prep:skel"
        | "prep:skel-fallback-copy"
        | "prep:outline" => ("bytes", "utf8_or_original_bytes_v1"),
        _ => ("unknown", "not_observed"),
    }
}
/// Counts are process-local observations, not claims about an unobserved host.
pub fn status() -> serde_json::Value {
    serde_json::json!({"schemaVersion":2,"coverage":"process_local",
        "sinkConfigured":crate::qa_env::push_telemetry_path().is_some(),
        "observed":OBSERVED.load(Ordering::Relaxed),"sinkMisses":LOST.load(Ordering::Relaxed),
        "taskOutcome":"unknown","providerBilledTokens":null})
}
pub fn record(
    verb: &str,
    before: usize,
    after: usize,
    metadata: Option<&str>,
    opportunity: Option<&str>,
) {
    OBSERVED.fetch_add(1, Ordering::Relaxed);
    let Some(path) = crate::qa_env::push_telemetry_path() else {
        return;
    };
    if verb.is_empty()
        || verb.len() > 32
        || !verb
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b":-_".contains(&c))
    {
        LOST.fetch_add(1, Ordering::Relaxed);
        return;
    }
    let metadata = metadata.filter(|m| {
        m.len() <= 2048
            && m.bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"=;:._/-".contains(&c))
    });
    let (measurement_unit, estimator_basis) = unit(verb);
    let observation = PushObservation {
        schema_version: 2,
        axis: super::AXIS,
        verb,
        before,
        after,
        measurement_unit,
        estimator_basis,
        observed_at: super::recovery::now_ms(),
        task_outcome: "unknown",
        provider_billed_tokens: None,
        opportunity_digest: opportunity.map(|o| super::recovery::digest(o.as_bytes())),
        metadata,
    };
    let Ok(line) = serde_json::to_string(&observation) else {
        return;
    };
    let Ok(_guard) = WRITER.get_or_init(|| Mutex::new(())).try_lock() else {
        LOST.fetch_add(1, Ordering::Relaxed);
        return;
    };
    let result = (|| -> std::io::Result<()> {
        let path = std::path::PathBuf::from(path);
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(std::io::ErrorKind::PermissionDenied.into());
        }
        if std::fs::metadata(&path).is_ok_and(|m| m.len() + line.len() as u64 > MAX_LOG_BYTES) {
            let backup = path.with_extension("jsonl.previous");
            if backup.exists() {
                std::fs::remove_file(&backup)?;
            }
            std::fs::rename(&path, backup)?;
        }
        rightkit_fs::append_record(
            &path,
            format!("{line}\n").as_bytes(),
            rightkit_fs::AppendOptions::private().durable(false),
        )
        .map(|_| ())
        .map_err(std::io::Error::other)
    })();
    if result.is_err() {
        LOST.fetch_add(1, Ordering::Relaxed);
    }
}
