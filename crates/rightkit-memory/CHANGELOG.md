# Changelog

## Unreleased

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.
- Removed the `fake-membrane` test-double binary and `tests/membrane_fake_binary.rs` (fake, not end to end).

## 0.1.1

- Add `agent_host` feature: modules api_memright, blueprint_tools, memory, memory_provenance, memory_save_tool (moved from an agent host binary).

## 0.1.0 (unreleased)

- First RightKit release, moved from `coderight-memory` (engine).
