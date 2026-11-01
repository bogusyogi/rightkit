# Changelog

## 0.1.0 (unreleased)

- First RightKit release of the device mesh, moved from the Pulse repository (`pulse-mesh`, in-tree since 2026-10-17): iroh live sessions, end-to-end encrypted mailbox (`rightkit-mesh-mailbox`), sync reconciliation, capped relayed transfers, capability hello.
- The Pulse trust adapter (`pulse-link` feature, `trust_pulse.rs`) is removed: the host implements `PeerTrust`.
- Unit tests for the seal and the relay caps.
