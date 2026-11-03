//! Orchestration: submit with a journaled request id, follow jobs, import
//! results (plan §4.3, §4.4, §5.5; review r6 findings 5 and 6).
//!
//! Crash-safety model:
//! * The request (id + spec) is journaled **before** the engine is invoked.
//! * GenRight maps one `request_id` to one job, so recovery first looks the
//!   request up (`job status --request-id`; `job_not_found` is
//!   authoritative), and only if GenRight never recorded it calls `generate`
//!   again with the same id (which GenRight deduplicates).
//! * A request already mapped to a job id never calls `generate` again.
//! * An import copies to `media/` (temp, fsync, rename), verifies sha256,
//!   records `(job_id, asset_id)` in the journal, and only then `job ack`s.

use std::collections::hash_map::RandomState;
use std::collections::HashSet;
use std::fs::{self, File};
use std::hash::{BuildHasher, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rightkit_fs::digest::{blake3_bytes, blake3_file, sha256_file_with_len};
use rightkit_fs::{copy_verified_with_digest, AtomicWriteOptions, DigestAlgorithm};
use serde_json::{json, Value};

use crate::engine::{
    progress_event, AssetDescriptor, EngineInfo, GenRightClient, GenerateSpec, JobRecord,
    MediaKind, ProcessRunner, ResultDescriptor, Runner,
};
use crate::error::{io, Error, Result};
use crate::journal::{import_key, ImportRecord, Journal, RequestEntry, RequestState};
use crate::models::{ModelCatalog, SpecViolation};

/// Consumer name sent with `job ack`.
pub const DEFAULT_CONSUMER: &str = "rightkit";
/// `generate` error codes that mean no job was created and retrying the same
/// request cannot help.
const PERMANENT_GENERATE_ERRORS: [&str; 8] = [
    "invalid_input",
    "invalid_request",
    "bad_input",
    "unknown_provider",
    "unknown_model",
    "no_credentials",
    "spend_cap",
    "unknown_command",
];

#[derive(Debug, Clone, PartialEq)]
pub struct Submission {
    pub request_id: String,
    pub job_id: String,
    /// True when the request already existed (journal or GenRight dedup).
    pub deduplicated: bool,
}

#[derive(Debug, Clone)]
pub struct WaitOptions {
    pub poll_interval: Duration,
    /// Give up waiting (the job keeps running in GenRight).
    pub timeout: Option<Duration>,
    pub cancel: Option<Arc<AtomicBool>>,
}

impl Default for WaitOptions {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(2),
            timeout: None,
            cancel: None,
        }
    }
}

pub struct Orchestrator<R: Runner> {
    client: GenRightClient<R>,
    journal: Journal,
    project: PathBuf,
    consumer: String,
    catalog: OnceLock<ModelCatalog>,
}

impl Orchestrator<ProcessRunner> {
    /// Open a project against the installed engine at `engine` (the path the
    /// engine's discovery resolved; never PATH or a checkout).
    pub fn for_engine(project: impl Into<PathBuf>, engine: impl Into<PathBuf>) -> Self {
        Self::new(project, ProcessRunner::new(engine))
    }
}

impl<R: Runner> Orchestrator<R> {
    pub fn new(project: impl Into<PathBuf>, runner: R) -> Self {
        let project = project.into();
        Self {
            client: GenRightClient::new(runner),
            journal: Journal::open(&project),
            project,
            consumer: DEFAULT_CONSUMER.to_string(),
            catalog: OnceLock::new(),
        }
    }

    pub fn with_consumer(mut self, consumer: impl Into<String>) -> Self {
        self.consumer = consumer.into();
        self
    }

    /// Install the host NDJSON event sink (events are JSON objects with an
    /// `event` field; GenRight stderr NDJSON arrives as `genright.progress`).
    pub fn set_event_sink(&self, sink: impl FnMut(Value) + Send + 'static) {
        self.client.set_event_sink(sink);
    }

    pub fn client(&self) -> &GenRightClient<R> {
        &self.client
    }

    pub fn journal(&self) -> &Journal {
        &self.journal
    }

    /// Capability check over the engine's doctor report (read through RightKit's
    /// `parse_doctor`): contract v2 plus the generate and job capabilities. Runs before any
    /// other call; nothing is cached here.
    pub fn ensure_ready(&self) -> Result<EngineInfo> {
        self.client.check_capabilities()
    }

    /// The engine's model catalog (`models`), fetched once per orchestrator.
    pub fn models(&self) -> Result<&ModelCatalog> {
        if let Some(c) = self.catalog.get() {
            return Ok(c);
        }
        let structured = self
            .ensure_ready()?
            .capability("models.durations")
            .is_some_and(|v| v >= 1);
        let c = if structured {
            self.client.models_structured()?
        } else {
            self.client.models()?
        };
        Ok(self.catalog.get_or_init(|| c))
    }

    /// Check a spec against the engine's catalog without submitting
    /// ([`ModelCatalog::validate`]). Engines without the `models` capability
    /// are not checked.
    pub fn check_spec(&self, spec: &GenerateSpec) -> std::result::Result<(), SpecViolation> {
        match self.models() {
            Ok(c) => c.validate(spec),
            Err(_) => Ok(()),
        }
    }

    /// Validate `spec` and, for an engine advertising `models.durations`,
    /// fill in the model's default duration when a video spec omits it.
    /// Returns the spec to journal.
    fn prepare(&self, spec: &GenerateSpec) -> Result<GenerateSpec> {
        let has_models = self.ensure_ready()?.capability("models").is_some();
        if !has_models {
            return Ok(spec.clone());
        }
        let catalog = self.models()?;
        let spec = catalog
            .with_default_duration(spec)
            .unwrap_or_else(|| spec.clone());
        catalog.validate(&spec).map_err(Error::from)?;
        Ok(spec)
    }

    // ---- submit / recover ------------------------------------------------

    /// Journal the request, then invoke `generate`. With `request_id`
    /// supplied, repeating the call is idempotent. On a crash or lost reply
    /// the request stays journaled: call [`Self::recover`].
    pub fn submit(&self, spec: &GenerateSpec, request_id: Option<&str>) -> Result<Submission> {
        self.submit_placed(spec, request_id, None)
    }

    /// [`Self::submit`] that also journals a placement intent on a new request, so
    /// [`Self::recover`]'s caller can place the asset after a restart. An existing
    /// request keeps the intent it was journaled with.
    pub fn submit_placed(
        &self,
        spec: &GenerateSpec,
        request_id: Option<&str>,
        place: Option<Value>,
    ) -> Result<Submission> {
        self.ensure_ready()?;
        let prepared = self.prepare(spec)?;
        let spec = &prepared;
        let rid = match request_id {
            Some(r) => validate_id(r)?.to_string(),
            None => new_request_id(&spec.prompt),
        };
        let (entry, created) = self.journal.update(|d| {
            if let Some(e) = d.requests.get(&rid) {
                if e.kind != spec.kind
                    || e.provider != spec.provider
                    || e.model != spec.model
                    || e.prompt != spec.prompt
                    || e.params != spec.params
                {
                    return Err(Error::RequestConflict {
                        request_id: rid.clone(),
                    });
                }
                return Ok((e.clone(), false));
            }
            let e = RequestEntry {
                request_id: rid.clone(),
                kind: spec.kind,
                provider: spec.provider.clone(),
                model: spec.model.clone(),
                prompt: spec.prompt.clone(),
                params: spec.params.clone(),
                state: RequestState::Pending,
                job_id: None,
                error: None,
                created_at: now_secs(),
                place: place.clone(),
            };
            d.requests.insert(rid.clone(), e.clone());
            Ok((e, true))
        })?;
        self.client
            .emit(json!({"event": "genright.request", "request_id": rid, "state": "journaled"}));
        self.drive(&entry, !created)
    }

    /// Resolve every journaled request that has no job id yet: look it up in
    /// GenRight by request id, else call `generate` again with the same id.
    /// Never touches requests that already have a job id.
    pub fn recover(&self) -> Result<Vec<(String, Result<Submission>)>> {
        self.ensure_ready()?;
        let pending: Vec<RequestEntry> = self
            .journal
            .read()?
            .requests
            .into_values()
            .filter(|e| e.state == RequestState::Pending && e.job_id.is_none())
            .collect();
        Ok(pending
            .into_iter()
            .map(|e| {
                let rid = e.request_id.clone();
                let r = self.drive(&e, true);
                (rid, r)
            })
            .collect())
    }

    fn drive(&self, entry: &RequestEntry, lookup_first: bool) -> Result<Submission> {
        let rid = &entry.request_id;
        if let Some(job) = &entry.job_id {
            return Ok(Submission {
                request_id: rid.clone(),
                job_id: job.clone(),
                deduplicated: true,
            });
        }
        if entry.state == RequestState::Failed {
            let e = entry.error.clone().unwrap_or(Value::Null);
            return Err(Error::RequestFailed {
                request_id: rid.clone(),
                code: e["code"].as_str().unwrap_or("unknown").into(),
                message: e["message"].as_str().unwrap_or("").into(),
            });
        }
        if lookup_first {
            if let Some(job) = self.find_job(rid)? {
                self.record_job(rid, &job.job_id)?;
                return Ok(Submission {
                    request_id: rid.clone(),
                    job_id: job.job_id,
                    deduplicated: true,
                });
            }
        }
        let spec = GenerateSpec {
            kind: entry.kind,
            provider: entry.provider.clone(),
            model: entry.model.clone(),
            prompt: entry.prompt.clone(),
            params: entry.params.clone(),
        };
        // The widened modalities are accepted by `media generate`, not the
        // original image/video/voice command surface. Keep that original
        // surface for older v2 engines and their existing requests.
        let reply = if matches!(
            spec.kind,
            MediaKind::Image | MediaKind::Video | MediaKind::Voice
        ) {
            self.client.generate(&spec, rid)
        } else {
            let info = self.ensure_ready()?;
            for (name, required) in crate::engine::MEDIA_CAPABILITIES {
                let found = info.capability(name);
                if found.is_none_or(|v| v < required) {
                    return Err(Error::MissingCapability {
                        name: name.into(),
                        required,
                        found,
                    });
                }
            }
            self.client.media_generate(&spec, rid)
        };
        match reply {
            Ok(reply) => {
                if let Some(echo) = reply.request_id.as_deref() {
                    if echo != rid {
                        return Err(Error::MappingConflict {
                            request_id: rid.clone(),
                            existing: echo.to_string(),
                            attempted: reply.job_id,
                        });
                    }
                }
                self.record_job(rid, &reply.job_id)?;
                Ok(Submission {
                    request_id: rid.clone(),
                    job_id: reply.job_id,
                    deduplicated: reply.deduplicated,
                })
            }
            Err(Error::Engine { code, message })
                if PERMANENT_GENERATE_ERRORS.contains(&code.as_str()) =>
            {
                let err = json!({"code": code, "message": message});
                self.journal.update(|d| {
                    if let Some(e) = d.requests.get_mut(rid) {
                        if e.job_id.is_none() {
                            e.state = RequestState::Failed;
                            e.error = Some(err.clone());
                        }
                    }
                    Ok(())
                })?;
                Err(Error::RequestFailed {
                    request_id: rid.clone(),
                    code,
                    message,
                })
            }
            Err(e) => Err(e),
        }
    }

    /// Look a request up in GenRight by request id (`job_not_found` means
    /// GenRight never recorded it).
    fn find_job(&self, request_id: &str) -> Result<Option<JobRecord>> {
        self.client.job_status_by_request(request_id)
    }

    /// Atomically map a request id to its single job.
    fn record_job(&self, request_id: &str, job_id: &str) -> Result<()> {
        self.journal.update(|d| {
            let e = d
                .requests
                .get_mut(request_id)
                .ok_or_else(|| Error::UnknownRequest {
                    request_id: request_id.to_string(),
                })?;
            match &e.job_id {
                Some(existing) if existing != job_id => Err(Error::MappingConflict {
                    request_id: request_id.to_string(),
                    existing: existing.clone(),
                    attempted: job_id.to_string(),
                }),
                _ => {
                    e.job_id = Some(job_id.to_string());
                    e.state = RequestState::Submitted;
                    e.error = None;
                    Ok(())
                }
            }
        })?;
        self.client.emit(json!({
            "event": "genright.request", "request_id": request_id,
            "state": "submitted", "job_id": job_id,
        }));
        Ok(())
    }

    // ---- follow ----------------------------------------------------------

    pub fn request(&self, request_id: &str) -> Result<RequestEntry> {
        self.journal
            .read()?
            .requests
            .remove(request_id)
            .ok_or_else(|| Error::UnknownRequest {
                request_id: request_id.to_string(),
            })
    }

    pub fn requests(&self) -> Result<Vec<RequestEntry>> {
        Ok(self.journal.read()?.requests.into_values().collect())
    }

    fn job_of(&self, request_id: &str) -> Result<String> {
        self.request(request_id)?
            .job_id
            .ok_or_else(|| Error::NotSubmitted {
                request_id: request_id.to_string(),
            })
    }

    /// Live `job status` for a request (safe to call repeatedly).
    pub fn job_status(&self, request_id: &str) -> Result<JobRecord> {
        self.ensure_ready()?;
        self.client.job_status(&self.job_of(request_id)?)
    }

    /// Poll `job status` until terminal, emitting `genright.job` events on
    /// each change. Reattaches after a restart because it only needs the
    /// journal and GenRight.
    pub fn wait(&self, request_id: &str, opts: &WaitOptions) -> Result<JobRecord> {
        self.ensure_ready()?;
        let job_id = self.job_of(request_id)?;
        let started = Instant::now();
        let mut last: Option<(String, i64)> = None;
        let mut events_seen = HashSet::<String>::new();
        loop {
            let (rec, events) = self.client.job_status_events(&job_id)?;
            self.forward_events(request_id, &events, &mut events_seen);
            let key = (rec.status.clone(), rec.attempts);
            if last.as_ref() != Some(&key) {
                self.client.emit(json!({
                    "event": "genright.job", "request_id": request_id, "job_id": job_id,
                    "status": rec.status, "attempts": rec.attempts, "error": rec.error,
                }));
                last = Some(key);
            }
            if rec.is_terminal() {
                return Ok(rec);
            }
            if opts
                .cancel
                .as_ref()
                .is_some_and(|c| c.load(Ordering::Relaxed))
            {
                return Err(Error::WaitCancelled { job_id });
            }
            if opts.timeout.is_some_and(|t| started.elapsed() >= t) {
                return Err(Error::WaitTimeout { job_id });
            }
            std::thread::sleep(opts.poll_interval);
        }
    }

    /// Emit not-yet-seen entries of GenRight's persisted progress log (the
    /// last 200 `job.progress` events) as `genright.job_progress` events.
    fn forward_events(&self, request_id: &str, events: &[Value], seen: &mut HashSet<String>) {
        for ev in events {
            if seen.insert(ev.to_string()) {
                self.client
                    .emit(progress_event(ev, Some(request_id), Some("job status")));
            }
        }
    }

    // ---- import ----------------------------------------------------------

    /// Import every asset of a request's finished job (idempotent).
    pub fn import(&self, request_id: &str) -> Result<Vec<ImportRecord>> {
        let records = self.import_unacked(request_id)?;
        self.ack_imports(records)
    }

    /// Import an output bundle without acknowledging it to GenRight yet.
    /// Call `ack_imports` only after the caller has validated and committed its
    /// own project state.
    pub fn import_unacked(&self, request_id: &str) -> Result<Vec<ImportRecord>> {
        let entry = self.request(request_id)?;
        let job_id = entry.job_id.ok_or_else(|| Error::NotSubmitted {
            request_id: request_id.to_string(),
        })?;
        self.import_job_unacked(&job_id, Some(request_id), Some(entry.kind))
    }

    /// Acknowledge a previously imported bundle after its consumer committed it.
    pub fn ack_imports(&self, records: Vec<ImportRecord>) -> Result<Vec<ImportRecord>> {
        records
            .into_iter()
            .map(|rec| self.settle_ack(rec))
            .collect()
    }

    /// Import by GenRight job id (for jobs started outside this project).
    pub fn import_job(
        &self,
        job_id: &str,
        request_id: Option<&str>,
        kind: Option<MediaKind>,
    ) -> Result<Vec<ImportRecord>> {
        let records = self.import_job_unacked(job_id, request_id, kind)?;
        self.ack_imports(records)
    }

    /// Import by GenRight job id without acknowledging the result bundle.
    pub fn import_job_unacked(
        &self,
        job_id: &str,
        request_id: Option<&str>,
        kind: Option<MediaKind>,
    ) -> Result<Vec<ImportRecord>> {
        self.ensure_ready()?;
        validate_id(job_id)?;
        let desc: ResultDescriptor = match self.client.job_result(job_id) {
            Ok(d) => d,
            Err(Error::Engine { code, message }) => {
                return match code.as_str() {
                    "result_cleaned" => {
                        // Already imported earlier: answer from the journal.
                        let existing = self.imports_of(job_id)?;
                        if existing.is_empty() {
                            Err(Error::ResultCleaned {
                                job_id: job_id.to_string(),
                            })
                        } else {
                            Ok(existing)
                        }
                    }
                    "not_ready" => Err(Error::NotReady {
                        job_id: job_id.to_string(),
                        status: message.trim_start_matches("job is ").to_string(),
                    }),
                    "job_not_successful" => Err(Error::JobNotSuccessful {
                        job_id: job_id.to_string(),
                        status: message.trim_start_matches("job is ").to_string(),
                    }),
                    _ => Err(Error::Engine { code, message }),
                };
            }
            Err(e) => return Err(e),
        };
        let mut out = Vec::new();
        for asset in &desc.assets {
            out.push(self.import_asset(&desc, asset, request_id, kind)?);
        }
        crate::generated_source::validate_result_assets(&out)?;
        // Generic result integrity is checked here. Callers with additional
        // output invariants choose when to acknowledge via `ack_imports`.
        Ok(out)
    }

    fn imports_of(&self, job_id: &str) -> Result<Vec<ImportRecord>> {
        let prefix = format!("{job_id}:");
        Ok(self
            .journal
            .read()?
            .imports
            .into_iter()
            .filter(|(k, _)| k.starts_with(&prefix))
            .map(|(_, v)| v)
            .collect())
    }

    /// Imports recorded in the journal.
    pub fn imports(&self) -> Result<Vec<ImportRecord>> {
        Ok(self.journal.read()?.imports.into_values().collect())
    }

    fn import_asset(
        &self,
        desc: &ResultDescriptor,
        asset: &AssetDescriptor,
        request_id: Option<&str>,
        kind: Option<MediaKind>,
    ) -> Result<ImportRecord> {
        let job_id = desc.job_id.as_str();
        let asset_id = validate_id(&asset.asset_id)?;
        let key = import_key(job_id, asset_id);

        // Repeat import: the recorded copy is still intact, nothing to copy.
        if let Some(rec) = self.journal.read()?.imports.remove(&key) {
            if sha256_file_with_len(&rec.media_path)
                .is_ok_and(|(sha, bytes)| bytes == rec.bytes && sha == rec.sha256)
                && blake3_file(&rec.media_path).is_ok_and(|hash| hash == rec.blake3)
            {
                return Ok(rec);
            }
        }

        let ext = Path::new(&asset.path)
            .extension()
            .and_then(|e| e.to_str())
            .filter(|e| e.chars().all(|c| c.is_ascii_alphanumeric()) && !e.is_empty())
            .unwrap_or("bin")
            .to_ascii_lowercase();
        let media_dir = self.project.join("media");
        fs::create_dir_all(&media_dir).map_err(io(&media_dir))?;
        let rel = format!("media/genright-{job_id}-{asset_id}.{ext}");
        let dest = media_dir.join(format!("genright-{job_id}-{asset_id}.{ext}"));
        let expected = asset.sha256.to_ascii_lowercase();

        // A copy left by a crash between rename and journal write: adopt it
        // if it verifies, else replace it.
        let adopted = if dest.exists() {
            let (sha, bytes) = sha256_file_with_len(&dest).map_err(io(&dest))?;
            if sha == expected {
                Some((sha, blake3_file(&dest).map_err(io(&dest))?, bytes))
            } else {
                None
            }
        } else {
            None
        };
        let hashes = match adopted {
            Some(h) => h,
            None => self.copy_verified(desc, asset, &dest, &expected)?,
        };
        if let Some(b) = asset.bytes {
            if b != hashes.2 {
                return Err(Error::SizeMismatch {
                    job_id: job_id.to_string(),
                    asset_id: asset_id.to_string(),
                    expected: b,
                    actual: hashes.2,
                });
            }
        }
        let kind = kind
            .or_else(|| parse_kind(asset.media["kind"].as_str()))
            .or_else(|| request_id.and_then(|r| self.request(r).ok().map(|e| e.kind)));
        let record = ImportRecord {
            request_id: request_id.map(String::from),
            job_id: job_id.to_string(),
            asset_id: asset_id.to_string(),
            media_path: dest,
            media_rel: rel,
            sha256: hashes.0,
            blake3: hashes.1,
            bytes: hashes.2,
            kind,
            mime: asset.mime.clone(),
            media: asset.media.clone(),
            needs_probe: asset.needs_probe(),
            provider: pick(asset.provider.as_deref(), &desc.provider),
            model: pick(asset.model.as_deref(), &desc.model),
            imported_at: now_secs(),
            est_usd: None,
            actual_usd: None,
            acked: false,
        };
        let stored = self.journal.update(|d| {
            let rec = match d.imports.get(&key) {
                Some(old) if old.sha256 == record.sha256 => ImportRecord {
                    acked: old.acked,
                    ..record.clone()
                },
                _ => record.clone(),
            };
            d.imports.insert(key.clone(), rec.clone());
            Ok(rec)
        })?;
        self.client.emit(json!({
            "event": "genright.import", "request_id": stored.request_id, "job_id": stored.job_id,
            "asset_id": stored.asset_id, "media_path": stored.media_path, "sha256": stored.sha256,
        }));
        Ok(stored)
    }

    /// Stream the source into a temp file, verify against the descriptor,
    /// fsync, rename into place.
    fn copy_verified(
        &self,
        desc: &ResultDescriptor,
        asset: &AssetDescriptor,
        dest: &Path,
        expected_sha: &str,
    ) -> Result<(String, String, u64)> {
        let src = match fs::metadata(&asset.path) {
            Ok(_) => PathBuf::from(&asset.path),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let (input, path) = self.open_resolved(desc, asset, expected_sha)?;
                drop(input);
                path
            }
            Err(e) => return Err(io(PathBuf::from(&asset.path))(e)),
        };
        let parent = dest.parent().unwrap_or(Path::new("."));
        let staging = tempfile::Builder::new()
            .prefix(".genright-import-")
            .tempdir_in(parent)
            .map_err(io(parent))?;
        let staged = staging.path().join("artifact");
        let copied = copy_verified_with_digest(
            &src,
            &staged,
            AtomicWriteOptions::default(),
            DigestAlgorithm::Sha256,
            Some(expected_sha),
            |_, _| {},
        )
        .map_err(|e| match e {
            rightkit_fs::FsError::DigestMismatch { actual, .. } => Error::ShaMismatch {
                job_id: desc.job_id.clone(),
                asset_id: asset.asset_id.clone(),
                expected: expected_sha.to_string(),
                actual,
            },
            other => io(dest)(other.into()),
        })?;
        if let Some(expected) = asset.bytes {
            if copied.bytes != expected {
                return Err(Error::SizeMismatch {
                    job_id: desc.job_id.clone(),
                    asset_id: asset.asset_id.clone(),
                    expected,
                    actual: copied.bytes,
                });
            }
        }
        let blake3 = blake3_file(&staged).map_err(io(&staged))?;
        rightkit_fs::publish_file(&staged, dest).map_err(|e| io(dest)(e.into()))?;
        Ok((copied.digest, blake3, copied.bytes))
    }

    /// The descriptor path is gone (library moved or cleaned): ask GenRight
    /// where the asset is now. The copy is still verified against the
    /// descriptor's sha256 as it streams. Typed `SourceMissing` when GenRight
    /// reports `asset_not_found` or `exists:false` (or the resolved file is
    /// not there); `asset_corrupt` becomes `ShaMismatch`.
    fn open_resolved(
        &self,
        desc: &ResultDescriptor,
        asset: &AssetDescriptor,
        expected_sha: &str,
    ) -> Result<(File, PathBuf)> {
        let missing = |path: PathBuf| Error::SourceMissing {
            job_id: desc.job_id.clone(),
            asset_id: asset.asset_id.clone(),
            path,
        };
        let original = PathBuf::from(&asset.path);
        let res = match self.client.asset_resolve(&asset.asset_id) {
            Ok(Some(r)) => r,
            Ok(None) => return Err(missing(original)),
            Err(Error::Engine { code, .. }) if code == "asset_corrupt" => {
                return Err(Error::ShaMismatch {
                    job_id: desc.job_id.clone(),
                    asset_id: asset.asset_id.clone(),
                    expected: expected_sha.to_string(),
                    actual: "(GenRight reports asset_corrupt)".to_string(),
                })
            }
            Err(e) => return Err(e),
        };
        let path = PathBuf::from(&res.path);
        if !res.exists || res.path.is_empty() {
            return Err(missing(if res.path.is_empty() { original } else { path }));
        }
        let claimed = res.sha256.to_ascii_lowercase();
        if !claimed.is_empty() && claimed != expected_sha {
            return Err(Error::ShaMismatch {
                job_id: desc.job_id.clone(),
                asset_id: asset.asset_id.clone(),
                expected: expected_sha.to_string(),
                actual: claimed,
            });
        }
        match File::open(&path) {
            Ok(f) => {
                self.client.emit(json!({
                    "event": "genright.resolved", "job_id": desc.job_id,
                    "asset_id": asset.asset_id, "path": path,
                }));
                Ok((f, path))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(missing(path)),
            Err(e) => Err(io(&path)(e)),
        }
    }

    /// Ack a durably recorded import once; failure leaves `acked=false` for a
    /// later [`Self::flush_acks`] (the import itself stays valid).
    fn settle_ack(&self, rec: ImportRecord) -> Result<ImportRecord> {
        if rec.acked {
            return Ok(rec);
        }
        match self.client.job_ack(&rec.job_id, &self.consumer) {
            Ok(_) => {
                let key = import_key(&rec.job_id, &rec.asset_id);
                let updated = self.journal.update(|d| {
                    Ok(d.imports.get_mut(&key).map(|r| {
                        r.acked = true;
                        r.clone()
                    }))
                })?;
                self.client.emit(json!({
                    "event": "genright.ack", "job_id": rec.job_id, "asset_id": rec.asset_id,
                }));
                Ok(updated.unwrap_or(ImportRecord { acked: true, ..rec }))
            }
            Err(e) => {
                self.client.emit(json!({
                    "event": "genright.ack_failed", "job_id": rec.job_id, "error": e.to_string(),
                }));
                Ok(rec)
            }
        }
    }

    /// Record GenRight's estimate/charge from a job record onto that job's
    /// imported assets (provenance for the asset library). Call after `wait`
    /// returns the record and `import` has run.
    pub fn record_cost(
        &self,
        job_id: &str,
        est_usd: Option<f64>,
        actual_usd: Option<f64>,
    ) -> Result<()> {
        if est_usd.is_none() && actual_usd.is_none() {
            return Ok(());
        }
        self.journal.update(|d| {
            for rec in d.imports.values_mut() {
                if rec.job_id == job_id {
                    rec.est_usd = rec.est_usd.or(est_usd);
                    rec.actual_usd = rec.actual_usd.or(actual_usd);
                }
            }
            Ok(())
        })
    }

    /// Retry acks for imports recorded without one. Returns how many succeeded.
    pub fn flush_acks(&self) -> Result<usize> {
        self.ensure_ready()?;
        let mut n = 0;
        for rec in self.imports()?.into_iter().filter(|r| !r.acked) {
            if self.settle_ack(rec)?.acked {
                n += 1;
            }
        }
        Ok(n)
    }

    /// Submit, wait, import: one call for a host prompt.
    pub fn generate_and_import(
        &self,
        spec: &GenerateSpec,
        request_id: Option<&str>,
        wait: &WaitOptions,
    ) -> Result<Vec<ImportRecord>> {
        let sub = self.submit(spec, request_id)?;
        self.wait(&sub.request_id, wait)?;
        self.import(&sub.request_id)
    }
}

static ID_SEQ: AtomicU64 = AtomicU64::new(0);

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Fresh caller request id (`cr-` + 24 hex).
pub fn new_request_id(salt: &str) -> String {
    let mut h = RandomState::new().build_hasher();
    h.write(salt.as_bytes());
    h.write_u64(ID_SEQ.fetch_add(1, Ordering::Relaxed));
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&h.finish().to_le_bytes());
    bytes.extend_from_slice(&nanos.to_le_bytes());
    bytes.extend_from_slice(&std::process::id().to_le_bytes());
    bytes.extend_from_slice(salt.as_bytes());
    format!("cr-{}", &blake3_bytes(&bytes)[..24])
}

/// Ids end up in file names and journal keys: restrict them.
fn validate_id(id: &str) -> Result<&str> {
    let ok = !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'));
    if ok {
        Ok(id)
    } else {
        Err(Error::BadId(id.to_string()))
    }
}

fn pick(per_asset: Option<&str>, fallback: &str) -> String {
    per_asset
        .filter(|s| !s.is_empty())
        .unwrap_or(fallback)
        .to_string()
}

fn parse_kind(s: Option<&str>) -> Option<MediaKind> {
    match s? {
        "image" => Some(MediaKind::Image),
        "video" => Some(MediaKind::Video),
        "voice" => Some(MediaKind::Voice),
        _ => None,
    }
}
