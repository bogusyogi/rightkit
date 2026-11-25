# Changelog

## 0.2.2

- Logic moved from Pulse into this crate (bogusyogi/pulse#643): see the module docs for what each new module holds.

## 0.2.1

- Requires `rightkit-hooks` 0.3.0.
- Fix (`deliver_claude::defuse_tags`): a closing tag was looked for only in the next few characters, so a long body let a forged closing tag through; the look-ahead is now 4096 characters and compares only the closing tag's length (bogusyogi/pulse#278).
- `claude_chats`: `touches_account`, `AccountWatch` and `run_account_switch`, so a host follows a Claude account switch (moved from Pulse, bogusyogi/pulse#308).
- `chat`: the fixes Pulse made to its copy after the port, carried in: #461, #520, #582, #612, #615, #619, #621, #623, #627, #638, the chat trace (`trace`, `trace_view`, `chat.trace`), stale-recipient refusal, Linux headless hub, and the `harness-vectors` feature.

## 0.2.0

- Breaking for dependents: dependency requirements `rightkit-app-settings` 0.3 -> 0.4, `rightkit-daemon` 0.5 -> 0.8, `rightkit-logs` 0.3 -> 0.4. 0.1.1 was published before these requirements were raised; a semver-breaking dependency is a breaking change, so they ship as 0.2.0 instead of changing 0.1.1 in place (#1572). No source change in this crate since 0.1.1.

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from Pulse (pulse-core chat/ and claude_chats.rs).
