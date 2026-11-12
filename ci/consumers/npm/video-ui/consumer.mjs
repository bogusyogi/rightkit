import { validateInstalledArtifact } from './provenance.mjs';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import path from 'node:path';

const recipeId = 'video-ui-root-export-consumer';
const result = { recipeId, schemaVersion: 1, ok: false, host: { platform: process.platform, arch: process.arch, node: process.version }, artifact: null, scenarios: [], errors: [], nativeHostQualified: false };
const failure = error => ({ name: error?.name ?? 'Error', message: String(error?.message ?? error) });
const required = name => { assert(process.env[name], `${name} is required`); return process.env[name]; };
const surface = [".", "./app", "./app.css", "./package.json", "./styles.css", "./tauri"];
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
  const provenance = validateInstalledArtifact({ fixtureUrl: import.meta.url, archive, sha256, version, resolution, packageName: '@rightkit/video-ui', rootResolvedUrl: root, entries: { '@rightkit/video-ui': root, '@rightkit/video-ui/app': app, '@rightkit/video-ui/tauri': tauri } });
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
