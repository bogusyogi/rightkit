# Changelog

## 0.1.1 (unpublished)

- `parse_capability`: `list_mcp_sources` is a read-class tool like `list_mcp_tools` (rightkit#1245), so the new tool is not treated as an unknown, gated class.
- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- Feature `legion`: effect adapter and authorization receipts binding agent policy decisions to sandbox effects, moved from CodeRight `coderight-legion-policy`.
- First RightKit release, moved from `coderight-policy` (engine).
