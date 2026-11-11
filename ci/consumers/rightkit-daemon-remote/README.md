# rightkit-daemon-remote 0.1.0 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-daemon-remote` by exact registry pin
(`=0.1.0`) with the `remote` and `relay` features (`link` comes with both), and on `rightkit-daemon =0.8.0` with
default features for the method table, handler and call context the device route dispatches to. It has no path,
Git, workspace or patch dependency and no committed `Cargo.lock`. Extra dependencies: `ring = "0.17"` (the
Ed25519 keys that sign device requests and relay tickets) and `serde_json = "1"`.

Exercises the daemon host in process (its core is transport-free): `Daemon::open` with `DeviceCalls` over a
rightkit-daemon `MethodTable` and `Handler`, an enrolment code, a signed enrolment (`POST /v1/devices`), a signed
`POST /v1/call/echo` that reaches the handler as `Caller::Device` with its role, the refusals of a local-only
method (403), an unsigned request and a replayed nonce (401), a public route and the refusal of one under `/v1`,
`list_devices`, `metrics`, and `revoke_device` with the `device_revoked` answer after it. Then one `rk-relay/1`
round trip through `RelayCore` between two `RelaySession`s of one circle: ticket authentication, a sealed `Put`
stored and delivered live, the de-duplicated second delivery, and the acknowledgement.

Not covered: the TLS listener (`remote::serve`, `serve_blocking`), uploads, OAuth and webhook routes, pairing,
the `rk-link/1` link client, relay persistence and quotas.

State: one per-run directory under the OS temp directory, removed on exit. No listener, no network, no real user
data.

Proof row: public API surface of the new crate (moved from `rightkit-daemon` 0.6.0 `remote`, `link` and `relay`).
Each check prints an `ok:` line; a failed check prints `FAIL:` and exits 1.
