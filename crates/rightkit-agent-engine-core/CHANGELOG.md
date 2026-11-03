# Changelog

## Unreleased

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `coderight-engine` (CodeRight).
- Skills come from the host: skill triggers, hints and effect enforcement take the skills a host-installed `rightkit_agent_host_traits::SkillCatalog` reports as triggered; with no catalog installed no skill triggers.
