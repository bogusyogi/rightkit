# @rightkit/config npm consumer (npm.@rightkit/config)

Packed-archive consumer for `@rightkit/config` 0.1.0 (the workspace version). The unit is installed only from the sealed `file:` archive; the reviewed closure pins nothing beyond the archive. Declared-only dependencies: `vite`.

Surface (exact `exports` keys): `.`, `./eslint`, `./rust/*`, `./tsconfig/base.json`, `./tsconfig/library.json`, `./tsconfig/react-app.json`, `./vite`.

Scenario (1): the installed manifest is the reviewed package and its export keys match; the root entry imports and exposes `DEFAULT_WATCH_IGNORED`, `rightkitViteConfig`, `DEFAULT_ESLINT_IGNORES`.

Required environment: `RIGHTKIT_CONSUMER_ARTIFACT`, `RIGHTKIT_CONSUMER_ARTIFACT_SHA256`, `RIGHTKIT_CONSUMER_EXPECTED_VERSION` and `RIGHTKIT_CONSUMER_RESOLUTION` (`packed` or `fetched`). `provenance.mjs` is the shared byte-identical helper that binds every installed file to the supplied archive. No network, no credentials. Output is one JSON line; `nativeHostQualified` is always false.
