//! Download-and-install flows for the speech models this crate catalogues, reporting progress
//! through a host sink.
//!
//! Two flows, both blocking (call them from a worker thread):
//!
//! * [`ensure_archive_model`]: a model served as one pinned `.tar.gz` by a model worker. Short-
//!   circuits when the install is already complete; otherwise a grant is acquired, the archive is
//!   downloaded resumably and hash-verified before it is committed, installed atomically through
//!   [`crate::archive_install`], and the install is re-checked with
//!   [`crate::model_paths::local_asr_ready`].
//! * [`fetch_shared_speech_model`]: the shared-store copy of the catalogued Ultra model (verify,
//!   repair, install and reference registration are `rightkit_models::suite`).
//!
//! The module holds no product name, copy, route, host, model id or path of its own: every such
//! value is an input, validated before any effect. Progress is reported to the caller's
//! [`ProgressSink`]; the module emits no log or telemetry of its own and never formats a path or
//! file content into a message it returns.
//!
//! Locking, activation, runtime state and settings are the host's: serialize calls per model with
//! [`crate::whisper_model::model_operation_lock`] before calling.
//!
//! Every step is bounded, none by this module alone: the archive download is capped at the host's
//! `max_archive_bytes`; each grant request & transfer attempt has a `timeout` budget, with at
//! most one authorization refresh; the extraction is capped at the host's
//! `max_installed_bytes` and at [`archive_install::MAX_ARCHIVE_ENTRIES`] members; the grant request
//! needs an `https://` worker and never reaches a private address
//! ([`model_download_grant::acquire_model_grant`]); the shared-store copy downloads through
//! [`shared::http_client`] (stall limits, capped at the catalog's pinned size by the store) and
//! extracts within limits taken from the catalog's pinned file list; no child process is started
//! and no lock is taken here.

use crate::shared::store;
use crate::{
    archive_install, model_download_grant, model_paths, resumable_download, shared, ultra_model,
    whisper_model,
};
use std::time::Duration;

/// A phase of a model fetch, with the stable wire name a renderer event carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Verifying,
    Downloading,
    Extracting,
    Preparing,
    Installed,
}

impl Phase {
    /// The stable lowercase name of the phase.
    pub const fn wire(self) -> &'static str {
        match self {
            Phase::Verifying => "verifying",
            Phase::Downloading => "downloading",
            Phase::Extracting => "extracting",
            Phase::Preparing => "preparing",
            Phase::Installed => "installed",
        }
    }
}

/// Progress callback: phase, bytes received, total bytes (0 when unknown), message. Called on the
/// calling thread only.
pub type ProgressSink<'a> = &'a dyn Fn(Phase, u64, u64, &str);

/// Host-owned user-visible strings. Each is 1 to 256 bytes with no control character.
#[derive(Clone, Copy, Debug)]
pub struct FlowText<'a> {
    /// First message of an archive download, naming the model.
    pub downloading_named: &'a str,
    /// Message of every later download progress report.
    pub downloading: &'a str,
    /// Shared-store verification.
    pub verifying: &'a str,
    /// Archive extraction and install.
    pub installing_archive: &'a str,
    /// Shared-store install.
    pub installing_shared: &'a str,
    pub preparing: &'a str,
    pub ready: &'a str,
    /// Error returned when no valid pinned checksum was supplied.
    pub checksum_missing: &'a str,
    /// Error returned when the installed model failed its readiness check.
    pub incomplete_archive: &'a str,
}

/// One archive-served model. All fields are host values.
#[derive(Clone, Copy, Debug)]
pub struct ArchiveModel<'a> {
    /// Model name sent to the worker in the grant request: 1 to 64 of `A-Za-z0-9._-`.
    pub grant_model: &'a str,
    /// Worker route, starting with `/`, no `..`, no whitespace or control, at most 256 bytes.
    pub route: &'a str,
    /// Top-level directory name inside the archive: one plain name (see [`plain_name`]).
    pub archive_top_level: &'a str,
    /// Install directory name under the models root: one plain name.
    pub install_subdir: &'a str,
    /// Pinned archive SHA-256, 64 hex digits. `None` or malformed refuses the download.
    pub sha256: Option<&'a str>,
    /// Worker origin: `https://` then a host, at most 512 bytes, no whitespace or control.
    pub worker_base: &'a str,
    /// Platform tag sent in the grant request: 1 to 16 of `A-Za-z0-9._-`.
    pub platform: &'a str,
    /// Device identifier sent in the grant request: 1 to 256 bytes, no control character.
    pub device_id: &'a str,
    /// Budget for each grant request & transfer attempt: 60 s to 24 h.
    /// Authorization refresh can start one additional grant request & transfer attempt.
    pub timeout: Duration,
    /// Minimum bytes between two download progress reports: 64 KiB to 64 MiB.
    pub progress_step_bytes: u64,
    /// Most bytes the downloaded archive may have: 1 to
    /// [`resumable_download::MAX_DOWNLOAD_BYTES_CEILING`] (16 GiB). The download fails, writing
    /// nothing past it, when the worker reports or sends more.
    pub max_archive_bytes: u64,
    /// Most bytes the archive may unpack to, in all and in any one file: 1 to
    /// [`archive_install::MAX_INSTALLED_BYTES_CEILING`] (32 GiB). The archive may hold at most
    /// [`archive_install::MAX_ARCHIVE_ENTRIES`] members. The install stops at the first member or
    /// byte past either, before the previous install is touched.
    pub max_installed_bytes: u64,
}

/// The shared-store copy of the catalogued Ultra model. All fields are host values.
#[derive(Clone, Copy, Debug)]
pub struct SharedModel<'a> {
    /// Owner id recorded in the store's `refs/` on success: one plain name.
    pub app_id: &'a str,
    pub grant_model: &'a str,
    pub route: &'a str,
    pub worker_base: &'a str,
    pub platform: &'a str,
    pub device_id: &'a str,
    /// Budget of the grant request: 60 s to 24 h.
    pub timeout: Duration,
}

/// One path component: 1 to 128 bytes of `A-Za-z0-9._-`, not starting with `.`, no `..`
/// ([`archive_install::is_plain_name`], the one check every name passes before it reaches a path).
pub fn plain_name(value: &str) -> bool {
    archive_install::is_plain_name(value)
}

fn tag_ok(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

fn text_ok(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

fn route_ok(route: &str) -> bool {
    route.starts_with('/')
        && route.len() <= 256
        && !route.contains("..")
        && !route.chars().any(|c| c.is_control() || c.is_whitespace())
}

fn base_ok(base: &str) -> bool {
    base.len() <= 512
        && base
            .strip_prefix("https://")
            .is_some_and(|rest| !rest.is_empty())
        && !base.chars().any(|c| c.is_control() || c.is_whitespace())
}

fn timeout_ok(timeout: Duration) -> bool {
    timeout >= Duration::from_secs(60) && timeout <= Duration::from_secs(24 * 60 * 60)
}

impl FlowText<'_> {
    fn validate(&self) -> Result<(), String> {
        let all = [
            self.downloading_named,
            self.downloading,
            self.verifying,
            self.installing_archive,
            self.installing_shared,
            self.preparing,
            self.ready,
            self.checksum_missing,
            self.incomplete_archive,
        ];
        if all.iter().all(|text| text_ok(text, 256)) {
            Ok(())
        } else {
            Err("model flow text is invalid".to_string())
        }
    }
}

impl ArchiveModel<'_> {
    fn validate(&self) -> Result<(), String> {
        let ok = tag_ok(self.grant_model, 64)
            && route_ok(self.route)
            && plain_name(self.archive_top_level)
            && plain_name(self.install_subdir)
            && base_ok(self.worker_base)
            && tag_ok(self.platform, 16)
            && text_ok(self.device_id, 256)
            && timeout_ok(self.timeout)
            && (64 * 1024..=64 * 1024 * 1024).contains(&self.progress_step_bytes)
            && (1..=resumable_download::MAX_DOWNLOAD_BYTES_CEILING)
                .contains(&self.max_archive_bytes)
            && (1..=archive_install::MAX_INSTALLED_BYTES_CEILING)
                .contains(&self.max_installed_bytes);
        if ok {
            Ok(())
        } else {
            Err("model download input is invalid".to_string())
        }
    }
}

impl SharedModel<'_> {
    fn validate(&self) -> Result<(), String> {
        let ok = plain_name(self.app_id)
            && tag_ok(self.grant_model, 64)
            && route_ok(self.route)
            && base_ok(self.worker_base)
            && tag_ok(self.platform, 16)
            && text_ok(self.device_id, 256)
            && timeout_ok(self.timeout);
        if ok {
            Ok(())
        } else {
            Err("shared model download input is invalid".to_string())
        }
    }
}

/// Ensure the archive-served `model` is installed and ready, downloading it when it is not.
///
/// Order: readiness check (a ready install returns after one `Installed` report and touches no
/// network); checksum check; models directory; resumable download with a fresh grant (one refresh
/// when the worker rejects the first); install; discard of the verified archive; readiness check.
/// The archive is committed only after its SHA-256 matches the pin, so a `.part` file is never
/// installed. Errors carry no path and no file content.
pub fn ensure_archive_model(
    model: &ArchiveModel<'_>,
    text: &FlowText<'_>,
    emit: ProgressSink<'_>,
) -> Result<(), String> {
    model.validate()?;
    text.validate()?;
    let models = model_paths::install_base();
    let dst = models.join(model.install_subdir);
    if model_paths::local_asr_ready(&dst, model.install_subdir) {
        emit(Phase::Installed, 1, 1, text.ready);
        return Ok(());
    }

    let expected_sha = model
        .sha256
        .map(str::trim)
        .filter(|sha| ultra_model::is_sha256_hex(sha))
        .ok_or_else(|| text.checksum_missing.to_string())?;
    std::fs::create_dir_all(&models).map_err(|e| format!("create models dir: {e}"))?;

    let url = format!("{}{}", model.worker_base.trim_end_matches('/'), model.route);
    let tmp = whisper_model::partial_archive_path(&models, model.install_subdir, expected_sha)?;
    emit(Phase::Downloading, 0, 0, text.downloading_named);

    let agent = resumable_download::download_client(model.timeout);
    let mut last_emit = 0u64;
    let download = resumable_download::download_resumable_with_authorization_refresh(
        &agent,
        resumable_download::RefreshingDownloadRequest {
            url: &url,
            part_path: &tmp,
            expected_sha256: expected_sha,
            max_bytes: model.max_archive_bytes,
        },
        || {
            model_download_grant::acquire_model_grant(
                model_download_grant::ModelGrantRequest {
                    worker_base: model.worker_base,
                    route: model.route,
                    model: model.grant_model,
                    platform: model.platform,
                    sha256: expected_sha,
                    device_id: model.device_id,
                },
                model.timeout,
            )
            .map(|token| format!("Bearer {token}"))
        },
        |received, total| {
            if received < last_emit {
                last_emit = 0;
            }
            if received.saturating_sub(last_emit) > model.progress_step_bytes {
                emit(Phase::Downloading, received, total, text.downloading);
                last_emit = received;
            }
        },
    )?;
    let received = download.received;
    let total = download.total;

    emit(Phase::Extracting, received, total, text.installing_archive);
    archive_install::install_single_top_level_tar_gz_as(
        &download.archive_path,
        &models,
        model.archive_top_level,
        model.install_subdir,
        &archive_install::extract_limits(model.max_installed_bytes),
    )?;
    resumable_download::discard_partial(&tmp);
    if !model_paths::local_asr_ready(&dst, model.install_subdir) {
        return Err(text.incomplete_archive.to_string());
    }
    emit(Phase::Preparing, received, total, text.preparing);
    emit(Phase::Installed, received, total, text.ready);
    Ok(())
}

/// Ensure a verified shared-store copy of the catalogued Ultra model.
///
/// Fails closed: the pinned checksum must be real and the store catalog must offer exactly the
/// catalogued model with that checksum, else nothing is fetched. A grant is acquired only when the
/// store copy is not already `Ready` under full verification. The store (`rightkit_models::suite`)
/// verifies, repairs, installs and registers `app_id`. The `Installed` report is the caller's to
/// send after its own activation succeeds.
pub fn fetch_shared_speech_model(
    model: &SharedModel<'_>,
    text: &FlowText<'_>,
    emit: ProgressSink<'_>,
) -> Result<(), String> {
    model.validate()?;
    text.validate()?;
    let spec = ultra_model::spec();
    // Fail closed: Ultra's checksum must be pinned in this build and the shared
    // catalog must offer exactly Ultra with that checksum, else nothing is fetched.
    let archive_sha256 =
        ultra_model::check_catalog(spec.id, shared::catalog_archive_sha256(&spec))?;
    let root = store::paths::models_root();
    let agent = shared::http_client();
    let status = store::status_at(&root, &spec, store::VerifyMode::Full);
    let authorization = if status.state == store::EntryState::Ready {
        None
    } else {
        Some(format!(
            "Bearer {}",
            model_download_grant::acquire_model_grant(
                model_download_grant::ModelGrantRequest {
                    worker_base: model.worker_base,
                    route: model.route,
                    model: model.grant_model,
                    platform: model.platform,
                    sha256: archive_sha256,
                    device_id: model.device_id,
                },
                model.timeout,
            )?
        ))
    };
    store::ensure_ready(
        &agent,
        &root,
        &spec,
        &store::EnsureOptions {
            seed_dir: None,
            authorization: authorization.as_deref(),
            app_id: model.app_id,
        },
        |progress| {
            let (phase, received, total, message) = match progress {
                store::Progress::Verify => (Phase::Verifying, 0, 0, text.verifying),
                store::Progress::Download {
                    received, total, ..
                } => (Phase::Downloading, received, total, text.downloading),
                store::Progress::Install => (Phase::Extracting, 0, 0, text.installing_shared),
                store::Progress::Done => (Phase::Preparing, 1, 1, text.preparing),
            };
            emit(phase, received, total, message);
        },
    )?;
    Ok(())
}
