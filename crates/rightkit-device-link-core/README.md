# rightkit-device-link-core

The transport-agnostic device link: pairing, the paired-device registry, the device circle, revocation,
resume tickets, per-request authorization, phone policy and the host helpers around them. It has no
iroh, no tokio and no async runtime, so a host that must not link a transport (Pulse's Tauri hub) can use it.

`rightkit-mesh` re-exports this crate as `rightkit_mesh::devicelink` under its `devicelink` feature and
implements `PathObservation` for its `PathEvent`. (`rightkit-device-link` is a different crate: the
reliable ordered-delivery layer.)
