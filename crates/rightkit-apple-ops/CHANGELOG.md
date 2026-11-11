# Changelog

## 0.2.0 (unpublished)

Taken on the recommended option of the shape review (`docs/audits/2026-11-11-shape-matrix/reports/cut-post-redraw.md`), pending the owner's confirmation.

- Breaking: requires `rightkit-engine-host` 0.2.0; `spec`, `register`, `run` and `wrap` take and return its `CommandSpec`, `CommandRegistry`, `EngineError` and `Handler`. No behaviour change.

## 0.1.2 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.1 (unreleased)

- Added `gate` (effect classification, request normalisation, bound targets and the `Gate` trait a host implements to authorise execute requests), `cli` (bounded JSON input, request shape, result marking) and `command` (the `apple` command for `rightkit-engine-host` registries), moved from Legion's `apple_mcp.rs` and `commands/apple.rs`. Additive.

## 0.1.0 (unreleased)

- First RightKit release, moved from `legion-apple` (engine).
