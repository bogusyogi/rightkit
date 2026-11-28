# Changelog

## 0.1.2

- `memory_save` (feature `agent_host`) asks for approval for anything that did not come from the user's own session (`rightkit_agent_policy::memory_write_is_authorized`), tool output included. It asked only for content from outside (web or MCP). Stricter for every host.
- The `agent_host` feature builds against `rightkit-agent-coding-tools` 0.2 and `rightkit-agent-policy` 0.2.4. The 0.1.1 on crates.io was published while the requirement was still 0.1, so a host on coding-tools 0.2 got two copies of the tools crate; this release is the first published with the 0.2 requirement.

## 0.1.1 (2026-11-16)

- Requires rightkit-agent-coding-tools 0.1.8 and names its `os-keyring` feature for `agent_host`, so the build is unchanged (coding-tools no longer implies the OS keyring). No behaviour change.

## 0.1.0 (renamed from `rightkit-memory` 0.1.1)

- Renamed from `rightkit-memory` (library `rightkit_memory_client`): the crate is the client of the memory engine, and the `rightkit-memory` name now belongs to the engine (0.2.0, a facade over `rightkit-memory-core` and `rightkit-memory-store`). Replace `rightkit_memory::` with `rightkit_memory_client::`.
- `backend::MemoryTier`, `backend::MemoryEntry` and `default_scope` are the engine's types from `rightkit-memory-core` 0.1.1, re-exported; the second declaration is gone. Same wire shape; `backend::types::default_scope` is now public (the engine's).
- Builds without the embedding stack: `rightkit-memory-core` 0.2 keeps embedding and retrieval behind its `embed` feature, which this crate leaves off, so `rightkit-embed` (ONNX inference, SQLite vectors) is no longer in its graph or in the graphs of `rightkit-agent-engine-mobile-runtime` and `rightkit-agent-server` through it.
- `src/retriever.rs`, a second retrieval router that was never compiled (not declared in `lib.rs`) and duplicated `rightkit-memory-core`'s routing, is deleted.
- Taken on the recommended option of the 2026-11-11 shape review, pending the owner's confirmation.
- `MemoryCausalOrigin::UntrustedWorkspace` (`untrusted-workspace`, risk rank 2) maps the new `rightkit-agent-policy` `ContentOrigin::UntrustedWorkspace`.

## rightkit-memory, unreleased changes carried into 0.1.0 above

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.
- Removed the `fake-membrane` test-double binary and `tests/membrane_fake_binary.rs` (fake, not end to end).

## rightkit-memory 0.1.1

- Add `agent_host` feature: modules api_memright, blueprint_tools, memory, memory_provenance, memory_save_tool (moved from an agent host binary).

## rightkit-memory 0.1.0 (unreleased)

- Synced with CodeRight: the adaptive retrieval router (`retriever`: bandit, tier router, `RecallCandidate`, `curate_recall_candidates`) and `MemoryStore::as_recall_candidates` are removed; the membrane owns retrieval and this crate ships no embedder.
- First RightKit release, moved from `coderight-memory` (engine).
