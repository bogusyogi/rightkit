//! The one data model: a request, its binding, its origin, its state, the grant a decision gives.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::digest::bind_of;
use crate::error::Error;

/// Domain tag of [`Binding::digest`].
const BINDING_DOMAIN: &[u8] = b"rightkit.approval.binding.v1";

/// Who asked and by which way in. A grant never crosses doors, filers or clients.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    /// The way the request came in (for example `local`, `gateway`, `window`). Part of the
    /// display digest.
    #[serde(default)]
    pub door: String,
    /// The process or label that filed it; empty for code running beside the store.
    #[serde(default)]
    pub filer: String,
    /// Whom the approval is for: a chat, an agent, a client key. Session grants are keyed by it.
    #[serde(default)]
    pub client: String,
}

/// What an approval is bound to. An approval holds only for these exact arguments, this
/// destination, until this expiry, at this revision.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    /// Hex SHA-256 of the exact arguments, from [`bind_of`] or a canonical-JSON hash. Empty only
    /// for a session request, which approves a family rather than one act.
    #[serde(default)]
    pub args: String,
    /// What the act reaches: a target id, a path, a recipient, a remote. Empty when `args`
    /// already covers it.
    #[serde(default)]
    pub destination: String,
    /// Unix milliseconds after which the approval is void; 0 leaves only the grant's own limit.
    #[serde(default)]
    pub expires_ms: u64,
    /// Revision of the thing approved; a different revision is a different thing.
    #[serde(default)]
    pub revision: u64,
}

impl Binding {
    /// A binding to `args` at `destination`, with no expiry of its own and revision 0.
    pub fn new(args: impl Into<String>, destination: impl Into<String>) -> Binding {
        Binding {
            args: args.into(),
            destination: destination.into(),
            expires_ms: 0,
            revision: 0,
        }
    }

    /// One hex SHA-256 over every field (length-prefixed, domain-tagged): the value to sign or
    /// store when the whole binding must travel as one string.
    pub fn digest(&self) -> String {
        bind_of(&[
            BINDING_DOMAIN,
            self.args.as_bytes(),
            self.destination.as_bytes(),
            &self.expires_ms.to_be_bytes(),
            &self.revision.to_be_bytes(),
        ])
    }

    /// The rule at the moment of use: `presented` (what is about to happen) must have the same
    /// destination ([`Error::Different`]), the same arguments and revision ([`Error::Changed`]),
    /// and `now_ms` must not be past this binding's expiry ([`Error::Expired`]).
    pub fn check(&self, presented: &Binding, now_ms: u64) -> Result<(), Error> {
        if self.destination != presented.destination {
            return Err(Error::Different);
        }
        if self.args != presented.args || self.revision != presented.revision {
            return Err(Error::Changed);
        }
        if self.expires_ms != 0 && now_ms > self.expires_ms {
            return Err(Error::Expired);
        }
        Ok(())
    }
}

/// A request for approval, as the store keeps it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    /// Opaque, unique in its store ([`crate::new_id`]). Not a secret: only a decision approves.
    pub id: String,
    /// What is asked: an app word (`act`, `session`, `send`, a draft kind).
    pub kind: String,
    /// Where it applies: a tool, a family, an account. Empty when the kind says it all.
    #[serde(default)]
    pub scope: String,
    #[serde(default)]
    pub origin: Origin,
    #[serde(default)]
    pub binding: Binding,
    /// What the decider is shown. Part of the display digest.
    #[serde(default)]
    pub summary: String,
    /// Display-only structured facts beside the summary; never part of a digest.
    #[serde(default)]
    pub detail: Value,
    pub created_ms: u64,
    /// The request waits for a decision until this time (inclusive).
    pub expires_ms: u64,
}

impl Request {
    /// The digest a decision is bound to: SHA-256 over `domain`, kind, scope, door, the
    /// binding's arguments and the summary, exactly as the decider is shown them. The deciding
    /// surface computes it from what it displayed; [`crate::rules::decide`] refuses a decision
    /// whose digest differs ([`Error::Shown`]). `domain` is the app's wire tag.
    pub fn display_digest(&self, domain: &str) -> String {
        bind_of(&[
            domain.as_bytes(),
            self.kind.as_bytes(),
            self.scope.as_bytes(),
            self.origin.door.as_bytes(),
            self.binding.args.as_bytes(),
            self.summary.as_bytes(),
        ])
    }

    /// True when `other` asks for the same thing: kind, scope, the whole origin, the binding's
    /// arguments and destination. A repeat of a waiting request is answered with the same id.
    pub fn same_ask(&self, other: &Request) -> bool {
        self.kind == other.kind
            && self.scope == other.scope
            && self.origin == other.origin
            && self.binding.args == other.binding.args
            && self.binding.destination == other.binding.destination
    }
}

/// Where a request stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// Waiting for a decision.
    #[default]
    Pending,
    /// Approved and not yet used (a session grant stays here while it lives).
    Approved,
    /// Refused by the decider.
    Denied,
    /// Used: the act ran. Final.
    Done,
    /// Claimed, but the act failed or its outcome is unknown. Final; never retried.
    Failed,
    /// Its time ran out before a decision. Final.
    Expired,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Pending => "pending",
            State::Approved => "approved",
            State::Denied => "denied",
            State::Done => "done",
            State::Failed => "failed",
            State::Expired => "expired",
        }
    }
}

/// How long an approval lives once given.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum Grant {
    /// One act, within `valid_ms` of the decision.
    Once { valid_ms: u64 },
    /// A session: every act until it has been idle for `idle_ms`.
    Idle { idle_ms: u64 },
    /// A session until `at_ms` (Unix milliseconds, exclusive).
    Until { at_ms: u64 },
    /// A session until it is revoked.
    Open,
}

/// A request and what became of it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub request: Request,
    #[serde(default)]
    pub state: State,
    /// Time of the last move (decision, use, failure); 0 while pending.
    #[serde(default)]
    pub decided_ms: u64,
    /// Last use of a session grant.
    #[serde(default)]
    pub last_used_ms: u64,
    /// Set by an approving decision.
    #[serde(default)]
    pub grant: Option<Grant>,
    /// A reason code for the last move (`user`, `timeout`, `unknown_outcome`).
    #[serde(default)]
    pub reason: Option<String>,
    /// What the act produced, when it names something (a message id, a stored name).
    #[serde(default)]
    pub result: Option<String>,
}

impl Record {
    /// A new pending record.
    pub fn pending(request: Request) -> Record {
        Record {
            request,
            ..Record::default()
        }
    }

    /// Whether the record can still be decided (pending) or used (approved) at `now_ms`.
    pub fn live(&self, now_ms: u64) -> bool {
        match self.state {
            State::Pending => now_ms <= self.request.expires_ms,
            State::Approved => {
                if self.request.binding.expires_ms != 0 && now_ms > self.request.binding.expires_ms
                {
                    return false;
                }
                match self.grant {
                    None | Some(Grant::Open) => true,
                    Some(Grant::Once { valid_ms }) => {
                        now_ms.saturating_sub(self.decided_ms) <= valid_ms
                    }
                    Some(Grant::Idle { idle_ms }) => {
                        now_ms.saturating_sub(self.last_used_ms.max(self.decided_ms)) <= idle_ms
                    }
                    Some(Grant::Until { at_ms }) => now_ms < at_ms,
                }
            }
            _ => false,
        }
    }
}

/// One move of a record, made by [`crate::Store::transition`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub to: State,
    pub at_ms: u64,
    /// Replaces the grant when `Some`.
    pub grant: Option<Grant>,
    pub reason: Option<String>,
    pub result: Option<String>,
}

impl Change {
    /// A move to `to` at `at_ms` with no grant, reason or result.
    pub fn to(to: State, at_ms: u64) -> Change {
        Change {
            to,
            at_ms,
            grant: None,
            reason: None,
            result: None,
        }
    }

    pub fn reason(mut self, reason: &str) -> Change {
        self.reason = Some(reason.to_string());
        self
    }

    pub fn result(mut self, result: &str) -> Change {
        self.result = Some(result.to_string());
        self
    }

    pub fn grant(mut self, grant: Grant) -> Change {
        self.grant = Some(grant);
        self
    }

    /// Applies the move to `record` (for stores that keep records in memory or in a file).
    pub fn apply(&self, record: &mut Record) {
        record.state = self.to;
        record.decided_ms = self.at_ms;
        record.last_used_ms = self.at_ms;
        if let Some(grant) = self.grant {
            record.grant = Some(grant);
        }
        record.reason = self.reason.clone();
        record.result = self.result.clone();
    }
}
