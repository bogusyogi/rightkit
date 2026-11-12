# Changelog

## 0.2.0

- Breaking for dependents: dependency requirements `rightkit-embed` 0.6 -> 0.7. 0.1.1 was published before these requirements were raised; a semver-breaking dependency is a breaking change, so they ship as 0.2.0 instead of changing 0.1.1 in place (#1572).
- Breaking: embedding and retrieval are behind the new feature `embed`, off by default: the modules `embed` and `retriever` and their re-exports (`Embedder`, `HashEmbedder`, `EmbedProviderReport`, `MemoryRetriever`, `CortexProjection`, `LexicalHit`, `ProjectedEntry`, `ProjectedKind`), and with them the `rightkit-embed` dependency (its ONNX inference and SQLite vector stack). `rightkit-memory-client`, which needs only `MemoryEntry`, `MemoryTier` and `default_scope`, no longer pulls that stack into `rightkit-agent-engine-mobile-runtime` (a staticlib) and `rightkit-agent-server`. A crate that uses those items enables `features = ["embed"]` (`rightkit-memory` and `rightkit-context-runtime` do). `EMBEDDING_MAX_SEQUENCE_TOKENS` stays at the root (defined in `fingerprint`; `embed::EMBEDDING_MAX_SEQUENCE_TOKENS` with the feature).

## 0.1.1

- `MemoryTier::as_str` and `MemoryTier::parse`, and `PartialEq` on `MemoryEntry`, so the memory client (`rightkit-memory-client`) uses these types instead of declaring its own copies. Additive. Taken on the recommended option of the 2026-11-11 shape review, pending the owner's confirmation.
- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `cortex-core` (engine).
