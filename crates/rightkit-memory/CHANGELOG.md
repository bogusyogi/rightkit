# Changelog

## 0.3.0

- Breaking for dependents: dependency requirements `rightkit-memory-core` 0.1 -> 0.2, `rightkit-memory-store` 0.1 -> 0.2. 0.2.0 was published before these requirements were raised; a semver-breaking dependency is a breaking change, so they ship as 0.3.0 instead of changing 0.2.0 in place (#1572). No source change in this crate since 0.2.0.
- Enables `rightkit-memory-core`'s new `embed` feature, so the re-exported embedding and retrieval items (`Embedder`, `HashEmbedder`, `MemoryRetriever`, ...) are all still here.

## 0.2.0

- The name now holds the memory engine: the crate re-exports `rightkit-memory-core` 0.1.1 at its root and `rightkit-memory-store` 0.1.1 as `store`. It has no code of its own.
- Breaking: the client that was published as `rightkit-memory` 0.1.x (the `MemoryBackend` over the membrane CLI, screening, the in-memory store and the `agent_host` modules) is now `rightkit-memory-client` 0.1.0, unchanged apart from using the engine's `MemoryTier` and `MemoryEntry`. Depend on it and replace `rightkit_memory::` with `rightkit_memory_client::`.
- Taken on the recommended option of the 2026-11-11 shape review, pending the owner's confirmation.

## 0.1.1 and earlier

Published as CodeRight's memory client; see `rightkit-memory-client`.
