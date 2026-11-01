//! Fail-closed binding between a resolved role pack and MCP channel availability.
//!
//! `role_pack::ResolvedRolePack` owns the validated role/toolset assignment;
//! the MCP crate owns transports and per-server channel policy.  This module
//! deliberately sits between them and accepts a snapshot of the channels the
//! MCP layer has made available.  The caller supplies the channels declared by
//! the runtime request and must not turn an unavailable channel into an empty
//! or reduced toolset.
//!
//! The MCP manager is intentionally not a dependency of this crate.  The
//! daemon adapts its live manager/configuration into [`McpChannelSet`], then
//! consumes the typed result before admitting the role pack.  That keeps one
//! MCP transport owner while making the role-pack admission decision visible.

use std::collections::BTreeSet;

use crate::role_pack::ResolvedRolePack;

/// A normalized snapshot of MCP channels available to a role-pack admission.
///
/// Channel identifiers follow the existing MCP convention: surrounding
/// whitespace is removed and comparison is ASCII case-insensitive.  An empty
/// channel is never inserted, so a caller cannot accidentally make a blank
/// declaration available.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McpChannelSet {
    channels: BTreeSet<String>,
}

impl McpChannelSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a channel snapshot from the MCP layer's available identifiers.
    /// Empty source entries are ignored because they do not identify a real
    /// channel; an empty *declaration* remains an error in the binder below.
    pub fn from_channels<'a, I>(channels: I) -> Self
    where
        I: IntoIterator<Item = &'a str>,
    {
        let mut set = Self::new();
        for channel in channels {
            set.insert(channel);
        }
        set
    }

    pub fn insert(&mut self, channel: &str) -> bool {
        let normalized = normalize_channel(channel);
        if normalized.is_empty() {
            return false;
        }
        self.channels.insert(normalized)
    }

    pub fn contains(&self, channel: &str) -> bool {
        let normalized = normalize_channel(channel);
        !normalized.is_empty() && self.channels.contains(&normalized)
    }

    pub fn is_empty(&self) -> bool {
        self.channels.is_empty()
    }

    pub fn len(&self) -> usize {
        self.channels.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.channels.iter().map(String::as_str)
    }
}

/// The role-pack assignment after it has been bound to the requested MCP
/// channels.  The toolset is retained exactly as resolved; no unavailable
/// channel can silently narrow it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RolePackMcpBinding {
    pub role: String,
    pub toolset: Vec<String>,
    pub channels: Vec<String>,
}

/// Admission failures are explicit so callers can report the role, toolset,
/// and channel rather than falling back to a less capable MCP configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RolePackMcpError {
    NoToolset { role: String },
    NoChannelsDeclared { role: String },
    EmptyChannel { role: String },
    UnavailableChannel { role: String, channel: String },
}

impl std::fmt::Display for RolePackMcpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoToolset { role } => {
                write!(f, "role pack for role {role:?} declares no toolset")
            }
            Self::NoChannelsDeclared { role } => {
                write!(f, "role pack for role {role:?} declares no MCP channels")
            }
            Self::EmptyChannel { role } => {
                write!(
                    f,
                    "role pack for role {role:?} declares an empty MCP channel"
                )
            }
            Self::UnavailableChannel { role, channel } => write!(
                f,
                "role pack for role {role:?} declares unavailable MCP channel {channel:?}"
            ),
        }
    }
}

impl std::error::Error for RolePackMcpError {}

/// Bind a resolved role pack's complete declared toolset to its requested MCP
/// channels.
///
/// Every requested channel must be present in `available_channels`.  The
/// operation is atomic: it returns no partial binding and never silently drops
/// a channel or toolset.  The daemon should pass the active session channel as
/// a one-item `declared_channels` slice, or pass an explicitly declared set
/// when a role-pack admission supports more than one channel.
pub fn bind_role_pack_mcp(
    resolved: &ResolvedRolePack,
    declared_channels: &[String],
    available_channels: &McpChannelSet,
) -> Result<RolePackMcpBinding, RolePackMcpError> {
    let role = resolved.pack.role.trim().to_string();
    if resolved.pack.toolset.is_empty() {
        return Err(RolePackMcpError::NoToolset { role });
    }

    let mut toolset = Vec::with_capacity(resolved.pack.toolset.len());
    for name in &resolved.pack.toolset {
        let name = name.trim();
        if name.is_empty() {
            return Err(RolePackMcpError::NoToolset {
                role: resolved.pack.role.clone(),
            });
        }
        toolset.push(name.to_string());
    }

    if declared_channels.is_empty() {
        return Err(RolePackMcpError::NoChannelsDeclared {
            role: resolved.pack.role.clone(),
        });
    }

    let mut channels = Vec::with_capacity(declared_channels.len());
    for channel in declared_channels {
        let normalized = normalize_channel(channel);
        if normalized.is_empty() {
            return Err(RolePackMcpError::EmptyChannel {
                role: resolved.pack.role.clone(),
            });
        }
        if !available_channels.contains(&normalized) {
            return Err(RolePackMcpError::UnavailableChannel {
                role: resolved.pack.role.clone(),
                channel: normalized,
            });
        }
        if !channels.contains(&normalized) {
            channels.push(normalized);
        }
    }

    Ok(RolePackMcpBinding {
        role,
        toolset,
        channels,
    })
}

fn normalize_channel(channel: &str) -> String {
    channel.trim().to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::role_pack::{resolve_role_pack, RolePackCatalogEntry};

    fn valid_raw() -> &'static str {
        r#"
 schema_version = 1
 role = "reviewer"
 primary = "model-strong"
 cheap_candidate = "model-cheap"
 fallbacks = ["model-fallback"]
 verifier = "model-verifier"
 reasoning = "high"
 context_budget = 50000
 toolset = ["Read", "Grep"]
 permissions = ["read"]
 isolation = "sandboxed"
 max_turns = 10
 max_cost_usd = 2.5
 retry_limit = 2
 escalate_on = ["timeout"]
 "#
    }

    fn resolved() -> crate::role_pack::ResolvedRolePack {
        let catalog = vec![
            RolePackCatalogEntry {
                id: "model-strong".into(),
                context_window_tokens: 200_000,
                cheap: false,
                provider_family: "anthropic".into(),
            },
            RolePackCatalogEntry {
                id: "model-cheap".into(),
                context_window_tokens: 100_000,
                cheap: true,
                provider_family: "anthropic".into(),
            },
            RolePackCatalogEntry {
                id: "model-fallback".into(),
                context_window_tokens: 100_000,
                cheap: false,
                provider_family: "anthropic".into(),
            },
            RolePackCatalogEntry {
                id: "model-verifier".into(),
                context_window_tokens: 100_000,
                cheap: false,
                provider_family: "openai".into(),
            },
        ];
        resolve_role_pack(valid_raw(), &catalog, None).unwrap()
    }

    #[test]
    fn channel_set_normalizes_and_deduplicates_channels() {
        let set = McpChannelSet::from_channels([" Desktop ", "CLI", "desktop"]);
        assert_eq!(set.len(), 2);
        assert!(set.contains("desktop"));
        assert!(set.contains(" CLI "));
        assert_eq!(set.iter().collect::<Vec<_>>(), vec!["cli", "desktop"]);
    }

    #[test]
    fn binds_complete_resolved_toolset_to_available_channels() {
        let resolved = resolved();
        let available = McpChannelSet::from_channels(["desktop", "telegram"]);
        let binding = bind_role_pack_mcp(
            &resolved,
            &["Desktop".to_string(), "telegram".to_string()],
            &available,
        )
        .unwrap();

        assert_eq!(binding.role, "reviewer");
        assert_eq!(binding.toolset, vec!["Read", "Grep"]);
        assert_eq!(binding.channels, vec!["desktop", "telegram"]);
    }

    #[test]
    fn rejects_unavailable_declared_channel_instead_of_dropping_it() {
        let resolved = resolved();
        let available = McpChannelSet::from_channels(["desktop"]);
        let error = bind_role_pack_mcp(
            &resolved,
            &["desktop".to_string(), "telegram".to_string()],
            &available,
        )
        .unwrap_err();

        assert_eq!(
            error,
            RolePackMcpError::UnavailableChannel {
                role: "reviewer".to_string(),
                channel: "telegram".to_string(),
            }
        );
    }

    #[test]
    fn rejects_empty_declared_channel_visibly() {
        let resolved = resolved();
        let available = McpChannelSet::from_channels(["desktop"]);
        let error = bind_role_pack_mcp(&resolved, &["  ".to_string()], &available).unwrap_err();
        assert_eq!(
            error,
            RolePackMcpError::EmptyChannel {
                role: "reviewer".to_string(),
            }
        );
    }

    #[test]
    fn rejects_missing_channel_declaration_visibly() {
        let resolved = resolved();
        let available = McpChannelSet::from_channels(["desktop"]);
        let error = bind_role_pack_mcp(&resolved, &[], &available).unwrap_err();
        assert_eq!(
            error,
            RolePackMcpError::NoChannelsDeclared {
                role: "reviewer".to_string(),
            }
        );
    }

    #[test]
    fn duplicate_declared_channels_do_not_duplicate_the_binding() {
        let resolved = resolved();
        let available = McpChannelSet::from_channels(["desktop"]);
        let binding = bind_role_pack_mcp(
            &resolved,
            &["desktop".to_string(), "DESKTOP".to_string()],
            &available,
        )
        .unwrap();
        assert_eq!(binding.channels, vec!["desktop"]);
    }
}
