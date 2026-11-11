# @rightkit/workspace-ui npm consumer (npm.@rightkit/workspace-ui)

Packed-archive consumer for `@rightkit/workspace-ui` (the workspace version, from `RIGHTKIT_CONSUMER_EXPECTED_VERSION`). The unit is installed only from the sealed `file:` archive; the reviewed closure pins `react` 19.3.1, `react-dom` 19.3.1, `@rightkit/platform-ui` 0.4.1 (the peer of `@rightkit/react-shell`), `@rightkit/react-shell` 0.7.3, `@rightkit/settings-ui` 0.7.1, `lucide-react` 1.66.0. No declared-only dependencies (0.2.0 no longer depends on `@rightkit/mail-ui`).

Surface (exact `exports` keys): `.`, `./client`, `./host`, `./lib/avatar`, `./lib/calendarUtils`, `./package.json`.

Scenario (1): the installed manifest is the reviewed package and its export keys match; the root entry imports and exposes the vCard helpers, the host contract (`WorkspaceHostProvider`, `useWorkspaceHost`, `useWorkspaceState`), the five views, the navs, `useDrive`, the avatar helpers and `WEEKDAYS`; a nav rendered outside a `WorkspaceHostProvider` refuses (no transport of its own); `lib/avatar` gives the same colour as the root export.

Required environment: `RIGHTKIT_CONSUMER_ARTIFACT`, `RIGHTKIT_CONSUMER_ARTIFACT_SHA256`, `RIGHTKIT_CONSUMER_EXPECTED_VERSION` and `RIGHTKIT_CONSUMER_RESOLUTION` (`packed` or `fetched`). `provenance.mjs` is the shared byte-identical helper that binds every installed file to the supplied archive. No network, no credentials. Output is one JSON line; `nativeHostQualified` is always false.
