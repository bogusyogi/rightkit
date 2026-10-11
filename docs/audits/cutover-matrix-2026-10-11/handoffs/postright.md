# PostRight — RightKit migration handoff

Source root: /Volumes/D/claude/postright

Use this audit snapshot to plan cutovers; recheck cited callers before edits because other chats may have changed source. Select verified published versions; keep source cutover, packaging & execution qualification separate.

[Full app evidence](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/postright.json)

## rightkit-channel-memory — N

Active channel.import parses fixture proof-bank records, checks scope, maps statuses & writes rows locally. This overlaps shared proof parsing & controlled proof-write scope, though it does not implement full channel-memory v1 or citation semantics; no shared caller exists.

Map fixture input to ChannelMemory & invoke shared parse/write rules before local persistence; retain PostRight channel table & product policy.

- Local: [parse_proofs](/Volumes/D/claude/postright/crates/engine/src/channels.rs:144) — Local production parser accepts proof-bank records & maps statuses before persistence.
- Local: [import](/Volumes/D/claude/postright/crates/engine/src/channels.rs:229) — channel.import locally upserts one channel plus proofs into SQLite.
- Local: [PostrightCommands::call](/Volumes/D/claude/postright/crates/engine/src/commands.rs:44) — Live engine dispatch routes channel.import to local importer.
- Owner: [parse](/Volumes/D/claude/rightkit/crates/rightkit-channel-memory/src/validate.rs:13) — Shared API validates one versioned channel-memory revision through schema, typed model & semantic checks.
- Owner: [check_proof_write](/Volumes/D/claude/rightkit/crates/rightkit-channel-memory/src/proof.rs:528) — Shared API controls proof record write authority, status transitions & record validation.

Delivery: rightkit-channel-memory is private; availability must be resolved before release qualification.

Before marking a task complete: migrate matching callers, remove only proven duplicate mechanisms, preserve product-specific schemas/policy/UX, then run affected target checks. Update this inventory from fresh evidence.
