# @rightkit/document-core npm consumer (npm.@rightkit/document-core)

Packed-archive consumer for `@rightkit/document-core` 0.1.2 (the workspace version). The unit is installed only from the sealed `file:` archive; the reviewed closure pins `react` 19.3.1, `react-dom` 19.3.1, `@rightkit/license` 0.3.1, `@rightkit/logs` 0.1.6, `@rightkit/pdf-ui` 0.3.0, `@rightkit/platform-ui` 0.4.1, `@rightkit/react-shell` 0.7.3, `@rightkit/settings-ui` 0.7.1, `@rightkit/tauri-shell` 0.1.4, `@rightkit/theme` 0.3.2. Declared-only dependencies: `@codemirror/view`, `@phosphor-icons/react`, `@tauri-apps/api`, `pdfjs-dist`.

Surface (exact `exports` keys): `.`, `./*`, `./*.css`, `./*.json`.

Scenario (1): the installed manifest is the reviewed package and its export keys match; the root entry imports and exposes `BlockDraftProvider`, `useBlockDrafts`, `useOptionalBlockDrafts`.

Required environment: `RIGHTKIT_CONSUMER_ARTIFACT`, `RIGHTKIT_CONSUMER_ARTIFACT_SHA256`, `RIGHTKIT_CONSUMER_EXPECTED_VERSION` and `RIGHTKIT_CONSUMER_RESOLUTION` (`packed` or `fetched`). `provenance.mjs` is the shared byte-identical helper that binds every installed file to the supplied archive. No network, no credentials. Output is one JSON line; `nativeHostQualified` is always false.
