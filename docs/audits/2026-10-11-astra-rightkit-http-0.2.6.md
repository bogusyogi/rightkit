# Astra adversarial read: rightkit-http 0.2.6 (2026-10-11)

Model gpt-6-astra, high reasoning, read-only via codex exec, against commit 67c7d4ce. No tests or exploits were executed. No P0. 6 P1, 14 P2, 1 P3. Publication of 0.2.6 is held until the P1 and P2 items are fixed and re-read.

1. **P3 — Successful download never performs promised fsync.** [download.rs:173](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:173), [download.rs:197](/Volumes/D/claude/rightkit/crates/rightkit-http/src/download.rs:197). File is closed, hashed & renamed without `sync_all`/`sync_data`. Power loss after reported success can leave incomplete destination data despite README’s fsync guarantee.
