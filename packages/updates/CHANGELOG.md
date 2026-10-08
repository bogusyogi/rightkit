# Changelog

## 0.2.5 — 2026-10-08

- Emit `ready` once, after `downloadAndInstall` resolves successfully, for manual
  & automatic installs. Download `Finished` events no longer signal readiness.
- Installation failures emit typed `error` status & call `onError` without
  emitting `ready`, preventing premature Restart prompts.
- Preserve public types, method signatures & download progress events.
