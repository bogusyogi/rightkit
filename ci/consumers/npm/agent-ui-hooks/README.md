# @rightkit/agent-ui-hooks npm consumer (npm.@rightkit/agent-ui-hooks)

Packed-archive consumer for `@rightkit/agent-ui-hooks` 0.1.0 (the workspace version). The unit is installed only from the sealed `file:` archive; the reviewed closure pins `react` 19.3.1, `react-dom` 19.3.1, `@rightkit/app-update` 0.4.2, `@rightkit/react-shell` 0.7.2. Declared-only dependencies: `@rightkit/agent-ui-core`, `@tauri-apps/api`, `@tauri-apps/plugin-dialog`.

Surface (exact `exports` keys): `.`, `./*`, `./*.css`, `./*.json`.

Scenario (1): the installed manifest is the reviewed package and its export keys match; the root entry imports and exposes `setHostAppName`, `hostAppName`, `renderStructuredSessionExport`.

Required environment: `RIGHTKIT_CONSUMER_ARTIFACT`, `RIGHTKIT_CONSUMER_ARTIFACT_SHA256`, `RIGHTKIT_CONSUMER_EXPECTED_VERSION` and `RIGHTKIT_CONSUMER_RESOLUTION` (`packed` or `fetched`). `provenance.mjs` is the shared byte-identical helper that binds every installed file to the supplied archive. No network, no credentials. Output is one JSON line; `nativeHostQualified` is always false.
