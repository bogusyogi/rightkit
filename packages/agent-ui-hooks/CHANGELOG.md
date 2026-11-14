# Changelog

## 0.1.1

- The history poll of the event stream and the driver status poll use `setVisibleInterval`: no tick while the window is hidden. Its `@tauri-apps/api` `invoke` and `listen` calls go through `@rightkit/agent-ui-core/host/tauriHost`.
- `useBrowserWorkspace` treats "session not found" from a background load as the empty state, not an error.
- The session export (`renderStructuredSessionMarkdown`, `renderTranscriptMarkdown`) titles an assistant turn with the `assistantExportLabel` host option (default "Assistant") and tolerates a message without `toolCalls`.
- `appName` re-exports `@rightkit/agent-ui-core/appName`, so `setHostAppName` and `setHostOptions` are shared with core.

## 0.1.0

- First release. React hooks for coding-agent UIs: sessions, composer, workspace files, approvals and runtime.
