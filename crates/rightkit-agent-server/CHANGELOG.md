# Changelog

## Unreleased

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
