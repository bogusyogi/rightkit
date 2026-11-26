# Changelog

## 0.3.2

- Fix: the Telegram transport splits a reply longer than 4096 UTF-16 units into several `sendMessage` calls, never inside a character and preferring a line break, with the inline buttons on the last piece. Before, one oversized message was sent and Telegram refused it. Taken for every host.
- Fix: a Bot API answer outside 2xx is a failed call. `telegram_post` returned success for any answer that arrived, so a refused `sendMessage` or `answerCallbackQuery` went unlogged; it now returns `Telegram returned HTTP <status>`, which the caller logs. The bot token stays out of the text.

## 0.3.1

- Fix: a webhook delivery record's `target_redacted` keeps only scheme, host and port plus a 12-hex-digit SHA-256 prefix of the full target (`https://discord.com/<redacted:1beb230cad8c>`). Before, the path stayed, and Slack, Discord and ntfy carry their secret in the path. Records written earlier keep the old text. Taken for every host.
- New `host` module, each value fixed once at start-up, defaults unchanged: `set_product_name` (names the host in the Telegram channel note: "Telegram messages enter <name> as remote untrusted content."; default `the host`), `set_final_replies_only` (Telegram forwards only the assistant message that ends the turn, phase `final`, instead of every completed message without tool calls; default off) and `set_approval_arguments` (the approval prompt adds the command, or the arguments, bounded to 800 characters with control characters dropped; default off). CodeRight calls `set_product_name("CodeRight")`, `set_final_replies_only(true)` and `set_approval_arguments(true)`.
- Synced with CodeRight's `coderight-remote`; its `#[cfg(test)]` module is not carried (no unit tests here).

## 0.3.0

- Depends on `rightkit-agent-engine-protocol` 0.4.0 (additive events and fields, see its changelog). Breaking for dependents: the crate exposes protocol types in its public API, so those types are now the 0.4 ones. `event_seq` handles the new `ToolArgumentsPartial` and `SubAgentOutput` events.

## 0.2.0

- Breaking for dependents: a dependency requirement was raised to a semver-breaking version.

## 0.1.2 (unpublished)

- Added `telegram_poll`: the Telegram transport that was a private module of `rightkit-agent-server` (the long-poll loop `run_telegram_bot`, the Bot API calls, the HTTP `DaemonClient` over the daemon's `/sessions` and `/remote` routes, `TelegramRuntimeStatus` and its store, backoff). Moved unchanged except that the binding store path is a parameter (the host picks it; the `.coderight` fallback stays in the host). Taken on the recommended option of the 2026-11-11 shape review, pending the owner's confirmation; design note: docs/audits/2026-11-11-shape-matrix/reports/agent-platform-redraw.md. New dependencies: `tracing` 0.1 and `rightkit-http`'s `agent_host` feature.

- Requires `rightkit-agent-engine-protocol` 0.2.0, whose approval records carry the approvals core's `Binding` (mandatory digest) instead of an optional `payload_digest`. Decision 4 of the 2026-11-11 shape review (docs/audits/2026-11-11-shape-matrix/README.md, "Agent engine") taken on the recommended option, pending the owner's confirmation; design note: docs/audits/2026-11-11-shape-matrix/reports/agent-engine-contract-design.md.

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `coderight-remote` (engine).
