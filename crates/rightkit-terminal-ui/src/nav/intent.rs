//! What nav asks the host application to do.
//!
//! Mirrors `composer::intent`: nav only *emits* intent -- it never descends
//! into, switches, or stops anything itself. Those effects (talking to the
//! daemon client, tearing down a subtree) live in the host, which owns the
//! connection nav doesn't. This is the enforcement point for "navigation is
//! not execution ownership": there is no method anywhere in this crate that
//! performs a stop or a switch, only ones that record the *intent* to ask for
//! one.

use crate::nav::model::{AgentId, SessionId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NavIntent {
    /// Open the `/agents` picker: the current root's tree.
    OpenAgents,
    /// Open the `/sessions` picker: sibling root sessions.
    OpenSessions,
    /// Descend into a child agent's transcript. View-only: does not touch
    /// whether that agent keeps running.
    Descend(AgentId),
    /// Ascend one level -- subagent to parent, parent to root, root to
    /// closing the picker. Never implies a stop.
    Ascend,
    /// Switch sideways to a different root session. Leaves the tree being
    /// left exactly as it was; this is detach, not cancel.
    SwitchSession(SessionId),
    /// Ask the host to start renaming this session (picker `r`). The host
    /// owns the title prompt and the durable write.
    RenameSession(SessionId),
    /// Ask the host to permanently delete this session. Emitted only after
    /// the picker's explicit `[y/N]` confirmation.
    DeleteSession(SessionId),
    /// Ask the host to stop exactly this agent's subtree. The only intent
    /// that ever asks for a stop, and it always names the agent explicitly --
    /// never implied by any navigation intent above. The scope is the subtree
    /// rooted at this id (`AgentNode::subtree_ids` resolves it): stopping a
    /// child must end that child and its descendants and nothing above or
    /// beside it.
    Stop(AgentId),
    /// Ask the host to apply a model selection through the route authority.
    /// Carries the model id as the shared catalog knows it; the picker that
    /// emitted it holds no catalog or persistence of its own.
    SelectModel(String),
    /// Ask the host to apply a reasoning-effort selection through the route
    /// authority. Emitted alongside (or instead of) `SelectModel` by the
    /// model/effort picker.
    SelectEffort(String),
    /// Ask the host to persist one model's `/model` visibility for a
    /// provider (`/providers` space toggle). Carries the NEW hidden state —
    /// the host owns the durable file; the picker only records what the
    /// user asked for.
    ToggleModelVisibility {
        provider: String,
        model: String,
        hidden: bool,
    },
    /// Ask the host to run one `/providers` level-2 action (sign in/out,
    /// add/replace/remove key, test connection, status). `secret` carries a
    /// typed API key for add/replace only; the host stores it through the
    /// keychain path and nothing here ever prints it.
    ProviderAction {
        provider: String,
        action: ProviderAction,
        secret: Option<SecretText>,
    },
}

/// A `/providers` level-2 action the host performs. Show/hide models is
/// picker-local and never an intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderAction {
    SignIn,
    SignOut,
    AddKey,
    ReplaceKey,
    RemoveKey,
    TestConnection,
    Status,
}

/// Secret text that never reaches `Debug` output: intents are logged and
/// asserted in tests, a typed API key must not leak through either.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretText(String);

impl SecretText {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The raw secret — only the host's keychain writer should call this.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for SecretText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretText(<redacted>)")
    }
}
