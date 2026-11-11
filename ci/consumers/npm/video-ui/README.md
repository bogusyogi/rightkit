# @rightkit/video-ui npm consumer (npm.@rightkit/video-ui)

Packed-archive consumer for `@rightkit/video-ui` 0.2.0 (the workspace version). The unit is installed only from the sealed `file:` archive; the reviewed closure pins `react` 19.3.1, `react-dom` 19.3.1, `@rightkit/media-ui` 0.3.5, `@rightkit/platform-ui` 0.4.1 and `@rightkit/react-shell` 0.7.3. No declared-only dependencies.

Surface (exact `exports` keys): `.`, `./package.json`, `./styles.css`.

Scenario (1): the installed manifest is the reviewed package and its export keys match; the root entry imports and exposes `installVideoHost`, `videoHost`, `TimelineEditorView`, `Timeline`, `NativePlayer`, `engineCall`, `projectShow` and `timecode`, and none of the app shell (`setHostAppName`, `hostAppName`, `Home`, `SettingsView`, `DesktopWorkspace`); `videoHost()` refuses before a host is installed; after `installVideoHost`, `engineCall` goes through the host's engine and `assetUrl` through its `assetUrl`.

Required environment: `RIGHTKIT_CONSUMER_ARTIFACT`, `RIGHTKIT_CONSUMER_ARTIFACT_SHA256`, `RIGHTKIT_CONSUMER_EXPECTED_VERSION` and `RIGHTKIT_CONSUMER_RESOLUTION` (`packed` or `fetched`). `provenance.mjs` is the shared byte-identical helper that binds every installed file to the supplied archive. No network, no credentials. Output is one JSON line; `nativeHostQualified` is always false.
