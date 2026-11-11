# Changelog

## 0.1.3 (unpublished)

- The approval queue (`approvals`) runs on the `rightkit-approvals` 0.2.0 core instead of its own copy. Decision 3 of the 2026-11-11 shape review (docs/audits/2026-11-11-shape-matrix/README.md, "Approvals") taken on the recommended option, pending the owner's confirmation; design note: docs/audits/2026-11-11-shape-matrix/reports/approvals-core-design.md. Deleted as duplicates: the `Held` request, decision and used-id lists, `prune_held`, the dedupe, cap, decide, take and session-lookup code, and `bind_of` (now the core's, re-exported here). The engine's record is the core's `MemoryStore`; filing, deciding and using go through `rules::file`, `rules::decide` (with `approval_sig::SignedApproval` as the proof for `method: "sig"`) and `rules::take`; session grants through `rules::session`. `request_digest` is the core's `Request::display_digest` under the `pulse.approval.request.v1` tag, byte for byte the same digest.
- Public API unchanged. Rules unchanged: ten-minute act validity after the decision, ten-minute pending life, thirty-minute session idle, thirty-day standing idle, unlock until its time or its chat, sixteen pending at most, refusals visible five minutes, 256 used ids remembered; the same refusal codes.
- Request ids are now 32 hex characters from the OS random source (were 16, with a clock fallback). If the random source fails, no request is filed (`approval_entropy`).

## 0.1.2 (unpublished)

- rightkit#1483: sysinfo 0.40 and rightkit-system-monitor 0.2.0. `System::new()` and `process_sampler::with_processes` now return `Result`; where the OS gives no process table: listings are empty, `procs.list` refuses with `unavailable`, an action-time identity check finds nothing to act on, and a process that cannot be shown gone counts as alive.

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from Pulse (pulse-core remote/*). No test-mode feature: no `RIGHTKIT_CLAUDE_BIN`/`PATH` Claude lookup, no settings-from-file override.
