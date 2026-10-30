//! Store orchestration: readiness checks, download/repair, legacy migration,
//! and unreferenced-entry pruning — the operations the shell's model commands
//! and the engine's path resolution compose.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::download::{self, DownloadRequest};
use crate::http::{AsHttpClient, ModelHttp};
use crate::lock;
use crate::suite::archive;
use crate::suite::catalog::{ModelSpec, RemoteFile, SpecSource};
use crate::suite::manifest::{self, FileState, VerifyMode};
use crate::suite::migration;
use crate::suite::paths;
use crate::suite::refs;

const INSTALL_WAIT: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryState {
    /// No usable copy in the store.
    Missing,
    /// Some files present; at least one expected file missing or mismatched.
    Partial,
    /// Verified against the manifest at the requested depth.
    Ready,
    /// Another process holds the entry lock (install/migration in flight).
    InProgress,
}

#[derive(Debug, Clone)]
pub struct ModelStatus {
    pub id: String,
    pub label: String,
    pub version: String,
    pub state: EntryState,
    /// `<models-root>/<id>` — the shared location, even when missing.
    pub dir: PathBuf,
    pub files_ok: usize,
    pub files_total: usize,
    /// Relative paths that would be (re-)fetched by `ensure`.
    pub needed: Vec<String>,
    pub bytes_on_disk: u64,
    pub size_hint_bytes: u64,
    pub can_download: bool,
    pub requires_auth: bool,
    /// Base or add-on (see [`crate::suite::catalog::ModelTier`]).
    pub tier: crate::suite::catalog::ModelTier,
    /// The bearer is the app's signed Pro entitlement rather than a grant.
    pub requires_entitlement: bool,
    /// True while a live lock file exists (this or another process).
    pub in_progress: bool,
}

/// Read the entry's manifest (written at install/migrate time). A store dir
/// without one is *not* trusted — it may be a failed or hand-placed copy —
/// so absence degrades to Partial rather than silently passing.
fn entry_manifest_trusted(entry_dir: &Path, spec: &ModelSpec) -> bool {
    matches!(manifest::read_manifest(entry_dir), Ok(Some(ref actual))
        if manifest::matches_trusted(actual, &spec.expected_manifest()))
}

/// Store readiness at `mode` depth for one spec.
pub fn status_at(models_root: &Path, spec: &ModelSpec, mode: VerifyMode) -> ModelStatus {
    status_in(paths::model_dir_at(models_root, spec.id), spec, mode)
}

/// Readiness of `spec` in an explicit entry directory (same trusted-manifest
/// and file checks as the shared store).
pub fn status_in(entry_dir: PathBuf, spec: &ModelSpec, mode: VerifyMode) -> ModelStatus {
    // Readiness is observational: recovery belongs to a locked mutation,
    // never a doctor or dry-run read.
    let in_progress = lock::install_in_progress(&entry_dir);

    let manifest = spec.expected_manifest();
    let identity_ok = entry_manifest_trusted(&entry_dir, spec);
    let report = if entry_dir.is_dir() {
        manifest::verify_dir(&entry_dir, &manifest, mode)
    } else {
        // Nothing on disk: every expected file is "missing" so `needed`
        // lists the whole set.
        manifest::VerifyReport {
            checks: manifest
                .files
                .iter()
                .map(|f| manifest::FileCheck {
                    path: f.path.clone(),
                    state: FileState::Missing,
                    size: 0,
                })
                .collect(),
        }
    };
    let bytes_on_disk = dir_size(&entry_dir);
    let state = if !entry_dir.is_dir() {
        EntryState::Missing
    } else if identity_ok && report.is_ready() {
        // A complete entry stays Ready even while locked — the lock guards
        // mutation, not reads, and the caller (often the lock holder itself)
        // needs to know the files verify.
        EntryState::Ready
    } else if in_progress {
        EntryState::InProgress
    } else {
        EntryState::Partial
    };
    ModelStatus {
        id: spec.id.to_string(),
        label: spec.label.to_string(),
        version: spec.version.to_string(),
        state,
        dir: entry_dir,
        files_ok: report.files_ok(),
        files_total: report.checks.len(),
        needed: report.needed().iter().map(|c| c.path.clone()).collect(),
        bytes_on_disk,
        size_hint_bytes: spec.size_hint_bytes,
        can_download: spec.can_download(),
        requires_auth: spec.requires_auth(),
        tier: spec.tier,
        requires_entitlement: spec.requires_entitlement(),
        in_progress,
    }
}

impl ModelStatus {
    /// Text for the UI: an add-on that is not installed reads "Optional, not installed"
    /// instead of an error-like "Not installed".
    pub fn status_label(&self) -> &'static str {
        use crate::suite::catalog::ModelTier;
        match (self.state, self.tier) {
            (EntryState::Ready, _) => "Ready",
            (EntryState::InProgress, _) => "Installing",
            (EntryState::Partial, _) => "Incomplete",
            (EntryState::Missing, ModelTier::Base) => "Not installed",
            (EntryState::Missing, ModelTier::AddOn) => "Optional, not installed",
        }
    }
}

/// Readiness of a set of models and what finishing it would download.
#[derive(Debug, Clone)]
pub struct SetStatus {
    pub models: Vec<ModelStatus>,
    /// Every base model in the set is `Ready`. Add-ons never count: a missing optional model
    /// does not make setup incomplete. An empty or add-on-only set is vacuously ready.
    pub ready: bool,
    /// Approximate bytes still to download for the base models in the set that are not ready.
    pub base_download_bytes: u64,
    /// Approximate size of every model in the set, installed or not, add-ons included.
    pub total_size_hint_bytes: u64,
}

/// Readiness rollup of `specs` at `mode` depth. Pass the set explicitly: `base_catalog()` for
/// onboarding, `all_models()` for a settings page.
pub fn set_status_at(models_root: &Path, specs: &[ModelSpec], mode: VerifyMode) -> SetStatus {
    let models: Vec<ModelStatus> = specs
        .iter()
        .map(|spec| status_at(models_root, spec, mode))
        .collect();
    let base = |m: &&ModelStatus| m.tier == crate::suite::catalog::ModelTier::Base;
    SetStatus {
        ready: models
            .iter()
            .filter(base)
            .all(|m| m.state == EntryState::Ready),
        base_download_bytes: models
            .iter()
            .filter(base)
            .filter(|m| m.state != EntryState::Ready)
            .map(|m| m.size_hint_bytes)
            .sum(),
        total_size_hint_bytes: specs.iter().map(|s| s.size_hint_bytes).sum(),
        models,
    }
}

/// [`set_status_at`] for the base set: the default "is setup complete, and how much is left".
pub fn base_status_at(models_root: &Path, mode: VerifyMode) -> SetStatus {
    set_status_at(models_root, &crate::suite::catalog::base_catalog(), mode)
}

/// Approximate size of `specs`, for onboarding copy.
pub fn size_hint_bytes(specs: &[ModelSpec]) -> u64 {
    specs.iter().map(|s| s.size_hint_bytes).sum()
}

/// [`size_hint_bytes`] of the base set: the default onboarding estimate.
pub fn base_size_hint_bytes() -> u64 {
    size_hint_bytes(&crate::suite::catalog::base_catalog())
}

/// Resolve a ready shared-store copy of `spec` — the engine's fast lookup
/// path (size-verify, not hash: ~700 MB of hashing on every engine start is
/// not acceptable, and a deeper check runs in `model_store status`/`ensure`
/// and after any install). `None` = fall back to the app-seeded copy.
pub fn resolve_ready_dir(models_root: &Path, spec: &ModelSpec) -> Option<PathBuf> {
    let status = status_at(models_root, spec, VerifyMode::Size);
    (status.state == EntryState::Ready).then_some(status.dir)
}

/// Progress phases surfaced to the UI via `model-store://progress` events.
#[derive(Debug, Clone)]
pub enum Progress {
    Verify,
    Download {
        file: String,
        received: u64,
        total: u64,
    },
    Install,
    Done,
}

pub struct EnsureOptions<'a> {
    /// Directory containing bundled-seed files for `SpecSource::Files` entries
    /// with `url: None` (the app's resource models dir). `None` errors only
    /// if such a file actually needs fetching.
    pub seed_dir: Option<&'a Path>,
    /// `Authorization` header value for gated routes (e.g. `Bearer …`).
    pub authorization: Option<&'a str>,
    /// App id recorded in `refs/` on success.
    pub app_id: &'a str,
}

/// Ensure a verified store copy of `spec`: verify → repair missing/mismatched
/// files → re-verify → write manifest → register the app ref. Waits for a
/// peer install already in flight instead of racing it.
///
/// Archive-sourced models repair whole-archive (the worker serves one blob);
/// `Files`-sourced models repair only the affected files.
///
/// `agent` is the HTTP transport: pass [`crate::http::http_client`] (or a client
/// built with [`crate::http::http_client_with_policy`]). A legacy `&ureq::Agent`
/// still compiles; it only contributes its redirect limit.
pub fn ensure_ready(
    agent: &impl AsHttpClient,
    models_root: &Path,
    spec: &ModelSpec,
    opts: &EnsureOptions<'_>,
    progress: impl FnMut(Progress),
) -> Result<ModelStatus, String> {
    ensure_ready_cancellable(agent, models_root, spec, opts, &|| false, progress)
}

/// As [`ensure_ready`]; `cancelled` is polled between body chunks. A cancelled
/// download keeps its `.part` file so the next call resumes it.
pub fn ensure_ready_cancellable(
    agent: &impl AsHttpClient,
    models_root: &Path,
    spec: &ModelSpec,
    opts: &EnsureOptions<'_>,
    cancelled: &dyn Fn() -> bool,
    progress: impl FnMut(Progress),
) -> Result<ModelStatus, String> {
    ensure_ready_recorded(
        agent,
        models_root,
        spec,
        opts,
        cancelled,
        progress,
        &download::FailureSlot::default(),
    )
}

/// As [`ensure_ready_cancellable`], also leaving the last HTTP-level failure of a download in
/// `slot` (typed entitlement errors read it).
pub(crate) fn ensure_ready_recorded(
    agent: &impl AsHttpClient,
    models_root: &Path,
    spec: &ModelSpec,
    opts: &EnsureOptions<'_>,
    cancelled: &dyn Fn() -> bool,
    mut progress: impl FnMut(Progress),
    slot: &download::FailureSlot,
) -> Result<ModelStatus, String> {
    // A speech model is HeardRight's alone: refuse before any download (rightkit#1316).
    refs::check_speech_owner(opts.app_id, spec.id)?;
    let client = agent.vetted_client();
    // Keep the registry guard from initial readiness check through ref
    // registration so GC cannot delete a ready entry between those steps.
    let refs_dir = paths::model_refs_dir_at(models_root);
    let mut ref_dirs = vec![refs_dir.clone()];
    let current_models = paths::models_root();
    let legacy_models = paths::legacy_suite_root().join("models");
    if models_root == current_models && legacy_models != *models_root {
        ref_dirs.push(paths::model_refs_dir_at(&legacy_models));
    }
    let ref_dir_refs: Vec<&Path> = ref_dirs.iter().map(PathBuf::as_path).collect();
    let (_ref_guard_paths, _refs_guards) = refs::acquire_registries(&ref_dir_refs)?;
    progress(Progress::Verify);
    let status = status_at(models_root, spec, VerifyMode::Full);
    if status.state == EntryState::Ready {
        refs::register_locked(&refs_dir, opts.app_id, spec.id)?;
        prune_retired_after_ensure(models_root, spec, &ref_dirs, opts.app_id);
        return Ok(status);
    }
    let entry_dir = paths::model_dir_at(models_root, spec.id);
    let _guard = lock::lock_timeout(&entry_dir, opts.app_id, INSTALL_WAIT)?
        .ok_or_else(|| format!("{} is still being installed by another process", spec.label))?;

    // Re-verify under the lock: the peer we waited on may have finished.
    let status = status_at(models_root, spec, VerifyMode::Full);
    if status.state == EntryState::Ready {
        refs::register_locked(&refs_dir, opts.app_id, spec.id)?;
        prune_retired_after_ensure(models_root, spec, &ref_dirs, opts.app_id);
        return Ok(status);
    }

    archive::recover_orphaned_install(models_root, spec.id)?;
    // Hand-placed or legacy bytes may be adopted only after a full check
    // against trusted catalog metadata, never their self-described manifest.
    if manifest::verify_dir(&entry_dir, &spec.expected_manifest(), VerifyMode::Full).is_ready() {
        manifest::write_manifest(&entry_dir, &spec.expected_manifest())?;
        refs::register_locked(&refs_dir, opts.app_id, spec.id)?;
        prune_retired_after_ensure(models_root, spec, &ref_dirs, opts.app_id);
        progress(Progress::Done);
        return Ok(status_at(models_root, spec, VerifyMode::Full));
    }
    if !spec.can_download() {
        return Err(format!(
            "{} is not installed and no download endpoint exists for this platform",
            spec.label
        ));
    }

    match &spec.source {
        SpecSource::Archive {
            url,
            sha256,
            size,
            requires_auth,
        } => {
            let url = url.ok_or("no download endpoint configured for this platform")?;
            if *requires_auth && opts.authorization.is_none() {
                return Err(format!(
                    "{} download requires an app download token (none configured)",
                    spec.label
                ));
            }
            download_archive(
                &client,
                models_root,
                spec,
                url,
                *sha256,
                *size,
                opts.authorization,
                cancelled,
                &mut progress,
                slot,
            )?;
        }
        SpecSource::Files(remote) => {
            download_files(
                &client,
                models_root,
                spec,
                remote,
                &status.needed,
                opts,
                cancelled,
                &mut progress,
                slot,
            )?;
        }
    }

    progress(Progress::Install);
    // Post-install contract: every spec file present + all pins matching.
    // The recorded manifest then carries real hashes for every file.
    let manifest = spec.expected_manifest();
    let verify = manifest::verify_dir(&entry_dir, &spec.expected_manifest(), VerifyMode::Full);
    if !verify.is_ready() {
        let missing: Vec<String> = verify.needed().iter().map(|c| c.path.clone()).collect();
        return Err(format!(
            "{} install incomplete — files still bad: {}",
            spec.label,
            missing.join(", ")
        ));
    }
    manifest::write_manifest(&entry_dir, &manifest)?;
    refs::register_locked(&refs_dir, opts.app_id, spec.id)?;
    prune_retired_after_ensure(models_root, spec, &ref_dirs, opts.app_id);
    cleanup_incoming(models_root, spec.id);
    progress(Progress::Done);
    // Release before the status read: our own live lock would otherwise
    // report the freshly-installed entry as InProgress.
    drop(_guard);
    Ok(status_at(models_root, spec, VerifyMode::Size))
}

fn download_archive(
    client: &ModelHttp,
    models_root: &Path,
    spec: &ModelSpec,
    url: &str,
    sha256: Option<&'static str>,
    size: Option<u64>,
    authorization: Option<&str>,
    cancelled: &dyn Fn() -> bool,
    progress: &mut dyn FnMut(Progress),
    slot: &download::FailureSlot,
) -> Result<(), String> {
    let incoming = models_root.join(".incoming");
    let dest = incoming.join(format!("{}.tar.gz", spec.id));
    let part = download::part_path_for(&dest);
    let file = spec.id.to_string();
    // A pinned archive size bounds the download (nothing past it is written) and is checked
    // exactly below, next to the whole-archive SHA-256.
    download::download_resumable_recorded_capped(
        client,
        DownloadRequest {
            url,
            part_path: &part,
            dest_path: &dest,
            expected_sha256: sha256,
            authorization,
            cancelled,
        },
        None,
        size,
        |received, total| {
            progress(Progress::Download {
                file: file.clone(),
                received,
                total,
            })
        },
        slot,
    )
    .map_err(|error| error.message)?;
    if let Some(pinned) = size {
        let got = fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
        if got != pinned {
            let _ = fs::remove_file(&dest);
            return Err(format!(
                "{} archive is {got} bytes, the pinned size is {pinned}",
                spec.label
            ));
        }
    }
    progress(Progress::Install);
    archive::install_tar_gz(&dest, models_root, spec.id)?;
    let _ = fs::remove_file(&dest);
    Ok(())
}

fn download_files(
    client: &ModelHttp,
    models_root: &Path,
    spec: &ModelSpec,
    remote: &[RemoteFile],
    needed: &[String],
    opts: &EnsureOptions<'_>,
    cancelled: &dyn Fn() -> bool,
    progress: &mut dyn FnMut(Progress),
    slot: &download::FailureSlot,
) -> Result<(), String> {
    let entry_dir = paths::model_dir_at(models_root, spec.id);
    fs::create_dir_all(&entry_dir).map_err(|e| format!("create {}: {e}", entry_dir.display()))?;
    for file in remote {
        // Repair semantics: only fetch files the verify pass flagged — unless
        // the file has no spec-level pin (then presence is the check).
        if !needed.iter().any(|n| n == file.path) {
            continue;
        }
        let Some(rel) = manifest::safe_rel_path(file.path) else {
            return Err(format!("invalid catalog path {:?}", file.path));
        };
        let dest = entry_dir.join(&rel);
        match file.url {
            Some(url) => {
                let part = download::part_path_for(&dest);
                let name = file.path.to_string();
                download::download_resumable_recorded(
                    client,
                    DownloadRequest {
                        url,
                        part_path: &part,
                        dest_path: &dest,
                        expected_sha256: file.sha256,
                        authorization: opts.authorization,
                        cancelled,
                    },
                    None,
                    |received, total| {
                        progress(Progress::Download {
                            file: name.clone(),
                            received,
                            total,
                        })
                    },
                    slot,
                )
                .map_err(|error| error.message)?;
            }
            None => {
                // Bundled seed file — copy from the caller's resource dir.
                let seed_dir = opts.seed_dir.ok_or_else(|| {
                    format!(
                        "{} requires bundled file {} but no seed dir was provided",
                        spec.label, file.path
                    )
                })?;
                let src = seed_dir.join(&rel);
                if let Some(parent) = dest.parent() {
                    fs::create_dir_all(parent)
                        .map_err(|e| format!("create {}: {e}", parent.display()))?;
                }
                fs::copy(&src, &dest)
                    .map_err(|e| format!("seed {} -> {}: {e}", src.display(), dest.display()))?;
                // A pinned bundled file proves the seed itself is right —
                // catching a stale resource dir is the point of the pin.
                if let Some(pin) = file.sha256 {
                    let got = manifest::sha256_file(&dest)?;
                    if !got.eq_ignore_ascii_case(pin) {
                        let _ = fs::remove_file(&dest);
                        return Err(format!(
                            "bundled file {} failed integrity check: got {got}, expected {pin}",
                            file.path
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

/// True when both directories hold the same files with identical sizes and SHA-256
/// hashes (migration and backup retirement use this; retired store ids no longer
/// do, see [`prune_retired_store_ids`]).
fn identical_payloads(left: &Path, right: &Path) -> bool {
    let (Ok(left), Ok(right)) = (
        manifest::manifest_from_dir(left, "compare", "compare"),
        manifest::manifest_from_dir(right, "compare", "compare"),
    ) else {
        return false;
    };
    !left.files.is_empty()
        && left.files.len() == right.files.len()
        && left.files.iter().zip(&right.files).all(|(a, b)| {
            a.path == b.path && a.sha256.is_some() && a.sha256 == b.sha256 && a.size == b.size
        })
}

fn directories_byte_identical(left: &Path, right: &Path) -> bool {
    identical_payloads(left, right)
}

/// What one retired-id prune did. Failures are data, never an error: a removal that fails
/// before rename leaves the live entry intact; after rename it leaves only a hidden retirement
/// directory for a later prune to sweep. References are changed only by explicit release.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetiredPruneReport {
    /// Retired ids whose entry directory was removed.
    pub removed: Vec<String>,
    /// Retired ids whose entry directory exists and was left: any app still references it,
    /// an install or entry lock is live, or a runtime-family lease is held.
    pub kept: Vec<String>,
    /// `(id, reason)` for each id that could not be handled (removal failed, corrupt refs, lease
    /// error).
    pub failed: Vec<(String, String)>,
}

/// The refs registries a store at `models_root` answers to: its own, plus the legacy suite root's
/// when this is the machine-wide store.
fn registry_dirs(models_root: &Path) -> Vec<PathBuf> {
    let mut ref_dirs = vec![paths::model_refs_dir_at(models_root)];
    if paths::is_suite_models_root(models_root) {
        let legacy_models = paths::legacy_suite_root().join("models");
        if legacy_models != *models_root {
            ref_dirs.push(paths::model_refs_dir_at(&legacy_models));
        }
    }
    ref_dirs
}

/// Prune only IDs unreferenced by every app, including the caller. The caller holds all
/// registry guards. Runtime leases, install markers & entry locks remain authoritative.
/// References are dropped only through explicit release after a replacement is Ready.
fn prune_retired_store_ids(
    models_root: &Path,
    ref_dirs: &[PathBuf],
    _app_id: &str,
    ids: &[&str],
    protected_id: Option<&str>,
) -> RetiredPruneReport {
    let mut report = RetiredPruneReport::default();
    if ids.is_empty() {
        return report;
    }
    let fail_all = |report: &mut RetiredPruneReport, reason: String| {
        for id in ids {
            report.failed.push(((*id).to_string(), reason.clone()));
        }
    };
    // ModelSpec is caller-constructible. Validate every deletion target before touching any
    // entry or ref; ref-id validation alone permits reserved names such as `refs`.
    for id in ids {
        if let Err(error) = crate::ids::ModelId::parse(id) {
            fail_all(&mut report, error.to_string());
            return report;
        }
        if id.eq_ignore_ascii_case("refs")
            || protected_id.is_some_and(|current| id.eq_ignore_ascii_case(current))
        {
            fail_all(
                &mut report,
                format!("refusing to retire protected entry {id:?}"),
            );
            return report;
        }
    }
    let mut others = BTreeSet::new();
    for refs_dir in ref_dirs {
        match refs::referenced_ids_locked(refs_dir) {
            Ok(found) => others.extend(found),
            // A corrupt or unreadable ref blocks every deletion.
            Err(error) => {
                fail_all(&mut report, error);
                return report;
            }
        }
    }
    let suite_root = models_root.parent().unwrap_or(models_root);
    match crate::suite::lease::try_acquire_for_gc(suite_root, "model-store") {
        Err(error) => fail_all(&mut report, error),
        Ok(None) => report.kept.extend(
            ids.iter()
                .filter(|id| models_root.join(id).exists())
                .map(|id| (*id).to_string()),
        ),
        Ok(Some(_lease_guard)) => {
            for id in ids {
                let dir = paths::model_dir_at(models_root, id);
                if others.iter().any(|other| id.eq_ignore_ascii_case(other))
                    || lock::install_in_progress(&dir)
                {
                    if dir.exists() {
                        report.kept.push((*id).to_string());
                    }
                    continue;
                }
                let _entry_guard = match lock::try_lock(&dir, "model-store") {
                    Ok(Some(guard)) => guard,
                    Ok(None) => {
                        report.kept.push((*id).to_string());
                        continue;
                    }
                    Err(error) => {
                        report.failed.push(((*id).to_string(), error));
                        continue;
                    }
                };
                if let Err(error) = sweep_retiring(models_root, id) {
                    report.failed.push(((*id).to_string(), error.to_string()));
                }
                match retire_entry_with(&dir, |path| fs::remove_dir_all(path)) {
                    Ok(true) => report.removed.push((*id).to_string()),
                    Ok(false) => {}
                    Err(error) => report.failed.push((
                        (*id).to_string(),
                        format!("retire {}: {error}", dir.display()),
                    )),
                }
            }
        }
    }
    report
}

// Hidden names are outside ModelId's alphabet, so partially deleted payloads cannot resolve
// as live model entries. The registry, lease & original-entry guards cover rename and sweep.
fn retire_entry_with(
    dir: &Path,
    remove: impl FnOnce(&Path) -> std::io::Result<()>,
) -> std::io::Result<bool> {
    if !dir.try_exists()? {
        return Ok(false);
    }
    let id = dir
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| std::io::Error::other("invalid retirement path"))?;
    let retired = dir.with_file_name(format!(".{id}.retiring-{:x}", migration_nonce()));
    if retired.try_exists()? {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "retirement collision",
        ));
    }
    fs::rename(dir, &retired)?;
    remove(&retired)?;
    Ok(true)
}

fn sweep_retiring(root: &Path, id: &str) -> std::io::Result<()> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let prefix = format!(".{id}.retiring-");
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(suffix) = name.to_str().and_then(|name| name.strip_prefix(&prefix)) else {
            continue;
        };
        if !suffix.is_empty()
            && suffix.bytes().all(|b| b.is_ascii_hexdigit())
            && entry.file_type()?.is_dir()
        {
            fs::remove_dir_all(entry.path())?;
        }
    }
    Ok(())
}

/// Best-effort prune after an ensure that left `spec` Ready (download, adoption, or already
/// Ready). Never fails the ensure; use [`prune_retired`] to read the report.
fn prune_retired_after_ensure(
    models_root: &Path,
    spec: &ModelSpec,
    ref_dirs: &[PathBuf],
    app_id: &str,
) {
    let _ = prune_retired_store_ids(
        models_root,
        ref_dirs,
        app_id,
        spec.retired_store_ids,
        Some(spec.id),
    );
}

/// Prune the store entries `spec` retires on behalf of `app_id`, and return what happened. The
/// same prune runs best-effort after every [`ensure_ready`] that leaves `spec` Ready; call this
/// to read its report or to retry after a failure. A no-op unless `spec` is Ready in the store
/// (a retired entry is never removed in favour of a superseding model that is not installed).
pub fn prune_retired(
    models_root: &Path,
    spec: &ModelSpec,
    app_id: &str,
) -> Result<RetiredPruneReport, String> {
    let ref_dirs = registry_dirs(models_root);
    let ref_dir_refs: Vec<&Path> = ref_dirs.iter().map(PathBuf::as_path).collect();
    let (_guard_paths, _guards) = refs::acquire_registries(&ref_dir_refs)?;
    if status_at(models_root, spec, VerifyMode::Size).state != EntryState::Ready {
        return Ok(RetiredPruneReport::default());
    }
    Ok(prune_retired_store_ids(
        models_root,
        &ref_dirs,
        app_id,
        spec.retired_store_ids,
        Some(spec.id),
    ))
}

/// HeardRight's sole-owner speech retire (rightkit#1316). Call it after `active` (the speech
/// model now in use, for example Parakeet Ultra) is activated: it removes every retired Parakeet
/// TDT entry ([`crate::suite::catalog::PARAKEET_TDT_RETIRED_STORE_IDS`], except `active`) and
/// strips those ids from every app's ref in every registry this store answers to.
///
/// Only [`crate::suite::catalog::SPEECH_OWNER_APP`] may call it, and only while `active` is a
/// speech model that is Ready in the store. Foreign refs, a foreign runtime lease and a corrupt
/// ref file never block it: no other app may hold a speech model, so no runtime-family lease is
/// taken. An install in progress or a held entry lock keeps that entry (reported in `kept`), and
/// a removal the OS refuses (a file still mapped on Windows) leaves the live entry intact and is
/// reported in `failed`; call again to retry. Idempotent.
pub fn retire_speech_models(
    models_root: &Path,
    app_id: &str,
    active: &ModelSpec,
) -> Result<RetiredPruneReport, String> {
    use crate::suite::catalog::{
        is_speech_model_id, PARAKEET_TDT_RETIRED_STORE_IDS, SPEECH_OWNER_APP,
    };
    if app_id != SPEECH_OWNER_APP {
        return Err(format!(
            "only {SPEECH_OWNER_APP} retires speech models, not {app_id:?}"
        ));
    }
    if !is_speech_model_id(active.id) {
        return Err(format!("{} is not a speech model", active.id));
    }
    let ids: Vec<&str> = PARAKEET_TDT_RETIRED_STORE_IDS
        .iter()
        .copied()
        .filter(|id| !id.eq_ignore_ascii_case(active.id))
        .collect();
    let ref_dirs = registry_dirs(models_root);
    let ref_dir_refs: Vec<&Path> = ref_dirs.iter().map(PathBuf::as_path).collect();
    let (_guard_paths, _guards) = refs::acquire_registries(&ref_dir_refs)?;
    if status_at(models_root, active, VerifyMode::Size).state != EntryState::Ready {
        return Err(format!(
            "{} is not Ready: nothing is retired before its replacement is installed",
            active.id
        ));
    }
    for refs_dir in &ref_dirs {
        refs::strip_ids_all_locked(refs_dir, &ids)?;
    }
    let mut report = RetiredPruneReport::default();
    for id in ids {
        let dir = paths::model_dir_at(models_root, id);
        if lock::install_in_progress(&dir) {
            if dir.exists() {
                report.kept.push(id.to_string());
            }
            continue;
        }
        let _entry_guard = match lock::try_lock(&dir, "model-store") {
            Ok(Some(guard)) => guard,
            Ok(None) => {
                report.kept.push(id.to_string());
                continue;
            }
            Err(error) => {
                report.failed.push((id.to_string(), error));
                continue;
            }
        };
        if let Err(error) = sweep_retiring(models_root, id) {
            report.failed.push((id.to_string(), error.to_string()));
        }
        match retire_entry_with(&dir, |path| fs::remove_dir_all(path)) {
            Ok(true) => report.removed.push(id.to_string()),
            Ok(false) => {}
            Err(error) => report
                .failed
                .push((id.to_string(), format!("retire {}: {error}", dir.display()))),
        }
    }
    Ok(report)
}

/// Every store id any built-in spec retires, plus the ids Parakeet Ultra retires (that spec is
/// pin-gated and not in [`crate::suite::catalog::all_models`]) and every retired TDT id.
fn known_retired_store_ids() -> BTreeSet<&'static str> {
    let mut ids: BTreeSet<&'static str> = crate::suite::catalog::all_models()
        .iter()
        .flat_map(|spec| spec.retired_store_ids.iter().copied())
        .collect();
    ids.extend(
        crate::suite::catalog::PARAKEET_ULTRA_RETIRED_STORE_IDS
            .iter()
            .copied(),
    );
    ids.extend(
        crate::suite::catalog::PARAKEET_TDT_RETIRED_STORE_IDS
            .iter()
            .copied(),
    );
    ids
}

/// Result of [`release_models`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReleaseReport {
    /// The requested ids no app references any more (sorted). Includes ids that were never
    /// referenced by anyone.
    pub unreferenced: Vec<String>,
    /// What pruning did with those of `unreferenced` that a built-in spec retires.
    pub pruned: RetiredPruneReport,
}

/// Drop `ids` from `app_id`'s refs in every registry this store answers to, under every registry
/// guard, and return the ids no app references any more. Of those, the ones a built-in spec
/// retires (superseded entries such as the Parakeet TDT v3 entries Ultra replaces) are then
/// pruned with the same checks as [`prune_retired`]: nothing is removed while an install is in
/// progress, an entry lock is held or a runtime-family lease is active. Ids that nothing
/// retires are only reported, never deleted (that stays [`prune_unref_entries`]'s decision).
///
/// Idempotent. Every id is validated and every ref read before anything changes; a corrupt
/// ref or an invalid id is an error and nothing is released.
pub fn release_models(
    models_root: &Path,
    app_id: &str,
    ids: &[&str],
) -> Result<ReleaseReport, String> {
    let ref_dirs = registry_dirs(models_root);
    let ref_dir_refs: Vec<&Path> = ref_dirs.iter().map(PathBuf::as_path).collect();
    let (_guard_paths, _guards) = refs::acquire_registries(&ref_dir_refs)?;
    // Validate everything first so a bad id or corrupt ref in a later registry changes nothing.
    refs::validate_model_ids(ids)?;
    for refs_dir in &ref_dirs {
        let _ = refs::referenced_ids_locked(refs_dir)?;
    }
    for refs_dir in &ref_dirs {
        refs::release_locked(refs_dir, app_id, ids)?;
    }
    let mut referenced = BTreeSet::new();
    for refs_dir in &ref_dirs {
        referenced.extend(refs::referenced_ids_locked(refs_dir)?);
    }
    let unreferenced: BTreeSet<&str> = ids
        .iter()
        .copied()
        .filter(|id| !referenced.contains(*id))
        .collect();
    let retired = known_retired_store_ids();
    let prunable: Vec<&str> = unreferenced
        .iter()
        .copied()
        .filter(|id| retired.contains(*id))
        .collect();
    let pruned = prune_retired_store_ids(models_root, &ref_dirs, app_id, &prunable, None);
    Ok(ReleaseReport {
        unreferenced: unreferenced.into_iter().map(str::to_string).collect(),
        pruned,
    })
}

fn cleanup_incoming(models_root: &Path, id: &str) {
    let incoming = models_root.join(".incoming");
    for suffix in [".tar.gz", ".tar.gz.part", ".tar.gz.part.meta"] {
        let _ = fs::remove_file(incoming.join(format!("{id}{suffix}")));
    }
    let _ = fs::remove_dir(&incoming); // no-op unless now empty
}

/// Record `app_id → model_id` in refs registry, surfacing write failures so
/// callers never proceed with an untracked install.
pub fn register_ref(models_root: &Path, app_id: &str, model_id: &str) -> Result<(), String> {
    refs::register(&paths::model_refs_dir_at(models_root), app_id, model_id)
}

// ---------------------------------------------------------------------------
// Migration — first-run move of a verified legacy copy into the store.
// ---------------------------------------------------------------------------

/// Deprecated unchecked entrypoint. Mutation requires native load validation.
pub fn migrate_legacy(
    _models_root: &Path,
    _spec: &ModelSpec,
    _legacy_models_root: &Path,
    _app_id: &str,
) -> MigrateOutcome {
    let mut out = MigrateOutcome::default();
    out.errors
        .push("unchecked legacy migration declined; use migrate_legacy_checked".to_string());
    out
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct MigrateOutcome {
    /// Spec ids migrated into the store.
    pub migrated: Vec<String>,
    /// `(legacy path, reason)` entries left in place — kept as the app's
    /// fallback because they did not verify against the spec.
    pub skipped: Vec<(String, String)>,
    /// Retired variants deleted under the legacy models root.
    pub removed: Vec<String>,
    pub errors: Vec<String>,
    #[serde(default)]
    pub deferred: bool,
}

/// Checked single-source migration. Source is fully pin-verified before any
/// mutation; destination is load-checked before source retirement.
pub fn migrate_source_checked(
    models_root: &Path,
    spec: &ModelSpec,
    source_dir: &Path,
    app_id: &str,
    load_check: &dyn Fn(&Path) -> Result<(), String>,
) -> MigrateOutcome {
    let mut out = MigrateOutcome::default();
    let destination = paths::model_dir_at(models_root, spec.id);
    let journal_path = migration::journal_path(models_root, spec.id);
    let journal_source = if journal_path.exists() {
        match migration::read_journal(&journal_path) {
            Ok(journal)
                if journal.version == migration::JOURNAL_VERSION
                    && journal.model_id == spec.id
                    && journal.destination == destination
                    && journal.source == source_dir =>
            {
                Some(journal.source)
            }
            Ok(_) => {
                out.errors
                    .push("migration journal identity/source mismatch".into());
                return out;
            }
            Err(error) => {
                out.errors.push(error);
                return out;
            }
        }
    } else {
        None
    };
    let source_for_guards = journal_source.as_deref().unwrap_or(source_dir);
    let source_models = source_for_guards
        .ancestors()
        .skip(1)
        .find(|dir| dir.file_name().is_some_and(|name| name == "models"))
        .or_else(|| {
            source_for_guards
                .parent()
                .filter(|dir| dir.file_name().is_some_and(|name| name == "asr"))
                .and_then(Path::parent)
        });
    let destination_root = models_root.parent().unwrap_or(models_root);
    let refs_dir = paths::model_refs_dir_at(models_root);
    let mut ref_paths = vec![refs_dir.clone()];
    let mut lease_root_paths = vec![destination_root.to_path_buf()];
    if paths::is_suite_models_root(models_root) {
        let legacy = paths::legacy_suite_root();
        ref_paths.push(legacy.join("models/refs"));
        lease_root_paths.push(legacy);
    }
    if let Some(source_models) = source_models {
        ref_paths.push(paths::model_refs_dir_at(source_models));
        lease_root_paths.push(
            source_models
                .parent()
                .unwrap_or(source_models)
                .to_path_buf(),
        );
    }
    let ref_path_refs: Vec<&Path> = ref_paths.iter().map(PathBuf::as_path).collect();
    let _refs_guards = match refs::acquire_registries(&ref_path_refs) {
        Ok(guards) => guards,
        Err(error) => {
            out.errors.push(error);
            return out;
        }
    };
    // Unknown/corrupt references make source retirement unsafe.
    for refs_dir in &ref_paths {
        if let Err(error) = refs::referenced_ids_locked(refs_dir) {
            out.errors.push(error);
            return out;
        }
    }
    let lease_roots: Vec<&Path> = lease_root_paths.iter().map(PathBuf::as_path).collect();
    let Some(_runtime_guards) =
        (match crate::suite::lease::try_acquire_for_gc_at_roots(&lease_roots, "model-migration") {
            Ok(guard) => guard,
            Err(error) => {
                out.errors.push(error);
                return out;
            }
        })
    else {
        out.skipped.push((
            source_dir.display().to_string(),
            "model runtime lease active; migration deferred".to_string(),
        ));
        out.deferred = true;
        return out;
    };
    let mut entry_paths = vec![destination.clone(), source_for_guards.to_path_buf()];
    entry_paths.sort();
    entry_paths.dedup();
    let mut entry_guards = Vec::new();
    for path in &entry_paths {
        match lock::try_lock(path, app_id) {
            Ok(Some(guard)) => entry_guards.push(guard),
            Ok(None) => {
                out.skipped.push((
                    path.display().to_string(),
                    "migration entry is in progress; migration deferred".to_string(),
                ));
                out.deferred = true;
                return out;
            }
            Err(error) => {
                out.errors.push(error);
                return out;
            }
        }
    }
    if let Err(error) = recover_migration_journal(models_root, spec, load_check) {
        out.errors.push(error);
        return out;
    }
    if let Err(error) = migration::verify_trusted_source(source_dir, spec) {
        out.skipped.push((source_dir.display().to_string(), error));
        return out;
    }
    let same_payload_path = source_dir == destination
        || source_dir.canonicalize().ok() == destination.canonicalize().ok();
    if same_payload_path {
        if let Err(error) = refs::register_locked(&refs_dir, app_id, spec.id) {
            out.errors.push(error);
        }
        return out;
    }
    if is_symlink(source_dir) {
        // Renaming a directory alias would move only its pointer, not payload.
        out.skipped.push((
            source_dir.display().to_string(),
            "source aliases another location; migrate its real directory instead".into(),
        ));
        return out;
    }
    let txn = format!("{}-{}", std::process::id(), migration_nonce());
    let staging = models_root.join(format!(".{}.migration-{txn}", spec.id));
    let aside = source_dir.with_file_name(format!(".{}.migration-aside-{txn}", spec.id));
    let mut journal = migration::MigrationJournal {
        version: migration::JOURNAL_VERSION,
        model_id: spec.id.to_string(),
        source: source_dir.to_path_buf(),
        destination: destination.clone(),
        staging: Some(staging.clone()),
        aside: Some(aside.clone()),
        backup: None,
        destination_created: false,
        source_moved: false,
        phase: migration::JournalPhase::Planned,
    };
    if let Err(error) = migration::write_journal(&journal_path, &journal) {
        out.errors.push(error);
        return out;
    }
    journal.phase = migration::JournalPhase::SourceVerified;
    if let Err(error) = migration::write_journal(&journal_path, &journal) {
        out.errors.push(error);
        return out;
    }

    let destination_ready =
        status_at(models_root, spec, VerifyMode::Full).state == EntryState::Ready;
    journal.destination_created = !destination_ready;
    if destination_ready && !directories_byte_identical(source_dir, &destination) {
        if let Err(error) = fs::remove_file(&journal_path) {
            out.errors
                .push(format!("remove migration journal: {error}"));
            return out;
        }
        out.skipped.push((
            source_dir.display().to_string(),
            "distinct payload preserved".to_string(),
        ));
        return out;
    }
    let mut backup: Option<PathBuf> = None;
    if !destination_ready {
        if staging.exists() {
            out.errors.push(format!(
                "migration staging path already exists: {}",
                staging.display()
            ));
            return out;
        }
        // Journal intent before rename so a crash cannot orphan source bytes.
        // Rename preserves file identity on one volume; only EXDEV permits copy.
        journal.source_moved = true;
        if let Err(error) = migration::write_journal(&journal_path, &journal) {
            out.errors.push(error);
            return out;
        }
        let staged = match fs::rename(source_dir, &staging) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::CrossesDevices => {
                journal.source_moved = false;
                migration::write_journal(&journal_path, &journal)
                    .and_then(|()| copy_dir_recursive(source_dir, &staging))
            }
            Err(error) => Err(format!("stage migration source: {error}")),
        };
        if let Err(error) = staged
            .and_then(|()| {
                let report =
                    manifest::verify_dir(&staging, &spec.expected_manifest(), VerifyMode::Full);
                if report.is_ready() {
                    Ok(())
                } else {
                    Err(format!(
                        "staged destination failed trusted verification: {}",
                        report.needed().len()
                    ))
                }
            })
            .and_then(|()| manifest::write_manifest(&staging, &spec.expected_manifest()))
        {
            if let Err(restore) = restore_renamed_source(&mut journal, spec) {
                out.errors.push(restore);
                out.errors.push(error);
                return out;
            }
            if staging.exists() {
                if let Err(cleanup) = fs::remove_dir_all(&staging) {
                    out.errors
                        .push(format!("remove failed migration staging: {cleanup}"));
                }
            }
            if let Err(cleanup) = fs::remove_file(&journal_path) {
                out.errors
                    .push(format!("remove failed migration journal: {cleanup}"));
            }
            out.errors.push(error);
            return out;
        }
        journal.phase = migration::JournalPhase::Staged;
        if let Err(error) = migration::write_journal(&journal_path, &journal) {
            out.errors.push(error);
            return out;
        }
        if destination.exists() {
            let replaced = destination.with_file_name(format!(".{}.replaced-{}", spec.id, txn));
            if replaced.exists() {
                out.errors.push(format!(
                    "migration backup path already exists: {}",
                    replaced.display()
                ));
                return out;
            }
            backup = Some(replaced.clone());
            journal.backup = backup.clone();
            if let Err(error) = migration::write_journal(&journal_path, &journal) {
                out.errors.push(error);
                return out;
            }
            if let Err(error) = fs::rename(&destination, &replaced) {
                out.errors
                    .push(format!("move existing destination aside: {error}"));
                return out;
            }
            if let Err(error) = fs::rename(&staging, &destination) {
                let _ = fs::rename(&replaced, &destination);
                out.errors
                    .push(format!("publish migration destination: {error}"));
                return out;
            }
        } else if let Err(error) = fs::rename(&staging, &destination) {
            out.errors
                .push(format!("publish migration destination: {error}"));
            return out;
        }
    }
    journal.phase = migration::JournalPhase::DestinationVerified;
    if let Err(error) = migration::write_journal(&journal_path, &journal) {
        out.errors.push(error);
        return out;
    }
    if let Err(error) = load_check(&destination) {
        journal.phase = migration::JournalPhase::Rollback;
        // Keep active recovery metadata until every rollback step succeeds.
        if let Err(rollback) = migration::write_journal(&journal_path, &journal)
            .and_then(|()| restore_source_from_aside(&journal))
            .and_then(|()| restore_renamed_source(&mut journal, spec))
            .and_then(|()| restore_destination_backup(&mut journal))
            .and_then(|()| archive_migration_journal(&journal_path, &journal))
        {
            out.errors.push(rollback);
        }
        out.errors
            .push(format!("destination load check failed: {error}"));
        return out;
    }
    // Replaced content can be another variant or user-owned extras. Retain
    // its journaled backup unless every payload byte is identical.
    if let Some(backup) = backup.as_ref() {
        if identical_payloads(backup, &destination) {
            if let Err(error) = fs::remove_dir_all(backup) {
                out.errors
                    .push(format!("remove identical migration backup: {error}"));
                return out;
            }
        }
    }
    if let Err(error) = refs::register_locked(&refs_dir, app_id, spec.id) {
        out.errors.push(error);
        return out;
    }
    if let Err(error) = complete_source_retirement(&journal_path, &mut journal) {
        out.errors.push(error);
        return out;
    }
    out.migrated.push(source_dir.display().to_string());
    out
}

pub fn migrate_legacy_checked(
    models_root: &Path,
    spec: &ModelSpec,
    legacy_models_root: &Path,
    app_id: &str,
    load_check: &dyn Fn(&Path) -> Result<(), String>,
) -> MigrateOutcome {
    let mut out = MigrateOutcome::default();
    let journal_path = migration::journal_path(models_root, spec.id);
    let mut sources: Vec<PathBuf> = spec
        .legacy_dirs
        .iter()
        .map(|rel| legacy_models_root.join(rel))
        .collect();
    if journal_path.exists() {
        match migration::read_journal(&journal_path) {
            Ok(journal) => {
                sources.retain(|source| source != &journal.source);
                sources.insert(0, journal.source);
            }
            Err(error) => {
                out.errors.push(error);
                return out;
            }
        }
    }
    let destination = paths::model_dir_at(models_root, spec.id);
    // Retired payloads qualify only when all bytes match current destination.
    // Checked activation preserves their old paths as working aliases.
    sources.extend(
        spec.retired_legacy_paths
            .iter()
            .map(|rel| legacy_models_root.join(rel))
            .filter(|source| source.is_dir()),
    );
    for (index, source) in sources.into_iter().enumerate() {
        if !source.exists() && !(index == 0 && journal_path.exists()) {
            continue;
        }
        let retired = spec
            .retired_legacy_paths
            .iter()
            .any(|rel| legacy_models_root.join(rel) == source);
        if retired && !identical_payloads(&source, &destination) {
            continue;
        }
        let result = migrate_source_checked(models_root, spec, &source, app_id, load_check);
        if retired && !result.migrated.is_empty() {
            out.removed.push(source.display().to_string());
        }
        out.migrated.extend(result.migrated);
        out.skipped.extend(result.skipped);
        out.removed.extend(result.removed);
        out.deferred |= result.deferred;
        out.errors.extend(result.errors);
        if !out.errors.is_empty() || out.deferred {
            break;
        }
    }
    out
}

fn create_legacy_alias(source: &Path, destination: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(destination, source).map_err(|error| {
            format!(
                "symlink {} -> {}: {error}",
                source.display(),
                destination.display()
            )
        })
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Directory junction (no privilege needed, unlike a symlink). cmd parses a `/` inside a
        // path as a switch ("Invalid switch"), so hand mklink backslash-only paths; never open a
        // console window (CREATE_NO_WINDOW).
        let backslashed = |path: &Path| path.as_os_str().to_string_lossy().replace('/', "\\");
        let output = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(backslashed(source))
            .arg(backslashed(destination))
            .creation_flags(0x0800_0000)
            .output()
            .map_err(|error| format!("junction {}: {error}", source.display()))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(format!(
                "junction {} -> {}: {}",
                source.display(),
                destination.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            ))
        }
    }
}

fn recover_migration_journal(
    models_root: &Path,
    spec: &ModelSpec,
    load_check: &dyn Fn(&Path) -> Result<(), String>,
) -> Result<(), String> {
    let path = migration::journal_path(models_root, spec.id);
    if !path.exists() {
        return Ok(());
    }
    let journal = migration::read_journal(&path)?;
    if journal.version != migration::JOURNAL_VERSION || journal.model_id != spec.id {
        return Err(format!(
            "invalid migration journal identity: {}",
            path.display()
        ));
    }
    let expected_destination = paths::model_dir_at(models_root, spec.id);
    if journal.destination != expected_destination {
        return Err(format!(
            "migration journal destination mismatch: {}",
            path.display()
        ));
    }
    let scratch_ok = |candidate: &Path| {
        candidate
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(&format!(".{}.", spec.id)))
            && candidate.parent() == Some(models_root)
    };
    if let Some(staging) = journal.staging.as_deref() {
        if !scratch_ok(staging) {
            return Err("migration journal staging path is outside owned scratch".to_string());
        }
    }
    if let Some(backup) = journal.backup.as_deref() {
        if !scratch_ok(backup) {
            return Err("migration journal backup path is outside owned scratch".to_string());
        }
    }
    if let Some(aside) = journal.aside.as_deref() {
        let aside_name_ok = aside
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.starts_with(&format!(".{}.", spec.id)))
            .unwrap_or(false);
        if !aside_name_ok || aside.parent() != journal.source.parent() {
            return Err("migration journal aside path is not owned scratch".to_string());
        }
    }
    let mut journal = journal;
    match journal.phase {
        migration::JournalPhase::Planned
        | migration::JournalPhase::SourceVerified
        | migration::JournalPhase::Staged => {
            restore_renamed_source(&mut journal, spec)?;
            if !journal.source.exists() {
                return Err("migration source disappeared before retirement".to_string());
            }
            let abandoned_staging = journal.staging.clone();
            journal.phase = migration::JournalPhase::Rollback;
            migration::write_journal(&path, &journal)?;
            restore_destination_backup(&mut journal)?;
            if let Some(staging) = abandoned_staging {
                if staging.exists() {
                    fs::remove_dir_all(&staging)
                        .map_err(|error| format!("remove abandoned migration staging: {error}"))?;
                }
            }
            archive_migration_journal(&path, &journal)?;
        }
        migration::JournalPhase::DestinationVerified => {
            if let Err(error) = migration::verify_trusted_source(&journal.destination, spec)
                .and_then(|()| load_check(&journal.destination))
            {
                journal.phase = migration::JournalPhase::Rollback;
                migration::write_journal(&path, &journal)?;
                restore_source_from_aside(&journal)?;
                restore_renamed_source(&mut journal, spec)?;
                restore_destination_backup(&mut journal)?;
                archive_migration_journal(&path, &journal)?;
                return Err(format!("recovered destination load check failed: {error}"));
            }
            if let Some(backup) = journal.backup.as_deref() {
                if backup.exists() && identical_payloads(backup, &journal.destination) {
                    fs::remove_dir_all(backup)
                        .map_err(|e| format!("remove identical recovered backup: {e}"))?;
                }
            }
            complete_source_retirement(&path, &mut journal)?;
        }
        migration::JournalPhase::SourceAside
        | migration::JournalPhase::AliasCreated
        | migration::JournalPhase::Complete => {
            if let Err(error) = migration::verify_trusted_source(&journal.destination, spec)
                .and_then(|()| load_check(&journal.destination))
            {
                journal.phase = migration::JournalPhase::Rollback;
                migration::write_journal(&path, &journal)?;
                restore_source_from_aside(&journal)?;
                restore_renamed_source(&mut journal, spec)?;
                restore_destination_backup(&mut journal)?;
                archive_migration_journal(&path, &journal)?;
                return Err(format!("recovered destination load check failed: {error}"));
            }
            complete_source_retirement(&path, &mut journal)?;
        }
        migration::JournalPhase::Rollback => {
            restore_source_from_aside(&journal)?;
            restore_renamed_source(&mut journal, spec)?;
            restore_destination_backup(&mut journal)?;
            archive_migration_journal(&path, &journal)?;
        }
    }
    Ok(())
}

fn restore_destination_backup(journal: &mut migration::MigrationJournal) -> Result<(), String> {
    if let Some(backup) = journal.backup.as_deref().filter(|path| path.exists()) {
        if journal.destination.exists() {
            let failed = journal.destination.with_file_name(format!(
                ".{}.failed-{}",
                journal.model_id,
                migration_nonce()
            ));
            // Record failed destination path before preserving it during rollback.
            journal.staging = Some(failed.clone());
            let models = journal
                .destination
                .parent()
                .ok_or("destination has no model root")?;
            migration::write_journal(&migration::journal_path(models, &journal.model_id), journal)?;
            fs::rename(&journal.destination, failed)
                .map_err(|error| format!("preserve failed destination: {error}"))?;
        }
        fs::rename(backup, &journal.destination)
            .map_err(|error| format!("restore destination backup: {error}"))?;
    } else if journal.backup.is_none()
        && journal.destination_created
        && journal.destination.exists()
    {
        let failed = journal.destination.with_file_name(format!(
            ".{}.failed-{}",
            journal.model_id,
            migration_nonce()
        ));
        journal.staging = Some(failed.clone());
        let models = journal
            .destination
            .parent()
            .ok_or("destination has no model root")?;
        migration::write_journal(&migration::journal_path(models, &journal.model_id), journal)?;
        fs::rename(&journal.destination, failed)
            .map_err(|error| format!("preserve failed new destination: {error}"))?;
    }
    Ok(())
}

fn restore_renamed_source(
    journal: &mut migration::MigrationJournal,
    spec: &ModelSpec,
) -> Result<(), String> {
    if !journal.source_moved {
        return Ok(());
    }
    if is_symlink(&journal.source) {
        if journal.source.canonicalize().ok() != journal.destination.canonicalize().ok() {
            return Err("cannot roll back an unrelated legacy alias".into());
        }
        remove_legacy_alias(&journal.source)?;
    } else if journal.source.exists() {
        // Rename intent can precede the move, or restoration can precede crash.
        journal.source_moved = false;
        return Ok(());
    }
    let payload = if let Some(staging) = journal.staging.as_deref().filter(|dir| dir.exists()) {
        staging.to_path_buf()
    } else if journal.destination_created && journal.destination.exists() {
        // A pre-existing partial destination must never masquerade as source.
        migration::verify_trusted_source(&journal.destination, spec)?;
        journal.destination.clone()
    } else {
        return Err("renamed migration source payload is missing; journal retained".into());
    };
    fs::rename(&payload, &journal.source)
        .map_err(|error| format!("restore renamed migration source: {error}"))?;
    journal.source_moved = false;
    Ok(())
}

fn complete_source_retirement(
    path: &Path,
    journal: &mut migration::MigrationJournal,
) -> Result<(), String> {
    let aside = journal
        .aside
        .as_deref()
        .ok_or_else(|| "migration has no journaled source backup".to_string())?;
    let already_alias = is_symlink(&journal.source);
    if already_alias {
        if journal.source.canonicalize().ok() != journal.destination.canonicalize().ok() {
            return Err("legacy alias does not point to migration destination".into());
        }
    } else if journal.source.exists() {
        if aside.exists() || !identical_payloads(&journal.source, &journal.destination) {
            return Err("migration source changed; original content preserved".into());
        }
        fs::rename(&journal.source, aside)
            .map_err(|error| format!("retire migration source: {error}"))?;
        journal.phase = migration::JournalPhase::SourceAside;
        migration::write_journal(path, journal)?;
    }
    if !already_alias {
        create_legacy_alias(&journal.source, &journal.destination)?;
    }
    journal.phase = migration::JournalPhase::AliasCreated;
    migration::write_journal(path, journal)?;
    if aside.exists() {
        if !identical_payloads(aside, &journal.destination) {
            return Err("retired source differs from destination; backup preserved".into());
        }
        fs::remove_dir_all(aside)
            .map_err(|error| format!("remove identical retired source: {error}"))?;
    }
    journal.phase = migration::JournalPhase::Complete;
    migration::write_journal(path, journal)?;
    archive_migration_journal(path, journal)
}

fn is_symlink(path: &Path) -> bool {
    #[cfg(windows)]
    {
        // std reports directory junctions (name-surrogate reparse points) as links.
        fs::symlink_metadata(path)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        fs::symlink_metadata(path)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(false)
    }
}

fn remove_legacy_alias(path: &Path) -> Result<(), String> {
    #[cfg(windows)]
    let result = fs::remove_dir(path);
    #[cfg(not(windows))]
    let result = fs::remove_file(path);
    result.map_err(|error| format!("remove legacy alias {}: {error}", path.display()))
}

fn restore_source_from_aside(journal: &migration::MigrationJournal) -> Result<(), String> {
    let Some(aside) = journal.aside.as_deref() else {
        return Ok(());
    };
    if !aside.exists() {
        return Ok(());
    }
    if is_symlink(&journal.source) {
        remove_legacy_alias(&journal.source)?;
    } else if journal.source.exists() {
        return Err("cannot restore migration source over existing path".to_string());
    }
    fs::rename(aside, &journal.source).map_err(|e| format!("restore migration source: {e}"))
}

fn archive_migration_journal(
    path: &Path,
    journal: &migration::MigrationJournal,
) -> Result<(), String> {
    let completed = path
        .parent()
        .ok_or_else(|| "migration journal has no parent".to_string())?
        .join("completed");
    fs::create_dir_all(&completed)
        .map_err(|error| format!("create completed migration dir: {error}"))?;
    let target = completed.join(format!(
        "{}-{}.json",
        path.file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| "migration journal has no filename".to_string())?,
        migration_nonce()
    ));
    migration::write_journal(&target, journal)?;
    fs::remove_file(path).map_err(|error| format!("remove active migration journal: {error}"))
}

fn migration_nonce() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_else(|_| std::process::id() as u128)
}

fn copy_dir_recursive(src: &Path, dest: &Path) -> Result<(), String> {
    fs::create_dir_all(dest).map_err(|e| format!("create {}: {e}", dest.display()))?;
    for entry in fs::read_dir(src).map_err(|e| format!("read {}: {e}", src.display()))? {
        let entry = entry.map_err(|e| format!("read entry: {e}"))?;
        let path = entry.path();
        let target = dest.join(entry.file_name());
        if path.is_dir() {
            copy_dir_recursive(&path, &target)?;
        } else {
            fs::copy(&path, &target)
                .map_err(|e| format!("copy {} -> {}: {e}", path.display(), target.display()))?;
        }
    }
    Ok(())
}

/// Delete store entries no app references (uninstall/prune path). Never
/// removes an entry while a live install lock exists. `keep` protects ids
/// that must survive even unreferenced (e.g. the running app's current spec
/// mid-onboarding).
pub fn prune_unref_entries(models_root: &Path, keep: &[&str]) -> Result<Vec<String>, String> {
    prune_unref_entries_with(models_root, keep, &crate::suite::catalog::all_models())
}

/// As [`prune_unref_entries`], protecting the replacement ids of every spec in
/// `catalog` whose legacy names are still referenced (apps with their own
/// trusted catalog pass it here instead of the built-in one).
pub fn prune_unref_entries_with(
    models_root: &Path,
    keep: &[&str],
    catalog: &[ModelSpec],
) -> Result<Vec<String>, String> {
    let refs_dir = paths::model_refs_dir_at(models_root);
    let mut ref_dirs = vec![refs_dir.clone()];
    let current_models = paths::models_root();
    let legacy_models = paths::legacy_suite_root().join("models");
    if models_root == current_models && legacy_models != *models_root {
        ref_dirs.push(paths::model_refs_dir_at(&legacy_models));
    }
    let ref_dir_refs: Vec<&Path> = ref_dirs.iter().map(PathBuf::as_path).collect();
    let (_guard_paths, _refs_guards) = refs::acquire_registries(&ref_dir_refs)?;
    let mut referenced = std::collections::BTreeSet::new();
    for refs_dir in &ref_dirs {
        referenced.extend(refs::referenced_ids_locked(refs_dir)?);
    }
    // Legacy apps may register generic ids after one-shot migration. Protect
    // their trusted immutable replacement while compatibility remains active.
    for spec in catalog {
        let legacy_name_referenced = spec.legacy_dirs.iter().any(|legacy_dir| {
            Path::new(legacy_dir)
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| referenced.contains(name))
        });
        if legacy_name_referenced
            || spec
                .retired_store_ids
                .iter()
                .any(|legacy| referenced.contains(*legacy))
        {
            referenced.insert(spec.id.to_string());
        }
    }
    let mut removed = Vec::new();
    let entries = match fs::read_dir(models_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(removed),
        Err(error) => {
            return Err(format!(
                "read model store {}: {error}",
                models_root.display()
            ))
        }
    };
    let suite_root = models_root.parent().unwrap_or(models_root);
    let lease_roots = if models_root == current_models && legacy_models != *models_root {
        vec![suite_root, legacy_models.parent().unwrap_or(&legacy_models)]
    } else {
        vec![suite_root]
    };
    let lease_root_refs: Vec<&Path> = lease_roots;
    let Some(_lease_guard) =
        crate::suite::lease::try_acquire_for_gc_at_roots(&lease_root_refs, "model-store")?
    else {
        return Ok(removed);
    };
    for entry in entries {
        let entry = entry.map_err(|error| format!("read model store entry: {error}"))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with('.') || name == "refs" || keep.contains(&name) {
            continue;
        }
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if referenced.contains(name) {
            continue;
        }
        let Some(_entry_guard) = lock::try_lock(&path, "model-store")? else {
            continue;
        };
        match fs::remove_dir_all(&path) {
            Ok(()) => removed.push(name.to_string()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("remove model {}: {error}", path.display())),
        }
    }
    Ok(removed)
}

fn dir_size(dir: &Path) -> u64 {
    fn walk(path: &Path) -> u64 {
        let Ok(entries) = fs::read_dir(path) else {
            return 0;
        };
        entries
            .flatten()
            .map(|e| {
                let p = e.path();
                // Never follow links (a legacy alias inside an entry would
                // double-count the revision it points at).
                if e.file_type().is_ok_and(|t| t.is_symlink()) {
                    0
                } else if p.is_dir() {
                    walk(&p)
                } else {
                    e.metadata().map(|m| m.len()).unwrap_or(0)
                }
            })
            .sum()
    }
    if dir.is_dir() {
        walk(dir)
    } else {
        0
    }
}

/// Shared ureq agent from before downloads were vetted; see
/// [`crate::download::http_agent`].
#[deprecated(
    since = "0.2.9",
    note = "use `rightkit_models::http::http_client`; the agent's transport is no longer used"
)]
#[allow(deprecated)]
pub fn http_agent() -> ureq::Agent {
    crate::download::http_agent()
}

#[cfg(test)]
mod retired_target_tests {
    use super::*;

    #[test]
    fn partial_removal_is_hidden_and_later_prune_sweeps_it() {
        let temp = tempfile::tempdir().unwrap();
        let models = temp.path().join("models");
        let old = models.join("old");
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("a"), b"a").unwrap();
        fs::write(old.join("b"), b"b").unwrap();
        let result = retire_entry_with(&old, |retired| {
            assert!(!old.exists(), "rename must precede every removal");
            fs::remove_file(retired.join("a"))?;
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "simulated loaded file",
            ))
        });
        assert!(result.is_err());
        assert!(!old.exists());
        let leftover = fs::read_dir(&models)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert_eq!(fs::read(leftover.join("b")).unwrap(), b"b");
        let refs_dir = models.join("refs");
        let (_paths, _guards) = refs::acquire_registries(&[&refs_dir]).unwrap();
        let report =
            prune_retired_store_ids(&models, &[refs_dir], "app", &["old"], Some("current"));
        assert!(report.failed.is_empty());
        assert!(!leftover.exists());
    }

    #[test]
    fn missing_retirement_is_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing");
        assert!(!retire_entry_with(&missing, |_| panic!("must not remove")).unwrap());
        // A deterministic injected removal error covers the post-rename failure separately.
    }

    #[test]
    fn another_apps_case_variant_ref_preserves_retired_entry() {
        let temp = tempfile::tempdir().unwrap();
        let models = temp.path().join("models");
        let refs_dir = models.join("refs");
        let old = models.join("old");
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("payload"), b"keep").unwrap();
        refs::register(&refs_dir, "other-app", "OLD").unwrap();
        let (_paths, _guards) = refs::acquire_registries(&[&refs_dir]).unwrap();
        let report = prune_retired_store_ids(
            &models,
            std::slice::from_ref(&refs_dir),
            "review-app",
            &["old"],
            Some("current"),
        );
        assert_eq!(report.kept, ["old"]);
        assert!(report.removed.is_empty());
        assert_eq!(fs::read(old.join("payload")).unwrap(), b"keep");
    }

    #[test]
    fn unsafe_retirement_targets_preserve_every_entry_and_ref() {
        let temp = tempfile::tempdir().unwrap();
        let models = temp.path().join("models");
        let refs_dir = models.join("refs");
        for dir in [
            models.join("old"),
            models.join("current"),
            temp.path().join("escape"),
        ] {
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("payload"), b"keep").unwrap();
        }
        refs::register(&refs_dir, "review-app", "old").unwrap();
        let (_paths, _guards) = refs::acquire_registries(&[&refs_dir]).unwrap();
        let before = refs::referenced_ids_locked(&refs_dir).unwrap();
        let absolute = temp.path().join("escape");
        for target in [
            "../escape",
            ".",
            "refs",
            "ReFs",
            "current",
            "CURRENT",
            absolute.to_str().unwrap(),
        ] {
            let report = prune_retired_store_ids(
                &models,
                std::slice::from_ref(&refs_dir),
                "review-app",
                &["old", target],
                Some("current"),
            );
            assert_eq!(report.failed.len(), 2, "{target}");
            assert!(report.removed.is_empty(), "{target}");
            assert_eq!(refs::referenced_ids_locked(&refs_dir).unwrap(), before);
            for dir in [
                models.join("old"),
                models.join("current"),
                temp.path().join("escape"),
            ] {
                assert_eq!(fs::read(dir.join("payload")).unwrap(), b"keep");
            }
        }
    }
}
