# @rightkit/library-ui npm consumer (npm.@rightkit/library-ui)

Packed-archive consumer for `@rightkit/library-ui` 0.2.1 (the workspace version). The unit is installed only from the sealed `file:` archive; the reviewed closure pins `react` 19.3.1, `react-dom` 19.3.1, `@rightkit/doc-render` 0.2.2, `@rightkit/media-ui` 0.3.6, `@rightkit/platform-ui` 0.4.1, `@rightkit/react-shell` 0.7.3, `@rightkit/tauri-shell` 0.1.4. Declared-only dependencies: `@phosphor-icons/react`, `@tauri-apps/api`, `@tauri-apps/plugin-dialog`, `lucide-react`.

Surface (exact `exports` keys): `.`, `./package.json`, `./queue`, `./styles.css`.

Scenario (1): the installed manifest is the reviewed package and its export keys match; the root entry imports and exposes `configureLibraryHost`, `LibraryScreen`, `LibraryReader`.

Required environment: `RIGHTKIT_CONSUMER_ARTIFACT`, `RIGHTKIT_CONSUMER_ARTIFACT_SHA256`, `RIGHTKIT_CONSUMER_EXPECTED_VERSION` and `RIGHTKIT_CONSUMER_RESOLUTION` (`packed` or `fetched`). `provenance.mjs` is the shared byte-identical helper that binds every installed file to the supplied archive. No network, no credentials. Output is one JSON line; `nativeHostQualified` is always false.
