//! Per-session status of each tool source: what the engine that holds the session's connections
//! actually mounted, with an explicit state for every configured source.
//!
//! The answer comes from the engine (a [`SessionDiscovery`] the host reads from its own engine,
//! for example Codex `mcpServerStatus/list` with the session's thread id or Claude Code's
//! `mcpServerStatus()`), never from a process-wide list. This module only classifies. It never
//! starts a server, calls a tool, or carries an engine error text, URL, header or token: a
//! [`SourceStatus`] holds the configured name, the state and the tool count.
//!
//! "Registered" means configured and permitted (the trust gate approves it). It does not mean the
//! external service is reachable.

use serde::Serialize;

use crate::pool::ServerEntry;
use crate::trust::{TrustDecision, TrustStore};

/// The state of one configured tool source in one session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceState {
    /// Mounted in this session and providing at least one tool.
    Ready,
    /// Mounted in this session and connected, but providing no tools.
    NoTools,
    /// Configured and permitted, but absent from this session's engine (added after the session
    /// started).
    NotMounted,
    /// The engine could not connect the source.
    Failed,
    /// The engine reports the source needs sign-in.
    NeedsAuth,
    /// Not yet approved by the trust gate, or still starting in the engine.
    Pending,
    /// Turned off by the user, or denied by the trust gate.
    Disabled,
    /// The harness has no discovery entry point for tool sources.
    DiscoveryUnavailable,
    /// The read of the engine's status failed.
    DiscoveryFailed,
}

/// The connection state the engine reports for one source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineConnection {
    /// Connected, with this many tools.
    Connected {
        tools: usize,
    },
    /// Still starting.
    Starting,
    Failed,
    NeedsAuth,
    Disabled,
}

/// One source as the engine reports it for a session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineSource {
    pub name: String,
    pub connection: EngineConnection,
}

/// One session's discovery result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionDiscovery {
    /// The harness has no entry point that reports this session's tool sources.
    Unavailable,
    /// The engine's status read failed. Its error text is never forwarded.
    Failed,
    /// The engine's report: every source it holds for the session.
    Sources(Vec<EngineSource>),
}

/// The status of one configured source in one session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SourceStatus {
    pub name: String,
    pub state: SourceState,
    /// Tools the engine reports for the source; zero unless it is `ready`.
    pub tools: usize,
}

fn classify_connection(connection: EngineConnection) -> (SourceState, usize) {
    match connection {
        EngineConnection::Connected { tools: 0 } => (SourceState::NoTools, 0),
        EngineConnection::Connected { tools } => (SourceState::Ready, tools),
        EngineConnection::Starting => (SourceState::Pending, 0),
        EngineConnection::Failed => (SourceState::Failed, 0),
        EngineConnection::NeedsAuth => (SourceState::NeedsAuth, 0),
        EngineConnection::Disabled => (SourceState::Disabled, 0),
    }
}

/// The status of every source a session's own manager holds, from the connection state it
/// reports (for example `McpManager::connection_states`). Permission was decided when each
/// source was registered, so no trust store is consulted; nothing is started or called.
pub fn engine_source_status(sources: &[EngineSource]) -> Vec<SourceStatus> {
    sources
        .iter()
        .map(|source| {
            let (state, tools) = classify_connection(source.connection);
            SourceStatus {
                name: source.name.clone(),
                state,
                tools,
            }
        })
        .collect()
}

/// Classify every configured source against one session's discovery.
///
/// Permission comes first: a source the trust gate denies is `disabled` and one it has not
/// approved is `pending`, whatever the engine reports. A permitted source takes the engine's
/// state; a permitted source the engine does not list is `not-mounted`.
pub fn session_source_status(
    configured: &[ServerEntry],
    discovery: &SessionDiscovery,
    trust: &TrustStore,
) -> Vec<SourceStatus> {
    configured
        .iter()
        .map(|entry| {
            let (state, tools) = match trust.gate(&entry.source, &entry.name, &entry.spec) {
                TrustDecision::Denied => (SourceState::Disabled, 0),
                TrustDecision::Pending => (SourceState::Pending, 0),
                TrustDecision::Approved => match discovery {
                    SessionDiscovery::Unavailable => (SourceState::DiscoveryUnavailable, 0),
                    SessionDiscovery::Failed => (SourceState::DiscoveryFailed, 0),
                    SessionDiscovery::Sources(sources) => {
                        match sources.iter().find(|s| s.name == entry.name) {
                            None => (SourceState::NotMounted, 0),
                            Some(source) => classify_connection(source.connection),
                        }
                    }
                },
            };
            SourceStatus {
                name: entry.name.clone(),
                state,
                tools,
            }
        })
        .collect()
}
