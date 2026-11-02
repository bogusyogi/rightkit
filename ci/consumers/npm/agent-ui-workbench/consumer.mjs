import { validateInstalledArtifact } from './provenance.mjs';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import path from 'node:path';

const recipeId = 'agent-ui-workbench-root-export-consumer';
const result = { recipeId, schemaVersion: 1, ok: false, host: { platform: process.platform, arch: process.arch, node: process.version }, artifact: null, scenarios: [], errors: [], nativeHostQualified: false };
const failure = error => ({ name: error?.name ?? 'Error', message: String(error?.message ?? error) });
const required = name => { assert(process.env[name], `${name} is required`); return process.env[name]; };
const surface = [".", "./components/RuntimePane", "./components/VirtualizedCodeBlock", "./components/runtime/ArtifactRowsPanel", "./components/runtime/MarkdownArtifactView", "./components/runtime/MemoryRuntimePanel", "./components/runtime/RecipeScheduleDialog", "./components/runtime/SchedulesRuntimePanel", "./components/runtime/TeamsRuntimePanel", "./components/runtime/WorkspaceFileViewer", "./components/workbench/ApprovalDetails", "./components/workbench/ApprovalKeyHandler", "./components/workbench/AwaySummaryCard", "./components/workbench/ContextBadges", "./components/workbench/ConversationFindBar", "./components/workbench/DispatchBadge", "./components/workbench/DriverLifecycleStrip", "./components/workbench/PermissionCard", "./components/workbench/PlanPrompt", "./components/workbench/QuestionPrompt", "./components/workbench/QueuedMessageRow", "./components/workbench/ReasoningBlock", "./components/workbench/RenderedMessageContent", "./components/workbench/RunAnnouncer", "./components/workbench/SessionActionBar", "./components/workbench/SessionEmptyState", "./components/workbench/SessionMinimap", "./components/workbench/StructuredQuestionCard", "./components/workbench/TasksPane", "./components/workbench/TranscriptBubble", "./components/workbench/TranscriptSearchBar", "./components/workbench/TranscriptStream", "./components/workbench/WorkbenchContentProps", "./components/workbench/references/DocumentPreview", "./components/workbench/references/ImageGallery", "./components/workbench/references/ReferenceChipStrip", "./components/workbench/useConversationFind", "./package.json"];
const expected = ["setHostAppName", "hostAppName", "scoreColor"];

try {
  const archive = path.resolve(required('RIGHTKIT_CONSUMER_ARTIFACT'));
  const sha256 = required('RIGHTKIT_CONSUMER_ARTIFACT_SHA256');
  const version = required('RIGHTKIT_CONSUMER_EXPECTED_VERSION');
  const resolution = required('RIGHTKIT_CONSUMER_RESOLUTION');
  assert(['packed', 'fetched'].includes(resolution));
  assert.match(sha256, /^[a-f0-9]{64}$/);
  assert.equal(createHash('sha256').update(readFileSync(archive)).digest('hex'), sha256);
  const root = import.meta.resolve('@rightkit/agent-ui-workbench');
  const provenance = validateInstalledArtifact({ fixtureUrl: import.meta.url, archive, sha256, version, resolution, packageName: '@rightkit/agent-ui-workbench', rootResolvedUrl: root, entries: { '@rightkit/agent-ui-workbench': root } });
  result.artifact = provenance.artifact;
  const manifest = JSON.parse(readFileSync(path.join(path.dirname(provenance.artifact.modulePath), '..', 'package.json'), 'utf8'));

  async function run(name, operation) {
    const entry = { name, ok: false, evidence: null, error: null };
    result.scenarios.push(entry);
    try { entry.evidence = await operation(); entry.ok = true; }
    catch (error) { entry.error = failure(error); throw error; }
  }

  await run('root-export-resolves-and-exposes-reviewed-names', async () => {
    assert.equal(manifest.name, '@rightkit/agent-ui-workbench');
    assert.deepEqual(Object.keys(manifest.exports ?? {}).sort(), surface);
    const mod = await import(provenance.entries['@rightkit/agent-ui-workbench']);
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
