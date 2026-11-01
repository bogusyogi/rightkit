//! The data nav renders.
//!
//! `daemon → root sessions → agent trees`. Agents are logical, tracked by the
//! daemon, never OS processes -- this module owns no process handle and no
//! lifecycle. It is a read model: something upstream (the daemon client, out
//! of scope here) refreshes it; nav only displays and points at it.

use std::fmt;

/// Opaque id for a root session, as the daemon names it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionId(pub String);

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Opaque id for an agent -- root or subagent -- within one session's tree.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AgentId(pub String);

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Where an agent is in its lifecycle, as the daemon reports it.
///
/// Never inferred here: nav is a lens, so if the daemon has not said an agent
/// stopped, nav must not decide that it did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    Running,
    WaitingApproval,
    Done,
    Stopped,
}

impl AgentState {
    /// The semantic role this state paints with. Colour is never the only
    /// signal -- pair with [`Self::glyph_role`]'s theme glyph.
    pub fn role(&self) -> crate::component::Role {
        match self {
            AgentState::Running => crate::component::Role::Running,
            AgentState::WaitingApproval => crate::component::Role::Attention,
            AgentState::Done => crate::component::Role::Success,
            AgentState::Stopped => crate::component::Role::Failure,
        }
    }

    /// Short human label, always present regardless of colour support.
    pub fn label(&self) -> &'static str {
        match self {
            AgentState::Running => "running",
            AgentState::WaitingApproval => "waiting-approval",
            AgentState::Done => "done",
            AgentState::Stopped => "stopped",
        }
    }
}

/// One node in an agent tree: a root agent or one of its descendants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentNode {
    pub id: AgentId,
    pub name: String,
    pub model: String,
    /// Pre-formatted elapsed/cost summary (e.g. "2m14s · $0.03"). Formatting
    /// the number belongs to whoever owns money and clocks; nav only shows it.
    pub elapsed_cost: String,
    pub state: AgentState,
    pub children: Vec<AgentNode>,
}

impl AgentNode {
    /// Depth-first search for a node by id, from this node down.
    pub fn find(&self, id: &AgentId) -> Option<&AgentNode> {
        if &self.id == id {
            return Some(self);
        }
        self.children.iter().find_map(|c| c.find(id))
    }

    /// Every id in this node's subtree, this node first, depth-first.
    ///
    /// The exact scope a confirmed stop of this node covers: resolving a
    /// stop against this list is what makes cancellation *subtree-scoped*
    /// instead of a whole-session kill -- nothing above or beside this node
    /// ever appears in it.
    pub fn subtree_ids(&self) -> Vec<AgentId> {
        let mut ids = vec![self.id.clone()];
        for child in &self.children {
            ids.extend(child.subtree_ids());
        }
        ids
    }
}

/// One selectable entry in `/sessions`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    pub id: SessionId,
    pub name: String,
}

/// Optional display metadata for one `/sessions` row, supplied by the host
/// through [`super::NavView::set_session_details`] keyed by session id. Kept
/// out of [`SessionSummary`] so existing constructors stay valid.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionDetail {
    /// Human age such as `3h ago`.
    pub age: String,
    /// Workspace folder (or its short name) the session ran in.
    pub workspace: String,
    /// First user message, single line.
    pub preview: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(id: &str) -> AgentNode {
        AgentNode {
            id: AgentId(id.into()),
            name: id.into(),
            model: "m".into(),
            elapsed_cost: "0s".into(),
            state: AgentState::Running,
            children: vec![],
        }
    }

    #[test]
    fn find_locates_a_nested_descendant() {
        let tree = AgentNode {
            children: vec![leaf("child"), leaf("sibling")],
            ..leaf("root")
        };
        assert_eq!(tree.find(&AgentId("child".into())).unwrap().name, "child");
        assert!(tree.find(&AgentId("nope".into())).is_none());
    }

    #[test]
    fn every_state_carries_a_distinct_label() {
        let labels: Vec<_> = [
            AgentState::Running,
            AgentState::WaitingApproval,
            AgentState::Done,
            AgentState::Stopped,
        ]
        .iter()
        .map(|s| s.label())
        .collect();
        let mut unique = labels.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(
            labels.len(),
            unique.len(),
            "states must be distinguishable without colour"
        );
    }

    // A stop resolved through subtree_ids can only ever cover the targeted
    // subtree: the parent and siblings are structurally absent from the list.
    #[test]
    fn a_subtree_scope_contains_the_node_and_its_descendants_never_parent_or_siblings() {
        let tree = AgentNode {
            children: vec![
                AgentNode {
                    children: vec![leaf("grandchild")],
                    ..leaf("child-a")
                },
                leaf("child-b"),
            ],
            ..leaf("root")
        };
        let builder = tree.find(&AgentId("child-a".into())).unwrap();
        assert_eq!(
            builder.subtree_ids(),
            vec![AgentId("child-a".into()), AgentId("grandchild".into())]
        );
        assert!(!builder.subtree_ids().contains(&AgentId("root".into())));
        assert!(!builder.subtree_ids().contains(&AgentId("child-b".into())));
    }
}
