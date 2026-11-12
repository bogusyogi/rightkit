# @rightkit/agent-ui-core npm consumer (npm.@rightkit/agent-ui-core)

Packed-archive consumer for `@rightkit/agent-ui-core` 0.1.1 (the workspace version). The unit is installed only from the sealed `file:` archive; the reviewed closure pins `react` 19.3.1, `react-dom` 19.3.1, `@rightkit/app-update` 0.4.3, `@rightkit/ci` 0.3.2, `@rightkit/legal` 0.6.3, `@rightkit/legal-ui` 0.4.2, `@rightkit/license` 0.3.1, `@rightkit/logs` 0.1.6, `@rightkit/onboarding` 0.6.4, `@rightkit/outdated` 0.3.1, `@rightkit/platform-ui` 0.4.1, `@rightkit/react-shell` 0.7.3, `@rightkit/release` 0.6.52, `@rightkit/tauri-shell` 0.1.4, `@rightkit/theme` 0.3.2. Declared-only dependencies: `@phosphor-icons/react`, `@tauri-apps/api`, `@tauri-apps/plugin-process`, `tailwindcss`, `zustand`.

Surface (exact `exports` keys): `.`, `./*`, `./*.css`, `./*.json`.

Scenario (1): the installed manifest is the reviewed package and its export keys match; the root entry imports and exposes `setHostAppName`, `hostAppName`, `TaskIndicator`.

Required environment: `RIGHTKIT_CONSUMER_ARTIFACT`, `RIGHTKIT_CONSUMER_ARTIFACT_SHA256`, `RIGHTKIT_CONSUMER_EXPECTED_VERSION` and `RIGHTKIT_CONSUMER_RESOLUTION` (`packed` or `fetched`). `provenance.mjs` is the shared byte-identical helper that binds every installed file to the supplied archive. No network, no credentials. Output is one JSON line; `nativeHostQualified` is always false.
