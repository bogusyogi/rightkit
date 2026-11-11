# rightkit-mail-gateway

The mail adapter of the agent gateway: the mail verbs an agent can call (`accounts`, `search`, `thread`, `labels`, `draft`, `send`, `forward`, `delete`, `label`, `archive`, `move`, `attachment`, `attach_file`, `approval`), their minimum profiles (read-only, draft, send with approval), the mail approval policy and the user-approval flow for send, forward, delete and attach, on the `rightkit-agent-gateway` core.

Moved into RightKit from MailRight (src-tauri agent/*, approval flow included) on 2026-11-01, as `rightkit-agent-gateway` 0.1 and 0.2; the mail verbs became this crate on 2026-11-11 when that name became the gateway core.

The core holds what is not mail: the agent registry and pairing (wrapped here as `registry` and `pairing` at this crate's `Profile` levels; an agent's scopes are its accounts), the gate's ordered checks and its one audit row (`gate` runs on `rightkit_agent_gateway::gate` with `Logging::Required`: no audit row, no result), the connection seam (`access`), the untrusted-content wrapper (`content` wraps with source `email`) and the redaction leak check (`redact`).

Approvals use the `rightkit-approvals` core's record and rules (decision, claim, pending cap); `approval_store::SqlStore` is the core `Store` over this app's `agent_approvals` table, and the mail limits (send and delete windows, decline cooldown) stay here.

The host implements `ConnAccess` (gated database connections), `GatewayHost` (mutation guard, local mail mirror, settings, file scanner, product name; installed with `set_host`), `DraftCreator`, `DraftMailer`, `ThreadTrasher`, `AttachFiles` and `KeyStore`.
