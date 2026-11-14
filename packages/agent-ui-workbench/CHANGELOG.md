# Changelog

## 0.1.1

- The webhook trigger panel's secret placeholder is the `webhookSecretEnv` host option (default `RIGHTKIT_WEBHOOK_SECRET`). `TranscriptStream` takes an optional `hasHistory` (default false) that hides the empty state when history is shown above. The driver lifecycle strip polls with `setVisibleInterval`.
- `appName` re-exports `@rightkit/agent-ui-core/appName`; `invoke` and `listen` go through `@rightkit/agent-ui-core/host/tauriHost`.

## 0.1.0

- First release. Workbench and runtime panels for coding-agent UIs.
