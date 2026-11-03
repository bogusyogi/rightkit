//! Result descriptors (§4.4) and acked-result cleanup.

use rightkit_db::rusqlite::params;
use serde_json::{json, Value};
use std::{collections::HashSet, fs, path::Path};
use time::{Duration, OffsetDateTime};

use crate::providers::{Output, Provider, ProviderError};
use crate::store::{self, je, Job};
use crate::{library, Context, GenError, GenResult};

fn media(kind: &str, data: &[u8]) -> Value {
    media_json(kind, crate::media::probe(data))
}

fn media_json(kind: &str, p: crate::media::Probe) -> Value {
    json!({"kind": kind, "width": p.width, "height": p.height, "duration_s": p.duration_s})
}

/// Probe an adopted library file from its leading bytes, at most
/// `PROBE_PREFIX_BYTES`. A read failure keeps the fields null; it never fails the job.
fn adopted_media(kind: &str, path: &Path, total_len: u64) -> Value {
    let probed = match read_prefix(path, crate::media::PROBE_PREFIX_BYTES) {
        Ok(prefix) => crate::media::probe_media(path, &prefix, total_len).unwrap_or_default(),
        Err(_) => crate::media::Probe::default(),
    };
    media_json(kind, probed)
}

/// Read up to `max` leading bytes of `path`.
fn read_prefix(path: &Path, max: usize) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut prefix = Vec::new();
    fs::File::open(path)?
        .take(max as u64)
        .read_to_end(&mut prefix)?;
    Ok(prefix)
}

/// Library asset ids a request names as inputs: any string under the library folder
/// (`<library>/<asset_id>/...`) and any `asset_id`, `asset_ids`, `parent_asset_ids`,
/// `source_asset_id` or `source_asset_ids` value whose library folder exists. These are the parents
/// of whatever the job derives from them.
fn parent_asset_ids(ctx: &Context, params: &Value) -> Vec<String> {
    fn walk(ctx: &Context, key: Option<&str>, v: &Value, out: &mut Vec<String>) {
        let library = ctx.library_dir();
        let mut add = |id: &str| {
            if !id.is_empty()
                && !id.contains(['/', '\\', '.'])
                && library.join(id).is_dir()
                && !out.iter().any(|o| o == id)
            {
                out.push(id.to_string());
            }
        };
        match v {
            Value::String(s) => {
                if matches!(
                    key,
                    Some(
                        "asset_id"
                            | "asset_ids"
                            | "parent_asset_ids"
                            | "source_asset_id"
                            | "source_asset_ids"
                    )
                ) {
                    add(s);
                } else if let Ok(rest) = Path::new(s).strip_prefix(&library) {
                    if let Some(first) = rest.components().next() {
                        add(&first.as_os_str().to_string_lossy());
                    }
                }
            }
            Value::Array(items) => items.iter().for_each(|i| walk(ctx, key, i, out)),
            Value::Object(map) => map.iter().for_each(|(k, i)| walk(ctx, Some(k), i, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(ctx, None, params, &mut out);
    out
}

/// Consent references of the voice record a job speaks with, as voices.rs stores them in the
/// record's `facts.rights.consent_refs`. Returns the record's reference (`voice:<id>`) and its
/// consent refs. `None` when the job names no voice record. A record with no consent returns an
/// empty list, and the caller then adds nothing.
///
/// Two places, both owned by the host's voice module (which depends on this crate, so the
/// layout is repeated here rather than imported):
/// - a local voice record `<data>/voices/local/<id>.json`, named by the `voice_prompt` param
///   (the record's `prompt_file`);
/// - a hosted ElevenLabs clone, named by `voice_id`, whose entry in
///   `<data>/voices/clone-consent.json` carries `facts`.
fn voice_consent(ctx: &Context, provider: &str, params: &Value) -> Option<(String, Vec<String>)> {
    if let Some(prompt) = params["voice_prompt"].as_str().filter(|p| !p.is_empty()) {
        return local_voice_consent(&ctx.data_dir.join("voices").join("local"), prompt);
    }
    let voice_id = params["voice_id"].as_str().filter(|v| !v.is_empty())?;
    if provider != "elevenlabs" {
        return None;
    }
    hosted_voice_consent(
        &ctx.data_dir.join("voices").join("clone-consent.json"),
        provider,
        voice_id,
    )
}

/// The local voice record whose `prompt_file` is `prompt`. A relative prompt resolves inside
/// `local`; an absolute one must be a file directly in `local` (`PathBuf::join` keeps an absolute
/// argument). Any other location names no record, so its consent is never borrowed.
fn local_voice_consent(local: &Path, prompt: &str) -> Option<(String, Vec<String>)> {
    let candidate = local.join(prompt);
    if candidate.parent() != Some(local) {
        return None;
    }
    let file = candidate.file_name()?.to_str()?.to_string();
    let mut paths: Vec<std::path::PathBuf> = fs::read_dir(local)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect();
    paths.sort();
    for path in paths {
        let Some(rec) = fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        else {
            continue;
        };
        if rec["prompt_file"].as_str() != Some(file.as_str()) {
            continue;
        }
        let id = match rec["id"].as_str() {
            Some(id) => id.to_string(),
            None => path.file_stem()?.to_str()?.to_string(),
        };
        return Some((format!("voice:{id}"), consent_refs_of(&rec["facts"])));
    }
    None
}

/// The latest consent-log entry for a hosted voice, written by voices.rs `clone_with`.
fn hosted_voice_consent(
    log_path: &Path,
    provider: &str,
    voice_id: &str,
) -> Option<(String, Vec<String>)> {
    let log: Value = serde_json::from_slice(&fs::read(log_path).ok()?).ok()?;
    let entry = log.as_array()?.iter().rev().find(|e| {
        e["provider"].as_str() == Some(provider) && e["voice_id"].as_str() == Some(voice_id)
    })?;
    Some((
        format!("voice:{provider}:{voice_id}"),
        consent_refs_of(&entry["facts"]),
    ))
}

/// Consent refs carried by a stored `facts` value; absent or unreadable facts carry none.
fn consent_refs_of(facts: &Value) -> Vec<String> {
    crate::host::asset_facts::AssetFacts::from_value(facts)
        .rights
        .consent_refs
}

/// Origin, evidentiary meaning and rights for the assets of `job`; a derived asset records its
/// parents and inherits their rights. A parent with no recorded facts reads `unknown`, which makes
/// the derived rights `unknown` too (never cleared by omission). Evidence follows the job kind
/// ([`job_evidence`]): a synthesising kind is `illustrative`; a transforming kind such as `upscale`
/// inherits the parents' evidence, so an upscale of an authentic photo stays `authentic` and a job
/// with no recorded parents is `unknown`.
///
/// A voice job that names a voice record (see [`voice_consent`]) carries that record's consent
/// references in `rights.consent_refs` and names the record in `origin.parents` as `voice:<id>`.
/// Only when the record has consent recorded; otherwise nothing is added. The rights status is
/// not changed by voice consent: consent is not a licence, so a root voice asset stays `unresolved`.
fn job_facts(ctx: &Context, job: &Job) -> crate::host::asset_facts::AssetFacts {
    use crate::host::asset_facts::{job_evidence, AssetFacts, EvidenceKind, Rights};
    let request: Value = serde_json::from_str(&job.request_json).unwrap_or(Value::Null);
    let parents = parent_asset_ids(ctx, &request["params"]);
    let parent_facts: Vec<AssetFacts> = if parents.is_empty() {
        Vec::new()
    } else {
        let db = store::open(ctx).ok();
        parents
            .iter()
            .map(|id| {
                db.as_ref()
                    .and_then(|d| store::asset_facts(d, id).ok())
                    .map(|(f, _)| f)
                    .unwrap_or_default()
            })
            .collect()
    };
    let parent_rights: Vec<Rights> = parent_facts.iter().map(|f| f.rights.clone()).collect();
    let parent_evidence: Vec<EvidenceKind> = parent_facts.iter().map(|f| f.evidence).collect();
    let evidence = job_evidence(&job.kind, &parent_evidence);
    let mut facts = AssetFacts::generated(
        &job.provider,
        &job.model,
        &job.job_id,
        parents,
        &parent_rights,
        evidence,
    );
    if let Some((voice_ref, consent_refs)) = voice_consent(ctx, &job.provider, &request["params"]) {
        if !consent_refs.is_empty() {
            for consent in consent_refs {
                if !facts.rights.consent_refs.contains(&consent) {
                    facts.rights.consent_refs.push(consent);
                }
            }
            if !facts.origin.parents.contains(&voice_ref) {
                facts.origin.parents.push(voice_ref);
            }
        }
    }
    facts
}

/// Carry a scoped job's ids onto the local-image provider's own `artifacts` row (the additive
/// `scope_json` column). Best effort: the library record and `facts.json` already carry the
/// scope, so a failure here never fails the job; the row then keeps a null scope, never a guess.
fn carry_scope_to_local_image(path: &Path, scope: &Value) {
    let database = crate::providers::local_image::engine_paths().database_path;
    if !database.is_file() {
        return;
    }
    if let Ok(store) = rightkit_local_gen::store::Store::open(&database) {
        let _ = store.set_artifact_scope_by_path(
            &path.to_string_lossy().replace('\\', "/"),
            &scope.to_string(),
        );
    }
}

/// Download (or take) each output, store it in the library, and build the
/// descriptor. Errors carry `retryable` so the runner can leave the job
/// recoverable.
pub fn build(
    ctx: &Context,
    provider: &dyn Provider,
    job: &Job,
    outputs: &[Output],
) -> Result<Value, (GenError, bool)> {
    if outputs.is_empty() {
        return Err((
            GenError::new("no_output", "provider returned no outputs"),
            false,
        ));
    }
    let mut assets = Vec::new();
    let facts = job_facts(ctx, job).to_value();
    let scope = store::job_scope(job);
    for (index, o) in outputs.iter().enumerate() {
        let mut storage: Option<&'static str> = None;
        let (s, mime, media_value, role, metadata) = match o {
            Output::Finalized {
                path,
                mime,
                sha256,
                bytes,
            } => {
                // The provider already finalized this file in its content tree: keep it there and
                // store a clone/link of it, so the library copy costs no second copy of the bytes.
                let root = provider.finalized_root().ok_or_else(|| {
                    (
                        GenError::new("invalid_output", "provider has no finalized content root"),
                        false,
                    )
                })?;
                let (s, kind) =
                    library::store_finalized_asset(ctx, &root, path, mime, sha256, *bytes)
                        .map_err(|e| {
                            let retryable = e.code != "invalid_output";
                            (e, retryable)
                        })?;
                storage = Some(kind.as_str());
                if !scope.is_null() {
                    carry_scope_to_local_image(path, &scope);
                }
                let media_value = adopted_media(&job.kind, &s.path, s.bytes);
                (s, mime.clone(), Some(media_value), None, None)
            }
            Output::Bytes { bytes, mime } => {
                let s = library::store_asset(ctx, bytes, mime).map_err(|e| (e, true))?;
                (s, mime.clone(), Some(media(&job.kind, bytes)), None, None)
            }
            Output::Url { .. } => {
                let (data, mime) = provider
                    .fetch(o)
                    .map_err(|e: ProviderError| (GenError::new(e.code, e.message), e.retryable))?;
                let s = library::store_asset(ctx, &data, &mime).map_err(|e| (e, true))?;
                (s, mime, Some(media(&job.kind, &data)), None, None)
            }
            Output::Path {
                path,
                mime,
                sha256,
                bytes,
            } => {
                let is_manifest = mime == "application/json"
                    && Path::new(path)
                        .file_name()
                        .is_some_and(|name| name == "shot-manifest.json");
                let source_manifest = if is_manifest {
                    if *bytes > 4 * 1024 * 1024 {
                        return Err((
                            GenError::new("invalid_output", "shot manifest exceeds 4 MiB"),
                            false,
                        ));
                    }
                    if let Ok(meta) = fs::symlink_metadata(path) {
                        if !meta.file_type().is_file() || meta.len() > 4 * 1024 * 1024 {
                            return Err((
                                GenError::new(
                                    "invalid_output",
                                    "shot manifest must be a regular file no larger than 4 MiB",
                                ),
                                false,
                            ));
                        }
                        let canonical = path
                            .canonicalize()
                            .map_err(|e| (crate::store::io_err(e), true))?;
                        let root = ctx
                            .data_dir
                            .join("blender-jobs")
                            .join(&job.job_id)
                            .canonicalize()
                            .map_err(|e| (crate::store::io_err(e), true))?;
                        if !canonical.starts_with(root) {
                            return Err((
                                GenError::new(
                                    "invalid_output",
                                    "shot manifest is outside its owned job directory",
                                ),
                                false,
                            ));
                        }
                        let body =
                            fs::read(canonical).map_err(|e| (crate::store::io_err(e), true))?;
                        let value: Value = serde_json::from_slice(&body).map_err(|e| {
                            (
                                GenError::new(
                                    "invalid_output",
                                    format!("invalid shot manifest JSON: {e}"),
                                ),
                                false,
                            )
                        })?;
                        if value["type"] != "generated_source" || value["job_id"] != job.job_id {
                            return Err((
                                GenError::new(
                                    "invalid_output",
                                    "shot manifest identity does not match job",
                                ),
                                false,
                            ));
                        }
                        Some(value)
                    } else {
                        None
                    }
                } else {
                    None
                };
                let s =
                    library::adopt_path_asset(ctx, &job.job_id, index, path, mime, sha256, *bytes)
                        .map_err(|e| {
                            let retryable = e.code != "invalid_output";
                            (e, retryable)
                        })?;
                let (role, metadata) = if is_manifest {
                    let value = match source_manifest {
                        Some(value) => value,
                        None => {
                            let body =
                                fs::read(&s.path).map_err(|e| (crate::store::io_err(e), true))?;
                            let value: Value = serde_json::from_slice(&body).map_err(|e| {
                                (
                                    GenError::new(
                                        "invalid_output",
                                        format!("invalid shot manifest JSON: {e}"),
                                    ),
                                    false,
                                )
                            })?;
                            if value["type"] != "generated_source" || value["job_id"] != job.job_id
                            {
                                return Err((
                                    GenError::new(
                                        "invalid_output",
                                        "shot manifest identity does not match job",
                                    ),
                                    false,
                                ));
                            }
                            value
                        }
                    };
                    (Some("shot_manifest"), Some(value))
                } else if mime.starts_with("video/") {
                    (Some("movie"), None)
                } else if mime.starts_with("image/") {
                    (Some("still"), None)
                } else {
                    (None, None)
                };
                let media_value = if role == Some("shot_manifest") {
                    None
                } else if role.is_some() {
                    Some(adopted_media(&job.kind, &s.path, s.bytes))
                } else {
                    None
                };
                (s, mime.clone(), media_value, role, metadata)
            }
        };
        let mut asset = json!({
            "asset_id": s.asset_id,
            "path": s.path.to_string_lossy(),
            "sha256": s.sha256,
            "bytes": s.bytes,
            "mime": mime,
            "provider": job.provider,
            "model": job.model,
        });
        asset["facts"] = facts.clone();
        asset["scope"] = scope.clone();
        // The gallery scan reads facts from this file beside the asset (it has no database).
        let facts_file = ctx.library_dir().join(&s.asset_id).join("facts.json");
        if !facts_file.exists() {
            let _ = rightkit_fs::atomic_write(
                &facts_file,
                json!({"facts": facts, "scope": scope})
                    .to_string()
                    .as_bytes(),
            );
        }
        if let Some(kind) = storage {
            asset["storage"] = json!(kind);
        }
        if let Some(media) = media_value {
            asset["media"] = media;
        }
        if let Some(role) = role {
            asset["role"] = json!(role);
            if role == "shot_manifest" {
                asset["media"] = json!({
                    "kind": "shot_manifest",
                    "role": "shot_manifest",
                    "metadata": {"generated_source": metadata.clone().unwrap_or(Value::Null)}
                });
            } else if !asset["media"].is_null() {
                asset["media"]["role"] = json!(role);
            }
        }
        if let Some(metadata) = metadata {
            asset["metadata"] = json!({"generated_source": metadata});
        }
        assets.push(asset);
    }
    // Map worker artifact IDs onto the corresponding library descriptors.
    // Hash/size/MIME binding survives library path renames and job retries.
    let generated_source = assets
        .iter()
        .find(|asset| asset["role"] == "shot_manifest")
        .map(|asset| asset["media"]["metadata"]["generated_source"].clone())
        .filter(|value| !value.is_null());
    if let Some(generated_source) = generated_source {
        if let Some(artifacts) = generated_source["artifacts"].as_array() {
            let mut mapped = HashSet::new();
            for artifact in artifacts {
                let id = artifact["id"].as_str().unwrap_or_default();
                let role = artifact["role"].as_str().unwrap_or_default();
                let matches: Vec<usize> = assets
                    .iter()
                    .enumerate()
                    .filter(|(_, asset)| {
                        asset["role"] != "shot_manifest"
                            && asset["sha256"] == artifact["sha256"]
                            && asset["bytes"] == artifact["bytes"]
                            && asset["mime"] == artifact["mime"]
                    })
                    .map(|(i, _)| i)
                    .collect();
                if matches.len() != 1
                    || id.is_empty()
                    || role.is_empty()
                    || !mapped.insert(matches[0])
                {
                    return Err((
                        GenError::new(
                            "invalid_output",
                            "shot manifest artifact does not uniquely match an adopted asset",
                        ),
                        false,
                    ));
                }
                let asset = &mut assets[matches[0]];
                asset["artifact_id"] = json!(id);
                asset["role"] = json!(role);
                asset["media"]["artifact_id"] = json!(id);
                asset["media"]["role"] = json!(role);
            }
        }
    }
    Ok(json!({
        "job_id": job.job_id,
        "request_id": job.request_id,
        "provider": job.provider,
        "model": job.model,
        "kind": job.kind,
        "created_at": store::now(),
        "acked_by": Value::Null,
        "acked_at": Value::Null,
        "assets": assets,
    }))
}

/// Delete library files of acked results older than `older_than_days`.
/// This is the only code path that deletes asset files or clears
/// `result_json`, and it only touches jobs with `acked_by` set. Un-acked
/// results are never touched. Returns the number of jobs cleaned. A job whose history rows could not be dropped
/// (`store_failed` from the row drop) is skipped, not counted and left as it was (its marker is intact, so the next
/// pass finishes it) and logged; the pass goes on with the other jobs. Any other failure still stops the pass.
pub fn cleanup_acked(ctx: &Context, older_than_days: i64) -> GenResult<usize> {
    cleanup_acked_pass(ctx, older_than_days).map(|pass| pass.cleaned)
}

/// What one [`cleanup_acked_pass`] did: the jobs cleaned, and the jobs skipped because their history rows could not be
/// dropped (left as they were; the next pass retries them).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CleanupPass {
    pub cleaned: usize,
    pub skipped: usize,
}

/// [`cleanup_acked`] reporting the skipped jobs as well as the cleaned ones.
pub fn cleanup_acked_pass(ctx: &Context, older_than_days: i64) -> GenResult<CleanupPass> {
    cleanup_acked_with(ctx, older_than_days, &library::remove_asset)
}

/// The app log (`<data dir>/logs/events.jsonl`, the file the desktop logs commands read): one warning for a skipped job,
/// carrying the job id and the error code only (never the message, which can hold paths). Best effort: a log failure
/// never changes the pass.
fn log_skipped_job(ctx: &Context, job_id: &str, code: &str) {
    let now_ms = (OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64;
    rightkit_logs::JsonlStore::new(
        ctx.data_dir.join("logs").join("events.jsonl"),
        10 * 1024 * 1024,
    )
    .append_best_effort(
        &rightkit_logs::EventRecord::new(now_ms, rightkit_logs::Level::Warn, "cleanup_job_skipped")
            .with_field("job_id", json!(job_id))
            .with_field("code", json!(code)),
    );
}

/// [`cleanup_acked`] with the per-asset removal injected (the tests make one job's row drop fail).
fn cleanup_acked_with(
    ctx: &Context,
    older_than_days: i64,
    remove_asset: &dyn Fn(&Context, &str) -> GenResult<library::Removed>,
) -> GenResult<CleanupPass> {
    let db = store::open(ctx)?;
    let cutoff = store::ts(OffsetDateTime::now_utc() - Duration::days(older_than_days));
    let rows: Vec<(String, String)> = db
        .read(|c| {
            let mut st = c.prepare("SELECT job_id, result_json FROM jobs WHERE status='complete' AND kind != 'asset_import' AND acked_by IS NOT NULL AND result_json IS NOT NULL AND updated_at <= ?1")?;
            let rows = st
                .query_map([&cutoff], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .map_err(je)?;
    let mut pass = CleanupPass::default();
    'jobs: for (id, rj) in rows {
        if let Ok(v) = serde_json::from_str::<Value>(&rj) {
            for a in v["assets"].as_array().into_iter().flatten() {
                if let Some(aid) = a["asset_id"].as_str() {
                    match remove_asset(ctx, aid) {
                        Ok(_) => {}
                        // The history rows could not be dropped: leave this job's result alone and move on.
                        Err(e) if e.code == "store_failed" => {
                            pass.skipped += 1;
                            log_skipped_job(ctx, &id, &e.code);
                            continue 'jobs;
                        }
                        Err(e) => return Err(e),
                    }
                }
            }
        }
        db.write(|c| {
            c.execute(
                "UPDATE jobs SET result_json=NULL, error_json=?2 WHERE job_id=?1 AND acked_by IS NOT NULL",
                params![
                    id,
                    json!({"code":"result_cleaned","message":"acknowledged result was cleaned up"})
                        .to_string()
                ],
            )?;
            Ok(())
        })
        .map_err(je)?;
        pass.cleaned += 1;
    }
    Ok(pass)
}
