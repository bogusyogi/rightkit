//! Keyless sd.cpp adapter over the shared desktop/headless image queue.
use super::{
    Durations, GenRequest, Kind, ModelInfo, Output, PollOutcome, Provider, ProviderError,
    SubmitOutcome,
};
use rightkit_local_gen::{
    commands,
    paths::{self, EnvValues},
    profiles, settings,
    store::JobState,
    LocalImagePaths, LocalImageService, ServiceOptions,
};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

pub const ID: &str = "local-image";

/// Saved local-image memory mode (`auto`, `performance`, `balanced`, `low`), if any.
fn saved_memory_mode(app_data: &Path) -> Option<String> {
    settings::read(app_data).memory_mode
}

/// Memory mode as `doctor` and the model manager show it (configured, resolved here, options,
/// estimates). Reads the saved setting; starts nothing.
pub fn memory_report() -> Value {
    let root = crate::host::app_data_root();
    rightkit_local_gen::memory_status(&|| saved_memory_mode(&root), None)
}

/// Estimated resident peak, GB, of the memory mode a local image job would use on this machine
/// (an ESTIMATE until measured; see `rightkit_local_gen::memory`). For readiness numbers.
pub fn memory_estimate_gb() -> f64 {
    let root = crate::host::app_data_root();
    rightkit_local_gen::memory::current_estimated_peak_gb(saved_memory_mode(&root).as_deref())
}

/// The model folder of the user's own library (saved setting, else the default under the
/// per-user data folder), ignoring `RIGHTKIT_APP_DATA`. Read-only: `brief run` keeps its job state
/// in the project folder but reads installed models from here.
pub fn user_model_root() -> PathBuf {
    let root = crate::host::user_data_root();
    settings::read(&root)
        .model_root
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| root.join("local-image").join("models"))
}

/// Matches desktop resolution: env, saved model root, bundled binary, app-data fallback.
pub fn engine_paths() -> LocalImagePaths {
    paths_for(
        &crate::host::app_data_root(),
        &EnvValues::from_process(),
        &exe_dirs(),
    )
}

/// Every local-image path under app data root `root`, resolved by the shared rules in
/// `rightkit_local_gen::paths`. Reads the saved settings; writes nothing.
fn paths_for(root: &Path, env: &EnvValues, exe_dirs: &[PathBuf]) -> LocalImagePaths {
    let saved = settings::read(root);
    let content = paths::content_root(env);
    paths::local_image(
        root,
        &content,
        saved.model_root.as_deref().map(Path::new),
        env,
        exe_dirs,
    )
}

/// The folder of this process's executable: where `sd-server` is looked for first.
fn exe_dirs() -> Vec<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .into_iter()
        .collect()
}

fn error(message: String) -> ProviderError {
    let code = message.split(':').next().unwrap_or("local_image");
    let code = match code {
        "model_not_installed" | "backend_missing" => "not_installed",
        "invalid_dimensions" | "unknown_profile" => "invalid_request",
        other => other,
    }
    .to_string();
    let retryable = matches!(code.as_str(), "backend_busy" | "not_installed");
    ProviderError::new(code, message, retryable)
}

fn engine_cancelled(request_id: &str) -> bool {
    let Some(id) = request_id
        .strip_prefix("engine:")
        .and_then(|r| r.split(":attempt:").next())
    else {
        return false;
    };
    crate::store::open(&crate::Context::production())
        .ok()
        .and_then(|db| crate::store::get_job(&db, id).ok())
        .is_some_and(|job| job.status == "cancelled")
}

pub struct LocalImage {
    paths: LocalImagePaths,
    service: Mutex<Option<LocalImageService>>,
    verified: Mutex<HashSet<String>>,
    /// One caller drives several consecutive jobs through this provider (`brief run`): keep the
    /// model server loaded after a job that succeeded. It ends when this provider is dropped, which
    /// the caller does when its run ends; see [`LocalImage::warm_for_run`].
    warm: bool,
}

impl LocalImage {
    pub fn new() -> Self {
        Self {
            paths: engine_paths(),
            service: Mutex::new(None),
            verified: Mutex::new(HashSet::new()),
            warm: false,
        }
    }

    /// A provider for ONE caller's run of consecutive jobs in this process (`brief run`). The
    /// model server stays loaded between those jobs. It is released (immediately, no idle grace)
    /// when this provider is dropped at the end of the run, whether the run succeeded, failed or
    /// timed out; at once when another process waits for the machine lock (the idle holder
    /// yields); after a job that failed or was cancelled; and by the 300 s idle backstop. It dies
    /// with the process (watchdog / Job Object / parent-death signal) if the caller is killed.
    pub fn warm_for_run() -> Self {
        Self {
            paths: engine_paths(),
            service: Mutex::new(None),
            verified: Mutex::new(HashSet::new()),
            warm: true,
        }
    }

    fn service(&self, request_prefix: String) -> Result<LocalImageService, ProviderError> {
        let mut slot = self.service.lock().unwrap();
        if let Some(service) = slot.as_ref() {
            if self.warm {
                service.allow_request_prefix(request_prefix);
            }
            return Ok(service.clone());
        }
        let app_data = self
            .paths
            .app_data_dir
            .parent()
            .unwrap_or(&self.paths.app_data_dir)
            .to_path_buf();
        let options = ServiceOptions {
            worker_request_prefix: Some(request_prefix),
            request_cancelled: Arc::new(engine_cancelled),
            keep_warm_between_jobs: self.warm,
            idle_unload_seconds: Arc::new(move || {
                settings::read(&app_data)
                    .idle_unload_seconds
                    .unwrap_or(rightkit_local_gen::DEFAULT_IDLE_UNLOAD_SECONDS)
            }),
            memory_mode: {
                let app_data = self
                    .paths
                    .app_data_dir
                    .parent()
                    .unwrap_or(&self.paths.app_data_dir)
                    .to_path_buf();
                Arc::new(move || saved_memory_mode(&app_data))
            },
            ..ServiceOptions::default()
        };
        let service =
            LocalImageService::start_with_options(self.paths.clone(), options).map_err(error)?;
        *slot = Some(service.clone());
        Ok(service)
    }

    fn profile(&self, req: &GenRequest) -> Result<&'static profiles::ProfileSpec, ProviderError> {
        if req.kind != Kind::Image {
            return Err(ProviderError::new(
                "invalid_request",
                "local-image produces images",
                false,
            ));
        }
        let spec = profiles::profile(&req.model)
            .map_err(error)?
            .ok_or_else(|| {
                ProviderError::new(
                    "unknown_model",
                    format!("unknown image profile {}", req.model),
                    false,
                )
            })?;
        if commands::profile_unavailable_on_current_backend(spec).map_err(error)? {
            return Err(ProviderError::new(
                "profile_backend_unavailable",
                format!("{} is unavailable on this backend", spec.id),
                false,
            ));
        }
        Ok(spec)
    }

    fn installed(&self, spec: &profiles::ProfileSpec) -> Result<(), ProviderError> {
        if !self.paths.backend_binary.is_file() {
            return Err(ProviderError::new(
                "not_installed",
                format!("sd-server missing: {}", self.paths.backend_binary.display()),
                true,
            ));
        }
        let dir = profiles::profile_dir(&self.paths, &spec.id, spec.revision);
        for component in spec.components.iter().filter(|c| c.required) {
            let present = std::fs::metadata(dir.join(&component.file))
                .ok()
                .is_some_and(|m| {
                    m.is_file()
                        && m.len() > 0
                        && component.bytes.is_none_or(|b| b > 0 && m.len() == b as u64)
                });
            if !present {
                return Err(ProviderError::new(
                    "not_installed",
                    format!("{} missing or incomplete: {}", spec.id, component.file),
                    true,
                ));
            }
        }
        Ok(())
    }

    /// Passive per-profile readiness; never starts sd-server or reads model bytes.
    pub fn model_ready(&self, model: &str) -> Result<(), ProviderError> {
        let spec = profiles::profile(model)
            .map_err(error)?
            .ok_or_else(|| error(format!("unknown_profile: {model}")))?;
        if commands::profile_unavailable_on_current_backend(spec).map_err(error)? {
            return Err(error(format!("profile_backend_unavailable: {model}")));
        }
        self.installed(spec)
    }

    pub fn first_installed_model(&self) -> Option<String> {
        profiles::catalogue()
            .ok()?
            .profiles
            .iter()
            .find(|p| self.model_ready(&p.id).is_ok())
            .map(|p| p.id.clone())
    }

    fn request(
        &self,
        req: &GenRequest,
        request_id: String,
    ) -> Result<commands::SubmitRequest, ProviderError> {
        let spec = self.profile(req)?;
        let p = &req.params;
        if !p.is_null() && !p.is_object() {
            return Err(error("invalid_request: params must be an object".into()));
        }
        if p.get("image_path").is_some() && p.get("reference_path").is_some() {
            return Err(error("invalid_request: supply one reference path".into()));
        }
        for key in ["image_path", "reference_path"] {
            if p.get(key)
                .is_some_and(|v| !v.as_str().is_some_and(|s| !s.is_empty()))
            {
                return Err(error(format!(
                    "invalid_request: {key} must be a local file path"
                )));
            }
        }
        if let Some(params) = p.as_object() {
            for key in params.keys() {
                if !matches!(
                    key.as_str(),
                    "aspect"
                        | "width"
                        | "height"
                        | "seed"
                        | "negative_prompt"
                        | "sample_steps"
                        | "sample_method"
                        | "scheduler"
                        | "txt_cfg"
                        | "image_path"
                        | "reference_path"
                ) {
                    return Err(error(format!(
                        "invalid_request: unknown local-image param {key}"
                    )));
                }
            }
        }
        // Pipeline image templates carry aspect; explicit dimensions still win.
        let (width, height) = match p.get("aspect").and_then(Value::as_str).unwrap_or("") {
            "" | "1:1" | "square" => (spec.defaults.width, spec.defaults.height),
            "16:9" | "landscape" => (
                spec.defaults.width,
                ((spec.defaults.width * 9 / 16) / spec.dimension_multiple.max(1))
                    * spec.dimension_multiple.max(1),
            ),
            "9:16" | "portrait" => (
                ((spec.defaults.height * 9 / 16) / spec.dimension_multiple.max(1))
                    * spec.dimension_multiple.max(1),
                spec.defaults.height,
            ),
            other => {
                return Err(error(format!(
                    "invalid_request: unsupported aspect {other}"
                )))
            }
        };
        let dimension = |key: &str, default: u32| -> Result<i64, ProviderError> {
            match p.get(key) {
                None => Ok(default as i64),
                Some(v) => v
                    .as_i64()
                    .filter(|n| {
                        (64..=4096).contains(n) && n % spec.dimension_multiple.max(1) as i64 == 0
                    })
                    .ok_or_else(|| error(format!("invalid_request: invalid {key}"))),
            }
        };
        let mut overrides = json!({});
        for key in [
            "negative_prompt",
            "sample_steps",
            "sample_method",
            "scheduler",
            "txt_cfg",
        ] {
            if let Some(v) = p.get(key) {
                overrides[key] = v.clone();
            }
        }
        let seed = p
            .get("seed")
            .map(|v| {
                v.as_i64()
                    .filter(|s| *s >= -1)
                    .ok_or_else(|| error("invalid_request: seed must be -1 or nonnegative".into()))
            })
            .transpose()?;
        let request = commands::SubmitRequest {
            request_id,
            conversation_id: None,
            parent_message_id: None,
            profile_id: req.model.clone(),
            prompt: req.prompt.clone(),
            negative_prompt: None,
            width: dimension("width", width)?,
            height: dimension("height", height)?,
            seed,
            reference_ids: Vec::new(),
            overrides: Some(overrides),
            retry_of: None,
        };
        commands::resolved_settings(spec, &request).map_err(error)?;
        Ok(request)
    }

    fn submit_as(&self, req: &GenRequest, engine_id: &str) -> Result<SubmitOutcome, ProviderError> {
        let spec = self.profile(req)?;
        self.installed(spec)?;
        let mut request = self.request(req, format!("engine:{engine_id}"))?;
        if engine_cancelled(&request.request_id) {
            return Err(error("cancelled: local image job cancelled".into()));
        }
        let prefix = engine_id.split(":attempt:").next().unwrap_or(engine_id);
        let service = self.service(format!("engine:{prefix}:attempt:"))?;
        {
            let mut verified = self.verified.lock().unwrap();
            if !verified.contains(&spec.id) {
                let records = service
                    .store()
                    .lock()
                    .unwrap()
                    .components_for_profile(&spec.id, spec.revision)
                    .map_err(error)?;
                // RightKit adoption verifies large files synchronously. Do not
                // hold the queue store or detached runner hostage to cold I/O
                // when cancellation arrives before a local job row exists.
                let paths = self.paths.clone();
                let adoption_service = service.clone();
                let aborted = Arc::new(AtomicBool::new(false));
                let adoption_aborted = aborted.clone();
                let (send, receive) = std::sync::mpsc::channel();
                std::thread::Builder::new()
                    .name("local-image-adopt".into())
                    .spawn(move || {
                        let result = profiles::adopt_profile(&paths, spec, &records, |r| {
                            if adoption_aborted.load(Ordering::SeqCst) {
                                return Err("cancelled: model adoption cancelled".into());
                            }
                            adoption_service.store().lock().unwrap().upsert_component(r)
                        });
                        let _ = send.send(result);
                    })
                    .map_err(|e| error(format!("model_adoption_failed: {e}")))?;
                loop {
                    if engine_cancelled(&request.request_id) {
                        aborted.store(true, Ordering::SeqCst);
                        return Err(error(
                            "cancelled: local image job cancelled during adoption".into(),
                        ));
                    }
                    match receive.recv_timeout(Duration::from_millis(100)) {
                        Ok(result) => {
                            result.map_err(error)?;
                            break;
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                        Err(e) => return Err(error(format!("model_adoption_failed: {e}"))),
                    }
                }
                verified.insert(spec.id.clone());
            }
        }
        let reference = req
            .params
            .get("image_path")
            .or_else(|| req.params.get("reference_path"));
        if let Some(path) = reference {
            if !spec.tasks.iter().any(|t| t == "image_edit") {
                return Err(error(
                    "invalid_request: profile does not support edits".into(),
                ));
            }
            let path = path.as_str().filter(|p| !p.is_empty()).ok_or_else(|| {
                error("invalid_request: image_path must be a local file path".into())
            })?;
            let imported =
                commands::import_reference_blocking(&service, path.into()).map_err(error)?;
            let id = imported["reference"]["id"]
                .as_str()
                .ok_or_else(|| error("invalid_reference: missing imported id".into()))?;
            request.reference_ids.push(id.into());
        }
        if engine_cancelled(&request.request_id) {
            return Err(error("cancelled: local image job cancelled".into()));
        }
        let submitted = commands::submit_blocking(&service, request).map_err(error)?;
        Ok(SubmitOutcome::Accepted {
            provider_job_id: submitted.job.id,
        })
    }
}

impl Default for LocalImage {
    fn default() -> Self {
        Self::new()
    }
}

/// Passive, job-bound backend ownership, available before model readiness.
/// The recorded PID is the watchdog/group leader on Mac, sd-server on Windows.
pub fn job_backend_status(job_id: &str, provider_job_id: Option<&str>) -> Option<Value> {
    let paths = engine_paths();
    if !paths.database_path.is_file() {
        return None;
    }
    let store = rightkit_local_gen::store::Store::open(&paths.database_path).ok()?;
    let instance = rightkit_local_gen::runtime::active_instance(&paths.log_dir)?;
    let jobs = if let Some(id) = provider_job_id {
        store.job(id).ok()?.into_iter().collect::<Vec<_>>()
    } else {
        (1..=crate::jobs::MAX_ATTEMPTS)
            .filter_map(|attempt| {
                store
                    .job_by_request_id(&format!("engine:{job_id}:attempt:{attempt}"))
                    .ok()
                    .flatten()
            })
            .collect()
    };
    let job = jobs
        .iter()
        .find(|job| job.runtime_session_id.as_deref() == Some(&instance.session_id))?;
    Some(json!({"pid": instance.pid, "owner_pid": instance.owner_pid,
        "session_id": instance.session_id, "state": job.state.as_str(), "stage": job.stage}))
}

impl Drop for LocalImage {
    fn drop(&mut self) {
        if let Some(service) = self.service.get_mut().unwrap().take() {
            service.shutdown();
        }
    }
}

impl Provider for LocalImage {
    fn id(&self) -> &'static str {
        ID
    }
    fn models(&self) -> &'static [ModelInfo] {
        static MODELS: OnceLock<Vec<ModelInfo>> = OnceLock::new();
        MODELS.get_or_init(|| {
            profiles::catalogue()
                .map(|catalogue| {
                    catalogue
                        .profiles
                        .iter()
                        .filter(|p| {
                            !commands::profile_unavailable_on_current_backend(p).unwrap_or(true)
                        })
                        .map(|p| ModelInfo {
                            provider: ID,
                            model: p.id.as_str(),
                            kind: Kind::Image,
                            label: p.label.as_str(),
                            est_usd: 0.0,
                            durations: Durations::NONE,
                        })
                        .collect()
                })
                .unwrap_or_default()
        })
    }
    /// `request`: `aspect` is 1:1, 16:9 or 9:16 (or square, landscape, portrait); anything else is refused. `seed`
    /// is -1 or nonnegative.
    fn capabilities(&self, _model: &str) -> crate::capabilities::Capabilities {
        crate::capabilities::Capabilities::default()
            .mode("prompt")
            .setting(
                "aspect",
                crate::capabilities::aspect("aspect", &["1:1", "16:9", "9:16"], false),
            )
            .setting("seed", crate::capabilities::seed())
    }
    fn needs_key(&self) -> bool {
        false
    }
    fn submit_is_idempotent(&self) -> bool {
        true
    }
    fn estimate_usd(&self, req: &GenRequest) -> Result<f64, ProviderError> {
        self.request(req, "estimate".into())?;
        Ok(0.0)
    }
    fn runtime_ready(&self) -> Option<Result<(), ProviderError>> {
        Some(profiles::catalogue().map_err(error).and_then(|catalogue| {
            for spec in &catalogue.profiles {
                if !commands::profile_unavailable_on_current_backend(spec).map_err(error)?
                    && self.installed(spec).is_ok()
                {
                    return Ok(());
                }
            }
            Err(ProviderError::new(
                "not_installed",
                "install a supported local image profile and sd-server",
                true,
            ))
        }))
    }
    fn test_key(&self, _key: &str) -> Result<(), ProviderError> {
        self.runtime_ready().unwrap()
    }
    fn submit(&self, req: &GenRequest, _key: &str) -> Result<SubmitOutcome, ProviderError> {
        self.submit_as(req, &format!("{}:attempt:1", uuid::Uuid::new_v4()))
    }
    fn submit_job(
        &self,
        req: &GenRequest,
        _key: &str,
        job_id: &str,
        attempt: i64,
        _idempotency_key: &str,
    ) -> Result<SubmitOutcome, ProviderError> {
        self.submit_as(req, &format!("{job_id}:attempt:{attempt}"))
    }
    fn poll(&self, _req: &GenRequest, id: &str, _key: &str) -> Result<PollOutcome, ProviderError> {
        let prefix = {
            let store =
                rightkit_local_gen::store::Store::open(&self.paths.database_path).map_err(error)?;
            let job = store
                .job(id)
                .map_err(error)?
                .ok_or_else(|| error("unknown_job: local image job missing".into()))?;
            format!(
                "{}:attempt:",
                job.request_id
                    .split(":attempt:")
                    .next()
                    .unwrap_or(&job.request_id)
            )
        };
        let service = self.service(prefix)?;
        let snapshot = commands::get_blocking(&service, id.into()).map_err(error)?;
        match snapshot.job.state {
            JobState::Succeeded => {
                let mut outputs = Vec::new();
                for artifact in snapshot.artifacts {
                    // The finalized file stays where the gallery scans it; the library stores a
                    // clone/link of it and verifies the recorded hash on its own copy.
                    let path = PathBuf::from(&artifact.path);
                    let meta = std::fs::metadata(&path)
                        .map_err(|e| error(format!("artifact_missing: {e}")))?;
                    let Some(sha256) = artifact.sha256.clone() else {
                        return Err(error(
                            "artifact_corrupt: local image has no recorded hash".into(),
                        ));
                    };
                    outputs.push(Output::Finalized {
                        path,
                        mime: "image/png".into(),
                        sha256,
                        bytes: meta.len(),
                    });
                }
                if outputs.is_empty() {
                    return Err(error(
                        "artifact_missing: completed image has no artifacts".into(),
                    ));
                }
                Ok(PollOutcome::Completed { outputs })
            }
            JobState::Interrupted => Ok(PollOutcome::Failed {
                message: snapshot
                    .job
                    .error_message
                    .unwrap_or_else(|| "local runtime interrupted; resubmitting".into()),
            }),
            JobState::Failed => Err(ProviderError::new(
                snapshot
                    .job
                    .error_code
                    .unwrap_or_else(|| "generation_failed".into()),
                snapshot
                    .job
                    .error_message
                    .unwrap_or_else(|| "local generation interrupted".into()),
                false,
            )),
            JobState::Cancelled => Err(ProviderError::new(
                "cancelled",
                "local image job cancelled",
                false,
            )),
            _ => Ok(PollOutcome::Running),
        }
    }
    fn progress_message(&self, _req: &GenRequest, id: &str) -> Option<String> {
        let service = self.service.lock().ok()?.as_ref()?.clone();
        let snapshot = commands::get_blocking(&service, id.into()).ok()?;
        Some(snapshot.job.stage)
    }
    fn cancel_job(
        &self,
        _req: &GenRequest,
        id: Option<&str>,
        job_id: &str,
    ) -> Result<(), ProviderError> {
        // A never-submitted job needs no service, worker or runtime.
        if !self.paths.database_path.is_file() {
            return Ok(());
        }
        let local_ids = {
            let store =
                rightkit_local_gen::store::Store::open(&self.paths.database_path).map_err(error)?;
            match id {
                Some(id) => store
                    .job(id)
                    .map_err(error)?
                    .into_iter()
                    .map(|job| job.id)
                    .collect::<Vec<_>>(),
                None => {
                    let mut jobs = Vec::new();
                    // Same stable attempt IDs cover cancellation before the external id is saved.
                    for attempt in 1..=crate::jobs::MAX_ATTEMPTS {
                        if let Some(job) = store
                            .job_by_request_id(&format!("engine:{job_id}:attempt:{attempt}"))
                            .map_err(error)?
                        {
                            jobs.push(job.id);
                        }
                    }
                    jobs
                }
            }
        };
        if local_ids.is_empty() {
            return Ok(());
        }
        let service = self.service(format!("engine:{job_id}:attempt:"))?;
        for id in local_ids {
            commands::cancel_blocking(&service, id.clone()).map_err(error)?;
            let deadline = Instant::now() + Duration::from_secs(30);
            loop {
                let snapshot = commands::get_blocking(&service, id.clone()).map_err(error)?;
                if snapshot.job.state.is_terminal() || snapshot.job.state == JobState::Finalizing {
                    break;
                }
                if Instant::now() >= deadline {
                    return Err(error(
                        "cancel_timeout: local image backend did not settle".into(),
                    ));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        Ok(())
    }
    fn fetch(&self, output: &Output) -> Result<(Vec<u8>, String), ProviderError> {
        match output {
            Output::Bytes { bytes, mime } => Ok((bytes.clone(), mime.clone())),
            _ => Err(error("invalid_request: local-image returns bytes".into())),
        }
    }
    fn finalized_root(&self) -> Option<PathBuf> {
        Some(self.paths.output_root.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh folder under the temp dir, empty on return.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rightkit-media-gen-local-image-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Environment with the content root and sd-server binary pinned, so no host state leaks in.
    fn env(content: &str) -> EnvValues {
        EnvValues {
            content_root: Some(content.to_string()),
            sd_server: Some("/opt/sd/sd-server".into()),
            temp_dir: "/tmp".into(),
            ..EnvValues::default()
        }
    }

    #[test]
    fn saved_settings_come_from_the_shared_module() {
        let root = scratch("saved");
        let model = root.join("my-models");
        {
            let store = settings::open(&root).unwrap();
            settings::apply(
                &store,
                settings::LocalImagePatch {
                    model_root: Some(Some(model.to_string_lossy().into_owned())),
                    idle_unload_seconds: Some(Some(45)),
                    memory_mode: Some(Some("LOW".into())),
                },
            )
            .unwrap();
        }
        assert_eq!(saved_memory_mode(&root).as_deref(), Some("low"));
        assert_eq!(settings::read(&root).idle_unload_seconds, Some(45));
        let paths = paths_for(&root, &env("/content"), &[]);
        assert_eq!(paths.model_root, model);
        assert_eq!(paths.lora_root, model.join("loras"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn defaults_when_nothing_is_saved() {
        let root = scratch("defaults");
        assert_eq!(saved_memory_mode(&root), None);
        let paths = paths_for(&root, &env("/content"), &[]);
        assert_eq!(paths.model_root, root.join("local-image").join("models"));
        assert_eq!(
            paths.database_path,
            root.join("local-image").join("local-image.sqlite3")
        );
        assert_eq!(paths.backend_binary, PathBuf::from("/opt/sd/sd-server"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn output_root_sits_under_the_content_root() {
        let root = scratch("outputs");
        let content = root.join("Content");
        let paths = paths_for(&root, &env(&content.to_string_lossy()), &[]);
        assert_eq!(
            paths.output_root,
            content
                .join(&crate::host::config().content_subdir)
                .join("local-images")
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
