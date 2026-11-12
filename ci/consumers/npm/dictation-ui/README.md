# @rightkit/dictation-ui npm consumer (npm.@rightkit/dictation-ui)

Packed-archive consumer for `@rightkit/dictation-ui` 0.2.1 (the workspace version). The unit is installed only from the sealed `file:` archive; the reviewed closure pins `react` 19.3.1, `react-dom` 19.3.1, `@rightkit/app-update` 0.4.4, `@rightkit/doc-render` 0.2.2, `@rightkit/library-ui` 0.2.1, `@rightkit/license` 0.3.2, `@rightkit/logs` 0.1.6, `@rightkit/media-ui` 0.3.6, `@rightkit/platform-ui` 0.4.1, `@rightkit/react-shell` 0.7.4, `@rightkit/settings-ui` 0.7.2, `@rightkit/tauri-shell` 0.1.4, `@rightkit/theme` 0.3.3. Declared-only dependencies: `@phosphor-icons/react`.

Surface (exact `exports` keys): `.`, `./package.json`, `./pill`.

Scenario (1): the installed manifest is the reviewed package and its export keys match; the root entry imports and exposes `configureDictationHost`, `HomePage`, `DictationHistory`, `deliveryRow`, `deliveryFilterKey`, `deliveryQueryArgs`; a delivery row shows the friendly app name (`Google Chrome`) and its `origin.filterKey` is the raw process name (`chrome.exe`) that `deliveryQueryArgs` sends as `app`.

Required environment: `RIGHTKIT_CONSUMER_ARTIFACT`, `RIGHTKIT_CONSUMER_ARTIFACT_SHA256`, `RIGHTKIT_CONSUMER_EXPECTED_VERSION` and `RIGHTKIT_CONSUMER_RESOLUTION` (`packed` or `fetched`). `provenance.mjs` is the shared byte-identical helper that binds every installed file to the supplied archive. No network, no credentials. Output is one JSON line; `nativeHostQualified` is always false.
