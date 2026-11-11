# rightkit-daemon-remote

The remote side of a Right Suite daemon: the `daemon-api` v1 HTTPS host for enrolled devices (Pulse and PostRight server builds), the blocking device client and `rk-link/1` link a machine with no inbound port uses to reach it, and the `rk-relay/1` blind relay protocol. The local same-user service it builds on (discovery, the current-user-only socket or pipe, the method table, handler and call context) is `rightkit-daemon`.

**Use it** for a server build that serves enrolled devices over TLS (`remote`), for a device that talks to such a host or holds a link to it (`link`), or for the relay's state machine and a device's relay session (`relay`).

**Do not use it** in a desktop app for local IPC: that is `rightkit-daemon` (and, for CodeRight's loopback HTTP wire, its `remote-loopback` feature). A desktop release that enables `remote` is refused by `@rightkit/release` unless the app declares `releaseSwitches.remoteHost: true`.

## Features

No feature is on by default.

- `link`: the blocking device client (`remote::client`: enrol, signed requests, resumable uploads, compare-and-swap job updates), request signing (`remote::signing`), pairing initiator (`remote::pairing`) and the `rk-link/1` client (`link::client::LinkClient`). std sockets and rustls only: no tokio, axum or hyper.
- `relay` (implies `link`): `relay::{RelayCore, RelaySession, issue_ticket, ...}`, transport-free.
- `remote` (implies `link`): the host. `remote::Daemon` (transport-free core: enrolment, signed device routes, uploads, OAuth callback and signed webhook route classes, durable state, lifecycle events, leased tasks over `rk-link/1`, pairing, public routes), `remote::router` (axum), `remote::serve` and `remote::serve_blocking` (TLS with rustls), and `POST /v1/call/{method}` through `remote::DeviceCalls`.

## One handler for local callers and devices

A service's `rightkit_daemon::contract::MethodTable` decides which methods a device may call (`MethodPolicy::and_remote`, `MethodPolicy::remote`), and the same `rightkit_daemon::contract::Handler` serves both transports:

```text
let service = rightkit_daemon::Service::builder()
    .methods(table.clone()).handler(handler.clone())
    .local(&suite, config, HostOptions::new())
    .start()?;
let daemon = Arc::new(Daemon::open(DaemonConfig::new(state_dir), Arc::new(SystemClock), None)?
    .with_calls(DeviceCalls::new(table, handler)));
// serve `daemon` over TLS with remote::serve_blocking
```

Until rightkit-daemon 0.7.0 the builder did the second half itself (`ServiceBuilder::remote`). Give the device side only a registration whose `profile()` is `TransportProfile::LocalAndRemote`.

## Outbound link (`link` / `remote`)

A machine with no inbound port reaches the host over `rk-link/1`: a TLS connection upgraded with a device-signed `GET /v1/link`, then signed, numbered JSON-line frames in both directions. The host leases durable tasks to the device (`Daemon::enqueue_task`); exactly one device owns a task at a time, results are fenced by a lease epoch, a dropped link resumes and the host sends again every leased task the device has not acked, a lapsed lease re-queues once. The device side is `rightkit_daemon_remote::link::client::LinkClient` (blocking, std and rustls only). See the rightkit-daemon `CHANGELOG.md` (0.2.0) for the frame and lease rules.

## Pairing a device by short code (`remote` feature)

`Daemon::open_pairing(PairingOptions)` opens a window in which a device with no credential can run a short-authenticated-string exchange (`rk-pair/1`, the protocol and its argument are in `remote::pairing`): the device commits to its key and a nonce, the host answers with its own nonce, the device reveals, and both derive the same six-digit code from the whole transcript. The owner reads the code off the device and types it into the host (`confirm_pairing`); only a match enrols the committed key. Routes (unauthenticated, outside the daemon-api table like `/v1/link`): `POST /v1/pair`, `POST /v1/pair/{id}/reveal`, `GET /v1/pair/{id}`. Devices use `remote::pairing::PairingInitiator` (feature `link`).

Bounds, all enforced in the daemon: nothing is served unless the owner opened a window; a window expires and serves one pairing; at most `max_pending` sessions at once and `max_begins` in total; a session lives `session_secs`; `max_wrong_codes` wrong codes burn a session and `max_failures` failures close the window. The code is only readable in process (`pairing_code`) and is in no event or response. Pairing state is memory only.

`Daemon::forget_device(id)` (in process, never routed) removes a revoked device and everything keyed by its id so the same key can enrol again as a new device; see its documentation for what is removed and what is kept.

## Blind relay (`relay` feature, `rk-relay/1`)

The protocol pieces for devices that cannot reach each other directly: a relay that stores and forwards sealed bytes it cannot read, and the device side that talks to it. `relay::RelayCore` is the relay's state machine (challenge and ticket authentication, queues, caps, quota; no sockets or clock), `relay::RelaySession` is a device's side, and the cipher between devices is a `Sealer` the caller supplies. What the relay learns: mailbox and circle pseudonyms, who talks to whom, when, and how much. See the rightkit-daemon 0.2.6 changelog for what is not built yet (the cipher, a socket adapter, direct paths, push-wake, durable queues).

## Public routes (`remote` feature)

An application route that needs no device signature (a provider webhook, a callback, a redirect) is registered with `Daemon::add_public_route(method, path, body_limit, handler)`. The method and path must match exactly, the path may not be under `/v1`, and the handler does its own verification over `Request::body`. Public routes are checked before any device check and before the daemon-api table.

## Operator surface (`remote` feature)

`Daemon::list_devices` (every device, revoked ones marked, `linked` while a link is open, never the key), device roles (`role:<name>` scopes granted at enrolment; `Caller::role`, `CallContext::role`, and `MethodPolicy::remote(["role:operator"])` to gate a method), and an event cursor for live screens (`Daemon::events_since`, `wait_events`; every event now carries a `seq`). See the rightkit-daemon 0.2.7 changelog for what issue #306 asks for that is not built.

## Contract

`schemas/daemon-api.v1.schema.json` is the `daemon-api` v1 contract the routes implement, byte-identical to the copy `rightkit-contracts` types (`crates/rightkit-contracts/schemas/`). The repository copy `contracts/daemon-api.v1.schema.json` still lacks the `device_revoked` error code (rightkit#306); that drift predates this crate.

## Runtime

Held-resource (docs/RUNTIME-BUDGETS.md): the listener and its runtime live while `serve_blocking` runs (until `StopHandle::stop`), a link thread while its `LinkHandle` is held. The cost is measured where an app adopts it. `benches/remote_budget.rs` (feature `remote`) drives a real TLS listener with signed job writes.

## Origin

The code moved here unchanged from `rightkit-daemon` 0.6.0, where it was the `remote`, `link` and `relay` modules behind the features of the same names (see CHANGELOG.md).

## License

MIT OR Apache-2.0.
