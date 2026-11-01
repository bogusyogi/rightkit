use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::path_policy::normalize_lossy;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GalleryResponse {
    pub artifacts: Vec<Value>,
    pub report: GalleryReport,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GalleryReport {
    pub schema_version: String,
    pub indexed_artifacts_total: usize,
    pub metadata_incomplete_count: usize,
    pub malformed_file_count: usize,
    pub missing_media_count: usize,
    pub skipped_path_count: usize,
    pub fatal_error_count: usize,
    pub elapsed_ms: u128,
    pub scanned_roots: Vec<String>,
    /// Matching artifacts across every root before paging; `indexed_artifacts_total` is this page.
    #[serde(default)]
    pub total_available: usize,
    #[serde(default)]
    pub offset: usize,
    #[serde(default)]
    pub has_more: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactKind {
    Image,
    Video,
    Audio,
    Unknown,
}

impl ArtifactKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Video => "video",
            Self::Audio => "audio",
            Self::Unknown => "unknown",
        }
    }
}

pub fn classify(path: &Path) -> ArtifactKind {
    let Some(ext) = path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
    else {
        return ArtifactKind::Unknown;
    };
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "webp" | "gif" | "avif" => ArtifactKind::Image,
        "mp4" | "mov" | "webm" | "mkv" => ArtifactKind::Video,
        "wav" | "mp3" | "m4a" | "flac" | "ogg" => ArtifactKind::Audio,
        _ => ArtifactKind::Unknown,
    }
}

/// Index media under `roots`, newest first. Every matching file is found with a cheap
/// metadata walk; full records (sidecars, approvals) are built only for the requested page
/// `[offset, offset + limit)`, so large libraries page instead of silently truncating.
/// `filter` narrows the matches before paging, so `total_available` counts the filtered set.
/// An invalid filter value is returned as the error message and nothing is walked.
pub fn index_roots(
    roots: &[String],
    offset: Option<usize>,
    limit: Option<usize>,
    limit_per_root: Option<usize>,
    include_intermediate: bool,
    filter: &IndexFilter,
) -> Result<GalleryResponse, String> {
    filter.validate()?;
    let started = Instant::now();
    let mut found: Vec<Found> = Vec::new();
    let mut skipped_path_count = 0;
    let mut fatal_error_count = 0;

    for root in roots {
        let root_path = normalize_lossy(root);
        if !root_path.exists() {
            skipped_path_count += 1;
            continue;
        }
        let before = found.len();
        if let Err(()) = visit_root(
            &root_path,
            limit_per_root.unwrap_or(usize::MAX),
            include_intermediate,
            &mut found,
        ) {
            fatal_error_count += 1;
        }
        if found.len() == before && !root_path.is_dir() {
            skipped_path_count += 1;
        }
    }

    // Overlapping roots must not list a file twice.
    found.sort_by(|a, b| a.path.cmp(&b.path));
    found.dedup_by(|a, b| a.path == b.path);
    // A local-image output is stored twice: the finalized gallery file and a library clone. The
    // finalized file is the listing while it exists in a scanned root; the clone is listed only without it.
    let scanned: Vec<PathBuf> = roots
        .iter()
        .filter_map(|root| normalize_lossy(root).canonicalize().ok())
        .collect();
    found.retain(|f| !superseded_by_gallery_file(&f.path, &scanned, include_intermediate));
    found.sort_by(|a, b| {
        b.modified
            .cmp(&a.modified)
            .then_with(|| a.path.cmp(&b.path))
    });

    let matched: Vec<&Found> = found
        .iter()
        .filter(|f| filter.matches_location(&f.path))
        .collect();
    // Eyes and jury states exist only in each record's sidecars, so those filters build every
    // match. Otherwise only the requested page is built. Stale eyes approvals are checked on the
    // records that are returned, and on every match when an eyes filter needs the approval state.
    let (total_available, offset, end, artifacts) = if filter.needs_records() {
        let mut records: Vec<Value> = matched
            .iter()
            .map(|f| artifact_json(&f.path, f.kind, f.visibility))
            .collect();
        if filter.eyes.is_some() {
            records.iter_mut().for_each(mark_stale_approval);
        }
        records.retain(|record| filter.matches_review(record));
        let (offset, end) = page_bounds(records.len(), offset, limit);
        let mut page = records[offset..end].to_vec();
        if filter.eyes.is_none() {
            page.iter_mut().for_each(mark_stale_approval);
        }
        (records.len(), offset, end, page)
    } else {
        let (offset, end) = page_bounds(matched.len(), offset, limit);
        let page: Vec<Value> = matched[offset..end]
            .iter()
            .map(|f| {
                let mut record = artifact_json(&f.path, f.kind, f.visibility);
                mark_stale_approval(&mut record);
                record
            })
            .collect();
        (matched.len(), offset, end, page)
    };

    Ok(GalleryResponse {
        report: GalleryReport {
            schema_version: "1.0.0".to_string(),
            indexed_artifacts_total: artifacts.len(),
            metadata_incomplete_count: artifacts
                .iter()
                .filter(|artifact| artifact["metadata_status"] != "complete")
                .count(),
            malformed_file_count: 0,
            missing_media_count: 0,
            skipped_path_count,
            fatal_error_count,
            elapsed_ms: started.elapsed().as_millis(),
            scanned_roots: roots.to_vec(),
            total_available,
            offset,
            has_more: end < total_available,
        },
        artifacts,
    })
}

/// `[offset, end)` of one page over `total` matches; `offset` is clamped to the match count.
fn page_bounds(total: usize, offset: Option<usize>, limit: Option<usize>) -> (usize, usize) {
    let offset = offset.unwrap_or(0).min(total);
    let end = offset
        .saturating_add(limit.unwrap_or(usize::MAX))
        .min(total);
    (offset, end)
}

/// Narrowing applied to the index before paging. An unset field matches every artifact.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IndexFilter {
    /// Exact `project_slug`: the artifact's parent folder name.
    pub project: Option<String>,
    /// Only artifacts with no project (an empty `project_slug`, i.e. library assets). Exclusive
    /// with `project`; an empty `project` string is never a way to ask for this.
    pub no_project: bool,
    /// Brand code, compared case-insensitively.
    pub brand: Option<String>,
    /// One of [`EYES_STATES`], compared against `approval.eyes`.
    pub eyes: Option<String>,
    /// One of [`JURY_STATES`], compared against `approval.auto_jury`.
    pub jury: Option<String>,
    /// Caller slug an engine caller registered the file under (`<file>.caller.json`), compared exactly.
    pub caller: Option<String>,
}

/// Sidecar naming the engine caller that registered a file. Must stay identical to
/// `rightkit_media_gen::host::CALLER_SIDECAR_SUFFIX`.
pub const CALLER_SIDECAR_SUFFIX: &str = ".caller.json";

/// `(slug, label)` from `<file>.caller.json` (`{"caller": slug, "label": text}`); `None` when the
/// file has no readable sidecar. The slug must be a plain lowercase token: anything else is ignored.
fn caller_of(path: &Path) -> Option<(String, String)> {
    let mut name = path.file_name()?.to_os_string();
    name.push(CALLER_SIDECAR_SUFFIX);
    let value = read_json_bounded(&path.with_file_name(name))?.ok()?;
    let slug = value.get("caller")?.as_str()?.trim().to_string();
    let plain = !slug.is_empty()
        && slug.len() <= 40
        && slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
    if !plain {
        return None;
    }
    let label = value
        .get("label")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|l| !l.is_empty() && l.len() <= 40)
        .unwrap_or(&slug)
        .to_string();
    Some((slug, label))
}

/// Eyes states the index reports in `approval.eyes`: the default, then `normalize_eyes_status`,
/// then `stale` for an approval whose file bytes changed since it was given (`mark_stale_approval`).
pub const EYES_STATES: &[&str] = &[
    "unknown", "pending", "approved", "rejected", "rework", "stale",
];
/// Jury states the index reports in `approval.auto_jury`: the default, then `normalize_jury_status`.
pub const JURY_STATES: &[&str] = &["unknown", "pending", "passed", "blocked", "warn_only"];

impl IndexFilter {
    /// Rejects an empty project or brand and any eyes or jury value the index never reports.
    pub fn validate(&self) -> Result<(), String> {
        if self.no_project && self.project.is_some() {
            return Err("no_project and project cannot be combined; send one of them".to_string());
        }
        for (field, value) in [
            ("project", &self.project),
            ("brand", &self.brand),
            ("caller", &self.caller),
        ] {
            if value.as_deref().map(str::trim).is_some_and(str::is_empty) {
                return Err(format!("{field} must not be empty when given"));
            }
        }
        if let Some(eyes) = self.eyes.as_deref().filter(|e| !EYES_STATES.contains(e)) {
            return Err(format!(
                "eyes must be one of {}; got {eyes:?}",
                EYES_STATES.join(", ")
            ));
        }
        if let Some(jury) = self.jury.as_deref().filter(|j| !JURY_STATES.contains(j)) {
            return Err(format!(
                "jury must be one of {}; got {jury:?}",
                JURY_STATES.join(", ")
            ));
        }
        Ok(())
    }

    /// Project and brand come from the path, so they are checked before any sidecar is read.
    fn matches_location(&self, path: &Path) -> bool {
        let absolute_path = path.to_string_lossy().replace('\\', "/");
        let slug = project_slug_of(path);
        self.project.as_deref().is_none_or(|p| p == slug)
            && (!self.no_project || slug.is_empty())
            && self
                .caller
                .as_deref()
                .is_none_or(|c| caller_of(path).is_some_and(|(slug, _)| slug == c.trim()))
            && self
                .brand
                .as_deref()
                .is_none_or(|b| b.eq_ignore_ascii_case(&infer_brand(&absolute_path)))
    }

    /// Eyes and jury come from sidecars, so they are checked on the built record. Both sides are
    /// normalised, because `apply_approval` stores the jury key's value as written.
    fn matches_review(&self, record: &Value) -> bool {
        let eyes = record["approval"]["eyes"].as_str().unwrap_or_default();
        let jury = record["approval"]["auto_jury"].as_str().unwrap_or_default();
        self.eyes
            .as_deref()
            .is_none_or(|e| normalize_eyes_status(eyes) == e)
            && self
                .jury
                .as_deref()
                .is_none_or(|j| normalize_jury_status(jury) == j)
    }

    fn needs_records(&self) -> bool {
        self.eyes.is_some() || self.jury.is_some()
    }
}

struct Found {
    path: PathBuf,
    kind: ArtifactKind,
    visibility: &'static str,
    modified: std::time::SystemTime,
}

fn visit_root(
    root: &Path,
    limit_per_root: usize,
    include_intermediate: bool,
    found: &mut Vec<Found>,
) -> Result<(), ()> {
    let mut stack = vec![root.to_path_buf()];
    let mut added_for_root = 0usize;
    while let Some(path) = stack.pop() {
        if added_for_root >= limit_per_root {
            break;
        }
        let meta = fs::metadata(&path).map_err(|_| ())?;
        if meta.is_dir() {
            if path
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.starts_with(".staging-"))
                .unwrap_or(false)
            {
                continue;
            }
            let mut children: Vec<PathBuf> = fs::read_dir(&path)
                .map_err(|_| ())?
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .collect();
            children.sort();
            for child in children.into_iter().rev() {
                stack.push(child);
            }
            continue;
        }
        let kind = classify(&path);
        if kind == ArtifactKind::Unknown {
            continue;
        }
        let path_string = path.to_string_lossy().replace('\\', "/");
        let visibility = if include_intermediate
            || !path_string.to_ascii_lowercase().contains("/intermediate")
        {
            "primary"
        } else {
            continue;
        };
        found.push(Found {
            path,
            kind,
            visibility,
            modified: meta.modified().unwrap_or(std::time::UNIX_EPOCH),
        });
        added_for_root += 1;
    }
    Ok(())
}

/// True when `path` is a library clone whose `pair.json` marker records a finalized gallery file that
/// still exists inside a scanned root (and would itself be listed). That gallery file is then the
/// listing for the image. The marker is read as plain JSON: this crate does not depend on rightkit-media-gen.
fn superseded_by_gallery_file(
    path: &Path,
    scanned: &[PathBuf],
    include_intermediate: bool,
) -> bool {
    let Some(dir) = path.parent() else {
        return false;
    };
    let Some(pair) = fs::read(dir.join("pair.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
    else {
        return false;
    };
    let Some(source) = pair["source"].as_str() else {
        return false;
    };
    let Ok(source) = Path::new(source).canonicalize() else {
        return false;
    };
    if !source.is_file() || !scanned.iter().any(|root| source.starts_with(root)) {
        return false;
    }
    include_intermediate
        || !source
            .to_string_lossy()
            .replace('\\', "/")
            .to_ascii_lowercase()
            .contains("/intermediate")
}

/// The artifact's project: its parent folder name, `unknown` when there is none. A library asset
/// (`<library>/<asset_id>/<asset_id>.<ext>`) has its asset id as parent, not a project, so its
/// project is empty and the gallery shows it under "No project".
fn project_slug_of(path: &Path) -> String {
    let Some(parent) = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|p| p.to_str())
    else {
        return "unknown".to_string();
    };
    let is_library_asset =
        path.file_stem().and_then(|s| s.to_str()) == Some(parent) && is_asset_id(parent);
    if is_library_asset {
        String::new()
    } else {
        parent.to_string()
    }
}

/// Shape of an asset id: five lowercase hex groups of 8-4-4-4-12. This matches a UUID and the
/// deterministic id `rightkit-media-gen` derives for adopted worker output (`adopt_path_asset`).
fn is_asset_id(name: &str) -> bool {
    let groups: Vec<&str> = name.split('-').collect();
    groups.len() == 5
        && [8, 4, 4, 4, 12].iter().zip(&groups).all(|(len, group)| {
            group.len() == *len
                && group
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
}

/// Eyes approvals are bound to the bytes they were given for, and to the engine's approval record.
/// An `approved` sidecar reads `approved` only when its recorded `hash` matches the file's current
/// bytes AND its `approval` record has the shape the engine's eyes gate writes and is bound to those
/// bytes (`approval_record_bound`); the gate in `rightkit-approvals` (`eyes::gate_state`) applies the same
/// rule. A recorded hash that no longer matches reads `stale`; an APPROVE without a bound record reads
/// `pending`, so a bare sidecar cannot pass the Gallery's eyes filter. Both block like a pending review.
///
/// Cost: every `approved` record a listing returns or that an eyes filter matches is hashed once,
/// because the record must be checked against the current bytes. Other records never read their media.
fn mark_stale_approval(artifact: &mut Value) {
    if artifact["approval"]["eyes"] != "approved" {
        return;
    }
    let Some(path) = artifact["absolute_path"].as_str().map(str::to_string) else {
        return;
    };
    let raw = artifact["raw_sidecars"]["eyes"].clone();
    let current = file_hash(Path::new(&path));
    let recorded = raw["hash"].as_str();
    let state = if recorded.is_some_and(|hash| current.as_deref() != Some(hash)) {
        "stale"
    } else if !approval_record_bound(&raw, current.as_deref()) {
        "pending"
    } else {
        return;
    };
    artifact["approval"]["eyes"] = json!(state);
    if artifact["approval"]["overall"] == "approved" {
        artifact["approval"]["overall"] = json!("needs_review");
    }
}

/// True when the sidecar's `approval` record has the shape `rightkit-approvals` writes
/// (non-empty `channel`, `nonce`, `approved_at`, a 64-hex `input_hash`, and a `legacy_hash` equal to
/// the file's current hash). Shape only: it cannot prove who wrote the record. Must stay identical to
/// `rightkit_approvals::eyes` approval-record check.
fn approval_record_bound(raw: &Value, current: Option<&str>) -> bool {
    let Some(record) = raw.get("approval").and_then(Value::as_object) else {
        return false;
    };
    let text = |key: &str| {
        record
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };
    let input_hash_ok = text("input_hash")
        .is_some_and(|h| h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit()));
    input_hash_ok
        && text("channel").is_some()
        && text("nonce").is_some()
        && text("approved_at").is_some()
        && matches!((text("legacy_hash"), current), (Some(bound), Some(now)) if bound == now)
}

/// `sha256:` plus the first 16 hex digits of the file's bytes, the form the eyes gate records.
/// Must stay identical to `rightkit_approvals::eyes::file_hash`. `None` when the file cannot be read.
fn file_hash(path: &Path) -> Option<String> {
    let hex = rightkit_fs::digest::sha256_file(path).ok()?;
    Some(format!("sha256:{}", &hex[..16]))
}

fn artifact_json(path: &Path, kind: ArtifactKind, visibility: &str) -> Value {
    let absolute_path = path.to_string_lossy().replace('\\', "/");
    let project_slug = project_slug_of(path);
    let brand = infer_brand(&absolute_path);
    let artifact_id = format!(
        "{}:{}",
        kind.as_str(),
        absolute_path.replace(':', "").replace('/', "_")
    );
    let mut artifact = json!({
        "schema_version": "1.0.0",
        "artifact_id": artifact_id,
        "artifact_type": kind.as_str(),
        "absolute_path": absolute_path,
        "project_slug": project_slug,
        "brand": brand,
        "visibility": visibility,
        "metadata_status": "incomplete",
        "approval": {
            "eyes": "unknown",
            "auto_jury": "unknown",
            "overall": "draft"
        },
        "generation": {
            "provider": null,
            "model": null,
            "chat_tool_version": null,
            "prompt": null,
            "aspect": null,
            "cost_usd": null
        },
        "sidecars": {
            "eyes_path": null,
            "sidecar_path": null,
            "verdict_path": null,
            "manifest_path": null
        },
        "diagnostics": ["indexed by Rust sidecar without legacy sidecar metadata"],
        "raw_sidecars": {},
        "indexed_at": chrono_like_now()
    });
    if kind == ArtifactKind::Image {
        if let Some((sidecar, sidecar_path)) = local_image_sidecar(path) {
            let generation = &mut artifact["generation"];
            for key in ["provider", "model", "prompt", "aspect", "cost_usd"] {
                generation[key] = sidecar.get(key).cloned().unwrap_or(Value::Null);
            }
            let complete = ["provider", "model", "prompt", "aspect", "cost_usd"]
                .iter()
                .all(|key| sidecar.get(key).map(sidecar_value_present).unwrap_or(false));
            artifact["metadata_status"] = json!(if complete { "complete" } else { "incomplete" });
            artifact["sidecars"]["sidecar_path"] = json!(sidecar_path);
            artifact["raw_sidecars"]["sidecar"] = sidecar;
            let manifest = path.parent().map(|parent| parent.join("run-manifest.json"));
            if let Some(manifest) = manifest.filter(|manifest| manifest.is_file()) {
                artifact["sidecars"]["manifest_path"] =
                    json!(manifest.to_string_lossy().replace('\\', "/"));
            }
            if complete {
                artifact["diagnostics"] = json!([]);
            }
        }
    }
    apply_asset_facts(path, &mut artifact);
    apply_review_sidecars(path, &mut artifact);
    let caller = caller_of(path);
    artifact["caller"] = json!(caller.as_ref().map(|(slug, _)| slug));
    artifact["caller_label"] = json!(caller.as_ref().map(|(_, label)| label));
    artifact
}

/// The unknown facts an asset without a recorded `facts.json` reads as (the shape of
/// `rightkit_media_gen::host::asset_facts::AssetFacts::unknown()`, which this crate does not depend on).
fn unknown_asset_facts() -> Value {
    json!({
        "origin": {"kind": "unknown", "provider": null, "model": null, "job_id": null, "source": null, "parents": []},
        "evidence": "unknown",
        "rights": {"status": "unknown", "licence_evidence": [], "source": null, "attribution": null,
                   "permitted_uses": [], "consent_refs": []},
    })
}

/// Largest facts file the gallery reads (a folder `facts.json` or a `<file>.facts.json` sidecar).
const MAX_FACTS_BYTES: u64 = 256 * 1024;

/// Origin, evidentiary meaning, rights and scope for an asset. A folder `facts.json` beside the file
/// (the library's asset folder) covers it when it is present and valid. Otherwise the file's own
/// `<file>.facts.json` sidecar is read, the one `rightkit_media_gen::provenance::write_sibling_facts`
/// writes beside a file that is not in the library. A file with neither (older assets, plain
/// gallery files) reads as explicit `unknown`. Sidecars are JSON, so `classify` never lists them.
fn apply_asset_facts(path: &Path, artifact: &mut Value) {
    let recorded = read_facts_file(&path.with_file_name("facts.json"))
        .or_else(|| read_facts_file(&facts_sidecar_path(path)));
    match recorded {
        Some(v) => {
            artifact["asset_facts"] = v["facts"].clone();
            artifact["scope"] = v.get("scope").cloned().unwrap_or(Value::Null);
        }
        None => {
            artifact["asset_facts"] = unknown_asset_facts();
            artifact["scope"] = Value::Null;
        }
    }
}

/// A facts file that is a regular file within the size limit and holds a `facts` object.
fn read_facts_file(path: &Path) -> Option<Value> {
    path.metadata()
        .ok()
        .filter(|m| m.is_file() && m.len() <= MAX_FACTS_BYTES)?;
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .filter(|v| v.get("facts").is_some_and(Value::is_object))
}

/// `<file>.facts.json` beside `path`: the file name with `.facts.json` appended.
fn facts_sidecar_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".facts.json");
    path.with_file_name(name)
}

// ---- Brand codes (host-registered catalogue) ----

static BRAND_CATALOG: std::sync::OnceLock<Value> = std::sync::OnceLock::new();

/// Register the host's brand catalogue once (`{default_code?, brands:[{code, name, segments:
/// [[seg, ...]], segment_prefixes:[...]}]}`). Returns the rejected value when one is set.
pub fn set_brand_catalog(catalog: Value) -> Result<(), Value> {
    BRAND_CATALOG.set(catalog)
}

/// The registered brand catalogue, or an empty one (`default_code` `NA`, no brands).
pub fn brand_catalog() -> &'static Value {
    static EMPTY: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    BRAND_CATALOG.get().unwrap_or_else(|| {
        EMPTY.get_or_init(|| serde_json::json!({"default_code": "NA", "brands": []}))
    })
}

/// Brand code for a file location against the registered catalogue (see
/// [`infer_brand_code_with`]).
pub fn infer_brand_code(path: &str) -> String {
    infer_brand_code_with(path, brand_catalog())
}

/// Brand code for a file location: first catalogue brand whose consecutive path-segment
/// sequence (or segment prefix) matches; the catalogue's `default_code` (`NA`) when none does.
pub fn infer_brand_code_with(path: &str, catalog: &Value) -> String {
    let lower = path.replace('\\', "/").to_ascii_lowercase();
    let segs: Vec<&str> = lower.split('/').filter(|s| !s.is_empty()).collect();
    let default = catalog["default_code"].as_str().unwrap_or("NA");
    for brand in catalog["brands"].as_array().into_iter().flatten() {
        let code = brand["code"].as_str().unwrap_or_default();
        let by_sequence = brand["segments"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|seq| {
                let seq: Vec<&str> = seq
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                !seq.is_empty() && segs.windows(seq.len()).any(|w| w == seq.as_slice())
            });
        let by_prefix = brand["segment_prefixes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .any(|p| segs.iter().any(|s| s.starts_with(p)));
        if by_sequence || by_prefix {
            return code.to_string();
        }
    }
    default.to_string()
}

// ---- Eyes / jury / extra sidecars (baseline server/sidecar-parsers.mjs) ----

const MAX_SIDECAR_BYTES: u64 = 1024 * 1024;

fn read_json_bounded(path: &Path) -> Option<Result<Value, String>> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    if meta.len() > MAX_SIDECAR_BYTES {
        return Some(Err("sidecar larger than 1 MiB".to_string()));
    }
    let bytes = fs::read(path).ok()?;
    Some(serde_json::from_slice::<Value>(&bytes).map_err(|e| e.to_string()))
}

fn first_str(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| {
        value
            .get(*k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    })
}

fn first_num(value: &Value, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|k| match value.get(*k)? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    })
}

/// Baseline `normalizeEyesStatus`: APPROVE(D)/REJECT(ED)/REWORK/PENDING.
pub fn normalize_eyes_status(raw: &str) -> String {
    match raw.trim().to_ascii_uppercase().as_str() {
        "APPROVE" | "APPROVED" => "approved".into(),
        "REJECT" | "REJECTED" => "rejected".into(),
        "REWORK" => "rework".into(),
        "PENDING" => "pending".into(),
        _ => raw.trim().to_string(),
    }
}

/// Baseline `applyApproval`: eyes/jury/overall from one sidecar value.
fn apply_approval(artifact: &mut Value, value: &Value, allow_bare_status: bool) {
    let eyes_keys: &[&str] = if allow_bare_status {
        &["eyes", "eyes_status", "verdict", "status"]
    } else {
        &["eyes", "eyes_status"]
    };
    let eyes = first_str(value, eyes_keys);
    let jury = first_str(value, &["auto_jury", "auto_jury_status", "jury_status"]);
    let overall = first_str(value, &["overall", "approval", "state"]);
    let approval = &mut artifact["approval"];
    if let Some(e) = eyes {
        approval["eyes"] = json!(normalize_eyes_status(&e));
    }
    if let Some(j) = jury {
        approval["auto_jury"] = json!(j);
    }
    if let Some(o) = overall {
        approval["overall"] = json!(o);
    } else {
        match approval["eyes"].as_str().unwrap_or("") {
            "approved" => approval["overall"] = json!("approved"),
            "pending" | "rework" => approval["overall"] = json!("needs_review"),
            "rejected" => approval["overall"] = json!("rejected"),
            _ => {}
        }
    }
}

fn apply_review_sidecars(path: &Path, artifact: &mut Value) {
    let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
        return;
    };
    let with_suffix = |suffix: &str| path.with_file_name(format!("{file_name}{suffix}"));
    let norm = |p: &Path| p.to_string_lossy().replace('\\', "/");
    let mut malformed = false;
    let mut diagnostics: Vec<Value> = Vec::new();

    // Eyes sidecar: `<asset>.eyes.json`.
    let eyes_path = with_suffix(".eyes.json");
    if let Some(parsed) = read_json_bounded(&eyes_path) {
        artifact["sidecars"]["eyes_path"] = json!(norm(&eyes_path));
        match parsed {
            Ok(v) => {
                apply_approval(artifact, &v, true);
                artifact["raw_sidecars"]["eyes"] = v;
            }
            Err(e) => {
                malformed = true;
                diagnostics.push(json!({"severity": "error", "path": norm(&eyes_path), "message": format!("Malformed JSON: {e}")}));
            }
        }
    }

    // Jury verdict: `<dir>/jury/<name>.verdict.json`.
    let verdict_path = path
        .with_file_name("jury")
        .join(format!("{file_name}.verdict.json"));
    if let Some(parsed) = read_json_bounded(&verdict_path) {
        artifact["sidecars"]["verdict_path"] = json!(norm(&verdict_path));
        match parsed {
            Ok(v) => {
                apply_approval(artifact, &v, false);
                // A verdict without an explicit jury key still reports its status.
                if artifact["approval"]["auto_jury"] == "unknown" {
                    if let Some(s) = first_str(&v, &["verdict", "status", "result"]) {
                        artifact["approval"]["auto_jury"] = json!(normalize_jury_status(&s));
                    }
                }
                artifact["raw_sidecars"]["verdict"] = v;
            }
            Err(e) => {
                malformed = true;
                diagnostics.push(json!({"severity": "error", "path": norm(&verdict_path), "message": format!("Malformed JSON: {e}")}));
            }
        }
    }

    // Provenance sidecars fill generation gaps (never overwrite): `.ai.json`, `.chat-image.json`,
    // and `.sidecar.json` for non-image media.
    let mut parsed_any = false;
    for (label, suffix) in [
        ("sidecar", ".sidecar.json"),
        ("ai", ".ai.json"),
        ("chat_image", ".chat-image.json"),
    ] {
        let p = with_suffix(suffix);
        if label == "sidecar" && artifact["raw_sidecars"].get("sidecar").is_some() {
            continue;
        }
        let Some(parsed) = read_json_bounded(&p) else {
            continue;
        };
        if artifact["sidecars"]["sidecar_path"].is_null() {
            artifact["sidecars"]["sidecar_path"] = json!(norm(&p));
        }
        match parsed {
            Ok(v) => {
                parsed_any = true;
                let g = &mut artifact["generation"];
                let fills: [(&str, Option<Value>); 6] = [
                    (
                        "provider",
                        first_str(&v, &["provider", "source_tool", "source", "backend"])
                            .map(Value::from),
                    ),
                    (
                        "model",
                        first_str(&v, &["model", "model_label", "generation_model"])
                            .map(Value::from),
                    ),
                    (
                        "chat_tool_version",
                        first_str(&v, &["chat_tool_version", "tool_version"]).map(Value::from),
                    ),
                    (
                        "prompt",
                        first_str(
                            &v,
                            &["prompt", "final_prompt", "image_prompt", "description"],
                        )
                        .map(Value::from),
                    ),
                    (
                        "aspect",
                        first_str(&v, &["aspect", "aspect_ratio", "size"]).map(Value::from),
                    ),
                    (
                        "cost_usd",
                        first_num(&v, &["cost_usd", "cost", "usd"]).map(Value::from),
                    ),
                ];
                for (key, val) in fills {
                    if let Some(val) = val {
                        if g[key].is_null() {
                            g[key] = val;
                        }
                    }
                }
                apply_approval(artifact, &v, false);
                artifact["raw_sidecars"][label] = v;
            }
            Err(e) => {
                malformed = true;
                diagnostics.push(json!({"severity": "error", "path": norm(&p), "message": format!("Malformed JSON: {e}")}));
            }
        }
    }

    if malformed {
        artifact["metadata_status"] = json!("malformed");
    } else if parsed_any && artifact["metadata_status"] == "incomplete" {
        artifact["metadata_status"] = json!("complete");
    }
    if parsed_any && !malformed {
        let only_default = artifact["diagnostics"]
            .as_array()
            .map(|d| {
                d.len() == 1
                    && d[0]
                        .as_str()
                        .map(|s| s.starts_with("indexed by Rust sidecar"))
                        .unwrap_or(false)
            })
            .unwrap_or(false);
        if only_default {
            artifact["diagnostics"] = json!([]);
        }
    }
    if !diagnostics.is_empty() {
        if let Some(arr) = artifact["diagnostics"].as_array_mut() {
            arr.extend(diagnostics);
        }
    }
}

/// Jury verdicts: pass/blocked/warn spelled several ways in baseline sidecars.
fn normalize_jury_status(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "pass" | "passed" | "ok" => "passed".into(),
        "block" | "blocked" | "fail" | "failed" => "blocked".into(),
        "warn" | "warn_only" | "warning" => "warn_only".into(),
        "pending" => "pending".into(),
        other => other.to_string(),
    }
}

fn sidecar_value_present(value: &Value) -> bool {
    match value {
        Value::String(text) => !text.trim().is_empty(),
        Value::Number(_) => true,
        _ => false,
    }
}

fn local_image_sidecar(path: &Path) -> Option<(Value, String)> {
    const MAX_SIDECAR_BYTES: u64 = 1024 * 1024;
    let file_name = path.file_name()?.to_str()?;
    let sidecar_path = path.with_file_name(format!("{file_name}.sidecar.json"));
    let metadata = fs::metadata(&sidecar_path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_SIDECAR_BYTES {
        return None;
    }
    let mut file = fs::File::open(&sidecar_path).ok()?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.by_ref()
        .take(MAX_SIDECAR_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_SIDECAR_BYTES {
        return None;
    }
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    if !value.is_object() {
        return None;
    }
    Some((value, sidecar_path.to_string_lossy().replace('\\', "/")))
}

fn infer_brand(path: &str) -> String {
    infer_brand_code(path)
}

fn chrono_like_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{seconds}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn classify_common_media_types() {
        assert_eq!(classify(Path::new("x.png")), ArtifactKind::Image);
        assert_eq!(classify(Path::new("x.mp4")), ArtifactKind::Video);
        assert_eq!(classify(Path::new("x.wav")), ArtifactKind::Audio);
        assert_eq!(classify(Path::new("x.txt")), ArtifactKind::Unknown);
    }

    #[test]
    fn file_sidecar_facts_are_read_and_the_sidecar_is_never_listed() {
        let root = std::env::temp_dir().join(format!(
            "rightkit-library-sidecar-facts-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("shot.png"), b"x").unwrap();
        fs::write(
            root.join("shot.png.facts.json"),
            json!({
                "facts": {
                    "origin": {"kind": "supplied", "provider": null, "model": null, "job_id": null, "source": "/in/shot.png", "parents": []},
                    "evidence": "authentic",
                    "rights": {"status": "unknown", "licence_evidence": [], "source": null, "attribution": null, "permitted_uses": [], "consent_refs": ["clone-consent:local:a"]}
                },
                "scope": null
            })
            .to_string(),
        )
        .unwrap();
        let response = index_roots(
            &[root.to_string_lossy().to_string()],
            None,
            None,
            None,
            true,
            &IndexFilter::default(),
        )
        .unwrap();
        assert_eq!(
            response.artifacts.len(),
            1,
            "the sidecar is not an artifact: {:?}",
            response.artifacts
        );
        let facts = &response.artifacts[0]["asset_facts"];
        assert_eq!(facts["origin"]["kind"], "supplied", "{facts}");
        assert_eq!(facts["evidence"], "authentic");
        assert_eq!(
            facts["rights"]["status"], "unknown",
            "a sidecar never clears rights"
        );
        assert_eq!(
            facts["rights"]["consent_refs"],
            json!(["clone-consent:local:a"])
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn indexes_fixture_media() {
        let root =
            std::env::temp_dir().join(format!("rightkit-library-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("project")).unwrap();
        fs::write(root.join("project").join("image.png"), b"x").unwrap();
        fs::create_dir_all(root.join(".staging-job-1")).unwrap();
        fs::write(root.join(".staging-job-1").join("image.png"), b"x").unwrap();
        let response = index_roots(
            &[root.to_string_lossy().to_string()],
            None,
            None,
            None,
            false,
            &IndexFilter::default(),
        )
        .unwrap();
        assert_eq!(response.artifacts.len(), 1);
        assert_eq!(response.artifacts[0]["artifact_type"], "image");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn local_image_sidecar_completes_generation_metadata_without_approving_artifact() {
        let root = std::env::temp_dir().join(format!(
            "rightkit-library-local-sidecar-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let job_dir = root.join("conversation-1").join("job-1");
        fs::create_dir_all(&job_dir).unwrap();
        let image = job_dir.join("image.png");
        fs::write(&image, b"test image").unwrap();
        fs::write(
            job_dir.join("image.png.sidecar.json"),
            serde_json::to_vec(&json!({
                "provider": "sdcpp-local",
                "model": "profile-x",
                "prompt": "a cat",
                "aspect": "8x8",
                "cost_usd": 0
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(job_dir.join("run-manifest.json"), b"{} ").unwrap();

        let response = index_roots(
            &[root.to_string_lossy().to_string()],
            None,
            None,
            None,
            false,
            &IndexFilter::default(),
        )
        .unwrap();
        assert_eq!(response.artifacts.len(), 1);
        let artifact = &response.artifacts[0];
        assert_eq!(artifact["metadata_status"], "complete");
        assert_eq!(artifact["generation"]["provider"], "sdcpp-local");
        assert_eq!(artifact["generation"]["model"], "profile-x");
        assert_eq!(artifact["generation"]["prompt"], "a cat");
        assert_eq!(artifact["generation"]["aspect"], "8x8");
        assert_eq!(artifact["generation"]["cost_usd"], 0);
        assert!(artifact["sidecars"]["sidecar_path"]
            .as_str()
            .unwrap()
            .ends_with("image.png.sidecar.json"));
        assert!(artifact["sidecars"]["manifest_path"]
            .as_str()
            .unwrap()
            .ends_with("run-manifest.json"));
        assert_eq!(artifact["approval"]["eyes"], "unknown");
        assert_eq!(artifact["approval"]["overall"], "draft");
        assert_eq!(response.report.metadata_incomplete_count, 0);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn infers_brand_codes_from_location() {
        let catalog = json!({
            "default_code": "NA",
            "brands": [
                {"code": "AB", "segments": [["content", "alpha"]]},
                {"code": "BK", "segment_prefixes": ["book-"]},
            ],
        });
        for (path, code) in [
            ("D:/Work/Content/alpha/hero.png", "AB"),
            ("/home/u/out/book-3/p.png", "BK"),
            ("/home/u/out/misc/p.png", "NA"),
        ] {
            assert_eq!(infer_brand_code_with(path, &catalog), code, "{path}");
        }
        assert_eq!(
            infer_brand_code_with("D:\\Work\\Content\\alpha\\a.png", &catalog),
            "AB"
        );
        assert_eq!(infer_brand_code("/anything/at/all.png"), "NA");
    }

    #[test]
    fn eyes_and_jury_sidecars_set_approval() {
        let root =
            std::env::temp_dir().join(format!("rightkit-library-review-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let dir = root.join("Content").join("damned").join("shoot");
        fs::create_dir_all(dir.join("jury")).unwrap();
        for n in ["a.png", "b.png", "c.png", "d.png"] {
            fs::write(dir.join(n), b"x").unwrap();
        }
        fs::write(
            dir.join("a.png.eyes.json"),
            approved_body(&dir.join("a.png")).to_string(),
        )
        .unwrap();
        fs::write(
            dir.join("jury").join("a.png.verdict.json"),
            br#"{"auto_jury":"passed"}"#,
        )
        .unwrap();
        fs::write(dir.join("b.png.eyes.json"), br#"{"eyes_status":"REWORK"}"#).unwrap();
        fs::write(
            dir.join("jury").join("b.png.verdict.json"),
            br#"{"verdict":"BLOCKED"}"#,
        )
        .unwrap();
        fs::write(dir.join("c.png.eyes.json"), b"{not json").unwrap();
        fs::write(
            dir.join("d.png.ai.json"),
            br#"{"model":"flux","prompt":"p","cost":"0.04"}"#,
        )
        .unwrap();
        let r = index_roots(
            &[root.to_string_lossy().to_string()],
            None,
            None,
            None,
            false,
            &IndexFilter::default(),
        )
        .unwrap();
        let by = |n: &str| {
            r.artifacts
                .iter()
                .find(|a| a["absolute_path"].as_str().unwrap().ends_with(n))
                .unwrap()
                .clone()
        };
        let a = by("a.png");
        assert_eq!(a["brand"], "DD");
        assert_eq!(a["approval"]["eyes"], "approved");
        assert_eq!(a["approval"]["auto_jury"], "passed");
        assert_eq!(a["approval"]["overall"], "approved");
        assert!(a["sidecars"]["eyes_path"]
            .as_str()
            .unwrap()
            .ends_with("a.png.eyes.json"));
        assert!(a["sidecars"]["verdict_path"]
            .as_str()
            .unwrap()
            .ends_with("a.png.verdict.json"));
        let b = by("b.png");
        assert_eq!(b["approval"]["eyes"], "rework");
        assert_eq!(b["approval"]["auto_jury"], "blocked");
        assert_eq!(b["approval"]["overall"], "needs_review");
        let c = by("c.png");
        assert_eq!(c["metadata_status"], "malformed");
        assert_eq!(c["approval"]["eyes"], "unknown");
        assert!(c["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["message"].as_str().unwrap_or("").contains("Malformed")));
        let d = by("d.png");
        assert_eq!(d["metadata_status"], "complete");
        assert_eq!(d["generation"]["model"], "flux");
        assert_eq!(d["generation"]["cost_usd"], 0.04);
        let _ = fs::remove_dir_all(root);
    }

    /// Four files: two approved DD knife-launch shots (one jury passed, one blocked), an
    /// unreviewed DD bead-set shot, and a reworked RH founder-story shot.
    fn filter_library(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "rightkit-library-filter-{label}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let knife = root.join("Content").join("damned").join("knife-launch");
        let bead = root.join("Content").join("damned").join("bead-set");
        let rh = root.join("Content").join("RH").join("founder-story");
        for dir in [&knife, &bead, &rh] {
            fs::create_dir_all(dir.join("jury")).unwrap();
        }
        for (dir, name) in [
            (&knife, "a1.png"),
            (&knife, "a2.png"),
            (&bead, "b1.png"),
            (&rh, "d1.png"),
        ] {
            fs::write(dir.join(name), b"x").unwrap();
        }
        fs::write(
            knife.join("a1.png.eyes.json"),
            approved_body(&knife.join("a1.png")).to_string(),
        )
        .unwrap();
        fs::write(
            knife.join("jury").join("a1.png.verdict.json"),
            br#"{"auto_jury":"passed"}"#,
        )
        .unwrap();
        fs::write(
            knife.join("a2.png.eyes.json"),
            approved_body(&knife.join("a2.png")).to_string(),
        )
        .unwrap();
        fs::write(
            knife.join("jury").join("a2.png.verdict.json"),
            br#"{"verdict":"BLOCKED"}"#,
        )
        .unwrap();
        fs::write(rh.join("d1.png.eyes.json"), br#"{"eyes_status":"REWORK"}"#).unwrap();
        root
    }

    fn query(
        root: &Path,
        offset: Option<usize>,
        limit: Option<usize>,
        filter: &IndexFilter,
    ) -> GalleryResponse {
        index_roots(
            &[root.to_string_lossy().to_string()],
            offset,
            limit,
            None,
            false,
            filter,
        )
        .unwrap()
    }

    /// File names of a response's artifacts, sorted.
    fn names(response: &GalleryResponse) -> Vec<String> {
        let mut names: Vec<String> = response
            .artifacts
            .iter()
            .map(|a| {
                a["absolute_path"]
                    .as_str()
                    .unwrap()
                    .rsplit('/')
                    .next()
                    .unwrap()
                    .to_string()
            })
            .collect();
        names.sort();
        names
    }

    fn with(
        project: Option<&str>,
        brand: Option<&str>,
        eyes: Option<&str>,
        jury: Option<&str>,
    ) -> IndexFilter {
        IndexFilter {
            project: project.map(str::to_string),
            no_project: false,
            brand: brand.map(str::to_string),
            eyes: eyes.map(str::to_string),
            jury: jury.map(str::to_string),
            caller: None,
        }
    }

    #[test]
    fn caller_sidecar_tags_the_listing_and_filters_it() {
        let root =
            std::env::temp_dir().join(format!("rightkit-library-caller-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let dir = root.join("conv").join("job");
        fs::create_dir_all(&dir).unwrap();
        for name in ["tagged.png", "plain.png", "other.png"] {
            fs::write(dir.join(name), name.as_bytes()).unwrap();
        }
        fs::write(
            dir.join("tagged.png.caller.json"),
            br#"{"caller":"postright","label":"PostRight"}"#,
        )
        .unwrap();
        fs::write(
            dir.join("other.png.caller.json"),
            br#"{"caller":"cutright","label":"CutRight"}"#,
        )
        .unwrap();
        let by = |caller: Option<&str>| {
            let filter = IndexFilter {
                caller: caller.map(str::to_string),
                ..IndexFilter::default()
            };
            names(&query(&root, None, None, &filter))
        };
        assert_eq!(by(None), ["other.png", "plain.png", "tagged.png"]);
        assert_eq!(by(Some("postright")), ["tagged.png"]);
        assert_eq!(by(Some("cutright")), ["other.png"]);
        assert!(by(Some("nobody")).is_empty());
        let all = query(&root, None, None, &IndexFilter::default());
        let tagged = all
            .artifacts
            .iter()
            .find(|a| a["absolute_path"].as_str().unwrap().ends_with("tagged.png"))
            .unwrap();
        assert_eq!(
            (tagged["caller"].as_str(), tagged["caller_label"].as_str()),
            (Some("postright"), Some("PostRight"))
        );
        let plain = all
            .artifacts
            .iter()
            .find(|a| a["absolute_path"].as_str().unwrap().ends_with("plain.png"))
            .unwrap();
        assert!(plain["caller"].is_null());
        assert!(IndexFilter {
            caller: Some("  ".into()),
            ..IndexFilter::default()
        }
        .validate()
        .is_err());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn filters_by_each_field_alone() {
        let root = filter_library("alone");
        let by = |f: IndexFilter| names(&query(&root, None, None, &f));
        assert_eq!(
            by(with(Some("knife-launch"), None, None, None)),
            ["a1.png", "a2.png"]
        );
        assert_eq!(
            by(with(None, Some("dd"), None, None)),
            ["a1.png", "a2.png", "b1.png"],
            "brand is case-insensitive"
        );
        assert_eq!(by(with(None, Some("RH"), None, None)), ["d1.png"]);
        assert_eq!(
            by(with(None, None, Some("approved"), None)),
            ["a1.png", "a2.png"]
        );
        assert_eq!(by(with(None, None, Some("rework"), None)), ["d1.png"]);
        assert_eq!(by(with(None, None, Some("unknown"), None)), ["b1.png"]);
        assert_eq!(by(with(None, None, None, Some("passed"))), ["a1.png"]);
        assert_eq!(by(with(None, None, None, Some("blocked"))), ["a2.png"]);
        assert_eq!(
            by(with(None, None, None, Some("unknown"))),
            ["b1.png", "d1.png"]
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn combines_filters_and_returns_empty_when_none_match() {
        let root = filter_library("combined");
        let by = |f: IndexFilter| names(&query(&root, None, None, &f));
        assert_eq!(
            by(with(None, Some("DD"), Some("approved"), None)),
            ["a1.png", "a2.png"]
        );
        assert_eq!(
            by(with(Some("knife-launch"), None, None, Some("blocked"))),
            ["a2.png"]
        );
        assert_eq!(
            by(with(None, None, Some("approved"), Some("passed"))),
            ["a1.png"]
        );
        assert!(
            by(with(None, Some("RH"), Some("approved"), None)).is_empty(),
            "no match is an empty page, not an error"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn paging_walks_the_filtered_set() {
        let root = filter_library("paging");
        let brand_dd = with(None, Some("DD"), None, None);
        let first = query(&root, Some(0), Some(2), &brand_dd);
        let second = query(&root, Some(2), Some(2), &brand_dd);
        assert_eq!(first.artifacts.len(), 2);
        assert_eq!(
            first.report.total_available, 3,
            "total counts the filtered set"
        );
        assert!(first.report.has_more);
        assert_eq!(second.artifacts.len(), 1);
        assert!(!second.report.has_more);
        let mut seen: Vec<String> = names(&first);
        seen.extend(names(&second));
        seen.sort();
        assert_eq!(
            seen,
            ["a1.png", "a2.png", "b1.png"],
            "pages cover the filtered set once each"
        );
        // Review filters page over the built records the same way.
        let approved = with(None, None, Some("approved"), None);
        let page = query(&root, Some(1), Some(5), &approved);
        assert_eq!(page.artifacts.len(), 1);
        assert_eq!(page.report.total_available, 2);
        assert!(!page.report.has_more);
        assert!(query(&root, Some(9), Some(5), &approved)
            .artifacts
            .is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_unknown_filter_values_before_walking() {
        let root = filter_library("invalid");
        let root_arg = [root.to_string_lossy().to_string()];
        let reject =
            |f: IndexFilter| index_roots(&root_arg, None, None, None, false, &f).unwrap_err();
        let message = reject(with(None, None, Some("maybe"), None));
        assert!(
            message.contains("approved") && message.contains("rework"),
            "names the allowed eyes values: {message}"
        );
        let message = reject(with(None, None, None, Some("PASSED")));
        assert!(
            message.contains("passed") && message.contains("warn_only"),
            "names the allowed jury values: {message}"
        );
        assert!(reject(with(Some(""), None, None, None)).contains("project"));
        assert!(reject(with(None, Some("  "), None, None)).contains("brand"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn no_filter_keeps_newest_first_paging() {
        let root = filter_library("unfiltered");
        let whole = query(&root, None, None, &IndexFilter::default());
        assert_eq!(whole.report.total_available, 4);
        let paged: Vec<String> = [(0, 1), (1, 1), (2, 1), (3, 1)]
            .iter()
            .flat_map(|(offset, limit)| {
                query(&root, Some(*offset), Some(*limit), &IndexFilter::default()).artifacts
            })
            .map(|a| a["absolute_path"].as_str().unwrap().to_string())
            .collect();
        let whole_paths: Vec<String> = whole
            .artifacts
            .iter()
            .map(|a| a["absolute_path"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            paged, whole_paths,
            "one-item pages reproduce the unfiltered order"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// An eyes sidecar that approves `file` as the engine's gate writes it: the verdict, the hash of
    /// the bytes now, and an `approval` record bound to those bytes.
    fn approved_body(file: &Path) -> Value {
        let hash = file_hash(file).unwrap();
        json!({
            "verdict": "APPROVE",
            "hash": hash,
            "approval": {
                "channel": "test_secret",
                "nonce": "00",
                "approved_at": "2026-10-16T00:00:00.000Z",
                "input_hash": "0".repeat(64),
                "legacy_hash": hash,
            },
        })
    }

    /// Writes an eyes sidecar approving `name` in `dir`. With a hash the approval is the full record
    /// of [`approved_body`]; without one it is a bare verdict, which the Gallery must not trust.
    fn approve_eyes(dir: &Path, name: &str, with_record: bool) {
        let file = dir.join(name);
        let body = if with_record {
            approved_body(&file)
        } else {
            json!({"verdict": "APPROVE"})
        };
        fs::write(dir.join(format!("{name}.eyes.json")), body.to_string()).unwrap();
    }

    #[test]
    fn approval_goes_stale_when_the_approved_bytes_change() {
        let root =
            std::env::temp_dir().join(format!("rightkit-library-stale-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let dir = root.join("Content").join("damned").join("shoot");
        fs::create_dir_all(&dir).unwrap();
        for n in ["kept.png", "edited.png", "unhashed.png"] {
            fs::write(dir.join(n), b"first bytes").unwrap();
        }
        approve_eyes(&dir, "kept.png", true);
        approve_eyes(&dir, "edited.png", true);
        approve_eyes(&dir, "unhashed.png", false);
        // Edit the approved file after approval; a bare verdict with no record never reads approved.
        fs::write(dir.join("edited.png"), b"second bytes").unwrap();
        fs::write(dir.join("unhashed.png"), b"second bytes").unwrap();

        let root_arg = [root.to_string_lossy().to_string()];
        let index = |f: IndexFilter| index_roots(&root_arg, None, None, None, false, &f).unwrap();
        let by = |f: IndexFilter| names(&index(f));
        assert_eq!(
            by(with(None, None, Some("approved"), None)),
            ["kept.png"],
            "only an unchanged, record-bound approval stays approved"
        );
        assert_eq!(
            by(with(None, None, Some("stale"), None)),
            ["edited.png"],
            "changed bytes make the approval stale"
        );
        assert_eq!(
            by(with(None, None, Some("pending"), None)),
            ["unhashed.png"],
            "a bare verdict is pending, not approved"
        );
        assert_eq!(
            by(with(None, None, None, None)),
            ["edited.png", "kept.png", "unhashed.png"]
        );

        let unfiltered = index(IndexFilter::default());
        let record = |name: &str| {
            unfiltered
                .artifacts
                .iter()
                .find(|a| a["absolute_path"].as_str().unwrap().ends_with(name))
                .unwrap()
                .clone()
        };
        assert_eq!(record("kept.png")["approval"]["eyes"], "approved");
        assert_eq!(record("kept.png")["approval"]["overall"], "approved");
        assert_eq!(record("edited.png")["approval"]["eyes"], "stale");
        assert_eq!(
            record("edited.png")["approval"]["overall"],
            "needs_review",
            "stale is not exportable"
        );
        assert_eq!(record("unhashed.png")["approval"]["eyes"], "pending");
        assert_eq!(
            record("unhashed.png")["approval"]["overall"],
            "needs_review"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn project_is_empty_for_library_assets_only() {
        let uuid = "3f2504e0-4f89-41d3-9a0c-0305e82c3301";
        let derived = "9c1d2e3f-4a5b-6c7d-8e9f-0a1b2c3d4e5f";
        let library = |id: &str, file: &str| PathBuf::from(format!("/data/library/{id}/{file}"));
        assert_eq!(
            project_slug_of(&library(uuid, &format!("{uuid}.png"))),
            "",
            "UUID asset folder"
        );
        assert_eq!(
            project_slug_of(&library(derived, &format!("{derived}.mp4"))),
            "",
            "deterministic asset folder"
        );
        // Same shape but a different file stem, uppercase hex, or a wrong group length: a project.
        assert_eq!(
            project_slug_of(&library(uuid, "render.png")),
            uuid,
            "file name is not the asset id"
        );
        assert_eq!(
            project_slug_of(&library(
                &uuid.to_uppercase(),
                &format!("{}.png", uuid.to_uppercase())
            )),
            uuid.to_uppercase(),
            "uppercase is not the id shape"
        );
        assert_eq!(
            project_slug_of(&library(
                "9c1d2e3f-4a5b-6c7d-8e9f-0a1b2c3d4e5",
                "9c1d2e3f-4a5b-6c7d-8e9f-0a1b2c3d4e5.png"
            )),
            "9c1d2e3f-4a5b-6c7d-8e9f-0a1b2c3d4e5",
            "last group too short"
        );
        // Any other layout keeps its parent folder name, even when the stem matches it.
        assert_eq!(
            project_slug_of(Path::new("/Content/damned/bead-set/bead-set.png")),
            "bead-set"
        );
        assert_eq!(
            project_slug_of(Path::new("/Content/damned/bead-set/a1.png")),
            "bead-set"
        );
        assert_eq!(
            project_slug_of(Path::new("/x.png")),
            "unknown",
            "no parent folder"
        );
    }

    #[test]
    fn library_asset_is_listed_once_when_its_root_overlaps() {
        let data = std::env::temp_dir().join(format!(
            "rightkit-library-library-root-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&data);
        let id = "3f2504e0-4f89-41d3-9a0c-0305e82c3301";
        let asset_dir = data.join("library").join(id);
        fs::create_dir_all(&asset_dir).unwrap();
        fs::write(asset_dir.join(format!("{id}.png")), b"x").unwrap();
        let library = data.join("library").to_string_lossy().to_string();
        let parent = data.to_string_lossy().to_string();
        for roots in [
            vec![library.clone()],
            vec![library.clone(), parent.clone()],
            vec![parent.clone(), library.clone()],
        ] {
            let response =
                index_roots(&roots, None, None, None, false, &IndexFilter::default()).unwrap();
            assert_eq!(
                response.report.total_available, 1,
                "{roots:?} lists the asset once"
            );
            assert_eq!(
                response.artifacts[0]["project_slug"], "",
                "library asset has no project"
            );
        }
        let _ = fs::remove_dir_all(data);
    }
}
