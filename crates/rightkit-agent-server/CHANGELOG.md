# Changelog

## 0.1.2 (unpublished)

- Requires `rightkit-agent-engine-protocol` 0.2.0 (its approval records carry the approvals core's `Binding`, a mandatory digest, instead of an optional `payload_digest`) and `rightkit-agent-engine-core` 0.1.1. "Approve for session" on a tool with no command always installs the payload grant: every pending approval now carries its core binding, so the branch that approved once without a grant (no digest) is gone. Decision 4 of the 2026-11-11 shape review (docs/audits/2026-11-11-shape-matrix/README.md, "Agent engine") taken on the recommended option, pending the owner's confirmation; design note: docs/audits/2026-11-11-shape-matrix/reports/agent-engine-contract-design.md.

## 0.1.1 (unpublished)

- Follows its dependencies' current API (commit 87c88e95, rightkit#1560): `host_agent_id` on `DriverBindingRow`, `SpawnRuntimeKind::HostNative`, `ensure_host_mcp_registered` and `HOST_MCP_SERVER_NAME` (the MCP server is `rightkit`, the Claude tool prefix `mcp__rightkit`), the five-field unauthenticated `/health`, the budget fields read from the persisted config, the hooks dry run over an `IdentifiedHook` list, the generic `ImportRequest`, the new `PluginError` and `ToolPolicyEvent` variants, `try_update` for `fetch_update`. The isolation-mode parser accepts both `exclusive_rightkit_gateway` and the stored legacy `exclusive_coderight_gateway`.
- `DriverRuntimeEvent::Normalized` carries a `Box<NormalizedEvent>` (clippy::large_enum_variant; the dependency boxes the same payload in `CodexMessage::Notification`). Internal type; the library's public API is otherwise unchanged.
- Fix, data loss: saving a workspace-scoped local-model route (`POST` provider route, workspace scope) and the toolset saves read their starting config from `<workspace>/.coderight/config.toml` but `ConfigLoader::update_workspace_from` writes `<workspace>/.rightkit/config.toml`; with no `.coderight/config.toml` the save started from `HostConfig::default()` and replaced the whole `.rightkit/config.toml` with defaults plus the one change. Both now read, write and report `<workspace>/.rightkit/config.toml` (the file `load_files_for_workspace` reads), and the settings config-status listing names that path. No migration of a `.coderight/config.toml`: app-settings never read it.
- Fix: the workspace task listing reads `<workspace>/.rightkit/tasks.json`, the file the `task_tools` of rightkit-agent-tools write (it read `.coderight/tasks.json`, which nothing writes).
- Dependency requirements raised to the versions that contain the APIs above: `rightkit-agents` 0.2.4, `rightkit-agent-tools` 0.3.3, `rightkit-agent-engine-storage` 0.1.1, `rightkit-agent-engine-protocol` 0.1.1, `rightkit-agent-engine-api-platform` 0.1.1, `rightkit-agent-plugins` 0.1.1, `rightkit-hooks` 0.2.3, `rightkit-logs` 0.3.9.
- `list_mcp_sources` (rightkit#1245) joins the MCP gateway tool list (`--no-mcp` removes it), the TUI render classes and the `research-gateway`, `mcp-discovery` and default read-only toolsets, next to `list_mcp_tools`.
- Removed the whole `tests/` tree, the `coderight` and `ops004_driver_fixture` harness binaries, the `e2e` test target, the `e2e-harness` feature and the dev-dependencies (`base64`, `tiny_http`, `portable-pty`, `vt100`, the test copy of `rightkit-app-settings`) (Adrian, 2026-11-03: "My decision was no unit tests. Only e2e."; "no dummy data, no fucking made up shit. real data."). Every file was cargo-driven against fakes (the scripted and mock LLM providers, `FakeMembrane`, `FakeHttp`), an isolated temporary suite root (the `test_isolation` module no longer exists) and a harness binary with empty skill services, so none drove the real installed app. The real end-to-end run is the owner app's `rightkit-qa` journey against the installed build, under snapshot and restore. The library API is unchanged.
- Removed test-only code: `NodeDispatchContext::for_test` and its `Default` impl (Adrian, 2026-11-03: "make sure no debug assertions in their respective apps/ repos"; "My decision was no unit tests. Only e2e."; "dev mode, is mine. im the fucking owner, my data. no dummy data ... real data. snapshot and restore."). Release behaviour unchanged.

- rightkit#1501: the licence snapshot is delivered over the daemon's private IPC and the loopback HTTP route `/internal/license-snapshot` (bearer token only, no peer identity) is retired. New module `license_ipc` hosts service `engine-license-<port>` with method `license.snapshot` (`rightkit_daemon::LICENSE_SNAPSHOT_METHOD`, allowed for app `coderight`); its handler calls `LicenseGate::deliver_from_owner(cx, &policy.host_expectation(), frame)`, which runs the daemon's peer check (same user, the owner's executable pinned by pid and start time, and the code signature from `PeerPolicy`) before a byte of the frame is read. `DeliveryError::Unauthenticated` is the `forbidden` wire error. `LicenseGate::deliver` is now private. Launch locators: `LicenseLocators` gains `owner: Option<OwnerLocator>` read from `RIGHTKIT_LICENSE_OWNER_EXE` plus `RIGHTKIT_LICENSE_OWNER_TEAM` (macOS team id) and/or `RIGHTKIT_LICENSE_OWNER_SIGNER` (Windows signer); they carry the licence prefix, so they are removed from the environment with the rest. Without an executable and a signing identity no service is hosted and the daemon stays Free. The owner side (CodeRight) delivers `reply_snapshot` after launch and after every licence invalidation, and must pass the new locators.

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0

- Initial release: engine binary library moved from CodeRight.
- Licensing: the daemon no longer reads the owner's credential store (preflight C3-05). It holds an
  `EngineSnapshotCell` and verifies the owner's snapshot with `EngineEntitlements::from_snapshot`
  over the owner's read-only anti-rollback ledger. The owner delivers the snapshot as a
  `rightkit_credentials::handoff` Reply frame to `POST /internal/license-snapshot` (per-launch
  bearer token) and gets the Ack frame back. Until the first delivery the gate is not Pro.
- The only licence launch locator is `RIGHTKIT_LICENSE_DIR`; `LicenseLocators` lost the credential
  service and account fields. `LicenseGate::open` no longer opens any credential store, and
  `with_parts` is replaced by `with_config`. The `rightkit-device` dependency is dropped and
  `rightkit-license` is used with `engine-snapshot` instead of `client`.
