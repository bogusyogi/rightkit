# @rightkit/workspace-ui npm consumer (npm.@rightkit/workspace-ui)

Packed-archive consumer for `@rightkit/workspace-ui` 0.1.0 (the workspace version). The unit is installed only from the sealed `file:` archive; the reviewed closure pins `react` 19.3.1, `react-dom` 19.3.1, `@rightkit/react-shell` 0.7.2, `@rightkit/settings-ui` 0.7.0. Declared-only dependencies: `@rightkit/mail-ui`.

Surface (exact `exports` keys): `.`, `./*`, `./*.css`, `./*.json`.

Scenario (1): the installed manifest is the reviewed package and its export keys match; the root entry imports and exposes `buildVcf`, `parseVcf`, `parseContactsCsv`.

Required environment: `RIGHTKIT_CONSUMER_ARTIFACT`, `RIGHTKIT_CONSUMER_ARTIFACT_SHA256`, `RIGHTKIT_CONSUMER_EXPECTED_VERSION` and `RIGHTKIT_CONSUMER_RESOLUTION` (`packed` or `fetched`). `provenance.mjs` is the shared byte-identical helper that binds every installed file to the supplied archive. No network, no credentials. Output is one JSON line; `nativeHostQualified` is always false.
