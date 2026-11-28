# rightkit-agent-server

Agent server host: daemon API routes, CLI and interactive-session runtime and session hosting for coding-agent applications.

Call `run(identity)` from a thin binary. Moved into RightKit from the CodeRight engine binary on 2026-11-01; CodeRight's current binary (36356aa9b) is merged in (0.3.0); 0.3.1 closes the review of that merge (see the changelog).

## Host identity

The library holds no product's names. The app passes a `HostIdentity` and every product name the server writes comes from it: the command name in messages and help, the state folder (`.{app}` in the workspace and the home directory), the `~/.config/{app}` folder, wire schema ids (`{app}.context-status.v1`), HTTP header names (`x-{app}-capability-token`), hash domains, the MCP server name, the licence app id, the Github sign-in credential namespace and client id, the provider referer, and the harness-radar watch-list.

```rust,ignore
let identity = HostIdentity::new("myapp")          // everything derives from the name
    .with_github_client_id("<public client id>")   // optional: Github sign-in
    .with_homepage("https://example.com/myapp")    // optional: provider attribution
    .with_harness_radar(watch_list);               // optional: /runtime/harness-radar
rightkit_agent_server::run(identity);
```

An app that keeps existing data passes its old name and gets the same paths, ids, headers and digests as before. In this crate's docs and help text `{app}` stands for the app name.

Before `run`, the host takes the steps listed in the crate documentation, in order: it names its environment prefix, state folder and display name (`rightkit_agent_config::ensure_host_names`; the identity's state folder must be the same), sets the configuration conventions (`set_budget_defaults`, `set_retired_config_keys`, `set_hardened_config_loader`, `set_hardened_instructions`, `set_nearest_ancestor_trust`), calls `rightkit_agent_server::scrub_inherited_licence_env()`, sets the options of the shared crates (including the skill catalog of the tools crate, which `run` does not install) and this crate's `host_options`. `run` refuses to start when the identity's state folder differs from the configured one. `with_version(env!("CARGO_PKG_VERSION"))` makes the server report the host's version.

## Host options

`host_options` holds the conventions one host wants and another may not (guest links and the LAN listener, the `rpc`, `ssh`, `acp-server` and `contract` commands, the cloud workspace routes, the plugin marketplace, the Adapt routes, the peer-team routes, the Membrane background review route, the legacy `--auto-approve` flags, bundled skills, tool registry controls, `/web` client files, and smaller ones; each setter is documented in the module). Each is set once, before `run`, and reads as this crate's own behaviour when nobody sets it. Security-relevant fixes are not options: they apply to every host.

`data/option-gated-routes.json` lists, per option, the method and path of every route that exists only while the option is on, the contract operation ids of those that the contract documents, and each option's default. The server builds its contract from that file, and a unit test keeps it in step with the endpoint tables. A tool that cannot run this code (a client generator, a contract check) reads the same file: it takes the endpoint tables under `src/api_contract/endpoints/`, then drops every route of each option whose value for the host is `false` (an option the host does not set has the `default` in the file).

## Features

- `state-store-env` (on by default): the environment may move the state stores (`<PREFIX>_{TEAM,MESSAGE,INSTINCTS,PERMISSION_RULE,PERMISSION_DENIAL,WORKSPACE_TRUST}_STORE`, `<PREFIX>_RUN_CHANGES_DIR`, `<PREFIX>_TELEGRAM_BINDINGS`, `<PREFIX>_PEER_TEAM_STORE_ROOT`), subject to `host_options::state_store_paths_from_environment`. Without it none of those names or reads is compiled in, the workspace trust rule is not installed, and every store sits under the state root; the option's setter still returns a value but it has no effect.
- `workflow-a2a-env` (on by default): the environment may configure a remote A2A workflow executor (`<PREFIX>_A2A_TRUST_ROOT_PUBKEY`, `<PREFIX>_A2A_DESCRIPTOR_JSON`, `<PREFIX>_A2A_EXECUTOR_URL`). Without it those names, the reads, the trust-root verifier and the remote transport are not compiled in, and workflows never get an A2A adapter.
- `benchmarks` (off by default): the evaluation harness (`/runtime/benchmark`, `/runtime/compare`, the terminal-bench task pack, the executed and corpus bake-offs, the soak run) and the benchmark history. Without it those routes do not exist and `/benchmarks` reports none recorded.

It holds no private copy of a shared crate: the GitHub App and sign-in are `rightkit-github`'s, the schedule model and bot profiles `rightkit-scheduler`'s, the Telegram transport `rightkit-bot-messaging`'s, the host observation shapes `rightkit-context-protocol`'s. What still sits here but is planned elsewhere (scheduler runner, workflow host, engine, agents, llm, orchestration, sandbox and evidence code) is listed in docs/audits/2026-11-11-shape-matrix/reports/agent-server-redraw.md.

## Licensing (Pro entitlement)

The daemon decides Pro itself and never reads the owner's credential store. The owner app (the product shell, the only binary that reads the OS store) builds a snapshot with `rightkit_license::LicenseClient::engine_snapshot` and delivers it; the daemon holds it in an `EngineSnapshotCell` and verifies it again with `EngineEntitlements::from_snapshot` (suite key, the host's licence app, the device binding the snapshot names, expiry, and the owner's anti-rollback ledger, read-only).

Owner-side contract:

- Launch the daemon with the licence locators (all non-secret; every `RIGHTKIT_LICENSE_*` variable, and the licence-named variables under the host's own prefix, is read once and removed from the daemon's environment): `RIGHTKIT_LICENSE_DIR=<app data>/license` (the ledger `clock.json` lives there; with `host_options::license_file_clock` on, the default, a daemon without it is Free; a host that turns the option off ignores the folder and relies on the mark delivered in the snapshot), `RIGHTKIT_LICENSE_OWNER_EXE=<absolute path of the owner executable>`, and the owner's signing identity, `RIGHTKIT_LICENSE_OWNER_TEAM=<Apple team id>` on macOS and/or `RIGHTKIT_LICENSE_OWNER_SIGNER=<Authenticode signer common name>` on Windows. Without the owner executable and at least one signing identity the daemon hosts no delivery endpoint and stays Free; it never accepts an unsigned owner. The owner's client must keep its anti-rollback ledger as the file ledger, not the credential-store ledger.
- Deliver the snapshot over the daemon's private IPC (rightkit#1501): connect to service `engine-license-<daemon port>` with `rightkit_daemon::Client::connect_expecting` (the daemon's own executable), say `hello` as the host's licence app (`HostIdentity::license_app`, by default the app name), and call `license.snapshot` (`rightkit_daemon::LICENSE_SNAPSHOT_METHOD`) with `{"frame": "<standard base64>"}`, the frame being one `rightkit_credentials::handoff` frame written with `write_message(&Message::reply_snapshot(id, client.engine_snapshot()))`. The result `{"ack": "<base64>"}` is the Ack frame; check it with `expect_ack(read_message(..), id)`. The daemon identifies your process first (same user, the owner executable pinned by pid and start time, and its code signature) and answers `forbidden` to anything else; `invalid_params`: not exactly one Reply frame (nothing changed). The old `POST /internal/license-snapshot` route no longer exists.
- Deliver once the daemon is reachable (right after launch) and again after every `LicenseInvalidation` (activation, refresh, deactivation, revocation). Each delivery replaces the whole snapshot.
- Until the first delivery, and after an owner read failure or a malformed snapshot, the daemon is not Pro. An owner that cannot read the licence should still deliver, with `licence.token` as a read failure, so the daemon fails closed at once.
- The daemon accepts no verdict, feature list, key or device id from any other source. Only the owner's own signed process may deliver.
