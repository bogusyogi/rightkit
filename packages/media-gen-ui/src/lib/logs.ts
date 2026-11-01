import { invoke } from '@tauri-apps/api/core';
import { createLogsClient } from '@rightkit/logs';

/**
 * Frontend structured logging (rule M15): the shared @rightkit/logs client writes schema-v1 JSONL
 * through the host's rightkit_logs_* commands. Local only; nothing uploads. Only the fields named
 * here reach disk, so never pass user content (prompts, file paths, keys) as a field.
 */
export const logs = createLogsClient({
  invoke,
  allowedFields: ['surface', 'code', 'kind', 'count', 'duration_ms', 'component'],
  maxBufferedEvents: 256,
  flushIntervalMs: 500,
});

/** Records frontend start. */
export function installFrontendDiagnostics(): void {
  logs.info('frontend_started', { surface: 'shell' });
}
