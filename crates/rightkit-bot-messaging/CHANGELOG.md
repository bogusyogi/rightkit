# Changelog

## 0.1.2 (unpublished)

- Added `telegram_poll`: the Telegram transport that was a private module of `rightkit-agent-server` (the long-poll loop `run_telegram_bot`, the Bot API calls, the HTTP `DaemonClient` over the daemon's `/sessions` and `/remote` routes, `TelegramRuntimeStatus` and its store, backoff). Moved unchanged except that the binding store path is a parameter (the host picks it; the `.coderight` fallback stays in the host). Taken on the recommended option of the 2026-11-11 shape review, pending the owner's confirmation; design note: docs/audits/2026-11-11-shape-matrix/reports/agent-platform-redraw.md. New dependencies: `tracing` 0.1 and `rightkit-http`'s `agent_host` feature.

- Requires `rightkit-agent-engine-protocol` 0.2.0, whose approval records carry the approvals core's `Binding` (mandatory digest) instead of an optional `payload_digest`. Decision 4 of the 2026-11-11 shape review (docs/audits/2026-11-11-shape-matrix/README.md, "Agent engine") taken on the recommended option, pending the owner's confirmation; design note: docs/audits/2026-11-11-shape-matrix/reports/agent-engine-contract-design.md.

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `coderight-remote` (engine).
