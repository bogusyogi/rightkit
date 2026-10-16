# rightkit-hooks

Typed Claude Code and Codex hook payloads, a deadline-bound dispatcher that exits 0, exact output builders, and delivery receipts. The native pair of `@rightkit/hooks` for capability `lifecycle-hooks`; it holds no policy.

| Item | Value |
|---|---|
| Where it runs | Hook processes launched by Claude Code or Codex (Pulse, Legion) |
| JS pair | `@rightkit/hooks` |
| License | MIT OR Apache-2.0 |

## API

| Item | What it does |
|---|---|
| `HookEvent::from_value / from_slice / from_reader(wire, ..)` | Parse a payload (stdin cap 4 MiB). Field aliases follow `normalizeHookEvent`; the untouched JSON stays in `raw`. `is_own_turn()` is Pulse's subagent guard. |
| `Wire`, `EventKind` | Harness (`Claude`, `Codex`; chosen by the registration) and event name. Legion's `pre-effect`-style names normalize to host names; unknown names are kept. |
| `Reply` | `additional_context(kind, text)` (SessionStart, UserPromptSubmit, PostToolUse only), `block_stop(reason)`, `deny_tool(wire, reason)`, `ask_tool(wire, reason)`, `with_system_message(text)`. |
| `HookModule`, `FnModule`, `dispatch`, `DispatchOptions` | Modules run in order. Defaults: 3 s per module, 8 s total, 60 000-byte context cap. Errors, panics and timeouts become typed results. |
| `run`, `run_with` | The whole hook process. Returns 0 on every failure, printing the configured fallback (default nothing). |
| `ReceiptStore`, `tag_context`, `marker`, `scan_transcript_tail` | `emitted` receipts with a marker line on the context; `observe` flips them to `observed` when the marker appears in the host transcript tail. |
| `ObserveModule` | The transcript-scan hook: on the chat's own `Stop`, runs `observe`. |

## Output shapes

| Case | Output |
|---|---|
| Context | `{"hookSpecificOutput":{"hookEventName":E,"additionalContext":T}}` |
| Stop continuation | `{"decision":"block","reason":R}` |
| Claude PreToolUse deny/ask | `{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny"\|"ask","permissionDecisionReason":R}}`, exit 0 |
| Codex PreToolUse deny (ask also denies) | empty stdout, reason on stderr, exit 2 |

The exit-2 case is the only non-zero exit, and only a module's explicit deny produces it.
