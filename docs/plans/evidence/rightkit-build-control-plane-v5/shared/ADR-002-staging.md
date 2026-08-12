# ADR-002 — external transactional staging

Status: frozen for Phase 0

Every build runs in a request-scoped external stage with dependency closure, Git/worktree/dirty provenance, output remapping & transactional checksum-verified sync-back. macOS chooses APFS clone only after cross-volume proof; otherwise use content-addressed incremental copy. A disconnect terminates request & poisons target; no local fallback is allowed.
