# @rightkit/dictation-ui npm consumer (npm.@rightkit/dictation-ui)

Packed-archive consumer for `@rightkit/dictation-ui` 0.2.0 (the workspace version). The unit is installed only from the sealed `file:` archive; the reviewed closure pins `react` 19.3.1, `react-dom` 19.3.1, `@rightkit/library-ui` 0.2.0, `@rightkit/platform-ui` 0.4.0, `@rightkit/react-shell` 0.7.2, `@rightkit/settings-ui` 0.7.0. Declared-only dependencies: `@phosphor-icons/react`.

Surface (exact `exports` keys): `.`, `./package.json`, `./pill`.

Scenario (1): the installed manifest is the reviewed package and its export keys match; the root entry imports and exposes `configureDictationHost`, `HomePage`, `DictationHistory`, `deliveryRow`.

Required environment: `RIGHTKIT_CONSUMER_ARTIFACT`, `RIGHTKIT_CONSUMER_ARTIFACT_SHA256`, `RIGHTKIT_CONSUMER_EXPECTED_VERSION` and `RIGHTKIT_CONSUMER_RESOLUTION` (`packed` or `fetched`). `provenance.mjs` is the shared byte-identical helper that binds every installed file to the supplied archive. No network, no credentials. Output is one JSON line; `nativeHostQualified` is always false.
