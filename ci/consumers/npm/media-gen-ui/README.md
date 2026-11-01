# @rightkit/media-gen-ui npm consumer (npm.@rightkit/media-gen-ui)

Packed-archive consumer for `@rightkit/media-gen-ui` 0.1.0 (the workspace version). The unit is installed only from the sealed `file:` archive; the reviewed closure pins `react` 19.3.1, `react-dom` 19.3.1, `@rightkit/app-update` 0.4.2, `@rightkit/legal` 0.6.3, `@rightkit/legal-ui` 0.4.1, `@rightkit/license` 0.3.0, `@rightkit/logs` 0.1.6, `@rightkit/media-ui` 0.3.4, `@rightkit/onboarding` 0.6.3, `@rightkit/platform-ui` 0.4.0, `@rightkit/react-shell` 0.7.2, `@rightkit/settings-ui` 0.7.0, `@rightkit/tauri-shell` 0.1.3. Declared-only dependencies: `@tauri-apps/api`, `@tauri-apps/plugin-process`, `@tauri-apps/plugin-updater`, `tailwindcss`.

Surface (exact `exports` keys): `.`, `./*`, `./*.css`, `./*.json`.

Scenario (1): the installed manifest is the reviewed package and its export keys match; the root entry imports and exposes `setHostAppName`, `hostAppName`, `Balances`.

Required environment: `RIGHTKIT_CONSUMER_ARTIFACT`, `RIGHTKIT_CONSUMER_ARTIFACT_SHA256`, `RIGHTKIT_CONSUMER_EXPECTED_VERSION` and `RIGHTKIT_CONSUMER_RESOLUTION` (`packed` or `fetched`). `provenance.mjs` is the shared byte-identical helper that binds every installed file to the supplied archive. No network, no credentials. Output is one JSON line; `nativeHostQualified` is always false.
