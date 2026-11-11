# @rightkit/agent-ui-components npm consumer (npm.@rightkit/agent-ui-components)

Packed-archive consumer for `@rightkit/agent-ui-components` 0.1.1 (the workspace version). The unit is installed only from the sealed `file:` archive; the reviewed closure pins `react` 19.3.1, `react-dom` 19.3.1, `@rightkit/app-update` 0.4.2, `@rightkit/legal-ui` 0.4.1, `@rightkit/platform-ui` 0.4.0, `@rightkit/react-shell` 0.7.2, `@rightkit/settings-ui` 0.7.0. Declared-only dependencies: `@rightkit/agent-ui-core`, `@rightkit/agent-ui-hooks`, `@rightkit/agent-ui-settings`, `@rightkit/agent-ui-workbench`, `@rightkit/terminal-ui`, `@tauri-apps/api`, `@tauri-apps/plugin-dialog`, `zustand`.

Surface (exact `exports` keys): `.`, `./*`, `./*.css`, `./*.json`.

Scenario (1): the installed manifest is the reviewed package and its export keys match; the root entry imports and exposes `setHostAppName`, `hostAppName`, `CliSessionBrowser`.

Required environment: `RIGHTKIT_CONSUMER_ARTIFACT`, `RIGHTKIT_CONSUMER_ARTIFACT_SHA256`, `RIGHTKIT_CONSUMER_EXPECTED_VERSION` and `RIGHTKIT_CONSUMER_RESOLUTION` (`packed` or `fetched`). `provenance.mjs` is the shared byte-identical helper that binds every installed file to the supplied archive. No network, no credentials. Output is one JSON line; `nativeHostQualified` is always false.
