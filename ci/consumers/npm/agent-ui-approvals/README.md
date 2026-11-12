# @rightkit/agent-ui-approvals npm consumer (npm.@rightkit/agent-ui-approvals)

Packed-archive consumer for `@rightkit/agent-ui-approvals` (the workspace version, from `RIGHTKIT_CONSUMER_EXPECTED_VERSION`). The unit is installed only from the sealed `file:` archive; the reviewed closure pins `react` 19.3.1, `react-dom` 19.3.1, `@rightkit/app-update` 0.4.2, `@rightkit/platform-ui` 0.4.0, `@rightkit/react-shell` 0.7.2, `@rightkit/settings-ui` 0.7.0, `@rightkit/mail-ui` 0.2.1 (needs 0.2.1 on npm; until then the run overrides it with the packed archive), `@rightkit/system-ui` 0.2.0. Declared-only dependencies: `@tauri-apps/api`, `lucide-react`.

Surface (exact `exports` keys): `.`, `./*`, `./*.css`, `./*.json`.

Scenario (1): the installed manifest is the reviewed package and its export keys match; the root entry imports and exposes `setHostAppName`, `hostAppName`, `AgentApprovalCard`, `agentScopes`, `auditScope`; `agentScopes` and `auditScope` read both agent wires (gateway 0.3.0 `scopes`/`scope`, older `accounts`/`account_id`).

Required environment: `RIGHTKIT_CONSUMER_ARTIFACT`, `RIGHTKIT_CONSUMER_ARTIFACT_SHA256`, `RIGHTKIT_CONSUMER_EXPECTED_VERSION` and `RIGHTKIT_CONSUMER_RESOLUTION` (`packed` or `fetched`). `provenance.mjs` is the shared byte-identical helper that binds every installed file to the supplied archive. No network, no credentials. Output is one JSON line; `nativeHostQualified` is always false.
