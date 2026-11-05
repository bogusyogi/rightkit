# Changelog

## Unreleased

- Depends on `rightkit-agent-orchestration` with its `coding` feature and imports `diff_walkthrough`, `small_model_utility`, `teams`, `goal_loop`, `session_recap` and `watchdog` from `coding::`, where they live (it did not compile).

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- Synced with CodeRight: the prepaid token-plan credential is removed (`ProviderConfig::token_plan`, `MobileRuntime::token_plan_credential` / `update_token_plan_credential`, the two `host_mobile_runtime_*token_plan_credential` FFI functions, `client_for_with_token_plan`); the image-provider key is again taken from any exact image endpoint.
- First RightKit release, moved from `coderight-mobile-runtime` (CodeRight).
- Skills come from the host: `host_mobile_skills_catalog_list` and `host_mobile_skills_catalog_search` ask the host-installed `rightkit_agent_host_traits::SkillCatalog`; `scope` is now a free-form string and the list is empty when no catalog is installed.
