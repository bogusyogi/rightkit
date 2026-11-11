# rightkit-memory-client

The client of the RightKit memory engine. The engine is `rightkit-memory` (a facade over `rightkit-memory-core` and `rightkit-memory-store`, Pulse's Cortex); this crate is how an app that does not embed the engine reaches it:

- `backend`: the typed `MemoryBackend` contract with its conformance run, the native backend over the external membrane CLI and an unavailable backend. Tiers and memory rows are the engine's `MemoryTier` and `MemoryEntry`, re-exported, not copies.
- `membrane`: the membrane CLI client (context pull, blueprint, adapt and transcript commands as well as memory).
- `screening`: durable-write screening; `consolidation_status`; `store`: a small in-memory store for hosts with no engine (mobile).
- Feature `agent_host`: the agent-server modules (memory save tool, blueprint tools, provenance, Memright API context).

Moved into RightKit from `coderight-memory` (engine) on 2026-11-01 as `rightkit-memory` 0.1.x; renamed `rightkit-memory-client` on 2026-11-11 so the engine holds the `rightkit-memory` name.
