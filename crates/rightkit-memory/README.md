# rightkit-memory

The RightKit memory engine (Pulse's Cortex), as one crate:

- the crate root re-exports `rightkit-memory-core`: memory tiers and entries, retrieval ranking and routing, the context planner, review contracts, transcript chunks, the evaluation gate and token accounting (no I/O);
- `store` re-exports `rightkit-memory-store`: the SQLite memory store, scopes, installation identity, absorbed records, temporal facts and context telemetry.

Apps that embed the engine depend on this crate (or on the two parts directly). Apps that reach an engine running elsewhere through the membrane CLI use `rightkit-memory-client`, whose tiers and rows are this crate's types.

Until 0.1.1 this name was CodeRight's memory client; that crate is now `rightkit-memory-client`.
