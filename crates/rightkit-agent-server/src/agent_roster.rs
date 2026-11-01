//! Per-sub-agent roster accumulator for the session agent rail (F1).
//!
//! Lives on the **parent** `LiveSession`. The parent's `sub.recv()` loop feeds
//! each child's `UsageUpdated`/`ToolCallStarted`/completion events here, and the
//! daemon publishes an `AgentRosterUpdated` snapshot + derives a
//! `SessionUsageRollup` (the F1 → F2 budget contract) from it.

use rightkit_agent_engine_protocol::{AgentRosterRow, SessionUsageRollup};
use std::collections::HashMap;

#[derive(Default)]
pub struct AgentRoster {
    rows: HashMap<String, AgentRosterRow>,
    order: Vec<String>,
    started_ms: HashMap<String, u128>,
    last_publish_ms: Option<u128>,
    last_status_fingerprint: Option<String>,
}

/// Minimum gap between roster broadcasts that carry only metric churn.
/// Token counters tick on every usage update, so an unthrottled roster made up
/// 123 of the 126 events in a single-agent run — inflating the event log and
/// the SSE stream for changes a human cannot perceive at that rate.
const ROSTER_METRIC_PUBLISH_INTERVAL_MS: u128 = 1_000;

fn now_ms() -> u128 {
    crate::api::now_millis()
}

impl AgentRoster {
    pub fn ensure(&mut self, sub_session_id: &str, role: &str, model: &str) {
        if !self.rows.contains_key(sub_session_id) {
            self.order.push(sub_session_id.to_string());
            self.started_ms.insert(sub_session_id.to_string(), now_ms());
            self.rows.insert(
                sub_session_id.to_string(),
                AgentRosterRow {
                    sub_session_id: sub_session_id.to_string(),
                    role: role.to_string(),
                    status: "running".into(),
                    model: model.to_string(),
                    tokens_in: 0,
                    tokens_out: 0,
                    cache_read: 0,
                    tool_uses: 0,
                    elapsed_ms: 0,
                    last_tools: vec![],
                },
            );
        }
    }

    fn touch_elapsed(&mut self, id: &str) {
        let started = self.started_ms.get(id).copied().unwrap_or_else(now_ms);
        if let Some(r) = self.rows.get_mut(id) {
            r.elapsed_ms = now_ms().saturating_sub(started) as u64;
        }
    }

    /// `UsageUpdated` carries cumulative totals, so set absolute (don't add).
    pub fn record_usage(&mut self, id: &str, tokens_in: u64, tokens_out: u64, cache_read: u64) {
        if let Some(r) = self.rows.get_mut(id) {
            r.tokens_in = tokens_in;
            r.tokens_out = tokens_out;
            r.cache_read = cache_read;
        }
        self.touch_elapsed(id);
    }

    pub fn record_tool(&mut self, id: &str, tool: &str) {
        if let Some(r) = self.rows.get_mut(id) {
            r.tool_uses += 1;
            r.last_tools.insert(0, tool.to_string());
            r.last_tools.truncate(3);
        }
        self.touch_elapsed(id);
    }

    pub fn set_status(&mut self, id: &str, status: &str) {
        self.touch_elapsed(id);
        if let Some(r) = self.rows.get_mut(id) {
            r.status = status.to_string();
        }
    }

    /// Whether the caller should broadcast the roster now.
    ///
    /// Anything a human reads as an event — an agent appearing, finishing,
    /// failing, or picking up a new tool — publishes immediately. Pure counter
    /// movement is throttled, because it changes many times a second and the
    /// rail renders the same way either way.
    pub fn should_publish(&mut self) -> bool {
        let fingerprint = self
            .order
            .iter()
            .filter_map(|id| self.rows.get(id))
            .map(|r| {
                format!(
                    "{}:{}:{}:{}",
                    r.sub_session_id,
                    r.status,
                    r.tool_uses,
                    r.last_tools.first().map(String::as_str).unwrap_or("")
                )
            })
            .collect::<Vec<_>>()
            .join("|");

        let now = now_ms();
        let structural_change =
            self.last_status_fingerprint.as_deref() != Some(fingerprint.as_str());
        let interval_elapsed = self
            .last_publish_ms
            .is_none_or(|last| now.saturating_sub(last) >= ROSTER_METRIC_PUBLISH_INTERVAL_MS);

        if structural_change || interval_elapsed {
            self.last_status_fingerprint = Some(fingerprint);
            self.last_publish_ms = Some(now);
            true
        } else {
            false
        }
    }

    pub fn rows(&self) -> Vec<AgentRosterRow> {
        self.order
            .iter()
            .filter_map(|id| self.rows.get(id).cloned())
            .collect()
    }

    pub fn rollup(&self, session_id: &str, now: &str) -> SessionUsageRollup {
        SessionUsageRollup::from_rows(session_id, &self.rows(), now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roster_tracks_two_agents_with_tools_and_tokens() {
        let mut r = AgentRoster::default();
        r.ensure("w1", "reviewer", "test/model");
        r.ensure("w2", "skeptic", "test/model");
        r.record_usage("w1", 100, 20, 0);
        r.record_tool("w1", "read_file");
        r.record_tool("w1", "grep");
        r.set_status("w1", "done");
        let rows = r.rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].tool_uses, 2);
        assert_eq!(
            rows[0].last_tools,
            vec!["grep".to_string(), "read_file".to_string()]
        );
        assert_eq!(rows[0].status, "done");
        assert_eq!(r.rollup("s1", "t").tokens_in, 100);
    }

    #[test]
    fn roster_publishes_on_change_and_throttles_pure_metric_churn() {
        let mut r = AgentRoster::default();
        r.ensure("w1", "auditor", "m");
        // A new agent is a structural change — always broadcast.
        assert!(r.should_publish());
        // Immediately after, with nothing changed, stay quiet.
        assert!(!r.should_publish());
        // Token counters moving is not worth an event on its own.
        r.record_usage("w1", 500, 100, 0);
        assert!(!r.should_publish());
        // A new tool call is user-visible — broadcast at once.
        r.record_tool("w1", "read_file");
        assert!(r.should_publish());
        // So is finishing.
        r.set_status("w1", "done");
        assert!(r.should_publish());
    }

    #[test]
    fn last_tools_capped_at_three_most_recent_first() {
        let mut r = AgentRoster::default();
        r.ensure("w", "x", "test/model");
        for t in ["a", "b", "c", "d"] {
            r.record_tool("w", t);
        }
        assert_eq!(
            r.rows()[0].last_tools,
            vec!["d", "c", "b"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>()
        );
    }
}
