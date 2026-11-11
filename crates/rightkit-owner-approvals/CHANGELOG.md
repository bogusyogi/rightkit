# Changelog

## 0.1.2 (unpublished)

- Breaking: `redact` moved to the gateway core, `rightkit-agent-gateway` 0.3.0 (`rightkit_agent_gateway::redact`, unchanged), next to MailRight's leak check: one redaction module for the gateways. Taken on the recommended option of the 2026-11-11 shape review, pending the owner's confirmation; design note: docs/audits/2026-11-11-shape-matrix/reports/agent-platform-redraw.md. `rightkit-remote-gateway` re-exports it under its old path.

- `Refusal` and `Result` are now the `rightkit-approvals` 0.2.0 core's, re-exported here under the same paths (same fields, same methods): one refusal type for every approval path. Decision 3 of the 2026-11-11 shape review (docs/audits/2026-11-11-shape-matrix/README.md, "Approvals") taken on the recommended option, pending the owner's confirmation; design note: docs/audits/2026-11-11-shape-matrix/reports/approvals-core-design.md.
- New `approval_sig::SignedApproval`: a signed approval as the core's `Proof` (the enrolled key signed kind `approve` for the record's own id and client and for the display digest the owner was shown). `rightkit-remote-gateway` decides with it.
- No behaviour change; the signed wire format (`pulse.approval.v1` lines) is unchanged.

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from Pulse (pulse-core remote approval_sig, redact, audit, protected_guard).
