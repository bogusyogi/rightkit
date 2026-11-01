//! Versioned, read-only health checks for the currently installed schema.
use rightkit_db::rusqlite::Connection;
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorReportV0 {
    pub schema_version: &'static str,
    pub system: &'static str,
    pub status: &'static str,
    pub checks: Vec<DoctorCheckV0>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorCheckV0 {
    pub code: &'static str,
    pub severity: &'static str,
    pub count: usize,
    pub sample_ids: Vec<String>,
    pub repair: &'static str,
}

pub fn run(path: impl AsRef<Path>) -> Result<DoctorReportV0, String> {
    run_with_policy(path, &[], &[])
}

/// Runs the fixed v0 checks while omitting only explicitly named, stable finding codes.
/// Suppressions are caller-owned policy; unknown names have no effect.
pub fn run_with_suppressions(
    path: impl AsRef<Path>,
    suppressed_codes: &[&str],
) -> Result<DoctorReportV0, String> {
    run_with_policy(path, suppressed_codes, &[])
}

/// Runs doctor checks while explicitly allowing known external wikilink targets.
/// External references are exact slugs supplied by the caller; unresolved targets
/// remain findings by default.
pub fn run_with_policy(
    path: impl AsRef<Path>,
    suppressed_codes: &[&str],
    external_refs: &[&str],
) -> Result<DoctorReportV0, String> {
    let connection = rightkit_db::open_read_only(path).map_err(|error| error.to_string())?;
    let embedding_check = check_embeddings(&connection)?;
    let mut checks = vec![
        check(
            &connection,
            "MRD-EMBED-MODEL-DRIFT",
            "warning",
            "SELECT id FROM memories WHERE embed_model IS NULL OR trim(embed_model) = ''",
            "reindex after selecting the intended embed model",
        )?,
        embedding_check,
        // A corpus embedded by the hash embedder matches lexically only, so a
        // clean doctor run over it is not evidence of semantic health. The
        // existing model-drift check only fires on a NULL or empty model, so
        // a uniform "hash-256" passed silently — which is exactly the state a
        // corpus written while the model was missing or not loaded leaves behind.
        check(
            &connection,
            "MRD-EMBED-DEGRADED",
            "info",
            "SELECT id FROM memories WHERE embed_model LIKE 'hash-%'",
            "no model is available yet; recall over these rows is lexical only until one ships and reindex runs",
        )?,
        check_model(),
        check_scope_anomalies(&connection)?,
        check(
            &connection,
            "MRD-EFFECTIVENESS-UNVERIFIED",
            "info",
            "SELECT id FROM memories WHERE inject_count > 0 AND access_count = 0",
            "collect verified access feedback before curation decisions",
        )?,
    ];
    checks.push(check_dangling_wikilinks(&connection, external_refs)?);
    {
        checks.extend([
            check(&connection, "MRD-LIFECYCLE-DANGLING", "critical", "SELECT id FROM memories WHERE superseded_by IS NOT NULL AND NOT EXISTS (SELECT 1 FROM memories successor WHERE successor.id=memories.superseded_by)", "restore the verified successor or clear invalid supersession")?,
            check(&connection, "MRD-LIFECYCLE-CYCLE", "critical", "WITH RECURSIVE chain(origin, next, depth) AS (SELECT id, superseded_by, 0 FROM memories WHERE superseded_by IS NOT NULL UNION ALL SELECT chain.origin, memories.superseded_by, chain.depth + 1 FROM chain JOIN memories ON memories.id=chain.next WHERE chain.next IS NOT NULL AND chain.depth < 100) SELECT DISTINCT origin FROM chain WHERE next=origin", "break the cycle with a typed supersession adjudication")?,
            check(&connection, "MRD-LIFECYCLE-WINDOW", "warning", "SELECT id FROM memories WHERE effective_from_ms IS NOT NULL AND effective_until_ms IS NOT NULL AND effective_until_ms <= effective_from_ms", "repair half-open effective interval")?,
            check(&connection, "MRD-PROTECTED-EXPIRED", "warning", "SELECT id FROM memories WHERE priority_class='protected' AND ((effective_until_ms IS NOT NULL AND effective_until_ms <= (unixepoch('now') * 1000)) OR (expires_at_ms IS NOT NULL AND expires_at_ms <= (unixepoch('now') * 1000)))", "retain if required, but reverify or retire expired protected row")?,
            check(&connection, "MRD-LIFECYCLE-EXPIRED", "warning", "SELECT id FROM memories WHERE lifecycle_state='active' AND record_type!='checkpoint' AND ((effective_until_ms IS NOT NULL AND effective_until_ms <= (unixepoch('now') * 1000)) OR (expires_at_ms IS NOT NULL AND expires_at_ms <= (unixepoch('now') * 1000)))", "retire or reverify the expired active row")?,
            check(&connection, "MRD-SUPERSEDED-INJECTED", "critical", "SELECT DISTINCT memories.id FROM memories JOIN memory_event_log ON memory_event_log.memory_id=memories.id WHERE memories.lifecycle_state='superseded' AND memories.effective_until_ms IS NOT NULL AND memory_event_log.event_kind='injected' AND (unixepoch(memory_event_log.ts) * 1000) >= memories.effective_until_ms", "repair admission so superseded rows cannot be injected")?,
            check(&connection, "MRD-CHECKPOINT-EXPIRED", "critical", "SELECT id FROM memories WHERE record_type='checkpoint' AND lifecycle_state='active' AND expires_at_ms IS NOT NULL AND expires_at_ms <= (unixepoch('now') * 1000)", "retire expired checkpoints from active delivery")?,
        ]);
    }
    checks.retain(|check| !suppressed_codes.contains(&check.code));
    let status = if checks
        .iter()
        .any(|check| check.severity == "critical" && check.count > 0)
    {
        "critical"
    } else if checks
        .iter()
        .any(|check| check.severity == "warning" && check.count > 0)
    {
        "warning"
    } else {
        "ok"
    };
    Ok(DoctorReportV0 {
        schema_version: "CortexDoctorV0",
        system: "Membrane",
        status,
        checks,
    })
}

/// The `model: ...` line status and doctor print.
pub fn model_status_line() -> String {
    crate::store::MODEL_READINESS.line()
}

/// Temporary or by-design states (#433) are informational: retrieval is lexical and
/// graph-only until the model is ready. A fault state is a warning that names its cause. The
/// line itself is the sample.
fn check_model() -> DoctorCheckV0 {
    let readiness = crate::store::MODEL_READINESS;
    let severity = if readiness.is_temporary()
        || readiness == crate::store::ModelReadiness::NotShipped
        || readiness == crate::store::ModelReadiness::Ready
    {
        "info"
    } else {
        "warn"
    };
    DoctorCheckV0 {
        code: "MRD-MODEL",
        severity,
        count: 1,
        sample_ids: vec![model_status_line()],
        repair: readiness.reason(),
    }
}

fn check_dangling_wikilinks(
    connection: &Connection,
    external_refs: &[&str],
) -> Result<DoctorCheckV0, String> {
    let mut statement = connection
        .prepare("SELECT dst_slug FROM links l WHERE NOT EXISTS (SELECT 1 FROM memories m WHERE m.id = l.dst_slug OR m.id LIKE '%' || '/' || l.dst_slug) ORDER BY dst_slug")
        .map_err(|error| format!("MRD-DANGLING-WIKILINK: query failed: {error}"))?;
    let mut targets = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| format!("MRD-DANGLING-WIKILINK: query failed: {error}"))?
        .filter_map(Result::ok)
        .filter(|target| !external_refs.contains(&target.as_str()))
        .collect::<Vec<_>>();
    let count = targets.len();
    targets.sort();
    targets.dedup();
    targets.truncate(20);
    Ok(DoctorCheckV0 {
        code: "MRD-DANGLING-WIKILINK",
        severity: "warning",
        count,
        sample_ids: targets,
        repair: "repair link target or add an external-reference allowlist entry",
    })
}

fn check_embeddings(connection: &Connection) -> Result<DoctorCheckV0, String> {
    let mut statement = connection
        .prepare("SELECT id, embedding_q FROM memories")
        .map_err(|error| format!("MRD-EMBED-SHORT: query failed: {error}"))?;
    let invalid = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<Vec<u8>>>(1)?))
        })
        .map_err(|error| format!("MRD-EMBED-SHORT: query failed: {error}"))?
        .filter_map(|row| match row {
            Ok((id, quantized)) if !valid_embedding(quantized.as_deref()) => Some(Ok(id)),
            Ok(_) => None,
            Err(error) => Some(Err(error)),
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("MRD-EMBED-SHORT: row failed: {error}"))?;
    let mut sample_ids = invalid.clone();
    sample_ids.sort();
    sample_ids.truncate(20);
    Ok(DoctorCheckV0 {
        code: "MRD-EMBED-SHORT",
        severity: "critical",
        count: invalid.len(),
        sample_ids,
        repair: "reindex affected rows; do not promote with missing embeddings",
    })
}

fn valid_embedding(quantized: Option<&[u8]>) -> bool {
    quantized.is_some_and(valid_quantized_embedding)
}

fn check_scope_anomalies(connection: &Connection) -> Result<DoctorCheckV0, String> {
    let mut statement = connection
        .prepare("SELECT id, scope_id FROM memories")
        .map_err(|error| format!("MRD-SCOPE-ANOMALY: query failed: {error}"))?;
    let invalid = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })
        .map_err(|error| format!("MRD-SCOPE-ANOMALY: query failed: {error}"))?
        .filter_map(|row| match row {
            Ok((_id, Some(scope)))
                if !scope.trim().is_empty() && crate::scope::normalize_scope(&scope) == scope =>
            {
                None
            }
            Ok((id, _)) => Some(Ok(id)),
            Err(error) => Some(Err(error)),
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("MRD-SCOPE-ANOMALY: row failed: {error}"))?;
    let mut sample_ids = invalid.clone();
    sample_ids.sort();
    sample_ids.truncate(20);
    Ok(DoctorCheckV0 {
        code: "MRD-SCOPE-ANOMALY",
        severity: "warning",
        count: invalid.len(),
        sample_ids,
        repair: "normalize affected scopes with platform-aware scope tooling",
    })
}

fn valid_quantized_embedding(blob: &[u8]) -> bool {
    if blob.len() < 8 {
        return false;
    }
    let scale = f32::from_le_bytes([blob[0], blob[1], blob[2], blob[3]]);
    scale.is_finite() && scale > 0.0
}

fn check(
    connection: &Connection,
    code: &'static str,
    severity: &'static str,
    sql: &str,
    repair: &'static str,
) -> Result<DoctorCheckV0, String> {
    let count_sql = format!("SELECT COUNT(*) FROM ({sql})");
    let count = connection
        .query_row(&count_sql, [], |row| row.get::<_, i64>(0))
        .map_err(|error| format!("{code}: count query failed: {error}"))?;
    let sample_sql = format!("SELECT * FROM ({sql}) ORDER BY 1 LIMIT 20");
    let mut statement = connection
        .prepare(&sample_sql)
        .map_err(|error| format!("{code}: sample query failed: {error}"))?;
    let samples = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| format!("{code}: sample query failed: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("{code}: sample row failed: {error}"))?;
    Ok(DoctorCheckV0 {
        code,
        severity,
        count: usize::try_from(count).map_err(|_| format!("{code}: count exceeds usize"))?,
        sample_ids: samples,
        repair,
    })
}
