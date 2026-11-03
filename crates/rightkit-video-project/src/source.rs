//! Source registration: BLAKE3 identity, owned copy (or in-place reference), typed
//! ffprobe, then the timeline `RegisterSource` action. Every step is idempotent, so
//! re-running after a crash or retry converges on the same project.

use std::path::{Path, PathBuf};

use rightkit_fs::FsError;
use rightkit_fs::{copy_verified_with_digest, AtomicWriteOptions, DigestAlgorithm};
use rightkit_timeline::{Action, SourceId, SourceInfo};

use crate::error::ProjectError;
use crate::probe::{probe_file, ProbeSummary};
use crate::project::{EditOutcome, Project, SourceMode, SourceRecord, MEDIA_DIR};
use crate::tools::MediaTools;

/// Streaming BLAKE3 of a file: `(id, size)`.
pub fn hash_file(path: &Path) -> Result<(SourceId, u64), ProjectError> {
    hash_file_progress(path, &mut |_| {})
}

/// [`hash_file`] reporting real byte progress (`read / total`, 0..=1).
pub fn hash_file_progress(
    path: &Path,
    progress: &mut dyn FnMut(f64),
) -> Result<(SourceId, u64), ProjectError> {
    let (digest, size) = rightkit_fs::digest::blake3_file_with_progress(path, |read, total| {
        progress((read as f64 / total.max(1) as f64).min(1.0));
    })
    .map_err(|e| ProjectError::io(format!("hashing {}", path.display()), e))?;
    Ok((SourceId::new(digest)?, size))
}

/// Copies `src` to `dest` atomically (RightKit writer: temp file, fsync, rename, directory
/// fsync), hashing while copying. Fails with `source_changed` when the bytes copied do not match
/// `expected`; nothing is published in that case.
pub(crate) fn copy_verified(
    src: &Path,
    dest: &Path,
    expected: &SourceId,
) -> Result<(), ProjectError> {
    // RightKit refuses symlink inputs; resolve user-selected aliases before copying.
    let canonical = src
        .canonicalize()
        .map_err(|e| ProjectError::io(format!("resolving {}", src.display()), e))?;
    copy_verified_with_digest(
        &canonical,
        dest,
        AtomicWriteOptions::default(),
        DigestAlgorithm::Blake3,
        Some(expected.as_str()),
        |_, _| {},
    )
    .map(|_| ())
    .map_err(|e| match e {
        FsError::DigestMismatch { .. } | FsError::Corrupt { .. } => {
            ProjectError::SourceChanged(src.display().to_string())
        }
        other => publish_error(dest, other),
    })
}

fn publish_error(dest: &Path, e: FsError) -> ProjectError {
    ProjectError::io(
        format!("writing {}", dest.display()),
        std::io::Error::other(e),
    )
}

pub(crate) fn media_file_name(id: &SourceId, original: &Path) -> String {
    let ext: String = original
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(8)
        .collect();
    if ext.is_empty() {
        id.to_string()
    } else {
        format!("{id}.{ext}")
    }
}

/// Checks that a registered source's file still hashes to its id.
pub fn verify_record(project: &Project, record: &SourceRecord) -> Result<PathBuf, ProjectError> {
    let path = project.source_path(record);
    if !path.is_file() {
        return Err(ProjectError::SourceNotFound(path.display().to_string()));
    }
    let (actual, _) = hash_file(&path)?;
    if actual != record.id {
        return Err(ProjectError::SourceHashMismatch {
            id: record.id.to_string(),
            detail: format!("{} now hashes to {actual}", path.display()),
        });
    }
    Ok(path)
}

#[derive(Debug, Clone)]
pub struct SourceReport {
    pub id: SourceId,
    /// `registered`, `already_registered`, or (dry run) `would_register`.
    pub status: &'static str,
    pub info: SourceInfo,
    pub fps: Option<(u32, u32)>,
    pub mode: SourceMode,
    pub path: String,
    pub size: u64,
}

fn info_from(id: &SourceId, probe: &ProbeSummary) -> SourceInfo {
    SourceInfo {
        id: id.clone(),
        duration: probe.duration,
        video: probe.video.clone(),
        has_audio: probe.has_audio,
    }
}

/// Registers `file` in the project. With `dry_run` nothing is changed (the project may
/// be opened read-only); the file is still hashed and probed so the report is real.
pub fn add_source(
    project: &mut Project,
    tools: &MediaTools,
    file: &Path,
    reference: bool,
    dry_run: bool,
) -> Result<SourceReport, ProjectError> {
    add_source_progress(project, tools, file, reference, dry_run, &mut |_, _| {})
}

/// [`add_source`] with per-phase progress: `progress(phase, fraction)` for
/// `"hashing"`, `"probing"` and `"copying"`. Hashing reports real byte
/// fractions; probing/copying bracket the actual ffprobe/copy work (the typed
/// boundary exposes no intermediate fraction, so those report 0 then 1 —
/// honest phase boundaries, never simulated values).
pub fn add_source_progress(
    project: &mut Project,
    tools: &MediaTools,
    file: &Path,
    reference: bool,
    dry_run: bool,
    progress: &mut dyn FnMut(&str, f64),
) -> Result<SourceReport, ProjectError> {
    if !file.is_file() {
        return Err(ProjectError::SourceNotFound(file.display().to_string()));
    }
    let file = file
        .canonicalize()
        .map_err(|e| ProjectError::io(format!("resolving {}", file.display()), e))?;
    let (id, size) = hash_file_progress(&file, &mut |f| progress("hashing", f))?;

    // Already registered and intact: nothing to do.
    if let Some(record) = project.source_record(&id).cloned() {
        if let Some(info) = project.timeline().source(&id).cloned() {
            verify_record(project, &record)?;
            return Ok(SourceReport {
                id,
                status: "already_registered",
                info,
                fps: None,
                mode: record.mode,
                path: record.path,
                size: record.size,
            });
        }
    }

    progress("probing", 0.0);
    let probe = probe_file(tools, &file)?;
    progress("probing", 1.0);
    let info = info_from(&id, &probe);
    let (mode, rel_or_abs) = if reference {
        (SourceMode::Reference, file.to_string_lossy().into_owned())
    } else {
        let name = media_file_name(&id, &file);
        (SourceMode::Copy, format!("{MEDIA_DIR}/{name}"))
    };
    if dry_run {
        return Ok(SourceReport {
            id,
            status: "would_register",
            info,
            fps: probe.fps,
            mode,
            path: rel_or_abs,
            size,
        });
    }

    // 1. Owned copy (content-addressed, so a retry finds it already there).
    if mode == SourceMode::Copy {
        let dest = project.dir.join(&rel_or_abs);
        let have = dest.is_file() && hash_file(&dest).map(|(h, _)| h == id).unwrap_or(false);
        if !have {
            progress("copying", 0.0);
            copy_verified(&file, &dest, &id)?;
            progress("copying", 1.0);
        }
    }
    // 2. Record where the bytes live (a retry keeps an existing record untouched).
    if project.source_record(&id).is_none() {
        project.meta.sources.push(SourceRecord {
            id: id.clone(),
            name: file
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            mode,
            path: rel_or_abs.clone(),
            size,
            added: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        });
        project.meta.sources.sort_by(|a, b| a.id.cmp(&b.id));
        project.save_meta()?;
    }
    // 3. Register in the timeline.
    let outcome = project.edit(Action::RegisterSource {
        source: info.clone(),
    })?;
    let record = project.source_record(&id).expect("recorded above");
    Ok(SourceReport {
        id,
        status: match outcome {
            EditOutcome::Recorded(_) => "registered",
            _ => "already_registered",
        },
        info,
        fps: probe.fps,
        mode: record.mode,
        path: record.path.clone(),
        size: record.size,
    })
}

/// Registers a file that already lives inside the project (for example a GenRight
/// import under `media/`) without copying it again. `rel` is project-relative with
/// forward slashes and must stay inside `media/`; the file's BLAKE3 must equal
/// `expected_blake3` when given. Idempotent: a retry reports `already_registered`.
pub fn register_in_place(
    project: &mut Project,
    tools: &MediaTools,
    rel: &str,
    expected_blake3: Option<&str>,
) -> Result<SourceReport, ProjectError> {
    let safe = rel.starts_with(&format!("{MEDIA_DIR}/"))
        && !rel.contains("..")
        && !rel.contains('\\')
        && !rel.contains("//");
    if !safe {
        return Err(ProjectError::BadInput(format!(
            "in-place source {rel:?} must be a path under {MEDIA_DIR}/"
        )));
    }
    let file = project.dir.join(rel);
    if !file.is_file() {
        return Err(ProjectError::SourceNotFound(file.display().to_string()));
    }
    let (id, size) = hash_file(&file)?;
    if let Some(want) = expected_blake3 {
        if !want.eq_ignore_ascii_case(id.as_str()) {
            return Err(ProjectError::SourceHashMismatch {
                id: want.to_string(),
                detail: format!("{} hashes to {id}", file.display()),
            });
        }
    }
    if let Some(record) = project.source_record(&id).cloned() {
        if let Some(info) = project.timeline().source(&id).cloned() {
            verify_record(project, &record)?;
            return Ok(SourceReport {
                id,
                status: "already_registered",
                info,
                fps: None,
                mode: record.mode,
                path: record.path,
                size: record.size,
            });
        }
    }
    let probe = probe_file(tools, &file)?;
    let info = info_from(&id, &probe);
    if project.source_record(&id).is_none() {
        project.meta.sources.push(SourceRecord {
            id: id.clone(),
            name: file
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            mode: SourceMode::Copy,
            path: rel.to_string(),
            size,
            added: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        });
        project.meta.sources.sort_by(|a, b| a.id.cmp(&b.id));
        project.save_meta()?;
    }
    let outcome = project.edit(Action::RegisterSource {
        source: info.clone(),
    })?;
    let record = project.source_record(&id).expect("recorded above");
    Ok(SourceReport {
        id,
        status: match outcome {
            EditOutcome::Recorded(_) => "registered",
            _ => "already_registered",
        },
        info,
        fps: probe.fps,
        mode: record.mode,
        path: record.path.clone(),
        size: record.size,
    })
}
