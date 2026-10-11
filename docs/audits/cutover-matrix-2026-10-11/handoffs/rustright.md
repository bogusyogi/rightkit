# RustRight — RightKit migration handoff

Source root: /Volumes/D/claude/rustright

Use this audit snapshot to plan cutovers; recheck cited callers before edits because other chats may have changed source. Select verified published versions; keep source cutover, packaging & execution qualification separate.

[Full app evidence](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/rustright.json)

## rightkit-jobs — N

JobService actively implements submit/watch/status/cancel, in-memory records, broker events & scheduling. This overlaps rightkit-jobs engine job runner/command contract, yet no rightkit-jobs route is linked.

Map build-control wire/state requirements to rightkit-jobs APIs, then replace local generic lifecycle pieces while retaining build policy.

- Local: [JobService::dispatch](/Volumes/D/claude/rustright/crates/rustright-broker/src/job_service/service.rs:690) — Routes LIST, SUBMIT, WATCH, STATUS & CANCEL for live broker jobs.
- Local: [Job](/Volumes/D/claude/rustright/crates/rustright-broker/src/job_service/job.rs:1) — Maintains live job state & bounded event output.
- Owner: [JobStore/Runner/JobCommands](/Volumes/D/claude/rightkit/crates/rightkit-jobs/src/lib.rs:1) — Owner exports persistent job store, runner & command contract.

## rightkit-logs — N

ThinLogger is active production append-only NDJSON lifecycle logging; it overlaps rightkit-logs JsonlStore capability, but no shared logging route is linked.

Adapt broker event schema to rightkit-logs JsonlStore/AsyncWriter while preserving broker lifecycle wire events.

- Local: [ThinLogger](/Volumes/D/claude/rustright/crates/rustright-service/src/logger.rs:16) — Owns append-only typed NDJSON file logger.
- Local: [run_loaded](/Volumes/D/claude/rustright/crates/rustright-broker/src/server/broker.rs:112) — Production broker opens lifecycle ThinLogger.
- Owner: [JsonlStore](/Volumes/D/claude/rightkit/crates/rightkit-logs/src/store.rs:10) — Owner persists append-only JSONL events.

## rightkit-sandbox — N

RustRight Scheduler reserves compile units before a broker process starts; it overlaps rightkit-sandbox ResourceAuthority reservation, but uses no shared authority/sandbox route.

Evaluate mapping compile-unit scheduler to rightkit-sandbox ResourceAuthority; retain workspace exclusion & build-cost policy.

- Local: [Scheduler::enqueue](/Volumes/D/claude/rustright/crates/rustright-scheduler/src/scheduler.rs:622) — Queues requests & yields RAII permits constrained by capacity.
- Local: [submit_and_watch](/Volumes/D/claude/rustright/crates/rustright-broker/src/job_service/service.rs:1410) — Build root creates Scheduler Request before process preparation.
- Owner: [ResourceAuthority::reserve](/Volumes/D/claude/rightkit/crates/rightkit-sandbox/src/resources.rs:103) — Owner reserves process capacity before spawn & releases through Reservation drop.

## rightkit-service — N

RustRight actively implements local Unix socket/named-pipe broker hosting, endpoint hardening & request dispatch. It overlaps rightkit-service local host transport, but has no shared discovery record, rank/takeover or allowlist route.

Assess adapting broker transport to rightkit-service while preserving immutable v2 protocol, endpoint & build-control admission semantics.

- Local: [bind/spawn_accept_loop](/Volumes/D/claude/rustright/crates/rustright-broker/src/server/listener.rs:23) — Hosts broker on Unix socket or Windows named pipe.
- Local: [prepare_unix_socket_endpoint](/Volumes/D/claude/rustright/crates/rustright-service/src/socket.rs:29) — Locally implements owner-only broker endpoint hardening.
- Owner: [host](/Volumes/D/claude/rightkit/crates/rightkit-service/src/server.rs:317) — Owner hosts authenticated local service with discovery/allowlist conventions.

Before marking a task complete: migrate matching callers, remove only proven duplicate mechanisms, preserve product-specific schemas/policy/UX, then run affected target checks. Update this inventory from fresh evidence.
