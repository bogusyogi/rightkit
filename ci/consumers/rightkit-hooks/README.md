# rightkit-hooks 0.2.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-hooks =0.2.0` exactly with the default feature set, the set the Pulse chat hooks use. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed assertion.

## What it proves

- Names. `Wire` and `EventKind` parse and print; the lower-case Legion aliases (`session-start`, `pre-effect` and the rest) normalise to the host names, an unknown name is kept verbatim, and `takes_additional_context` is true for exactly `SessionStart`, `UserPromptSubmit` and `PostToolUse`.
- Event parsing. `HookEvent::from_value`, `from_slice` and `from_reader` read the field aliases (`hook_event_name`, `hookEventName`, `event`; `thread_id`, `session_id`, `sessionId`; `tool_name`, `toolName`; `prompt`, `user_prompt`), skip blank strings, keep the untouched JSON in `raw`, and refuse malformed JSON, a non-object, a missing event name and input over `STDIN_CAP` (`ParseError`). `is_own_turn` follows the subagent guard on both wires.
- Output builders. `Reply::additional_context` for the three events that take it and `OutputError` for the others, `block_stop`, `deny_tool` and `ask_tool` on Claude (JSON on stdout, exit 0) and on Codex (reason on stderr, exit 2), `with_system_message`, `stdout_line` and `write_to`.
- The dispatcher. Modules run in order; contexts join with a blank line and are capped on a character boundary; a decision wins over context and is dropped where the event does not take it; an error, a panic and a timed-out module become typed results with status `Error` and the fallback reply; a spent total deadline records the remaining modules without starting them; `compose_reply` on its own; the default deadlines and cap.
- The process loop. `run_with` over byte streams exits 0 on garbage input and on a panicking module, prints the configured fallback, and exits 2 only for a Codex deny.
- Receipts. `marker`, `tag_context` (the cap is never exceeded and the marker survives), unique alphanumeric receipt ids, `ReceiptStore` `record_emitted`, `tag_and_record`, `list` and `observe` against a transcript file, the Membrane store's two field spellings on disk (and reading a receipt Membrane flipped to `delivered`), `scan_transcript_tail` on a missing file, and `ObserveModule` marking receipts observed on a `Stop` event and doing nothing on another event.
- The Membrane wire types. Module ids and their order, the status builders, `aggregate_hook_status`, and `project_hook_host_response` for `Stop`, `PreToolUse` and an event with no host projection, with a JSON round trip that reproduces the wire exactly and refuses unknown fields.

## What it deliberately does not prove

- `run`, the entry point that reads the real process stdin and writes the real stdout and stderr. `run_with` is proven over in-memory streams instead.
- The 3 s and 8 s default deadlines under load; the timeout test uses a 150 ms deadline.
- A real Claude Code or Codex host, or any module body; the crate holds no policy.

State: a per-process directory under the OS temp directory, removed at the end, and short-lived module threads. No network, no process spawn, no real user data.
