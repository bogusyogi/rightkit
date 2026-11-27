# Changelog

## 0.1.2 (unpublished)

A superset of CodeRight's private `coderight-mobile-runtime` 0.1.0, so that crate can become a thin static library over this one. **Defaults unchanged** for a host that sets none of the new once-per-process options (module `host_options`; each setter returns `false` and changes nothing when a choice was already made, the first call winning), with the exceptions listed under "Stricter for every host". Every existing `host_mobile_*` C symbol keeps its name and signature; no existing Rust item is removed or re-typed.

New in the Rust API:

- Every C entry point (`ffi`, `team_fleet`, `wasm`) is now also an ordinary Rust function named like its symbol without the `host_mobile_` prefix (`ffi::runtime_new` for `host_mobile_runtime_new`, `ffi::string_free`, ...), carrying the doc and `# Safety` contract; the `host_mobile_*` symbol is a `#[no_mangle]` one-line wrapper around it. A host static library can export its own symbol names by wrapping the Rust functions. New default-on feature `c-abi` gates the crate's own `host_mobile_*` symbols: a host that exports its own names depends on this crate with `default-features = false`, so no second copy of the symbols is linked. `wasm::ExecuteFn`, `wasm::FreeFn`, `ffi::WebkitRenderFn` and `ffi::WebkitFreeFn` are public; `wasm::install_wasm_executor` and `ffi::install_webkit_renderer` are `unsafe fn`s (their callbacks are called from Rust with pointers Rust owns); the C symbol `host_mobile_install_wasm_executor` stays declared without `unsafe`, as before.
- New C symbols: `host_mobile_runtime_resolve_question`, `host_mobile_runtime_resolve_plan`, `host_mobile_audit_log_read`, `host_mobile_install_webkit_renderer`; Rust `MobileRuntime::resolve_question` / `resolve_plan`, `audit_log::read_all`, `llm::assist_client_with_response_format`.
- `RuntimeConfig` gains `bundled_skills_dir` (`bundledSkillsDir`, an absolute read-only skills folder searched after the workspace skills) and `workspace_trust` (`workspaceTrust`: `trusted`, `untrusted`; see below).
- `git`: new typed operations `merge`, `mergeContinue`, `mergeAbort`, `rebase`, `rebaseContinue`, `rebaseAbort`, `conflicts`, `resolve`, `revert`, `stageHunk`, `stashList`, `stashCreate`, `stashApply`, `stashDrop`, `tagList`, `tagCreate`, `tagDelete`, `tagPush`, `log`, `commitDetail`, `progress`, `cancel`; `clone`, `fetch`, `pull`, `push` take an `operationId` for progress and cancellation, a rejected push is an error, a failed or cancelled clone leaves its (previously empty) destination empty, and `status` reports `repoState`. Merges and rebases start only on a clean tree and on an explicit request; nothing is forced; every path is an exact repository path.
- `audit_log`: record kind `authorizationDecision` (`allow`, `deny`, `card`, the policy rule text; never arguments), written for tool approvals and policy denials.
- `interaction` and `subagent` modules: the on-device `ask_user` / `exit_plan_mode` bridge and the in-process, model-only `spawn_subagent` spawner (one child at a time, at most eight iterations, the parent's permission mode, workspace trust, interrupt and remaining cost ceiling, no approval cards, no GitHub or Cloudflare credential, no MCP server, no `ask_user` / `exit_plan_mode` / `spawn_subagent`).
- `small_model`: structured (JSON Schema) answers for follow-up, classify and progress-audit assists, with a plain-text fallback.

Host options (all default `false`): `set_interactive_tools` (wires `ask_user` and `exit_plan_mode`; off leaves both out of the schema), `set_subagents` (wires `spawn_subagent`; off leaves it out), `set_git_loopback_fixture` (a git request may carry `loopbackBase` `http://127.0.0.1:<port>/`; off refuses any request that does), `set_small_model_structured_output`, `set_browser_control_capability` (the capability list names `browserControl` as unavailable).

Naming: the workspace state folder (`.rightkit` by default: runtime storage, skills, runtime-state exclusion from git status and stash), the product name in the system prompts and in the HTTP `User-Agent` of the Responses route (`Agent` by default), and the reference and reflog names git writes (`refs/<host slug>/premerge` and `prerebase`, `<app name> fast-forward pull`, `... merge`, `... pre-merge HEAD`, `... pre-rebase HEAD`) come from `rightkit-agent-config` (`state_dir_name`, `app_name`, `host_slug`), which the crate now requires (0.1.5). For the default host every one of them is the value it had.

Stricter for every host:

- An absent or empty `workspaceTrust` is `untrusted` (the engine default `trusted` is no longer inherited): effectful tools raise an ask instead of auto-allowing under `acceptEdits` and `autoSafe`.
- An approved plan leaves Plan mode only when the session is in Plan mode.
- `host_mobile_git_execute`, `host_mobile_runtime_new` and `host_mobile_runtime_submit` catch a panic and return the error envelope, an error and a failed turn instead of aborting the app.
- The capability report's `wasmCompute` is available only when both the host reports it and the callback is installed (was: the callback alone).
- The journal and the audit log are still best-effort, but the first failure of the process is written to standard error instead of being discarded; the interrupt flag is stored even when no receiver is attached.
- Lint waivers and discarded results removed: the crate-wide `collapsible_match` allow, `let _ =` and underscore-named parameters in the code this release touches.

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
