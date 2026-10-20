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
| `@rightkit/logs` | `createLogsClient(config)`; `createDiagnosticsRecorder(config)`; `installWindowCrashCapture(config)` (returns the uninstall function); `scrubLogText(text)`, `scrubWindowCrashText(text, redactValues?)`; constants `RIGHTKIT_LOGS_WRITE_COMMAND` (`rightkit_logs_write`), `RIGHTKIT_LOGS_COLLECT_COMMAND` (`rightkit_logs_collect`), `RIGHTKIT_LOGS_CLEAR_COMMAND` (`rightkit_logs_clear`), `RIGHTKIT_CRASH_WINDOW_ERROR_COMMAND` (`rightkit_crash_window_error`); types `LogLevel`, `LogFieldValue`, `LogFields`, `StructuredLogEvent`, `LogsInvoke`, `LogsClientConfig`, `LogsClient`, `DiagnosticKind`, `DiagnosticInput`, `DiagnosticEntry`, `DiagnosticsRecorderConfig`, `DiagnosticsRecorder`, `PersistenceResult`, `WindowCrashPayload`, `WindowCrashTarget`, `WindowCrashConfig` |

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

Fields not in `allowedFields` are dropped, and so are keys named like secrets or content (for example `password`, `token`, `api_key`, `transcript`), even when allowed. When the buffer is full, the oldest event is dropped and a `logs_dropped` marker is written.

## Persistent renderer diagnostics

`createDiagnosticsRecorder(config)` keeps up to 80 content-free entries in memory & browser storage. It installs only `pagehide` & document `visibilitychange` listeners, at factory call time. It adds no crash listeners, observers, network access, UI, settings synchronization, or support delivery.

Config requires `storageKey` & `allowedEvents` (a copied, app-owned static allowlist). Optional fields: `legacyKeys`, `storage` (`getItem`/`setItem`/`removeItem` adapter), `enabled` (default `true`), & `now` (default `Date.now`). Without an adapter, it uses `window.localStorage` when accessible. Own one instance per key per window; use distinct keys for independent windows. Writes replace each key's history; there is no cross-window merge or aggregate guarantee.

`record({ kind, event, duration_ms? })` accepts `load`, `event`, `error`, `rejection`, or `perf` kinds. Event labels must exactly match `allowedEvents`, contain only ASCII letters, digits, `_ . : -`, & be 1–96 characters. Never populate this allowlist from document names, paths, user text, or other dynamic content. Only `perf` accepts optional `duration_ms`; duration & timestamp must be nonnegative safe integers. Unknown properties are dropped by explicit projection; message, detail, stack, source, paths, & arbitrary payloads never enter this recorder's history.

Accepted entries have `{ schema_version: 1, ts_ms, kind, event, duration_ms? }`. `record` returns `false` for invalid, disabled, disposed, or deduplicated input. Consecutive equal kind + event labels within 1000 ms collapse, including performance entries with different durations. `snapshot()` returns independent copies; `text()` returns readable JSON of those safe entries.

`load`, `error`, & `rejection` flush synchronously. Other entries share one idle callback with a 2000 ms timeout, or one 1000 ms timer. Page hide & hidden visibility flush pending history. Without a browser window, accepted entries attempt synchronous persistence through an injected adapter.

`flush()`, `clear()`, `setEnabled(boolean)`, & `dispose()` return `PersistenceResult`: `ok`, `disabled`, `unavailable`, `failed`, or `disposed`. `ok` means requested work succeeded or no write was needed. Storage failure retains dirty history for later explicit, lifecycle, or record-triggered retry, without an automatic retry loop. Malformed stored data yields empty restored history; failed reads remain unresolved & retain pending accepted entries. Check results when presenting persistence or erase success to users.

`setEnabled(false)` attempts one final synchronous flush of already accepted history, cancels pending work, then blocks recording & writes. History remains readable; enabling again resets consecutive-event dedup. A failed final flush stays dirty but is never retried while disabled. `clear()` remains available while disabled: it empties memory, cancels pending work, resets dedup, & attempts removal of current & every configured legacy key. Any removal failure returns `failed`. `dispose()` attempts one flush while enabled, cancels work, removes listeners, & clears memory; later calls are inert (`disposed`, `false`, or empty snapshots).

Storage format is `{ schema_version: 1, entries: [...] }`. Restore rejects input above 64 KiB of UTF-8 before parsing, validates every entry, projects only safe fields, & retains last 80 valid entries. Current key takes precedence, including malformed current data. Only an absent current key permits configured legacy keys, in order. Legacy arrays accept `{ t, kind, msg }` with valid timestamps & allowlisted labels; `detail` & other fields are discarded. A successful sanitized write precedes legacy-key removal; failed writes never delete legacy history. An unresolved storage read blocks writes: newly accepted entries stay bounded in memory, then merge after recovered sanitized history before persistence. `clear()` explicitly discards pending history & attempts erasure.

Crash capture formats structured `{ code, message }` failures, primitives, cycles, & nested own data through a private formatter. Unknown objects admit at most 8 keys per object, depth 3, & 32 visited values; accessors & inaccessible values become fixed markers, with no `toJSON`/`toString` calls. Intact strings share a 65,536-character admission budget per reason (including Error name, message, & stack); excess values become whole-value markers. Sources above 65,536 characters also become markers. Admitted strings are scrubbed before JSON escaping, source extraction, & existing output truncation. Existing rate limits, configured output limits, & listener count remain unchanged; formatted reason text goes only through existing native crash capture, never recorder history.

### Compose existing crash capture

Use existing `installWindowCrashCapture` once, adapting its invoke path. This example stores only fixed labels in recorder while forwarding existing scrubbed crash reports to app's authorized native command. App privacy gate covers both destinations; changing recorder state alone does not gate native forwarding. Capture's existing defaults & rate limits still apply.

```ts
import { invoke as nativeInvoke } from '@tauri-apps/api/core';
import {
  createDiagnosticsRecorder, installWindowCrashCapture,
  type LogsInvoke, type WindowCrashPayload,
} from '@rightkit/logs';

let diagnosticsEnabled = readAppPrivacySetting();
const recorder = createDiagnosticsRecorder({
  storageKey: 'app.main.diagnostics', // window-specific ownership
  allowedEvents: ['app.loaded', 'window.error', 'window.unhandledrejection'],
  enabled: diagnosticsEnabled,
});

const captureInvoke: LogsInvoke = async <T>(command: string, args?: Record<string, unknown>) => {
  if (!diagnosticsEnabled) return undefined as T;
  const report = args?.report as WindowCrashPayload;
  recorder.record(report.kind === 'error'
    ? { kind: 'error', event: 'window.error' }
    : { kind: 'rejection', event: 'window.unhandledrejection' });
  return nativeInvoke<T>(command, args);
};
const stopCapture = installWindowCrashCapture({ invoke: captureInvoke });
recorder.record({ kind: 'load', event: 'app.loaded' });

function updateDiagnosticsEnabled(enabled: boolean) {
  diagnosticsEnabled = enabled; // gate native forwarding immediately
  return recorder.setEnabled(enabled);
}

function teardown() {
  stopCapture();
  return recorder.dispose();
}
```

Settings persistence, cross-window privacy-setting synchronization, native-log erasure, support mail, & product UI remain app responsibilities. Exported types include `DiagnosticKind`, `DiagnosticInput`, `DiagnosticEntry`, `DiagnosticsRecorderConfig`, `DiagnosticsRecorder`, & `PersistenceResult`.

## Scrub parity with Rust

`scrubLogText` mirrors Rust `rightkit_logs::scrub::scrub_text`: URL userinfo, query and fragment removal, OAuth secret parameters, `Bearer` tokens, credential prefixes and JWTs, opaque 32-character runs, e-mail, IPv4 and home-directory user segments. The behaviour is pinned by one shared vector file, `crates/rightkit-logs/tests/scrub-parity.v1.json` in this package's source (not published to npm). `src/index.test.ts` runs every entry against `scrubLogText`; the Rust suite `crates/rightkit-logs/tests/scrub_parity.rs` runs every entry against `scrub_text`; `src/crash.test.ts` runs the entries marked `windowCrash` against `scrubWindowCrashText`, a narrower scrubber for window crash reports. Change a scrub rule on one side and the shared file fails the other. Quoted-value (`redact_quoted`) and full-path (`ScrubOptions::redact_paths`) redaction are Rust-only and outside the vectors.

## Known issues

- `settings-ui` peers this package with no upper bound (`>=0.1.0`).
