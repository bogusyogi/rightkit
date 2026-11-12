# Changelog

## 0.3.0 (unpublished)

- Wire compatibility for one release: `pairing::AgentView` serializes `scopes` and also its 0.2 name `accounts` (same value), and `pairing::AuditView` serializes `scope` and also `account_id`. Both now implement `Deserialize` and accept either name or both (the new name wins), so MailRight's agents settings and any reader of the 0.2 field names keep working. The old names go in the next breaking release.
- Tests build with `rightkit-db/bundled` as a dev-dependency: without `rightkit-mail` in the graph nothing else selected a SQLite provider, so the Windows test link failed (`LNK1181: sqlite3.lib`). The library still leaves the provider to the host.

- Breaking: the package is now the gateway core, with no product verbs. Taken on the recommended option of the 2026-11-11 shape review (docs/audits/2026-11-11-shape-matrix/reports/family-agent-platform.md, "rightkit-agent-gateway"), pending the owner's confirmation; design note: docs/audits/2026-11-11-shape-matrix/reports/agent-platform-redraw.md.
- The mail verbs that versions 0.1 and 0.2 held (drafts, send, forward, delete, label, attachments, the mail approval policy and the `GatewayHost` seam) moved unchanged to the new crate `rightkit-mail-gateway` 0.1.0, which runs on this core.
- Kept here, generalised: the agent registry (`registry`: `Agent<P>` and `Verified<P>` over any `gate::Level`; `accounts` is `scopes`, `set_accounts` is `set_scopes`), pairing (`pairing`, generic over the level; `AgentView.scopes`, `AuditView.scope`), the connection seam (`access`), the SQL audit (`audit::SqlAudit`, `audit::page`; `Entry.account_id` is `Entry.scope`, plus `origin` and `fields`), `registry::invisible_or_control`. All under feature `sql` (default).
- New: `gate` (the ordered checks `lookup`, `allow_level`, `allow_scope`, `allow_door` over a static `Method` table; `Denied` with stable codes; `conclude` writes one audit row per call, `Logging::Required` or `Logging::BestEffort`), `audit::AuditSink` (a sink trait for a table or a log file), `Origin` (the door, from `rightkit-remote-gateway`), `content` (the untrusted wrapper with the source as a parameter, `Budget`, and `stamped` for Pulse's unverified-sender line).
- `redact` is one module: the secret masking that was `rightkit-owner-approvals::redact` (moved unchanged) plus MailRight's leak check (`find_secret`, `text_has_secret`, `assert_clean`, unchanged markers) that was this crate's `redact`.

## 0.2.0 (unpublished)

- MailRight's mail gateway; see `rightkit-mail-gateway`'s CHANGELOG for its history.
