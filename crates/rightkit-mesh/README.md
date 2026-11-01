# rightkit-mesh

Device mesh for Right Suite apps: iroh live sessions between paired devices, an end-to-end encrypted mailbox for offline devices, sync reconciliation and capped relayed transfers. Product-neutral: the host app supplies identity and trust through `PeerTrust` (Pulse implements it over its device-link registry).

Moved into RightKit from the Pulse repository (`pulse-mesh`, bogusyogi/pulse#138) on 2026-11-01. The whole mesh is OFF by default (`MeshSettings::enabled`); `Mesh::start` refuses to run while it is off.

## What it does

| Part | File | Notes |
|---|---|---|
| Endpoint | `src/endpoint.rs` | iroh 1.3.0, `presets::Minimal` (never `N0`), our relay URL from settings, no address lookup, no DHT. Dial by endpoint id plus our relay; iroh upgrades to a direct path inside the connection. A trust gate closes any connection whose key is not an active paired device. |
| Identity and trust | `src/identity.rs` | The host's device seed becomes the iroh secret key, so the endpoint id is the device's public key. Trust is the host's registry, read through `PeerTrust` on every handshake. No second device list. |
| Mailbox (offline devices) | `src/mailbox/`, `src/bin/rightkit-mesh-mailbox.rs` | ALPN `right/mbox/1`. Deposit, collect, ack, set_allow. The recipient dials out and pulls; the server deletes on ack or after 7 days; only 64 KB items are stored. |
| Seal and keys | `src/seal.rs`, `src/keys.rs` | ChaCha20-Poly1305, HKDF-SHA256 per item, per-pairing per-direction keys exchanged on the first live connection (ALPN `right/keys/1`). ring only. |
| Sync reconciliation | `src/sync.rs` | ALPN `right/sync/1`. "What do you have since X" by record id and version; application supplies a `RecordStore`. |
| Caps and transfers | `src/caps.rs`, `src/transfer.rs` | D7: 25 MB per relayed transfer, 10 MB per relayed clipboard or phone image or chat attachment, 250 MB per circle per month, screens direct-only. Phone images wait for a direct path first (default 5 s), then relay as the capped fallback. |
| Owner circle (D4) | `src/circle.rs` | Introductions add a device with no grants; members may revoke third devices (revoke-only). |
| Settings | `src/settings.rs` | Documented defaults; see `docs/settings.md`, "Device mesh". |



## How it is used

```text
let seed = KeyVault::load_or_create_seed()          // existing device-link identity
let trust = Arc::new(PulseTrust::new(registry_path)) // reads the device-link registry
let mesh = Mesh::start(MeshInit { seed, settings, trust, keys, budget_path, records, bulk }, |router| router).await?;
mesh.exchange_keys(&peer).await?                     // once per pairing, on the first live connection
mesh.reconcile(&peer, &[("messages".into(), cursor)]).await?   // on connect
mesh.send_bulk(&peer, offer, reader).await?          // direct if possible, else capped relay
mesh.mailbox().unwrap().deposit(&peer, seq, bytes).await?      // peer asleep
mesh.mailbox().unwrap().pull(|inbound| ...).await?   // this device woke: dial out and collect
```

## Design notes

- No default public relay. A build with no `relayUrl` cannot start. Nothing here talks to n0 services.
- A path that is not provably a direct IP path counts as relayed, so every cap fails closed. Direct is read from `Connection::paths()` (selected path, `is_ip`).
- The mailbox identifies a caller only by the iroh handshake. A device reads its own mailbox only; a deposit needs the recipient to have allowed the sender (`set_allow`, replaced after every pairing, introduction and revoke).
- Mailbox keys: either side may start the exchange; a simultaneous dial settles by endpoint id (lower id wins). Two epochs are kept so queued items still open after a re-exchange.
- Images and files travel on live streams, not in the mailbox. The relay forwards QUIC ciphertext and stores nothing (D7).
- The protocol shape follows Happy (MIT) as the donor named for the mailbox; its source was not read (see `docs/relay-hetzner.md`).

## Not done, not proven

- Compiles in the Mac gate (workspace member). Not run on two real devices yet: the journey below is the proof.
- No test was added (Pulse's rule is end-to-end journeys only). The journey P10 in the design, on two networks, is the proof, and it needs the relay and mailbox deployed.
- iroh-blobs is not used: files move as plain QUIC streams, so there is no BLAKE3 verification or resume. LAN discovery (mDNS, a separate iroh crate) is not wired; the "find my devices on this network" setting has no effect yet.
- No durable `KeyStore`: `MemoryKeyStore` loses keys at restart (a new exchange at the next live connection). A credential-store implementation is owed.
- The circle budget is per device, not shared; the server cannot count relayed bytes (iroh-relay 1.3.0 has no per-endpoint counter).
- The engine starts the mesh (`engine/src/devicelink_host.rs:2353`, `SyncMesh::start`, settings from `MeshSettings::for_device_link`, line 2293) only while `deviceLinkListen` is on, a pairing is open or a paired device exists; trust is the device-link registry. The `deviceMesh` master key in `settings.rs` is not read by the engine and has no manifest row. No pairing change: pairing still runs over today's LAN channel, so `exchange_keys` and the introduction fan-out have no caller.
- `trust_pulse.rs` builds registry records field by field and records no introducer; protocol v3 (design lane P0) must update it. Android is allowed: the phone FFI declares an `android` joiner (`rightkit-mesh-ffi/src/pairing.rs:52-55`) and the registry lists it (`core/src/devicelink/pairing.rs:339`).
- Phone apps (iOS Notification Service Extension, Android) and push wakes are not part of this crate yet; the mailbox is pull-only.

## Device link (`devicelink` feature)

Since 0.1.1 the crate also carries the device link as `rightkit_mesh::devicelink` (feature `devicelink`, off by default): the Ed25519 device identity, short-code and join-token pairing, the paired-device registry file, the device circle (signed member lists, introductions, endorsements and grants), resume tickets and the per-request capability check (`DeviceLink::authorize`). It is transport-agnostic; `devicelink::transport` runs the pairing over a stream. The record types live in `rightkit-device-link-types`, owner-check verification in `rightkit-approvals`.
