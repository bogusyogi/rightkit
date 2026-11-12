# rightkit-agent-server

Agent server host: daemon API routes, CLI and interactive-session runtime and session hosting for coding-agent applications.

Call `run(identity, skill_catalog, skill_host)` from a thin binary. Moved into RightKit from the CodeRight engine binary on 2026-11-01.

## Host identity

The library holds no product's names. The app passes a `HostIdentity` and every product name the server writes comes from it: the command name in messages and help, the state folder (`.{app}` in the workspace and the home directory), the `~/.config/{app}` folder, wire schema ids (`{app}.context-status.v1`), HTTP header names (`x-{app}-capability-token`), hash domains, the MCP server name, the licence app id, the Github sign-in credential namespace and client id, the provider referer, and the harness-radar watch-list.

```rust,ignore
let identity = HostIdentity::new("myapp")          // everything derives from the name
    .with_github_client_id("<public client id>")   // optional: Github sign-in
    .with_homepage("https://example.com/myapp")    // optional: provider attribution
    .with_harness_radar(watch_list);               // optional: /runtime/harness-radar
rightkit_agent_server::run(identity, skill_catalog, skill_host);
```

An app that keeps existing data passes its old name and gets the same paths, ids, headers and digests as before. In this crate's docs and help text `{app}` stands for the app name.

## Features

- `benchmarks` (off by default): the evaluation harness (`/runtime/benchmark`, `/runtime/compare`, the terminal-bench task pack, the executed and corpus bake-offs, the soak run) and the benchmark history. Without it those routes do not exist and `/benchmarks` reports none recorded.

It holds no private copy of a shared crate: the GitHub App and sign-in are `rightkit-github`'s, the schedule model and bot profiles `rightkit-scheduler`'s, the Telegram transport `rightkit-bot-messaging`'s, the host observation shapes `rightkit-context-protocol`'s. What still sits here but is planned elsewhere (scheduler runner, workflow host, engine, agents, llm, orchestration, sandbox and evidence code) is listed in docs/audits/2026-11-11-shape-matrix/reports/agent-server-redraw.md.

## Licensing (Pro entitlement)

The daemon decides Pro itself and never reads the owner's credential store. The owner app (the product shell, the only binary that reads the OS store) builds a snapshot with `rightkit_license::LicenseClient::engine_snapshot` and delivers it; the daemon holds it in an `EngineSnapshotCell` and verifies it again with `EngineEntitlements::from_snapshot` (suite key, the host's licence app, the device binding the snapshot names, expiry, and the owner's anti-rollback ledger, read-only).

Owner-side contract:

- Launch the daemon with the licence locators (all non-secret; every `RIGHTKIT_LICENSE_*` variable is read once and removed from the daemon's environment): `RIGHTKIT_LICENSE_DIR=<app data>/license` (the ledger `clock.json` lives there; without it the daemon runs no gate and is Free), `RIGHTKIT_LICENSE_OWNER_EXE=<absolute path of the owner executable>`, and the owner's signing identity, `RIGHTKIT_LICENSE_OWNER_TEAM=<Apple team id>` on macOS and/or `RIGHTKIT_LICENSE_OWNER_SIGNER=<Authenticode signer common name>` on Windows. Without the owner executable and at least one signing identity the daemon hosts no delivery endpoint and stays Free; it never accepts an unsigned owner. The owner's client must keep its anti-rollback ledger as the file ledger, not the credential-store ledger.
- Deliver the snapshot over the daemon's private IPC (rightkit#1501): connect to service `engine-license-<daemon port>` with `rightkit_daemon::Client::connect_expecting` (the daemon's own executable), say `hello` as the host's licence app (`HostIdentity::license_app`, by default the app name), and call `license.snapshot` (`rightkit_daemon::LICENSE_SNAPSHOT_METHOD`) with `{"frame": "<standard base64>"}`, the frame being one `rightkit_credentials::handoff` frame written with `write_message(&Message::reply_snapshot(id, client.engine_snapshot()))`. The result `{"ack": "<base64>"}` is the Ack frame; check it with `expect_ack(read_message(..), id)`. The daemon identifies your process first (same user, the owner executable pinned by pid and start time, and its code signature) and answers `forbidden` to anything else; `invalid_params`: not exactly one Reply frame (nothing changed). The old `POST /internal/license-snapshot` route no longer exists.
- Deliver once the daemon is reachable (right after launch) and again after every `LicenseInvalidation` (activation, refresh, deactivation, revocation). Each delivery replaces the whole snapshot.
- Until the first delivery, and after an owner read failure or a malformed snapshot, the daemon is not Pro. An owner that cannot read the licence should still deliver, with `licence.token` as a read failure, so the daemon fails closed at once.
- The daemon accepts no verdict, feature list, key or device id from any other source. Only the owner's own signed process may deliver.
