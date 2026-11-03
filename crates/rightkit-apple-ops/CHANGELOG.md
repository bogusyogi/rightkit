# Changelog

## Unreleased

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.1 (unreleased)

- Added `gate` (effect classification, request normalisation, bound targets and the `Gate` trait a host implements to authorise execute requests), `cli` (bounded JSON input, request shape, result marking) and `command` (the `apple` command for `rightkit-engine-host` registries), moved from Legion's `apple_mcp.rs` and `commands/apple.rs`. Additive.

## 0.1.0 (unreleased)

- First RightKit release, moved from `legion-apple` (engine).
