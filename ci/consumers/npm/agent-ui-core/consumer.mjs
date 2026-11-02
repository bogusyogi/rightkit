import { validateInstalledArtifact } from './provenance.mjs';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import path from 'node:path';

const recipeId = 'agent-ui-core-root-export-consumer';
const result = { recipeId, schemaVersion: 1, ok: false, host: { platform: process.platform, arch: process.arch, node: process.version }, artifact: null, scenarios: [], errors: [], nativeHostQualified: false };
const failure = error => ({ name: error?.name ?? 'Error', message: String(error?.message ?? error) });
const required = name => { assert(process.env[name], `${name} is required`); return process.env[name]; };
const surface = [".", "./components/CommandPalette", "./components/Icon", "./components/PaneErrorBoundary", "./components/PaneLoading", "./components/app/AppShellHeader", "./components/app/WorkspaceTrustGate", "./components/app/types", "./extensions/ExtensionSlot", "./hooks/palette/usePaletteRecents", "./hooks/runtime/useSchedulesRuntime", "./hooks/runtime/useWebhookTriggersRuntime", "./hooks/useConfirmationDialog", "./hooks/useGoalManagement", "./hooks/useLicenseState", "./hooks/useLspSettings", "./hooks/useSubAgents", "./lib/accounts/accounts", "./lib/analytics/analyticsProjection", "./lib/api-contract", "./lib/appSurfaceProps/backgroundTasksTypes", "./lib/claudeModels", "./lib/cliProviders", "./lib/cliUpdate/updateProjection", "./lib/composer/completionRanking", "./lib/composer/editorProjection", "./lib/concurrency/runtimeGuards", "./lib/conductorModels", "./lib/conductorOptions", "./lib/connection/connectionAttemptState", "./lib/connection/connectionErrorProjection", "./lib/connection/recovery/recoveryPolicy", "./lib/daemon", "./lib/daemon/browserWorkspace", "./lib/daemon/cli-providers", "./lib/daemon/config-host", "./lib/daemon/delivery-environments-attention", "./lib/daemon/drivers", "./lib/daemon/inference", "./lib/daemon/marketplaces", "./lib/daemon/memright", "./lib/daemon/streamReconcile", "./lib/daemon/workflows", "./lib/daemon/workspace-files", "./lib/git/gitIndexMutationQueue", "./lib/messageNotices", "./lib/notifications", "./lib/orchestrationNotices", "./lib/performance/longTranscriptRender", "./lib/pluginContributionProjection", "./lib/pluginContributionTypes", "./lib/promptTemplates/compilePrompt", "./lib/promptTemplates/roles", "./lib/promptTemplates/styles", "./lib/promptTemplates/templates", "./lib/providerPresets", "./lib/remote/remoteClient", "./lib/secureStore", "./lib/sessionShareCode", "./lib/shellWindow", "./lib/shortcuts/bindings", "./lib/shortcuts/dispatcher", "./lib/shortcuts/schema", "./lib/statusToast", "./lib/streamDeltaBatcher", "./lib/terminal/suggest/ghostText", "./lib/terminal/suggest/suggestionEngine", "./lib/tools/managedToolHealth", "./lib/tools/useManagedToolHealth", "./lib/transcriptGroups", "./lib/transcriptMessages", "./lib/transcriptSearch", "./lib/types", "./lib/types/cliProviders", "./lib/types/inference", "./lib/types/references", "./lib/types/settings-plugins", "./lib/types/workspace-files", "./lib/usage/turnUsageProjection", "./lib/usage/usageProjection", "./lib/utils", "./lib/worktrees/worktreeProjectionManager", "./package.json", "./qaBuild", "./state/accountSelectionStore", "./state/attentionStore", "./state/browserStore", "./state/composerModeStore", "./state/composerScopeStore", "./state/composerStore", "./state/connectionProfiles", "./state/deliveryStore", "./state/diagnosticsStore", "./state/editorBufferStore", "./state/environmentsStore", "./state/executionStore", "./state/goalsStore", "./state/importedThemeStore", "./state/layoutStore", "./state/legal", "./state/license", "./state/localModelStore", "./state/paletteStore", "./state/platform", "./state/proBridge", "./state/projectRecentsStore", "./state/rateLimitStore", "./state/referenceStore", "./state/rightPanelStore", "./state/sessionAttentionStore", "./state/sidePanelStore", "./state/streamingStore", "./state/subagentsStore", "./state/telegramStore", "./state/themeStore", "./state/updates", "./state/workspaceStore", "./types/browserWorkspace"];
const expected = ["setHostAppName", "hostAppName", "TaskIndicator"];

try {
  const archive = path.resolve(required('RIGHTKIT_CONSUMER_ARTIFACT'));
  const sha256 = required('RIGHTKIT_CONSUMER_ARTIFACT_SHA256');
  const version = required('RIGHTKIT_CONSUMER_EXPECTED_VERSION');
  const resolution = required('RIGHTKIT_CONSUMER_RESOLUTION');
  assert(['packed', 'fetched'].includes(resolution));
  assert.match(sha256, /^[a-f0-9]{64}$/);
  assert.equal(createHash('sha256').update(readFileSync(archive)).digest('hex'), sha256);
  const root = import.meta.resolve('@rightkit/agent-ui-core');
  const provenance = validateInstalledArtifact({ fixtureUrl: import.meta.url, archive, sha256, version, resolution, packageName: '@rightkit/agent-ui-core', rootResolvedUrl: root, entries: { '@rightkit/agent-ui-core': root } });
  result.artifact = provenance.artifact;
  const manifest = JSON.parse(readFileSync(path.join(path.dirname(provenance.artifact.modulePath), '..', 'package.json'), 'utf8'));

  async function run(name, operation) {
    const entry = { name, ok: false, evidence: null, error: null };
    result.scenarios.push(entry);
    try { entry.evidence = await operation(); entry.ok = true; }
    catch (error) { entry.error = failure(error); throw error; }
  }

  await run('root-export-resolves-and-exposes-reviewed-names', async () => {
    assert.equal(manifest.name, '@rightkit/agent-ui-core');
    assert.deepEqual(Object.keys(manifest.exports ?? {}).sort(), surface);
    const mod = await import(provenance.entries['@rightkit/agent-ui-core']);
    for (const key of expected) assert.ok(key in mod, `missing root export ${key}`);
    return { checked: expected, exportKeys: surface.length };
  });

  assert.equal(result.scenarios.length, 1);
  result.ok = true;
} catch (error) {
  result.errors.push(failure(error));
  process.exitCode = 1;
}
process.stdout.write(`${JSON.stringify(result)}\n`);
