//! Principal: who submitted or authorized an act.
//! Source: `always-on-agent.md` section 2, "Gateway-origin requests": the
//! principal is the gateway's local connection and client id.

use serde::{Deserialize, Serialize};

/// Version of the [`Principal`] record. 2: the record may name a `paired-device` kind.
pub const PRINCIPAL_SCHEMA_VERSION: u32 = 2;
/// Vocabulary version for [`PrincipalKind`]. 2: adds `paired-device`.
pub const PRINCIPAL_KIND_SCHEMA_VERSION: u32 = 2;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Principal {
    pub schema_version: u32,
    pub kind: PrincipalKind,
    /// Owner id, relaying chat id, gateway client id, local client id, or paired
    /// device id (`dev_` plus 32 hex), according to `kind`.
    pub subject: String,
    /// Transport connection, present for gateway calls and local IPC clients. For a
    /// paired device it is the `pairing_id`.
    pub connection: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PrincipalKind {
    /// The workspace owner (the owner). Authority from his own words or grants.
    Owner,
    /// A chat that relays the owner's request. Its authority is the relayed
    /// provenance in `intent.source`, never the relay itself.
    RelayingChat,
    /// A ChatGPT gateway call. Authority is the local grant the owner set.
    GatewayClient,
    /// A UI, CLI or another local client attached to the engine.
    LocalClient,
    /// A remote device paired with this one through the device link. A grant holder
    /// (`devicelink` grants, rechecked on every request), never the owner; a policy check
    /// must not treat it as a local client.
    PairedDevice,
}
