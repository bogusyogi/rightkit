/// `GET /runtime/resource-metrics` — daemon resource counters with a stable
/// JSON shape (`{app}.resource-metrics.v1`). Deliberately distinct from
/// `/runtime/metrics`, which returns learning-domain JSON and keeps its own
/// consumers.
///
/// Recorded metrics (turn latency, round trips, wakeups) come from
/// `rightkit_agent_telemetry::resource`; live gauges (RSS, streams, inference
/// load, pool sizes, child agents) are read at request time. H4/H6/H8/H9/H10 evidence
/// is exposed additively under `membraneEvidence` from bounded host sinks.
///
/// This file is `include!`d into `api_runtime::implementation`, so it shares
/// that module's imports and must not restate them: `State`, `Json`,
/// `lock_recover`, `now_iso`, and `DaemonState` all arrive from
/// `routes_and_types.rs`, which is included into the same module. Adding local
/// `use` statements here is a name collision, not a safety net.
#[derive(Deserialize)]
struct ResourceMetricsQuery {
    #[serde(default)]
    window: Option<String>,
}

/// OPS-003 sampling cadence for the background resource series.
const RESOURCE_SAMPLE_INTERVAL_SECS: u64 = 60;
/// 512 rows covers the 8h soak window at the 60s cadence (480 rows) with slack
/// for on-request samples. Older rows are evicted oldest-first.
const RESOURCE_SAMPLE_CAPACITY: usize = 512;
const RESOURCE_SOAK_WINDOW_SECS: u64 = 8 * 60 * 60;
const RESOURCE_DEFAULT_WINDOW_SECS: u64 = 60 * 60;

/// One real point in the OPS-003 time series. Each field is read from the
/// live host or from a bounded engine sink at sample time.
struct ResourceSampleRow {
    captured_at: String,
    captured: std::time::Instant,
    rss_bytes: Option<u64>,
    threads: Option<u32>,
    child_count: Option<usize>,
    handles: Option<u32>,
    queue_depth: usize,
    sqlite_latency_ms: Option<f64>,
}

static RESOURCE_SAMPLES: std::sync::Mutex<std::collections::VecDeque<ResourceSampleRow>> =
    std::sync::Mutex::new(std::collections::VecDeque::new());

/// Parse a window such as `90s`, `30m`, or `8h`. Zero and unitless values are
/// rejected so a malformed window cannot silently widen or collapse the query.
fn parse_resource_window_secs(raw: &str) -> Option<u64> {
    let split = raw.find(|c: char| !c.is_ascii_digit())?;
    let (digits, unit) = raw.split_at(split);
    let amount: u64 = digits.parse().ok()?;
    let multiplier = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        _ => return None,
    };
    let secs = amount.checked_mul(multiplier)?;
    (secs > 0).then_some(secs)
}

/// Capture one sample from the live host and append it to the bounded ring.
pub(crate) async fn capture_resource_sample(state: &DaemonState) {
    let queue_depth = state.inference_scheduler.snapshot().await.queued_agents;
    let child_count = enumerate_child_pids().map(|children| children.len());
    let (threads, handles) = self_threads_and_handles();
    let sqlite_latency_ms = [
        rightkit_agent_telemetry::SqliteOpKind::AppendEvent,
        rightkit_agent_telemetry::SqliteOpKind::ReadEvents,
    ]
    .into_iter()
    .filter_map(rightkit_agent_telemetry::sqlite_latency_summary)
    .map(|summary| summary.p95_us)
    .max()
    .map(|p95_us| p95_us as f64 / 1000.0);
    // A fresh process has no storage traffic yet; time one real event read
    // (the replay query path) so every sample carries a measured latency.
    let sqlite_latency_ms = match (sqlite_latency_ms, state.storage.clone()) {
        (Some(latency), _) => Some(latency),
        (None, Some(storage)) => tokio::task::spawn_blocking(move || {
            let store = storage.blocking_lock();
            let started = std::time::Instant::now();
            store.events_after("__resource_probe__", 0).ok()?;
            let elapsed = started.elapsed();
            rightkit_agent_telemetry::record_sqlite_op(
                rightkit_agent_telemetry::SqliteOpKind::ReadEvents,
                elapsed,
            );
            Some(elapsed.as_secs_f64() * 1000.0)
        })
        .await
        .ok()
        .flatten(),
        (None, None) => None,
    };
    let row = ResourceSampleRow {
        captured_at: now_iso(),
        captured: std::time::Instant::now(),
        rss_bytes: daemon_rss_bytes(),
        threads,
        child_count,
        handles,
        queue_depth,
        sqlite_latency_ms,
    };
    let mut rows = lock_recover(&RESOURCE_SAMPLES, "resource.samples");
    if rows.len() >= RESOURCE_SAMPLE_CAPACITY {
        rows.pop_front();
    }
    rows.push_back(row);
}

/// Start the periodic OPS-003 sampler. The first tick fires immediately, so
/// the series has a boot-time point before any request arrives.
pub(crate) fn spawn_resource_sampler(state: DaemonState) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(
            RESOURCE_SAMPLE_INTERVAL_SECS,
        ));
        loop {
            ticker.tick().await;
            capture_resource_sample(&state).await;
        }
    });
}

/// Project the retained samples inside `window_secs` (oldest first) and return
/// the real elapsed span they cover.
fn retained_resource_samples(window_secs: u64) -> (Vec<serde_json::Value>, u64) {
    let rows = lock_recover(&RESOURCE_SAMPLES, "resource.samples");
    let cutoff = std::time::Instant::now().checked_sub(std::time::Duration::from_secs(window_secs));
    let selected: Vec<&ResourceSampleRow> = rows
        .iter()
        .filter(|row| cutoff.is_none_or(|cutoff| row.captured >= cutoff))
        .collect();
    let observed_secs = match (selected.first(), selected.last()) {
        (Some(first), Some(last)) => last.captured.duration_since(first.captured).as_secs(),
        _ => 0,
    };
    let samples = selected
        .iter()
        .map(|row| {
            serde_json::json!({
                "capturedAt": row.captured_at,
                "rssBytes": row.rss_bytes,
                "threads": row.threads,
                "childCount": row.child_count,
                "handles": row.handles,
                "queueDepth": row.queue_depth,
                "sqliteLatencyMs": row.sqlite_latency_ms,
            })
        })
        .collect();
    (samples, observed_secs)
}

/// `GET /runtime/resource-metrics?window=8h` — the shared snapshot plus the
/// sampled time series and a soak receipt. `windowSeconds` is the requested
/// evaluation window; `observedSeconds` is the real span the retained samples
/// cover. `soakComplete` is true only when the observed span reaches the
/// 8-hour soak requirement.
async fn get_resource_metrics(
    State(state): State<DaemonState>,
    Query(query): Query<ResourceMetricsQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let window_secs = match query.window.as_deref() {
        None => RESOURCE_DEFAULT_WINDOW_SECS,
        Some(raw) => parse_resource_window_secs(raw).ok_or(StatusCode::BAD_REQUEST)?,
    };
    capture_resource_sample(&state).await;
    let mut body = resource_metrics_json(&state).await;
    let (samples, observed_secs) = retained_resource_samples(window_secs);
    let sample_count = samples.len();
    let soak_complete = observed_secs >= RESOURCE_SOAK_WINDOW_SECS;
    body["samples"] = serde_json::json!(samples);
    body["windowSeconds"] = serde_json::json!(window_secs);
    body["observedSeconds"] = serde_json::json!(observed_secs);
    body["sampleIntervalSeconds"] = serde_json::json!(RESOURCE_SAMPLE_INTERVAL_SECS);
    body["soakReceipt"] = serde_json::json!({
        "schema": crate::host_identity::schema("ops003.soak-receipt.v1"),
        "requestedWindowSeconds": window_secs,
        "requiredSoakSeconds": RESOURCE_SOAK_WINDOW_SECS,
        "observedSeconds": observed_secs,
        "sampleCount": sample_count,
        "soakComplete": soak_complete,
        "note": "samples are real host readings; soakComplete is false until the process has retained at least the required soak span; no post-task reclamation baseline is recorded by this receipt",
    });
    Ok(Json(body))
}

/// Thread and handle counts for this process. `None` where the host cannot
/// report them; a missing figure is never rendered as zero.
#[cfg(target_os = "linux")]
fn self_threads_and_handles() -> (Option<u32>, Option<u32>) {
    let sample = rightkit_agent_telemetry::resource_report::procfs::sample_self();
    (sample.threads, sample.open_descriptors)
}

#[cfg(windows)]
fn self_threads_and_handles() -> (Option<u32>, Option<u32>) {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessHandleCount};

    let self_pid = std::process::id();
    // SAFETY: the snapshot handle is owned here and closed on every path;
    // THREADENTRY32 is zero-initialized with its size set before first use.
    let threads = unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            None
        } else {
            let mut entry: THREADENTRY32 = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
            let mut count = 0u32;
            let mut ok = Thread32First(snapshot, &mut entry) != 0;
            while ok {
                if entry.th32OwnerProcessID == self_pid {
                    count += 1;
                }
                ok = Thread32Next(snapshot, &mut entry) != 0;
            }
            CloseHandle(snapshot);
            Some(count)
        }
    };
    // SAFETY: GetCurrentProcess is a pseudo-handle needing no release; the
    // out-parameter is a live u32 on the stack.
    let handles = unsafe {
        let mut handle_count = 0u32;
        (GetProcessHandleCount(GetCurrentProcess(), &mut handle_count) != 0).then_some(handle_count)
    };
    (threads, handles)
}

#[cfg(not(any(target_os = "linux", windows)))]
fn self_threads_and_handles() -> (Option<u32>, Option<u32>) {
    (None, None)
}

#[derive(Deserialize)]
struct ProcessTopologyQuery {
    #[serde(default = "default_topology_profile")]
    profile: String,
}

fn default_topology_profile() -> String {
    "practical".to_string()
}

/// Sample the live host against a requested qualification profile. The query
/// selects the profile being checked; it does not relabel ServiceHost as an
/// InlineHost or change process policy. Unmeasurable children fail explicitly.
async fn get_process_topology(
    State(state): State<DaemonState>,
    Query(query): Query<ProcessTopologyQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if !matches!(query.profile.as_str(), "strict" | "practical" | "service") {
        return Err(StatusCode::BAD_REQUEST);
    }
    let children = enumerate_child_processes().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let sessions: Vec<_> = lock_recover(&state.sessions, "state.sessions")
        .values()
        .cloned()
        .collect();
    let mut invoked = std::collections::HashSet::new();
    for session in sessions {
        let shells = lock_recover(&session.background_shells, "session.background_shells");
        invoked.extend(shells.values().filter_map(|shell| shell.pid()));
    }
    let mut processes = vec![json!({
        "pid": std::process::id(), "owner": crate::host_identity::app(), "role": "resident",
        "hostMode": "service",
    })];
    let mut host_children = 0usize;
    for (pid, executable) in &children {
        // CREATE_NO_WINDOW still needs Windows' console infrastructure.
        // Retain it in the census, separately from invoked execution trees.
        let console_host = cfg!(windows)
            && executable
                .as_deref()
                .is_some_and(|name| name.eq_ignore_ascii_case("conhost.exe"));
        if !console_host {
            host_children += 1;
        }
        processes.push(json!({
            "pid": pid, "parentPid": std::process::id(), "executable": executable,
            "owner": if console_host { "windows" } else { crate::host_identity::app() },
            "role": if console_host { "console-host" } else { "helper" },
            "invoked": invoked.contains(pid),
        }));
    }
    Ok(Json(json!({
        "schema": crate::host_identity::schema("process-topology.v1"),
        "sampledAt": now_iso(),
        "profile": query.profile,
        "hostMode": "service",
        "processes": processes,
        format!("{app}ChildCount", app = crate::host_identity::app()): host_children,
        "membrane": {"sampled": false, "reason": "separate context authority"},
        "note": "live direct-child census; invoked is backed by the session execution registry; untracked children are not declared authorized",
    })))
}

/// Shared builder for the `{app}.resource-metrics.v1` JSON object. Also
/// embedded into the corpus-bakeoff report as `resourceMetrics`.
pub(crate) async fn resource_metrics_json(state: &DaemonState) -> serde_json::Value {
    let (open_sse_streams, live_sessions, active_child_agents, session_handles) = {
        let sessions = lock_recover(&state.sessions, "state.sessions");
        let mut open_sse_streams = 0usize;
        let mut active_child_agents = 0usize;
        for session in sessions.values() {
            open_sse_streams += session.conduit.subscriber_count();
            if session.parent_session_id.is_some() {
                active_child_agents += 1;
            }
        }
        let handles: Vec<std::sync::Arc<crate::api::LiveSession>> =
            sessions.values().cloned().collect();
        (
            open_sse_streams,
            sessions.len(),
            active_child_agents,
            handles,
        )
    };

    // Cumulative child-agent counts live in each run's budget ledger; lock
    // them per session only after the daemon sessions lock is released.
    let mut cumulative_child_agents = 0u64;
    for session in &session_handles {
        let ledger = session.run_budget.lock().await;
        cumulative_child_agents += u64::from(ledger.cumulative_used());
    }

    // Active/queued inference reuse the scheduler snapshot — the same source
    // `/inference/load` reads — instead of duplicating its counters.
    let load = state.inference_scheduler.snapshot().await;

    let mcp_pool_size = state.mcp_pool.snapshot().len();

    let latency = rightkit_agent_telemetry::turn_latency_percentiles();
    let latency_raw = rightkit_agent_telemetry::latency_raw_samples();
    let wakeups = rightkit_agent_telemetry::wakeup_summary();
    let round_trips = rightkit_agent_telemetry::round_trip_aggregates();
    let round_trips_total: u64 = round_trips.iter().map(|(_, count)| *count).sum();
    let prompt_overhead = rightkit_agent_telemetry::prompt_overhead_aggregates();
    let cache_prefix_events = rightkit_agent_telemetry::cache_prefix_events();
    let tool_schema_observations = rightkit_agent_telemetry::tool_schema_observations();
    let procedural_asset_observations =
        rightkit_agent_telemetry::procedural_asset_observations();
    let evaluation_outcomes = rightkit_agent_telemetry::evaluation_outcomes();
    let provider_turn_evidence =
        rightkit_agent_telemetry::evidence::provider_turn_evidence_ring();
    let live_session_ids: std::collections::HashSet<&str> = session_handles
        .iter()
        .map(|session| session.id.as_str())
        .collect();
    let remaining_context_ceilings = rightkit_agent_telemetry::remaining_context_ceilings()
        .into_iter()
        .filter(|row| live_session_ids.contains(row.session_id.as_str()))
        .collect::<Vec<_>>();
    let cache_prefix_changes = cache_prefix_events
        .iter()
        .filter(|event| event.prefix_changed)
        .count();
    let lsp_bridge_count = rightkit_agent_tools::coding_tools::lsp_tool::bridge_count().await;

    // Context is served by Pulse's `membrane` CLI; RightKit holds no in-process binding.
    // Reports whether the binary is installed without spawning it (use
    // /runtime/context/status for a live read).
    let mut membrane_binding_json = crate::context_status::cli_binding_json();
    if let Some(object) = membrane_binding_json.as_object_mut() {
        object.insert(
            "contextDegraded".into(),
            serde_json::json!(state.context_degraded_reason().is_some()),
        );
        object.insert(
            "degradedReason".into(),
            serde_json::json!(state.context_degraded_reason()),
        );
    }

    serde_json::json!({
        "schemaVersion": crate::host_identity::schema("resource-metrics.v1"),
        "generatedAt": now_iso(),
        "idle": crate::api_clients::idle_resource_metrics(),
        // Recorded counters come from real call sites in the engine turn loop
        // (`engine_loop.rs`): one latency sample per user turn via the
        // `submit_user_message` wrapper, one round trip per completed LLM
        // response attributed to the session. Counters are bounded
        // (`MAX_LATENCY_SAMPLES=1024`, `MAX_DISTINCT_ROWS=512` in
        // `rightkit_agent_telemetry::resource`); excess samples evict oldest-first,
        // excess distinct task rows increment `droppedRows` rather than growing
        // unbounded. Reset/snapshot semantics are explicit: the sink is
        // process-global until `reset_resource_telemetry` clears it or a
        // `snapshot_resource_telemetry` captures a point-in-time view for
        // per-scenario delta attribution (the baseline harness does the latter;
        // it never copies process-global totals to each scenario row).
        "turnLatency": {
            "available": latency.map(|l| l.samples > 0).unwrap_or(false),
            "note": "recorded per user turn by the engine loop; available=false means no turn has completed since process start; bounded window MAX_LATENCY_SAMPLES=1024, evicts oldest-first",
            "samples": latency.map(|l| l.samples).unwrap_or(0),
            "p50Ms": latency.map(|l| l.p50_ms),
            "p95Ms": latency.map(|l| l.p95_ms),
            "rawSamples": latency_raw,
        },
        "roundTripsPerTask": {
            "available": !round_trips.is_empty(),
            "note": "recorded per completed LLM response by the engine loop, attributed to session; failed provider requests are not counted; bounded MAX_DISTINCT_ROWS=512, excess increments droppedRows",
            "tasks": round_trips.len(),
            "total": round_trips_total,
            "rows": round_trips
                .into_iter()
                .map(|(task, count)| serde_json::json!({ "task": task, "roundTrips": count }))
                .collect::<Vec<_>>(),
            "droppedRows": rightkit_agent_telemetry::resource_dropped_rows(),
        },
        "promptOverhead": {
            "available": !prompt_overhead.is_empty(),
            "note": "estimated conservatively as ceiling(bytes / 4); provider/model rows are bounded at 512 and report droppedRows rather than growing without limit",
            "rows": prompt_overhead.into_iter().map(|((provider, model), aggregate)| serde_json::json!({
                "provider": provider,
                "model": model,
                "observations": aggregate.observations,
                "totalSystemPromptEstTokens": aggregate.total_system_prompt_est_tokens,
                "totalToolPreludeEstTokens": aggregate.total_tool_prelude_est_tokens,
                "maxSystemPromptEstTokens": aggregate.max_system_prompt_est_tokens,
                "maxToolPreludeEstTokens": aggregate.max_tool_prelude_est_tokens,
            })).collect::<Vec<_>>(),
            "droppedRows": rightkit_agent_telemetry::prompt_overhead_dropped_rows(),
        },
        "cachePrefix": {
            "available": !cache_prefix_events.is_empty(),
            "retainedEvents": cache_prefix_events.len(),
            "prefixChanges": cache_prefix_changes,
            "note": "bounded local event history; changed prefixes retain an attributable reason in their event record",
        },
        "membraneEvidence": {
            "h4": {
                "toolSchemaObservations": tool_schema_observations,
                "proceduralAssetObservations": procedural_asset_observations,
            },
            "h6": {
                "evaluationOutcomes": evaluation_outcomes,
            },
            "h8": {
                "remainingContextCeilings": remaining_context_ceilings,
            },
            "h9": provider_turn_evidence_json(&provider_turn_evidence, true),
            "h10": provider_turn_evidence_json(&provider_turn_evidence, false),
            "note": "host-owned mechanical observations only; each row carries schema version, provenance, estimator basis, or typed unavailable gaps; provider rows contain bounded identities/digests only, never request content",
        },
        "membraneBinding": membrane_binding_json,
        // OPS-009: resource claims must distinguish incremental RightKit cost
        // from combined RightKit + canonical Membrane steady state. This is
        // the production consumer of that contract - the endpoint reports the
        // RightKit-only figure and, when no Membrane process was observed,
        // reports the typed refusal instead of a whole-system number.
        "ops009": ops009_resource_report(),
        "daemon": {
            // RSS is read live from the OS, so it is always a real figure.
            "rssBytes": daemon_rss_bytes(),
            "wakeups": {
                "available": wakeups.total > 0,
                "note": "recorder defined; daemon wakeup sites (workflow tick loop and client-presence reaper in main_runtime_server.rs) are instrumented via record_wakeup; rate is observation-window derived, total is monotonic until reset",
                "total": wakeups.total,
                "perSecond": wakeups.per_second,
            },
            // OPS-003: SQLite latency measured inside the single storage
            // authority (append_event / all_events); bounded windows.
            "sqliteLatency": {
                "appendEvent": sqlite_latency_json(
                    rightkit_agent_telemetry::SqliteOpKind::AppendEvent,
                ),
                "readEvents": sqlite_latency_json(
                    rightkit_agent_telemetry::SqliteOpKind::ReadEvents,
                ),
                "note": "microseconds; measured inside Storage ops via SqliteOpTimer, every exit path recorded",
            },
            // OPS-003: event-loop scheduling delay sampled on the workflow
            // supervisor's 1s tick (scheduled deadline vs actual poll time).
            "eventLoopDelay": event_loop_delay_json(),
        },
        // OPS-002: live RightKit-owned process topology. Children are
        // enumerated by the platform sampler and labelled RightKitHelper;
        // Membrane is never a RightKit child and is reported separately.
        "processTopology": process_topology_json(),
        "openSseStreams": open_sse_streams,
        "liveSessions": live_sessions,
        "inference": {
            "active": load.active_agents,
            "queued": load.queued_agents,
            "activeLimit": load.active_limit,
        },
        "mcpPoolSize": mcp_pool_size,
        "lspBridges": {
            "available": true,
            "count": lsp_bridge_count,
            "note": "live count from rightkit_agent_tools::coding_tools::lsp_tool::workspace_manager registry (one entry per distinct workspace/language/server)",
        },
        "childAgents": {
            "active": active_child_agents,
            "cumulative": cumulative_child_agents,
        },
    })
}

/// Project one bounded provider-attempt ring into H9 or H10 JSON without
/// exposing prepared request bytes. Both projections retain identical causal,
/// authorization, measurement, physical-attempt, and submission identities.
fn provider_turn_evidence_json(
    ring: &rightkit_agent_telemetry::evidence::ProviderTurnEvidenceRingV1,
    h9: bool,
) -> serde_json::Value {
    let rows = ring
        .rows
        .iter()
        .map(|row| {
            let mut projected = serde_json::json!({
                "schemaVersion": row.schema_version,
                "sessionId": row.session_id,
                "taskId": row.task_id,
                "requestId": row.request_id,
                "generation": row.generation,
                "authorizationId": row.authorization_id,
                "prepared": {
                    "digest": row.prepared_digest,
                    "contentType": row.prepared_content_type,
                    "route": row.prepared_route,
                    "model": row.prepared_model,
                    "serializerGeneration": row.serializer_generation,
                    "toolCatalogGeneration": row.tool_catalog_generation,
                    "modality": row.modality,
                },
                "measuredTokens": row.measured_tokens,
                "physicalAttempt": row.physical_attempt,
                "submissionState": row.submission_state,
                "provenance": row.provenance,
            });
            let object = projected
                .as_object_mut()
                .expect("provider evidence projection is an object");
            if h9 {
                object.insert(
                    "loadedContextIdentities".to_string(),
                    serde_json::to_value(&row.loaded_context_identities)
                        .expect("typed H9 evidence serializes"),
                );
            } else {
                object.insert(
                    "retainedRepresentationDigest".to_string(),
                    serde_json::to_value(&row.retained_representation_digest)
                        .expect("typed H10 evidence serializes"),
                );
            }
            projected
        })
        .collect::<Vec<_>>();

    serde_json::json!({
        "available": !rows.is_empty(),
        "schemaVersion": rightkit_agent_telemetry::evidence::PROVIDER_TURN_EVIDENCE_SCHEMA,
        "capacity": ring.capacity,
        "retainedRows": rows.len(),
        "droppedRows": ring.dropped_rows,
        "rows": rows,
        "note": "bounded oldest-first evidence ring; Complete loaded identities or retained digest is admitted only for submitted attempts",
    })
}

/// OPS-003 SQLite latency projection: `None`/absent stays typed-unavailable
/// rather than rendering a fabricated zero.
fn sqlite_latency_json(kind: rightkit_agent_telemetry::SqliteOpKind) -> serde_json::Value {
    match rightkit_agent_telemetry::sqlite_latency_summary(kind) {
        Some(summary) => serde_json::json!({
            "available": true,
            "samples": summary.samples,
            "p50Us": summary.p50_us,
            "p95Us": summary.p95_us,
            "maxUs": summary.max_us,
        }),
        None => serde_json::json!({
            "available": false,
            "note": "no sample recorded since process start; bounded window MAX_SQLITE_LATENCY_SAMPLES=1024",
        }),
    }
}

/// OPS-003 event-loop stall projection over the bounded delay window.
fn event_loop_delay_json() -> serde_json::Value {
    match rightkit_agent_telemetry::event_loop_delay_summary() {
        Some(summary) => serde_json::json!({
            "available": true,
            "samples": summary.samples,
            "p50Us": summary.p50_us,
            "p95Us": summary.p95_us,
            "maxUs": summary.max_us,
            "note": "microseconds of scheduler delay on the workflow supervisor 1s tick",
        }),
        None => serde_json::json!({
            "available": false,
            "note": "no tick has been scheduled yet; bounded window MAX_EVENT_LOOP_DELAY_SAMPLES=1024",
        }),
    }
}

/// Enumerate the daemon's direct child process ids. `None` means this host
/// cannot enumerate children — reported as typed-unavailable rather than a
/// fabricated zero-child topology.
#[cfg(unix)]
fn enumerate_child_pids() -> Option<Vec<u32>> {
    rightkit_agent_telemetry::resource_report::platform_topology::children_of(
        std::process::id(),
    )
}

/// Windows child enumeration via Toolhelp32. The exe name is carried so the
/// report can label helpers; RSS sampling of a child needs an opened handle
/// and is left to the platform sampler.
#[cfg(windows)]
fn enumerate_child_pids() -> Option<Vec<u32>> {
    enumerate_child_processes().map(|rows| rows.into_iter().map(|(pid, _)| pid).collect())
}

#[cfg(windows)]
fn enumerate_child_processes() -> Option<Vec<(u32, Option<String>)>> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    let self_pid = std::process::id();
    // SAFETY: CreateToolhelp32Snapshot returns a snapshot handle we own and
    // close on every path; PROCESSENTRY32W is zero-initialized with its size
    // field set before first use, per the Toolhelp contract.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut children = Vec::new();
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut ok = Process32FirstW(snapshot, &mut entry) != 0;
        while ok {
            if entry.th32ParentProcessID == self_pid {
                let end = entry
                    .szExeFile
                    .iter()
                    .position(|ch| *ch == 0)
                    .unwrap_or(entry.szExeFile.len());
                let executable = String::from_utf16_lossy(&entry.szExeFile[..end]);
                children.push((entry.th32ProcessID, Some(executable)));
            }
            ok = Process32NextW(snapshot, &mut entry) != 0;
        }
        CloseHandle(snapshot);
        children.sort_unstable_by_key(|(pid, _)| *pid);
        Some(children)
    }
}

#[cfg(not(windows))]
fn enumerate_child_processes() -> Option<Vec<(u32, Option<String>)>> {
    enumerate_child_pids().map(|rows| rows.into_iter().map(|pid| (pid, None)).collect())
}

#[cfg(not(any(unix, windows)))]
fn enumerate_child_pids() -> Option<Vec<u32>> {
    None
}

/// OPS-002/003: enumerate RightKit-owned children into labelled helper
/// observations. Every real child is sampled where the platform supports it;
/// children are helpers regardless of which host binary they are.
fn observe_host_helpers(
    report: &mut rightkit_agent_telemetry::resource_report::ResourceReport,
) -> Vec<u32> {
    use rightkit_agent_telemetry::resource_report::{LabelledProcess, ProcessOwnership};
    let children = enumerate_child_pids().unwrap_or_default();
    for &pid in &children {
        #[cfg(unix)]
        let sample = rightkit_agent_telemetry::resource_report::procfs::sample_pid(pid);
        #[cfg(windows)]
        let sample = sample_child_windows(pid);
        #[cfg(not(any(unix, windows)))]
        let sample = rightkit_agent_telemetry::resource_report::ProcessResourceSample {
            pid: Some(pid),
            ..Default::default()
        };
        report.observe(LabelledProcess {
            ownership: ProcessOwnership::HostHelper,
            name: format!("{app}-helper-pid-{pid}", app = crate::host_identity::app()),
            sample,
        });
    }
    children
}

#[cfg(windows)]
fn sample_child_windows(
    pid: u32,
) -> rightkit_agent_telemetry::resource_report::ProcessResourceSample {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
    };

    // SAFETY: OpenProcess returns an owned handle closed on every path; the
    // counters struct is zero-initialized POD passed with its real size.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ, 0, pid);
        if handle.is_null() {
            return rightkit_agent_telemetry::resource_report::ProcessResourceSample {
                pid: Some(pid),
                ..Default::default()
            };
        }
        let mut counters: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
        counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        let (rss_bytes, commit_bytes) =
            if GetProcessMemoryInfo(handle, &mut counters, counters.cb) != 0 {
                (
                    Some(counters.WorkingSetSize as u64),
                    (counters.PagefileUsage != 0).then_some(counters.PagefileUsage as u64),
                )
            } else {
                (None, None)
            };
        CloseHandle(handle);
        rightkit_agent_telemetry::resource_report::ProcessResourceSample {
            pid: Some(pid),
            rss_bytes,
            commit_bytes,
            ..Default::default()
        }
    }
}

/// OPS-002 topology projection: the allowed RightKit-owned process shape is
/// one primary plus explicitly-allowed helpers. `childrenMeasurable=false`
/// means this host cannot enumerate children — never rendered as zero.
fn process_topology_json() -> serde_json::Value {
    match enumerate_child_pids() {
        Some(children) => serde_json::json!({
            "measurable": true,
            "primaryPid": std::process::id(),
            format!("{app}ChildPids", app = crate::host_identity::app()): children,
            format!("{app}ChildCount", app = crate::host_identity::app()): children.len(),
            "note": "Membrane is separate first-party infrastructure and is never a RightKit child; strict profile requires this count to be zero",
        }),
        None => serde_json::json!({
            "measurable": false,
            "primaryPid": std::process::id(),
            "note": "host cannot enumerate child processes; a zero-child claim is not made",
        }),
    }
}

/// OPS-009 production report.
///
/// Labels this process as the RightKit primary and samples it for real.
/// Membrane runs as separate first-party infrastructure and is not a
/// RightKit child, so no Membrane process is observable from here; the
/// combined claim is therefore REFUSED rather than silently reported as the
/// RightKit-only total. That refusal is the point of the atom.
fn ops009_resource_report() -> serde_json::Value {
    use rightkit_agent_telemetry::resource_report::{
        LabelledProcess, ProcessOwnership, ReclamationPoint, ResourceReport,
    };

    let mut report = ResourceReport::new(ReclamationPoint::DuringTask);
    // /proc exists only on Linux; macOS samples RSS through task_info.
    #[cfg(target_os = "linux")]
    let sample = rightkit_agent_telemetry::resource_report::procfs::sample_self();
    #[cfg(windows)]
    let sample = sample_child_windows(std::process::id());
    #[cfg(not(any(target_os = "linux", windows)))]
    let sample = rightkit_agent_telemetry::resource_report::ProcessResourceSample {
        pid: Some(std::process::id()),
        rss_bytes: daemon_rss_bytes(),
        ..Default::default()
    };
    report.observe(LabelledProcess {
        ownership: ProcessOwnership::HostPrimary,
        name: crate::host_identity::app().to_string(),
        sample,
    });
    // OPS-003 helper residency: real enumerated children join the report as
    // RightKitHelper so incremental cost and the post-task reclamation
    // check have genuine helper observations, not a fabricated zero.
    let _child_pids = observe_host_helpers(&mut report);

    let incremental = report.incremental_host();
    let combined = match report.combined_with_membrane() {
        Ok(totals) => serde_json::json!({
            "available": true,
            "rssBytes": totals.rss_bytes,
            "processCount": totals.process_count,
        }),
        Err(refusal) => serde_json::json!({
            "available": false,
            "refusal": refusal.to_string(),
        }),
    };

    serde_json::json!({
        "schema": crate::host_identity::schema("ops009.resource-report.v1"),
        "incrementalRightKit": {
            "processCount": incremental.process_count,
            "rssBytes": incremental.rss_bytes,
            "pssBytes": incremental.pss_bytes,
            "threads": incremental.threads,
            "openDescriptors": incremental.open_descriptors,
            "helperProcesses": report.host_helper_count(),
        },
        "combinedWithMembrane": combined,
        "note": "incremental RightKit cost only; a combined RightKit + Membrane steady state is reported only when a Membrane process is actually observed",
    })
}

/// Resident set size of the daemon process in bytes, when the platform can
/// report it without a new dependency. `None` elsewhere.
#[cfg(target_os = "linux")]
fn daemon_rss_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status.lines().find_map(|line| {
        let value = line.strip_prefix("VmRSS:")?;
        let kb = value
            .trim()
            .strip_suffix("kB")?
            .trim()
            .parse::<u64>()
            .ok()?;
        Some(kb * 1024)
    })
}

#[cfg(target_os = "macos")]
fn daemon_rss_bytes() -> Option<u64> {
    unsafe {
        let mut info: libc::mach_task_basic_info = std::mem::zeroed();
        let mut count = libc::MACH_TASK_BASIC_INFO_COUNT;
        let result = libc::task_info(
            mach2::traps::mach_task_self(),
            libc::MACH_TASK_BASIC_INFO,
            &mut info as *mut libc::mach_task_basic_info as libc::task_info_t,
            &mut count,
        );
        (result == libc::KERN_SUCCESS).then_some(info.resident_size)
    }
}

#[cfg(target_os = "windows")]
fn daemon_rss_bytes() -> Option<u64> {
    // SAFETY: `GetCurrentProcess` returns a pseudo-handle (no ownership to
    // release); `PROCESS_MEMORY_COUNTERS` is a plain-old-data struct we
    // zero-initialize and pass with its own size, matching the Win32 contract
    // for `GetProcessMemoryInfo`.
    unsafe {
        let mut counters: windows_sys::Win32::System::ProcessStatus::PROCESS_MEMORY_COUNTERS =
            std::mem::zeroed();
        let size = std::mem::size_of_val(&counters) as u32;
        let ok = windows_sys::Win32::System::ProcessStatus::GetProcessMemoryInfo(
            windows_sys::Win32::System::Threading::GetCurrentProcess(),
            &mut counters,
            size,
        );
        (ok != 0).then_some(counters.WorkingSetSize as u64)
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn daemon_rss_bytes() -> Option<u64> {
    None
}
