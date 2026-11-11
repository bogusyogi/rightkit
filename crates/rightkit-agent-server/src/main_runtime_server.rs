use crate::api::lock_recover;

/// Set owner-only permissions across a data directory and the files in it.
///
/// The daemon's state directories accumulate sensitive files that each write
/// path was expected to protect on its own: the session database and its WAL
/// sidecars, the memory store, `config.toml` (provider routes), workspace-trust
/// decisions, permission denials, and a middleware span log holding paths and
/// tool metadata. Hand-rolled per-writer chmods left gaps — a live install had
/// ten group/world-readable files in these directories after the database
/// itself had been "fixed".
///
/// Now a thin wrapper over [`rightkit_fs::sweep_owner_only`] so
/// the daemon has exactly one directory-sweep implementation. The sweep runs
/// on every start rather than only at creation: a creation-time fix does
/// nothing for the installs already on disk with loose modes, which is
/// every install that predates it. Best-effort throughout — a directory on
/// a filesystem without POSIX modes must not stop the daemon booting.
pub(crate) fn harden_data_dir(dir: &std::path::Path) {
    /// RightKit-owned entries directly inside the data directory. Foreign
    /// siblings are deliberately left untouched.
    const OWNED_STATE_NAMES: &[&str] = &[
        "coderight.db",
        "coderight.db-wal",
        "coderight.db-shm",
        "daemon.port",
        "config.toml",
        "triggers.json",
        "runtime-middleware.json",
        "workspace-trust.json",
        "permission-denials.json",
        "memory.db",
    ];
    if dir.is_dir() {
        let _ = rightkit_fs::sweep_owner_only(dir, OWNED_STATE_NAMES);
    }
}

async fn serve(
    port: u16,
    db: Option<String>,
    model: Option<String>,
    provider: Option<String>,
    context_mode: ContextMode,
    resident: bool,
) {
    // Windows: every descendant (membrane worker, PTY hosts, shells) dies with
    // this process, so `stop`/crash can never orphan a child.
    crate::daemon::kill_tree::contain_descendants_in_job();

    rightkit_onnx::agent_ort::configure_ort_dylib();
    rightkit_onnx::agent_ort::configure_embed_model_dir();
    if cfg!(feature = "fastembed") {
        if let Err(diagnostic) = crate::startup::validate_ort_dylib_path() {
            eprintln!("fatal: {diagnostic}");
            std::process::exit(1);
        }
    }

    let token = generate_token();
    let db_path = db.unwrap_or_else(default_db_path);
    // A daemon on the default per-user DB is the machine's primary daemon and
    // owns the shared `~/.coderight` surfaces (daemon.port, journals). A
    // daemon started with an explicit `--db` elsewhere (QA, dev snapshots) is
    // auxiliary: it must not clobber the primary's port file — the installed
    // app's CLI/attach discovery breaks mid-session — and must not share the
    // primary's journal dir, or its startup recovery scan replays the
    // primary's interrupted sessions against the wrong database.
    let primary_daemon = crate::daemon::ownership::same_db_path(
        std::path::Path::new(&db_path),
        std::path::Path::new(&default_db_path()),
    );

    // CG-C01 install-layout recovery: an interrupted staged activation is a rightkit-fs
    // transaction in the installer target's directory. It is resolved here, before any
    // activation or reload state is read and before storage/transport make this daemon
    // observable: uncommitted work rolls back, committed work finishes its cleanup. DL-014 still
    // owns process reload; this call only repairs the durable install-layout leg.
    if primary_daemon {
        if let Ok(target) = std::env::current_exe() {
            match crate::daemon::migrate::recover_activation(&target) {
                Ok(report) => {
                    for recovered in &report.transactions {
                        eprintln!(
                            "[serve] install activation transaction {}: {:?}",
                            recovered.id, recovered.outcome
                        );
                    }
                }
                Err(error) => eprintln!("[serve] install activation recovery refused: {error}"),
            }
        }
    }

    // DL-001: claim the primary daemon instance before any discovery,
    // transport, or storage startup can make this process observable. The
    // guard is held by this scope for the full server lifetime; dropping it
    // on return releases the OS lock. Auxiliary --db instances intentionally
    // remain independent so QA/dev snapshots do not contend with the
    // installed daemon's singleton.
    //
    // RIGHTKIT_SERVE_FORCE only skips the live-daemon health guard and the
    // stale-owner takeover below; it never skips the exclusive lock itself. It is a test seam:
    // a release build ignores it entirely.
    let serve_force = false;
    let ownership_lock = if primary_daemon {
        let lock_path = crate::daemon::ownership::default_lockfile_path()
            .unwrap_or_else(|| std::path::PathBuf::from("daemon.lock"));
        let acquisition = match crate::daemon::ownership::OwnershipLock::acquire(&lock_path) {
            Ok(Some(lock)) => Ok(Some(lock)),
            Ok(None)
                if !serve_force
                    && crate::daemon::ownership::default_discovery_path()
                        .is_some_and(|path| path.is_file()) =>
            {
                crate::daemon::takeover::acquire_or_take_over(
                    &lock_path,
                    crate::daemon::takeover::TAKEOVER_GRACE,
                )
            }
            Ok(None) => Ok(None),
            Err(error) => Err(error),
        };
        match acquisition {
            Ok(Some(lock)) => {
                rightkit_agent_policy::register_daemon_lock_path(lock_path.clone());
                Some(lock)
            }
            Ok(None) => {
                eprintln!(
                    "[serve] another RightKit daemon owns {}; refusing second instance",
                    lock_path.display()
                );
                std::process::exit(1);
            }
            Err(error) => {
                eprintln!(
                    "[serve] cannot acquire daemon ownership lock {}: {error}",
                    lock_path.display()
                );
                std::process::exit(1);
            }
        }
    } else {
        None
    };
    let ownership_lock = std::sync::Arc::new(std::sync::Mutex::new(ownership_lock));

    // Single-daemon-per-DB guard (U-F): refuse to start a second daemon on the
    // same database unless explicitly overridden. A different db path (tests
    // use distinct temp DBs) always proceeds.
    if !serve_force {
        if let Err(message) = run_startup_guard(&db_path).await {
            eprintln!("{message}");
            std::process::exit(1);
        }
    }

    // Keep an exclusive DB-lifetime claim until the server future returns.
    // This closes the race between two clients that both pass port-file
    // liveness checks before either writes discovery.
    let db_lock_result = {
        let database = std::path::Path::new(&db_path);
        let canonical = crate::daemon::ownership::canonical_db_path(database);
        rightkit_fs::FileLock::try_acquire(&canonical.with_extension("daemon.lock"))
    };
    let _db_lifetime_lock = match db_lock_result {
        Ok(lock) => lock,
        Err(error) => {
            eprintln!("[serve] database daemon lock unavailable: {error}");
            std::process::exit(1);
        }
    };

    // DL-013 — orphan recovery sweep, once per daemon start.
    //
    // Runs after the DB-lifetime claim is held and before any new work is
    // accepted. `recover_orphans` is deliberately conservative: it re-checks
    // each journaled worker's exact process identity and *adopts* the ones
    // that still match, terminating only those whose pid has since been
    // reused by an unrelated process — rather than either blindly trusting or
    // blindly killing every recorded pid.
    //
    // Only the primary daemon sweeps. An auxiliary daemon started against an
    // explicit `--db` elsewhere must not replay the primary's interrupted
    // sessions against the wrong database (see `primary_daemon` above).
    if primary_daemon {
        if let Some(journal) = crate::daemon::recovery::default_journal_path() {
            let adopted = crate::daemon::recovery::recover_orphans(&journal, std::process::id());
            if !adopted.is_empty() {
                eprintln!(
                    "[serve] recovered {} orphaned worker(s) from a previous run",
                    adopted.len()
                );
            }
        }
    }

    // Try to open storage — non-fatal: we log and continue without persistence.
    //
    // CG-C01 — routed through `daemon::migrate::open_with_migration_report`
    // rather than `Storage::open` directly, so the pending-migration journal
    // and the applied-migrations flag are observable at runtime instead of
    // being a silent side effect of opening the database. Both are reported
    // below; the journal write/clear itself happens inside
    // `rightkit_agent_engine_storage::migrations::migrate`, which `Storage::open` already
    // calls, so this wrapper adds reporting, not a second migration path.
    let storage: Option<SharedStorage> =
        match crate::daemon::migrate::open_with_migration_report(&db_path) {
            Ok((s, migration_report)) => {
                // Exclusive DB-lifetime ownership is held before storage opens.
                // An unresolved prior-process fence is an unknown outcome,
                // never permission to dispatch the accepted input again.
                match s.reconcile_orphaned_message_dispatches(
                    &rightkit_agent_engine_protocol::now_iso(),
                ) {
                    Ok(count) if count > 0 => eprintln!(
                        "{}",
                        serde_json::json!({
                            "event": "orphaned_message_dispatches_reconciled",
                            "count": count,
                            "effectState": "unknown"
                        })
                    ),
                    Ok(_) => {}
                    Err(error) => {
                        eprintln!("[serve] cannot reconcile orphaned dispatch fences: {error}");
                        std::process::exit(1);
                    }
                }
                if let Some(journal) = &migration_report.resumed_from_crash {
                    eprintln!(
                        "[serve] recovered from an interrupted migration: a previous run \
                         left a pending journal (v{} -> v{}, {} pending, pid {}); \
                         schema resumed cleanly via PRAGMA user_version",
                        journal.from_version,
                        journal.to_version,
                        journal.pending_versions.len(),
                        journal.pid
                    );
                }
                if migration_report.applied_migrations {
                    eprintln!("[serve] database migrations applied on startup");
                }
                Some(SharedStorage::new(s))
            }
            Err(e) => {
                eprintln!("[serve] storage unavailable ({e}), events won't be persisted");
                None
            }
        };

    // Install the daemon-lifetime Adapt host before routes are composed so
    // proposal intake uses the same durable guidance store across requests
    // and restarts. Policy remains fail-closed until startup supplies its
    // authenticated authority adapter.
    if let Some(ref durable_storage) = storage {
        // Receipts persist to the same durable storage as guidance so replay
        // dedup and the stale-base CAS race guard survive a daemon restart;
        // an in-memory log would otherwise reopen every prior receipt_id to
        // re-application after every restart.
        let host = crate::api_adapt::AdaptApplicationHost::from_storage(
            durable_storage.clone(),
            std::sync::Arc::new(rightkit_agent_engine_core::adapt_consumer::DenyAllAuthority),
            rightkit_agent_engine_core::adapt_consumer::ProjectionRegistry::new(),
            std::sync::Arc::new(
                crate::api_adapt::adapt_application_host::StorageApplicationReceiptLog::new(
                    durable_storage.clone(),
                ),
            ),
        );
        let _ = crate::api_adapt::install_adapt_application_host(host);
    }

    // Bind first so we know the actual port (when port == 0).
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .unwrap_or_else(|error| {
            panic!(
                "failed to bind serve port: {}",
                file_descriptor_error(&error, &raise_nofile_soft_limit())
            )
        });
    let actual_port = listener.local_addr().expect("local_addr").port();

    // Emit the startup contract line as the very first stdout output.
    let info = LoopbackRecord::for_this_process(
        actual_port,
        &token,
        &RecordLabels {
            database_path: db_path.to_string(),
            protocol_version: 1,
            schema_id: "coderight.daemon.v1".to_string(),
            runtime: "rust".to_string(),
            generation: 1,
        },
    );
    rightkit_sidecar::write_json_line_flush(&mut std::io::stdout().lock(), &info, 64 * 1024)
        .unwrap_or_else(|error| panic!("startup contract line not written: {error}"));
    init_daemon_telemetry();

    let config = load_current_merged_config();

    // Resolve provider / model (configured routes first, then the OpenRouter hosted default).
    let engine_cfg = if let Some(cfg) =
        resolve_engine_config(&config, model.as_deref(), provider.as_deref())
    {
        eprintln!(
            "[serve] provider route model={} baseUrl={}",
            cfg.model_id, cfg.base_url
        );
        Some(cfg)
    } else {
        // State what was actually inspected. "No API key" is only one of
        // the reasons resolution fails; a configured route that names an
        // unknown model, or a workspace-only route the boot-time merged
        // config never sees, lands here too.
        let route_count = config.provider_routes.len();
        let reason = if route_count == 0 {
            "no provider API key and no local provider route are configured (checked ~/.coderight/config.toml). Add a key in Settings, or register a local OpenAI-compatible route, then restart RightKit.".to_string()
        } else {
            let names = config
                .provider_routes
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "{route_count} provider route(s) are configured ({names}) but none resolved for the requested model{}. Check that the route's model matches the session model and that its baseUrl is reachable, then restart RightKit.",
                model
                    .as_deref()
                    .map(|m| format!(" '{m}'"))
                    .unwrap_or_default()
            )
        };
        eprintln!("[serve] engine unavailable — {reason}");
        crate::api::record_engine_unavailable_reason(reason);
        None
    };

    // Load agent overrides from config files and apply them on top of builtins.
    let remote_cfg = config.remote.clone();
    let agent_overrides: std::collections::HashMap<String, (String, String)> = config
        .orchestration
        .agents
        .into_iter()
        .map(|(name, ov)| {
            let provider_id = normalized_agent_override_provider_id(ov.provider_id);
            (name, (ov.model_id, provider_id))
        })
        .collect();
    let boss_model = config.orchestration.boss_model;
    let mut effective_registry = AgentRegistry::with_builtins();
    if !agent_overrides.is_empty() {
        effective_registry.apply_config_overrides(&agent_overrides);
    }
    let resolved_boss_model = resolve_configured_model_alias(&boss_model, &effective_registry);
    let budget_cfg = config.orchestration.budget.clone();

    // Persist learning under the state root so self-improvement accumulates across
    // restarts; fall back to in-memory if there is no state directory.
    let evo_path =
        rightkit_agent_config::state_root().map(|root| root.join("evolution.db"));
    if let Some(p) = evo_path.as_ref() {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
    }
    let evo_db = evo_path
        .as_ref()
        .and_then(|p| evo_db::EvolutionDb::open(p).ok());
    // Membrane is Pulse's CLI only (Orthic-Labs/pulse#26). Memory routes through
    // the adapter-backed backend; Pulse absent yields a typed unavailable backend
    // with context off (OFF_REASON). No resident holder, no loopback transport.
    let mut serve_context_degraded: Option<String> = None;
    let (memory_backend, memory_provenance) =
        match rightkit_memory::backend::native::connect_from_environment() {
            Ok(backend) => {
                // Pulse absent connects to the typed unavailable backend; the
                // context is still off and must be reported degraded.
                if rightkit_memory::backend::native::context_is_off() {
                    use std::io::Write as _;
                    let _ = writeln!(
                        std::io::stderr(),
                        "[serve] Membrane context unavailable (context off): {}",
                        rightkit_memory::membrane::OFF_REASON
                    );
                    serve_context_degraded =
                        Some(rightkit_memory::membrane::OFF_REASON.to_string());
                }
                let provenance =
                    rightkit_memory::memory_provenance::MemoryProvenanceStore::from_backend(
                        backend.clone(),
                    );
                (backend, provenance)
            }
            Err(error) => {
                // Keep diagnostics off startup-contract stdout, regardless of
                // tracing subscriber configuration.
                use std::io::Write as _;
                let _ = writeln!(
                    std::io::stderr(),
                    "[serve] Membrane context unavailable (context off): {error}"
                );
                // D18: context failure never prevents local execution. Keep the
                // daemon alive with typed-unavailable memory and a visible reason;
                // no local context store is created as a fallback.
                serve_context_degraded = Some(rightkit_memory::membrane::OFF_REASON.to_string());
                let backend: std::sync::Arc<dyn rightkit_memory::backend::MemoryBackend> =
                    std::sync::Arc::new(
                        rightkit_memory::backend::unavailable::UnavailableMemoryBackend::new(
                            error.code(),
                            error.message(),
                        ),
                    );
                let provenance =
                    rightkit_memory::memory_provenance::MemoryProvenanceStore::unavailable(
                        rightkit_memory::membrane::OFF_REASON.to_string(),
                    );
                (backend, provenance)
            }
        };
    let (learning, audit, observations, trajectories) = match evo_db {
        Some(db) => (
            learning::LearningHarness::open(db.clone()),
            audit::AuditLog::open(db.clone()),
            observation::ObservationStore::open(db.clone()),
            trajectory::TrajectoryStore::open(db),
        ),
        None => (
            learning::LearningHarness::new(),
            audit::AuditLog::new(),
            observation::ObservationStore::new(),
            trajectory::TrajectoryStore::new(),
        ),
    };

    let token_for_telegram = token.clone();
    let mut state = DaemonState::new(token).with_context_mode(context_mode);
    if let Some(reason) = serve_context_degraded {
        state = state.with_context_degraded(reason);
    }
    let mut state = state
        .with_runtime_info(actual_port, db_path.clone())
        .with_runtime_middleware_store_path(runtime_middleware_store_path(&db_path))
        .with_recipe_store_path(recipe_store_path(&db_path))
        .with_skillopt_store_path(skillopt_store_path(&db_path))
        .with_spec_store_path(spec_store_path(&db_path))
        .with_learning(learning)
        .with_memory_provenance(memory_provenance)
        .with_memory_backend(memory_backend)
        .with_membrane(crate::native_transcript_exporter::membrane_backend())
        .with_transcript_exporter(Some(std::sync::Arc::new(
            crate::native_transcript_exporter::NativeTranscriptExporter::new(),
        )))
        .with_audit(audit)
        .with_observations(observations)
        .with_trajectories(trajectories)
        .with_telegram_remote_config(remote_cfg.clone());

    // PL-004: provider extensions are admitted through daemon-owned registry
    // state. Discovery contributes no trust; malformed/colliding documents
    // remain visible as startup diagnostics instead of partial routes.
    if let Err(error) = state.register_provider_extensions() {
        eprintln!("[serve] provider extension registration unavailable: {error}");
    }

    // FM-145: the daemon decides Pro itself. A release daemon opens the licence record the shell's
    // licence owner maintains (locators only: no token, key or device id is ever passed in) and
    // verifies it live through `EngineEntitlements`, so a refresh, revocation or new activation
    // needs no restart. Without locators (a CLI or standalone daemon) it stays on the Free tier.
    // The owner delivers its snapshot over the daemon's private IPC (`license.snapshot`), which
    // identifies the peer process (rightkit#1501); the service stays published while this function
    // runs and is released when it returns.
    let mut _license_service: Option<rightkit_daemon::Service> = None;
    {
        if let Some(locators) = crate::license::launch_locators() {
            match crate::license::LicenseGate::open(locators) {
                Ok(gate) => {
                    let gate = std::sync::Arc::new(gate);
                    state = state.with_license_gate(gate.clone());
                    match crate::license_ipc::host_license_service(gate, locators, actual_port) {
                        Ok(service) => _license_service = service,
                        Err(error) => eprintln!(
                            "[license] licence snapshot service unavailable, running as Free: {error}"
                        ),
                    }
                }
                Err(error) => {
                    eprintln!("[license] licence gate unavailable, running as Free: {error}")
                }
            }
        }
    }

    // FM-100: eagerly initialize the global single-flight MCP pool at daemon
    // startup so concurrent session loads dedupe to one in-flight build (the
    // same contract jcode's `init_shared_pool` guarantees at daemon boot).
    let _mcp_pool = rightkit_mcp::agent_mcp::init_shared_pool().await;
    // FM-100 reaper: periodically evict idle (zero-lease) transports from the
    // lower leasing layer, then drop the matching shared single-flight cache
    // entries so a dead transport is never served. Both pools are daemon-wide
    // singletons, so this task is spawned exactly once at boot.
    {
        let shared = _mcp_pool.clone();
        let lower = state.mcp_pool.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                ticker.tick().await;
                let evicted = lower.evict_idle(std::time::Duration::from_secs(300));
                for key in evicted {
                    shared.invalidate_service(&key).await;
                }
            }
        });
    }
    if let Some(store) = &storage {
        state = state.with_storage(store.clone());
    }

    // Resolve the workflow feature gate from the merged config and attach a
    // per-router [`WorkflowApiState`] when the gate is on and storage is
    // available. The router mount in [`api_router::build_router`] honours
    // the gate (mounts when on, hides when off); the per-handler 423 in
    // `api_workflows::routes` is the defence-in-depth fallback for the
    // (currently impossible) case where the gate is on but state is None.
    let workflows_feature = rightkit_workflow::control_plane::prelude::FeatureGate::from_config(
        config.features.workflows_enabled,
    );
    state = state.with_workflows_feature(workflows_feature);
    if workflows_feature.is_enabled() {
        if let Some(store) = storage.clone() {
            let import_verifier = std::sync::Arc::new(
                crate::api_workflows::state::Ed25519PackageVerifier::from_config(
                    &config.features.workflow_import_trusted_public_keys,
                ),
            );
            let workflow_api_state =
                crate::api_workflows::WorkflowApiState::new_with_import_verifier(
                    store,
                    workflows_feature,
                    import_verifier,
                );
            tracing::info!(
                workflow_gate = workflows_feature.is_enabled(),
                "autonomous workflow control plane mounted (WP-09 / ADR-0021)"
            );
            state = state.with_workflow_api_state(workflow_api_state);
        } else {
            tracing::warn!(
                "workflows_enabled=true but storage is unavailable; \
                 workflow REST surface is not mounted"
            );
        }
    }
    if !primary_daemon {
        if let Some(dir) = std::path::Path::new(&db_path).parent() {
            state = state.with_journal_dir(dir.join("journals"));
        }
    }

    // Seed the in-memory settings store from persisted config so budgets and
    // iteration limits saved in a previous session take effect on startup.
    {
        let mut s = lock_recover(&state.settings, "state.settings");
        s.per_task_max_usd = budget_cfg.per_task_max_usd;
        s.per_goal_max_usd = budget_cfg.per_goal_max_usd;
        s.per_session_max_usd = budget_cfg.per_session_max_usd;
        s.per_agent_call_max_usd = budget_cfg.per_agent_call_max_usd;
        s.max_retries_per_role = budget_cfg.max_retries_per_role;
        s.max_wall_clock_minutes = budget_cfg.max_wall_clock_minutes;
        s.max_iterations = budget_cfg.max_iterations;
        s.max_workers = budget_cfg.max_workers;
        // `serve --resident` is how installed-service launches (SCM, launchd,
        // systemd --user, the per-user task lane) declare they must survive
        // the no-client linger window. It only ever turns the toggle on and
        // is intentionally not persisted: the settings store stays the sole
        // writer, so uninstalling the service cleanly reverts residency.
        s.keep_daemon_resident = s.keep_daemon_resident || resident;
    }
    if let Some(mut cfg) = engine_cfg {
        cfg.fallback_models = config.model.fallback_models.clone();
        // Boss model from config overrides the CLI --model flag. The setting
        // accepts either a raw model slug or an agent alias such as
        // "conductor"; resolve aliases before building the provider route.
        if !resolved_boss_model.is_empty() {
            cfg.model_id = resolved_boss_model;
        }
        state = state.with_engine(cfg);
    }
    if !agent_overrides.is_empty() {
        state.agent_registry = Arc::new(effective_registry);
    }

    // Startup housekeeping (marking interrupted sessions + journal scan +
    // persistent recovery-marker reconciliation) runs in the BACKGROUND so it
    // never delays the HTTP listener bind: with accumulated journals/sessions
    // and a loaded machine the serial version pushed readiness past 30s
    // (e2e readiness deadline, and a bad first-run feel). Nothing below is
    // required to serve requests.
    let journal_dir_for_startup = state.journal_dir.clone();
    let recovery_live_generations = state.cli_recovery_live_generations.clone();
    let recovery_live_epoch_state = state.cli_recovery_live_epoch.clone();
    {
        let storage = storage.clone();
        tokio::spawn(async move {
            // PR 13: walk every active binding through the recovery
            // walker before any resume attempts to reach the bridge.
            // The walker projects each persisted row to a snapshot,
            // re-validates the executable digest, and compares stored
            // generation / epoch against the live runtime state. Each
            // outcome is logged so the UI can surface refusals with
            // their stable code and hint; outcomes marked `Resume`
            // become the set the supervisor resumes natively.
            if let Some(store) = &storage {
                let live_epoch = recovery_live_epoch_state.current();
                let outcomes = crate::cli_recovery_runtime::walk_active_bindings_at_startup(
                    store,
                    &recovery_live_generations,
                    &live_epoch,
                )
                .await;
                for outcome in outcomes {
                    match outcome.decision {
                        rightkit_agents::driver_host::cli_runtime::ResumeDecision::Resume {
                            ..
                        } => {
                            // Native resume will be re-attempted when the
                            // executor's resume() path is invoked with
                            // the daemon-bound guard attached.
                        }
                        rightkit_agents::driver_host::cli_runtime::ResumeDecision::Refuse(
                            ref error,
                        ) => {
                            eprintln!(
                                "[serve] refusing to resume binding {}: {} ({})",
                                outcome.binding_id,
                                error,
                                error.code()
                            );
                        }
                    }
                }
            }
            if let Some(store) = &storage {
                let s = store.lock().await;
                // Recorded before the rows are read, so it is a lower bound on
                // every write that can race this scan. See the CAS call below.
                let scan_started = now_iso();
                if let Ok(sessions) = s.list_sessions() {
                    let now = now_iso();
                    for sess in sessions {
                        let terminal = matches!(
                            sess.status.as_str(),
                            "dormant" | "done" | "deleted" | "archived" | "idle" | ""
                        );
                        if !terminal {
                            // ra-008: this scan can run concurrently with a
                            // freshly-accepted turn that persists "running"
                            // on this exact row (HTTP readiness is published
                            // while this background walk is still going).
                            // The naive `update_session_status(..,
                            // "interrupted", ..)` used to fire unconditionally
                            // from the stale `sessions` snapshot taken above,
                            // silently overwriting that live "running" write
                            // with a stale "interrupted" one.
                            //
                            // Write only if the row is still exactly what this
                            // scan observed (same status AND same updated_at),
                            // i.e. nothing has touched it since
                            // `list_sessions()` ran above. Any concurrent
                            // writer (a new turn, another recovery path)
                            // changes at least `updated_at`, so this is an
                            // optimistic-concurrency guard without needing a
                            // boot-generation column on the row.
                            //
                            // The comparison lives in the UPDATE's own WHERE
                            // clause rather than in a preceding `get_session`:
                            // reading and then writing is two statements, and a
                            // concurrent turn committing between them would
                            // still lose its "running" to this stale
                            // "interrupted". One conditional statement leaves
                            // no such window.
                            //
                            // Equality alone is still not enough, because
                            // `now_iso()` resolves to whole seconds: a live
                            // turn re-persisting the *same* status inside the
                            // same second yields a row identical to the
                            // snapshot, and equality would match. Every such
                            // write happens at or after `scan_started`, and
                            // every genuinely stale row predates it, so passing
                            // that bound separates them. A row written in the
                            // same second the scan began is skipped rather than
                            // clobbered; it is re-examined on the next start.
                            let _ = s.update_session_status_if_unchanged(
                                &sess.id,
                                "interrupted",
                                &now,
                                &sess.status,
                                &sess.updated_at,
                                Some(&scan_started),
                            );
                        }
                    }
                }
            }
            if let Some(jdir) = journal_dir_for_startup.as_deref() {
                journal::detect_interrupted_journals(jdir).await;
            }
            // Reconcile persistent recovery markers (P1 J1): for each
            // persisted session, set or clear `recovery_marker` based on
            // whether a `.journal` file still exists. Picks up journals
            // created or closed in previous daemons without losing the UI
            // badge for sessions that crashed mid-flight.
            if let (Some(store), Some(jdir)) = (&storage, journal_dir_for_startup.as_deref()) {
                journal::persist_recovery_markers(store, jdir).await;
            }
        });
    }

    // Seed supervisor residency before any client attaches. Workflow-enabled
    // daemons remain eligible for schedule evaluation from birth; workers
    // recovered above are already adopted by the workflow recovery path.
    crate::api_clients::supervisor_seed_startup_residency(usize::from(
        workflows_feature.is_enabled(),
    ));

    // Seed a default session so a fresh daemon is immediately usable. Event
    // persistence (and the session row) are now wired inside `create_session`
    // for every session, so no special-casing here.
    // Titled "New session" so the auto-title path (`should_auto_title`) renames
    // it from the first real message, like every user-created session.
    let _default_session = state.create_session("default", None, "", "New session", None);

    scheduler::spawn_schedule_runner(state.clone());
    // Keep idle minimization resident from daemon birth, including the
    // zero-client case. The loop is scope-owned so shutdown aborts its timer
    // instead of leaving a background wakeup behind.
    let idle_state = state.clone();
    // RA-003: the sweep itself now runs for real (see
    // `api_clients::run_daemon_idle_eviction_sweep`) instead of only reaping
    // expired client leases. `idle_eviction_policy_from_env` defaults to
    // `Off` — an operator opts in via `RIGHTKIT_IDLE_EVICTION_MINUTES` —
    // so wiring the caller does not change behavior for anyone who hasn't
    // set that variable.
    let idle_eviction_policy = crate::api_clients::idle_eviction_policy_from_env();
    let _idle_eviction_loop = crate::daemon::idle_eviction::IdleEvictionLoop::start(
        std::time::Duration::from_secs(60),
        move || {
            rightkit_agent_telemetry::record_wakeup();
            let _ = crate::api_clients::reap_expired_clients(&idle_state);
            let report = crate::api_clients::run_daemon_idle_eviction_sweep(
                &idle_state,
                idle_eviction_policy,
            );
            if report.evicted_workers > 0
                || report.eviction_failures > 0
                || report.passivation_failures > 0
                || report.drain_timed_out
            {
                tracing::info!(
                    refreshed = report.refreshed_workers,
                    refresh_failures = report.refresh_failures,
                    passivated = report.passivated_workers,
                    passivation_failures = report.passivation_failures,
                    evicted = report.evicted_workers,
                    eviction_failures = report.eviction_failures,
                    drain_timed_out = report.drain_timed_out,
                    "idle eviction sweep"
                );
            }
        },
    );
    crate::learning::spawn_daily_analyzer(std::sync::Arc::new(state.clone()));

    // Workflow processing uses same writer as HTTP handlers. Startup replay
    // happens before polling; shutdown drops this task through daemon watch.
    if workflows_feature.is_enabled() {
        if let Some(workflow_storage) = storage.clone() {
            let supervisor = crate::workflow::maybe_build_supervisor(
                workflow_storage.clone(),
                workflows_feature,
                std::sync::Arc::new(rightkit_workflow::control_plane::prelude::SystemClock::new()),
            );
            if let Some(supervisor) = supervisor {
                let recovery = crate::workflow::RecoveryWorker::new(
                    workflow_storage.clone(),
                    std::sync::Arc::new(
                        rightkit_workflow::control_plane::prelude::SystemClock::new(),
                    ),
                );
                let workflow_state = std::sync::Arc::new(state.clone());
                let mut workflow_shutdown = state.shutdown_tx.subscribe();
                tokio::spawn(async move {
                    if let Err(error) = recovery.rehydrate().await {
                        tracing::error!(%error, "workflow recovery failed");
                    }
                    let mut tick = tokio::time::interval(tokio::time::Duration::from_secs(1));
                    // FM-102 / S:G11: when a trusted remote A2A executor is
                    // configured, daemon startup now injects a real, fail-closed
                    // adapter (trusted transport + ed25519 trust-root verifier)
                    // through `new_with_a2a_adapter`. When unconfigured, A2A
                    // stays closed (`None`), preserving the fail-closed default.
                    let a2a_adapter = crate::workflow::executors::try_build_daemon_a2a_adapter();
                    let dispatcher = std::sync::Arc::new(
                        crate::workflow::dispatcher::RuntimeNodeDispatcher::new_with_a2a_adapter(
                            workflow_state,
                            a2a_adapter,
                        ),
                    );
                    let executor = crate::workflow::NodeExecutor::new(
                        workflow_storage.clone(),
                        supervisor.clone(),
                        dispatcher,
                        std::sync::Arc::new(
                            rightkit_workflow::control_plane::prelude::SystemClock::new(),
                        ),
                        crate::workflow::ResourceLimiters {
                            max_concurrent_per_run: 1,
                            max_total_concurrent: 4,
                            max_input_bytes: 1_048_576,
                        },
                    );
                    let continuations =
                        crate::workflow::ContinuationWorker::new(workflow_storage.clone());
                    let cron = crate::workflow::CronWorker::new(workflow_storage.clone());
                    let monitor_queue =
                        std::sync::Arc::new(crate::workflow::MonitorEventQueue::new());
                    let monitor = crate::workflow::MonitorWorker::new(
                        workflow_storage.clone(),
                        monitor_queue,
                    );
                    loop {
                        tokio::select! {
                            _ = workflow_shutdown.changed() => break,
                            tick_instant = tick.tick() => {
                                // R12/CR-38: one daemon wakeup. The workflow
                                // tick runs on its interval regardless of
                                // whether any continuation or cron job is
                                // actually due, so it counts toward idle cost
                                // the same way the presence reaper does.
                                rightkit_agent_telemetry::record_wakeup();
                                // OPS-003 event-loop stall probe: the interval
                                // resolves to its scheduled deadline; the gap
                                // between that deadline and now is scheduler
                                // delay, the honest stall measure.
                                rightkit_agent_telemetry::record_event_loop_delay(
                                    std::time::Instant::now()
                                        .saturating_duration_since(tick_instant.into()),
                                );
                                continuations.tick().await;
                                cron.tick().await;
                                monitor.tick().await;
                                match supervisor.claim_ready(crate::workflow::SupervisorLeaseTtl { initial_secs: 30, renewal_secs: 10 }).await {
                                Ok(claims) => for initial_claim in claims {
                                    let holder = format!("pid_{}", std::process::id());
                                    let mut claim = initial_claim;
                                    crate::api_clients::supervisor_claim_worker(
                                        &claim.run_id,
                                        &claim.run_id,
                                    );
                                    // A bounded drive owns one lease generation at a time. Each
                                    // renewed claim re-reads projections, so node selection never
                                    // runs on a stale snapshot after a sibling completion.
                                    for _ in 0..128 {
                                        let run_id = claim.run_id.clone();
                                        match executor.run_pass(claim).await {
                                            Ok(crate::workflow::PassOutcome::Advanced { .. }) => {
                                                match supervisor.claim_run(&run_id, &holder, 30).await {
                                                    Ok(next) => claim = next,
                                                    Err(error) => { tracing::warn!(%error, %run_id, "workflow lease heartbeat/renewal failed"); break; }
                                                }
                                            }
                                            Ok(crate::workflow::PassOutcome::Failed { failure, .. }) => {
                                                if let Err(error) = supervisor.release_run(&run_id, &holder, Some(failure)).await {
                                                    tracing::warn!(%error, %run_id, "workflow failed-run release failed");
                                                }
                                                crate::api_clients::supervisor_release_worker(&run_id);
                                                break;
                                            }
                                            Ok(crate::workflow::PassOutcome::Completed { .. }) => {
                                                if let Err(error) = supervisor.release_run(&run_id, &holder, None).await {
                                                    tracing::warn!(%error, %run_id, "workflow completed-run release failed");
                                                } else {
                                                    monitor.publish_workflow_completion(&run_id).await;
                                                }
                                                crate::api_clients::supervisor_release_worker(&run_id);
                                                break;
                                            }
                                            Ok(crate::workflow::PassOutcome::Busy | crate::workflow::PassOutcome::Waiting { .. } | crate::workflow::PassOutcome::Empty) => break,
                                            Err(error) => { tracing::warn!(%error, %run_id, "workflow executor pass failed"); break; }
                                        }
                                    }
                                },
                                Err(error) => tracing::warn!(%error, "workflow supervisor tick failed"),
                            }
                            },
                        }
                    }
                });
            }
        }
    }

    // Client-presence reaper (U-F): sweeps expired client leases and decides
    // whether the daemon should self-exit once the refcount has been at zero
    // (with nothing in flight) for the linger window. Never fires for a bare
    // dev/test daemon that no client has ever attached to.
    spawn_client_presence_reaper(state.clone());

    let telegram_runtime_status = state.telegram_status.runtime.clone();
    let shutdown_rx = state.shutdown_tx.subscribe();

    // OPS-003: periodic resource sampler feeding /runtime/resource-metrics.
    crate::api_runtime::spawn_resource_sampler(state.clone());
    let app = build_router(state);

    // Hourly retention job.
    //
    // CR-16: canonical-history invariant. The append-only `events` table is
    // the canonical event ledger; a `SessionCompacted` event is an
    // IMMUTABLE PROJECTION RECORD referencing the source seq range it
    // summarizes (see `rightkit_agent_engine_core::compaction_archive::
    // CompactionProjectionRecord`), never a rewrite of the events that
    // produced it. Deleting the pre-compaction rows this job used to run
    // unconditionally destroyed exactly the range a projection is supposed
    // to remain verifiable against -- replay, audit, export, and recovery
    // could no longer check a projection against its canonical source once
    // that source was gone. That made retention an IMPLICIT CONSEQUENCE of
    // compaction rather than a policy an operator opted into.
    //
    // Destructive retention remains fail-closed until the product has an
    // authenticated restoration path into the canonical ledger. The env var
    // is intentionally ignored: an operator toggle must not authorize
    // irreversible deletion of the only canonical event rows.
    let event_retention_enabled = false;
    if let Some(storage_arc) = storage.clone() {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(3600));
            let mut pending_projections: PendingProjections = std::collections::HashMap::new();
            loop {
                interval.tick().await;
                let store = storage_arc.lock().await;
                run_event_retention_pass(&store, event_retention_enabled, &mut pending_projections);
            }
        });
    }

    // Spawn the Telegram long-poll task if a bot token is configured.
    if let Some(bot_token) = remote_cfg.telegram_token {
        let spawn_port = actual_port;
        let spawn_token = token_for_telegram;
        let allowed = remote_cfg.telegram_allowed_users.clone();
        tokio::spawn(async move {
            telegram_task::run_telegram_bot(
                bot_token,
                allowed,
                spawn_port,
                spawn_token,
                telegram_runtime_status,
            )
            .await;
        });
    }

    // Publish discovery only after every blocking startup stage has completed.
    // A bound listener is not a ready daemon: publishing earlier makes clients
    // attach to a process that cannot answer while memory/model initialization
    // is still running or stuck.
    if primary_daemon {
        write_daemon_port_file(&info);
    } else {
        eprintln!("[serve] auxiliary daemon (--db): not writing ~/.coderight/daemon.port");
    }

    // Same readiness point, reported back to a reload coordinator. If this
    // process was spawned as a reload replacement it now satisfies the same
    // definition of ready that discovery uses, so publish `SocketReady` for
    // that request; the coordinator's `wait_ready` is blocked on exactly this
    // and would otherwise hit its deadline and roll us back. No-op for an
    // ordinary start (the identity env vars are absent).
    if crate::daemon::reload::publish_socket_ready_if_replacement() {
        eprintln!("[serve] published reload socket-ready for replacement");
    }

    // The drain is bounded. `with_graceful_shutdown` waits for in-flight
    // requests to finish, and an SSE stream never finishes — a subscribed UI
    // holds one open indefinitely. So an otherwise clean shutdown parked here
    // forever: the listener was already closed, the process stayed alive, and
    // the code below (which clears the port file) was never reached. What the
    // user saw was an app whose every request was refused while
    // `daemon.port` still advertised the dead port, with no way back short of
    // quitting the app.
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(
            shutdown_rx,
            primary_daemon,
            ownership_lock.clone(),
        ))
        .await
        .expect("server error");

    // Graceful exit (Ctrl-C/SIGTERM or client-presence auto-exit): remove the
    // port file so CLI subcommands and the startup guard don't find a stale
    // (now-dead) daemon. Auxiliary daemons never wrote it — and must not
    // delete the primary daemon's file on their way out.
    if primary_daemon {
        remove_daemon_port_file();
    }
    // #296: revoke the interpreter read-root ACL leases pinned for the life of
    // this process so no grant for a dead AppContainer SID stays on the user's
    // Python install.
    #[cfg(windows)]
    rightkit_agent_tools::coding_tools::sandbox::release_pinned_read_roots();
}

/// Cap on the post-shutdown drain. Long enough for a real request to finish,
/// short enough that a never-ending stream cannot strand the process.
const SHUTDOWN_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Per-session cache of the last observed pre-compaction canonical range,
/// keyed by `(session_id, compaction_seq)`. Owned by the hourly retention
/// task's loop; not persisted across a daemon restart.
type PendingProjections = std::collections::HashMap<
    (String, u32),
    rightkit_agent_engine_core::compaction_archive::CompactionProjectionRecord,
>;

/// CR-16: the real hourly retention job entry point. Both the production
/// scheduler loop above and the tests below call this same function. It is
/// deliberately fail-closed: projection verification without authenticated
/// restoration cannot authorize deletion from the canonical ledger.
///
/// The live compaction write path now builds a projection record at archive
/// time over the exact pre-compaction surface and hands it to the archive
/// sink (`CompactionArchiveInput::projection`); when such a record has been
/// persisted durably (event kind `compactionProjection`, see
/// `rightkit_agent_engine_core::compaction_archive::PROJECTION_EVENT_KIND`) this job
/// consumes it in preference to deriving a projection operationally. The
/// record remains useful for audit/recovery, but it never authorizes deletion
/// until an authenticated restoration path is implemented.
fn run_event_retention_pass(
    store: &rightkit_agent_engine_storage::Storage,
    _event_retention_enabled: bool,
    pending: &mut PendingProjections,
) {
    let _ = store;
    // Hash/range verification proves what existed, but does not restore it.
    // Until restoration is authenticated & durably appended to canonical
    // storage, every retention mode keeps all event rows.
    pending.clear();
}

/// CR-16: prefer a projection record persisted at live-compaction time over
/// an operationally derived one. Records are stored as ordinary append-only
/// event rows (kind `compactionProjection`, i.e.
/// `rightkit_agent_engine_core::compaction_archive::PROJECTION_EVENT_KIND`, payload =
/// the record's JSON) appended strictly AFTER their session's compaction marker, so scanning for them never
/// touches the pre-compaction range under protection. A record qualifies
/// when its source range covers the pre-compaction range about to be
/// considered for deletion; the latest qualifying record wins. When none
/// exists (sinks that do not persist projections, sessions compacted before
/// the record existed), the caller falls back to deriving the projection
/// from a fresh canonical fetch.
fn load_persisted_projection(
    store: &rightkit_agent_engine_storage::Storage,
    session_id: &str,
    compaction_seq: u32,
) -> Option<rightkit_agent_engine_core::compaction_archive::CompactionProjectionRecord> {
    use rightkit_agent_engine_core::compaction_archive::{
        CompactionProjectionRecord, PROJECTION_EVENT_KIND,
    };
    let rows = store.all_events(session_id).ok()?;
    let to_seq = compaction_seq.saturating_sub(1);
    rows.into_iter()
        .filter(|row| row.kind == PROJECTION_EVENT_KIND)
        .filter_map(|row| CompactionProjectionRecord::decode_json(&row.payload_json).ok())
        .rfind(|record| {
            record.session_id == session_id
                && record.source_seq_start == 0
                && record.source_seq_end >= to_seq
        })
}

include!("main_runtime_server_client_presence.rs");

fn init_daemon_telemetry() {
    rightkit_agent_telemetry::init();
}

fn resolve_configured_model_alias(
    configured: &str,
    registry: &rightkit_agent_orchestration::coding::registry::AgentRegistry,
) -> String {
    let trimmed = configured.trim();
    if trimmed.is_empty() {
        String::new()
    } else {
        registry
            .get(trimmed)
            .map(|agent| agent.model_id.clone())
            .unwrap_or_else(|| trimmed.to_string())
    }
}

fn normalized_agent_override_provider_id(provider_id: String) -> String {
    if provider_id.trim().is_empty() {
        "openrouter".to_string()
    } else {
        provider_id
    }
}

/// Write the daemon startup info to `~/.coderight/daemon.port` so CLI
/// subcommands can locate the running daemon without an env var.
///
/// The file carries the daemon bearer token; the default umask would
/// leave it world-readable on Unix, letting any local process read the
/// token and make authenticated API calls. The write goes through
/// [`rightkit_fs::write_private_file`] — atomic temp file,
/// fsync, rename, owner-only chmod / Windows DACL — so a crash mid-
/// write never leaves a half-written `daemon.port` and the previously
/// valid one is preserved intact.
fn write_daemon_port_file(info: &LoopbackRecord) {
    let Some(dir) = rightkit_agent_config::state_root() else {
        eprintln!("[daemon.port] cannot resolve private daemon home");
        std::process::exit(1);
    };
    // ensure_secure_dir creates + hardens. Sweep afterward so the
    // existing siblings (config.toml, workspace-trust.json,
    // permission-denials.json, the memory store) are tightened by the
    // same single source of truth that tightens the database and
    // its WAL sidecars. Fail-closed: if the private dir cannot be
    // created/hardened, the daemon must not start with a world-readable
    // directory — propagate and exit non-zero.
    if let Err(e) = rightkit_fs::ensure_private_dir(&dir) {
        eprintln!(
            "[daemon.port] failed to ensure secure dir {}: {e}",
            dir.display()
        );
        std::process::exit(1);
    }
    harden_data_dir(&dir);
    let path = dir.join("daemon.port");
    // The owner writes the record atomically and owner-only (temp file, fsync, rename).
    if let Err(e) = info.publish(&path) {
        eprintln!("[daemon.port] failed to write private port file: {e}");
        std::process::exit(1);
    }
    let _ = DAEMON_RECORD.set(info.clone());
}

/// The record this process published, so shutdown removes only a `daemon.port` that still holds it.
static DAEMON_RECORD: std::sync::OnceLock<LoopbackRecord> = std::sync::OnceLock::new();

/// Resolve when the process is asked to stop — Ctrl-C, SIGTERM on Unix, or the
/// client-presence reaper deciding the last client has detached — so the
/// server can drain in-flight requests instead of dropping connections.
async fn shutdown_signal(
    mut client_presence_shutdown: tokio::sync::watch::Receiver<bool>,
    primary_daemon: bool,
    _ownership_lock: std::sync::Arc<
        std::sync::Mutex<Option<crate::daemon::ownership::OwnershipLock>>,
    >,
) {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    let client_presence = async {
        if *client_presence_shutdown.borrow() {
            return;
        }
        // `changed()` only resolves on a `true` value in this daemon (the
        // reaper never sends `false`), so waiting for any change is enough.
        let _ = client_presence_shutdown.changed().await;
    };

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
        _ = client_presence => {}
    }
    eprintln!("[serve] shutdown signal received, draining in-flight requests");

    // DL-011 — ordered, single-flight shutdown.
    //
    // Runs HERE, at the moment shutdown is requested, not after the drain
    // completes: the drain watchdog armed below may force-exit the process,
    // and steps queued after the drain would then never run at all.
    //
    // The coordinator is a process-lifetime static so its single-flight
    // guarantee actually holds — a second signal racing an in-flight shutdown
    // observes the first run's report instead of re-running the steps.
    static COORDINATOR: std::sync::OnceLock<crate::daemon::shutdown::ShutdownCoordinator> =
        std::sync::OnceLock::new();

    let journal_path = crate::daemon::recovery::default_journal_path();
    let mut steps: Vec<crate::daemon::shutdown::ShutdownStep> = Vec::new();

    // Step 1 — runtime. Nothing to tear down here: axum's
    // `with_graceful_shutdown` owns closing the listener and draining
    // in-flight requests once this future returns. The step exists so the
    // ordering is explicit and the phase is observable in the log.
    steps.push(crate::daemon::shutdown::ShutdownStep {
        name: "runtime",
        run: Box::new(|| {
            eprintln!("[serve] shutdown step 'runtime': drain requested, listener closing");
            Ok(())
        }),
    });

    if primary_daemon {
        // Step 2 — API: stop advertising the port now rather than after the
        // drain. A client that discovered this daemon during the drain would
        // be routed to a socket that is already refusing connections.
        steps.push(crate::daemon::shutdown::ShutdownStep {
            name: "api",
            run: Box::new(|| {
                remove_daemon_port_file();
                Ok(())
            }),
        });
        // Step 3 — persistence: a clean exit leaves no stale worker records
        // for the next start's DL-013 recovery sweep to consider.
        steps.push(crate::daemon::shutdown::ShutdownStep {
            name: "persistence",
            run: Box::new(move || {
                if let Some(path) = journal_path {
                    crate::daemon::recovery::clear_journal(&path);
                }
                Ok(())
            }),
        });
        // Owner lock remains held through server drain & DB-lock release.
        // `_ownership_lock` keeps its guard alive until this shutdown future
        // returns; `serve` retains its own reference through final cleanup.
    }

    let report = COORDINATOR
        .get_or_init(crate::daemon::shutdown::ShutdownCoordinator::new)
        .run_once(
            steps,
            crate::daemon::shutdown::DEFAULT_SHUTDOWN_EXIT_GRACE,
            || {
                // Hard deadline is forced-kill backstop. Keep owner lease held
                // until process termination so a wedged daemon cannot leave
                // descendants running behind a released lock.
                eprintln!(
                    "[serve] shutdown steps exceeded {}s; forcing process termination (owner lease held)",
                    crate::daemon::shutdown::DEFAULT_SHUTDOWN_EXIT_GRACE.as_secs()
                );
                std::process::exit(1);
            },
        );
    for (name, error) in &report.failures {
        eprintln!("[serve] shutdown step '{name}' failed: {error}");
    }

    // Arm the drain watchdog HERE — once shutdown is actually requested.
    //
    // `with_graceful_shutdown` waits for in-flight requests to finish, and an
    // SSE stream never finishes: a subscribed UI holds one open for as long as
    // its window is open. Without a bound the process parks forever with its
    // listener already closed — alive, unreachable, and still advertising its
    // port. Every request the app makes is refused, with no recovery.
    //
    // Bounding the SERVER future instead of the drain is the obvious-looking
    // mistake and a much worse bug: `timeout(D, serve.with_graceful_shutdown())`
    // starts counting at startup, so the daemon exits D seconds after booting,
    // every time, whether or not anyone asked it to stop. That shipped for one
    // build and killed the engine ten seconds into every launch.
    tokio::spawn(async move {
        tokio::time::sleep(SHUTDOWN_DRAIN_TIMEOUT).await;
        eprintln!(
            "[serve] drain exceeded {}s (long-lived streams still open); forcing process termination (owner lease held)",
            SHUTDOWN_DRAIN_TIMEOUT.as_secs()
        );
        // Same ownership rule as the normal exit path: an auxiliary daemon
        // must never delete the primary's file on its way out.
        if primary_daemon {
            remove_daemon_port_file();
        }
        std::process::exit(1);
    });
}
