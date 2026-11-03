# Changelog

## 0.1.1 (unpublished)

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
