//! Shared reader/importer for GenRight's generated-source shot manifest.
//!
//! The worker protocol owns this JSON shape. This module validates its paths,
//! identities and artifact hashes before returning project-owned media copies.

use std::fs;
use std::path::Path;

use rightkit_fs::digest::{blake3_file, sha256_file, sha256_file_with_len};
use rightkit_fs::{copy_verified_with_digest, AtomicWriteOptions, DigestAlgorithm};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RationalFps {
    pub num: u32,
    pub den: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeneratedRecipe {
    pub id: String,
    pub version: String,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeneratedSourceIdentity {
    pub path: String,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeneratedArtifact {
    pub id: String,
    pub path: String,
    pub mime: String,
    pub sha256: String,
    pub bytes: u64,
    pub role: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Regeneration {
    pub provider: String,
    pub model: String,
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeneratedShot {
    pub id: String,
    pub start_frame: u64,
    pub end_frame: u64,
    pub artifact_id: String,
    pub source_start_frame: u64,
    pub camera: serde_json::Value,
    pub lens_mm: f64,
    pub source_sha256: String,
    pub regen: Regeneration,
}

/// Human product finish approval attached to Blender generated output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProductReview {
    pub reviewed: bool,
    pub product_id: Option<String>,
    pub variant: Option<String>,
    pub confirmed: bool,
    pub source_sha256: String,
    pub finish_tags: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub approved_by: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeneratedSourceManifest {
    pub schema: u32,
    #[serde(rename = "type")]
    pub kind: String,
    pub job_id: String,
    pub worker_version: String,
    pub blender_version: String,
    pub recipe: GeneratedRecipe,
    pub source: GeneratedSourceIdentity,
    pub fps: RationalFps,
    pub artifacts: Vec<GeneratedArtifact>,
    pub shots: Vec<GeneratedShot>,
    /// Absent on legacy and non-Blender generated sources.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_review: Option<ProductReview>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportedGeneratedArtifact {
    pub artifact_id: String,
    pub role: String,
    pub media_rel: String,
    pub sha256: String,
    pub blake3: String,
    pub bytes: u64,
    pub mime: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportedGeneratedSource {
    pub manifest_sha256: String,
    pub source_binding_path: String,
    pub manifest: GeneratedSourceManifest,
    pub artifacts: Vec<ImportedGeneratedArtifact>,
}

/// Read, validate and verify every declared final artifact before importing any.
/// Copies land atomically under `<project>/media`; the journal is written last.
pub fn import_generated_source(
    project: &Path,
    manifest_path: &Path,
    source_override: Option<&Path>,
) -> Result<ImportedGeneratedSource> {
    let manifest_bytes = fs::read(manifest_path).map_err(|e| crate::error::io(manifest_path)(e))?;
    let manifest: GeneratedSourceManifest =
        serde_json::from_slice(&manifest_bytes).map_err(|e| Error::BadOutput {
            command: "generated-source manifest".into(),
            detail: e.to_string(),
        })?;
    validate_manifest(&manifest)?;
    let root = manifest_path.parent().unwrap_or(Path::new("."));
    let declared_source = Path::new(&manifest.source.path);
    let source_path = if let Some(path) = source_override {
        path.to_path_buf()
    } else if declared_source.is_absolute() {
        declared_source.to_path_buf()
    } else {
        root.join(declared_source)
    };
    let source_hash = sha256_file(&source_path).map_err(crate::error::io(&source_path))?;
    if source_hash != manifest.source.sha256.to_ascii_lowercase() {
        return Err(Error::BadOutput {
            command: "generated-source source".into(),
            detail: format!(
                "bound source failed sha256 verification: {}",
                source_path.display()
            ),
        });
    }
    let manifest_sha256 = rightkit_fs::digest::sha256_bytes(&manifest_bytes);
    let imports_dir = project.join("imports");
    fs::create_dir_all(&imports_dir).map_err(crate::error::io(&imports_dir))?;
    let record_path = imports_dir.join(format!(
        "generated-source-{}.json",
        safe_name(&manifest.job_id)
    ));
    if record_path.is_file() {
        let existing: ImportedGeneratedSource = serde_json::from_slice(
            &fs::read(&record_path).map_err(crate::error::io(&record_path))?,
        )
        .map_err(|e| Error::JournalCorrupt {
            path: record_path.clone(),
            detail: e.to_string(),
        })?;
        if existing.manifest_sha256 != manifest_sha256 {
            return Err(Error::RequestConflict {
                request_id: manifest.job_id.clone(),
            });
        }
    }
    let media_dir = project.join("media");
    fs::create_dir_all(&media_dir).map_err(crate::error::io(&media_dir))?;

    // Verify every source before placing any clip; earlier verified copies are
    // harmless and are reused if a later artifact is absent or corrupt.
    let mut imported = Vec::with_capacity(manifest.artifacts.len());
    for artifact in &manifest.artifacts {
        let source = rightkit_fs::contain(root, &artifact.path).map_err(|e| Error::BadOutput {
            command: "generated-source manifest".into(),
            detail: format!(
                "artifact path escapes worker output or is unavailable: {:?}: {e}",
                artifact.path
            ),
        })?;
        let ext = Path::new(&artifact.path)
            .extension()
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric()))
            .unwrap_or("bin")
            .to_ascii_lowercase();
        let basename = format!(
            "generated-{}-{}.{}",
            safe_name(&manifest.job_id),
            safe_name(&artifact.id),
            ext
        );
        let dest = media_dir.join(&basename);
        let hash = copy_or_verify(&source, &dest, &artifact.sha256, artifact.bytes)?;
        imported.push(ImportedGeneratedArtifact {
            artifact_id: artifact.id.clone(),
            role: artifact.role.clone(),
            media_rel: format!("media/{basename}"),
            sha256: hash.0,
            blake3: hash.1,
            bytes: hash.2,
            mime: artifact.mime.clone(),
        });
    }

    let result = ImportedGeneratedSource {
        manifest_sha256,
        source_binding_path: source_path.to_string_lossy().into_owned(),
        manifest,
        artifacts: imported,
    };
    let encoded = serde_json::to_vec_pretty(&result).map_err(|e| Error::BadOutput {
        command: "generated-source journal".into(),
        detail: e.to_string(),
    })?;
    rightkit_fs::atomic_write(&record_path, &encoded)
        .map_err(|e| crate::error::io(&record_path)(e.into()))?;
    Ok(result)
}

fn validate_manifest(m: &GeneratedSourceManifest) -> Result<()> {
    let bad = |detail: String| Error::BadOutput {
        command: "generated-source manifest".into(),
        detail,
    };
    if m.schema != 1 || m.kind != "generated_source" {
        return Err(bad("expected schema 1 generated_source manifest".into()));
    }
    if !valid_id(&m.job_id)
        || m.fps.num == 0
        || m.fps.den == 0
        || m.worker_version.is_empty()
        || m.blender_version.is_empty()
        || m.recipe.id.is_empty()
        || m.recipe.version.is_empty()
        || m.source.path.is_empty()
    {
        return Err(bad("invalid job_id or rational fps".into()));
    }
    if m.artifacts.is_empty() || m.shots.is_empty() {
        return Err(bad("manifest needs artifacts and shots".into()));
    }
    let blender_product = m.shots.iter().any(|s| {
        let provider = s.regen.provider.to_ascii_lowercase();
        provider.contains("blender") || provider.contains("renderright")
    });
    if blender_product {
        let Some(review) = &m.product_review else {
            return Err(bad("Blender product draft is not deliverable: product_review is missing; approve the product finish before delivery".into()));
        };
        if !review.reviewed || !review.confirmed {
            return Err(bad("Blender product draft is not deliverable: finish review and explicit confirmation are required".into()));
        }
        if review
            .product_id
            .as_deref()
            .is_none_or(|id| id.trim().is_empty())
            || review
                .variant
                .as_deref()
                .is_none_or(|id| id.trim().is_empty())
            || review
                .approved_by
                .as_deref()
                .is_none_or(|owner| owner.trim().is_empty())
            || review.finish_tags.is_empty()
            || review
                .finish_tags
                .iter()
                .any(|(id, version)| id.trim().is_empty() || version.trim().is_empty())
        {
            return Err(bad("Blender product draft is not deliverable: product id, variant, approver and finish tags must be nonempty".into()));
        }
        if !valid_sha(&review.source_sha256)
            || !review.source_sha256.eq_ignore_ascii_case(&m.source.sha256)
        {
            return Err(bad("Blender product draft is not deliverable: product review source hash does not match generated source".into()));
        }
    }
    let mut artifact_ids = std::collections::BTreeSet::new();
    for a in &m.artifacts {
        if !valid_id(&a.id) || !artifact_ids.insert(a.id.as_str()) {
            return Err(bad(format!("invalid or duplicate artifact id {:?}", a.id)));
        }
        if !matches!(a.role.as_str(), "movie" | "hero") || a.bytes == 0 || !valid_sha(&a.sha256) {
            return Err(bad(format!(
                "artifact {:?} must be a hashed movie or still",
                a.id
            )));
        }
        if a.path.is_empty() || a.mime.is_empty() {
            return Err(bad(format!("artifact {:?} has empty path or mime", a.id)));
        }
    }
    let mut shot_ids = std::collections::BTreeSet::new();
    let mut previous_end = 0;
    for (i, s) in m.shots.iter().enumerate() {
        if !valid_id(&s.id) || !shot_ids.insert(s.id.as_str()) || s.end_frame <= s.start_frame {
            return Err(bad(format!(
                "invalid or duplicate shot {:?} or empty frame range",
                s.id
            )));
        }
        if (i == 0 && s.start_frame != 0) || (i > 0 && s.start_frame != previous_end) {
            return Err(bad(
                "shots must form a contiguous timeline beginning at frame zero".into(),
            ));
        }
        if !artifact_ids.contains(s.artifact_id.as_str())
            || !s.source_sha256.eq_ignore_ascii_case(&m.source.sha256)
        {
            return Err(bad(format!(
                "shot {:?} has unknown artifact or source identity mismatch",
                s.id
            )));
        }
        if !s.lens_mm.is_finite()
            || s.lens_mm <= 0.0
            || s.regen.provider != "local-blender"
            || s.regen.model.is_empty()
        {
            return Err(bad(format!(
                "shot {:?} has invalid lens or regeneration identity",
                s.id
            )));
        }
        previous_end = s.end_frame;
    }
    if !valid_sha(&m.source.sha256) || !valid_sha(&m.recipe.sha256) {
        return Err(bad(
            "source and recipe sha256 values must be 64 hex characters".into(),
        ));
    }
    Ok(())
}

/// Validate a manifest received from a GenRight library descriptor.
pub fn validate_generated_source_manifest(m: &GeneratedSourceManifest) -> Result<()> {
    validate_manifest(m)
}

/// Points a journal record at a new bound-source path (a bundle import rebinds the source to
/// its copy inside the imported project). Nothing is verified here; callers run
/// [`verify_generated_source_binding`] on the new path.
pub fn rebind_source(record: &mut ImportedGeneratedSource, new_path: &Path) {
    record.source_binding_path = new_path.to_string_lossy().into_owned();
}

/// Validate the adopted GenRight assets against the embedded shot manifest.
/// Called before `job ack`, so an incomplete or mismatched bundle remains retryable.
pub fn validate_result_assets(records: &[crate::ImportRecord]) -> Result<()> {
    let Some(value) = records.iter().find_map(|record| {
        record
            .media
            .get("metadata")?
            .get("generated_source")
            .cloned()
    }) else {
        if records
            .iter()
            .any(|record| record.provider == "local-blender" && record.model == "blender-film")
        {
            return Err(Error::BadOutput {
                command: "generated-source result".into(),
                detail: "local-blender film result is missing generated_source shot manifest"
                    .into(),
            });
        }
        return Ok(());
    };
    let manifest: GeneratedSourceManifest =
        serde_json::from_value(value).map_err(|e| Error::BadOutput {
            command: "generated-source manifest".into(),
            detail: e.to_string(),
        })?;
    validate_manifest(&manifest)?;
    verify_generated_source_binding(Path::new(&manifest.source.path), &manifest.source.sha256)?;
    for artifact in &manifest.artifacts {
        let record = records
            .iter()
            .find(|record| record.media["artifact_id"] == artifact.id)
            .ok_or_else(|| Error::BadOutput {
                command: "generated-source result".into(),
                detail: format!("result is missing artifact {}", artifact.id),
            })?;
        if record.media["role"].as_str() != Some(artifact.role.as_str())
            || !record.sha256.eq_ignore_ascii_case(&artifact.sha256)
            || record.mime != artifact.mime
            || record.bytes != artifact.bytes
        {
            return Err(Error::BadOutput {
                command: "generated-source result".into(),
                detail: format!(
                    "artifact {} identity, role, MIME, hash or size differs from manifest",
                    artifact.id
                ),
            });
        }
    }
    Ok(())
}

/// Verify a bound source using the protocol's SHA-256 identity.
pub fn verify_generated_source_binding(path: &Path, expected_sha256: &str) -> Result<()> {
    if !valid_sha(expected_sha256) {
        return Err(Error::BadOutput {
            command: "generated-source manifest".into(),
            detail: "source sha256 must be 64 hex characters".into(),
        });
    }
    let actual = sha256_file(path).map_err(crate::error::io(path))?;
    if actual != expected_sha256.to_ascii_lowercase() {
        return Err(Error::BadOutput {
            command: "generated-source source".into(),
            detail: format!(
                "bound source failed sha256 verification: {}",
                path.display()
            ),
        });
    }
    Ok(())
}

fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

fn valid_sha(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn safe_name(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn copy_or_verify(
    src: &Path,
    dest: &Path,
    expected_sha: &str,
    expected_bytes: u64,
) -> Result<(String, String, u64)> {
    if dest.is_file() {
        if let Ok((sha, bytes)) = sha256_file_with_len(dest) {
            if sha.eq_ignore_ascii_case(expected_sha) && bytes == expected_bytes {
                let b3 = blake3_file(dest).map_err(crate::error::io(dest))?;
                return Ok((sha, b3, bytes));
            }
        }
        return Err(Error::BadOutput {
            command: "generated-source artifact".into(),
            detail: format!(
                "project media copy failed immutable hash verification: {}",
                dest.display()
            ),
        });
    }
    let parent = dest.parent().unwrap_or(Path::new("."));
    let staging = tempfile::Builder::new()
        .prefix(".generated-source-")
        .tempdir_in(parent)
        .map_err(crate::error::io(parent))?;
    let staged = staging.path().join("artifact");
    let copied = copy_verified_with_digest(
        src,
        &staged,
        AtomicWriteOptions::default(),
        DigestAlgorithm::Sha256,
        Some(expected_sha),
        |_, _| {},
    )
    .map_err(|e| Error::BadOutput {
        command: "generated-source artifact".into(),
        detail: format!("artifact {} failed verified copy: {e}", src.display()),
    })?;
    if copied.bytes != expected_bytes {
        return Err(Error::BadOutput {
            command: "generated-source artifact".into(),
            detail: format!("artifact {} failed size verification", src.display()),
        });
    }
    let b3 = blake3_file(&staged).map_err(crate::error::io(&staged))?;
    rightkit_fs::publish_file_with(
        &staged,
        dest,
        AtomicWriteOptions {
            replace: rightkit_fs::ReplaceMode::NoReplace,
            ..AtomicWriteOptions::default()
        },
    )
    .map_err(|e| crate::error::io(dest)(e.into()))?;
    Ok((copied.digest, b3, copied.bytes))
}

#[cfg(test)]
mod tests {
    // Focused malformed-metadata/hash-boundary checks; real CLI import is covered in generated_source_e2e.
    use super::*;

    fn write_fixture(root: &Path, reviewed: bool) -> (std::path::PathBuf, std::path::PathBuf) {
        fs::create_dir_all(root).unwrap();
        let source = root.join("source.blend");
        fs::write(&source, b"source fixture").unwrap();
        let artifact = root.join("shot.mp4");
        fs::write(&artifact, b"movie fixture").unwrap();
        let source_sha = sha256_file(&source).unwrap();
        let artifact_hash = sha256_file_with_len(&artifact).unwrap();
        let manifest = GeneratedSourceManifest {
            schema: 1,
            kind: "generated_source".into(),
            job_id: "job-1".into(),
            worker_version: "1".into(),
            blender_version: "4".into(),
            recipe: GeneratedRecipe {
                id: "recipe".into(),
                version: "1".into(),
                sha256: "a".repeat(64),
            },
            source: GeneratedSourceIdentity {
                path: "source.blend".into(),
                sha256: source_sha.clone(),
            },
            fps: RationalFps { num: 24, den: 1 },
            artifacts: vec![GeneratedArtifact {
                id: "artifact-1".into(),
                path: "shot.mp4".into(),
                mime: "video/mp4".into(),
                sha256: artifact_hash.0,
                bytes: artifact_hash.1,
                role: "movie".into(),
            }],
            shots: vec![GeneratedShot {
                id: "shot-1".into(),
                start_frame: 0,
                end_frame: 24,
                artifact_id: "artifact-1".into(),
                source_start_frame: 0,
                camera: serde_json::Value::Null,
                lens_mm: 50.0,
                source_sha256: source_sha.clone(),
                regen: Regeneration {
                    provider: "local-blender".into(),
                    model: "blender-film".into(),
                    params: serde_json::Value::Null,
                },
            }],
            product_review: reviewed.then(|| ProductReview {
                reviewed: true,
                product_id: Some("product-1".into()),
                variant: Some("variant-1".into()),
                confirmed: true,
                source_sha256: source_sha,
                finish_tags: [("matte".into(), "v1".into())].into(),
                approved_by: Some("owner".into()),
            }),
        };
        let manifest_path = root.join("manifest.json");
        fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        (manifest_path, root.join("project"))
    }

    #[test]
    fn unreviewed_blender_draft_fails_before_project_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let (manifest, project) = write_fixture(dir.path(), false);
        let error = import_generated_source(&project, &manifest, None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("draft is not deliverable"), "{error}");
        assert!(
            !project.exists(),
            "rejected draft must not create project files"
        );
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
        value["product_review"] = serde_json::json!({"reviewed":false,"product_id":null,"variant":null,
            "confirmed":false,"source_sha256":value["source"]["sha256"],"finish_tags":{},"approved_by":null});
        fs::write(&manifest, serde_json::to_vec(&value).unwrap()).unwrap();
        let error = import_generated_source(&project, &manifest, None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("draft is not deliverable"), "{error}");
        assert!(!project.exists());
    }

    #[test]
    fn reviewed_blender_product_imports() {
        let dir = tempfile::tempdir().unwrap();
        let (manifest, project) = write_fixture(dir.path(), true);
        let imported = import_generated_source(&project, &manifest, None).unwrap();
        assert_eq!(
            imported
                .manifest
                .product_review
                .as_ref()
                .unwrap()
                .product_id
                .as_deref(),
            Some("product-1")
        );
        assert!(project.join(&imported.artifacts[0].media_rel).is_file());
    }
}
