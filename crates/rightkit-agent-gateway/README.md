# rightkit-agent-gateway

The core every local agent gateway runs on, with no product verbs: the agent registry (several named agents, each with a pairing key stored only as a SHA-256 hash, a permission level and a scope list, revocable at once), the gate's checks in one fixed order over a static method table, exactly one audit row per call, the door a call came in by (`Origin`), stable refusal codes (`Refusal`, the `rightkit-approvals` core's type, and `gate::Denied`), untrusted-content wrapping and stamping (`content`), and secret redaction with the gateway's leak check (`redact`).

Gate order (`gate`): caller known and not revoked, app unlocked, method in the table, level high enough, scope allowed, a read-only door refuses methods that change things, the adapter's handler, then one audit row (`gate::conclude`, `Logging::Required` or `Logging::BestEffort`). Pending approvals are the `rightkit-approvals` core's.

Feature `sql` (default): the registry (`registry`), pairing (`pairing`), the connection seam (`access::ConnAccess`) and the SQL audit sink (`audit::SqlAudit`) over the host's `agents`, `agent_accounts` and `agent_audit` tables. The host owns the schema. Without it the crate has no database dependency (an app with one owner and a log file implements `audit::AuditSink` itself).

Adapters:

* `rightkit-mail-gateway`: mail verbs (MailRight), on the registry, pairing, SQL audit and gate checks.
* `rightkit-remote-gateway`: files, terminals, screen and the rest for one owner (Pulse), on the door, the read-only door check, the best-effort activity row, the stamp and the redaction.

Version 0.3.0 is this core. Versions 0.1 and 0.2 of this package were MailRight's mail gateway, now `rightkit-mail-gateway`.
