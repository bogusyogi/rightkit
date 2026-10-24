# Blind relay for rightkit-daemon (issue #133)

Status: design, 2026-10-16. Author: Sage. Docs only; nothing built or run. Input: Pulse R4 ("linked devices reach each other off the LAN; the relay never sees plaintext").
Questions for Adrian are marked **ASK ADRIAN**. Cloudflare limits and prices are unverified (not fetched); verify before the worker issue starts.

## 1. Decision

Add an opt-in `relay` transport to `rightkit-daemon`. The host daemon and the linked device both dial OUT to a stateless Cloudflare Worker plus Durable Object (DO). The DO pairs sockets and copies opaque bytes. Inside that pipe the two ends run a Noise_IK handshake keyed to static keys authenticated at pairing, then carry the existing `rk-link/1` frames unchanged. The relay holds no account, no device registry and no storage. Authorisation is a host-signed ticket the DO verifies statelessly.

Why this shape, from the code read (read-only):
- `rk-link/1` (`src/link/frame.rs`, `remote/core/link.rs`) is already a transport-neutral, newline-JSON, sequence-numbered, signed/HMAC'd frame stream. Host logic is `Daemon::link_accept` / `link_frame` plus an adapter that pumps lines (`remote/http.rs::run_link`). A relay is a second adapter producing the same `Accepted` and line stream; the core does not change.
- Pairing (`remote/pairing.rs`, `remote/core/pairing.rs`) authenticates the device key to the host with a commit-reveal SAS, bounded attempts, and an opaque `host_binding`. The device authenticates the host only through the TLS certificate it pins (`remote/client/mod.rs`). Off-LAN there is no pinned endpoint, so the host needs its own identity key (3.1). This is the one real gap.
- Every device request and frame today is bound to TLS. Dropping TLS as the trust anchor is safe only if the replacement is mutually authenticated and encrypted end to end. That is Noise, not a "relay token".

## 2. Threat model

The relay operator (us, Cloudflare, or a compromised worker) is assumed hostile to confidentiality and integrity, curious about metadata, and may be unavailable.

| Adversary | Can do | Must not be able to do | Control |
|---|---|---|---|
| Relay / Cloudflare | See room id, IPs, connect times, message sizes and timing, how many devices attach | Read, forge, replay, reorder or inject frames; learn labels, task content, scopes | Noise_IK AEAD; rk-link/1 sequence numbers inside; no keys at the relay |
| Active MITM at relay | Drop, delay, cut; swap which socket pairs with which | Impersonate host or device | IK needs the peer static key from pairing; mismatch closes. Availability is not guaranteed |
| Stranger who learns a room id | Open sockets | Attach to the room, pass auth | Ticket verifies against `host_pub` where `room_id = H(host_pub)`; handshake needs a private static key |
| Stolen/revoked device | Replay an old ticket until expiry | Complete Noise (key in OS store), or stay accepted once revoked | Host checks the device key against `state.devices` (`revoked`) after IK; short ticket TTL |
| Flooder | Create rooms, burn DO time | Unbounded free use | Section 6 |
| Recorded-session replay | Resend bytes | Be accepted | Noise nonces; per-link `link_id` and `n` sequence |
| Compromised device or host | Whatever that device is scoped to | Reach other devices' sessions | Existing per-device scopes (`link`); relay adds no authority |

Out of scope: traffic analysis beyond padding, a malicious OS on a paired device, Cloudflare-wide outage. User copy states the residual leak plainly: the relay cannot read content but can see that two devices talk, and when.

## 3. Keys and identities

### 3.1 Host identity (new)

Add a long-lived host static identity: Ed25519 signing key plus X25519 static key, generated on first run, stored via `rightkit-credentials`, never in `state.json`. Its fingerprint becomes `host_binding` for relay-capable pairings (today a TLS-certificate hash is the suggested binding).

- `pairing_code(..., host_binding)` already takes opaque bytes and the SAS differs if they differ, so a pairing that binds the host static key into the SAS authenticates the host to the device with no extra human step. The device static X25519 key must also join the commitment (today only `public_key` and `label` are framed). That is a protocol bump, `rk-pair/2`.
- Existing paired devices get relay capability by re-pairing. No silent upgrade: a host key learned over TLS-only trust is trust-on-first-use through a channel the relay could sit in front of. **ASK ADRIAN**: acceptable to require re-pairing?

### 3.2 Device identity

Reuse the enrolled Ed25519 device key for signatures; add a separately generated X25519 static key registered in `Device` at pairing. **ASK ADRIAN**: two keys (recommended; one key for both signature and DH is the textbook mistake) or Ed25519-to-X25519 conversion to keep one?

### 3.3 Session crypto

`Noise_IK_25519_ChaChaPoly_SHA256`. Device is initiator (knows `host_x25519` from pairing), host is responder. After the handshake the host learns the device static key, looks it up in `state.devices` (present, not revoked, scope `link`), else closes. Prologue is `"rk-relay/1" || room_id`. The daemon has `ring` only, which has no Noise, so this adds the `snow` crate. **ASK ADRIAN**: new dependency `snow` (recommended) vs hand-rolled Noise on `ring` (not recommended). Run `rustright owns` and the dependency allowlist process first.

## 4. Protocol `rk-relay/1`

The outer layer (what the relay understands) is tiny. Transport: WebSocket over TLS to the worker (TLS here is hygiene, not trusted for secrecy). Binary messages only, max 65,535 bytes (Noise ceiling).

```
wss://<relay>/v1/room/<room_id>?role=host|device
room_id = hex(sha256("rk-relay/1 room" || host_ed25519_pub))
```

1. **Challenge then hello.** DO sends a random `challenge`. First client message within 10 s: `{v:1, role, ...}`.
   - host: `host_pub`, `ts`, signature by the host key over `"rk-relay/1 host" || room_id || ts || challenge`.
   - device: `host_pub`, `device_pub`, `exp`, the host's ticket signature over `"rk-relay/1 ticket" || room_id || device_pub || exp`, plus the device's signature over the challenge (proves it holds `device_pub`).
2. DO checks `sha256(host_pub) == room_id`, verifies signatures and `exp` (max 24 h). It never stores `device_pub` beyond the live socket. Rejections are a generic close code (no oracle).
3. **Attach.** A new host socket supersedes the old one (mirrors link-supersede in `core/link.rs`). Each device socket gets a DO-assigned 8-byte `sid`; the DO tells the host `{type:"peer", sid}` and the device `{type:"ready"}`. No host present: device gets `host_offline` and is held up to 30 s, then closed. Presence is metadata the relay necessarily sees.
4. **Data.** After `ready` every message is opaque. Device to host: DO forwards `sid(8) || bytes`. Host to device: host sends `sid(8) || bytes`, DO strips the prefix. The prefix exists only on the host socket (the host multiplexes devices).
5. **Inside the pipe.** Noise IK messages 1 and 2, then each Noise transport message carries one or more `rk-link/1` lines. The `GET /v1/link` upgrade request (`x-rk-timestamp/nonce/signature`) becomes the first encrypted application message, signed with the device key as today, so `Daemon::link_accept` runs on it unchanged. The per-link HMAC key stays (redundant under AEAD, kept so one code path serves LAN and relay).
6. **Keepalive.** The 20 s `rk-link/1` heartbeat would defeat DO hibernation and bill duration. Use the WebSocket auto-response (`setWebSocketAutoResponse`) for a relay-level ping/pong that does not wake the DO. The inner 60 s dead-peer rule stays.
7. **Padding.** Pad each Noise message to a 256-byte bucket to blunt size leakage. Costs bandwidth, not correctness.

Failure semantics: the relay is lossy and never buffers. Durability already lives on the host: tasks are durable with leases (`Task`, `lease_ttl`, `epoch` fencing). On reconnect the device re-handshakes and existing lease rules apply.

## 5. Rendezvous and path selection

- At pairing the host gives the device `{host_pub, host_x25519, relay_urls[]}`. The URL list is a hint, not a trust anchor (Noise protects against a wrong or malicious relay). Default is our hosted worker; both ends can point at a self-hosted relay.
- Device path order: LAN direct first (`discovery.rs`, pinned TLS as today), relay on failure or no LAN. One path per session, never both.
- Host dials the relay only when relay is enabled and at least one relay-capable device is paired; it then keeps one hibernated socket.
- Host offline: relay returns `host_offline`. No wake or push in v1.

## 6. Relay authorisation and rate limits (no account linkage)

Principle: the relay learns only a per-host pseudonym (`room_id`). A ticket never names an account, licence or email.

Stateless checks in the Worker before any DO is touched: hello size cap, signature validity, `sha256(host_pub) == room_id`, `exp`. A failing request costs no DO time.

Limits enforced in the DO, constants in config so they change without a protocol bump (initial values, tunable):
- 1 host socket, 8 device sockets, 16 unauthenticated sockets per room, 10 s hello deadline.
- Message max 64 KiB; per-socket token bucket 100 msgs/s and 2 MiB/s; excess closes with a retry-after code.
- Per-room transfer cap per day (5 GiB) and connected-time cap.
- New-room creation: Workers rate-limiting binding per IP (10 per hour). A global circuit breaker returns 503 with `Retry-After`.
- Bulk objects are NOT relayed in v1. The daemon's resumable uploads (`UploadLimits`, up to 8 GiB) stay LAN or direct HTTPS. The relay is a control and small-payload path.

Free-riding beyond that: v2 option is an anonymous allowance token (Privacy-Pass style, blind-signed by the licence service so redemption cannot be linked to a licence). **ASK ADRIAN**: free for every install, or Pro-gated? Presenting the offline-verifiable licence token would link a room to a licence id (metadata linkage, no plaintext); blind tokens avoid that at build cost. Recommendation: free with these limits for v1; revisit on observed abuse.

## 7. Hosting

- One Cloudflare Worker (stateless verify and route) plus one DO class `Room` on the hibernatable WebSocket API. No SQLite, D1, R2 or persisted DO storage, so there is nothing to leak or erase. DO id = `idFromName(room_id)`.
- Observability: counters only (`attach`, `reject:<reason>`, `bytes`, `rate_limited`). No payloads, no `device_pub`, we log no IPs (Cloudflare edge logs are theirs; say so in copy).
- Cost (unverified): scales with request count and DO wall-clock while awake. With hibernation and auto-response pings an idle host socket should be near-free. Verify against current pricing in the worker issue.
- Self-host: the protocol is small enough that a Rust reference relay (axum is already a `remote` dependency) doubles as the in-process test relay and a self-host binary.
- Worker location: under `infrastructure/` (exists at repo root; check its ownership rules first). Capability record `daemon-relay`, owner `rightkit-daemon`. The worker is not a crate.

## 8. Integration with rightkit-daemon (shape only; another agent is editing these files)

- Features: `relay-client` (implies `link`; blocking WebSocket plus `snow`, no tokio) and `relay-host` (implies `remote`). `link` must stay free of tokio/axum/hyper (invariant stated in `remote/mod.rs`). `default` unchanged.
- Modules: `src/link/relay.rs` (shared outer protocol, hello, Noise wrapper), `src/link/relay_client.rs`, `src/remote/relay.rs` (host adapter feeding `Daemon::link_accept` through the same `Accepted` type `http.rs::run_link` uses).
- State: add `Device.x25519` (state version bump) and the host identity store. No other `Daemon` core change.
- The local same-user transport (`transport.rs`, `peer.rs`, `host_identity.rs`) authenticates a local peer by OS identity and is a different trust class. Relay must not reuse `HostExpectation`; the README should say so.
- Events: `RELAY_UP`, `RELAY_DOWN`, `RELAY_REJECT` in `events.rs`, content-free.

## 9. Test plan (for the issues; nothing was run for this design)

Vectors for `room_id`, ticket and hello. Noise IK known-answer vectors. In-process relay with fault injection (drop, reorder, duplicate, truncate, swapped sockets). A golden transcript test proving a relay that logs every byte cannot recover any task payload. Revoked-device rejection after handshake, ticket expiry, host supersede, rate-limit closes. The live path through a deployed worker is a guarded QA step on the owning host, not CI.

## 10. Open questions (summary)

1. **ASK ADRIAN** Relay free for all installs, or Pro-gated; blind tokens now or later (6).
2. **ASK ADRIAN** Two device keys vs conversion (3.2).
3. **ASK ADRIAN** `snow` as a new dependency (3.3).
4. **ASK ADRIAN** Re-pairing required for relay capability (3.1).
5. v1 payload scope: link tasks and events only (recommended), or tunnel HTTP (OAuth, uploads) over the same Noise stream later.
6. Who owns the production worker deployment and secrets (which Cloudflare account)? HeardRight's `model-serve` worker is a separate project.

## 11. Issues to file (ordered)

1. **rightkit-daemon: host identity key** - Ed25519 + X25519 host keys via rightkit-credentials; fingerprint API; state version bump.
2. **rightkit-daemon: rk-pair/2** - bind device and host static keys into commitment and SAS; `Device.x25519`; rk-pair/1 stays for LAN-only.
3. **rightkit-daemon: `rk-relay/1` spec** - outer protocol, ticket format, vectors, error codes; decision record for 3.2 and 3.3.
4. **rightkit-daemon: relay core (shared)** - room id, ticket sign/verify, hello, Noise IK wrapper, padding; pure functions with vectors.
5. **rightkit-daemon: in-process reference relay** - Rust relay with the section 6 limits as test fixture and self-host binary; fault injection.
6. **rightkit-daemon: relay device client (`relay-client`)** - blocking dial, handshake, `rk-link/1` over Noise, LAN-first path selection.
7. **rightkit-daemon: relay host adapter (`relay-host`)** - outbound dial, sid multiplexing, supersede, `link_accept` feed, events, revocation check.
8. **rightkit-relay-worker: Worker + Room DO** - stateless verify, hibernating DO, auto-response ping, rate limits, counters; verify current limits and pricing first.
9. **rightkit-daemon: relay negative and QA tests** - "relay sees nothing" golden transcript, fault matrix, revocation, expiry; guarded live test.
10. **docs: ownership, privacy copy, threat model** - `daemon-relay` capability record; metadata disclosure text; daemon README.
11. **rightkit-relay-worker: abuse controls v2 (deferred)** - anonymous allowance tokens, only if section 6 limits prove insufficient.
