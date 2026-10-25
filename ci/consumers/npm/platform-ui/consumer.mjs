import { validateInstalledArtifact, fixtureEntry } from './provenance.mjs';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import path from 'node:path';

const recipeId = 'platform-ui-chords-headless-qa-consumer';
const result = { recipeId, schemaVersion: 1, ok: false, host: { platform: process.platform, arch: process.arch, node: process.version }, artifact: null, scenarios: [], errors: [], nativeHostQualified: false };
const failure = error => ({ name: error?.name ?? 'Error', message: String(error?.message ?? error) });
const required = name => { assert(process.env[name], `${name} is required`); return process.env[name]; };
const surface = {
  '.': ['DEFAULT_WINDOW_CONTROL_LABELS', 'WINDOW_CONTROL_LABELS', 'blockNativeContextMenu', 'configurePlatform', 'createFullscreenControls', 'createWindowControlLabels', 'createWindowControls', 'formatChord', 'getPlatform', 'isTauri', 'loadPlatform', 'normalizePlatform', 'platformCapabilities', 'platformInitializationScript', 'relativeTimeFrom'],
  './react': ['Kbd', 'usePlatform'],
  './headless': ['DEFAULT_TOAST_TTL', 'attachDismiss', 'attachFocusTrap', 'blockNativeContextMenu', 'clampIndex', 'createExitTransition', 'createToastBus', 'defaultToastBus', 'fitToViewport', 'floatingPanelStyle', 'formatChord', 'matchScore', 'moveIndex', 'normalizePlatform', 'platformInitializationScript', 'rankCommands', 'rankItems', 'readIndex', 'relativeTimeFrom', 'selectByPointer', 'toast', 'typeaheadMatch', 'useDismiss', 'useExitTransition', 'useFocusTrap', 'useLatest', 'useListNavigation', 'useModalDismiss'],
  './qa': ['DEFAULT_WINDOW_CONTROL_LABELS', 'WINDOW_CONTROL_LABELS', 'blockNativeContextMenu', 'bootstrapQaPlatform', 'configurePlatform', 'configureQaPlatform', 'createFullscreenControls', 'createWindowControlLabels', 'createWindowControls', 'formatChord', 'getPlatform', 'isTauri', 'loadPlatform', 'normalizePlatform', 'platformCapabilities', 'platformInitializationScript', 'relativeTimeFrom', 'resolveQaPlatform'],
};
const wait = ms => new Promise(resolve => setTimeout(resolve, ms));
const until = async (predicate, what) => { for (let i = 0; i < 400; i += 1) { if (predicate()) return; await wait(5); } assert.fail(`timed out waiting for ${what}`); };
const keyEvent = (type, props = {}) => Object.assign(new Event(type, { cancelable: true }), props);
// Node's EventTarget ignores a boolean capture flag on removal, unlike the DOM; this minimal target honours it.
class FakeTarget {
  #listeners = [];
  get size() { return this.#listeners.length; }
  addEventListener(type, listener, options) { this.#listeners.push({ type, listener, capture: options === true || options?.capture === true }); }
  removeEventListener(type, listener, options) {
    const capture = options === true || options?.capture === true;
    const at = this.#listeners.findIndex(entry => entry.type === type && entry.listener === listener && entry.capture === capture);
    if (at >= 0) this.#listeners.splice(at, 1);
  }
  dispatchEvent(event) { for (const entry of [...this.#listeners]) if (entry.type === event.type) entry.listener(event); return !event.defaultPrevented; }
}

try {
  const archive = path.resolve(required('RIGHTKIT_CONSUMER_ARTIFACT'));
  const sha256 = required('RIGHTKIT_CONSUMER_ARTIFACT_SHA256');
  const version = required('RIGHTKIT_CONSUMER_EXPECTED_VERSION');
  const resolution = required('RIGHTKIT_CONSUMER_RESOLUTION');
  assert(['packed', 'fetched'].includes(resolution));
  assert.match(sha256, /^[a-f0-9]{64}$/);
  assert.equal(createHash('sha256').update(readFileSync(archive)).digest('hex'), sha256);
  const specifiers = { '.': '@rightkit/platform-ui', './react': '@rightkit/platform-ui/react', './headless': '@rightkit/platform-ui/headless', './qa': '@rightkit/platform-ui/qa' };
  const root = import.meta.resolve('@rightkit/platform-ui');
  const provenance = validateInstalledArtifact({ fixtureUrl: import.meta.url, archive, sha256, version, resolution, packageName: '@rightkit/platform-ui', rootResolvedUrl: root, entries: Object.fromEntries(Object.values(specifiers).map(specifier => [specifier, import.meta.resolve(specifier)])) });
  result.artifact = provenance.artifact;
  const distDir = path.dirname(provenance.artifact.modulePath);
  const manifest = JSON.parse(readFileSync(path.join(distDir, '..', 'package.json'), 'utf8'));
  const React = await import(fixtureEntry(import.meta.url, import.meta.resolve('react')));
  const { renderToStaticMarkup } = await import(fixtureEntry(import.meta.url, import.meta.resolve('react-dom/server')));
  const h = React.createElement;

  // Platform detection is process-wide state and loads the optional plugin lazily, so the host injection and the
  // minimal Tauri OS-plugin bridge are in place before the first platform read, and no earlier step reads a default platform.
  globalThis.__RIGHTKIT_PLATFORM__ = 'win32';
  globalThis.window = { __TAURI_OS_PLUGIN_INTERNALS__: { platform: 'macos' } };
  const ui = await import(provenance.entries['@rightkit/platform-ui']);
  const reactEntry = await import(provenance.entries['@rightkit/platform-ui/react']);
  const headless = await import(provenance.entries['@rightkit/platform-ui/headless']);
  const qa = await import(provenance.entries['@rightkit/platform-ui/qa']);
  const modules = { '.': ui, './react': reactEntry, './headless': headless, './qa': qa };

  async function run(name, operation) {
    const entry = { name, ok: false, evidence: null, error: null };
    result.scenarios.push(entry);
    try { entry.evidence = await operation(); entry.ok = true; }
    catch (error) { entry.error = failure(error); throw error; }
  }

  await run('export-surface-exact-per-subpath', async () => {
    assert.deepEqual(Object.keys(manifest.exports), Object.keys(surface));
    for (const [subpath, expected] of Object.entries(surface)) assert.deepEqual(Object.keys(modules[subpath]).sort(), expected, subpath);
    assert.equal(manifest.sideEffects, false);
    assert.deepEqual(Object.keys(manifest.peerDependencies).sort(), ['@tauri-apps/plugin-os', 'react']);
    assert.equal(manifest.peerDependenciesMeta['@tauri-apps/plugin-os'].optional, true);
    // The headless entry must stay free of native detection: only the root entry imports the plugin, and it does so lazily.
    // Walk the whole relative-import closure of the headless entry (static, side-effect and dynamic forms) and refuse any native package.
    const importSpecifiers = text => [...text.matchAll(/(?:\bfrom\s*|\bimport\s*\(?\s*)["']([^"']+)["']/g)].map(match => match[1]);
    const closure = new Set();
    for (const queue = ['headless.js']; queue.length;) {
      const file = queue.pop();
      if (closure.has(file)) continue;
      closure.add(file);
      for (const specifier of importSpecifiers(readFileSync(path.join(distDir, file), 'utf8'))) {
        assert(!specifier.startsWith('@tauri-apps/'), `${file} imports ${specifier}`);
        if (specifier.startsWith('./')) queue.push(specifier.slice(2));
      }
    }
    assert.deepEqual([...closure].sort(), ['context-menu.js', 'dismiss.js', 'exit.js', 'focus.js', 'format.js', 'geometry.js', 'headless.js', 'list.js', 'rank.js', 'time.js', 'toast.js']);
    assert.match(readFileSync(path.join(distDir, 'index.js'), 'utf8'), /import\(\s*["']@tauri-apps\/plugin-os["']\s*\)/);
    assert.equal(qa.isTauri, ui.isTauri, 'the QA entry re-exports the host detection');
    assert.notEqual(qa.getPlatform, ui.getPlatform, 'the QA entry replaces platform reads with its override-aware versions');
    return Object.fromEntries(Object.entries(surface).map(([subpath, names]) => [subpath, names.length]));
  });

  await run('chord-display-tokens-per-platform', async () => {
    const names = [['macos', 'macos'], ['mac', 'macos'], ['darwin', 'macos'], ['ios', 'ios'], ['windows', 'windows'], ['win', 'windows'], ['win32', 'windows'], ['Win32', 'windows'], ['linux', 'linux'], ['MacOS', 'unknown'], ['android', 'unknown'], ['', 'unknown']];
    for (const [raw, expected] of names) assert.equal(ui.normalizePlatform(raw), expected, raw);
    assert.deepEqual(ui.formatChord('mod+shift+p', 'macos'), ['⌘', '⇧', 'P']);
    assert.deepEqual(ui.formatChord('mod+shift+p', 'windows'), ['Ctrl', 'Shift', 'P']);
    assert.deepEqual(ui.formatChord('mod+shift+p', 'linux'), ['Ctrl', 'Shift', 'P']);
    assert.deepEqual(ui.formatChord('mod+shift+p', 'ios'), ['Touch', 'Touch', 'P']);
    assert.deepEqual(ui.formatChord('mod+shift+p', 'unknown'), ['Mod', 'Shift', 'P']);
    assert.deepEqual(['macos', 'windows', 'linux', 'ios', 'unknown'].map(platform => ui.formatChord('ctrlormeta+k', platform)[0]), ['⌘/⌃', 'Ctrl/Win', 'Ctrl/Win', 'Touch', 'Ctrl/Meta']);
    assert.deepEqual(ui.formatChord('lctrl+rshift+ralt+x', 'windows'), ['Left Ctrl', 'Right Shift', 'Right Alt', 'X']);
    assert.deepEqual(ui.formatChord('alt+enter', 'macos'), ['⌥', '↩']);
    assert.deepEqual(ui.formatChord('space', 'ios'), ['Record']);
    assert.deepEqual(ui.formatChord(' MOD + , ', 'windows'), ['Ctrl', ',']);
    assert.deepEqual(ui.formatChord('mod+f5', 'windows'), ['Ctrl', 'F5']);
    assert.deepEqual(ui.formatChord('esc', 'windows', { labels: { esc: 'esc' } }), ['esc']);
    assert.deepEqual(ui.formatChord('', 'windows'), []);
    assert.deepEqual(headless.formatChord('mod+k'), ['Mod', 'K'], 'the headless entry defaults to the unknown platform');
    assert.deepEqual(headless.formatChord('mod+k', 'macos', { labels: { mod: 'Cmd' } }), ['Cmd', 'K']);
    return { platformNamesNormalised: names.length, macosChord: ui.formatChord('mod+shift+p', 'macos'), windowsChord: ui.formatChord('mod+shift+p', 'windows') };
  });

  await run('capabilities-labels-and-window-adapters', async () => {
    assert.deepEqual(ui.platformCapabilities('macos'), { desktopWindowControls: true, nativeTrafficLights: true, customCaptionButtons: false, touchFirst: false });
    assert.deepEqual(ui.platformCapabilities('windows'), { desktopWindowControls: true, nativeTrafficLights: false, customCaptionButtons: true, touchFirst: false });
    assert.deepEqual(ui.platformCapabilities('linux'), ui.platformCapabilities('windows'));
    assert.deepEqual(ui.platformCapabilities('ios'), { desktopWindowControls: false, nativeTrafficLights: false, customCaptionButtons: false, touchFirst: true });
    assert.deepEqual(ui.platformCapabilities('unknown'), { desktopWindowControls: false, nativeTrafficLights: false, customCaptionButtons: false, touchFirst: false });
    assert.deepEqual(ui.DEFAULT_WINDOW_CONTROL_LABELS, { minimize: 'Minimize window', maximize: 'Maximize or restore window', close: 'Close window', enterFullscreen: 'Enter fullscreen', exitFullscreen: 'Exit fullscreen' });
    assert.equal(ui.WINDOW_CONTROL_LABELS, ui.DEFAULT_WINDOW_CONTROL_LABELS);
    assert.deepEqual(ui.createWindowControlLabels({ close: 'Quit' }), { ...ui.DEFAULT_WINDOW_CONTROL_LABELS, close: 'Quit' });
    assert.notEqual(ui.createWindowControlLabels(), ui.DEFAULT_WINDOW_CONTROL_LABELS, 'a fresh object per call');
    const calls = [];
    const controls = ui.createWindowControls({ minimize: async () => { calls.push('minimize'); }, toggleMaximize: async () => { calls.push('toggleMaximize'); }, close: async () => { calls.push('close'); } });
    assert.deepEqual(Object.keys(controls).sort(), ['close', 'minimize', 'toggleMaximize']);
    await controls.minimize(); await controls.toggleMaximize(); await controls.close();
    const fullscreen = ui.createFullscreenControls({ setFullscreen: async value => { calls.push(`fullscreen:${value}`); } });
    assert.deepEqual(Object.keys(fullscreen).sort(), ['enterFullscreen', 'exitFullscreen']);
    await fullscreen.enterFullscreen(); await fullscreen.exitFullscreen();
    assert.deepEqual(calls, ['minimize', 'toggleMaximize', 'close', 'fullscreen:true', 'fullscreen:false']);
    return { adapterCalls: calls.length };
  });

  await run('host-injection-then-native-load-through-fake-os-plugin-bridge', async () => {
    assert.equal(ui.platformInitializationScript('mac'), 'window.__RIGHTKIT_PLATFORM__ = "macos";');
    assert.equal(ui.platformInitializationScript('Win32'), 'window.__RIGHTKIT_PLATFORM__ = "windows";');
    assert.equal(ui.platformInitializationScript('"; alert(1); "'), 'window.__RIGHTKIT_PLATFORM__ = "unknown";', 'only a closed set of names reaches the script');
    assert.equal(ui.isTauri(), false);
    globalThis.__TAURI_INTERNALS__ = {};
    assert.equal(ui.isTauri(), true);
    delete globalThis.__TAURI_INTERNALS__;
    const warnings = [];
    const originalWarn = console.warn;
    console.warn = (...args) => { warnings.push(args.join(' ')); };
    try {
      assert.equal(ui.getPlatform(), 'windows', 'before the native value loads, the host-injected platform is read synchronously');
      assert.equal(await ui.loadPlatform(), 'macos', 'the native plugin value wins');
      assert.equal(ui.getPlatform(), 'macos');
      assert.equal(await ui.loadPlatform(), 'macos', 'the native value is cached');
    } finally { console.warn = originalWarn; }
    assert.deepEqual(warnings, ['[rightkit] injected platform "windows" disagrees with native "macos"; using native.']);
    assert.deepEqual(ui.formatChord('mod+k'), ['⌘', 'K'], 'the default platform is the detected one');
    assert.deepEqual(ui.platformCapabilities(), ui.platformCapabilities('macos'));
    assert.equal(await qa.loadPlatform(), 'macos');
    return { injected: 'windows', native: 'macos', warnings: warnings.length };
  });

  await run('qa-entry-overrides-are-opt-in-and-bounded', async () => {
    assert.equal(qa.resolveQaPlatform({}), undefined);
    assert.equal(qa.resolveQaPlatform({ query: 'qaPlatform=linux' }), undefined, 'the query needs qa=1 by default');
    assert.equal(qa.resolveQaPlatform({ query: '?qa=1&qaPlatform=linux' }), 'linux');
    assert.equal(qa.resolveQaPlatform({ query: 'qa=1&qaPlatform=darwin' }), 'macos');
    assert.equal(qa.resolveQaPlatform({ query: 'qa=1&qaPlatform=beos' }), 'unknown');
    assert.equal(qa.resolveQaPlatform({ env: { RIGHTKIT_QA: '1', RIGHTKIT_QA_PLATFORM: 'win' } }), 'windows');
    assert.equal(qa.resolveQaPlatform({ env: { RIGHTKIT_QA: true, RIGHTKIT_QA_PLATFORM: 'ios' } }), 'ios');
    assert.equal(qa.resolveQaPlatform({ env: { RIGHTKIT_QA: '0', RIGHTKIT_QA_PLATFORM: 'ios' } }), undefined);
    assert.equal(qa.resolveQaPlatform({ env: { RIGHTKIT_QA: '1' } }), undefined);
    qa.configureQaPlatform({ qaQueryParam: 'p', qaRequireQaFlag: false });
    assert.equal(qa.resolveQaPlatform({ query: 'p=linux' }), 'linux');
    assert.equal(qa.resolveQaPlatform({ query: 'qaPlatform=linux' }), undefined);
    qa.configureQaPlatform({ qaQueryParam: 'qaPlatform', qaRequireQaFlag: true });
    assert.equal(globalThis.__RIGHTKIT_QA_PLATFORM__, undefined);
    assert.equal(qa.bootstrapQaPlatform({ query: 'qa=1&qaPlatform=linux' }), 'linux');
    assert.equal(globalThis.__RIGHTKIT_QA_PLATFORM__, 'linux');
    assert.equal(qa.getPlatform(), 'linux', 'the QA entry reports the override');
    assert.equal(ui.getPlatform(), 'macos', 'the root entry ignores QA overrides');
    assert.deepEqual(qa.formatChord('mod+k'), ['Ctrl', 'K']);
    assert.deepEqual(qa.platformCapabilities(), ui.platformCapabilities('linux'));
    qa.configurePlatform({ nativeWinsOverQa: true });
    assert.equal(qa.getPlatform(), 'macos', 'inside a native host the native platform wins when configured');
    qa.configurePlatform({ nativeWinsOverQa: false });
    assert.equal(qa.getPlatform(), 'linux');
    globalThis.__RIGHTKIT_QA_PLATFORM__ = 'beos';
    qa.configurePlatform({ fallbackPlatform: 'windows' });
    assert.equal(qa.getPlatform(), 'windows', 'fallbackPlatform replaces an unknown QA platform');
    assert.equal(qa.bootstrapQaPlatform({}), undefined);
    assert.equal(globalThis.__RIGHTKIT_QA_PLATFORM__, undefined, 'bootstrap without an override clears a stale one');
    assert.equal(qa.getPlatform(), 'macos');
    return { sources: ['query', 'env'], flagRequiredByDefault: true, rootEntryIgnoresOverride: true };
  });

  await run('react-entry-renders-chords-and-reads-the-platform', async () => {
    assert.equal(
      renderToStaticMarkup(h(reactEntry.Kbd, { chord: 'mod+shift+p' })),
      '<span class="rightkit-kbd-chord" aria-label="⌘ + ⇧ + P" style="--rightkit-kbd-gap:4px"><kbd class="rightkit-kbd-key">⌘</kbd><kbd class="rightkit-kbd-key">⇧</kbd><kbd class="rightkit-kbd-key">P</kbd></span>',
    );
    assert.equal(
      renderToStaticMarkup(h(reactEntry.Kbd, { chord: 'ctrlormeta+k', platform: 'windows', ariaLabel: 'Palette', gap: 8, className: 'chord', keyClassName: 'key' })),
      '<span class="chord" aria-label="Palette" style="--rightkit-kbd-gap:8px"><kbd class="key">Ctrl/Win</kbd><kbd class="key">K</kbd></span>',
    );
    assert.equal(renderToStaticMarkup(h(reactEntry.Kbd, { chord: '' })), '<span class="rightkit-kbd-chord" aria-label="" style="--rightkit-kbd-gap:4px"></span>');
    const Probe = () => h('i', null, reactEntry.usePlatform());
    assert.equal(renderToStaticMarkup(h(Probe)), '<i>macos</i>');
    return { markupAssertions: 4 };
  });

  await run('toast-bus-semantics', async () => {
    assert.deepEqual(headless.DEFAULT_TOAST_TTL, { info: 3000, success: 3000, warning: 5000, error: 6000 });
    const bus = headless.createToastBus();
    const events = [];
    const off = bus.subscribe(event => events.push(event));
    const a = bus.push('Saved', 'success');
    const b = bus.push('Failed', { kind: 'error', ttl: 0 });
    assert.deepEqual(events.map(event => [event.type, event.toast.id, event.toast.message, event.toast.kind, event.toast.ttl]), [['push', a, 'Saved', 'success', 3000], ['push', b, 'Failed', 'error', 0]]);
    bus.dismiss(a); bus.dismiss(a); bus.dismiss(999);
    assert.deepEqual(events.slice(2), [{ type: 'dismiss', id: a }], 'dismiss is idempotent and ignores unknown ids');
    off();
    bus.push('after unsubscribe');
    assert.equal(events.length, 3);
    const tuned = headless.createToastBus({ ttl: { info: 10 }, dedupe: true, max: 2 });
    const seen = [];
    tuned.subscribe(event => seen.push(`${event.type}:${event.type === 'push' ? event.toast.message : event.id}`));
    const first = tuned.push('same');
    assert.equal(tuned.push('same'), first, 'a plain duplicate returns the live toast');
    tuned.push('with action', { action: { label: 'Undo', run() {} } });
    tuned.push('with action', { action: { label: 'Undo', run() {} } });
    assert.deepEqual(seen, ['push:same', 'push:with action', 'dismiss:1', 'push:with action'], 'max 2 dismisses the oldest live toast; actions are never deduplicated');
    const keyed = headless.createToastBus();
    const keyedSeen = [];
    keyed.subscribe(event => keyedSeen.push(event.type));
    keyed.push('v1', { id: 'save' }); keyed.push('v2', { id: 'save' });
    assert.deepEqual(keyedSeen, ['push', 'dismiss', 'push'], 'a keyed push replaces the live toast with the same key');
    const defaultSeen = [];
    const stop = headless.defaultToastBus.subscribe(event => defaultSeen.push(event.type));
    const id = headless.toast('process-wide', 'warning');
    headless.defaultToastBus.dismiss(id);
    stop();
    assert.deepEqual(defaultSeen, ['push', 'dismiss']);
    return { eventsChecked: events.length + seen.length + keyedSeen.length + defaultSeen.length };
  });

  await run('ranking-geometry-and-time-helpers', async () => {
    assert.equal(headless.matchScore('open file', 'Open File'), 200);
    assert.equal(headless.matchScore('op', 'Open File'), 153);
    assert.equal(headless.matchScore('fi', 'Open File'), 119);
    assert.equal(headless.matchScore('pen', 'Open File'), 99);
    assert.equal(headless.matchScore('zip', 'Archive', ['ZIP tools']), 70);
    assert.equal(headless.matchScore('zip', 'Archive', [], 'zipped'), 60);
    assert.equal(headless.matchScore('of', 'Open File'), 36);
    assert.equal(headless.matchScore('zz', 'Open'), null);
    assert.equal(headless.matchScore('  ', 'anything'), 0);
    const items = [{ label: 'Open File' }, { label: 'Close', keywords: ['open'] }, { label: 'Quit' }, { label: 'Reopen', hint: 'recent' }];
    assert.deepEqual(headless.rankItems(items, 'open').map(item => item.label), ['Open File', 'Reopen', 'Close']);
    assert.deepEqual(headless.rankItems(items, '').map(item => item.label), ['Open File', 'Close', 'Quit', 'Reopen'], 'an empty query keeps input order');
    assert.deepEqual(headless.rankCommands(items, 'open', 1).map(item => item.label), ['Open File']);
    assert.deepEqual(headless.rankItems([{ label: 'b x' }, { label: 'a x' }], 'x', 50, { tieBreak: 'label' }).map(item => item.label), ['a x', 'b x']);
    assert.deepEqual(headless.rankItems([{ label: 'Preferences', aliases: ['settings'] }, { label: 'Settings sync' }], 'settings', 50, { aliasTiers: true }).map(item => item.label), ['Settings sync', 'Preferences']);
    assert.deepEqual(headless.rankItems([{ label: 'Alpha', sub: '/home/docs' }, { label: 'Beta' }], 'home', 50, { subLabel: true }).map(item => item.label), ['Alpha']);
    assert.deepEqual(headless.fitToViewport({ x: 900, y: 700, size: { width: 200, height: 300 }, viewport: { width: 1000, height: 800 } }), { left: 792, top: 492, width: 200, height: 300, maxWidth: 984, maxHeight: 784 });
    assert.deepEqual(headless.fitToViewport({ x: -50, y: -50, estimatedSize: { width: 5000, height: 10 }, viewport: { width: 400, height: 300 }, margin: 4 }), { left: 4, top: 4, width: 392, height: 10, maxWidth: 392, maxHeight: 292 });
    assert.throws(() => headless.fitToViewport({ x: 0, y: 0, viewport: { width: 10, height: 10 } }), TypeError);
    assert.deepEqual(headless.floatingPanelStyle({ x: 900, y: 700, width: 200, estimatedHeight: 300, viewportWidth: 1000, viewportHeight: 800 }), { left: 792, top: 492, width: 200, maxWidth: 984, maxHeight: 784 });
    assert.equal(ui.relativeTimeFrom(0, 30_000), 'just now');
    assert.equal(ui.relativeTimeFrom(0, 60_000), '1 minute ago');
    assert.equal(ui.relativeTimeFrom(0, 90_000), '2 minutes ago');
    assert.equal(ui.relativeTimeFrom(0, 3_600_000), '1 hour ago');
    assert.equal(ui.relativeTimeFrom(0, 5 * 3_600_000), '5 hours ago');
    assert.equal(ui.relativeTimeFrom(0, 2 * 86_400_000), '2 days ago');
    assert.equal(headless.relativeTimeFrom(10, 0), 'just now', 'a future timestamp reads as just now');
    return { ranked: headless.rankItems(items, 'open').map(item => item.label), fitted: headless.fitToViewport({ x: 900, y: 700, size: { width: 200, height: 300 }, viewport: { width: 1000, height: 800 } }).left };
  });

  await run('list-navigation-helpers', async () => {
    assert.deepEqual([[-3, 5], [2, 5], [9, 5], [3, 0]].map(([index, count]) => headless.clampIndex(index, count)), [0, 2, 4, -1]);
    assert.deepEqual([headless.readIndex(-1, 4), headless.readIndex(-1, 4, 'preserve'), headless.readIndex(9, 4, 'preserve'), headless.readIndex(2, 0)], [0, -1, 3, -1]);
    assert.equal(headless.moveIndex(2, 3, 'next', true), 0);
    assert.equal(headless.moveIndex(2, 3, 'next', false), 2);
    assert.equal(headless.moveIndex(0, 3, 'prev', true), 2);
    assert.equal(headless.moveIndex(0, 3, 'prev', false), 0);
    assert.equal(headless.moveIndex(0, 4, 'next', true, index => index === 1), 2, 'disabled rows are skipped');
    assert.equal(headless.moveIndex(1, 4, 'first', true, index => index === 0), 1);
    assert.equal(headless.moveIndex(0, 4, 'last', true, index => index === 3), 2);
    assert.equal(headless.moveIndex(1, 2, 'next', true, () => true), 1, 'all disabled: unchanged');
    assert.equal(headless.moveIndex(-1, 3, 'prev', true), 2);
    assert.equal(headless.moveIndex(5, 0, 'next', true), -1);
    assert.equal(headless.moveIndex(5, 0, 'prev', false, undefined, true), 0, 'preserve: prev on an empty list stores 0');
    const base = { index: 1, pointer: null, keyboardDriving: true };
    const pointerOnly = headless.selectByPointer(base, 4, { x: 1, y: 1 });
    assert.deepEqual(pointerOnly, { index: 1, pointer: { x: 1, y: 1 }, keyboardDriving: true }, 'the first pointer event never takes the keyboard selection');
    assert.equal(headless.selectByPointer(pointerOnly, 4, { x: 1, y: 1 }), pointerOnly);
    assert.deepEqual(headless.selectByPointer(pointerOnly, 4, { x: 2, y: 2 }), { index: 4, pointer: { x: 2, y: 2 }, keyboardDriving: false });
    const labels = ['Alpha', 'Beta', 'Bravo', 'Charlie'];
    assert.equal(headless.typeaheadMatch(labels, 'b', -1), 1);
    assert.equal(headless.typeaheadMatch(labels, 'b', 1), 2, 'a single character cycles from the next item');
    assert.equal(headless.typeaheadMatch(labels, 'br', 1), 2);
    assert.equal(headless.typeaheadMatch(labels, 'bb', 1), 2, 'a repeated character cycles');
    assert.equal(headless.typeaheadMatch(labels, 'z', 0), -1);
    assert.equal(headless.typeaheadMatch(labels, 'b', -1, index => index === 1), 2);
    assert.equal(headless.typeaheadMatch([], 'b', 0), -1);
    return { wrapped: headless.moveIndex(2, 3, 'next', true), clamped: headless.moveIndex(2, 3, 'next', false), typeahead: headless.typeaheadMatch(labels, 'br', 1) };
  });

  await run('exit-transition-core-state-machine', async () => {
    const log = [];
    const transition = headless.createExitTransition(() => ({ exitMs: 5, onExitStart: () => log.push('start'), onClosed: () => log.push('closed') }));
    assert.deepEqual(transition.getState(), { phase: 'open', pending: false });
    const states = [];
    const off = transition.subscribe(() => states.push(transition.getState().phase));
    let ran = 0;
    const closing = transition.close(() => { ran += 1; });
    assert.deepEqual(transition.getState(), { phase: 'closing', pending: false });
    await closing;
    await transition.close(() => { ran += 1; });
    assert.equal(ran, 1, 'a second close while closing drops its action');
    await until(() => transition.getState().phase === 'closed', 'the exit timer');
    assert.deepEqual(log, ['start', 'closed']);
    assert.deepEqual(states, ['closing', 'closed']);
    transition.open();
    assert.deepEqual(transition.getState(), { phase: 'open', pending: false });
    off();
    const instant = headless.createExitTransition(() => ({ exitMs: 0, initialOpen: true }));
    await instant.close();
    assert.equal(instant.getState().phase, 'closed', 'exitMs 0 finishes at once');
    const settled = headless.createExitTransition(() => ({ exitMs: 0, closeAfter: 'settled' }));
    let release;
    const pending = settled.close(() => new Promise(resolve => { release = resolve; }));
    assert.deepEqual(settled.getState(), { phase: 'open', pending: true });
    release();
    await pending;
    assert.equal(settled.getState().phase, 'closed', 'closes only after the action settled');
    const failing = headless.createExitTransition(() => ({ exitMs: 0 }));
    await assert.rejects(failing.close(() => { throw new Error('action failed'); }), /action failed/);
    assert.equal(failing.getState().phase, 'closed', 'the close still happens when the action throws');
    const propagating = headless.createExitTransition(() => ({ exitMs: 0, onSyncThrow: 'propagate' }));
    assert.throws(() => propagating.close(() => { throw new Error('refused'); }), /refused/);
    assert.equal(propagating.getState().phase, 'open', 'propagate leaves the layer open');
    const closedStart = headless.createExitTransition(() => ({ initialOpen: false }));
    assert.equal(closedStart.getState().phase, 'closed');
    closedStart.dispose();
    return { phases: ['open', 'closing', 'closed'], settledActionHoldsOpen: true, syncThrowPropagates: true };
  });

  await run('dismiss-and-focus-trap-cores-on-fake-event-targets', async () => {
    const win = new FakeTarget();
    const doc = new FakeTarget();
    const reasons = [];
    let outsideNow = true;
    const detach = headless.attachDismiss({
      onDismiss: reason => reasons.push(reason), escape: true, escapeTarget: null, preventDefault: true, stopPropagation: true, imeGuard: true,
      outside: 'mousedown', outsideContextMenu: false, isOutside: () => outsideNow, onWindowBlur: true, onResize: true, onScroll: false,
    }, { win, doc });
    const escape = keyEvent('keydown', { key: 'Escape' });
    win.dispatchEvent(escape);
    assert.deepEqual(reasons, ['escape']);
    assert.equal(escape.defaultPrevented, true);
    win.dispatchEvent(keyEvent('keydown', { key: 'a' }));
    win.dispatchEvent(keyEvent('keydown', { key: 'Escape', isComposing: true }));
    assert.deepEqual(reasons, ['escape'], 'other keys and IME composition never dismiss');
    doc.dispatchEvent(keyEvent('mousedown'));
    outsideNow = false;
    doc.dispatchEvent(keyEvent('mousedown'));
    win.dispatchEvent(keyEvent('blur')); win.dispatchEvent(keyEvent('resize')); win.dispatchEvent(keyEvent('scroll'));
    assert.deepEqual(reasons, ['escape', 'outside', 'blur', 'resize']);
    assert.equal(win.size + doc.size, 4);
    detach();
    win.dispatchEvent(keyEvent('keydown', { key: 'Escape' })); win.dispatchEvent(keyEvent('blur'));
    assert.equal(reasons.length, 4, 'detach removes every listener');
    assert.equal(win.size + doc.size, 0);

    const log = [];
    const focusDoc = new FakeTarget();
    focusDoc.activeElement = null;
    const control = name => ({ name, hasAttribute: () => false, getClientRects: () => [{}], focus() { focusDoc.activeElement = this; log.push(`focus:${name}`); } });
    const first = control('first');
    const last = control('last');
    const container = Object.assign(new FakeTarget(), {
      ownerDocument: focusDoc, isConnected: true, contains: node => [first, last, container].includes(node), querySelectorAll: () => [first, last],
      focus() { focusDoc.activeElement = this; log.push('focus:container'); },
    });
    let active = true;
    const opener = { focus: () => log.push('focus:opener') };
    const release = headless.attachFocusTrap(container, {
      autoFocus: true, trap: true, restore: true, restoreTiming: 'sync', restoreWhen: 'inactive', captureOpener: 'render', fromOutside: 'both',
      skipHidden: true, emptyList: 'prevent', focusFallback: 'container', opener: { current: opener }, isActive: () => active,
    });
    assert.equal(focusDoc.activeElement, first, 'the first control takes focus on open');
    const tab = shiftKey => { const event = keyEvent('keydown', { key: 'Tab', shiftKey }); container.dispatchEvent(event); return event.defaultPrevented; };
    focusDoc.activeElement = last;
    assert.equal(tab(false), true);
    assert.equal(focusDoc.activeElement, first, 'Tab on the last control wraps to the first');
    assert.equal(tab(true), true);
    assert.equal(focusDoc.activeElement, last, 'Shift+Tab on the first control wraps to the last');
    focusDoc.activeElement = { outside: true };
    assert.equal(tab(false), true);
    assert.equal(focusDoc.activeElement, first, 'Tab from outside enters at the first control');
    release();
    assert.deepEqual(log.slice(-1), ['focus:first'], 'focus stays put while the trap is still active');
    active = false;
    const closed = headless.attachFocusTrap(container, { autoFocus: false, trap: true, restore: true, restoreTiming: 'sync', restoreWhen: 'inactive', captureOpener: 'render', fromOutside: 'both', skipHidden: true, emptyList: 'prevent', focusFallback: 'container', opener: { current: opener }, isActive: () => active });
    closed();
    assert.equal(log.at(-1), 'focus:opener', 'closing restores focus to the opener');
    const hidden = new FakeTarget();
    assert.equal(headless.blockNativeContextMenu(hidden) === headless.blockNativeContextMenu(hidden), true, 'installing twice returns the same uninstall');
    const menu = keyEvent('contextmenu');
    hidden.dispatchEvent(menu);
    assert.equal(menu.defaultPrevented, true);
    headless.blockNativeContextMenu(hidden)();
    const afterUninstall = keyEvent('contextmenu');
    hidden.dispatchEvent(afterUninstall);
    assert.equal(afterUninstall.defaultPrevented, false);
    assert.equal(typeof ui.blockNativeContextMenu(undefined), 'function', 'no document: a no-op uninstall');
    return { dismissReasons: reasons, focusMoves: log.length };
  });

  await run('hooks-render-initial-state-on-the-server', async () => {
    let captured;
    const Probe = () => {
      const exit = headless.useExitTransition({ exitMs: 0 });
      const nav = headless.useListNavigation({ count: 3, idPrefix: 'fixture', role: 'menu', isDisabled: index => index === 2 });
      const listbox = headless.useListNavigation({ count: 0 });
      const modal = headless.useModalDismiss(() => {}, { escape: true });
      const latest = headless.useLatest({ n: 7 });
      headless.useFocusTrap({ current: null }, true);
      headless.useDismiss(() => {}, { enabled: true });
      captured = {
        exit: [exit.phase, exit.closing, exit.isOpen, exit.pending, typeof exit.open, typeof exit.close],
        nav: [nav.activeIndex, nav.rawIndex, nav.activeId, nav.getListProps().id, nav.getListProps().role, nav.getOptionProps(1).id, nav.getOptionProps(1).role, nav.getOptionProps(1)['aria-selected'], nav.getOptionProps(2)['aria-disabled'], nav.getInputProps().role, nav.getInputProps()['aria-controls']],
        empty: [listbox.activeIndex, listbox.activeId, listbox.getOptionProps(0).role, listbox.getOptionProps(0)['aria-selected']],
        modal: [modal.closing, typeof modal.close],
        latest: latest.current,
      };
      return h('span', null, 'probe');
    };
    assert.equal(renderToStaticMarkup(h(Probe)), '<span>probe</span>');
    assert.deepEqual(captured.exit, ['open', false, true, false, 'function', 'function']);
    assert.deepEqual(captured.nav, [0, 0, 'fixture-option-0', 'fixture-list', 'menu', 'fixture-option-1', 'menuitem', undefined, true, 'combobox', 'fixture-list']);
    assert.deepEqual(captured.empty, [-1, undefined, 'option', false]);
    assert.deepEqual(captured.modal, [false, 'function']);
    assert.deepEqual(captured.latest, { n: 7 });
    return { hooks: ['useExitTransition', 'useListNavigation', 'useModalDismiss', 'useLatest', 'useFocusTrap', 'useDismiss'] };
  });

  assert.equal(result.scenarios.length, 12);
  result.ok = true;
} catch (error) {
  result.errors.push(failure(error));
  process.exitCode = 1;
} finally {
  delete globalThis.window;
  delete globalThis.__RIGHTKIT_PLATFORM__;
}
process.stdout.write(`${JSON.stringify(result)}\n`);
