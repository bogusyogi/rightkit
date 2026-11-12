# @rightkit/system-ui npm consumer (npm.@rightkit/system-ui)

Packed-archive consumer for `@rightkit/system-ui` (the workspace version, from `RIGHTKIT_CONSUMER_EXPECTED_VERSION`). The unit is installed only from the sealed `file:` archive; the reviewed closure pins `react` 19.3.1, `react-dom` 19.3.1, `@rightkit/platform-ui` 0.4.1, `@rightkit/react-shell` 0.7.4, `@rightkit/settings-ui` 0.7.2, `lucide-react` 1.66.0. No declared-only dependencies (0.2.0 has no Tauri, logs, onboarding or legal dependency).

Surface (exact `exports` keys): `.`, `./activityLease`, `./api`, `./components/Chart`, `./host`, `./metrics`, `./package.json`, `./styles.css`, `./views/Storage`, `./views/monitor.css`, `./visible`.

Scenario (1): the installed manifest is the reviewed package and its export keys match; the root entry imports and exposes the views, the clients, `configureSystemHost`/`systemHost` and the helpers, and none of the non-system modules (chat, nearby, launcher settings, device API, share API, QR); `api.status()` refuses before `configureSystemHost`, and afterwards `api` and `appsApi` reach the host's clients and `systemHost().appName` is the host's.

Required environment: `RIGHTKIT_CONSUMER_ARTIFACT`, `RIGHTKIT_CONSUMER_ARTIFACT_SHA256`, `RIGHTKIT_CONSUMER_EXPECTED_VERSION` and `RIGHTKIT_CONSUMER_RESOLUTION` (`packed` or `fetched`). `provenance.mjs` is the shared byte-identical helper that binds every installed file to the supplied archive. No network, no credentials. Output is one JSON line; `nativeHostQualified` is always false.
