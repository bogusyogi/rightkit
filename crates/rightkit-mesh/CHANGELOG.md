# Changelog

## 0.1.1 (unreleased)

- Additive: the `devicelink` module (feature `devicelink`, off by default): device identity, short-code and join-token pairing, the paired-device registry file, the device circle (member lists, introductions, endorsements, grants), resume tickets and the per-request capability check, moved from Pulse (`pulse-core devicelink`). The record types come from `rightkit-device-link-types` 0.1.0 and owner-check verification from `rightkit-approvals` 0.1.0. No existing API changed.

## 0.1.0 (unreleased)

- First RightKit release of the device mesh, moved from the Pulse repository (`pulse-mesh`, in-tree since 2026-10-17): iroh live sessions, end-to-end encrypted mailbox (`rightkit-mesh-mailbox`), sync reconciliation, capped relayed transfers, capability hello.
- The Pulse trust adapter (`pulse-link` feature, `trust_pulse.rs`) is removed: the host implements `PeerTrust`.
- Unit tests for the seal and the relay caps.
