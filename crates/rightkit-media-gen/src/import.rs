//! `asset.import`: bring a user-supplied file into the library with its provenance (PENDING 91, 92).
//!
//! The file is copied (never moved or linked to its source inode), checked against the media
//! probe, and recorded as a library asset beside a `facts.json`, an `assets` row, an
//! `asset_facts` row and (when given) its scope ids. An import is its own completed job of kind
//! `asset_import` (the `assets` table keys on a job), which never spends and is never cleaned up
//! by acknowledgement. Only a generation job may claim origin `generated`; rights stay `unknown`
//! unless the caller supplies a `cleared` status together with licence evidence.

use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use crate::host::asset_facts::{AssetFacts, EvidenceKind, OriginKind, Rights, RightsStatus, Scope};
use serde_json::{json, Value};

use crate::store::{self, io_err};
use crate::{library, provenance, Context, GenError, GenResult};
use rightkit_ffmpeg::quickprobe as media;

/// Top-level keys `asset.import` accepts; anything else is refused by name.
pub const IMPORT_KEYS: &[&str] = &[
    "path",
    "origin",
    "evidence",
    "rights",
    "source_url",
    "scope",
    "project",
    "brand",
    "dry_run",
];
const RIGHTS_KEYS: &[&str] = &[
    "status",
    "licence_evidence",
    "attribution",
    "permitted_uses",
    "consent_refs",
];
const SCOPE_KEYS: &[&str] = &["venture", "channel", "account"];

fn invalid(code: &str, message: impl Into<String>) -> GenError {
    GenError::new(code, message)
}

fn parse_origin(input: &Value) -> GenResult<OriginKind> {
    match input.get("origin").and_then(Value::as_str) {
        Some("supplied") => Ok(OriginKind::Supplied),
        Some("acquired") => Ok(OriginKind::Acquired),
        Some("captured") => Ok(OriginKind::Captured),
        Some("generated") => Err(invalid(
            "invalid_origin",
            "origin generated is refused: only a generation job can claim it; use supplied, acquired or captured",
        )),
        Some(other) => Err(invalid("invalid_origin", format!("origin must be supplied, acquired or captured, got {other}"))),
        None => Err(invalid("invalid_origin", "origin is required: supplied, acquired or captured")),
    }
}

fn parse_evidence(input: &Value) -> GenResult<EvidenceKind> {
    match input.get("evidence") {
        None | Some(Value::Null) => Ok(EvidenceKind::Unknown),
        Some(Value::String(s)) => match s.as_str() {
            "authentic" => Ok(EvidenceKind::Authentic),
            "illustrative" => Ok(EvidenceKind::Illustrative),
            "unknown" => Ok(EvidenceKind::Unknown),
            other => Err(invalid(
                "invalid_evidence",
                format!("evidence must be authentic, illustrative or unknown, got {other}"),
            )),
        },
        Some(_) => Err(invalid(
            "invalid_evidence",
            "evidence must be authentic, illustrative or unknown",
        )),
    }
}

/// Bounded single-line text (URLs, names, attributions).
fn opt_text(input: &Value, key: &str, max: usize) -> GenResult<Option<String>> {
    match input.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() || t.len() > max || t.chars().any(char::is_control) {
                Err(invalid(
                    "invalid_request",
                    format!("{key} must be 1 to {max} characters of plain text"),
                ))
            } else {
                Ok(Some(t.to_string()))
            }
        }
        Some(_) => Err(invalid(
            "invalid_request",
            format!("{key} must be a string"),
        )),
    }
}

/// A string or a list of strings, trimmed, de-duplicated and bounded.
fn string_list(v: &Value, field: &str) -> GenResult<Vec<String>> {
    let bad = || {
        invalid(
            "invalid_rights",
            format!("rights.{field} must be a string or a list of up to 50 non-empty strings"),
        )
    };
    let items: Vec<&Value> = match v {
        Value::Array(a) => a.iter().collect(),
        Value::String(_) => vec![v],
        _ => return Err(bad()),
    };
    if items.len() > 50 {
        return Err(bad());
    }
    let mut out: Vec<String> = Vec::new();
    for item in items {
        let s = item
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty() && s.len() <= 2000)
            .ok_or_else(bad)?;
        if !out.iter().any(|o| o == s) {
            out.push(s.to_string());
        }
    }
    Ok(out)
}

fn parse_rights(input: &Value, source_url: Option<&str>) -> GenResult<Rights> {
    let mut rights = Rights {
        source: source_url.map(str::to_string),
        ..Rights::default()
    };
    let Some(given) = input.get("rights").filter(|r| !r.is_null()) else {
        return Ok(rights);
    };
    let object = given
        .as_object()
        .ok_or_else(|| invalid("invalid_rights", "rights must be an object"))?;
    if let Some(key) = object.keys().find(|k| !RIGHTS_KEYS.contains(&k.as_str())) {
        return Err(invalid(
            "invalid_rights",
            format!(
                "unknown rights key {key}; accepted: {}",
                RIGHTS_KEYS.join(", ")
            ),
        ));
    }
    let list = |field: &str| -> GenResult<Vec<String>> {
        match object.get(field) {
            None | Some(Value::Null) => Ok(Vec::new()),
            Some(v) => string_list(v, field),
        }
    };
    rights.licence_evidence = list("licence_evidence")?;
    rights.permitted_uses = list("permitted_uses")?;
    rights.consent_refs = list("consent_refs")?;
    rights.attribution =
        opt_text(given, "attribution", 2000).map_err(|e| invalid("invalid_rights", e.message))?;
    rights.status = match object.get("status") {
        None | Some(Value::Null) => RightsStatus::Unknown,
        Some(Value::String(s)) => match s.as_str() {
            "unknown" => RightsStatus::Unknown,
            "unresolved" => RightsStatus::Unresolved,
            "cleared" => RightsStatus::Cleared,
            other => {
                return Err(invalid(
                    "invalid_rights",
                    format!("rights.status must be unknown, unresolved or cleared, got {other}"),
                ))
            }
        },
        Some(_) => {
            return Err(invalid(
                "invalid_rights",
                "rights.status must be unknown, unresolved or cleared",
            ))
        }
    };
    if rights.status == RightsStatus::Cleared && rights.licence_evidence.is_empty() {
        return Err(invalid(
            "invalid_rights",
            "rights.status cleared needs licence_evidence (a licence, receipt, agreement or release reference); without it rights stay unknown",
        ));
    }
    Ok(rights)
}

fn parse_scope(input: &Value) -> GenResult<Option<Scope>> {
    let Some(given) = input.get("scope").filter(|s| !s.is_null()) else {
        return Ok(None);
    };
    let object = given.as_object().ok_or_else(|| {
        invalid(
            "invalid_scope",
            "scope must be an object {venture, channel, account}",
        )
    })?;
    if let Some(key) = object.keys().find(|k| !SCOPE_KEYS.contains(&k.as_str())) {
        return Err(invalid(
            "invalid_scope",
            format!("unknown scope key {key}; accepted: venture, channel, account"),
        ));
    }
    Scope::from_request(given).map_err(|m| invalid("invalid_scope", m))
}

/// True when `path` (canonical) lies inside `root`, compared as given and canonicalised.
fn inside(path: &Path, root: &Path) -> bool {
    path.starts_with(root)
        || root
            .canonicalize()
            .map(|r| path.starts_with(r))
            .unwrap_or(false)
}

struct Source {
    canonical: PathBuf,
    bytes: u64,
    mime: &'static str,
    probe: media::Probe,
}

/// Validate the source file: absolute, a regular non-symlink file outside the library and the
/// local-image gallery, non-empty, and a media type the probe reads.
fn inspect(ctx: &Context, raw: &str) -> GenResult<Source> {
    let path = PathBuf::from(raw);
    if !path.is_absolute() {
        return Err(invalid("invalid_request", "path must be absolute"));
    }
    let meta = fs::symlink_metadata(&path).map_err(|e| {
        invalid(
            "source_not_found",
            format!("cannot read {}: {e}", path.display()),
        )
    })?;
    if meta.file_type().is_symlink() {
        return Err(invalid(
            "invalid_request",
            "path is a symbolic link; name the real file",
        ));
    }
    if !meta.is_file() {
        return Err(invalid("invalid_request", "path is not a regular file"));
    }
    let canonical = path.canonicalize().map_err(io_err)?;
    let library = ctx.library_dir();
    if inside(&canonical, &library) || inside(&path, &library) {
        return Err(invalid(
            "already_in_library",
            "path is already inside the library; it is not imported again (use its asset id)",
        ));
    }
    let images = library::local_images_root();
    if inside(&canonical, &images) || inside(&path, &images) {
        return Err(invalid(
            "invalid_request",
            "path is inside the local-image gallery, which holds produced assets already; import is for files from outside the app",
        ));
    }
    let bytes = meta.len();
    if bytes == 0 {
        return Err(invalid("unsupported_media", "the file is empty"));
    }
    let mut prefix = Vec::new();
    fs::File::open(&canonical)
        .and_then(|f| {
            f.take(media::PROBE_PREFIX_BYTES as u64)
                .read_to_end(&mut prefix)
        })
        .map_err(io_err)?;
    let mime = media::sniff_mime(&prefix).ok_or_else(|| {
        invalid(
            "unsupported_media",
            "the file is not a supported media type (PNG, JPEG, GIF, WebP, WAV, FLAC, MP3, MP4/MOV/M4A)",
        )
    })?;
    let probe = media::probe_media(&canonical, &prefix, bytes).map_err(io_err)?;
    if mime.starts_with("image/") && (probe.width.is_none() || probe.height.is_none()) {
        return Err(invalid(
            "unsupported_media",
            "the image header could not be read",
        ));
    }
    Ok(Source {
        canonical,
        bytes,
        mime,
        probe,
    })
}

fn media_kind(mime: &str) -> &'static str {
    if mime.starts_with("image/") {
        "image"
    } else if mime.starts_with("audio/") {
        "audio"
    } else {
        "video"
    }
}

/// `asset.import` `{path, origin, evidence?, rights?, source_url?, scope?, project?, brand?}` →
/// the new library asset (`asset_id, job_id, path, sha256, bytes, mime, exists, facts, scope,
/// media, storage`). `dry_run` validates everything and writes nothing.
pub fn import_asset_command(ctx: &Context, input: &Value, dry_run: bool) -> GenResult<Value> {
    let object = input
        .as_object()
        .ok_or_else(|| invalid("invalid_request", "asset.import input must be an object"))?;
    let unknown: Vec<&str> = object
        .keys()
        .map(String::as_str)
        .filter(|k| !IMPORT_KEYS.contains(k))
        .collect();
    if !unknown.is_empty() {
        return Err(invalid(
            "invalid_request",
            format!(
                "unknown asset.import key(s): {}; accepted keys: {}",
                unknown.join(", "),
                IMPORT_KEYS.join(", ")
            ),
        ));
    }
    let dry_run = dry_run
        || object
            .get("dry_run")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    let raw_path = input
        .get("path")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| invalid("invalid_request", "missing field: path"))?;
    let origin_kind = parse_origin(input)?;
    let evidence = parse_evidence(input)?;
    let source_url = opt_text(input, "source_url", 2048)?;
    let rights = parse_rights(input, source_url.as_deref())?;
    let scope = parse_scope(input)?;
    let project = opt_text(input, "project", 256)?;
    let brand = opt_text(input, "brand", 256)?;
    let source = inspect(ctx, raw_path)?;
    let kind = media_kind(source.mime);
    let facts: AssetFacts = provenance::recorded_facts(
        origin_kind,
        Some(
            source_url
                .clone()
                .unwrap_or_else(|| source.canonical.to_string_lossy().into_owned()),
        ),
        evidence,
        rights,
    );
    let facts_value = facts.to_value();
    let scope_value = scope.as_ref().map(Scope::to_value).unwrap_or(Value::Null);
    let media_value = json!({
        "kind": kind, "width": source.probe.width, "height": source.probe.height, "duration_s": source.probe.duration_s,
    });
    if dry_run {
        return Ok(json!({
            "dry_run": true,
            "would_import": {
                "path": source.canonical.to_string_lossy(), "mime": source.mime, "bytes": source.bytes,
                "media": media_value, "facts": facts_value, "scope": scope_value, "project": project, "brand": brand,
            },
        }));
    }

    let (source_sha, source_bytes) =
        library::sha256_file_with_len(&source.canonical).map_err(io_err)?;
    let asset_id = uuid::Uuid::new_v4().to_string();
    let dir = ctx.library_dir().join(&asset_id);
    fs::create_dir_all(&dir).map_err(io_err)?;
    let cleanup = |e: GenError| {
        let _ = fs::remove_dir_all(&dir);
        e
    };
    let final_path = dir.join(format!("{asset_id}.{}", library::ext_for_mime(source.mime)));
    // RightKit's ladder without the hard-link step: a clone or a byte copy, so the library file is
    // its own inode and an edit to the user's original never changes it. Nothing is overwritten.
    let outcome = rightkit_fs::clone_or_link_or_copy_nofollow_with(
        &source.canonical,
        &final_path,
        &rightkit_fs::CloneOptions::default().without_hard_link(),
    )
    .map_err(|e| cleanup(io_err(std::io::Error::other(e))))?;
    let storage = match outcome {
        rightkit_fs::CloneOutcome::Cloned => "clone",
        rightkit_fs::CloneOutcome::HardLinked => "hardlink",
        rightkit_fs::CloneOutcome::Copied => "copy",
    };
    let (sha256, bytes) =
        library::sha256_file_with_len(&final_path).map_err(|e| cleanup(io_err(e)))?;
    if sha256 != source_sha || bytes != source_bytes {
        return Err(cleanup(invalid(
            "import_failed",
            "the copy does not match the source; the source changed during import",
        )));
    }

    let mut request = json!({
        "kind": "asset_import", "provider": "import", "model": "file",
        "path": source.canonical.to_string_lossy(), "origin": origin_kind, "evidence": evidence,
        "source_url": source_url, "project": project, "brand": brand,
    });
    if let (Some(o), Some(_)) = (request.as_object_mut(), scope.as_ref()) {
        o.insert("scope".into(), scope_value.clone());
    }
    let db = store::open(ctx).map_err(cleanup)?;
    let (job, _) = store::insert_or_existing(
        &db,
        &store::NewJob {
            request_id: None,
            kind: "asset_import",
            provider: "import",
            model: "file",
            request_json: &request.to_string(),
            est_usd: 0.0,
        },
    )
    .map_err(cleanup)?;
    let finish = || -> GenResult<Value> {
        provenance::write_facts_file(
            &dir,
            &facts,
            &scope_value,
            Some(json!({"import": {"project": project, "brand": brand, "job_id": job.job_id}})),
        )?;
        let asset = json!({
            "asset_id": asset_id,
            "path": final_path.to_string_lossy(),
            "sha256": sha256,
            "bytes": bytes,
            "mime": source.mime,
            "provider": "import",
            "model": "file",
            "facts": facts_value,
            "scope": scope_value,
            "media": media_value,
            "storage": storage,
        });
        let descriptor = json!({
            "job_id": job.job_id, "request_id": Value::Null, "provider": "import", "model": "file",
            "kind": "asset_import", "created_at": store::now(), "acked_by": Value::Null, "acked_at": Value::Null,
            "assets": [asset.clone()],
        });
        store::record_assets(&db, &job.job_id, &descriptor)?;
        store::mark_complete(&db, &job.job_id, &descriptor)?;
        Ok(asset)
    };
    match finish() {
        Ok(mut asset) => {
            asset["job_id"] = json!(job.job_id);
            asset["exists"] = json!(final_path.is_file());
            asset["project"] = json!(project);
            asset["brand"] = json!(brand);
            asset["source_preserved"] = json!(source.canonical.is_file());
            Ok(asset)
        }
        Err(e) => {
            let _ = store::mark_failed(&db, &job.job_id, &store::rk_err(&e));
            Err(cleanup(e))
        }
    }
}
