use super::artifacts;
use super::profiles::{self, ProfileView};
use super::runtime;
use super::store::{
    new_id, now_iso, JobState, JobUpdate, NewJob, NewMessage, ReferenceRecord, SubmitOutcome,
};
use super::{ConversationSnapshot, JobSnapshot, LocalImageService, RuntimeStatus};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Deserialize, Serialize)]
pub struct SubmitRequest {
    pub request_id: String,
    pub conversation_id: Option<String>,
    pub parent_message_id: Option<String>,
    pub profile_id: String,
    pub prompt: String,
    pub negative_prompt: Option<String>,
    pub width: i64,
    pub height: i64,
    pub seed: Option<i64>,
    #[serde(default)]
    pub reference_ids: Vec<String>,
    pub overrides: Option<serde_json::Value>,
    pub retry_of: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SubmitResponse {
    pub job: super::store::JobRecord,
}

pub fn resolved_settings(
    spec: &profiles::ProfileSpec,
    request: &SubmitRequest,
) -> Result<serde_json::Value, String> {
    let defaults = &spec.defaults;
    let mut resolved = serde_json::json!({
        "sample_steps": defaults.sample_steps, "sample_method": defaults.sample_method,
        "scheduler": defaults.scheduler, "txt_cfg": defaults.txt_cfg,
        "negative_prompt": defaults.negative_prompt,
    });
    if let Some(overrides) = &request.overrides {
        let values = overrides
            .as_object()
            .ok_or("invalid_request: overrides must be an object")?;
        for (key, value) in values {
            if !matches!(
                key.as_str(),
                "sample_steps" | "sample_method" | "scheduler" | "txt_cfg" | "negative_prompt"
            ) {
                return Err(format!("invalid_request: unknown override {key}"));
            }
            resolved[key] = value.clone();
        }
    }
    if let Some(negative) = &request.negative_prompt {
        resolved["negative_prompt"] = serde_json::json!(negative);
    }
    if !matches!(resolved["sample_steps"].as_u64(), Some(1..=100))
        || !resolved["txt_cfg"]
            .as_f64()
            .is_some_and(|n| n.is_finite() && (0.0..=50.0).contains(&n))
        || !resolved["sample_method"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
        || !(resolved["scheduler"].is_null()
            || resolved["scheduler"]
                .as_str()
                .is_some_and(|s| !s.is_empty()))
        || !resolved["negative_prompt"].is_string()
    {
        return Err("invalid_request: invalid sampling settings".into());
    }
    if request.seed.is_some_and(|seed| seed < -1) {
        return Err("invalid_request: seed must be -1 or nonnegative".into());
    }
    Ok(resolved)
}

pub fn profiles_blocking(service: &LocalImageService) -> Result<serde_json::Value, String> {
    let catalogue = profiles::catalogue()?;
    let backend = current_backend_name()?;
    let backend = backend.as_deref();
    let mut views: Vec<ProfileView> = {
        let store = service.store().lock().unwrap();
        catalogue
            .profiles
            .iter()
            .filter(|spec| !profile_backend_is_blocked(&spec.qualification, backend))
            .map(|spec| {
                let records = store
                    .components_for_profile(&spec.id, spec.revision)
                    .unwrap_or_default();
                profiles::profile_view(spec, &records, service.paths())
            })
            .collect()
    };
    for view in &mut views {
        view.label = profile_label_for_backend(&view.label, backend);
        view.defaults = profile_defaults_for_backend(&view.defaults, backend);
    }
    let preferred = if backend == Some("metal") {
        "qwen21-uc-q6"
    } else {
        "qwen21-base-q6"
    };
    views.sort_by_key(|profile| profile.id != preferred);
    Ok(serde_json::json!({ "profiles": views }))
}

pub fn current_backend_name() -> Result<Option<String>, String> {
    Ok(profiles::runtime_manifest()?
        .binaries
        .get(current_runtime_platform())
        .and_then(|entry| entry.get("backend"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string))
}

pub fn profile_unavailable_on_current_backend(
    spec: &profiles::ProfileSpec,
) -> Result<bool, String> {
    let backend = current_backend_name()?;
    Ok(profile_backend_is_blocked(
        &spec.qualification,
        backend.as_deref(),
    ))
}

fn profile_backend_is_blocked(qualification: &serde_json::Value, backend: Option<&str>) -> bool {
    backend
        .and_then(|name| qualification.get(name))
        .and_then(serde_json::Value::as_str)
        .is_some_and(|status| status.starts_with("failed_") || status.starts_with("unavailable_"))
}

fn current_runtime_platform() -> &'static str {
    if cfg!(windows) {
        "windows-x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "macos-aarch64"
    } else {
        "macos-x86_64"
    }
}

fn profile_label_for_backend(label: &str, backend: Option<&str>) -> String {
    if backend == Some("cpu") && !label.ends_with(" (CPU)") {
        format!("{label} (CPU)")
    } else {
        label.to_string()
    }
}

fn profile_defaults_for_backend(
    defaults: &profiles::ProfileDefaults,
    backend: Option<&str>,
) -> profiles::ProfileDefaults {
    let mut defaults = defaults.clone();
    if backend == Some("cpu") {
        defaults.width = 256;
        defaults.height = 256;
    }
    defaults
}

pub fn runtime_blocking(service: &LocalImageService) -> Result<RuntimeStatus, String> {
    Ok(service.runtime_status())
}

pub fn submit_blocking(
    service: &LocalImageService,
    request: SubmitRequest,
) -> Result<SubmitResponse, String> {
    let request_hash =
        super::store::sha256_hex(&serde_json::to_vec(&request).map_err(|e| e.to_string())?);
    {
        let store = service.store().lock().unwrap();
        if let Some(existing) = store.job_by_request_id(&request.request_id)? {
            if let Some(original) = existing
                .overrides
                .get("_request_hash")
                .and_then(|v| v.as_str())
            {
                if original != request_hash {
                    return Err(
                        "request_conflict: request_id was used with a different payload".into(),
                    );
                }
                return Ok(SubmitResponse { job: existing });
            }
        }
    }
    let spec = profiles::profile(&request.profile_id)?
        .ok_or_else(|| format!("unknown_profile: {}", request.profile_id))?;
    if profile_unavailable_on_current_backend(spec)? {
        return Err(format!(
            "profile_backend_unavailable: {} is not supported by this backend",
            spec.id
        ));
    }

    if request.request_id.trim().is_empty() {
        return Err("invalid_request: request_id is required".to_string());
    }
    if request.prompt.trim().is_empty() {
        return Err("invalid_request: prompt is required".to_string());
    }
    if request.width < 64
        || request.height < 64
        || request.width > 4096
        || request.height > 4096
        || request.width % spec.dimension_multiple.max(1) as i64 != 0
        || request.height % spec.dimension_multiple.max(1) as i64 != 0
    {
        return Err(format!(
            "invalid_dimensions: {}x{} must be within 64..4096 and multiples of {}",
            request.width, request.height, spec.dimension_multiple
        ));
    }

    let mut overrides = resolved_settings(spec, &request)?;
    overrides["_request_hash"] = serde_json::json!(request_hash);
    let store = service.store().lock().unwrap();
    if let Some(existing) = store.job_by_request_id(&request.request_id)? {
        if let Some(original) = existing
            .overrides
            .get("_request_hash")
            .and_then(|v| v.as_str())
        {
            if original != request_hash {
                return Err(
                    "request_conflict: request_id was used with a different payload".into(),
                );
            }
            return Ok(SubmitResponse { job: existing });
        }
        let attempt = NewJob {
            request_id: request.request_id.clone(),
            conversation_id: existing.conversation_id.clone(),
            message_id: None,
            parent_message_id: request.parent_message_id.clone(),
            profile_id: request.profile_id.clone(),
            profile_revision: spec.revision,
            prompt: request.prompt.clone(),
            negative_prompt: request.negative_prompt.clone().unwrap_or_default(),
            width: request.width,
            height: request.height,
            seed: request.seed.unwrap_or(-1),
            reference_ids: request.reference_ids.clone(),
            overrides: request.overrides.clone().unwrap_or(serde_json::json!({})),
            retry_of: request.retry_of.clone(),
        };
        return match store.submit(&attempt)? {
            SubmitOutcome::Existing(job) => Ok(SubmitResponse { job }),
            SubmitOutcome::Created(job) => Ok(SubmitResponse { job }),
        };
    }

    let references = store.references_by_ids(&request.reference_ids)?;
    if references.len() != request.reference_ids.len() {
        return Err("invalid_request: unknown reference id".to_string());
    }
    if request.reference_ids.len() > 1 {
        return Err("invalid_request: at most one reference per edit".to_string());
    }

    {
        let records = store.components_for_profile(&spec.id, spec.revision)?;
        let view = profiles::profile_view(spec, &records, service.paths());
        if view.install_state != "installed" {
            return Err(format!(
                "model_not_installed: profile {} is {}; install it first",
                spec.id, view.install_state
            ));
        }
    }

    let job = store.transaction(|store| {
        let conversation = match &request.conversation_id {
            Some(id) => store
                .conversation(id)?
                .ok_or_else(|| format!("unknown_conversation: {id}"))?,
            None => {
                let title: String = request.prompt.chars().take(48).collect();
                store.create_conversation(Some(&title))?
            }
        };

        let message = store.insert_message(&NewMessage {
            conversation_id: conversation.id.clone(),
            parent_id: request.parent_message_id.clone(),
            role: "user".to_string(),
            kind: "prompt".to_string(),
            prompt: Some(request.prompt.clone()),
            reference_ids: request.reference_ids.clone(),
            job_id: None,
        })?;

        let component_hashes: HashMap<String, String> = store
            .components_for_profile(&spec.id, spec.revision)?
            .into_iter()
            .filter(|c| {
                spec.components
                    .iter()
                    .any(|expected| expected.required && expected.kind == c.kind)
            })
            .filter_map(|c| c.sha256.map(|hash| (c.kind, hash)))
            .collect();
        overrides["_component_hashes"] = serde_json::json!(component_hashes);
        let job = NewJob {
            request_id: request.request_id.clone(),
            conversation_id: conversation.id.clone(),
            message_id: Some(message.id.clone()),
            parent_message_id: request.parent_message_id.clone(),
            profile_id: request.profile_id.clone(),
            profile_revision: spec.revision,
            prompt: request.prompt.clone(),
            negative_prompt: overrides["negative_prompt"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            width: request.width,
            height: request.height,
            seed: match request.seed {
                Some(seed) if seed >= 0 => seed,
                _ => rand::random::<u32>() as i64,
            },
            reference_ids: request.reference_ids.clone(),
            overrides,
            retry_of: request.retry_of.clone(),
        };
        let outcome = store.submit(&job)?;
        let job = match outcome {
            SubmitOutcome::Created(job) | SubmitOutcome::Existing(job) => job,
        };
        store.insert_message(&NewMessage {
            conversation_id: conversation.id.clone(),
            parent_id: Some(message.id.clone()),
            role: "assistant".to_string(),
            kind: "result".to_string(),
            prompt: None,
            reference_ids: vec![],
            job_id: Some(job.id.clone()),
        })?;
        Ok(job)
    })?;
    drop(store);
    service.emit_job(&job.id);
    service.notify();
    Ok(SubmitResponse { job })
}

pub fn get_blocking(service: &LocalImageService, job_id: String) -> Result<JobSnapshot, String> {
    let store = service.store().lock().unwrap();
    let job = store
        .job(&job_id)?
        .ok_or_else(|| format!("unknown_job: {job_id}"))?;
    let events = store.job_events(&job_id)?;
    let artifacts = store.artifacts_for_job(&job_id)?;
    let message = job
        .message_id
        .as_deref()
        .and_then(|id| store.message(id).ok().flatten());
    Ok(JobSnapshot {
        job,
        events,
        artifacts,
        message,
    })
}

pub fn list_blocking(
    service: &LocalImageService,
    conversation_id: Option<String>,
    limit: Option<u32>,
) -> Result<serde_json::Value, String> {
    let store = service.store().lock().unwrap();
    let conversations = match &conversation_id {
        Some(id) => store.conversation(id)?.into_iter().collect::<Vec<_>>(),
        None => store.list_conversations(limit.unwrap_or(50))?,
    };
    let mut snapshots = Vec::with_capacity(conversations.len());
    for conversation in conversations {
        let messages = store.list_messages(&conversation.id)?;
        let jobs = store.list_jobs(Some(&conversation.id), 200)?;
        let mut artifacts: HashMap<String, Vec<super::store::ArtifactRecord>> = HashMap::new();
        for job in &jobs {
            let records = store.artifacts_for_job(&job.id)?;
            if !records.is_empty() {
                artifacts.insert(job.id.clone(), records);
            }
        }
        snapshots.push(ConversationSnapshot {
            conversation,
            messages,
            jobs,
            artifacts,
        });
    }
    Ok(serde_json::json!({ "conversations": snapshots }))
}

pub fn cancel_blocking(
    service: &LocalImageService,
    job_id: String,
) -> Result<serde_json::Value, String> {
    let store = service.store().lock().unwrap();
    let job = store
        .job(&job_id)?
        .ok_or_else(|| format!("unknown_job: {job_id}"))?;
    let cancel_transition = |update: &JobUpdate| {
        match store.set_state(&job_id, update) {
            Ok(job) => Ok(job),
            Err(error) => {
                // The owner may have observed durable engine cancellation &
                // finished between this passive client's read and write.
                match store.job(&job_id)? {
                    Some(job)
                        if job.state.is_terminal()
                            || matches!(job.state, JobState::Cancelling | JobState::Finalizing) =>
                    {
                        Ok(job)
                    }
                    _ => Err(error),
                }
            }
        }
    };
    let updated = match job.state {
        JobState::Succeeded
        | JobState::Failed
        | JobState::Cancelled
        | JobState::Interrupted
        | JobState::Finalizing => job,
        JobState::Queued | JobState::Paused => {
            let mut upd = JobUpdate::transition(JobState::Cancelled, "cancelled");
            upd.error_code = Some(Some("user_cancel".to_string()));
            let record = cancel_transition(&upd)?;
            rightkit_logs::JsonlStore::new(
                service.paths().log_dir.join(runtime::LOCAL_IMAGE_LOG_FILE),
                runtime::LOCAL_IMAGE_LOG_MAX_BYTES,
            )
            .append_best_effort(
                &rightkit_logs::EventRecord::new(
                    runtime::log_now_ms(),
                    rightkit_logs::Level::Info,
                    "cancel_requested",
                )
                .with_field("job_id", serde_json::json!(job_id))
                .with_field("where", serde_json::json!("queue")),
            );
            record
        }
        JobState::Cancelling => job,
        _ => {
            service.request_cancel_flag(&job_id);
            let mut upd = JobUpdate::transition(JobState::Cancelling, "cancelling");
            upd.error_code = Some(Some("user_cancel".to_string()));
            let record = cancel_transition(&upd)?;
            rightkit_logs::JsonlStore::new(
                service.paths().log_dir.join(runtime::LOCAL_IMAGE_LOG_FILE),
                runtime::LOCAL_IMAGE_LOG_MAX_BYTES,
            )
            .append_best_effort(
                &rightkit_logs::EventRecord::new(
                    runtime::log_now_ms(),
                    rightkit_logs::Level::Info,
                    "cancel_requested",
                )
                .with_field("job_id", serde_json::json!(job_id))
                .with_field("where", serde_json::json!("active")),
            );
            record
        }
    };
    drop(store);
    service.emit_job(&job_id);
    service.notify();
    Ok(serde_json::json!({ "job": updated }))
}

pub fn resume_blocking(
    service: &LocalImageService,
    job_id: String,
) -> Result<serde_json::Value, String> {
    let store = service.store().lock().unwrap();
    let job = store
        .job(&job_id)?
        .ok_or_else(|| format!("unknown_job: {job_id}"))?;
    let updated = if job.state == JobState::Paused {
        store.set_state(&job_id, &JobUpdate::transition(JobState::Queued, "queued"))?
    } else {
        job
    };
    drop(store);
    service.emit_job(&job_id);
    service.notify();
    Ok(serde_json::json!({ "job": updated }))
}

pub fn import_reference_blocking(
    service: &LocalImageService,
    path: String,
) -> Result<serde_json::Value, String> {
    let source = std::path::PathBuf::from(&path);
    let canonical = source
        .canonicalize()
        .map_err(|e| format!("invalid_reference: {path}: {e}"))?;
    use std::io::Read;
    const MAX_REF_BYTES: usize = 64 * 1024 * 1024;
    let file = std::fs::File::open(&canonical)
        .map_err(|e| format!("read {}: {e}", canonical.display()))?;
    if file.metadata().map_err(|e| e.to_string())?.len() > MAX_REF_BYTES as u64 {
        return Err("invalid_reference: file exceeds 64 MiB".to_string());
    }
    let mut bytes = Vec::new();
    file.take(MAX_REF_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > MAX_REF_BYTES {
        return Err("invalid_reference: file exceeds 64 MiB".to_string());
    }
    let format = image::guess_format(&bytes)
        .map_err(|_| "invalid_reference: not a PNG/JPEG/WebP image".to_string())?;
    let mime = match format {
        image::ImageFormat::Png => "image/png",
        image::ImageFormat::Jpeg => "image/jpeg",
        image::ImageFormat::WebP => "image/webp",
        _ => return Err("invalid_reference: not a PNG/JPEG/WebP image".to_string()),
    };
    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(&bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    let decoded = reader
        .decode()
        .map_err(|e| format!("invalid_reference: {e}"))?;
    let sha256 = super::store::sha256_hex(&bytes);
    let ext = match format {
        image::ImageFormat::Png => "png",
        image::ImageFormat::Jpeg => "jpg",
        image::ImageFormat::WebP => "webp",
        _ => "bin",
    };
    if let Some(existing) = service
        .store()
        .lock()
        .unwrap()
        .reference_by_sha256(&sha256)?
    {
        return Ok(serde_json::json!({ "reference": existing }));
    }
    let stored_path = service
        .paths()
        .reference_dir
        .join(format!("{sha256}.{ext}"));
    if !(stored_path.exists() && super::store::sha256_file(&stored_path)? == sha256) {
        rightkit_fs::atomic_write(&stored_path, &bytes)
            .map_err(|e| format!("activate reference: {e}"))?;
    }
    let record = ReferenceRecord {
        id: new_id("ref"),
        stored_path: artifacts::normalize_fs_path(&stored_path),
        sha256,
        bytes: bytes.len() as i64,
        width: decoded.width() as i64,
        height: decoded.height() as i64,
        mime: mime.to_string(),
        created_at: now_iso(),
    };
    let store = service.store().lock().unwrap();
    if let Some(existing) = store.reference_by_sha256(&record.sha256)? {
        return Ok(serde_json::json!({ "reference": existing }));
    }
    store.insert_reference(&record)?;
    Ok(serde_json::json!({ "reference": record }))
}

pub fn install_profile_blocking(
    service: &LocalImageService,
    profile_id: String,
) -> Result<serde_json::Value, String> {
    service.install_profile(&profile_id)?;
    Ok(serde_json::json!({ "started": true }))
}

pub fn pause_install_blocking(
    service: &LocalImageService,
    profile_id: String,
) -> Result<serde_json::Value, String> {
    Ok(serde_json::json!({ "paused": service.pause_install(&profile_id) }))
}

pub fn cancel_install_blocking(
    service: &LocalImageService,
    profile_id: String,
) -> Result<serde_json::Value, String> {
    Ok(serde_json::json!({ "cancelled": service.cancel_install(&profile_id) }))
}

/// How long `unload_blocking` waits for the worker to stop the server.
pub const UNLOAD_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// Unload the model server now when no job holds it (`local_image.unload`). Returns
/// `{unloaded, runtime}`: `unloaded` is false when the server is still up after the wait (a job is
/// running or queued; the request stays pending for the next idle tick).
pub fn unload_blocking(service: &LocalImageService) -> Result<serde_json::Value, String> {
    let unloaded = service.request_unload(UNLOAD_WAIT);
    let runtime =
        serde_json::to_value(runtime_blocking(service)?).map_err(|e| format!("encode: {e}"))?;
    Ok(serde_json::json!({ "unloaded": unloaded, "runtime": runtime }))
}

/// Save `idle_unload_seconds` (0 to 86400) in the settings document under `app_data` (the app data
/// root), log it, and return the runtime status. The hosting process (the engine sidecar) calls
/// this; the running service reads the value on its next idle check.
pub fn set_idle_unload_blocking(
    service: &LocalImageService,
    app_data: &std::path::Path,
    seconds: u64,
) -> Result<RuntimeStatus, String> {
    let store =
        super::settings::open(app_data).map_err(|e| format!("settings unavailable: {e}"))?;
    super::settings::apply(
        &store,
        super::settings::LocalImagePatch {
            idle_unload_seconds: Some(Some(seconds)),
            ..Default::default()
        },
    )?;
    log_setting(
        service,
        "idle_unload_configured",
        "idle_unload_seconds",
        serde_json::json!(seconds),
    );
    runtime_blocking(service)
}

/// Save the local image memory mode (`auto`, `performance`, `balanced`, `low`). A server running in
/// another mode is restarted before the next job.
pub fn set_memory_mode_blocking(
    service: &LocalImageService,
    app_data: &std::path::Path,
    mode: &str,
) -> Result<RuntimeStatus, String> {
    super::memory::parse_requested(mode)?;
    let mode = mode.trim().to_ascii_lowercase();
    let store =
        super::settings::open(app_data).map_err(|e| format!("settings unavailable: {e}"))?;
    super::settings::apply(
        &store,
        super::settings::LocalImagePatch {
            memory_mode: Some(Some(mode.clone())),
            ..Default::default()
        },
    )?;
    log_setting(
        service,
        "memory_mode_configured",
        "memory_mode",
        serde_json::json!(mode),
    );
    runtime_blocking(service)
}

fn log_setting(
    service: &LocalImageService,
    event: &'static str,
    field: &'static str,
    value: serde_json::Value,
) {
    rightkit_logs::JsonlStore::new(
        service.paths().log_dir.join(runtime::LOCAL_IMAGE_LOG_FILE),
        runtime::LOCAL_IMAGE_LOG_MAX_BYTES,
    )
    .append_best_effort(
        &rightkit_logs::EventRecord::new(runtime::log_now_ms(), rightkit_logs::Level::Info, event)
            .with_field(field, value),
    );
}
