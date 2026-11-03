# Changelog

## 0.1.2 (unpublished)

- Added the relay admission registry `mailbox::registry` and the service `rightkit-mesh-relay-admission` (#952): device registration by proof of work (`pow_ok`, default 20 leading zero bits) or by a pairing attestation, an Ed25519 signature from a current circle member over `attestation_input(circle, endpoint)`; quotas per circle, per /24 (IPv4) or /48 (IPv6) network prefix, per registry; eviction (`evict`, `evict_circle`, `readmit`) effective at the next relay connection with no relay restart, persisted in one atomically replaced JSON state file that fails closed when unreadable. The service answers the relay's `POST /admit` from the registry, the operator allowlist (the floor, loaded by the existing `Allowlist`) or unexpired grants, and serves `POST /register`, `POST /evict` (admin token) and `GET /pow`, loopback only behind a TLS proxy. Limit: a connection already up is not cut by an eviction (iroh-relay 1.4.1 has no per-connection revocation hook and no per-endpoint byte metering); a cut of a live device still restarts the relay. Additive. Not compiled here. Needs: relay-side deployment (`deploy/relay`, the Pulse owner) to point `access.http.url` at the new service.

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.1 (unreleased)

- Additive: the `devicelink` module (feature `devicelink`, off by default): device identity, short-code and join-token pairing, the paired-device registry file, the device circle (member lists, introductions, endorsements, grants), resume tickets and the per-request capability check, moved from Pulse (`pulse-core devicelink`). The record types come from `rightkit-device-link-types` 0.1.0 and owner-check verification from `rightkit-owner-approvals` 0.1.0. `devicelink::policy::authorize` decides whether a paired computer may use chat here. No existing API changed.

## 0.1.0 (unreleased)

- First RightKit release of the device mesh, moved from the Pulse repository (`pulse-mesh`, in-tree since 2026-10-17): iroh live sessions, end-to-end encrypted mailbox (`rightkit-mesh-mailbox`), sync reconciliation, capped relayed transfers, capability hello.
- The Pulse trust adapter (`pulse-link` feature, `trust_pulse.rs`) is removed: the host implements `PeerTrust`.
- Unit tests for the seal and the relay caps.
