use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MiddlewarePhase {
    SessionStart,
    PreLlmCall,
    PostLlmCall,
    PreToolCall,
    PostToolCall,
    ApprovalRequested,
    SubagentStart,
    SubagentStop,
    ScheduledJobStart,
    ScheduledJobEnd,
    SessionEnd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MiddlewareSource {
    BuiltIn,
    LocalPlugin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MiddlewareEffect {
    Observe,
    MutateArgs,
    RequireApproval,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MiddlewareDefinition {
    pub id: String,
    pub title: String,
    pub source: MiddlewareSource,
    pub phase: MiddlewarePhase,
    pub effect: MiddlewareEffect,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_toolset: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<MiddlewareCondition>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub mutate_args: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub enabled: bool,
    pub order: u32,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MiddlewareCondition {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argument: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contains: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub equals: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MiddlewareContext {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub toolset: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_backend: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust_level: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum MiddlewareDecision {
    Continue,
    MutateArgs { arguments: Value },
    RequireApproval { reason: String },
    Deny { reason: String },
    EmitObservation { payload: Value },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSpan {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    pub phase: MiddlewarePhase,
    pub status: String,
    pub started_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    pub attributes: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeObservation {
    pub id: String,
    pub span_id: String,
    pub session_id: String,
    pub severity: String,
    pub message: String,
    pub payload: Value,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MiddlewareRegistrySnapshot {
    pub middleware: Vec<MiddlewareDefinition>,
    pub spans: Vec<RuntimeSpan>,
    pub observations: Vec<RuntimeObservation>,
}

/// Retention caps for the runtime middleware store. It is rewritten in full, so
/// these bound both file size and per-write cost.
const MAX_RETAINED_SPANS: usize = 5000;
const MAX_RETAINED_OBSERVATIONS: usize = 5000;
/// The store file is rewritten at most once per second (sessions rule 36):
/// span and observation records arrive many times a second, and each write is
/// the whole file. Enabled-state changes and [`RuntimeMiddlewareRegistry::flush`]
/// write at once.
const MIN_PERSIST_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);
/// The store file never exceeds 1 MiB. The oldest spans and observations are
/// dropped from memory and file until the serialized snapshot fits.
const MAX_STORE_BYTES: usize = 1024 * 1024;
/// A span can carry arbitrarily rich execution detail. The durable execution
/// observation is a compact contract projection, never a second unbounded copy
/// of that detail.
const MAX_EXECUTION_OBSERVATION_ATTRIBUTES_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone)]
pub struct RuntimeMiddlewareRegistry {
    middleware: BTreeMap<String, MiddlewareDefinition>,
    spans: BTreeMap<String, RuntimeSpan>,
    observations: BTreeMap<String, RuntimeObservation>,
    store_path: Option<PathBuf>,
    last_persisted_at: Option<std::time::Instant>,
    dirty: bool,
}

impl Default for RuntimeMiddlewareRegistry {
    fn default() -> Self {
        Self::with_builtin_middleware()
    }
}

impl RuntimeMiddlewareRegistry {
    pub fn with_builtin_middleware() -> Self {
        let mut registry = Self {
            middleware: BTreeMap::new(),
            spans: BTreeMap::new(),
            observations: BTreeMap::new(),
            store_path: None,
            last_persisted_at: None,
            dirty: false,
        };
        for definition in builtin_middleware() {
            registry
                .middleware
                .insert(definition.id.clone(), definition);
        }
        registry
    }

    pub fn with_store_path(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let mut registry = Self::load_from_path(&path);
        registry.store_path = Some(path);
        registry.persist();
        registry
    }

    fn load_from_path(path: &Path) -> Self {
        let mut registry = Self::with_builtin_middleware();
        let Ok(raw) = std::fs::read_to_string(path) else {
            return registry;
        };
        let Ok(snapshot) = serde_json::from_str::<PersistedRuntimeMiddleware>(&raw) else {
            return registry;
        };
        for persisted in snapshot.middleware {
            if let Some(current) = registry.middleware.get_mut(&persisted.id) {
                current.enabled = persisted.enabled;
            } else {
                registry.middleware.insert(persisted.id.clone(), persisted);
            }
        }
        for span in snapshot.spans {
            registry.spans.insert(span.id.clone(), span);
        }
        for observation in snapshot.observations {
            registry
                .observations
                .insert(observation.id.clone(), observation);
        }
        registry
    }

    /// Write the store now, within the size cap.
    fn persist(&mut self) {
        let Some(path) = self.store_path.clone() else {
            return;
        };
        let body = loop {
            let snapshot = PersistedRuntimeMiddleware {
                middleware: self.middleware(),
                spans: self.spans(None),
                observations: self.observations(None),
            };
            let Ok(body) = serde_json::to_string(&snapshot) else {
                return;
            };
            if body.len() <= MAX_STORE_BYTES
                || (self.spans.is_empty() && self.observations.is_empty())
            {
                break body;
            }
            // Over the cap: drop the oldest tenth of each map and measure again.
            let spans = self.spans.len();
            let observations = self.observations.len();
            Self::evict_oldest(&mut self.spans, spans - spans.div_ceil(10), |s| {
                s.started_at.clone()
            });
            Self::evict_oldest(
                &mut self.observations,
                observations - observations.div_ceil(10),
                |o| o.created_at.clone(),
            );
        };
        // Atomic replace (`rightkit-fs`): a crash leaves the previous snapshot, not a torn
        // file.
        let _ = rightkit_fs::atomic_write_with(
            &path,
            rightkit_fs::AtomicOptions {
                create_parents: true,
                ..rightkit_fs::AtomicOptions::default()
            },
            |file| std::io::Write::write_all(file, body.as_bytes()),
        );
        self.last_persisted_at = Some(std::time::Instant::now());
        self.dirty = false;
    }

    /// Write after a span or observation record, at most once per
    /// [`MIN_PERSIST_INTERVAL`]. A record inside the interval stays in memory
    /// and is written by the next record after the interval, or by `flush`.
    fn persist_throttled(&mut self) {
        self.dirty = true;
        let due = self
            .last_persisted_at
            .is_none_or(|at| at.elapsed() >= MIN_PERSIST_INTERVAL);
        if due {
            self.persist();
        }
    }

    /// Write any records still held back by the once-per-second limit.
    pub fn flush(&mut self) {
        if self.dirty {
            self.persist();
        }
    }

    pub fn middleware(&self) -> Vec<MiddlewareDefinition> {
        let mut rows = self.middleware.values().cloned().collect::<Vec<_>>();
        rows.sort_by(|a, b| {
            a.order
                .cmp(&b.order)
                .then_with(|| a.phase.cmp(&b.phase))
                .then_with(|| a.id.cmp(&b.id))
        });
        rows
    }

    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> Option<MiddlewareDefinition> {
        let definition = self.middleware.get_mut(id)?;
        definition.enabled = enabled;
        let updated = definition.clone();
        self.persist();
        Some(updated)
    }

    pub fn replace_local_plugin_middleware(
        &mut self,
        definitions: Vec<MiddlewareDefinition>,
    ) -> Vec<MiddlewareDefinition> {
        self.middleware
            .retain(|_, definition| definition.source != MiddlewareSource::LocalPlugin);
        for mut definition in definitions {
            definition.source = MiddlewareSource::LocalPlugin;
            self.middleware.insert(definition.id.clone(), definition);
        }
        self.persist();
        self.middleware()
            .into_iter()
            .filter(|definition| definition.source == MiddlewareSource::LocalPlugin)
            .collect()
    }

    pub fn enabled_for_phase(&self, phase: MiddlewarePhase) -> Vec<MiddlewareDefinition> {
        self.middleware()
            .into_iter()
            .filter(|definition| definition.enabled && definition.phase == phase)
            .collect()
    }

    pub fn record_span(&mut self, span: RuntimeSpan) {
        let observation = execution_observation(&span);
        self.spans.insert(span.id.clone(), span);
        Self::evict_oldest(&mut self.spans, MAX_RETAINED_SPANS, |s| {
            s.started_at.clone()
        });
        self.observations
            .insert(observation.id.clone(), observation);
        Self::evict_oldest(&mut self.observations, MAX_RETAINED_OBSERVATIONS, |o| {
            o.created_at.clone()
        });
        self.persist_throttled();
    }

    pub fn record_observation(&mut self, observation: RuntimeObservation) {
        self.observations
            .insert(observation.id.clone(), observation);
        Self::evict_oldest(&mut self.observations, MAX_RETAINED_OBSERVATIONS, |o| {
            o.created_at.clone()
        });
        self.persist_throttled();
    }

    /// Drop the oldest entries once `map` exceeds `cap`.
    ///
    /// Every record rewrites the whole store file, so an uncapped map is an O(n)
    /// write against unbounded n — the file reached 6.4 MB on one machine with
    /// no mechanism that would ever shrink it. The event log has a 30-day prune;
    /// this store had nothing.
    fn evict_oldest<T, F>(map: &mut BTreeMap<String, T>, cap: usize, key: F)
    where
        F: Fn(&T) -> String,
    {
        if map.len() <= cap {
            return;
        }
        let mut stamped: Vec<(String, String)> =
            map.iter().map(|(id, v)| (key(v), id.clone())).collect();
        // Oldest first; ISO-8601 timestamps sort lexicographically.
        stamped.sort();
        let excess = map.len() - cap;
        for (_, id) in stamped.into_iter().take(excess) {
            map.remove(&id);
        }
        tracing::warn!(
            cap,
            dropped = excess,
            "runtime middleware store saturated; dropped oldest entries"
        );
    }

    pub fn spans(&self, session_id: Option<&str>) -> Vec<RuntimeSpan> {
        let mut rows = self
            .spans
            .values()
            .filter(|span| session_id.is_none_or(|id| span.session_id == id))
            .cloned()
            .collect::<Vec<_>>();
        // IDs are opaque, so map order is not execution order. Consumers use
        // this projection as a causal timeline; preserve timestamp order with
        // ID only as deterministic tie-breaker.
        rows.sort_by(|left, right| {
            left.started_at
                .cmp(&right.started_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        rows
    }

    pub fn span(&self, id: &str) -> Option<RuntimeSpan> {
        self.spans.get(id).cloned()
    }

    pub fn observations(&self, session_id: Option<&str>) -> Vec<RuntimeObservation> {
        let mut rows = self
            .observations
            .values()
            .filter(|observation| session_id.is_none_or(|id| observation.session_id == id))
            .cloned()
            .collect::<Vec<_>>();
        rows.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        rows
    }

    pub fn snapshot(&self) -> MiddlewareRegistrySnapshot {
        MiddlewareRegistrySnapshot {
            middleware: self.middleware(),
            spans: self.spans(None),
            observations: self.observations(None),
        }
    }
}

fn execution_observation(span: &RuntimeSpan) -> RuntimeObservation {
    let created_at = span
        .ended_at
        .clone()
        .unwrap_or_else(|| span.started_at.clone());
    RuntimeObservation {
        id: format!("execution:{}", span.id),
        span_id: span.id.clone(),
        session_id: span.session_id.clone(),
        severity: if span.status.eq_ignore_ascii_case("failed") {
            "error"
        } else {
            "info"
        }
        .to_string(),
        message: format!("{:?}: {}", span.phase, span.status),
        payload: serde_json::json!({
            "schemaVersion": "coderight.execution-observation.v1",
            "eventId": span.id.clone(),
            "sessionId": span.session_id.clone(),
            "causalParentId": span.parent_id.clone(),
            "phase": span.phase,
            "status": span.status.clone(),
            "createdAt": created_at.clone(),
            "attributes": bounded_execution_observation_attributes(&span.attributes),
        }),
        created_at,
    }
}

fn bounded_execution_observation_attributes(attributes: &Value) -> Value {
    let Ok(encoded) = serde_json::to_vec(attributes) else {
        return serde_json::json!({ "truncated": true, "originalBytes": null });
    };
    if encoded.len() <= MAX_EXECUTION_OBSERVATION_ATTRIBUTES_BYTES {
        return attributes.clone();
    }
    serde_json::json!({
        "truncated": true,
        "originalBytes": encoded.len(),
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedRuntimeMiddleware {
    #[serde(default)]
    middleware: Vec<MiddlewareDefinition>,
    #[serde(default)]
    spans: Vec<RuntimeSpan>,
    #[serde(default)]
    observations: Vec<RuntimeObservation>,
}

pub fn builtin_middleware() -> Vec<MiddlewareDefinition> {
    vec![
        MiddlewareDefinition {
            id: "builtin.trace-session-lifecycle".to_string(),
            title: "Trace session start".to_string(),
            source: MiddlewareSource::BuiltIn,
            phase: MiddlewarePhase::SessionStart,
            effect: MiddlewareEffect::Observe,
            tool: None,
            requires_toolset: None,
            condition: None,
            mutate_args: BTreeMap::new(),
            reason: None,
            enabled: true,
            order: 10,
            description: "Records local spans for session start boundaries.".to_string(),
        },
        MiddlewareDefinition {
            id: "builtin.trace-session-end".to_string(),
            title: "Trace session end".to_string(),
            source: MiddlewareSource::BuiltIn,
            phase: MiddlewarePhase::SessionEnd,
            effect: MiddlewareEffect::Observe,
            tool: None,
            requires_toolset: None,
            condition: None,
            mutate_args: BTreeMap::new(),
            reason: None,
            enabled: true,
            order: 11,
            description: "Records local spans for session end boundaries.".to_string(),
        },
        MiddlewareDefinition {
            id: "builtin.trace-llm-call-start".to_string(),
            title: "Trace LLM call start".to_string(),
            source: MiddlewareSource::BuiltIn,
            phase: MiddlewarePhase::PreLlmCall,
            effect: MiddlewareEffect::Observe,
            tool: None,
            requires_toolset: None,
            condition: None,
            mutate_args: BTreeMap::new(),
            reason: None,
            enabled: true,
            order: 15,
            description: "Records local spans before provider calls.".to_string(),
        },
        MiddlewareDefinition {
            id: "builtin.trace-llm-call-end".to_string(),
            title: "Trace LLM call end".to_string(),
            source: MiddlewareSource::BuiltIn,
            phase: MiddlewarePhase::PostLlmCall,
            effect: MiddlewareEffect::Observe,
            tool: None,
            requires_toolset: None,
            condition: None,
            mutate_args: BTreeMap::new(),
            reason: None,
            enabled: true,
            order: 16,
            description: "Records local spans after provider calls.".to_string(),
        },
        MiddlewareDefinition {
            id: "builtin.guard-tool-arguments".to_string(),
            title: "Guard tool arguments".to_string(),
            source: MiddlewareSource::BuiltIn,
            phase: MiddlewarePhase::PreToolCall,
            effect: MiddlewareEffect::RequireApproval,
            tool: None,
            requires_toolset: None,
            condition: None,
            mutate_args: BTreeMap::new(),
            reason: None,
            enabled: true,
            order: 20,
            description:
                "Reserves a stable pre-tool policy point for argument normalization and approvals."
                    .to_string(),
        },
        MiddlewareDefinition {
            id: "builtin.observe-tool-results".to_string(),
            title: "Observe tool results".to_string(),
            source: MiddlewareSource::BuiltIn,
            phase: MiddlewarePhase::PostToolCall,
            effect: MiddlewareEffect::Observe,
            tool: None,
            requires_toolset: None,
            condition: None,
            mutate_args: BTreeMap::new(),
            reason: None,
            enabled: true,
            order: 21,
            description: "Records local spans after tool dispatch finishes.".to_string(),
        },
        MiddlewareDefinition {
            id: "builtin.observe-approvals".to_string(),
            title: "Observe approvals".to_string(),
            source: MiddlewareSource::BuiltIn,
            phase: MiddlewarePhase::ApprovalRequested,
            effect: MiddlewareEffect::Observe,
            tool: None,
            requires_toolset: None,
            condition: None,
            mutate_args: BTreeMap::new(),
            reason: None,
            enabled: true,
            order: 30,
            description: "Records approval-request observations for operator review.".to_string(),
        },
        MiddlewareDefinition {
            id: "builtin.trace-subagent-start".to_string(),
            title: "Trace subagent start".to_string(),
            source: MiddlewareSource::BuiltIn,
            phase: MiddlewarePhase::SubagentStart,
            effect: MiddlewareEffect::Observe,
            tool: None,
            requires_toolset: None,
            condition: None,
            mutate_args: BTreeMap::new(),
            reason: None,
            enabled: true,
            order: 33,
            description: "Records local spans when delegated subagents begin.".to_string(),
        },
        MiddlewareDefinition {
            id: "builtin.trace-subagent-stop".to_string(),
            title: "Trace subagent stop".to_string(),
            source: MiddlewareSource::BuiltIn,
            phase: MiddlewarePhase::SubagentStop,
            effect: MiddlewareEffect::Observe,
            tool: None,
            requires_toolset: None,
            condition: None,
            mutate_args: BTreeMap::new(),
            reason: None,
            enabled: true,
            order: 34,
            description: "Records local spans when delegated subagents finish.".to_string(),
        },
        MiddlewareDefinition {
            id: "builtin.trace-scheduled-job-start".to_string(),
            title: "Trace scheduled job start".to_string(),
            source: MiddlewareSource::BuiltIn,
            phase: MiddlewarePhase::ScheduledJobStart,
            effect: MiddlewareEffect::Observe,
            tool: None,
            requires_toolset: None,
            condition: None,
            mutate_args: BTreeMap::new(),
            reason: None,
            enabled: true,
            order: 35,
            description: "Records local spans when workspace scheduled jobs begin.".to_string(),
        },
        MiddlewareDefinition {
            id: "builtin.trace-scheduled-job-end".to_string(),
            title: "Trace scheduled job end".to_string(),
            source: MiddlewareSource::BuiltIn,
            phase: MiddlewarePhase::ScheduledJobEnd,
            effect: MiddlewareEffect::Observe,
            tool: None,
            requires_toolset: None,
            condition: None,
            mutate_args: BTreeMap::new(),
            reason: None,
            enabled: true,
            order: 36,
            description: "Records local spans when workspace scheduled jobs finish.".to_string(),
        },
    ]
}
