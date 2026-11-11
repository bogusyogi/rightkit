# Changelog

## 0.2.0 (unpublished)

- Breaking: approvals use the `rightkit-approvals` 0.2.0 core's record and rules instead of their own copy. Decision 3 of the 2026-11-11 shape review (docs/audits/2026-11-11-shape-matrix/README.md, "Approvals") taken on the recommended option, pending the owner's confirmation; design note: docs/audits/2026-11-11-shape-matrix/reports/approvals-core-design.md.
- `approval_store::Approval` is now the core's `Record` (a type alias); `MailApproval` reads it in this crate's words (`agent_id()`, `account_id()`, `draft_id()`, `kind()`, `status()`, `created_at()`, `expires_at()`); `result_id` is `result`. The table is unchanged: `SqlStore` implements the core `Store` trait over `agent_approvals` (kept here rather than as a feature of the core because it needs `rusqlite` through `rightkit-db` and this app's table).
- Deleted as duplicates: the status-string state machine (`finish`, `claim_approve`, `insert_pending`, `get`, `list_pending`, `count_pending`, the free `sweep`). The claim is the core's `rules::decide` (one guarded UPDATE, unexpired and from this run); creating a request is `rules::file` with the per-agent pending cap; status moves are `SqlStore::move_row`. Kept here as mail policy: the send and delete windows, the decline cooldown, `live_for_draft`, the drafts and attach-path tables.
- Behaviour unchanged, except new approval ids are 32 hex characters (were 22 base64url characters).
- `SqlStore` implements the core's new `Store::claimed` (none) and refuses a move to or from the core's `State::Claimed` (`move_row`): the table has no claimed status; its `approved` row is the send in flight, settled by `sweep`.

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from MailRight (src-tauri agent: access, approval flow, attachments, audit, content, drafts, export, forward, gate, import, mutate, outcome, pairing, read handlers, registry, send requests).
