# @rightkit/logs

Local-only structured logging for Right Suite webviews. It has no network or
Tauri dependency: each app injects its existing `invoke` function and provides
a closed allowlist of content-free fields.

The SDK batches schema-v1 JSON events into three shared native commands:

- `rightkit_logs_write`
- `rightkit_logs_collect`
- `rightkit_logs_clear`

The Rust host implements those commands with the published `rightkit-logs` crate.
Nothing uploads automatically; the user explicitly collects a diagnostics
bundle and decides whether to share it.

Install `rightkit-logs` from crates.io at the version pinned by RightKit's
release manifest. Canonical Rust source lives only in `crates/rightkit-logs`;
app repositories must not vendor npm-carried or local copies.

```ts
import { invoke } from '@tauri-apps/api/core';
import { createLogsClient } from '@rightkit/logs';

export const logs = createLogsClient({
  invoke,
  allowedFields: ['surface', 'count', 'duration_ms', 'code'],
});

logs.info('app_started', { surface: 'shell' });
```

## Publish

This package is public/free on npm under the RightKit organization. The
publishing token must have read/write access to all `@rightkit` packages and
bypass 2FA for automation, then run:

```powershell
pnpm --filter @rightkit/logs build
npm publish --workspace packages/logs
```
