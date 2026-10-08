# @rightkit/logs

Local-only structured logging for Right Suite webviews. The client batches schema-v1 JSON events and sends them to three shared native commands. It has no network and no Tauri dependency: the app injects its own `invoke`.

| Item | Value |
|---|---|
| Where it runs | App webview UI (client). The Rust host writes the JSONL files. |
| Rust pair | `rightkit-logs` (privacy-safe local JSONL logging and diagnostics) |
| Version | see INDEX.md |
| License | MIT OR Apache-2.0 |

Nothing uploads automatically. The user collects a diagnostics bundle and decides whether to share it.

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/logs` | `createLogsClient(config)`; constants `RIGHTKIT_LOGS_WRITE_COMMAND` (`rightkit_logs_write`), `RIGHTKIT_LOGS_COLLECT_COMMAND` (`rightkit_logs_collect`), `RIGHTKIT_LOGS_CLEAR_COMMAND` (`rightkit_logs_clear`); types `LogLevel`, `LogFields`, `StructuredLogEvent`, `LogsInvoke`, `LogsClientConfig`, `LogsClient` |

`LogsClientConfig`: `invoke`, `allowedFields` (closed list of field names that reach disk), `maxBufferedEvents`, `flushIntervalMs` (0 for explicit flush), `now`.

Peer dependencies: none.

## Example

```ts
import { invoke } from "@tauri-apps/api/core";
import { createLogsClient } from "@rightkit/logs";

export const logs = createLogsClient({
  invoke,
  allowedFields: ["surface", "count", "duration_ms", "code"],
});

logs.info("app_started", { surface: "shell" });
await logs.flush();
```

Fields not in `allowedFields` are dropped. When the buffer is full, the oldest event is dropped and a `logs_dropped` marker is written.

## Known issues

- `@rightkit/tauri` repeats the three command-name literals instead of importing these constants (`packages/tauri/src/index.ts`). One of them should own the names (npm audit 2026-10-08, logs finding 1).
- `settings-ui` peers this package with no upper bound (`>=0.1.0`).
