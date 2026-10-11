# Rhook — RightKit migration handoff

Source root: /Volumes/D/claude/tools/rhook

Use this audit snapshot to plan cutovers; recheck cited callers before edits because other chats may have changed source. Select verified published versions; keep source cutover, packaging & execution qualification separate.

[Full app evidence](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/rhook.json)

## rightkit-db — N

Live AuthorityStore opens and owns Rhook SQLite state directly; this overlaps rightkit-db writer bootstrap, with no shared route.

Move AuthorityStore connection bootstrap to rightkit-db while preserving schema, transactions, and fail-closed authority checks.

- Local: [Production bounded modes open AuthorityStore at authority.sqlite3.](/Volumes/D/claude/tools/rhook/src/main.rs:173) — authority
- Local: [Creates Rhook SQLite authority store and schema.](/Volumes/D/claude/tools/rhook/src/bounded/store.rs:93) — AuthorityStore::open
- Owner: [Shared owner opens configured writer connection.](/Volumes/D/claude/rightkit/crates/rightkit-db/src/lib.rs:237) — Database::open_writer

## rightkit-fs — N

Live bounded-run persistence uses Rhook-local atomic temporary-file rename for state and active-marker writes; this overlaps rightkit-fs atomic write API, with no shared route.

Replace local state-file atomic writes with rightkit-fs while retaining Rhook state paths and hook policy.

- Local: [Writes temp file then renames it for bounded run state.](/Volumes/D/claude/tools/rhook/src/main.rs:193) — write_atomic
- Local: [Live bounded-run entrypoint persists active marker through write_atomic.](/Volumes/D/claude/tools/rhook/src/main.rs:530) — bounded_freeze
- Owner: [Shared owner provides owner-only atomic replacement.](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/atomic.rs:146) — atomic_write_private

## rightkit-process — N

Live compact and opportunity-recording paths spawn, wait for, and time-limit child processes locally; this overlaps rightkit-process owned child lifecycle, with no shared route.

Route compact and cortex child lifecycle through rightkit-process, preserving direct-argv and bounded-output policy.

- Local: [Production compact command directly spawns child argv and waits for output.](/Volumes/D/claude/tools/rhook/src/compact.rs:469) — compact::run
- Local: [Production hook starts cortex child and kills it after three-second deadline.](/Volumes/D/claude/tools/rhook/src/lib.rs:1457) — record_transform_opportunity
- Owner: [Shared owner creates owned child lifecycle.](/Volumes/D/claude/rightkit/crates/rightkit-process/src/owned.rs:225) — OwnedCommand::spawn
- Owner: [Shared owner terminates owned process tree.](/Volumes/D/claude/rightkit/crates/rightkit-process/src/owned.rs:469) — OwnedChild::terminate_tree

## rightkit-sandbox — N

Live bounded authority reserves work, mutation, process, verification, and output budgets before calls; this overlaps rightkit-sandbox reserve-before-spawn resource authority, with no shared route.

Map Rhook process/resource reservations to rightkit-sandbox while retaining signed capsule and hook-specific budgets.

- Local: [Enforces Rhook-local work, mutation, process, verification, and output limits.](/Volumes/D/claude/tools/rhook/src/bounded/budget.rs:49) — Budget::reserve
- Local: [Live pre-tool hook reserves capability budget before allow decision.](/Volumes/D/claude/tools/rhook/src/main.rs:561) — bounded_pre
- Owner: [Shared owner reserves resource slot before execution.](/Volumes/D/claude/rightkit/crates/rightkit-sandbox/src/resources.rs:119) — ResourceAuthority::reserve

Before marking a task complete: migrate matching callers, remove only proven duplicate mechanisms, preserve product-specific schemas/policy/UX, then run affected target checks. Update this inventory from fresh evidence.
