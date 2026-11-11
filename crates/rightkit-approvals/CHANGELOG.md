# Changelog

## 0.2.0 (unpublished)

- Breaking: the crate is now the one approvals core every app uses. Decision 3 of the 2026-11-11 shape review (docs/audits/2026-11-11-shape-matrix/README.md, "Approvals") taken on the recommended option, pending the owner's confirmation; design note: docs/audits/2026-11-11-shape-matrix/reports/approvals-core-design.md.
- The model: `Request` (id, kind, scope, `Origin` {door, filer, client}, `Binding`, summary, display-only detail, created, deadline), `Record` (a request plus its `State`: pending, approved, denied, done, failed, expired; the `Grant` an approval gives: one act, idle session, until a time, until revoked; reason; result), `Change` (one move).
- The reference rule (Pulse's): an approval is bound to a digest of the exact arguments, the destination, the expiry and the revision (`Binding`, `Binding::check`, `Binding::digest`, `bind_of`, the length-prefixed SHA-256 that `rightkit-remote-gateway` used); a decision is bound to the display digest of what the decider was shown (`Request::display_digest`, with the app's own domain tag).
- The rules, once (`rules`): `file` (a repeat of a waiting request gets the same id; pending cap, overall or per client), `decide` (only a pending, unexpired request; proof and display digest checked; one compare-and-set), `take` (an approval is spent by one act for the same kind, scope, door, filer, destination, arguments and revision; with a proof a pending request is decided and used in one move), `session` (a live session grant, refreshed by use). Refusals: `Error` (typed) and `Refusal` (stable code; moved here from `rightkit-owner-approvals`, which re-exports it).
- The stores: the `Store` trait (insert, get, pending, approved, an atomic `transition`, touch), `MemoryStore` (Pulse's in-memory queue, with `Retention` caps) and `FileStore` (feature `file`, on by default: GenRight's file layout, one owner-only file per request and a `create_new` marker per move). MailRight's SQL store implements the trait in `rightkit-agent-gateway` (it needs `rusqlite` and the app's table, which the core should not pull).
- The proof: the `Proof` trait. Its implementations live with their key material: the launch-secret token in `rightkit-media-gen-approvals`, the signed approver key in `rightkit-owner-approvals` (`approval_sig::SignedApproval`).
- Moved out: everything GenRight-specific (the `studio.approvals.*` and `studio.blender.spec.*` commands, `arm`, `gate`, `issue`, `consume`, `register_kind`, `eyes`, the `blender_finish` kind and the `rightkit-media-gen`, `rightkit-daemon`, `rightkit-child-process`, `rightkit-contracts` and `time` dependencies) is now `rightkit-media-gen-approvals` 0.1.0, built on this core, with the same paths under `rightkit_media_gen_approvals::`.
- No product names, wire tags or command names in this crate.

## Unreleased

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from GenRight (`genright-studio` approvals.rs).
- `eyes`: the human-eyes gate (`studio.eyes.*`: status, request, approve, reject, rework, list_pending, export_check; `gate_state`, `ensure_exportable`, `review_gate` on `rightkit-contracts`' `eyes_gate`), moved from `genright-studio` `eyes.rs`. `eyes` is a built-in approval kind.
