//! Repair telemetry (master-change row 5.3): local-first, content-free
//! accounting of the deterministic tool-argument repair stage.
//!
//! Dimensions: provider, model, tool, validator error kind, repair kind, and
//! whether the repaired call then validated (success). No argument bodies,
//! paths, or model outputs are ever recorded — only these label dimensions
//! and counts.
//!
//! Attribution follows the code's existing boundary: the engine repair seam
//! knows tool / repair kind / validator error / success, while provider and
//! model are injected by the daemon adapter that owns the session route.

use std::collections::BTreeMap;
use std::sync::Mutex;

/// Cap on distinct dimension rows kept in memory. Repair kinds and validator
/// error kinds are bounded enums in practice; the cap exists so a
/// misbehaving caller cannot grow the sink unbounded.
const MAX_DISTINCT_ROWS: usize = 512;

/// One recorded repair-stage outcome.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RepairEvent {
    /// Provider id (e.g. `openrouter`) when the adapter attributed it.
    pub provider: Option<String>,
    /// Model id when the adapter attributed it.
    pub model: Option<String>,
    /// Tool name the call targeted.
    pub tool: Option<String>,
    /// Deterministic repair kind applied (e.g. `singleton_to_array`),
    /// or `None` for a validation failure with no repair.
    pub repair_kind: Option<String>,
    /// Bounded validator-error kind (e.g. `not_valid_json`,
    /// `type_mismatch`, `missing_required`), or `None` on success.
    pub validator_error: Option<String>,
    /// Whether the (repaired) arguments validated and could execute.
    pub success: bool,
}

/// Aggregated counter row keyed by the full dimension tuple.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RepairCounter {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub tool: Option<String>,
    pub repair_kind: Option<String>,
    pub validator_error: Option<String>,
    pub success: bool,
    pub count: u64,
}

#[derive(Default)]
struct RepairSink {
    counters: BTreeMap<RepairEvent, u64>,
    dropped_rows: u64,
}

static SINK: Mutex<Option<RepairSink>> = Mutex::new(None);

fn with_sink<R>(f: impl FnOnce(&mut RepairSink) -> R) -> R {
    let mut guard = SINK.lock().expect("repair telemetry sink lock poisoned");
    let sink = guard.get_or_insert_with(RepairSink::default);
    f(sink)
}

/// Record one repair-stage outcome. Local `tracing` emission uses only
/// metadata keys (`provider`, `model`, `error_kind`, `operation`).
pub fn record_repair_event(event: RepairEvent) {
    tracing::info!(
        operation = "tool_argument_repair",
        provider = event.provider.as_deref().unwrap_or("unknown"),
        model = event.model.as_deref().unwrap_or("unknown"),
        error_kind = event
            .validator_error
            .as_deref()
            .unwrap_or(if event.success { "none" } else { "unspecified" }),
        repair_kind = event.repair_kind.as_deref().unwrap_or("none"),
        tool = event.tool.as_deref().unwrap_or("unknown"),
        success = event.success,
        "tool argument repair stage outcome"
    );
    with_sink(|sink| {
        if !sink.counters.contains_key(&event) && sink.counters.len() >= MAX_DISTINCT_ROWS {
            sink.dropped_rows += 1;
            return;
        }
        *sink.counters.entry(event).or_default() += 1;
    });
}

/// Snapshot the aggregated counters, ordered by dimension tuple.
pub fn repair_counters() -> Vec<RepairCounter> {
    with_sink(|sink| {
        sink.counters
            .iter()
            .map(|(event, count)| RepairCounter {
                provider: event.provider.clone(),
                model: event.model.clone(),
                tool: event.tool.clone(),
                repair_kind: event.repair_kind.clone(),
                validator_error: event.validator_error.clone(),
                success: event.success,
                count: *count,
            })
            .collect()
    })
}

/// Rows dropped because the distinct-dimension cap was reached.
pub fn repair_dropped_rows() -> u64 {
    with_sink(|sink| sink.dropped_rows)
}

/// Reset the sink. Intended for tests and diagnostics endpoints.
pub fn reset_repair_telemetry() {
    with_sink(|sink| {
        sink.counters.clear();
        sink.dropped_rows = 0;
    });
}

/// Classify a validator error message into a bounded, content-free kind.
/// Repair telemetry never records the raw message (it embeds schema text and
/// offending values); only the stable kind prefix is kept.
pub fn validator_error_kind(message: &str) -> String {
    let first = message.lines().next().unwrap_or_default();
    if first.contains("not valid JSON") {
        "not_valid_json".to_string()
    } else if first.contains("must be ") {
        "type_mismatch".to_string()
    } else if first.contains("missing required") || first.contains("Missing required") {
        "missing_required".to_string()
    } else if first.contains("Invalid tool arguments") {
        "invalid_arguments".to_string()
    } else {
        "other".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sink is process-global; tests that reset and assert on it must not
    /// run concurrently with each other.
    #[test]
    fn counters_aggregate_identical_dimensions() {
        let _guard = crate::agent_telemetry::TELEMETRY_TEST_LOCK.lock().unwrap();
        reset_repair_telemetry();
        let event = RepairEvent {
            provider: Some("openrouter".into()),
            model: Some("test/model".into()),
            tool: Some("write_file".into()),
            repair_kind: Some("singleton_to_array".into()),
            validator_error: None,
            success: true,
        };
        record_repair_event(event.clone());
        record_repair_event(event.clone());
        let counters = repair_counters();
        assert_eq!(counters.len(), 1);
        assert_eq!(counters[0].count, 2);
        assert!(counters[0].success);
        assert_eq!(
            counters[0].repair_kind.as_deref(),
            Some("singleton_to_array")
        );
        reset_repair_telemetry();
    }

    #[test]
    fn distinct_dimensions_stay_distinct() {
        let _guard = crate::agent_telemetry::TELEMETRY_TEST_LOCK.lock().unwrap();
        reset_repair_telemetry();
        record_repair_event(RepairEvent {
            provider: None,
            model: None,
            tool: Some("read_file".into()),
            repair_kind: None,
            validator_error: Some("not_valid_json".into()),
            success: false,
        });
        record_repair_event(RepairEvent {
            provider: None,
            model: None,
            tool: Some("read_file".into()),
            repair_kind: Some("string_to_boolean".into()),
            validator_error: None,
            success: true,
        });
        let counters = repair_counters();
        assert_eq!(counters.len(), 2);
        assert!(counters
            .iter()
            .any(|row| !row.success && row.validator_error.as_deref() == Some("not_valid_json")));
        assert!(counters
            .iter()
            .any(|row| row.success && row.repair_kind.as_deref() == Some("string_to_boolean")));
        reset_repair_telemetry();
    }

    #[test]
    fn sink_caps_distinct_rows() {
        let _guard = crate::agent_telemetry::TELEMETRY_TEST_LOCK.lock().unwrap();
        reset_repair_telemetry();
        for index in 0..(MAX_DISTINCT_ROWS + 8) {
            record_repair_event(RepairEvent {
                provider: Some(format!("p{index}")),
                model: None,
                tool: Some("t".into()),
                repair_kind: None,
                validator_error: None,
                success: true,
            });
        }
        assert_eq!(repair_counters().len(), MAX_DISTINCT_ROWS);
        assert_eq!(repair_dropped_rows(), 8);
        reset_repair_telemetry();
    }

    #[test]
    fn validator_error_kinds_are_bounded_labels() {
        assert_eq!(
            validator_error_kind("Invalid tool arguments: not valid JSON (x)."),
            "not_valid_json"
        );
        assert_eq!(
            validator_error_kind("$.path must be string"),
            "type_mismatch"
        );
        assert_eq!(
            validator_error_kind("missing required property `path`"),
            "missing_required"
        );
        assert_eq!(validator_error_kind("something novel"), "other");
    }
}
