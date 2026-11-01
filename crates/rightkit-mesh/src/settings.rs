//! Mesh settings and their documented defaults (everything is a setting; `docs/settings.md`).
//!
//! The whole mesh is OFF until it is verified on installed builds (Pulse #138): the master
//! switch defaults to false and [`crate::Mesh::start`] refuses to run while it is off. Every
//! other setting has the default the design names. The names here are the keys the hub and the
//! notches publish; wiring them into those settings stores is not part of this crate.

use serde::{Deserialize, Serialize};

/// Where the Orthic mailbox lives: its iroh endpoint id (64 hex characters of its public key).
/// Its relay is the one in [`MeshSettings::relay_url`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailboxTarget {
    pub endpoint_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MeshSettings {
    /// Master switch, `deviceMesh`. Default off until verified.
    pub enabled: bool,
    /// Our own relay, `deviceMeshRelayUrl`, for example `https://relay.example.com`. No default:
    /// there is no public relay in this crate.
    pub relay_url: Option<String>,
    /// The mailbox server's identity, `deviceMeshMailbox`. `None` means no offline delivery.
    pub mailbox: Option<MailboxTarget>,
    /// "Connect through the Orthic relay", `deviceMeshRelay`. Default on. Off means direct
    /// connections only (no relay is used, so only devices with a known reachable address connect).
    pub use_relay: bool,
    /// "Accept connections from my devices", `deviceMeshAccept`. Default on. Off registers no
    /// protocol handlers, so this device can dial out but nothing can dial in.
    pub accept_connections: bool,
    /// "Direct connections for large transfers", `deviceMeshDirectLarge`. Default on. Off refuses
    /// anything above the relayed cap instead of waiting for a direct path.
    pub direct_for_large: bool,
    /// "Send small files through the relay when direct fails", `deviceMeshRelaySmall`. Default
    /// on, capped by the D7 caps. Off means nothing is ever relayed except messages and records.
    pub relay_small_files: bool,
    /// How long a phone image waits for a direct path before the capped relayed fallback,
    /// `deviceMeshDirectWaitSecs`. Default 5 (a starting guess from `docs/connectivity-images.md`;
    /// measure before trusting it).
    pub direct_wait_secs: u64,
    /// "Find my devices on this network", `deviceMeshLocal`. Default on. Announces this device and
    /// looks up paired devices by mDNS on the local network (iroh's `MdnsAddressLookup`), so two
    /// devices on one network connect directly with no relay and no internet. Off leaves only
    /// addresses a relay or a known direct address supply.
    pub local_discovery: bool,
    /// The build text this device answers in its capability hello (`hello.rs`, Pulse #560): the
    /// host's version and commit. Empty answers an empty build.
    pub hello_build: String,
    /// Capability words the host adds to the hello beside the mesh's own (`idle-close`), for
    /// example `send-manifest`.
    pub hello_caps: Vec<String>,
}

impl Default for MeshSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            relay_url: None,
            mailbox: None,
            use_relay: true,
            accept_connections: true,
            direct_for_large: true,
            relay_small_files: true,
            direct_wait_secs: 5,
            local_discovery: true,
            hello_build: String::new(),
            hello_caps: Vec::new(),
        }
    }
}

impl MeshSettings {
    /// The settings a device link runs the mesh with (Pulse #138): on, with local discovery. With
    /// `relay_url` `Some` (OUR relay; the caller passes one only when the user configured it, so
    /// there is never a public default) the relay is the fallback; with `None` there is no relay at
    /// all: direct connections found by local discovery only. Accepting incoming connections when
    /// `accept` is true. No mailbox.
    pub fn for_device_link(relay_url: Option<&str>, accept: bool, local_discovery: bool) -> Self {
        Self {
            enabled: true,
            relay_url: relay_url.map(str::to_string),
            use_relay: relay_url.is_some(),
            accept_connections: accept,
            local_discovery,
            ..Self::default()
        }
    }
}
