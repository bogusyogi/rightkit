# Changelog

## 0.1.0 (unreleased)

- Synced with CodeRight: the adaptive retrieval router (`retriever`: bandit, tier router, `RecallCandidate`, `curate_recall_candidates`) and `MemoryStore::as_recall_candidates` are removed; the membrane owns retrieval and this crate ships no embedder.
- First RightKit release, moved from `coderight-memory` (engine).
