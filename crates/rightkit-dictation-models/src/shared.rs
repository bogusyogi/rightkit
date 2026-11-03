//! The app adapter for the canonical Orthic Labs store. No store mechanics
//! or trusted hashes are duplicated here; ScrapeRight owns that crate.
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

pub use rightkit_models::suite as store;
/// Shared-store download transport matching the former `store::http_agent()`: no overall,
/// connect, header or idle limit, and no automatic retry.
pub fn http_client() -> rightkit_models::http::ModelHttp {
    crate::resumable_download::unlimited_download_client()
}
/// The store owner id this adapter registers speech-model references under. It is the on-disk
/// reference file name (`refs/heardright.json`) and the one id `rightkit-models` lets hold
/// speech models, so it stays as existing installs wrote it.
pub const APP_ID: &str = "heardright";
// Keep legacy aliases readable through both 0.1.222 & 0.1.223. Cleanup is not
// automatic at the boundary: removing compatibility requires an explicit release.
pub const LEGACY_COMPAT_THROUGH: &str = "0.1.223";

/// Whether the host app at `app_version` (its own release version, e.g. HeardRight's
/// `package.json` version) still needs the legacy aliases. An unparsable version keeps them.
pub fn legacy_compatibility_required(app_version: &str) -> bool {
    fn parts(version: &str) -> Option<Vec<u64>> {
        version.split('.').map(|v| v.parse().ok()).collect()
    }
    parts(app_version)
        .zip(parts(LEGACY_COMPAT_THROUGH))
        .map(|(version, cutoff)| version <= cutoff)
        .unwrap_or(true)
}

/// File in the app data folder that `prepare` writes after a successful native load of the
/// shared Ultra copy: `{"id", "revision", "path"}`.
const ACTIVATION_RECORD: &str = "shared-model-activation.json";

/// What this process knows about the shared Ultra copy.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum Activation {
    /// Never prepared in this process, a prepare is running, or the last prepare failed for a
    /// reason that says nothing about the copy (the Parakeet lease is held by another app, a
    /// migration is deferred, an I/O error). The persisted record may select the copy.
    #[default]
    Unset,
    /// A prepare verified the copy in full (and loaded it, or found the load on record).
    Active(PathBuf),
    /// `deactivate()` ran, or a prepare failed on integrity: hash or `ensure_ready`
    /// verification, a manifest mismatch or a native load failure. Nothing selects the copy and
    /// the record is never read until a later prepare verifies it again.
    Denied,
}

static ACTIVE: OnceLock<Mutex<Activation>> = OnceLock::new();
/// The record fallback logs its first rejection per process, not every `resolve`.
static REJECTION_LOGGED: AtomicBool = AtomicBool::new(false);
thread_local! {
    static LOAD_CHECK: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

/// Install subdir of the current (Ultra) model; also the migration source name
/// under the app-private models root.
pub fn legacy_subdir() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        crate::model_paths::MACOS_TDT_CURRENT_SUBDIR
    }
    #[cfg(not(target_os = "macos"))]
    {
        crate::model_paths::WINDOWS_TDT_CURRENT_SUBDIR
    }
}

/// Archive checksum pinned by a catalog entry, when it is an archive source.
pub fn catalog_archive_sha256(spec: &store::ModelSpec) -> Option<&'static str> {
    match &spec.source {
        store::catalog::SpecSource::Archive { sha256, .. } => *sha256,
        _ => None,
    }
}

/// Fail-closed gate: Ultra's checksum is pinned here and the shared catalog
/// offers exactly Ultra with that checksum. Returns the pinned checksum.
pub fn ultra_catalog_pin() -> Result<&'static str, String> {
    let spec = crate::ultra_model::spec();
    crate::ultra_model::check_catalog(spec.id, catalog_archive_sha256(&spec))
}

/// True when an Ultra download could start. While false (checksum
/// placeholders, or a catalog that still offers a retired id) nothing is
/// downloaded, activated or prompted, and retired models keep serving.
pub fn ultra_downloadable() -> bool {
    ultra_catalog_pin().is_ok()
}

/// Reclaim the shared-store entries Ultra supersedes (#298).
///
/// rightkit-models never prunes an entry an app still references, our own included, so once
/// Ultra is Ready the app releases its references to every id Ultra's spec retires and then
/// prunes them. Idempotent and safe to call any time: nothing happens unless Ultra is Ready in the
/// store, and the store keeps an entry another app still references, one with an install or
/// entry lock in progress, and everything while any app holds a Parakeet runtime lease (our own
/// engine included). Call it at activation (releases the references) and again at launch before
/// the engine starts (the prune then succeeds).
pub fn reclaim_retired_store_entries() -> Result<store::RetiredPruneReport, String> {
    let spec = crate::ultra_model::spec();
    // The retired models are the fallback until Ultra has actually loaded here: `prepare`
    // writes the activation record only after a successful native load of Ultra.
    if !activation_recorded_for(&record_path(), &spec) {
        return Ok(store::RetiredPruneReport::default());
    }
    reclaim_retired_store_entries_at(&store::paths::models_root(), &spec)
}

/// True when `record` names `spec` at its current revision (written by [`prepare`] after a
/// successful native load).
fn activation_recorded_for(record: &Path, spec: &store::ModelSpec) -> bool {
    std::fs::read(record)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .is_some_and(|value| value["id"] == spec.id && value["revision"] == spec.version)
}

fn reclaim_retired_store_entries_at(
    root: &Path,
    spec: &store::ModelSpec,
) -> Result<store::RetiredPruneReport, String> {
    if store::status_at(root, spec, store::VerifyMode::Size).state != store::EntryState::Ready {
        return Ok(store::RetiredPruneReport::default());
    }
    store::release_models(root, APP_ID, spec.retired_store_ids)?;
    strip_foreign_speech_refs(root, spec.retired_store_ids);
    let report = store::prune_retired(root, spec, APP_ID)?;
    if !report.removed.is_empty() || !report.failed.is_empty() {
        crate::telemetry::log_message(
            "retired_store_entries_reclaimed",
            &format!(
                "removed={:?} kept={:?} failed={:?}",
                report.removed, report.kept, report.failed
            ),
        );
    }
    Ok(report)
}

/// The app is the only owner of speech models: no other app uses them directly, so a
/// `refs/<app>.json` that still names a retired speech id is a stale leftover, never a dependent.
/// rightkit-models 0.5.2 already ignores a foreign speech ref when it decides what protects an
/// entry (rightkit#1316), so this is tidying, not a precondition of the prune: the retired ids are
/// removed from every other app's ref file so no stale leftover names a deleted model. Best
/// effort: an unreadable or unparseable ref file is left alone.
fn strip_foreign_speech_refs(root: &Path, retired_ids: &[&str]) {
    let Ok(entries) = std::fs::read_dir(root.join("refs")) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            continue;
        };
        let Some(models) = value.get_mut("models").and_then(|m| m.as_array_mut()) else {
            continue;
        };
        let before = models.len();
        models.retain(|m| {
            m.as_str()
                .is_none_or(|id| !retired_ids.iter().any(|r| r.eq_ignore_ascii_case(id)))
        });
        if models.len() == before {
            continue;
        }
        if let Ok(text) = serde_json::to_string_pretty(&value) {
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, &path).is_err() {
                let _ = std::fs::remove_file(&tmp);
            }
        }
    }
}

/// Newest retired TDT subdir whose complete fileset sits in the shared store.
/// Retired entries are fallbacks only until Ultra is Ready; then
/// [`reclaim_retired_store_entries`] releases and prunes them.
pub fn retired_subdir_in_store() -> Option<&'static str> {
    let root = store::paths::models_root();
    crate::model_paths::RETIRED_TDT_MODELS
        .iter()
        .find(|(subdir, id)| crate::model_paths::local_asr_ready(&root.join(id), subdir))
        .map(|(subdir, _)| *subdir)
}

/// The shared-store directory of retired model `subdir`, when complete.
pub fn resolve_retired(subdir: &str) -> Option<PathBuf> {
    let (_, id) = crate::model_paths::RETIRED_TDT_MODELS
        .iter()
        .find(|(retired, _)| *retired == subdir)?;
    let dir = store::paths::models_root().join(id);
    crate::model_paths::local_asr_ready(&dir, subdir).then_some(dir)
}

pub fn is_load_check() -> bool {
    LOAD_CHECK.with(|dir| dir.borrow().is_some())
}

/// Scoped production-loader override, used only while the idle runtime guard
/// owns activation. RAII restores resolution even if the callback unwinds.
pub fn with_load_check<T>(dir: &Path, action: impl FnOnce() -> T) -> T {
    struct Reset(Option<PathBuf>);
    impl Drop for Reset {
        fn drop(&mut self) {
            LOAD_CHECK.with(|dir| *dir.borrow_mut() = self.0.take());
        }
    }
    let _reset = Reset(LOAD_CHECK.with(|slot| slot.replace(Some(dir.to_path_buf()))));
    action()
}

fn record_path() -> PathBuf {
    crate::app_paths::app_data_file(ACTIVATION_RECORD)
}

fn activation() -> Activation {
    // A poisoned slot fails closed.
    ACTIVE
        .get_or_init(Default::default)
        .lock()
        .map(|slot| slot.clone())
        .unwrap_or(Activation::Denied)
}

fn set_activation(next: Activation) {
    *ACTIVE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = next;
}

/// The path a prepare of this process verified, still Ready by size.
fn verified_active_dir() -> Option<PathBuf> {
    let Activation::Active(active) = activation() else {
        return None;
    };
    let spec = crate::ultra_model::spec();
    let root = active.parent()?;
    store::resolve_ready_dir(root, &spec).filter(|dir| *dir == active)
}

/// The shared Ultra directory for `subdir`, for every path-selection caller: engine launch frames,
/// model status, the replacement prompt, the engine's one-shot roles.
///
/// 1. A scoped load check.
/// 2. This process's own verified activation (`Activation::Active`).
/// 3. Only while nothing was prepared here (`Activation::Unset`): the persisted activation
///    record, when it names this exact Ultra id and revision at the path the store reports Ready.
///    `prepare` writes it after a native load of that path, so a relaunched process (onboarding
///    resumed after a restart) selects the copy without repeating the verification. It is a
///    selection hint only: nothing destructive may rest on it (see [`resolve_verified`]).
/// 4. `Activation::Denied` resolves nothing, record or not.
pub fn resolve(subdir: &str) -> Option<PathBuf> {
    if subdir != legacy_subdir() {
        return None;
    }
    if let Some(dir) = LOAD_CHECK.with(|slot| slot.borrow().clone()) {
        return Some(dir);
    }
    match activation() {
        Activation::Active(_) => verified_active_dir(),
        Activation::Unset => recorded_dir(),
        Activation::Denied => None,
    }
}

/// [`resolve`] without the record fallback and without a load check: only a copy this process
/// verified in full. Every caller that deletes or finalizes something on the strength of Ultra
/// being active uses this, never [`resolve`].
pub fn resolve_verified(subdir: &str) -> Option<PathBuf> {
    (subdir == legacy_subdir())
        .then(verified_active_dir)
        .flatten()
}

/// True once a prepare in this process verified the shared Ultra copy in full. False on the record
/// path, after `deactivate()` and after an integrity failure.
pub fn is_verified_active() -> bool {
    verified_active_dir().is_some()
}

/// Ultra's store directory when the activation record names it, read cheaply: one small file, one
/// size-level store check, no hash and no lock. `None` for a missing, torn or foreign record, a
/// store entry that is not Ready, and a path that is not the one the record names. The record
/// file is checked before `models_root()` is called, which fails closed in a test-mode build
/// without a suite override.
fn recorded_dir() -> Option<PathBuf> {
    let spec = crate::ultra_model::spec();
    // The app activates only the id it pins (same guard as `prepare_at`).
    if spec.id != legacy_subdir() {
        return None;
    }
    let record = record_path();
    if !record.is_file() {
        return None;
    }
    let Some(value) = read_record(&record) else {
        note_record_rejected("unreadable record");
        return None;
    };
    if !record_names(&value, &spec) {
        note_record_rejected("record names another model or revision");
        return None;
    }
    let dir = store::resolve_ready_dir(&store::paths::models_root(), &spec);
    match dir {
        Some(dir) if record_path_is(&value, &dir) => Some(dir),
        _ => {
            note_record_rejected("store entry is not Ready at the recorded path");
            None
        }
    }
}

fn read_record(record: &Path) -> Option<serde_json::Value> {
    std::fs::read(record)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

fn record_names(value: &serde_json::Value, spec: &store::ModelSpec) -> bool {
    value["id"] == spec.id && value["revision"] == spec.version
}

/// Path-wise, never by comparing JSON values.
fn record_path_is(value: &serde_json::Value, dir: &Path) -> bool {
    value["path"].as_str().is_some_and(|path| path == dir)
}

/// True when `record` names `spec` at `dir`: a native load of this exact path already succeeded.
fn record_matches_dir(record: &Path, spec: &store::ModelSpec, dir: &Path) -> bool {
    read_record(record)
        .is_some_and(|value| record_names(&value, spec) && record_path_is(&value, dir))
}

fn note_record_rejected(reason: &str) {
    if !REJECTION_LOGGED.swap(true, Ordering::Relaxed) {
        crate::telemetry::log_message("shared_model_record_rejected", reason);
    }
}

/// Withdraw the shared copy's activation for the rest of this process: the shell's rollback, and
/// the state after an integrity failure. Only a later successful [`prepare`] lifts it.
pub fn deactivate() {
    set_activation(Activation::Denied);
}

/// Test-only: forget everything this process knows about the shared copy.
#[cfg(feature = "test-mode")]
pub fn reset_activation_for_tests() {
    set_activation(Activation::Unset);
    REJECTION_LOGGED.store(false, Ordering::Relaxed);
}

/// Test-only: mark `dir` as verified in this process, as a successful prepare would.
#[cfg(feature = "test-mode")]
pub fn activate_for_tests(dir: &Path) {
    set_activation(Activation::Active(dir.to_path_buf()));
}

/// Called before the first resident load, under the shell's idle guard.
/// Store performs verification, journaling, cross-process locks & rollback;
/// callback must load the published path and unload before returning.
///
/// A matching activation record means a native load of this exact path already succeeded: the
/// full pin verification still runs, but neither the load nor the Parakeet lease is needed, so a
/// sibling app's speech service holding the lease cannot fail the activation.
pub fn prepare(load: &dyn Fn(&Path) -> Result<(), String>) -> Result<Option<PathBuf>, String> {
    let spec = crate::ultra_model::spec();
    let record = record_path();
    let receipt_failed = Cell::new(false);
    prepare_at_inner(
        &store::paths::models_root(),
        &crate::model_paths::install_base(),
        &record,
        &|dir| {
            // Full pin verification still runs on every launch. A successful native
            // load at this exact immutable path need only be repeated after a move;
            // ordinary launches do one resident load, not a test load plus restart.
            if record_matches_dir(&record, &spec, dir) {
                return Ok(());
            }
            load(dir)?;
            write_record(&record, &spec, dir).inspect_err(|_| receipt_failed.set(true))
        },
        &|dir| record_matches_dir(&record, &spec, dir),
        &receipt_failed,
    )
}

fn write_record(record: &Path, spec: &store::ModelSpec, dir: &Path) -> Result<(), String> {
    let identity = serde_json::json!({
        "id": spec.id,
        "revision": spec.version,
        "path": serde_json::to_value(dir).map_err(|e| e.to_string())?,
    });
    if let Some(parent) = record.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    // A torn optional receipt simply causes another native load check.
    rightkit_fs::atomic_write(
        record,
        &serde_json::to_vec(&identity).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("record shared model activation: {e}"))
}

pub fn prepare_at(
    root: &Path,
    legacy_root: &Path,
    load: &dyn Fn(&Path) -> Result<(), String>,
) -> Result<Option<PathBuf>, String> {
    prepare_at_inner(
        root,
        legacy_root,
        &record_path(),
        load,
        &|_| false,
        &Cell::new(false),
    )
}

/// Why a prepare failed. Only an integrity failure denies the copy; contention, a deferral or an
/// I/O error says nothing about it.
struct PrepareFailure {
    message: String,
    integrity: bool,
}

impl PrepareFailure {
    fn integrity(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            integrity: true,
        }
    }
    fn other(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            integrity: false,
        }
    }
}

/// `recorded(dir)`: a native load of `dir` is on record, so none will run. `receipt_failed`: set
/// by `load` when it failed after a successful native load (the activation record could not be
/// written), which is not an integrity failure either.
fn prepare_at_inner(
    root: &Path,
    legacy_root: &Path,
    record: &Path,
    load: &dyn Fn(&Path) -> Result<(), String>,
    recorded: &dyn Fn(&Path) -> bool,
    receipt_failed: &Cell<bool>,
) -> Result<Option<PathBuf>, String> {
    let spec = crate::ultra_model::spec();
    // The app activates only the model id it pins. A catalog that still
    // offers a retired id must not be resolved under the Ultra subdir.
    if spec.id != legacy_subdir() {
        crate::telemetry::log_message(
            "shared_model_catalog_mismatch",
            &format!("catalog={} expected={}", spec.id, legacy_subdir()),
        );
        return Ok(None);
    }
    // The state is untouched while this runs: the full verification hashes ~700 MB, and other
    // threads (status, the launch reclaim) must neither lose the model nor gain a new one.
    match prepare_variant_checked(
        root,
        &legacy_root.join(legacy_subdir()),
        &spec,
        load,
        recorded,
    ) {
        Ok(Some(dir)) => {
            set_activation(Activation::Active(dir.clone()));
            Ok(Some(dir))
        }
        Ok(None) => {
            // Nothing is activatable now: an earlier activation no longer stands, but a Denied
            // state stays Denied.
            if matches!(activation(), Activation::Active(_)) {
                set_activation(Activation::Unset);
            }
            Ok(None)
        }
        Err(failure) => {
            if failure.integrity && !receipt_failed.get() {
                set_activation(Activation::Denied);
                reject_record(record);
            }
            Err(failure.message)
        }
    }
}

/// Move the activation record aside so the next process cannot trust a copy known to be bad.
fn reject_record(record: &Path) {
    if !record.exists() {
        return;
    }
    let rejected = record.with_extension("json.rejected");
    let _ = std::fs::remove_file(&rejected);
    if std::fs::rename(record, &rejected).is_err() {
        let _ = std::fs::remove_file(record);
    }
    crate::telemetry::log_message(
        "shared_model_record_rejected",
        "activation record moved aside after a failed verification",
    );
}

fn prepare_variant_checked(
    root: &Path,
    source: &Path,
    spec: &store::ModelSpec,
    load: &dyn Fn(&Path) -> Result<(), String>,
    recorded: &dyn Fn(&Path) -> bool,
) -> Result<Option<PathBuf>, PrepareFailure> {
    let journal = store::migration::journal_path(root, spec.id);
    let loaded = std::cell::Cell::new(false);
    let check = |dir: &Path| {
        load(dir)?;
        loaded.set(true);
        Ok(())
    };
    if source.exists() || journal.exists() {
        let result = store::migrate_source_checked(root, spec, source, APP_ID, &check);
        if !result.errors.is_empty() {
            return Err(PrepareFailure::integrity(result.errors.join("; ")));
        }
        if result.deferred {
            return Ok(None);
        }
    }
    let status = store::status_at(root, spec, store::VerifyMode::Size);
    if status.state != store::EntryState::Ready {
        return Ok(None);
    }
    // ensure_ready's verified branch registers our ref atomically without any
    // network request or download grant. Migration may already have done so.
    if !loaded.get() {
        // ENG-04 (`RIGHTKIT_MODEL_VERIFY=receipt`, test-mode): an unchanged stat receipt from the last
        // full verification skips the hash and only registers the ref. Default: full, as shipped.
        let receipt =
            crate::verify_receipt::policy() == crate::verify_receipt::VerifyPolicy::Receipt;
        if receipt && crate::verify_receipt::matches(&status.dir, spec) {
            store::refs::register(&store::paths::model_refs_dir_at(root), APP_ID, spec.id)
                .map_err(PrepareFailure::other)?;
            crate::telemetry::log_message("shared_model_verify", "policy=receipt result=hit");
        } else {
            store::ensure_ready(
                &http_client(),
                root,
                spec,
                &store::EnsureOptions {
                    seed_dir: None,
                    authorization: None,
                    app_id: APP_ID,
                },
                |_| {},
            )
            .map_err(PrepareFailure::integrity)?;
            if receipt {
                crate::verify_receipt::record(&status.dir, spec);
                crate::telemetry::log_message("shared_model_verify", "policy=receipt result=full");
            }
        }
    }
    // Completed migration also tested this path; existing/shared-only copies
    // still need the exact same production load check before selection, unless a load of this
    // exact path is already on record: then nothing loads, so no lease is taken either.
    if !loaded.get() && !recorded(&status.dir) {
        let suite = root
            .parent()
            .ok_or_else(|| PrepareFailure::other("shared models root has no parent"))?;
        let legacy_suite = store::paths::legacy_suite_root();
        let roots = if root == store::paths::models_root() {
            vec![suite, legacy_suite.as_path()]
        } else {
            vec![suite]
        };
        let _lease = store::lease::try_acquire_at_roots(&roots, "parakeet-tdt-v3", APP_ID)
            .map_err(PrepareFailure::other)?
            .ok_or_else(|| PrepareFailure::other("Speech model is in use; activation deferred."))?;
        load(&status.dir).map_err(PrepareFailure::integrity)?;
    }
    Ok(Some(status.dir))
}

pub fn unregister() -> Result<(), String> {
    let result = store::refs::unregister_across_roots(
        &store::paths::legacy_suite_root().join("models"),
        &store::paths::models_root(),
        APP_ID,
    )
    .map(|_| ());
    // Once the app holds no reference, a reinstall that keeps app data must verify and register
    // the copy again before anything selects it; a surviving record would skip that.
    let _ = std::fs::remove_file(record_path());
    result
}
