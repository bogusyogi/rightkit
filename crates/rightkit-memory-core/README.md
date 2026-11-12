# rightkit-memory-core

Durable-memory core primitives: memory records, token budgeting, ranking and embedding-backed recall helpers. The embedding and retrieval half (`Embedder`, `HashEmbedder`, `MemoryRetriever`) is behind the feature `embed` (off by default), which pulls `rightkit-embed` and its ONNX and SQLite stack; without it the memory types, planner, review and accounting build alone.

Moved into RightKit from `cortex-core` (engine) on 2026-11-01.
