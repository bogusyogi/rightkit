//! Agent runtime telemetry for an agent host: resource metrics, turn latency, provider round trips,
//! SQLite op latency, wakeups, prompt overhead, context ceiling, evidence, repair counters and the
//! resource-report contract. See README.md.

#![deny(unsafe_code)]

pub mod agent_observability;
pub mod layer;
pub mod measurement;
pub mod prompt_overhead;
pub mod repair;
pub mod resource;
pub mod resource_report;

/// R12 CR-38/39/40/41 baseline instrument — S0/S1/S10/S50 table.
///
/// This is the *instrument*, not the numbers. No cell of the baseline table is
/// populated by anything in this crate; every cell starts as `None` and renders
/// as "not measured" until a real daemon run observes it. Do not interpolate,
/// estimate, or transcribe a number into the table that was not observed.
///
/// Scenario semantics (from `docs/pending/performance.md`, plan unit P2-1):
/// - **S0** — idle daemon, zero clients.
/// - **S1** — one active session.
/// - **S10** — ten concurrent sessions.
/// - **S50** — fifty concurrent sessions.
///
/// Units, fixed by column:
/// - `p50_ms` / `p95_ms` — turn wall-clock latency, milliseconds (nearest-rank
///   percentiles over the bounded recent-sample window).
/// - `round_trips` — total provider round-trips observed since sink creation
///   (per-task breakdown: [`crate::resource::round_trip_aggregates`] and the
///   `/runtime/resource-metrics` endpoint).
/// - `wakeups_per_s` — daemon wakeups per second over the observation window.
/// - `prompt_overhead_est_tokens` — mean estimated tokens (bytes / 4,
///   conservative — see [`crate::prompt_overhead::estimate_tokens`]) of base
///   system prompt + tool prelude per provider request.
/// - `rss_bytes` — daemon resident set size, bytes. Live gauge: only readable
///   inside the daemon process; always `None` from this crate.
/// - `open_sse_streams` — live SSE subscriber count. Live gauge: same rule.
///
/// How the table gets filled (the real measurement path — none of it runs
/// here): build and run the daemon, drive each scenario's concurrency level,
/// read `GET /runtime/resource-metrics` (which surfaces the same counters this
/// crate records) at each level, and transcribe the observed numbers into
/// `docs/pending/performance.md`, stating the hardware and conditions alongside. The
/// plan pins the reference host: the 8 GB Air. Until a run happens, every cell
/// is "not measured" and the P0-E transport-reuse decision stays open.
///
/// Verification of this instrument (no numbers produced):
/// `rustright cargo test --manifest-path engine/Cargo.toml -p host_telemetry`
/// exercises snapshot, render, and the recorder wiring end-to-end.
pub mod baseline {
    use crate::prompt_overhead::prompt_overhead_aggregates;
    use crate::resource::{round_trip_aggregates, turn_latency_percentiles, wakeup_summary};

    /// One row of the S0/S1/S10/S50 baseline table. Every metric cell starts as
    /// `None` = "not measured"; only an observed run turns it into `Some`.
    #[derive(Debug, Clone, PartialEq, Default)]
    pub struct BaselineRow {
        pub scenario: &'static str,
        pub p50_ms: Option<u64>,
        pub p95_ms: Option<u64>,
        /// Total provider round-trips observed (see module units doc).
        pub round_trips: Option<u64>,
        pub wakeups_per_s: Option<f64>,
        pub prompt_overhead_est_tokens: Option<u64>,
        /// Live gauge; `None` unless filled by the daemon process itself.
        pub rss_bytes: Option<u64>,
        /// Live gauge; `None` unless filled by the daemon process itself.
        pub open_sse_streams: Option<u64>,
    }

    impl BaselineRow {
        /// True only when at least one metric cell was actually observed.
        pub fn any_cell_measured(&self) -> bool {
            self.p50_ms.is_some()
                || self.p95_ms.is_some()
                || self.round_trips.is_some()
                || self.wakeups_per_s.is_some()
                || self.prompt_overhead_est_tokens.is_some()
                || self.rss_bytes.is_some()
                || self.open_sse_streams.is_some()
        }
    }

    /// The four plan scenarios, every cell "not measured".
    pub fn empty_table() -> Vec<BaselineRow> {
        vec![
            BaselineRow {
                scenario: "S0",
                ..Default::default()
            },
            BaselineRow {
                scenario: "S1",
                ..Default::default()
            },
            BaselineRow {
                scenario: "S10",
                ..Default::default()
            },
            BaselineRow {
                scenario: "S50",
                ..Default::default()
            },
        ]
    }

    /// Snapshot one scenario row from whatever the process-global telemetry
    /// sinks currently hold. Inside the daemon after real traffic this yields
    /// observed numbers; in a fresh process every cell is `None` ("not
    /// measured"). The two live gauges (`rss_bytes`, `open_sse_streams`) are
    /// always `None` here — they are readable only by the daemon itself.
    pub fn snapshot_row(scenario: &'static str) -> BaselineRow {
        let latency = turn_latency_percentiles();
        let wakeups = wakeup_summary();
        let round_trips_total: u64 = round_trip_aggregates()
            .into_iter()
            .map(|(_, count)| count)
            .sum();
        let mean_overhead = mean_overhead_tokens(&prompt_overhead_aggregates());

        BaselineRow {
            scenario,
            p50_ms: latency.map(|l| l.p50_ms),
            p95_ms: latency.map(|l| l.p95_ms),
            round_trips: (round_trips_total > 0).then_some(round_trips_total),
            wakeups_per_s: wakeups.per_second,
            prompt_overhead_est_tokens: mean_overhead,
            rss_bytes: None,
            open_sse_streams: None,
        }
    }

    /// Mean fixed overhead per provider request across all observed
    /// (provider, model) rows: summed estimated tokens over summed
    /// observations. `None` until at least one request was recorded.
    pub(crate) fn mean_overhead_tokens(
        rows: &[crate::prompt_overhead::PromptOverheadRow],
    ) -> Option<u64> {
        let total_tokens: u64 = rows
            .iter()
            .map(|(_, agg)| agg.total_system_prompt_est_tokens + agg.total_tool_prelude_est_tokens)
            .sum();
        let total_observations: u64 = rows.iter().map(|(_, agg)| agg.observations).sum();
        (total_observations > 0).then(|| total_tokens / total_observations)
    }

    /// Snapshot a row per scenario label, in order.
    pub fn snapshot_table(scenarios: &[&'static str]) -> Vec<BaselineRow> {
        scenarios.iter().map(|s| snapshot_row(s)).collect()
    }

    /// Render rows as the baseline Markdown table. Unobserved cells render as
    /// exactly `not measured` — never zero, never blank, never a guess.
    /// Hardware and measurement conditions are deliberately not a column here:
    /// they must be stated in prose wherever a populated table is published.
    pub fn render_markdown(rows: &[BaselineRow]) -> String {
        let mut out = String::from(
            "| scenario | p50_ms | p95_ms | round_trips | wakeups/s | \
             prompt_overhead_est_tokens | rss_bytes | open_sse_streams |\n\
             |---|---|---|---|---|---|---|---|\n",
        );
        for r in rows {
            let fmt_u = |v: Option<u64>| {
                v.map(|x| x.to_string())
                    .unwrap_or_else(|| "not measured".into())
            };
            let fmt_f = |v: Option<f64>| {
                v.map(|x| format!("{x:.2}"))
                    .unwrap_or_else(|| "not measured".into())
            };
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} | {} |\n",
                r.scenario,
                fmt_u(r.p50_ms),
                fmt_u(r.p95_ms),
                fmt_u(r.round_trips),
                fmt_f(r.wakeups_per_s),
                fmt_u(r.prompt_overhead_est_tokens),
                fmt_u(r.rss_bytes),
                fmt_u(r.open_sse_streams),
            ));
        }
        out
    }

    /// Record a bounded burst of *synthetic* reference events through the real
    /// recorders — an instrument self-check that proves the recording path is
    /// wired end-to-end (recorder → sink → snapshot). This is calibration, not
    /// measurement: the events are fabricated, so anything they write must
    /// never be read as a baseline number.
    ///
    /// Never call this in the daemon process: the sinks are process-global and
    /// synthetic events would contaminate real measurements. Pair with
    /// [`crate::resource::reset_resource_telemetry`] before and after.
    ///
    /// - `turns` synthetic turn-latency samples of 1..=`turns` ms
    /// - `tasks` distinct synthetic task identities, one round-trip each
    /// - `wakeups` synthetic daemon wakeups
    pub fn record_synthetic_reference_load(turns: u64, tasks: u64, wakeups: u64) {
        use crate::resource::{record_round_trip, record_turn_latency, record_wakeup};
        for ms in 1..=turns {
            record_turn_latency(crate::resource::TurnLatencyEvent { latency_ms: ms });
        }
        for t in 1..=tasks {
            record_round_trip(crate::resource::RoundTripEvent {
                task: format!("synthetic-reference-{t}"),
            });
        }
        for _ in 0..wakeups {
            record_wakeup();
        }
    }
}

pub mod context_ceiling;
pub mod evidence;

pub use context_ceiling::{
    now_unix_ms, produce_remaining_context_ceiling, record_remaining_context_ceiling,
    remaining_context_ceilings, reset_remaining_context_telemetry, EstimatorBasis,
    HostObservationProvenance, ObservationCoverage as ContextObservationCoverage,
    ObservationUnavailableReason as ContextObservationUnavailableReason,
    ObservedField as ContextObservedField, RemainingContextCeilingV1, RemainingContextInput,
    TokenEstimate as ContextTokenEstimate, H8_ESTIMATOR_BASIS_ID, H8_ESTIMATOR_BASIS_VERSION,
    REMAINING_CONTEXT_CEILING_SCHEMA, REMAINING_CONTEXT_CEILING_SCHEMA_VERSION,
};
pub use evidence::{
    evaluation_outcomes, procedural_asset_observations, record_evaluation_outcome,
    record_procedural_asset_observation, record_tool_schema_observation, reset_evidence_telemetry,
    tool_schema_observations, AssetProvenance, CountObservationV1, EvaluationOutcomeV1,
    EvaluationVerdict, ObservationCoverage, ProceduralAssetKind, ProceduralAssetObservationV1,
    ProvenanceV1, TokenEstimateClass, TokenEstimateV1, ToolSchemaObservationV1, UnavailableFieldV1,
    UnavailableReason,
};
pub use layer::init;
pub use measurement::{
    HostGauges, MeasurementError, MeasurementObservation, MeasurementReceipt, MeasurementScenario,
    ResourceMeasurement, MEASUREMENT_SCHEMA,
};
pub use prompt_overhead::{
    cache_prefix_events, estimate_tokens, prompt_overhead_aggregates, prompt_overhead_dropped_rows,
    record_cache_prefix_event, record_prompt_overhead, reset_cache_prefix_telemetry,
    reset_prompt_overhead_telemetry, CachePrefixEvent, PromptOverheadAggregate,
    PromptOverheadEvent,
};
pub use repair::{
    record_repair_event, repair_counters, reset_repair_telemetry, validator_error_kind,
    RepairCounter, RepairEvent,
};
pub use resource::{
    event_loop_delay_summary, latency_raw_samples, record_event_loop_delay, record_round_trip,
    record_sqlite_op, record_turn_latency, record_wakeup, reset_resource_telemetry,
    resource_dropped_rows, round_trip_aggregates, sqlite_latency_summary, turn_latency_percentiles,
    wakeup_summary, EventLoopDelaySummary, RoundTripEvent, SqliteLatencySummary, SqliteOpKind,
    TurnLatencyEvent, TurnLatencyPercentiles, WakeupSummary,
};
