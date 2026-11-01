// Local image generation: atomic artifact finalization — see docs/plans/local-image-generation.md

use std::fs::{self, File};
use std::io::Read;
use std::path::Path;

use image::GenericImageView;
use serde_json::{json, Map, Value};

use crate::LocalImagePaths;

use super::store::{
    new_id, now_iso, sha256_hex, ArtifactRecord, JobRecord, JobState, JobUpdate, Store,
};

const MAX_RECOVERY_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_RECOVERY_IMAGE_BYTES: u64 = 128 * 1024 * 1024;

pub struct ArtifactContext {
    pub profile_id: String,
    pub profile_revision: i64,
    pub component_hashes: Vec<(String, String)>,
    pub backend_pin: String,
    pub backend_commit: String,
    pub backend_binary_sha256: String,
    pub capabilities: Value,
    pub provider_job_id: Option<String>,
    pub runtime_session_id: Option<String>,
    pub steps: u32,
    pub sample_method: String,
    pub scheduler: Option<String>,
    pub txt_cfg: f64,
    pub negative_prompt: Option<String>,
    pub queued_at: String,
    pub started_at: String,
    pub finished_at: String,
    pub generate_ms: u64,
}

pub struct PreparedArtifacts {
    pub records: Vec<ArtifactRecord>,
}

pub struct DecodedImage {
    pub bytes: Vec<u8>,
    pub width: i64,
    pub height: i64,
    pub has_alpha: bool,
}

pub fn decode_image(bytes: &[u8]) -> Result<DecodedImage, String> {
    const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    const MAX_DIMENSION: u32 = 4096;
    const MAX_PIXELS: u64 = 16_777_216;
    const MAX_ENCODED_BYTES: usize = 128 * 1024 * 1024;
    if bytes.len() > MAX_ENCODED_BYTES {
        return Err(format!("encoded image exceeds {MAX_ENCODED_BYTES} bytes"));
    }
    if !bytes.starts_with(PNG_SIGNATURE) {
        return Err("backend image output must be PNG".to_string());
    }
    let header_reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| format!("detect image format: {e}"))?;
    let (header_width, header_height) = header_reader
        .into_dimensions()
        .map_err(|e| format!("read image dimensions: {e}"))?;
    if header_width == 0
        || header_height == 0
        || header_width > MAX_DIMENSION
        || header_height > MAX_DIMENSION
        || u64::from(header_width) * u64::from(header_height) > MAX_PIXELS
    {
        return Err(format!(
            "image dimensions exceed limit: {header_width}x{header_height}"
        ));
    }
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| format!("detect image format: {e}"))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode().map_err(|e| format!("decode image: {e}"))?;
    let (width, height) = image.dimensions();
    Ok(DecodedImage {
        bytes: bytes.to_vec(),
        width: width as i64,
        height: height as i64,
        has_alpha: image.color().has_alpha(),
    })
}

pub fn normalize_fs_path(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

pub fn finalize_images(
    store: &Store,
    paths: &LocalImagePaths,
    job: &JobRecord,
    ctx: &ArtifactContext,
    images: &[Vec<u8>],
) -> Result<Vec<ArtifactRecord>, String> {
    let prepared = prepare_images(paths, job, ctx, images)?;
    store.complete_finalization(&job.id, &prepared.records)?;
    Ok(prepared.records)
}

/// Writes and publishes image files without holding the SQLite store lock.
/// Database rows and success state are committed separately via `Store::complete_finalization`.
pub fn prepare_images(
    paths: &LocalImagePaths,
    job: &JobRecord,
    ctx: &ArtifactContext,
    images: &[Vec<u8>],
) -> Result<PreparedArtifacts, String> {
    if images.is_empty() {
        return Err("finalize_images: no images".to_string());
    }
    let conversation_dir = paths.output_root.join(&job.conversation_id);
    let target = conversation_dir.join(&job.id);
    if target.exists() {
        return Err(format!(
            "artifacts already finalized: {}",
            normalize_fs_path(&target)
        ));
    }
    let staging = paths.output_root.join(format!(".staging-{}", job.id));
    if staging.exists() {
        fs::remove_dir_all(&staging)
            .map_err(|e| format!("clear {}: {e}", normalize_fs_path(&staging)))?;
    }
    fs::create_dir_all(&staging)
        .map_err(|e| format!("create {}: {e}", normalize_fs_path(&staging)))?;
    let entries = write_payload(&staging, job, ctx, images)?;
    fs::create_dir_all(&conversation_dir)
        .map_err(|e| format!("create {}: {e}", normalize_fs_path(&conversation_dir)))?;
    rightkit_fs::publish_dir(&staging, &target).map_err(|e| {
        format!(
            "publish {} -> {}: {e}",
            normalize_fs_path(&staging),
            normalize_fs_path(&target)
        )
    })?;
    let mut records = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let record = ArtifactRecord {
            id: new_id("art"),
            job_id: job.id.clone(),
            path: normalize_fs_path(&target.join(&entry.file)),
            sidecar_path: Some(normalize_fs_path(
                &target.join(format!("{}.sidecar.json", entry.file)),
            )),
            manifest_path: (index == 0)
                .then(|| normalize_fs_path(&target.join("run-manifest.json"))),
            sha256: Some(entry.sha256.clone()),
            bytes: entry.bytes,
            width: entry.decoded.width,
            height: entry.decoded.height,
            created_at: now_iso(),
        };
        records.push(record);
    }
    Ok(PreparedArtifacts { records })
}

pub fn recover_finalized(store: &Store, paths: &LocalImagePaths) -> Result<u32, String> {
    let root = &paths.output_root;
    let jobs = store.finalizing_jobs()?;
    let mut recovered = 0u32;
    for job in jobs {
        let conv_dir = root.join(&job.conversation_id);
        let target = conv_dir.join(&job.id);
        let staging = root.join(format!(".staging-{}", job.id));
        let source = if target.is_dir() {
            target.clone()
        } else if staging.is_dir() {
            staging.clone()
        } else {
            mark_interrupted(store, &job.id, "finalization_incomplete")?;
            continue;
        };
        let Some(mut records) = validate_finalized_directory(&source, &job)? else {
            mark_interrupted(store, &job.id, "finalization_incomplete")?;
            continue;
        };
        if source == staging {
            fs::create_dir_all(&conv_dir)
                .map_err(|e| format!("create {}: {e}", normalize_fs_path(&conv_dir)))?;
            rightkit_fs::publish_dir(&staging, &target).map_err(|e| {
                format!(
                    "publish {} -> {}: {e}",
                    normalize_fs_path(&staging),
                    normalize_fs_path(&target)
                )
            })?;
            for record in &mut records {
                let file = Path::new(&record.path)
                    .file_name()
                    .ok_or_else(|| "artifact path has no file name".to_string())?;
                record.path = normalize_fs_path(&target.join(file));
                record.sidecar_path = record.sidecar_path.as_ref().and_then(|path| {
                    Path::new(path)
                        .file_name()
                        .map(|file| normalize_fs_path(&target.join(file)))
                });
                record.manifest_path = record
                    .manifest_path
                    .as_ref()
                    .map(|_| normalize_fs_path(&target.join("run-manifest.json")));
            }
        }
        store.recover_finalization(&job.id, &records)?;
        recovered += 1;
    }
    Ok(recovered)
}

fn mark_interrupted(store: &Store, job_id: &str, reason: &str) -> Result<(), String> {
    let mut update = JobUpdate::transition(JobState::Interrupted, "interrupted");
    update.error_code = Some(Some(reason.to_string()));
    update.error_message = Some(Some(
        "finalization files were incomplete or invalid".to_string(),
    ));
    store.set_state(job_id, &update)?;
    Ok(())
}

fn validate_finalized_directory(
    dir: &Path,
    job: &JobRecord,
) -> Result<Option<Vec<ArtifactRecord>>, String> {
    let manifest_path = dir.join("run-manifest.json");
    if !manifest_path.is_file() {
        return Ok(None);
    }
    let Some(raw) = read_recovery_file(&manifest_path, MAX_RECOVERY_MANIFEST_BYTES)? else {
        return Ok(None);
    };
    let manifest: Value = match serde_json::from_slice(&raw) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    let Some(items) = manifest.get("artifacts").and_then(Value::as_array) else {
        return Ok(None);
    };
    if items.is_empty() {
        return Ok(None);
    }
    let mut records = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let Some(file_name) = item.get("file").and_then(Value::as_str) else {
            return Ok(None);
        };
        let file = Path::new(file_name);
        if file.components().count() != 1
            || file.file_name().and_then(|n| n.to_str()) != Some(file_name)
        {
            return Ok(None);
        }
        let image_path = dir.join(file);
        let Some(actual) = read_recovery_file(&image_path, MAX_RECOVERY_IMAGE_BYTES)? else {
            return Ok(None);
        };
        let actual_hash = sha256_hex(&actual);
        let expected_hash = item.get("sha256").and_then(Value::as_str);
        let expected_bytes = item.get("bytes").and_then(Value::as_i64);
        let decoded = match decode_image(&actual) {
            Ok(decoded) => decoded,
            Err(_) => return Ok(None),
        };
        if expected_hash != Some(actual_hash.as_str())
            || expected_bytes != Some(actual.len() as i64)
            || item.get("width").and_then(Value::as_i64) != Some(decoded.width)
            || item.get("height").and_then(Value::as_i64) != Some(decoded.height)
        {
            return Ok(None);
        }
        let sidecar = dir.join(format!("{file_name}.sidecar.json"));
        records.push(ArtifactRecord {
            id: new_id("art"),
            job_id: job.id.clone(),
            path: normalize_fs_path(&image_path),
            sidecar_path: sidecar.is_file().then(|| normalize_fs_path(&sidecar)),
            manifest_path: (index == 0).then(|| normalize_fs_path(&manifest_path)),
            sha256: Some(actual_hash),
            bytes: actual.len() as i64,
            width: decoded.width,
            height: decoded.height,
            created_at: now_iso(),
        });
    }
    Ok(Some(records))
}

fn read_recovery_file(path: &Path, max_bytes: u64) -> Result<Option<Vec<u8>>, String> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => metadata,
        _ => return Ok(None),
    };
    if metadata.len() > max_bytes {
        return Ok(None);
    }
    let mut file =
        File::open(path).map_err(|e| format!("open {}: {e}", normalize_fs_path(path)))?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    std::io::Read::by_ref(&mut file)
        .take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("read {}: {e}", normalize_fs_path(path)))?;
    if bytes.len() as u64 > max_bytes {
        return Ok(None);
    }
    Ok(Some(bytes))
}

struct WrittenArtifact {
    file: String,
    sha256: String,
    bytes: i64,
    decoded: DecodedImage,
}

fn write_payload(
    dir: &Path,
    job: &JobRecord,
    ctx: &ArtifactContext,
    images: &[Vec<u8>],
) -> Result<Vec<WrittenArtifact>, String> {
    let mut entries = Vec::with_capacity(images.len());
    for (index, raw) in images.iter().enumerate() {
        let decoded = decode_image(raw)?;
        let file = if index == 0 {
            "image.png".to_string()
        } else {
            format!("image_{index}.png")
        };
        write_file_synced(&dir.join(&file), raw)?;
        let sidecar = serde_json::to_vec_pretty(&sidecar_json(job, ctx, &decoded))
            .map_err(|e| e.to_string())?;
        write_file_synced(&dir.join(format!("{file}.sidecar.json")), &sidecar)?;
        entries.push(WrittenArtifact {
            file,
            sha256: sha256_hex(raw),
            bytes: raw.len() as i64,
            decoded,
        });
    }
    let manifest =
        serde_json::to_vec_pretty(&manifest_json(job, ctx, &entries)).map_err(|e| e.to_string())?;
    write_file_synced(&dir.join("run-manifest.json"), &manifest)?;
    Ok(entries)
}

fn write_file_synced(path: &Path, bytes: &[u8]) -> Result<(), String> {
    rightkit_fs::atomic_write(path, bytes)
        .map_err(|e| format!("write {}: {e}", normalize_fs_path(path)))
}

fn component_map(ctx: &ArtifactContext) -> Map<String, Value> {
    ctx.component_hashes
        .iter()
        .map(|(kind, sha)| (kind.clone(), Value::String(sha.clone())))
        .collect()
}

fn backend_json(ctx: &ArtifactContext) -> Value {
    json!({
        "pin": ctx.backend_pin,
        "commit": ctx.backend_commit,
        "session_id": ctx.runtime_session_id,
        "provider_job_id": ctx.provider_job_id,
    })
}

fn timings_json(ctx: &ArtifactContext) -> Value {
    json!({
        "queued_at": ctx.queued_at,
        "started_at": ctx.started_at,
        "finished_at": ctx.finished_at,
        "generate_ms": ctx.generate_ms,
    })
}

fn sidecar_json(job: &JobRecord, ctx: &ArtifactContext, decoded: &DecodedImage) -> Value {
    let mut sidecar = json!({
        "provider": "sdcpp-local",
        "model": ctx.profile_id,
        "prompt": job.prompt,
        "aspect": format!("{}x{}", decoded.width, decoded.height),
        "cost_usd": 0,
        "job_id": job.id,
        "profile_revision": ctx.profile_revision,
        "seed": job.seed,
        "width": decoded.width,
        "height": decoded.height,
        "steps": ctx.steps,
        "sample_method": ctx.sample_method,
        "txt_cfg": ctx.txt_cfg,
        "references": job.reference_ids,
        "component_hashes": component_map(ctx),
        "backend": backend_json(ctx),
        "backend_binary_sha256": ctx.backend_binary_sha256,
        "capabilities": ctx.capabilities,
        "timings": timings_json(ctx),
        "local_cost": { "api_charge_usd": 0, "note": "local compute only" },
    });
    if let Some(scheduler) = &ctx.scheduler {
        sidecar["scheduler"] = json!(scheduler);
    }
    if let Some(negative) = ctx.negative_prompt.as_deref().filter(|s| !s.is_empty()) {
        sidecar["negative_prompt"] = json!(negative);
    }
    sidecar
}

fn manifest_json(job: &JobRecord, ctx: &ArtifactContext, entries: &[WrittenArtifact]) -> Value {
    json!({
        "schema_version": "1.0.0",
        "job": {
            "id": job.id,
            "request_id": job.request_id,
            "conversation_id": job.conversation_id,
            "message_id": job.message_id,
            "parent_message_id": job.parent_message_id,
            "profile_id": job.profile_id,
            "profile_revision": job.profile_revision,
            "prompt": job.prompt,
            "negative_prompt": job.negative_prompt,
            "width": job.width,
            "height": job.height,
            "seed": job.seed,
            "reference_ids": job.reference_ids,
            "overrides": job.overrides,
            "retry_of": job.retry_of,
            "created_at": job.created_at,
            "sampling": {
                "steps": ctx.steps,
                "sample_method": ctx.sample_method,
                "scheduler": ctx.scheduler,
                "txt_cfg": ctx.txt_cfg,
            },
        },
        "artifacts": entries
            .iter()
            .map(|entry| json!({
                "file": entry.file,
                "sha256": entry.sha256,
                "bytes": entry.bytes,
                "width": entry.decoded.width,
                "height": entry.decoded.height,
                "has_alpha": entry.decoded.has_alpha,
            }))
            .collect::<Vec<_>>(),
        "component_hashes": component_map(ctx),
        "backend": backend_json(ctx),
        "backend_binary_sha256": ctx.backend_binary_sha256,
        "capabilities": ctx.capabilities,
        "provider_job_id": ctx.provider_job_id,
        "runtime_session_id": ctx.runtime_session_id,
        "timings": timings_json(ctx),
        "prompt": job.prompt,
        "negative_prompt": ctx.negative_prompt,
    })
}
