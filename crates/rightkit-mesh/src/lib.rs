//! `rightkit-mesh`: the Orthic device mesh (Pulse #138), built in-tree in Pulse and handed to
//! RightKit later, like `swift/RightKitHub`. Pulse never edits the RightKit repository.
//!
//! What it is (Adrian, 2026-10-17: "Build in Pulse now"; the hybrid design):
//!
//! * **Live sessions on iroh 1.3.0.** Direct first, falling back to OUR iroh-relay on the
//!   existing Hetzner box. No default public relay, no n0 DNS, no DHT (`endpoint.rs`).
//! * **Identity and trust from device-link.** The Pulse identity seed is the iroh secret key; the
//!   endpoint id is the device-link public key. Trust is the paired registry, asked through
//!   [`PeerTrust`]; the mesh keeps no second device list (`identity.rs`; the host implements it).
//! * **End-to-end mailbox for offline devices.** Sealed items (ChaCha20-Poly1305 under
//!   per-pairing keys exchanged on the first live connection) deposited at `rightkit-mesh-mailbox`;
//!   the recipient dials out, pulls, acknowledges, and the server deletes (`mailbox/`, `seal.rs`,
//!   `keys.rs`). 7-day TTL.
//! * **Sync reconciliation by record id** on connect: "what do you have since X" (`sync.rs`).
//! * **Capped relayed transfers** (D7): 25 MB per transfer, 10 MB per clipboard or phone image or
//!   chat attachment, 250 MB per circle per month, screens direct-only; phone images prefer a
//!   direct stream and relay only as the capped fallback (`caps.rs`, `transfer.rs`).
//! * **The owner circle (D4):** introductions add a device everywhere with no grants; members may
//!   revoke third devices; revoke-only (`circle.rs`).
//!
//! The whole mesh is OFF by default (`MeshSettings::enabled`) until it is verified on installed
//! builds. Nothing here has been compiled or run; see `rightkit-mesh/README.md`.

pub mod caps;
pub mod circle;
pub mod endpoint;
pub mod error;
pub mod frame;
pub mod hello;
pub mod identity;
pub mod keys;
pub mod mailbox;
pub mod seal;
pub mod settings;
pub mod sync;
pub mod transfer;

#[cfg(feature = "devicelink")]
pub mod devicelink;

mod util;

pub use caps::{
    Kind, PathKind, Refusal, RefusalCode, RelayBudget, Route, check_route, check_small, month_key,
};
pub use circle::{CircleMessage, CircleOutcome, Introduction, handle_circle_message};
pub use endpoint::{
    BlockingStream, Mesh, MeshInit, PathEvent, PathLabel, PeerPath, StreamControl, StreamService,
    SyncMesh, path_is_nearby, path_kind, set_path_observer,
};
pub use error::MeshError;
pub use hello::{ALPN_HELLO, HelloEvent, PeerHello, set_hello_observer};
pub use identity::{IntroOutcome, PeerInfo, PeerTrust, public_key_for_seed, secret_key_from_seed};
pub use mailbox::{Inbound, Mailbox, MailboxClient, PullReport};
pub use seal::{KeyStore, MemoryKeyStore, PairKeys};
pub use settings::{MailboxTarget, MeshSettings};
pub use sync::{Change, PullResult, RecordStore};
pub use transfer::{Begin, BulkReceiver, BulkSink, Delivery, Offer, SendOutcome};
