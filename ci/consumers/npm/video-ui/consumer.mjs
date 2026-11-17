import { validateInstalledArtifact } from './provenance.mjs';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import path from 'node:path';

const recipeId = 'video-ui-root-export-consumer';
const result = { recipeId, schemaVersion: 1, ok: false, host: { platform: process.platform, arch: process.arch, node: process.version }, artifact: null, scenarios: [], errors: [], nativeHostQualified: false };
const failure = error => ({ name: error?.name ?? 'Error', message: String(error?.message ?? error) });
const required = name => { assert(process.env[name], `${name} is required`); return process.env[name]; };
const surface = [".", "./app", "./app.css", "./package.json", "./styles.css", "./tauri", "./workspace"];
const expected = ["installVideoHost", "videoHost", "TimelineEditorView", "Timeline", "NativePlayer", "engineCall", "projectShow", "timecode"];
const appExpected = ["DesktopWorkspace", "Home", "SettingsView", "SourcesView", "AutoEditView", "ShortsView", "ExportsView", "OnboardingGate", "configureLegal", "setHostAppName", "hostAppName", "themeStore", "shellBridge", "mediaAdapters", "logs", "licenseClient", "updates", "useDemo", "demoStatus", "pickFolder", "videoAppHost", "installVideoHost", "TimelineEditorView"];

try {
  const archive = path.resolve(required('RIGHTKIT_CONSUMER_ARTIFACT'));
  const sha256 = required('RIGHTKIT_CONSUMER_ARTIFACT_SHA256');
  const version = required('RIGHTKIT_CONSUMER_EXPECTED_VERSION');
  const resolution = required('RIGHTKIT_CONSUMER_RESOLUTION');
  assert(['packed', 'fetched'].includes(resolution));
  assert.match(sha256, /^[a-f0-9]{64}$/);
  assert.equal(createHash('sha256').update(readFileSync(archive)).digest('hex'), sha256);
  const root = import.meta.resolve('@rightkit/video-ui');
  const app = import.meta.resolve('@rightkit/video-ui/app');
  const tauri = import.meta.resolve('@rightkit/video-ui/tauri');
  const workspace = import.meta.resolve('@rightkit/video-ui/workspace');
  const provenance = validateInstalledArtifact({ fixtureUrl: import.meta.url, archive, sha256, version, resolution, packageName: '@rightkit/video-ui', rootResolvedUrl: root, entries: { '@rightkit/video-ui': root, '@rightkit/video-ui/app': app, '@rightkit/video-ui/tauri': tauri, '@rightkit/video-ui/workspace': workspace } });
  result.artifact = provenance.artifact;
  const manifest = JSON.parse(readFileSync(path.join(path.dirname(provenance.artifact.modulePath), '..', 'package.json'), 'utf8'));

  async function run(name, operation) {
    const entry = { name, ok: false, evidence: null, error: null };
    result.scenarios.push(entry);
    try { entry.evidence = await operation(); entry.ok = true; }
    catch (error) { entry.error = failure(error); throw error; }
  }

  await run('root-export-resolves-and-exposes-reviewed-names', async () => {
    assert.equal(manifest.name, '@rightkit/video-ui');
    assert.deepEqual(Object.keys(manifest.exports ?? {}).sort(), surface);
    // The root entry stays the editing components over a host: no app shell there.
    const mod = await import(provenance.entries['@rightkit/video-ui']);
    for (const key of expected) assert.ok(key in mod, `missing root export ${key}`);
    for (const gone of ["setHostAppName", "hostAppName", "Home", "SettingsView", "DesktopWorkspace"]) assert.ok(!(gone in mod), `app shell export ${gone} is on the root entry`);
    assert.throws(() => mod.videoHost(), /installVideoHost/);
    const calls = [];
    const appCalls = [];
    const host = {
      engine: { call: async (method, params) => { calls.push([method, params]); return { ok: true }; }, onEvent: async () => () => {}, status: async () => ({ state: 'ready', hello: null, restarts: 0, last_error: null }) },
      playback: {}, assetUrl: p => `asset://${p}`, allowMediaScope: async () => {}, mediaSourceId: async p => p, revealPath: async () => {},
    };
    mod.installVideoHost(host);
    assert.deepEqual(await mod.engineCall('project.show', { dir: '/p' }), { ok: true });
    assert.deepEqual(calls, [['project.show', { dir: '/p' }]]);
    await mod.reframeAnalyzeMissing('/p', 'source-one', '9:16');
    const reviewedHash = 'a'.repeat(64);
    await mod.reframeApproveChecked('/p', 'source-one', { aspect: '9:16', expected_plan_hash: reviewedHash, approver: 'ui' });
    assert.deepEqual(calls.slice(1), [
      ['reframe.analyze_missing', { dir: '/p', source: 'source-one', aspect: '9:16' }],
      ['reframe.approve_checked', { dir: '/p', source: 'source-one', all: true, aspect: '9:16', expected_plan_hash: reviewedHash, approver: 'ui' }],
    ]);
    assert.equal(mod.assetUrl('/a.png'), 'asset:///a.png');
    assert.equal(mod.timecode(61000), '01:01');
    // Generation progress folds both event spellings: media_gen.* (rightkit-media-gen-client) and genright.* (0.1.x engines).
    for (const prefix of ['media_gen', 'genright']) {
      const state = mod.reduceFrame(mod.beginProgress('t', null), { id: 1, tag: 't', event: { event: `${prefix}.job`, request_id: 'r1', job_id: 'j1', status: 'running' } });
      assert.equal(state.jobs.r1?.status, 'running', `${prefix}.job not folded`);
    }
    assert.equal(mod.generationEventName('genright.import'), 'media_gen.import');
    // The app entry is a superset of the root (one module instance) with CutRight's pages and services over the
    // host's app part.
    const appMod = await import(provenance.entries['@rightkit/video-ui/app']);
    const workspaceMod = await import(provenance.entries['@rightkit/video-ui/workspace']);
    for (const key of ['useVideoProjects', 'useVideoAssembly', 'useProgress', 'useVideoWorkspace', 'useVideoUpdateBusy', 'progressViewModel', 'renderFraction']) {
      assert.equal(typeof workspaceMod[key], 'function', `missing workspace export ${key}`);
      assert.equal(workspaceMod[key], appMod[key]);
    }
    for (const key of ['useVideoHome', 'useVideoHomeThumbnail']) {
      assert.equal(typeof workspaceMod[key], 'function', `missing project list export ${key}`);
    }
    assert.equal(typeof workspaceMod.useVideoPhrases, 'function');
    assert.equal(typeof workspaceMod.useVideoRetention, 'function');
    assert.equal(typeof workspaceMod.useVideoShorts, 'function');
    assert.equal(typeof workspaceMod.useVideoFramingReview, 'function');
    assert.equal(typeof workspaceMod.useVideoRecipeLibrary, 'function');
    assert.equal(typeof workspaceMod.useVideoRecipe, 'function');
    for (const key of ['useVideoAssets', 'useVideoAssetPlacement', 'useVideoAssetGeneration']) {
      assert.equal(typeof workspaceMod[key], 'function', `missing workspace export ${key}`);
    }
    assert.equal(workspaceMod.videoAssetPath('D:/project', { path: 'D:\\media\\take.wav' }), 'D:\\media\\take.wav');
    assert.equal(workspaceMod.videoAssetPath('D:/project', { path: 'media/take.wav' }), 'D:/project/media/take.wav');
    assert.equal(workspaceMod.videoAssetMatches('Narration', 'voice'), true);
    assert.equal(workspaceMod.videoAssetMatches('Narration', 'sfx'), false);
    assert.equal(workspaceMod.videoAssetJobAction({ state: 'ready' }), 'import');
    assert.equal(workspaceMod.videoAssetJobAction({ state: 'done' }), null);
    for (const key of ['useVideoSettings', 'useVideoSettingsPickers', 'useVideoStorageSettings', 'useVideoGenerationSettings']) {
      assert.equal(typeof workspaceMod[key], 'function', `missing workspace export ${key}`);
    }
    assert.deepEqual(workspaceMod.videoEncoderSettings('macos', 'nvenc'), { choices: ['auto', 'videotoolbox'], value: 'auto' });
    assert.deepEqual(workspaceMod.videoEncoderSettings('windows', 'mf'), { choices: ['auto', 'nvenc', 'mf'], value: 'mf' });
    const settingWrites = [];
    const set = async (...args) => { settingWrites.push(args); };
    const editing = workspaceMod.videoEditingSettings({}, set);
    assert.deepEqual(editing.formats, ['16:9']);
    await editing.toggleFormat('9:16');
    await workspaceMod.videoFinishingSettings({}, set).setAutoFraming(true);
    assert.deepEqual(settingWrites, [['default_formats', ['16:9', '9:16']], ['framing_approval', 'auto']]);
    assert.equal(workspaceMod.framingStatus({ approved: true, approval: { approver: 'policy:auto' } }, false), 'approved_automatically');
    assert.equal(workspaceMod.framingStatus({ approved: true }, true), 'approved');
    assert.equal(workspaceMod.framingStatus(undefined, true), 'automatic');
    assert.equal(workspaceMod.framingStatus(undefined, false), 'needs_approval');
    assert.equal(typeof workspaceMod.useShortThumbnail, 'function');
    assert.equal(typeof workspaceMod.useShortTrimDrag, 'function');
    const proposal = { start_ms: 3000, end_ms: 6000 };
    assert.deepEqual(workspaceMod.shortTrimBounds(proposal, 3000, 6000, 'in', -1), { start: 0, end: 6000 });
    assert.deepEqual(workspaceMod.shortTrimBounds(proposal, 3000, 6000, 'in', 1), { start: 5750, end: 6000 });
    assert.deepEqual(workspaceMod.shortTrimBounds(proposal, 3000, 6000, 'out', 0), { start: 3000, end: 3250 });
    assert.deepEqual(workspaceMod.shortTrimBounds(proposal, 3000, 6000, 'out', 2), { start: 3000, end: 11000 });
    assert.deepEqual(workspaceMod.shortTrimBounds(proposal, 3000, 6000, 'in', 1 / 3), { start: 3667, end: 6000 });
    assert.deepEqual(workspaceMod.phraseHighlight('the Phrase here', 'phrase'), { before: 'the ', match: 'Phrase', after: ' here' });
    assert.equal(workspaceMod.phraseHighlight('the Phrase here', '  '), null);
    for (const key of appExpected) assert.ok(key in appMod, `missing app export ${key}`);
    assert.equal(appMod.installVideoHost, mod.installVideoHost);
    assert.throws(() => appMod.videoAppHost(), /no `app` part/);
    mod.installVideoHost({
      ...host,
      app: {
        invoke: async (command, args) => { appCalls.push([command, args ?? null]); return command === 'demo_status' ? { enabled: false } : null; },
        listen: async () => () => {}, onEngineRaw: async () => () => {}, onDragDrop: async () => () => {},
        appVersion: async () => '9.9.9', relaunch: async () => {}, exit: async () => {}, quit: async () => {}, checkUpdate: async () => null,
        pickFolder: async options => `/picked/${options.title}`, pickFile: async () => null, pickFiles: async () => null,
        window: { minimize: async () => {}, toggleMaximize: async () => {}, close: async () => {}, setFullscreen: async () => {} },
      },
    });
    assert.deepEqual(await appMod.demoStatus(), { enabled: false });
    assert.equal(await appMod.pickFolder('Open project'), '/picked/Open project');
    assert.deepEqual(appCalls, [['demo_status', null]]);
    appMod.setHostAppName('Host');
    assert.equal(appMod.hostAppName(), 'Host');
    // The Tauri host builds without touching Tauri and carries every part, the app part included.
    const tauriMod = await import(provenance.entries['@rightkit/video-ui/tauri']);
    const tauriHost = tauriMod.tauriVideoHost();
    for (const key of ['engine', 'playback', 'assetUrl', 'allowMediaScope', 'mediaSourceId', 'revealPath', 'app']) assert.ok(key in tauriHost, `tauriVideoHost lacks ${key}`);
    for (const key of ['invoke', 'listen', 'onEngineRaw', 'onDragDrop', 'appVersion', 'relaunch', 'exit', 'quit', 'checkUpdate', 'pickFolder', 'pickFile', 'pickFiles']) assert.equal(typeof tauriHost.app[key], 'function', `tauriVideoHost().app.${key}`);
    return { checked: expected, appChecked: appExpected.length, exportKeys: surface.length, engineCalls: calls.length, appCalls: appCalls.length };
  });

  assert.equal(result.scenarios.length, 1);
  result.ok = true;
} catch (error) {
  result.errors.push(failure(error));
  process.exitCode = 1;
}
process.stdout.write(`${JSON.stringify(result)}\n`);
