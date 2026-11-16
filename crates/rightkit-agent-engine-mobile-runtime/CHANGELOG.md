# Changelog

## 0.1.1 (unpublished)

- Requires `rightkit-mcp` 0.9.0 (agent MCP server options, bearer config, prompts and the CodeRight sync; see its changelog).
- Requires `rightkit-llm` 0.6.0 (provider host settings; see its changelog). No behaviour change.
- Depends on `rightkit-memory-client` 0.1.0, CodeRight's memory client under its new name (it was published as `rightkit-memory` 0.1.x; that name is now the memory engine). No behaviour change. Taken on the recommended option of the 2026-11-11 shape review, pending the owner's confirmation.
- Requires `rightkit-agent-engine-protocol` 0.2.0 (its approval records carry the approvals core's `Binding`, a mandatory digest, instead of an optional `payload_digest`) and `rightkit-agent-engine-core` 0.1.1. Decision 4 of the 2026-11-11 shape review (docs/audits/2026-11-11-shape-matrix/README.md, "Agent engine") taken on the recommended option, pending the owner's confirmation; design note: docs/audits/2026-11-11-shape-matrix/reports/agent-engine-contract-design.md.
- Depends on `rightkit-agent-orchestration` with `coding-core` instead of `coding`: it imports only `teams`, `small_model_utility`, `goal_loop`, `session_recap`, `diff_walkthrough` and `watchdog`, none of which needs `rightkit-repo-graph`, so the tree-sitter grammars and the embedding runtime leave its dependency tree. No public API or behaviour change.
- The OS credential store is no longer linked: the crate no longer names `rightkit-llm`'s `agent_providers-os-keyring` or `rightkit-mcp`'s `agent_mcp-os-keyring`, and requires rightkit-agent-coding-tools 0.1.8, rightkit-agent-orchestration 0.1.6 and rightkit-agent-engine-core 0.1.3, which leave it out too. With default features the dependency tree has no `*-os-keyring` feature, no `rightkit-credentials` and no `keyring` package, which an iOS app needs. New opt-in feature `os-keyring` brings it back for a host that has a credential store. No public item changes.

## Unreleased

- Depends on `rightkit-agent-orchestration` with its `coding` feature and imports `diff_walkthrough`, `small_model_utility`, `teams`, `goal_loop`, `session_recap` and `watchdog` from `coding::`, where they live (it did not compile).

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- Synced with CodeRight: the prepaid token-plan credential is removed (`ProviderConfig::token_plan`, `MobileRuntime::token_plan_credential` / `update_token_plan_credential`, the two `host_mobile_runtime_*token_plan_credential` FFI functions, `client_for_with_token_plan`); the image-provider key is again taken from any exact image endpoint.
- First RightKit release, moved from `coderight-mobile-runtime` (CodeRight).
- Skills come from the host: `host_mobile_skills_catalog_list` and `host_mobile_skills_catalog_search` ask the host-installed `rightkit_agent_host_traits::SkillCatalog`; `scope` is now a free-form string and the list is empty when no catalog is installed.
