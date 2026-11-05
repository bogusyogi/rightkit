//! Console-free Windows entrypoint for the installed resident service. No shell wrapper or
//! visible console host sits between its lifecycle owner and the resident service.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

use rightkit_repo_graph::{
    BlueprintRequest, CancellationToken, NativeService, Operation, ServiceStatus,
};

const RESIDENT_SUPERVISOR_STOP_TIMEOUT: Duration = Duration::from_secs(5);

struct BackgroundCancellationEpoch {
    blueprint: CancellationToken,
    ledger: tokio_util::sync::CancellationToken,
}

impl BackgroundCancellationEpoch {
    fn fresh() -> Self {
        Self {
            blueprint: CancellationToken::new(),
            ledger: tokio_util::sync::CancellationToken::new(),
        }
    }
}

#[derive(Clone)]
pub struct LifecycleControl {
    admission_open: Arc<AtomicBool>,
    background_authority: Arc<AtomicBool>,
    shutdown_requested: Arc<AtomicBool>,
    background_cancellation: Arc<Mutex<BackgroundCancellationEpoch>>,
    shutdown_cancellation: tokio_util::sync::CancellationToken,
    command: Arc<Mutex<Option<String>>>,
    failure: Arc<Mutex<Option<String>>>,
    /// Waits that a drain must cut short (see `new_wake`).
    wakes: Arc<Mutex<Vec<crate::wake::Wake>>>,
}

impl Default for LifecycleControl {
    fn default() -> Self {
        Self {
            admission_open: Arc::new(AtomicBool::new(true)),
            background_authority: Arc::new(AtomicBool::new(false)),
            shutdown_requested: Arc::new(AtomicBool::new(false)),
            background_cancellation: Arc::new(Mutex::new(BackgroundCancellationEpoch::fresh())),
            shutdown_cancellation: tokio_util::sync::CancellationToken::new(),
            command: Arc::new(Mutex::new(None)),
            failure: Arc::new(Mutex::new(None)),
            wakes: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl LifecycleControl {
    /// A wait that `request_drain` closes, so a loop blocked on it stops at once instead of
    /// waiting out its backstop. Already closed when the drain has begun.
    pub(crate) fn new_wake(&self) -> crate::wake::Wake {
        let wake = crate::wake::Wake::new();
        if let Ok(mut wakes) = self.wakes.lock() {
            // Drop wakes whose loop has ended (only this list still holds them).
            wakes.retain(|held| held.is_shared());
            wakes.push(wake.clone());
        }
        if self.shutdown_requested() {
            wake.close();
        }
        wake
    }

    pub fn admission_open(&self) -> bool {
        self.admission_open.load(Ordering::Acquire)
    }

    pub fn shutdown_requested(&self) -> bool {
        self.shutdown_requested.load(Ordering::Acquire)
    }

    /// Descendant work observes engine drain without owning engine lifetime.
    pub(crate) fn cancellation_token(&self) -> tokio_util::sync::CancellationToken {
        self.shutdown_cancellation.child_token()
    }

    /// Background work (watcher, refresh, indexers) is authorized for the
    /// whole life of the resident and drains only with the engine.
    pub fn background_authority_open(&self) -> bool {
        if self.background_cancellation.is_poisoned() {
            self.background_authority.store(false, Ordering::Release);
            return false;
        }
        self.background_authority.load(Ordering::Acquire)
    }

    pub(crate) fn background_cancellation_token(&self) -> tokio_util::sync::CancellationToken {
        self.background_cancellation
            .lock()
            .map(|epoch| epoch.ledger.child_token())
            .unwrap_or_else(|_| {
                let token = tokio_util::sync::CancellationToken::new();
                token.cancel();
                token
            })
    }

    pub(crate) fn background_blueprint_cancellation_token(&self) -> CancellationToken {
        self.background_cancellation
            .lock()
            .map(|epoch| epoch.blueprint.clone())
            .unwrap_or_else(|_| {
                let token = CancellationToken::new();
                token.cancel();
                token
            })
    }

    pub fn grant_background(&self, reason: &str) {
        if self.shutdown_requested() {
            return;
        }
        let Ok(mut token) = self.background_cancellation.lock() else {
            self.background_authority.store(false, Ordering::Release);
            return;
        };
        if self.shutdown_requested() {
            return;
        }
        let was_open = self.background_authority.load(Ordering::Acquire);
        if !was_open {
            *token = BackgroundCancellationEpoch::fresh();
            self.background_authority.store(true, Ordering::Release);
            emit_lifecycle("background_authority_granted", Some(reason));
            drop(token);
            self.notify_wakes();
        }
    }

    pub fn drain_background(&self, reason: &str) {
        let Ok(token) = self.background_cancellation.lock() else {
            self.background_authority.store(false, Ordering::Release);
            return;
        };
        let was_open = self.background_authority.swap(false, Ordering::AcqRel);
        if was_open {
            token.blueprint.cancel();
            token.ledger.cancel();
            emit_lifecycle("background_authority_drained", Some(reason));
            drop(token);
            self.notify_wakes();
        }
    }

    /// Signals every registered wait that authority changed, so it re-evaluates now.
    fn notify_wakes(&self) {
        if let Ok(wakes) = self.wakes.lock() {
            for wake in wakes.iter() {
                wake.notify();
            }
        }
    }

    pub fn request_drain(&self, command: Option<&str>) {
        if let Some(command) = command {
            if let Ok(mut current) = self.command.lock() {
                if current.is_none() {
                    *current = Some(command.to_string());
                }
            }
        }
        self.admission_open.store(false, Ordering::Release);
        let first_drain = !self.shutdown_requested.swap(true, Ordering::AcqRel);
        match self.background_cancellation.lock() {
            Ok(token) => {
                self.background_authority.store(false, Ordering::Release);
                token.blueprint.cancel();
                token.ledger.cancel();
            }
            Err(_) => self.background_authority.store(false, Ordering::Release),
        }
        self.shutdown_cancellation.cancel();
        if let Ok(wakes) = self.wakes.lock() {
            for wake in wakes.iter() {
                wake.close();
            }
        }
        if first_drain {
            let reason = self
                .command()
                .or_else(|| self.failure())
                .unwrap_or_else(|| "unspecified".to_string());
            emit_lifecycle("drain_requested", Some(&reason));
        }
    }

    pub fn fail(&self, reason: impl Into<String>) {
        if let Ok(mut failure) = self.failure.lock() {
            if failure.is_none() {
                *failure = Some(reason.into());
            }
        }
        self.request_drain(None);
    }

    pub fn failure(&self) -> Option<String> {
        self.failure.lock().ok().and_then(|value| value.clone())
    }

    pub fn command(&self) -> Option<String> {
        self.command.lock().ok().and_then(|value| value.clone())
    }
}

static LIFECYCLE_CONTROL: OnceLock<RwLock<LifecycleControl>> = OnceLock::new();
static RESIDENT_ENGINE_ACTIVE: AtomicBool = AtomicBool::new(false);

struct ResidentRepo {
    root: String,
    service: Arc<NativeService>,
    generation_id: String,
    generation_complete: bool,
}

struct ResidentBlueprintState {
    repos: Vec<ResidentRepo>,
    enrolled_repo_count: u64,
    registry_error: Option<String>,
    cancellation: CancellationToken,
    /// Per-root build backoff: consecutive failure count and last attempt.
    /// Each attempt is deadline-bounded, but a repository that keeps failing
    /// (e.g. a stale multi-GB enrollment) would otherwise re-burn its whole
    /// budget every reconcile pass and starve the other enrolled roots.
    build_failures: std::collections::HashMap<String, (u32, Instant)>,
    /// Roots whose snapshot exceeds the size/entry limit: root -> (limit in
    /// force when measured, reason). Recorded once, never rebuilt until the
    /// limit changes (the failed attempt reads and digests the whole tree, so
    /// retrying it on a timer only burns disk and CPU).
    oversized: std::collections::HashMap<String, (u64, String)>,
    /// Missing/unavailable enrollment bindings already reported to logs.
    /// Reconciliation runs frequently; emit only transitions.
    skipped_roots: std::collections::BTreeMap<String, String>,
    /// Initialization completion is a transition, not a per-pass heartbeat.
    initialization_completed: bool,
    /// What the supervisor thread is currently doing. Stop timeouts name this
    /// stage so a drain timeout is attributable without a live debugger.
    supervisor_stage: Arc<Mutex<String>>,
    /// Panics the watcher thread has caught and restarted from (#555), and the last one's
    /// message with the stage it died in. Reported by status so `watcher_unavailable` is explained.
    watcher_panics: u64,
    last_watcher_panic: Option<String>,
}

fn set_supervisor_stage(stage: &Arc<Mutex<String>>, value: &str) {
    if let Ok(mut current) = stage.lock() {
        *current = value.to_owned();
    }
}

fn supervisor_stage(stage: &Arc<Mutex<String>>) -> String {
    stage
        .lock()
        .map(|current| current.clone())
        .unwrap_or_else(|_| "unknown".to_owned())
}

/// Bounded exponential cooldown between rebuild attempts for a root that
/// keeps failing: 15 min, doubling per consecutive failure, capped at 24 h.
/// A full rebuild of a large enrollment reads and digests the whole tree, so
/// a failing root must go quiet quickly (#435: 10-minute retries kept the Dell
/// at sustained disk/CPU load). Success removes the entry and resets it.
fn retry_cooldown(attempts: u32) -> Duration {
    let policy = rightkit_sync::BackoffPolicy {
        base_secs: 900.0,
        max_secs: 86_400.0,
        pending_floor_secs: 0.0,
    };
    Duration::from_secs_f64(policy.delay_secs(attempts, None))
}

fn retry_due(
    failures: &std::collections::HashMap<String, (u32, Instant)>,
    key: &str,
    now: Instant,
) -> bool {
    match failures.get(key) {
        Some((attempts, last)) => now.saturating_duration_since(*last) >= retry_cooldown(*attempts),
        None => true,
    }
}

/// On-disk Blueprint retry memory (#435). A relaunch used to forget every failing and
/// over-limit root, so each start repeated a full read and digest of each one. Wall-clock times
/// are stored because `Instant` does not survive the process.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct BlueprintRetryRecord {
    #[serde(default)]
    build_failures: std::collections::BTreeMap<String, RetryFailureRecord>,
    #[serde(default)]
    oversized: std::collections::BTreeMap<String, OversizedRecord>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct RetryFailureRecord {
    attempts: u32,
    last_failure_unix_ms: u64,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct OversizedRecord {
    limit: u64,
    reason: String,
}

type RetryMaps = (
    std::collections::HashMap<String, (u32, Instant)>,
    std::collections::HashMap<String, (u64, String)>,
);

fn blueprint_retry_path() -> PathBuf {
    crate::paths::state_root()
        .join("runtime")
        .join("blueprint-retry.json")
}

fn unix_ms_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// Read the persisted retry memory. A missing or unreadable file starts empty; an unreadable
/// file is logged once and replaced by the next save.
fn load_blueprint_retry() -> RetryMaps {
    let path = blueprint_retry_path();
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(_) => return Default::default(),
    };
    let record = match serde_json::from_str::<BlueprintRetryRecord>(&text) {
        Ok(record) => record,
        Err(error) => {
            crate::lifecycle_log::emit(serde_json::json!({
                "event": "resident_blueprint_retry_state",
                "stage": "unreadable",
                "error": error.to_string()
            }));
            return Default::default();
        }
    };
    let now_ms = unix_ms_now();
    let now = Instant::now();
    let failures = record
        .build_failures
        .into_iter()
        .map(|(root, failure)| {
            // A failure stamped in the future (clock change) counts as just now: the wait is
            // never shortened by a clock step.
            let age = Duration::from_millis(now_ms.saturating_sub(failure.last_failure_unix_ms));
            let last = now.checked_sub(age).unwrap_or(now);
            (root, (failure.attempts, last))
        })
        .collect();
    let oversized = record
        .oversized
        .into_iter()
        .map(|(root, entry)| (root, (entry.limit, entry.reason)))
        .collect();
    (failures, oversized)
}

/// Persist the retry memory after a change. Writes go to a sibling file and are renamed into
/// place, so a crash leaves either the old record or the new one.
fn save_blueprint_retry(
    failures: &std::collections::HashMap<String, (u32, Instant)>,
    oversized: &std::collections::HashMap<String, (u64, String)>,
) {
    let now_ms = unix_ms_now();
    let now = Instant::now();
    let record = BlueprintRetryRecord {
        build_failures: failures
            .iter()
            .map(|(root, (attempts, last))| {
                let age_ms = now.saturating_duration_since(*last).as_millis() as u64;
                (
                    root.clone(),
                    RetryFailureRecord {
                        attempts: *attempts,
                        last_failure_unix_ms: now_ms.saturating_sub(age_ms),
                    },
                )
            })
            .collect(),
        oversized: oversized
            .iter()
            .map(|(root, (limit, reason))| {
                (
                    root.clone(),
                    OversizedRecord {
                        limit: *limit,
                        reason: reason.clone(),
                    },
                )
            })
            .collect(),
    };
    let path = blueprint_retry_path();
    let Ok(bytes) = serde_json::to_vec_pretty(&record) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let staged = path.with_extension("json.tmp");
    let written = std::fs::write(&staged, &bytes).and_then(|_| std::fs::rename(&staged, &path));
    if let Err(error) = written {
        crate::lifecycle_log::emit(serde_json::json!({
            "event": "resident_blueprint_retry_state",
            "stage": "save_failed",
            "error": error.to_string()
        }));
    }
}

struct ResidentBlueprint {
    state: Arc<Mutex<ResidentBlueprintState>>,
    lifecycle: LifecycleControl,
    supervisor: Option<std::thread::JoinHandle<()>>,
    ledger_supervisor: Option<std::thread::JoinHandle<()>>,
}

static RESIDENT_BLUEPRINT: OnceLock<Mutex<Option<ResidentBlueprint>>> = OnceLock::new();

/// Start the one resident native Blueprint watcher for the installed Hub.
/// Enrollment is read from the canonical installation registry; no watcher
/// is created for an absent or malformed registry. The returned status is
/// consumed by health, while this same service owns polling, refresh and drain.
pub fn start_resident_blueprint() -> Result<(), String> {
    let slot = RESIDENT_BLUEPRINT.get_or_init(|| Mutex::new(None));
    let mut current = slot
        .lock()
        .map_err(|_| "resident Blueprint state unavailable".to_string())?;
    if let Some(existing) = current.as_mut() {
        let running = existing
            .state
            .lock()
            .map(|state| {
                (state.enrolled_repo_count == 0
                    || state.repos.len() == state.enrolled_repo_count as usize)
                    && state
                        .repos
                        .iter()
                        .all(|repo| repo.service.status() == ServiceStatus::Running)
            })
            .unwrap_or(false);
        if running {
            return Ok(());
        }
        // A failed supervisor leaves a terminal service object behind. Stop it
        // while the singleton slot is still occupied; never detach a worker
        // that may still own writable repository services.
        existing.stop_bounded(RESIDENT_SUPERVISOR_STOP_TIMEOUT)?;
    }
    current.take();
    let registry =
        crate::authorization::load_installation_registry().map_err(|error| error.to_string())?;
    let (roots, skipped_roots) = enrolled_roots(&registry)?;
    emit_skipped_root_events(&skipped_roots);
    let cancellation = CancellationToken::new();
    let supervisor_stage = Arc::new(Mutex::new(String::from("startup")));
    let enrolled_repo_count = roots.len() as u64;
    let (build_failures, oversized) = load_blueprint_retry();
    let state = Arc::new(Mutex::new(ResidentBlueprintState {
        enrolled_repo_count,
        repos: Vec::new(),
        registry_error: None,
        cancellation,
        build_failures,
        oversized,
        skipped_roots,
        initialization_completed: false,
        supervisor_stage: Arc::clone(&supervisor_stage),
        watcher_panics: 0,
        last_watcher_panic: None,
    }));
    let lifecycle = lifecycle_control();
    let supervised = Arc::clone(&state);
    let supervisor_lifecycle = lifecycle.clone();
    let supervisor = std::thread::Builder::new()
        .name("membrane-blueprint-resident-watcher".into())
        .spawn(move || {
            crate::lifecycle_log::emit(serde_json::json!({"event":"resident_blueprint_initialization", "stage":"started", "enrolledRepoCount": enrolled_repo_count}));
            let wake = supervisor_lifecycle.new_wake();
            // A panic in a pass (#555) is caught here, recorded in the resident status and the
            // lifecycle log, and the pass loop restarts after a bounded, doubling backoff. The
            // thread therefore never dies silently and leaves `watcher_unavailable` unexplained.
            let mut panics_in_a_row: u32 = 0;
            loop {
                let started = Instant::now();
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    // Idle backoff: an unchanged pass doubles the wait (250 ms up to 1 s);
                    // any change in resident state resets it to 250 ms.
                    let mut idle_wait = RESIDENT_SUPERVISE_MIN;
                    let mut last_fingerprint = String::new();
                    let mut last_registry_stat: Option<(std::time::SystemTime, u64)> = None;
                    let mut last_reconcile_at: Option<Instant> = None;
                    let mut authority_was_open = false;
                    // A registry change wakes the pass at once; without a live watch the bounded backoff
                    // polling stays in force, and a dead or unstarted watch is retried on the next pass.
                    let registry_path = crate::authorization::default_registry_path();
                    let start_registry_watch = || {
                        let path = registry_path.as_ref()?;
                        let dir = path.parent()?.to_path_buf();
                        let name = path.file_name()?.to_str()?.to_owned();
                        crate::wake::watch_dir_into(&dir, move |file| file.starts_with(&name), &wake)
                    };
                    let mut registry_watch = start_registry_watch();
                    while !supervisor_lifecycle.shutdown_requested() {
                        if !registry_watch.as_ref().is_some_and(|watch| watch.is_live()) {
                            registry_watch = start_registry_watch();
                        }
                        if supervisor_lifecycle.background_authority_open() {
                            set_supervisor_stage(&supervisor_stage, "supervise");
                            if !supervise_resident_repositories(&supervised) {
                                break;
                            }
                            // Skip the full registry reload when the registry file is
                            // unchanged and the resident is settled; reload on a stat
                            // change, after authority was closed, while any build is
                            // pending or failed, or at least every 60 s.
                            let stat = registry_file_stat();
                            let settled = resident_is_settled(&supervised);
                            let skip = settled
                                && authority_was_open
                                && stat.is_some()
                                && stat == last_registry_stat
                                && last_reconcile_at.is_some_and(|at| at.elapsed() < RESIDENT_RECONCILE_MAX);
                            if !skip {
                                set_supervisor_stage(&supervisor_stage, "reconcile");
                                reconcile_resident_repositories(
                                    &supervised,
                                    &supervisor_stage,
                                    supervisor_lifecycle.background_blueprint_cancellation_token(),
                                );
                                last_registry_stat = stat;
                                last_reconcile_at = Some(Instant::now());
                            }
                            authority_was_open = true;
                        } else {
                            authority_was_open = false;
                            // Holder loss drains automatic repository work while the
                            // engine/listener remains available for explicit calls.
                            set_supervisor_stage(&supervisor_stage, "holder_drain");
                            drain_resident_repositories(&supervised);
                        }
                        set_supervisor_stage(&supervisor_stage, "idle");
                        let fingerprint = resident_state_fingerprint(&supervised);
                        if fingerprint == last_fingerprint {
                            idle_wait = (idle_wait * 2).min(RESIDENT_SUPERVISE_MAX);
                        } else {
                            idle_wait = RESIDENT_SUPERVISE_MIN;
                            last_fingerprint = fingerprint;
                        }
                        // Settled with a working registry watch: block until a registry event, an
                        // authority change or a drain, with the 30 s backstop. Pending or failed builds
                        // (and a missing watch) keep the bounded retry cadence.
                        let registry_watched = registry_watch.as_ref().is_some_and(|watch| watch.is_live());
                        let wait_for = if registry_watched
                            && (!supervisor_lifecycle.background_authority_open()
                                || resident_is_settled(&supervised))
                        {
                            crate::wake::BACKSTOP
                        } else {
                            idle_wait
                        };
                        sleep_until_or_shutdown(&wake, wait_for);
                    }
                }));
                let Err(payload) = outcome else {
                    break;
                };
                // A pass that ran for a minute before panicking is a new incident, not a streak.
                if started.elapsed() >= RESIDENT_PANIC_STREAK_RESET {
                    panics_in_a_row = 0;
                }
                panics_in_a_row = panics_in_a_row.saturating_add(1);
                let backoff = resident_panic_backoff(panics_in_a_row);
                record_resident_watcher_panic(&supervised, &supervisor_stage, payload.as_ref(), backoff);
                sleep_until_or_shutdown(&wake, backoff);
                if supervisor_lifecycle.shutdown_requested() {
                    break;
                }
            }
            crate::lifecycle_log::emit(serde_json::json!({"event":"resident_blueprint_initialization", "stage":"stopped"}));
            drain_resident_repositories(&supervised);
        })
        .map_err(|error| {
            drain_resident_repositories(&state);
            format!("resident Blueprint supervisor unavailable: {error}")
        })?;
    // Ledger maintenance runs on its own thread under the same background
    // authority gate. Sharing the watcher supervisor starved barrier
    // observation for whole maintenance windows — freshness reads reported
    // `stale` while a large root synced. Its WorkBudget rides the lifecycle
    // cancellation token so drain aborts a mid-sync pass instead of waiting
    // out the per-root bound.
    let ledger_supervised = Arc::clone(&state);
    let ledger_lifecycle = lifecycle.clone();
    let ledger_supervisor = std::thread::Builder::new()
        .name("membrane-ledger-maintenance".into())
        .spawn(move || {
            let wake = ledger_lifecycle.new_wake();
            let mut next_at = Instant::now();
            while !ledger_lifecycle.shutdown_requested() {
                if ledger_lifecycle.background_authority_open() && Instant::now() >= next_at {
                    reconcile_resident_ledger(&ledger_supervised, &ledger_lifecycle);
                    reconcile_resident_transcripts(&ledger_lifecycle);
                    next_at = Instant::now() + LEDGER_MAINTENANCE_INTERVAL;
                }
                // Next maintenance is minutes away: sleep until it is due. A drain or an
                // authority change wakes this wait at once; the cap is the backstop.
                let wait_for = if ledger_lifecycle.background_authority_open() {
                    next_at
                        .saturating_duration_since(Instant::now())
                        .clamp(Duration::from_millis(1), crate::wake::BACKSTOP)
                } else {
                    // Gate closed: nothing to do until `grant_background` or a drain wakes us.
                    crate::wake::BACKSTOP
                };
                sleep_until_or_shutdown(&wake, wait_for);
            }
        });
    let ledger_supervisor = match ledger_supervisor {
        Ok(thread) => thread,
        Err(error) => {
            // Roll back the already-spawned watcher rather than detach a
            // writable worker the singleton does not own.
            if let Ok(state) = state.lock() {
                state.cancellation.cancel();
            }
            lifecycle.request_drain(Some("ledger_supervisor_spawn_failed"));
            let _ = supervisor.join();
            drain_resident_repositories(&state);
            return Err(format!("resident Ledger maintenance unavailable: {error}"));
        }
    };
    *current = Some(ResidentBlueprint {
        state,
        lifecycle,
        supervisor: Some(supervisor),
        ledger_supervisor: Some(ledger_supervisor),
    });
    Ok(())
}

fn enrolled_roots(
    registry: &crate::authorization::InstallationRegistryV1,
) -> Result<(Vec<PathBuf>, std::collections::BTreeMap<String, String>), String> {
    // A registry binding whose repository has since been deleted, moved, or is
    // otherwise unreachable must NOT take the whole resident down: the resident
    // serves every still-enrolled repository and records the unavailable ones as
    // omissions. Canonicalize-or-skip keeps one stale binding (e.g. a cleaned-up
    // qualification temp repo) from crash-looping the daemon, honouring the
    // "record material omissions/degradation" invariant instead of failing closed
    // on state the operator can no longer influence.
    let mut roots = Vec::new();
    let mut skipped = std::collections::BTreeMap::new();
    for binding in registry.bindings() {
        match PathBuf::from(&binding.root).canonicalize() {
            // A repository inside another one's ignored paths (test fixtures, build evidence) is
            // scratch: mapping it costs CPU on every change for nothing (#449).
            Ok(root) if root.is_dir() && ignored_by_parent_repository(&root) => {
                skipped.insert(
                    root.to_string_lossy().into_owned(),
                    "ignored_by_parent_repository".into(),
                );
            }
            Ok(root) if root.is_dir() => roots.push(root),
            Ok(root) => {
                skipped.insert(
                    root.to_string_lossy().into_owned(),
                    "not_a_directory".into(),
                );
            }
            Err(error) => {
                skipped.insert(binding.root.clone(), format!("unavailable: {error}"));
            }
        }
    }
    roots.sort_by(|left, right| left.to_string_lossy().cmp(&right.to_string_lossy()));
    roots.dedup();
    Ok((roots, skipped))
}

/// Whether `root` sits inside an enclosing git repository that ignores it. Asked once per root
/// (`git check-ignore` in the nearest enclosing repository) and remembered for the process.
#[allow(clippy::disallowed_methods)] // spawned with CREATE_NO_WINDOW below, as the rule asks
fn ignored_by_parent_repository(root: &Path) -> bool {
    static SEEN: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<PathBuf, bool>>> =
        std::sync::OnceLock::new();
    let seen = SEEN.get_or_init(Default::default);
    if let Some(known) = seen.lock().ok().and_then(|map| map.get(root).copied()) {
        return known;
    }
    let ignored = root
        .ancestors()
        .skip(1)
        .find(|dir| dir.join(".git").exists())
        .is_some_and(|parent| {
            let mut git = std::process::Command::new("git");
            #[cfg(windows)]
            {
                // No console window for a background check.
                use std::os::windows::process::CommandExt;
                git.creation_flags(0x0800_0000);
            }
            // git does not take Windows verbatim (`\\?\`) paths, which canonicalize returns.
            let plain = |path: &Path| {
                let text = path.to_string_lossy();
                PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text))
            };
            git.arg("-C")
                .arg(plain(parent))
                .args(["check-ignore", "-q", "--"])
                .arg(plain(root))
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
        });
    if let Ok(mut map) = seen.lock() {
        map.insert(root.to_path_buf(), ignored);
    }
    ignored
}

fn emit_skipped_root_events(skipped: &std::collections::BTreeMap<String, String>) {
    for (root, reason) in skipped {
        emit_skipped_root_event(root, reason);
    }
}

fn emit_skipped_root_event(root: &str, detail: &str) {
    let (reason, error) = detail
        .strip_prefix("unavailable: ")
        .map_or((detail, None), |error| ("unavailable", Some(error)));
    let mut event = serde_json::json!({
        "event": "resident_blueprint_enrollment_skipped",
        "reason": reason,
        "root": root
    });
    if let Some(error) = error {
        event["error"] = serde_json::Value::String(error.to_string());
    }
    crate::lifecycle_log::emit(event);
}

fn record_skipped_root_transitions(
    state: &Arc<Mutex<ResidentBlueprintState>>,
    skipped: &std::collections::BTreeMap<String, String>,
    available_roots: &[PathBuf],
) {
    let Ok(mut state) = state.lock() else { return };
    let (new_or_changed, recovered) = skipped_root_transitions(&state.skipped_roots, skipped);
    let new_or_changed = new_or_changed
        .into_iter()
        .map(|(root, reason)| (root.clone(), reason.clone()))
        .collect::<Vec<_>>();
    let recovered = recovered
        .into_iter()
        .filter(|root| {
            available_roots
                .iter()
                .any(|candidate| candidate.to_string_lossy() == root.as_str())
        })
        .cloned()
        .collect::<Vec<_>>();
    state.skipped_roots = skipped.clone();
    drop(state);
    for (root, reason) in new_or_changed {
        emit_skipped_root_event(&root, &reason);
    }
    for root in recovered {
        crate::lifecycle_log::emit(serde_json::json!({
            "event": "resident_blueprint_enrollment_recovered",
            "root": root
        }));
    }
}

fn skipped_root_transitions<'a>(
    previous: &'a std::collections::BTreeMap<String, String>,
    current: &'a std::collections::BTreeMap<String, String>,
) -> (Vec<(&'a String, &'a String)>, Vec<&'a String>) {
    let changed = current
        .iter()
        .filter(|(root, reason)| previous.get(*root) != Some(*reason))
        .collect();
    let recovered = previous
        .keys()
        .filter(|root| !current.contains_key(*root))
        .collect();
    (changed, recovered)
}

fn build_resident_repo(
    root: &Path,
    cancellation: &CancellationToken,
) -> Result<ResidentRepo, String> {
    let started = std::time::Instant::now();
    crate::lifecycle_log::emit(
        serde_json::json!({"event":"resident_blueprint_repository_build", "stage":"started", "root":root}),
    );
    let service = Arc::new(NativeService::resident(
        rightkit_repo_graph::NativeBlueprintOperation,
        root.to_path_buf(),
    ));
    service
        .start()
        .map_err(|error| format!("resident Blueprint startup service start: {error}"))?;
    let mut build = BlueprintRequest::new(
        format!("resident-build-{}", std::process::id()),
        Operation::Build,
        root.to_string_lossy(),
    );
    // Cold first-build of a large enrolled repo can exceed two minutes under
    // in-resident contention (a second repo's watcher plus daemon overhead push
    // a repo that builds in ~80s standalone past a 120s ceiling). Give the
    // initial build room to finish and persist its generation; once cached,
    // subsequent starts refresh incrementally and are fast. Without this the
    // build is cancelled at the deadline, never caches, and retries cold
    // forever — leaving the Hub permanently short of full watcher coverage.
    build.deadline_ms = rightkit_repo_graph::model::MAX_BUILD_DEADLINE_MS;
    let response = service.dispatch(build, cancellation.clone());
    crate::lifecycle_log::emit(
        serde_json::json!({"event":"resident_blueprint_repository_build", "stage":if response.ok { "completed" } else { "failed" }, "root":root, "elapsedMs":started.elapsed().as_millis()}),
    );
    let generation_id = response
        .result
        .as_ref()
        .and_then(|result| result.get("generationId"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let complete = response
        .result
        .as_ref()
        .and_then(|result| result.get("complete"))
        .and_then(serde_json::Value::as_bool)
        == Some(true);
    if !response.ok || generation_id.is_none() {
        let detail = response
            .error
            .map(|error| {
                format!(
                    "resident Blueprint startup initial build {}: {}",
                    error.code, error.message
                )
            })
            .unwrap_or_else(|| "resident Blueprint startup initial build failed".into());
        let _ = service.drain();
        return Err(detail);
    }
    let Some(generation_id) = generation_id else {
        let _ = service.drain();
        return Err("resident Blueprint startup initial build returned no generation".into());
    };
    Ok(ResidentRepo {
        root: root.to_string_lossy().into_owned(),
        service,
        generation_id,
        generation_complete: complete,
    })
}

const RESIDENT_SUPERVISE_MIN: Duration = Duration::from_millis(250);
const RESIDENT_SUPERVISE_MAX: Duration = Duration::from_secs(1);
/// Safety net: a full registry reconcile runs at least this often.
const RESIDENT_RECONCILE_MAX: Duration = Duration::from_secs(60);

/// First wait after a watcher panic; each consecutive panic doubles it up to the cap.
const RESIDENT_PANIC_BACKOFF_MIN: Duration = Duration::from_secs(1);
const RESIDENT_PANIC_BACKOFF_MAX: Duration = Duration::from_secs(30);
/// A pass that ran this long before panicking starts a fresh backoff streak.
const RESIDENT_PANIC_STREAK_RESET: Duration = Duration::from_secs(60);

fn resident_panic_backoff(panics_in_a_row: u32) -> Duration {
    RESIDENT_PANIC_BACKOFF_MIN
        .saturating_mul(1u32 << panics_in_a_row.saturating_sub(1).min(5))
        .min(RESIDENT_PANIC_BACKOFF_MAX)
}

fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|message| (*message).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".to_owned())
}

/// Records a caught watcher panic in the resident status and the lifecycle log. A panic while
/// holding the state or stage lock poisons it, and every later `lock()` would then fail and end
/// the watcher for good, so the poison is cleared here: the state is plain data and the next
/// reconcile pass rebuilds whatever the interrupted pass left half done.
fn record_resident_watcher_panic(
    state: &Arc<Mutex<ResidentBlueprintState>>,
    stage: &Arc<Mutex<String>>,
    payload: &(dyn std::any::Any + Send),
    backoff: Duration,
) {
    let message = panic_payload_message(payload);
    let died_in = stage
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    stage.clear_poison();
    let restarts = {
        let mut guard = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.watcher_panics = guard.watcher_panics.saturating_add(1);
        guard.last_watcher_panic = Some(format!(
            "resident Blueprint watcher panicked in stage {died_in}: {message} (restart {})",
            guard.watcher_panics
        ));
        guard.watcher_panics
    };
    state.clear_poison();
    set_supervisor_stage(stage, "panic_backoff");
    crate::lifecycle_log::emit(serde_json::json!({
        "event": "resident_blueprint_watcher_panic",
        "stage": "failed",
        "panicStage": died_in,
        "error": message,
        "restartCount": restarts,
        "backoffMs": backoff.as_millis() as u64,
    }));
}

/// (mtime, len) of the installation registry file the reconcile reads.
/// `None` when unresolvable or absent, which always forces a reload.
fn registry_file_stat() -> Option<(std::time::SystemTime, u64)> {
    let path = crate::authorization::default_registry_path()?;
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

/// True when a reconcile has nothing pending: initialized, no registry error,
/// no failed or oversized builds awaiting retry.
fn resident_is_settled(state: &Arc<Mutex<ResidentBlueprintState>>) -> bool {
    state.lock().is_ok_and(|state| {
        state.initialization_completed
            && state.registry_error.is_none()
            && state.build_failures.is_empty()
            && state.oversized.is_empty()
    })
}

/// Blocks up to `total`; a drain, an authority change or a file event wakes it at once.
fn sleep_until_or_shutdown(wake: &crate::wake::Wake, total: Duration) {
    wake.wait(total);
}

/// Cheap digest of everything a supervisor pass can change; equal digests
/// across passes mean the resident is idle.
fn resident_state_fingerprint(state: &Arc<Mutex<ResidentBlueprintState>>) -> String {
    let Ok(state) = state.lock() else {
        return String::new();
    };
    let mut out = format!(
        "{}|{:?}|{}|{}|{}",
        state.enrolled_repo_count,
        state.registry_error,
        state.initialization_completed,
        state.build_failures.len(),
        state.oversized.len()
    );
    for repo in &state.repos {
        out.push_str(&format!(
            "|{}:{}:{}",
            repo.root, repo.generation_id, repo.generation_complete
        ));
    }
    out
}

fn supervise_resident_repositories(state: &Arc<Mutex<ResidentBlueprintState>>) -> bool {
    let services = {
        let Ok(mut state) = state.lock() else {
            return false;
        };
        state
            .repos
            .retain(|repo| repo.service.status() == ServiceStatus::Running);
        state
            .repos
            .iter()
            .map(|repo| (repo.root.clone(), Arc::clone(&repo.service)))
            .collect::<Vec<_>>()
    };
    let cancellation = state
        .lock()
        .map(|state| state.cancellation.clone())
        .unwrap_or_default();
    // One degraded watcher retires only that repository — never the whole
    // resident. A deleted or unwatched enrolled root (e.g. a cleaned-up
    // qualification temp repo) otherwise took every healthy repo's watcher
    // down with it and left the Hub reporting watcher_unavailable until the
    // next engine start. Reconcile owns enrollment truth and drops or
    // rebuilds the retired root on the next pass.
    let mut failed = Vec::new();
    for (root, service) in &services {
        if service.supervise_with_cancellation(cancellation.clone()) != ServiceStatus::Running {
            let readiness = service.readiness();
            let detail = format!(
                "resident Blueprint watcher {root}: {:?}: {}",
                readiness.status,
                readiness
                    .detail
                    .as_deref()
                    .unwrap_or("watcher unavailable without detail")
            );
            crate::lifecycle_log::emit(
                serde_json::json!({"event":"resident_blueprint_supervision", "stage":"failed", "root":root, "error":detail}),
            );
            failed.push((root.clone(), detail));
        }
    }
    if !failed.is_empty() {
        let retired = {
            let Ok(mut state) = state.lock() else {
                return false;
            };
            state.registry_error = failed.first().map(|(_, detail)| detail.clone());
            // A retired-but-still-enrolled root is rebuilt by reconcile; feed
            // the same backoff as a failed build so a flapping watcher cannot
            // churn unbounded rebuild work.
            for (root, _) in &failed {
                let entry = state
                    .build_failures
                    .entry(root.clone())
                    .or_insert((0, Instant::now()));
                entry.0 = entry.0.saturating_add(1);
                entry.1 = Instant::now();
            }
            save_blueprint_retry(&state.build_failures, &state.oversized);
            let failed_roots = failed
                .iter()
                .map(|(root, _)| root.clone())
                .collect::<std::collections::BTreeSet<_>>();
            let mut retired = Vec::new();
            state.repos.retain(|repo| {
                if failed_roots.contains(&repo.root) {
                    retired.push(Arc::clone(&repo.service));
                    false
                } else {
                    true
                }
            });
            retired
        };
        for service in retired {
            let _ = service.drain();
        }
        return true;
    }
    if let Ok(mut state) = state.lock() {
        for repo in &mut state.repos {
            if let Some((generation_id, complete)) = repo.service.generation_metadata() {
                repo.generation_id = generation_id;
                repo.generation_complete = complete;
            }
        }
    }
    true
}

fn reconcile_resident_repositories(
    state: &Arc<Mutex<ResidentBlueprintState>>,
    supervisor_stage: &Arc<Mutex<String>>,
    background_cancellation: CancellationToken,
) {
    let registry = match crate::authorization::load_installation_registry() {
        Ok(registry) => registry,
        Err(error) => {
            mark_registry_error(state, error.to_string());
            return;
        }
    };
    let (roots, skipped_roots) = match enrolled_roots(&registry) {
        Ok(roots) => roots,
        Err(error) => {
            mark_registry_error(state, error);
            return;
        }
    };
    record_skipped_root_transitions(state, &skipped_roots, &roots);
    let desired = roots
        .iter()
        .map(|root| root.to_string_lossy().into_owned())
        .collect::<std::collections::BTreeSet<_>>();
    let (existing, cancellation, failures, oversized) = {
        let Ok(mut state) = state.lock() else { return };
        state.enrolled_repo_count = desired.len() as u64;
        let mut removed = Vec::new();
        state.repos.retain(|repo| {
            let keep = desired.contains(&repo.root);
            if !keep {
                removed.push(Arc::clone(&repo.service));
            }
            keep
        });
        drop(removed);
        // Retry memory tracks only roots that are still enrolled; a removed root's record is dropped.
        let tracked_before = state.build_failures.len() + state.oversized.len();
        state
            .build_failures
            .retain(|root, _| desired.contains(root));
        state.oversized.retain(|root, _| desired.contains(root));
        if state.build_failures.len() + state.oversized.len() != tracked_before {
            save_blueprint_retry(&state.build_failures, &state.oversized);
        }
        (
            state
                .repos
                .iter()
                .map(|repo| repo.root.clone())
                .collect::<std::collections::BTreeSet<_>>(),
            background_cancellation.clone(),
            state.build_failures.clone(),
            state.oversized.clone(),
        )
    };
    // Roots that have never failed build first; a repository inside its
    // backoff window is skipped rather than re-burning the sequential budget.
    let mut ordered_roots = roots;
    ordered_roots.sort_by_key(|root| failures.contains_key(&root.to_string_lossy().into_owned()));
    let mut build_errors = Vec::new();
    for root in ordered_roots {
        if cancellation.is_cancelled() {
            break;
        }
        let key = root.to_string_lossy().into_owned();
        if existing.contains(&key) {
            continue;
        }
        // Over-limit roots stay skipped until the limit changes; a changed
        // limit is the only retry trigger (size is only known by building).
        if oversized.get(&key).is_some_and(|(limit, _)| {
            *limit == rightkit_repo_graph::watch::DEFAULT_MAX_SNAPSHOT_BYTES
        }) {
            continue;
        }
        if !retry_due(&failures, &key, Instant::now()) {
            continue;
        }
        set_supervisor_stage(supervisor_stage, &format!("build:{key}"));
        match build_resident_repo(&root, &cancellation) {
            Ok(repo) => {
                // Publish each completed root immediately. A large enrolled
                // repository must not hide smaller completed roots from
                // health or watcher identity until the whole batch finishes.
                if cancellation.is_cancelled() {
                    let _ = repo.service.drain();
                } else if let Ok(mut state) = state.lock() {
                    let cleared_failure = state.build_failures.remove(&key).is_some();
                    let cleared_oversized = state.oversized.remove(&key).is_some();
                    if cleared_failure || cleared_oversized {
                        save_blueprint_retry(&state.build_failures, &state.oversized);
                    }
                    if desired.contains(&repo.root)
                        && !state.repos.iter().any(|current| current.root == repo.root)
                    {
                        state.repos.push(repo);
                    }
                }
            }
            Err(error) => {
                if cancellation.is_cancelled() {
                    break;
                }
                if error.contains("snapshot limit exceeded") {
                    // Record once with a clear reason; health reports it as a
                    // skipped repository instead of a rebuild failure.
                    let reason = format!(
                        "over_snapshot_limit: {error} (limit {} bytes); skipped until the limit changes",
                        rightkit_repo_graph::watch::DEFAULT_MAX_SNAPSHOT_BYTES
                    );
                    if let Ok(mut state) = state.lock() {
                        state.build_failures.remove(&key);
                        state.oversized.insert(
                            key.clone(),
                            (
                                rightkit_repo_graph::watch::DEFAULT_MAX_SNAPSHOT_BYTES,
                                reason.clone(),
                            ),
                        );
                        save_blueprint_retry(&state.build_failures, &state.oversized);
                    }
                    crate::lifecycle_log::emit(serde_json::json!({
                        "event": "resident_blueprint_enrollment_skipped",
                        "reason": "over_snapshot_limit",
                        "root": key,
                        "error": reason
                    }));
                    continue;
                }
                if let Ok(mut state) = state.lock() {
                    let entry = state
                        .build_failures
                        .entry(key.clone())
                        .or_insert((0, Instant::now()));
                    entry.0 = entry.0.saturating_add(1);
                    entry.1 = Instant::now();
                    save_blueprint_retry(&state.build_failures, &state.oversized);
                }
                build_errors.push(error);
            }
        }
    }
    if let Ok(mut state) = state.lock() {
        if let Some(error) = build_errors.into_iter().next() {
            crate::lifecycle_log::emit(
                serde_json::json!({"event":"resident_blueprint_initialization", "stage":"failed", "error": error}),
            );
            state.registry_error = Some(error);
            state.initialization_completed = false;
        } else if state.repos.len()
            + state
                .oversized
                .keys()
                .filter(|root| desired.contains(*root))
                .count()
            == desired.len()
        {
            state.registry_error = None;
            if !state.initialization_completed {
                crate::lifecycle_log::emit(
                    serde_json::json!({"event":"resident_blueprint_initialization", "stage":"completed", "repoCount": state.repos.len()}),
                );
                state.initialization_completed = true;
            }
        }
        // A skipped retry is not recovery. Keep its concrete failure visible
        // through health until every desired repository has joined.
    }
}

/// Resident Ledger maintenance cadence: the persisted document projection is
/// reconciled in the background under the same `background_authority_open`
/// gate as watcher work — never inside a retrieval request. Five minutes is
/// the maintenance interval; a faster discovery path stays available through
/// the explicit `ledger sync` operation.
const LEDGER_MAINTENANCE_INTERVAL: Duration = Duration::from_secs(300);

/// Authorized Ledger maintenance for every enrolled root: reconciles the
/// persisted document projection through `LedgerService::maintain` — the
/// same `sync_locked` index/update mechanism the explicit "sync" operation
/// uses. Runs only when the supervisor holds background authority, bounded
/// per root, and reports typed failure rather than stopping the loop.
fn reconcile_resident_ledger(
    state: &Arc<Mutex<ResidentBlueprintState>>,
    lifecycle: &LifecycleControl,
) {
    let Ok(registry) = crate::authorization::load_installation_registry() else {
        return;
    };
    let Ok(owner) = crate::ledger::service::active_owner() else {
        return;
    };
    for binding in registry.bindings() {
        let cancelled = state
            .lock()
            .map(|state| state.cancellation.is_cancelled())
            .unwrap_or(true);
        if cancelled {
            break;
        }
        let root = Path::new(&binding.root);
        let canonical_id = rightkit_context_federation::root::canonical_repository_id(root);
        let caller = match crate::ledger::service::Caller::enrolled(root, &canonical_id) {
            Ok(caller) => caller,
            Err(error) => {
                crate::lifecycle_log::emit(
                    serde_json::json!({"event":"resident_ledger_maintenance", "stage":"skipped", "root":binding.root, "error":error}),
                );
                continue;
            }
        };
        let started = Instant::now();
        // Cold publication walks and indexes every enrolled Markdown source in
        // one transaction; large roots need minutes, not seconds. Bounded at
        // ten minutes — background maintenance, never retrieval. The lifecycle
        // cancellation token lets engine drain abort a mid-sync pass.
        let budget = crate::ledger::limits::WorkBudget::new(
            Instant::now() + Duration::from_secs(600),
            lifecycle.background_cancellation_token(),
        );
        match owner.maintain(&caller, &budget) {
            Ok(report) => crate::lifecycle_log::emit(
                serde_json::json!({"event":"resident_ledger_maintenance", "stage":"completed", "root":binding.root, "elapsedMs":started.elapsed().as_millis(), "generation":report.index_generation, "registered":report.registered, "manifestOversized":report.manifests_oversized}),
            ),
            Err(error) => crate::lifecycle_log::emit(
                serde_json::json!({"event":"resident_ledger_maintenance", "stage":"failed", "root":binding.root, "elapsedMs":started.elapsed().as_millis(), "error":error}),
            ),
        }
    }
}

/// Transcript lane maintenance (upstream #431). Independent of repository enrollment: the user-enabled
/// source manifest is its authorization, and with no manifest (the default) the pass stores
/// nothing and purges anything the lane held. Same cadence and background-authority gate as the
/// document pass; the lifecycle token aborts it on drain.
fn reconcile_resident_transcripts(lifecycle: &LifecycleControl) {
    let Ok(owner) = crate::ledger::service::active_owner() else {
        return;
    };
    let started = Instant::now();
    let budget = crate::ledger::limits::WorkBudget::new(
        Instant::now() + Duration::from_secs(600),
        lifecycle.background_cancellation_token(),
    );
    match owner.maintain_transcripts(&budget) {
        Ok(report) if !report.enabled && report.purged_rows == 0 => {}
        Ok(report) => crate::lifecycle_log::emit(
            serde_json::json!({"event":"resident_ledger_transcripts", "stage":"completed", "elapsedMs":started.elapsed().as_millis(), "report":report}),
        ),
        Err(error) => crate::lifecycle_log::emit(
            serde_json::json!({"event":"resident_ledger_transcripts", "stage":"failed", "elapsedMs":started.elapsed().as_millis(), "error":error}),
        ),
    }
}

fn mark_registry_error(state: &Arc<Mutex<ResidentBlueprintState>>, error: String) {
    if let Ok(mut state) = state.lock() {
        state.repos.clear();
        state.enrolled_repo_count = 0;
        state.registry_error = Some(error);
    }
}

fn drain_resident_repositories(state: &Arc<Mutex<ResidentBlueprintState>>) {
    if let Ok(mut state) = state.lock() {
        for repo in state.repos.drain(..) {
            let _ = repo.service.drain();
        }
    }
}

fn stop_resident_blueprint() -> Result<(), String> {
    let Some(slot) = RESIDENT_BLUEPRINT.get() else {
        return Ok(());
    };
    let mut current = slot
        .lock()
        .map_err(|_| "resident Blueprint state unavailable".to_string())?;
    if let Some(resident) = current.as_mut() {
        // Keep singleton ownership in the slot until supervisor termination is
        // observed. A timeout is surfaced to the caller; no writable worker is
        // silently detached or allowed to outlive its owner.
        resident.stop_bounded(RESIDENT_SUPERVISOR_STOP_TIMEOUT)?;
    }
    current.take();
    Ok(())
}

impl Drop for ResidentBlueprint {
    fn drop(&mut self) {
        // Normal teardown goes through stop_bounded while this value remains
        // in RESIDENT_BLUEPRINT. Keep Drop defensive and bounded for failed
        // startup/replacement paths; it must never block process teardown
        // forever.
        let _ = self.stop_bounded(RESIDENT_SUPERVISOR_STOP_TIMEOUT);
    }
}

impl ResidentBlueprint {
    fn stop_bounded(&mut self, timeout: Duration) -> Result<(), String> {
        if let Ok(state) = self.state.lock() {
            state.cancellation.cancel();
        }
        self.lifecycle.request_drain(Some("resident_runtime_exit"));
        if let Some(supervisor) = self.supervisor.as_ref() {
            if !wait_for_thread_exit(supervisor, timeout) {
                let stage = match self.state.lock() {
                    Ok(state) => supervisor_stage(&state.supervisor_stage),
                    Err(_) => "state_unavailable".to_owned(),
                };
                crate::lifecycle_log::emit(serde_json::json!({
                    "event": "resident_blueprint_supervisor_stop_timeout",
                    "stage": stage,
                    "timeoutMs": timeout.as_millis() as u64,
                }));
                return Err(format!(
                    "resident Blueprint supervisor drain timeout; singleton retained (supervisor stage at timeout: {stage})"
                ));
            }
        }
        if let Some(supervisor) = self.supervisor.take() {
            supervisor
                .join()
                .map_err(|_| "resident Blueprint supervisor panicked while draining".to_string())?;
        }
        if let Some(ledger) = self.ledger_supervisor.as_ref() {
            if !wait_for_thread_exit(ledger, timeout) {
                crate::lifecycle_log::emit(serde_json::json!({
                    "event": "resident_ledger_maintenance_stop_timeout",
                    "timeoutMs": timeout.as_millis() as u64,
                }));
                return Err(
                    "resident Ledger maintenance drain timeout; singleton retained".to_owned(),
                );
            }
        }
        if let Some(ledger) = self.ledger_supervisor.take() {
            ledger
                .join()
                .map_err(|_| "resident Ledger maintenance panicked while draining".to_string())?;
        }
        drain_resident_repositories(&self.state);
        Ok(())
    }
}

fn wait_for_thread_exit(thread: &std::thread::JoinHandle<()>, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while !thread.is_finished() {
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    true
}

/// The resident Blueprint service enrolled for `repo_root`, if this engine
/// holds one. Freshness reads use this to consult the resident watcher's
/// own observation state (`barrier_all`) plus the sealed generation basis
/// instead of spawning a per-request live worktree fingerprint. `None`
/// means no resident coverage — caller must fall back or degrade honestly.
pub(crate) fn resident_blueprint_service(repo_root: &Path) -> Option<Arc<NativeService>> {
    let slot = RESIDENT_BLUEPRINT.get()?;
    let current = slot.lock().ok()?;
    let resident = current.as_ref()?;
    let state = resident.state.lock().ok()?;
    let wanted = std::fs::canonicalize(repo_root)
        .map(|path| path.to_string_lossy().to_lowercase())
        .unwrap_or_else(|_| repo_root.to_string_lossy().to_lowercase());
    state
        .repos
        .iter()
        .find(|repo| {
            if repo
                .root
                .eq_ignore_ascii_case(repo_root.to_string_lossy().as_ref())
            {
                return true;
            }
            std::fs::canonicalize(&repo.root)
                .map(|path| path.to_string_lossy().to_lowercase() == wanted)
                .unwrap_or(false)
        })
        .map(|repo| Arc::clone(&repo.service))
}

/// Actual resident watcher state for health and Hub composition.
pub fn resident_blueprint_status() -> serde_json::Value {
    let Some(slot) = RESIDENT_BLUEPRINT.get() else {
        return serde_json::json!({
            "watcherRunning": false,
            "enrolledRepoCount": 0,
            "watcherIdentity": null,
        });
    };
    let Ok(current) = slot.lock() else {
        return serde_json::json!({"watcherRunning": false, "enrolledRepoCount": 0});
    };
    let Some(resident) = current.as_ref() else {
        return serde_json::json!({
            "watcherRunning": false,
            "enrolledRepoCount": 0,
            "watcherIdentity": null,
        });
    };
    let Ok(state) = resident.state.lock() else {
        return serde_json::json!({"watcherRunning": false, "enrolledRepoCount": 0});
    };
    resident_status_json(&state)
}

fn resident_status_json(state: &ResidentBlueprintState) -> serde_json::Value {
    // Over-limit repositories are skipped by design and reported below; they
    // do not make the rest of the watcher unavailable.
    let skipped = state.oversized.len();
    let required = (state.enrolled_repo_count as usize).saturating_sub(skipped);
    let watcher_running = required > 0
        && state.repos.len() == required
        && state
            .repos
            .iter()
            .all(|repo| repo.service.status() == ServiceStatus::Running);
    let watcher_ready = watcher_running && state.repos.iter().all(|repo| repo.service.is_ready());
    let coverage = if state.enrolled_repo_count > 0
        && skipped == 0
        && state.repos.len() == state.enrolled_repo_count as usize
        && state.repos.iter().all(|repo| repo.generation_complete)
    {
        "complete"
    } else {
        "partial"
    };
    let identities = state
        .repos
        .iter()
        .map(|repo| {
            let events = repo.service.events();
            // Per-root health (#555): the aggregate `watcherReady` is a conjunction over every
            // root, so one degraded root hid the healthy ones; each root now states its own.
            let readiness = repo.service.readiness();
            serde_json::json!({
                "root": repo.root,
                "state": readiness.status.as_str(),
                "ready": readiness.ready,
                "detail": readiness.detail,
                "generationId": repo.generation_id,
                "complete": repo.generation_complete,
                "eventSequence": events.last().map(|event| event.sequence).unwrap_or(0),
                "lastEvent": events.last().map(|event| event.kind.clone()),
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "watcherRunning": watcher_running,
        "watcherReady": watcher_ready,
        "watcherState": if watcher_ready { "running" } else if state.enrolled_repo_count == 0 { "not_configured" } else { "watcher_unavailable" },
        "watcherCoverage": coverage,
        "watcherRestarts": state.watcher_panics,
        "watcherLastPanic": state.last_watcher_panic,
        "watcherDetail": state.registry_error.clone().or_else(|| state.last_watcher_panic.clone().filter(|_| !watcher_ready)).or_else(|| (skipped > 0).then_some("repository_over_snapshot_limit".into())).or_else(|| (coverage == "partial").then_some("generation_partial_coverage".into())),
        "skippedRepositories": state.oversized.iter().map(|(root, (limit, reason))| serde_json::json!({"root": root, "limit": limit, "reason": reason})).collect::<Vec<_>>(),
        "enrolledRepoCount": state.enrolled_repo_count,
        "watcherIdentity": identities,
        "buildFailures": state.build_failures.iter().map(|(root, (count, last))| serde_json::json!({"root": root, "consecutiveFailures": count, "lastAttemptAgeSecs": last.elapsed().as_secs()})).collect::<Vec<_>>(),
    })
}

pub fn install_lifecycle_control(control: LifecycleControl) -> Result<(), String> {
    let slot = LIFECYCLE_CONTROL.get_or_init(|| RwLock::new(LifecycleControl::default()));
    *slot
        .write()
        .map_err(|_| "lifecycle control unavailable".to_string())? = control;
    Ok(())
}

pub fn lifecycle_control() -> LifecycleControl {
    LIFECYCLE_CONTROL
        .get_or_init(|| RwLock::new(LifecycleControl::default()))
        .read()
        .map(|control| control.clone())
        .unwrap_or_default()
}

struct ResidentEngineClaim;

impl ResidentEngineClaim {
    fn acquire() -> Result<Self, String> {
        RESIDENT_ENGINE_ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| Self)
            .map_err(|_| "membrane_resident_already_active".to_string())
    }
}

impl Drop for ResidentEngineClaim {
    fn drop(&mut self) {
        RESIDENT_ENGINE_ACTIVE.store(false, Ordering::Release);
    }
}

/// OS-backed owner for the installed resident engine, held through `rightkit-fs`.  The lock is
/// acquired before any store, embedder, watcher, or listener initialization.  A contender fails
/// closed instead of selecting another port/store.  The OS drops the lock if the process dies;
/// the owner record (pid, host, label) lives in the sibling `membrane-engine.lock.owner`.
pub(crate) struct EngineOwner {
    _lock: rightkit_fs::lock::FileLock,
}

impl EngineOwner {
    pub(crate) fn acquire(runtime: &Runtime) -> Result<Self, String> {
        Self::acquire_waiting(runtime, Duration::ZERO)
    }

    /// Like `acquire`, but a busy lock is retried for up to `wait` before failing closed.
    pub(crate) fn acquire_waiting(runtime: &Runtime, wait: Duration) -> Result<Self, String> {
        std::fs::create_dir_all(&runtime.workspace_root)
            .map_err(|error| format!("create resident state root: {error}"))?;
        let state_root = std::fs::canonicalize(&runtime.workspace_root)
            .map_err(|error| format!("canonicalize resident state root: {error}"))?;
        let path = state_root.join("tools/.cache/memory/membrane-engine.lock");
        let parent = path
            .parent()
            .ok_or_else(|| "resident owner lock has no parent".to_string())?;
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("create resident owner lock directory: {error}"))?;
        let options = rightkit_fs::lock::LockOptions::wait(wait).labeled("membrane-engine");
        let lock =
            rightkit_fs::lock::FileLock::acquire(&path, &options).map_err(|error| match error {
                rightkit_fs::FsError::Locked { .. } | rightkit_fs::FsError::LockTimeout { .. } => {
                    emit_lifecycle("owner_contended", Some("owner_lock_held"));
                    format!("membrane_engine_already_owned: {error}")
                }
                other => format!("open resident owner lock {}: {other}", path.display()),
            })?;
        emit_lifecycle("owner_acquired", Some("resident_engine"));
        Ok(Self { _lock: lock })
    }
}

impl Drop for EngineOwner {
    fn drop(&mut self) {
        emit_lifecycle("owner_released", Some("resident_engine"));
    }
}

fn build_info() -> serde_json::Value {
    serde_json::json!({
        "product_version": env!("CARGO_PKG_VERSION"),
        "membrane_source_commit": crate::release_identity::source_commit().unwrap_or("unknown"),
        "source_tree_sha256": crate::release_identity::source_tree_sha256().unwrap_or("unknown"),
        "release_generation": crate::release_identity::release_generation(),

        "release_identity_source": crate::release_identity::source_kind(),
        "store_replacement": rightkit_memory_store::store_replacement_status(&crate::cli::cortex_db_path()),
        "target": crate::release_identity::target_triple(),
    })
}

pub(crate) fn prepare_runtime_identity(
    runtime: &Runtime,
) -> Result<
    (
        crate::installation_identity::InstallationIdentity,
        crate::installation_identity::StartupClaim,
    ),
    String,
> {
    crate::installation_identity::prepare_service_start(&runtime.workspace_root)
        .map_err(|error| format!("prepare installation identity: {error}"))
}

static RESIDENT_STORE: OnceLock<crate::MemoryStore> = OnceLock::new();

pub(crate) fn install_resident_store(store: crate::MemoryStore) -> Result<(), String> {
    RESIDENT_STORE
        .set(store)
        .map_err(|_| "resident Cortex store already owned".to_owned())
}

pub(crate) fn resident_store() -> Option<crate::MemoryStore> {
    RESIDENT_STORE.get().cloned()
}

pub(crate) fn resident_store_for_db(path: &Path) -> Option<crate::MemoryStore> {
    let store = resident_store()?;
    let exe = std::env::current_exe().ok()?;
    let runtime = runtime_from_installed_exe(&exe).ok()?;
    let requested = std::fs::canonicalize(path).ok()?;
    let installed = std::fs::canonicalize(runtime.db).ok()?;
    if requested == installed {
        Some(store)
    } else {
        None
    }
}

pub(crate) fn open_installed_store() -> Result<crate::MemoryStore, String> {
    if let Some(store) = resident_store() {
        return Ok(store);
    }
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    let runtime = runtime_from_installed_exe(&exe)?;
    if let Some(parent) = runtime.db.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    crate::MemoryStore::try_open(
        crate::MemDb::open(&runtime.db).map_err(|error| error.to_string())?,
    )
}

pub(crate) fn emit_lifecycle(event: &str, reason: Option<&str>) {
    let mut value = serde_json::json!({
        "event": event,
        "pid": std::process::id(),
        "observedAtUnixMs": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or_default(),
    });
    if let Some(reason) = reason {
        value["reason"] = serde_json::Value::String(reason.to_owned());
    }
    crate::lifecycle_log::emit(value);
}

/// Foreground hook owner. Opens persisted Cortex rows without loading the
/// optional embedding runtime; callers use its lexical read-only arm.
pub(crate) fn open_installed_lexical_store() -> Result<crate::MemoryStore, String> {
    if let Some(store) = resident_store() {
        return Ok(store);
    }
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    let runtime = runtime_from_installed_exe(&exe)?;
    // Hook startup is a read-only bounded path.  In particular, do not create
    // an absent store here: missing sealed state must become a typed omission.
    crate::MemoryStore::try_open_lexical(
        crate::MemDb::open_read_only(&runtime.db).map_err(|error| error.to_string())?,
    )
}

/// The resident's paths: its state root, the Cortex database inside it, and where the binary
/// was found (`Helpers/` in an installed the host app).
pub(crate) struct Runtime {
    pub(crate) workspace_root: PathBuf,
    pub(crate) db: PathBuf,
    pub(crate) stable_current: Option<PathBuf>,
    pub(crate) version_root: Option<PathBuf>,
}

/// The host app's layout (C1): the binary sits in `Helpers/` (`Contents/Helpers` on the Mac) and state
/// lives in the shared state root (`paths::state_root`). A binary anywhere else is a development
/// build and resolves only when `RIGHTKIT_CONTEXT_STATE_ROOT` names the state it may touch.
pub(crate) fn runtime_from_installed_exe(exe: &Path) -> Result<Runtime, String> {
    let helpers = exe
        .parent()
        .ok_or_else(|| "executable has no parent".to_string())?;
    let in_helpers = helpers.file_name().is_some_and(|name| name == "Helpers");
    let pinned = crate::qa_env::state_root().is_some_and(|value| !value.is_empty());
    if !in_helpers && !pinned {
        return Err("executable is not under the host app Helpers".into());
    }
    let state = crate::paths::state_root();
    Ok(Runtime {
        db: crate::cli::cortex_db_path(),
        workspace_root: state,
        stable_current: Some(helpers.to_path_buf()),
        version_root: Some(helpers.to_path_buf()),
    })
}

/// Projection used only by explicit native enrollment (`membrane init`, non-dry-run). It derives
/// paths from the installed executable's the host app layout; it never falls back to workspace or
/// process CWD state. The installation identity is loaded through its canonical owner: an absent
/// identity is created once (exclusive, never replacing), an existing one is returned byte for
/// byte, and a malformed one fails. It claims no startup, mints no service instance id and
/// advances no generation.
pub fn installed_binding_projection() -> Result<serde_json::Value, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let runtime = runtime_from_installed_exe(&exe)?;
    let paths =
        crate::installation_identity::InstallationPaths::for_workspace(&runtime.workspace_root);
    let identity = crate::installation_identity::load_or_create_installation(&paths.identity)
        .map_err(|e| format!("load installed identity {}: {e}", paths.identity.display()))?;
    Ok(serde_json::json!({
        "workspaceRoot": runtime.workspace_root,
        "stableCurrent": runtime.stable_current,
        "db": runtime.db,
        "installationId": identity.installation_id,
        "createdAt": identity.created_at,
        "startupGeneration": identity.startup_generation,
        "serviceInstanceId": identity.current_service_instance_id,
    }))
}

// Installed ownership lasts until OS process exit, including any blocking
// tasks left behind by Tokio shutdown_timeout. Never unlock beneath a writer.
static INSTALLED_ENGINE_OWNER: OnceLock<EngineOwner> = OnceLock::new();

/// What `membrane serve` holds for the life of the resident: the identity claim, the
/// environment the engine reads, the Blueprint supervisor, and the lifecycle control that
/// drains them. The owner lock lives in a process-wide static and is released at process exit.
pub struct ResidentEngine {
    pub db: PathBuf,
    pub state_root: PathBuf,
    pub started: Instant,
    pub identity: crate::installation_identity::InstallationIdentity,
    pub claim: crate::installation_identity::StartupClaim,
    lifecycle: LifecycleControl,
    _claim: ResidentEngineClaim,
}

impl ResidentEngine {
    pub fn lifecycle(&self) -> LifecycleControl {
        self.lifecycle.clone()
    }

    /// Drain background work and stop the Blueprint supervisor. The owner lock is released at
    /// process exit.
    pub fn stop(&self, reason: &str) -> Result<(), String> {
        self.lifecycle.request_drain(Some(reason));
        stop_resident_blueprint()?;
        emit_lifecycle("engine_stopped", Some("resident_engine"));
        Ok(())
    }
}

/// Start the sole resident engine; `serve` calls this once. The owner lock is the first
/// stateful operation: store opening, identity minting, watchers and the endpoint all come after
/// exclusive ownership. A contender fails with `membrane_engine_already_owned: ...` and must
/// exit 0 ("already serving"). Background work is authorized here for the whole life of the
/// resident; there are no holders to grant or drain it.
pub fn start_resident_engine(lifecycle: LifecycleControl) -> Result<ResidentEngine, String> {
    let startup_started = Instant::now();
    emit_lifecycle("activation_requested", Some("resident_engine"));
    let claim = ResidentEngineClaim::acquire()?;
    install_lifecycle_control(lifecycle.clone())?;
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    let runtime = runtime_from_installed_exe(&exe)?;
    let emit_startup_stage = |stage: &str, stage_started: Instant| {
        crate::lifecycle_log::emit(serde_json::json!({
            "event": "membrane_startup_stage",
            "stage": stage,
            "durationMs": stage_started.elapsed().as_millis() as u64,
            "elapsedMs": startup_started.elapsed().as_millis() as u64,
        }));
    };
    let stage_started = Instant::now();
    let owner = EngineOwner::acquire(&runtime)?;
    emit_startup_stage("ownership", stage_started);
    INSTALLED_ENGINE_OWNER
        .set(owner)
        .map_err(|_| "installed engine already initialized".to_owned())?;
    let stage_started = Instant::now();
    // Debug builds publish the resolved bindings through the environment for the debug-only
    // overrides that read them back; a release build reads no such variable, so it sets none.
    let catalog_path = crate::catalog::default_catalog_path().map_err(|error| error.to_string())?;
    let _ = catalog_path;
    emit_startup_stage("environment", stage_started);
    // State the retrieval mode once at startup: with no model, recall is lexical and
    // graph-only, and the resident log is where that is visible before any command is run.
    stderr_line!("[startup] {}", crate::store::MODEL_READINESS.line());
    let stage_started = Instant::now();
    let (identity, startup_claim) = prepare_runtime_identity(&runtime)?;
    emit_startup_stage("identity", stage_started);
    // Publish the handshake manifest before the endpoint admits any peer; a
    // resident that has not published it must refuse every handshake.
    let active_manifest = crate::installation_manifest::build_active_manifest(
        &identity,
        &startup_claim,
        &runtime.workspace_root,
    );
    let stage_started = Instant::now();
    crate::installation_manifest::publish_active_manifest(active_manifest)
        .map_err(|error| format!("publish installation manifest: {error}"))?;
    emit_startup_stage("active_manifest", stage_started);
    std::env::set_var(
        "RIGHTKIT_CONTEXT_INSTALLATION_ID",
        &identity.installation_id,
    );
    std::env::set_var(
        "RIGHTKIT_CONTEXT_SERVICE_INSTANCE_ID",
        &startup_claim.service_instance_id,
    );
    // Start only after all pre-serve initialization can still fail, so a
    // startup error leaves no detached watcher behind.
    lifecycle.grant_background("resident_serve");
    let stage_started = Instant::now();
    start_resident_blueprint()?;
    emit_startup_stage("blueprint_supervisor_dispatch", stage_started);
    Ok(ResidentEngine {
        db: runtime.db.clone(),
        state_root: runtime.workspace_root.clone(),
        started: startup_started,
        identity,
        claim: startup_claim,
        lifecycle,
        _claim: claim,
    })
}

// ---------------------------------------------------------------------------
// Resident endpoint (C3)
// ---------------------------------------------------------------------------

use rightkit_context_protocol::{
    RESIDENT_APP as ENDPOINT_APP, RESIDENT_SERVICE as ENDPOINT_SERVICE,
};
const ENDPOINT_RANK: u8 = 10;

/// What the endpoint dispatches into. `push` and `status` use the resident's store; `pull`,
/// `blueprint` and `hook` run the same code as the in-process path.
pub struct EndpointContext {
    /// Root of the endpoint's discovery tree (the Membrane state root).
    pub state_root: PathBuf,
    pub store: crate::MemoryStore,
}

/// A running endpoint. Dropping it releases the socket or pipe and removes
/// the discovery record.
pub struct Endpoint {
    hosted: rightkit_daemon::Hosted,
}

impl Endpoint {
    /// Socket path (Mac) or pipe name (Windows) the endpoint is bound to,
    /// for `resident.json`.
    pub fn endpoint(&self) -> String {
        self.hosted.record().endpoint.clone()
    }

    pub fn shutdown(&self) {
        self.hosted.shutdown();
    }

    /// Block until the endpoint released, or the timeout passes.
    pub fn wait_released(&self, timeout: Duration) -> bool {
        self.hosted.wait_released(timeout)
    }
}

struct EndpointHandler {
    store: crate::MemoryStore,
}

impl rightkit_daemon::Handler for EndpointHandler {
    fn call(
        &self,
        _cx: &rightkit_daemon::CallContext,
        method: &str,
        params: &serde_json::Value,
        _emit: &rightkit_daemon::Emit,
    ) -> Result<serde_json::Value, rightkit_daemon::ServiceError> {
        let answer = self.answer(method, params);
        serde_json::to_value(answer).map_err(|error| {
            rightkit_daemon::ServiceError::new("internal_error", error.to_string())
        })
    }
}

impl EndpointHandler {
    fn answer(
        &self,
        method: &str,
        params: &serde_json::Value,
    ) -> rightkit_context_protocol::ResidentAnswerV1 {
        use rightkit_context_protocol::{ResidentAnswerV1, ResidentOp, ResidentRequestV1};
        let request: ResidentRequestV1 = match serde_json::from_value(params.clone()) {
            Ok(request) => request,
            Err(error) => {
                return ResidentAnswerV1::failure("bad_request", error.to_string(), false);
            }
        };
        if request.v != rightkit_context_protocol::RESIDENT_ENDPOINT_VERSION {
            return ResidentAnswerV1::failure(
                "version_unsupported",
                "unsupported request version",
                false,
            );
        }
        if request.op.as_str() != method {
            return ResidentAnswerV1::failure("bad_request", "op does not match method", false);
        }
        if !lifecycle_control().admission_open() {
            return ResidentAnswerV1::failure("draining", "resident is shutting down", true);
        }
        let args = request.args;
        match request.op {
            ResidentOp::Pull => Self::fold_pull_answer(crate::cli::op_pull(&args)),
            ResidentOp::Blueprint => Self::fold(crate::cli::op_blueprint(&args)),
            ResidentOp::Push => self.push(&args),
            ResidentOp::Status => ResidentAnswerV1::success(self.status()),
            ResidentOp::Hook => ResidentAnswerV1::success(crate::hook::run_hook_payload(args)),
            ResidentOp::MemorySearch => {
                Self::fold(crate::cli::op_memory_search(&self.store, &args))
            }
            ResidentOp::MemoryDelete => {
                Self::fold(crate::cli::op_memory_delete(&self.store, &args))
            }
            ResidentOp::AdaptOperator => Self::status_envelope(
                crate::adapt_service::operator_response(&self.store, &args.to_string()),
            ),
            ResidentOp::AdaptObservation => Self::status_envelope(
                crate::serve::observation_body_response(&self.store, &args.to_string()),
            ),
            ResidentOp::TranscriptTip => {
                Self::status_envelope(crate::adapt_observations::session_transcript::tip_response(
                    &self.store,
                    &args.to_string(),
                ))
            }
        }
    }

    /// A `(status, body)` route answer as a resident answer. The status is data here, not a
    /// failure, so a refusal keeps its typed body (a transcript cursor mismatch carries the
    /// current cursor) for the caller to map to an exit code.
    fn status_envelope(answer: (u16, String)) -> rightkit_context_protocol::ResidentAnswerV1 {
        let (status, text) = answer;
        let body = match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(value) => value,
            Err(_) => serde_json::Value::String(text),
        };
        rightkit_context_protocol::ResidentAnswerV1::success(
            serde_json::json!({"status": status, "body": body}),
        )
    }

    /// An operation's `Result` as a resident answer. A failure is not retryable: the caller
    /// runs the query in process instead.
    fn fold(
        result: Result<serde_json::Value, String>,
    ) -> rightkit_context_protocol::ResidentAnswerV1 {
        use rightkit_context_protocol::ResidentAnswerV1;
        match result {
            Ok(data) => ResidentAnswerV1::success(data),
            Err(error) => ResidentAnswerV1::failure("operation_failed", error, false),
        }
    }

    /// A pull's `Result` as a resident answer. An authorization denial is answered under its own
    /// code, so the caller reports it and never runs the pull again in process.
    fn fold_pull_answer(
        result: Result<serde_json::Value, String>,
    ) -> rightkit_context_protocol::ResidentAnswerV1 {
        match result {
            Err(error) if error.starts_with(crate::authorization::PULL_REFUSAL_PREFIX) => {
                rightkit_context_protocol::ResidentAnswerV1::failure(
                    crate::authorization::PULL_REFUSAL_CODE,
                    error,
                    false,
                )
            }
            other => Self::fold(other),
        }
    }

    /// `memory push`: the durable memory write, through the same handler the
    /// in-process path uses.
    fn push(&self, args: &serde_json::Value) -> rightkit_context_protocol::ResidentAnswerV1 {
        use rightkit_context_protocol::ResidentAnswerV1;
        let Some(request) = args.as_object() else {
            return ResidentAnswerV1::failure("bad_request", "push args must be an object", false);
        };
        let (status, body) = crate::serve::explicit_memory_response(
            &self.store,
            rightkit_context_protocol::explicit::ExplicitOperation::Remember,
            request,
        );
        if (200..300).contains(&status) {
            ResidentAnswerV1::success(body)
        } else {
            let text = |key: &str| {
                body.get(key)
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            };
            ResidentAnswerV1::failure(
                &text("code").unwrap_or_else(|| format!("http_{status}")),
                text("error").unwrap_or_else(|| "memory write refused".into()),
                status >= 500,
            )
        }
    }

    fn status(&self) -> serde_json::Value {
        serde_json::json!({
            "running": true,
            "pid": std::process::id(),
            "build": build_info(),
            "blueprint": resident_blueprint_status(),
            "embedderDim": self.store.embedder_dim(),
            "model": crate::store::MODEL_READINESS.status_json(),
            "background": lifecycle_control().background_authority_open(),
        })
    }
}

/// Bind the owner-only endpoint and publish `<state>/runtime/membrane/service.json`.
/// `rightkit-daemon` creates the socket in a 0700 folder with mode 0600 and
/// checks the peer uid on every accept on the Mac; on Windows it creates a
/// named pipe whose DACL allows only the current user SID and rejects remote
/// clients. The returned `Endpoint` serves on its own threads until dropped or
/// shut down; the serve loop owns its lifetime.
pub fn start_endpoint(context: EndpointContext) -> Result<Endpoint, String> {
    let suite = rightkit_daemon::Suite::at(&context.state_root)
        .map_err(|error| format!("resident endpoint root: {error}"))?;
    let allowlist = rightkit_daemon::Allowlist::new().allow(
        ENDPOINT_APP,
        rightkit_context_protocol::ResidentOp::ALL
            .map(rightkit_context_protocol::ResidentOp::as_str),
    );
    let config = rightkit_daemon::ServiceConfig::new(
        ENDPOINT_SERVICE,
        "membrane-resident",
        ENDPOINT_RANK,
        env!("CARGO_PKG_VERSION"),
        allowlist,
    );
    let handler = Arc::new(EndpointHandler {
        store: context.store,
    });
    let hosted = rightkit_daemon::host(&suite, config, handler)
        .map_err(|error| format!("start resident endpoint: {error}"))?;
    emit_lifecycle("ready", Some("endpoint"));
    Ok(Endpoint { hosted })
}
