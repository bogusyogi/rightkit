# Changelog

## 0.2.0 (unpublished)

Taken on the recommended option of the shape review (`docs/audits/2026-11-11-shape-matrix/reports/cut-post-redraw.md`), pending the owner's confirmation.

- The one engine doctor report: `rightkit-engine-host` 0.2.0 deletes its own `doctor` module (a second report envelope and capability list), so `report` here is the only one. Requires `rightkit-engine-host` 0.2.0 (breaking: `command` takes and returns its `CommandRegistry` and `Handler`).
- `DoctorHost::resource_dir` defaults to `rightkit_engine_host::env::resource_dir`, the rule engines resolve resources by: a macOS executable outside an `.app` bundle and a Linux executable now report no resource folder (`null`) instead of the executable's folder.

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release: verdict and row check models, the engine doctor report, the `doctor` command for `rightkit-engine-host`, neutral probes and host-folder helpers, moved from the Legion, CodeRight, PostRight, GenRight, CutRight and MailRight doctors.
