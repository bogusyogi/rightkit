//! Shared, bounded connection pool with a mandatory trust gate for workspace-sourced servers.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use serde_json::Value;

use crate::client::{ClientOptions, McpClient, ServerSpec};
use crate::error::{McpError, Result};
use crate::sanitize::sanitize_tool_name;
use crate::trust::{fingerprint, pending_for, ServerSource, TrustDecision, TrustStore};
use crate::types::{ToolDescriptor, ToolResult};

#[derive(Debug, Clone)]
pub struct ServerEntry {
    pub name: String,
    pub spec: ServerSpec,
    pub source: ServerSource,
}

pub struct PoolConfig {
    pub max_connections: usize,
    pub options: ClientOptions,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self { max_connections: 16, options: ClientOptions::default() }
    }
}

struct Slot {
    client: Arc<McpClient>,
    last_used: Instant,
}

pub struct ConnectionPool {
    cfg: PoolConfig,
    trust: Mutex<TrustStore>,
    slots: Mutex<HashMap<String, Slot>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl ConnectionPool {
    pub fn new(cfg: PoolConfig, trust: TrustStore) -> Self {
        Self { cfg, trust: Mutex::new(trust), slots: Mutex::new(HashMap::new()) }
    }

    pub fn trust(&self) -> MutexGuard<'_, TrustStore> {
        lock(&self.trust)
    }

    fn key(e: &ServerEntry) -> String {
        let scope = match &e.source {
            ServerSource::User => "user".to_string(),
            ServerSource::Workspace(w) => format!("ws:{w}"),
        };
        format!("{scope}|{}|{}", e.name, fingerprint(&e.spec))
    }

    /// Gate first (no spawn/connect on Pending/Denied), then share one client per (source, name, fingerprint).
    pub fn get(&self, e: &ServerEntry) -> Result<Arc<McpClient>> {
        if let ServerSource::Workspace(ws) = &e.source {
            match lock(&self.trust).decide(ws, &e.name, &e.spec) {
                TrustDecision::Approved => {}
                TrustDecision::Denied => {
                    return Err(McpError::Denied(format!("server '{}' was denied for this configuration", e.name)))
                }
                TrustDecision::Pending => return Err(McpError::NotApproved(Box::new(pending_for(ws, &e.name, &e.spec)))),
            }
        }
        let key = Self::key(e);
        let mut slots = lock(&self.slots);
        if let Some(s) = slots.get_mut(&key) {
            s.last_used = Instant::now();
            return Ok(s.client.clone());
        }
        if slots.len() >= self.cfg.max_connections {
            // Evict the least-recently-used connection nobody else holds.
            let victim = slots
                .iter()
                .filter(|(_, s)| Arc::strong_count(&s.client) == 1)
                .min_by_key(|(_, s)| s.last_used)
                .map(|(k, _)| k.clone());
            match victim {
                Some(k) => {
                    slots.remove(&k);
                }
                None => return Err(McpError::PoolFull(self.cfg.max_connections)),
            }
        }
        let client = Arc::new(McpClient::connect(&e.name, e.spec.clone(), self.cfg.options.clone())?);
        slots.insert(key, Slot { client: client.clone(), last_used: Instant::now() });
        Ok(client)
    }

    /// Drop every connection that no caller currently holds.
    pub fn evict_idle(&self) {
        lock(&self.slots).retain(|_, s| Arc::strong_count(&s.client) > 1);
    }

    pub fn close_all(&self) {
        lock(&self.slots).clear();
    }

    pub fn len(&self) -> usize {
        lock(&self.slots).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// List the tools of every reachable server. Unapproved/unreachable servers are reported in `skipped`
    /// (never contacted when unapproved). Sanitised-name collisions are an error.
    pub fn catalog(&self, entries: &[ServerEntry]) -> Result<Catalog> {
        let mut tools: Vec<QualifiedTool> = Vec::new();
        let mut skipped = Vec::new();
        for e in entries {
            let listed = self.get(e).and_then(|c| c.list_tools());
            match listed {
                Ok(list) => {
                    for t in list {
                        tools.push(QualifiedTool {
                            qualified: sanitize_tool_name(&e.name, &t.name),
                            server: e.name.clone(),
                            tool: t,
                        });
                    }
                }
                Err(err) => skipped.push((e.name.clone(), err.to_string())),
            }
        }
        let mut seen: HashMap<&str, Vec<String>> = HashMap::new();
        for t in &tools {
            seen.entry(&t.qualified).or_default().push(format!("{}/{}", t.server, t.tool.name));
        }
        if let Some((q, o)) = seen.into_iter().find(|(_, v)| v.len() > 1) {
            return Err(McpError::Collision { qualified: q.into(), originals: o });
        }
        Ok(Catalog { tools, skipped, entries: entries.to_vec() })
    }
}

#[derive(Debug, Clone)]
pub struct QualifiedTool {
    pub qualified: String,
    pub server: String,
    pub tool: ToolDescriptor,
}

#[derive(Debug, Clone)]
pub struct Catalog {
    pub tools: Vec<QualifiedTool>,
    pub skipped: Vec<(String, String)>,
    entries: Vec<ServerEntry>,
}

impl Catalog {
    pub fn call(&self, pool: &ConnectionPool, qualified: &str, args: Value) -> Result<ToolResult> {
        let t = self
            .tools
            .iter()
            .find(|t| t.qualified == qualified)
            .ok_or_else(|| McpError::ToolNotFound(qualified.into()))?;
        let entry = self.entries.iter().find(|e| e.name == t.server).ok_or_else(|| McpError::ToolNotFound(qualified.into()))?;
        pool.get(entry)?.call_tool(&t.tool.name, args)
    }
}
