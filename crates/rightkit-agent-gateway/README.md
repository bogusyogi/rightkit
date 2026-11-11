# rightkit-agent-gateway

Local agent gateway for a mail-style app: pairing registry, append-only audit log, request gate profiles, untrusted-content wrapping and the user-approval flow for send, forward, delete and attach, over a host-implemented connection trait.

Moved into RightKit from MailRight (src-tauri agent/*, approval flow included) on 2026-11-01.

Approvals use the `rightkit-approvals` core's record and rules (decision, claim, pending cap); `approval_store::SqlStore` is the core `Store` over this app's `agent_approvals` table, and the mail limits (send and delete windows, decline cooldown) stay here.

The host implements `ConnAccess` (gated database connections), `GatewayHost` (mutation guard, local mail mirror, settings, file scanner, product name; installed with `set_host`), `DraftCreator`, `DraftMailer`, `ThreadTrasher`, `AttachFiles` and `KeyStore`.
