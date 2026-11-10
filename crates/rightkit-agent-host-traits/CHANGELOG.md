# Changelog

## 0.1.3

- Publishes the source changes made since 0.1.2 was published under the same version number (src/skill_host.rs).

## 0.1.2 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.1

- `skill_host`: data model (slash commands, custom commands, skill manifests, triggers, inline shell) and the `SkillHost` trait a host installs; additive.

## 0.1.0 (unreleased)

- First RightKit release: the `SkillCatalog` host trait and its installation point, so agent crates take skills from the host.
