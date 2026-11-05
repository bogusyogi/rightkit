# Changelog

## Unreleased

- Depends on `rightkit-agent-orchestration` with its `coding` feature and imports `diff_walkthrough`, `small_model_utility`, `teams`, `goal_loop`, `session_recap` and `watchdog` from `coding::`, where they live (it did not compile).

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `coderight-mobile-runtime` (CodeRight).
- Skills come from the host: `host_mobile_skills_catalog_list` and `host_mobile_skills_catalog_search` ask the host-installed `rightkit_agent_host_traits::SkillCatalog`; `scope` is now a free-form string and the list is empty when no catalog is installed.
