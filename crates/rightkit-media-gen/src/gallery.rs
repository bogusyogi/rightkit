//! The gallery index: GenRight's media listing over output folders, with each file's generation
//! metadata, asset facts, caller, brand and review state (eyes and jury sidecars).
//!
//! Moved from `rightkit-library` (where it sat as an undeclared, never-compiled `artifacts.rs`)
//! in 0.1.2. It reads the sidecars this crate writes: [`crate::host::CALLER_SIDECAR_SUFFIX`],
//! the library's `facts.json` and [`crate::provenance::sibling_facts_path`], the library's
//! `pair.json`, and the eyes record checked by [`crate::host::approval::eyes_record_bound`].
//! Results are JSON values, the shape the gallery front end reads.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::host::approval::{eyes_file_hash, eyes_record_bound};
use crate::host::asset_facts::AssetFacts;
use crate::host::CALLER_SIDECAR_SUFFIX;
use crate::library::PAIR_FILE;
use crate::provenance::sibling_facts_path;
use std::time::Instant;

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
/// listing for the image.
fn superseded_by_gallery_file(
    path: &Path,
    scanned: &[PathBuf],
    include_intermediate: bool,
) -> bool {
    let Some(dir) = path.parent() else {
        return false;
    };
    let Some(pair) = fs::read(dir.join(PAIR_FILE))
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
/// deterministic id this crate derives for adopted worker output (`adopt_path_asset`).
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
/// bytes ([`eyes_record_bound`]); the gate in `rightkit-media-gen-approvals` (`eyes::gate_state`) applies the same
/// function. A recorded hash that no longer matches reads `stale`; an APPROVE without a bound record reads
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
    let current = eyes_file_hash(Path::new(&path));
    let recorded = raw["hash"].as_str();
    let state = if recorded.is_some_and(|hash| current.as_deref() != Some(hash)) {
        "stale"
    } else if !eyes_record_bound(&raw, current.as_deref()) {
        "pending"
    } else {
        return;
    };
    artifact["approval"]["eyes"] = json!(state);
    if artifact["approval"]["overall"] == "approved" {
        artifact["approval"]["overall"] = json!("needs_review");
    }
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

/// Largest facts file the gallery reads (a folder `facts.json` or a `<file>.facts.json` sidecar).
const MAX_FACTS_BYTES: u64 = 256 * 1024;

/// Origin, evidentiary meaning, rights and scope for an asset. A folder `facts.json` beside the file
/// (the library's asset folder) covers it when it is present and valid. Otherwise the file's own
/// `<file>.facts.json` sidecar is read, the one [`crate::provenance::write_sibling_facts`]
/// writes beside a file that is not in the library. A file with neither (older assets, plain
/// gallery files) reads as explicit `unknown`. Sidecars are JSON, so `classify` never lists them.
fn apply_asset_facts(path: &Path, artifact: &mut Value) {
    let recorded = read_facts_file(&path.with_file_name("facts.json"))
        .or_else(|| read_facts_file(&sibling_facts_path(path)));
    match recorded {
        Some(v) => {
            artifact["asset_facts"] = v["facts"].clone();
            artifact["scope"] = v.get("scope").cloned().unwrap_or(Value::Null);
        }
        None => {
            artifact["asset_facts"] = AssetFacts::unknown().to_value();
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

/// `path` with `\\` read as `/`, `.` dropped and `..` popping a component, without touching the
/// filesystem. Roots arrive from the front end in either separator style.
fn normalize_lossy(path: impl AsRef<str>) -> PathBuf {
    let path = path.as_ref().replace('\\', "/");
    let source = Path::new(&path);
    let mut out = PathBuf::new();
    for component in source.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}
