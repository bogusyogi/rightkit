import { validateInstalledArtifact, fixtureEntry } from './provenance.mjs';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import vm from 'node:vm';

const recipeId = 'react-shell-registries-and-server-markup-consumer';
const result = { recipeId, schemaVersion: 1, ok: false, host: { platform: process.platform, arch: process.arch, node: process.version }, artifact: null, scenarios: [], errors: [], nativeHostQualified: false };
const failure = error => ({ name: error?.name ?? 'Error', message: String(error?.message ?? error) });
const required = name => { assert(process.env[name], `${name} is required`); return process.env[name]; };
const noop = () => {};
const rootSurface = ['DEFAULT_TOAST_TTL', 'THEME_CHOICES', 'applyBodyFont', 'applyStoredThemeSync', 'canonicalChord', 'chordFromEvent', 'chordSteps', 'createCommandRegistry', 'createKeymapStore', 'createShortcutRegistry', 'createThemeStore', 'createToastBus', 'defaultToastBus', 'eventMatchesShortcutPrefix', 'filterNavGroups', 'firstNavItem', 'flattenNav', 'floatingPanelStyle', 'groupByName', 'initialPaletteSelection', 'installHoverGuard', 'isShortcutPrefixHeld', 'isTypingTarget', 'matchScore', 'navNeighborIndex', 'rankItems', 'resolveTheme', 'selectByKeyboard', 'selectByPointer', 'systemPrefersDark', 'themeBootScript', 'toast'];
const reactSurface = ['ActionDialog', 'AppFrame', 'AppShell', 'AppearanceMenu', 'Badge', 'Button', 'CaptionControls', 'Card', 'Checkbox', 'CommandPalette', 'ConfirmDialog', 'ContextMenu', 'ContrastIcon', 'DEFAULT_PALETTE_LABELS', 'DEFAULT_SHELL_LABELS', 'DEFAULT_TOAST_TTL', 'Dialog', 'EmptyState', 'ErrorBoundary', 'Field', 'FuseCurves', 'GearIcon', 'IconButton', 'Input', 'JumpTo', 'Kbd', 'ModalShell', 'NavList', 'Notice', 'PageShell', 'PaletteTrigger', 'Pill', 'Plane', 'Popover', 'ProgressBar', 'PromptDialog', 'SearchIcon', 'SegmentedControl', 'Select', 'ShellProvider', 'ShortcutHelp', 'ShortcutRecorder', 'SidebarIcon', 'Skeleton', 'SkeletonRows', 'Slider', 'StatusRow', 'THEME_CHOICES', 'Table', 'TableBody', 'TableCaption', 'TableCell', 'TableHead', 'TableHeaderCell', 'TableMessage', 'TableRow', 'Titlebar', 'ToastViewport', 'Toggle', 'anchorPoint', 'applyBodyFont', 'applyStoredThemeSync', 'blurThen', 'canonicalChord', 'chordFromEvent', 'chordSteps', 'createCommandRegistry', 'createKeymapStore', 'createShortcutRegistry', 'createThemeStore', 'createToastBus', 'cx', 'defaultToastBus', 'eventMatchesShortcutPrefix', 'filterNavGroups', 'firstNavItem', 'flattenNav', 'floatingPanelStyle', 'groupByName', 'initialPaletteSelection', 'installHoverGuard', 'isShortcutPrefixHeld', 'isTypingTarget', 'matchScore', 'navNeighborIndex', 'rankCommands', 'rankItems', 'resolveShellKeybindings', 'resolveTheme', 'selectByKeyboard', 'selectByPointer', 'systemPrefersDark', 'themeBootScript', 'toast', 'useCommands', 'useContextMenu', 'useFocusTrap', 'useLatest', 'useMediaQuery', 'useModalDismiss', 'usePersistedState', 'useShell', 'useShellOptional', 'useShortcuts', 'useTheme', 'useToast'];
const stylesheets = ['shell.css', 'shell.layered.css', 'shell.structure.css', 'shell.visual.css', 'suite-aliases.css'];

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
const memoryStorage = (initial = {}) => {
  const values = new Map(Object.entries(initial));
  return { values, getItem: key => (values.has(key) ? values.get(key) : null), setItem: (key, value) => { values.set(key, String(value)); }, removeItem: key => { values.delete(key); } };
};
const attributeRoot = () => {
  const attrs = new Map();
  return { attrs, setAttribute: (name, value) => { attrs.set(name, value); }, removeAttribute: name => { attrs.delete(name); }, getAttribute: name => (attrs.has(name) ? attrs.get(name) : null) };
};
const systemMedia = matches => {
  const media = { matches, listeners: new Set(), addEventListener: (type, listener) => { assert.equal(type, 'change'); media.listeners.add(listener); }, removeEventListener: (type, listener) => { media.listeners.delete(listener); } };
  media.set = value => { media.matches = value; for (const listener of [...media.listeners]) listener(); };
  return media;
};
const keyEvent = (key, extra = {}) => ({ key, code: '', ctrlKey: false, metaKey: false, altKey: false, shiftKey: false, isComposing: false, defaultPrevented: false, target: null, prevented: 0, preventDefault() { this.defaultPrevented = true; this.prevented += 1; }, ...extra });

try {
  const archive = path.resolve(required('RIGHTKIT_CONSUMER_ARTIFACT'));
  const sha256 = required('RIGHTKIT_CONSUMER_ARTIFACT_SHA256');
  const version = required('RIGHTKIT_CONSUMER_EXPECTED_VERSION');
  const resolution = required('RIGHTKIT_CONSUMER_RESOLUTION');
  assert(['packed', 'fetched'].includes(resolution));
  assert.match(sha256, /^[a-f0-9]{64}$/);
  assert.equal(createHash('sha256').update(readFileSync(archive)).digest('hex'), sha256);
  const specifiers = ['@rightkit/react-shell', '@rightkit/react-shell/react', '@rightkit/react-shell/headless', ...stylesheets.map(file => `@rightkit/react-shell/${file}`)];
  const root = import.meta.resolve('@rightkit/react-shell');
  const provenance = validateInstalledArtifact({ fixtureUrl: import.meta.url, archive, sha256, version, resolution, packageName: '@rightkit/react-shell', rootResolvedUrl: root, entries: Object.fromEntries(specifiers.map(specifier => [specifier, import.meta.resolve(specifier)])) });
  result.artifact = provenance.artifact;
  const manifest = JSON.parse(readFileSync(path.join(path.dirname(provenance.artifact.modulePath), '..', 'package.json'), 'utf8'));
  const css = Object.fromEntries(stylesheets.map(file => [file, readFileSync(fileURLToPath(provenance.entries[`@rightkit/react-shell/${file}`]), 'utf8')]));
  const React = await import(fixtureEntry(import.meta.url, import.meta.resolve('react')));
  const { renderToStaticMarkup } = await import(fixtureEntry(import.meta.url, import.meta.resolve('react-dom/server')));
  const platformHeadless = await import(fixtureEntry(import.meta.url, import.meta.resolve('@rightkit/platform-ui/headless')));
  // The host platform is injected before the first read so no step depends on the machine running the recipe. The
  // optional OS plugin is not installed, which is the documented fallback path.
  globalThis.__RIGHTKIT_PLATFORM__ = 'macos';
  const shell = await import(provenance.entries['@rightkit/react-shell']);
  const ui = await import(provenance.entries['@rightkit/react-shell/react']);
  const headless = await import(provenance.entries['@rightkit/react-shell/headless']);
  const h = React.createElement;
  const render = (component, props, ...children) => renderToStaticMarkup(h(component, props, ...children)).replace(/_R_[0-9A-Za-z]+_/g, 'ID');
  const plain = html => html.replace(/<svg[^>]*>.*?<\/svg>/g, '<svg/>');
  const adapter = () => { const calls = []; return { calls, minimize: async () => { calls.push('minimize'); }, toggleMaximize: async () => { calls.push('toggleMaximize'); }, close: async () => { calls.push('close'); } }; };
  const groups = [{ title: 'Library', items: [{ id: 'home', label: 'Home' }, { id: 'docs', label: 'Documents', badge: '3', keywords: ['files'] }] }, { items: [{ id: 'hidden', label: 'Hidden', hidden: true }, { id: 'about', label: 'About', ariaLabel: 'About this app' }] }];

  async function run(name, operation) {
    const entry = { name, ok: false, evidence: null, error: null };
    result.scenarios.push(entry);
    try { entry.evidence = await operation(); entry.ok = true; }
    catch (error) { entry.error = failure(error); throw error; }
  }

  await run('export-surface-exact-per-subpath-and-shared-platform-peer', async () => {
    assert.deepEqual(Object.keys(shell).sort(), rootSurface);
    assert.deepEqual(Object.keys(ui).sort(), reactSurface);
    assert.deepEqual(Object.keys(headless).sort(), ['useSegmentedControl', 'useToggle']);
    assert.deepEqual(Object.keys(manifest.exports), ['.', './react', './headless', ...stylesheets.map(file => `./${file}`)]);
    assert.deepEqual(Object.keys(manifest.peerDependencies).sort(), ['@rightkit/platform-ui', '@tauri-apps/plugin-os', 'react', 'react-dom']);
    assert.equal(manifest.peerDependenciesMeta['@tauri-apps/plugin-os'].optional, true);
    assert.deepEqual(manifest.sideEffects, ['*.css']);
    for (const name of ['matchScore', 'rankItems', 'createToastBus', 'defaultToastBus', 'toast', 'floatingPanelStyle', 'DEFAULT_TOAST_TTL']) assert.equal(shell[name], platformHeadless[name], `${name} is the platform-ui implementation`);
    assert.equal(ui.rankCommands, platformHeadless.rankCommands);
    assert.equal(ui.useFocusTrap, platformHeadless.useFocusTrap);
    assert.throws(() => import.meta.resolve('@tauri-apps/plugin-os'), error => error.code === 'ERR_MODULE_NOT_FOUND', 'the optional OS plugin is not part of this installation');
    return { rootExports: rootSurface.length, reactExports: reactSurface.length, headlessExports: 2, stylesheets: stylesheets.length };
  });

  await run('navigation-and-palette-selection-helpers', async () => {
    assert.deepEqual(shell.filterNavGroups(groups, '').map(group => group.items.map(item => item.id)), [['home', 'docs'], ['about']], 'hidden items drop out');
    assert.deepEqual(shell.filterNavGroups(groups, 'FILES').map(group => group.items.map(item => item.id)), [['docs']], 'a keyword matches case-insensitively and empty groups drop out');
    assert.deepEqual(shell.filterNavGroups(groups, 'nothing'), []);
    assert.equal(shell.firstNavItem(groups).id, 'home');
    assert.equal(shell.firstNavItem([{ items: [{ id: 'x', label: 'X', hidden: true }] }]), undefined);
    assert.deepEqual(shell.flattenNav(groups).map(item => item.id), ['home', 'docs', 'about']);
    assert.deepEqual([['ArrowDown', 0], ['ArrowUp', 0], ['Home', 2], ['End', 0], ['x', 0]].map(([key, at]) => shell.navNeighborIndex('vertical', key, at, 4)), [1, 3, 0, 3, null]);
    assert.deepEqual([['ArrowRight', 3], ['ArrowLeft', 0], ['ArrowDown', 0]].map(([key, at]) => shell.navNeighborIndex('horizontal', key, at, 4)), [0, 3, null]);
    assert.equal(shell.navNeighborIndex('vertical', 'ArrowDown', 0, 0), null);
    assert.deepEqual(shell.groupByName([{ id: 1, group: 'B' }, { id: 2 }, { id: 3, group: 'B' }, { id: 4, group: 'A' }]).map(group => [group.name, group.items.map(item => item.id)]), [['B', [1, 3]], ['Commands', [2]], ['A', [4]]], 'first-seen group order and a fallback group');
    assert.equal(shell.groupByName([{ id: 1 }], 'Other')[0].name, 'Other');
    assert.deepEqual(shell.initialPaletteSelection, { active: 0, pointer: null, keyboardDriving: false });
    assert.deepEqual(shell.selectByKeyboard(shell.initialPaletteSelection, 'up', 3), { active: 2, pointer: null, keyboardDriving: true });
    assert.equal(shell.selectByKeyboard({ active: 2, pointer: null, keyboardDriving: false }, 'down', 3).active, 0);
    assert.equal(shell.selectByKeyboard(shell.initialPaletteSelection, 'down', 0).active, 0);
    const driving = shell.selectByKeyboard(shell.initialPaletteSelection, 'down', 5);
    const withPointer = shell.selectByPointer(driving, 4, { x: 1, y: 1 });
    assert.deepEqual(withPointer, { ...driving, pointer: { x: 1, y: 1 } }, 'a resting pointer never takes the keyboard selection');
    assert.deepEqual(shell.selectByPointer(withPointer, 4, { x: 2, y: 2 }), { active: 4, pointer: { x: 2, y: 2 }, keyboardDriving: false });
    assert.equal(shell.matchScore('open', 'Open file'), 155);
    assert.deepEqual(shell.rankItems([{ label: 'Close' }, { label: 'Open file' }], 'open').map(item => item.label), ['Open file']);
    assert.deepEqual(ui.anchorPoint({ left: 100, top: 100, right: 180, bottom: 130 }, 'bottom-start', { width: 160, height: 200 }, { width: 800, height: 600 }), { x: 100, y: 134 });
    assert.deepEqual(ui.anchorPoint({ left: 100, top: 100, right: 180, bottom: 130 }, 'bottom-end', { width: 160, height: 200 }, { width: 800, height: 600 }), { x: 20, y: 134 });
    assert.deepEqual(ui.anchorPoint({ left: 100, top: 500, right: 180, bottom: 530 }, 'bottom-start', { width: 160, height: 200 }, { width: 800, height: 600 }), { x: 100, y: 296 }, 'it flips above when below would overflow');
    assert.deepEqual(ui.anchorPoint({ left: 100, top: 10, right: 180, bottom: 40 }, 'top-start', { width: 160, height: 200 }, { width: 800, height: 600 }), { x: 100, y: 44 }, 'and flips below when above does not fit');
    return { navGroups: shell.filterNavGroups(groups, '').length, firstItem: shell.firstNavItem(groups).id, placements: ['bottom-start', 'bottom-end', 'top-start'] };
  });

  await run('theme-store-persistence-system-media-and-first-paint', async () => {
    assert.deepEqual(shell.THEME_CHOICES, ['system', 'light', 'dark']);
    assert.deepEqual([['system', true], ['system', false], ['light', true], ['dark', false]].map(([choice, dark]) => shell.resolveTheme(choice, dark)), ['dark', 'light', 'light', 'dark']);
    const storage = memoryStorage();
    const el = attributeRoot();
    const media = systemMedia(false);
    const store = shell.createThemeStore({ storageKey: 'fx.theme', storage, root: el, systemMedia: media });
    const seen = [];
    const off = store.subscribe(choice => seen.push(choice));
    assert.equal(store.read(), 'system');
    assert.equal(store.resolved(), 'light');
    store.apply(store.read());
    assert.equal(el.attrs.get('data-theme'), 'light', 'system resolves and always paints data-theme');
    media.set(true);
    assert.equal(el.attrs.get('data-theme'), 'dark', 'the store follows the OS without React');
    assert.deepEqual(seen, ['system']);
    store.save('light');
    assert.deepEqual([storage.values.get('fx.theme'), el.attrs.get('data-theme'), store.read()], ['light', 'light', 'light']);
    media.set(false); media.set(true);
    assert.equal(seen.filter(choice => choice === 'system').length, 1, 'a fixed choice ignores OS changes');
    store.save('system');
    assert.equal(storage.values.has('fx.theme'), false, 'saving the default removes the key');
    off();
    store.dispose();
    assert.equal(media.listeners.size, 0);
    const dark = shell.createThemeStore({ storageKey: 'fx.theme', storage: memoryStorage(), root: attributeRoot(), systemMedia: null, defaultChoice: 'dark' });
    assert.equal(dark.read(), 'dark');
    const stored = shell.createThemeStore({ storageKey: 'k', storage: memoryStorage({ k: 'light' }), root: attributeRoot(), systemMedia: null, qaOverride: 'dark' });
    assert.equal(stored.read(), 'dark', 'the QA override beats storage');
    const bad = shell.createThemeStore({ storageKey: 'k', storage: { getItem() { throw new Error('blocked'); }, setItem() { throw new Error('blocked'); }, removeItem() { throw new Error('blocked'); } }, root: attributeRoot(), systemMedia: null });
    assert.equal(bad.read(), 'system');
    assert.doesNotThrow(() => bad.save('dark'));
    assert.equal(bad.read(), 'dark', 'the choice is kept in memory when storage throws');
    const unpainted = attributeRoot();
    const lazy = shell.createThemeStore({ storageKey: 'k', storage: memoryStorage(), root: unpainted, systemMedia: systemMedia(true), alwaysPaintDataTheme: false });
    lazy.apply('light');
    assert.equal(unpainted.attrs.get('data-theme'), 'light');
    lazy.apply('system');
    assert.equal(unpainted.attrs.has('data-theme'), false, 'with alwaysPaintDataTheme false, system leaves the decision to CSS');
    const darkDefault = attributeRoot();
    const removing = shell.createThemeStore({ storageKey: 'k', storage: memoryStorage(), root: darkDefault, systemMedia: null, darkRemovesAttribute: true });
    removing.apply('light');
    assert.equal(darkDefault.attrs.get('data-theme'), 'light');
    removing.apply('dark');
    assert.equal(darkDefault.attrs.has('data-theme'), false, 'dark removes the attribute for dark-by-default stylesheets');
    const writes = attributeRoot();
    let setCalls = 0;
    const counting = { ...writes, setAttribute: (name, value) => { setCalls += 1; writes.setAttribute(name, value); } };
    assert.equal(shell.applyStoredThemeSync({ storageKey: 'k', storage: memoryStorage({ k: 'dark' }), root: counting, systemMedia: null }), 'dark');
    const second = shell.createThemeStore({ storageKey: 'k', storage: memoryStorage({ k: 'dark' }), root: counting, systemMedia: null });
    second.apply(second.read());
    assert.equal(setCalls, 1, 'a theme applied before first render is not written twice');
    assert.equal(shell.applyStoredThemeSync({ storageKey: 'k', storage: { getItem() { throw new Error('blocked'); } }, root: attributeRoot(), systemMedia: systemMedia(true) }), 'dark', 'unreadable storage falls back to the default choice');
    assert.equal(shell.applyStoredThemeSync({ storageKey: 'k', storage: memoryStorage(), root: attributeRoot(), systemMedia: null, alwaysPaintDataTheme: false }), null);
    const fontRoot = { style: { props: new Map(), setProperty(name, value) { this.props.set(name, value); }, removeProperty(name) { const old = this.props.get(name) ?? ''; this.props.delete(name); return old; } } };
    shell.applyBodyFont('"Fixture Sans", sans-serif', fontRoot);
    assert.equal(fontRoot.style.props.get('--rk-font-body'), '"Fixture Sans", sans-serif');
    shell.applyBodyFont(null, fontRoot);
    assert.equal(fontRoot.style.props.size, 0);
    assert.equal(shell.systemPrefersDark(), false, 'no window: light');
    const boot = (script, { stored = {}, prefersDark = false } = {}) => {
      const target = attributeRoot();
      vm.runInNewContext(script, { localStorage: { getItem: key => (Object.hasOwn(stored, key) ? stored[key] : null) }, matchMedia: () => ({ matches: prefersDark }), document: { documentElement: target } }, { timeout: 5000 });
      return target.attrs.get('data-theme') ?? null;
    };
    const script = shell.themeBootScript({ storageKey: 'fx.theme' });
    assert.deepEqual([boot(script, { stored: { 'fx.theme': 'dark' } }), boot(script, { prefersDark: true }), boot(script, {}), boot(shell.themeBootScript({ storageKey: 'fx.theme', qaOverride: 'dark' }), { stored: { 'fx.theme': 'light' } }), boot(shell.themeBootScript({ storageKey: 'fx.theme', defaultChoice: 'dark' }))], ['dark', 'dark', 'light', 'dark', 'dark']);
    assert.equal(boot(shell.themeBootScript({ storageKey: 'k', alwaysPaintDataTheme: false }), { prefersDark: true }), null);
    assert(!shell.themeBootScript({ storageKey: '</script>' }).includes('</script>'));
    return { choices: shell.THEME_CHOICES, bootScriptBytes: script.length };
  });

  await run('shortcut-registry-dispatch-sequences-overrides-and-conflicts', async () => {
    const calls = [];
    const registry = shell.createShortcutRegistry({ platform: 'macos', sequenceTimeout: 5000 });
    assert.equal(registry.platform, 'macos');
    const offSave = registry.register({ id: 'file.save', chord: 'mod+s', label: 'Save', group: 'File', rebindable: true, run: () => { calls.push('save'); } });
    registry.register({ id: 'go.inbox', chord: 'g i', label: 'Go to inbox', group: 'Navigation', run: () => { calls.push('inbox'); } });
    registry.register({ id: 'help', chord: '?', label: 'Help', run: () => { calls.push('help'); } });
    registry.register({ id: 'either', chord: 'ctrlormeta+e', label: 'Either', hidden: true, run: () => { calls.push('either'); } });
    registry.register({ id: 'digit', chord: 'mod+digit', label: 'Tab by number', run: event => { calls.push(`digit:${event.key}`); } });
    registry.register({ id: 'refuse', chord: 'mod+r', label: 'Refuses', run: () => false });
    registry.register({ id: 'gated', chord: 'mod+g', label: 'Gated', when: () => false, run: () => { calls.push('gated'); } });
    const save = keyEvent('s', { metaKey: true });
    assert.equal(registry.handle(save), true);
    assert.equal(save.prevented, 1);
    assert.deepEqual(calls.splice(0), ['save']);
    assert.equal(registry.handle(keyEvent('s', { ctrlKey: true })), false, 'on macOS mod is Cmd, not Ctrl');
    assert.equal(registry.handle(keyEvent('s', { metaKey: true, shiftKey: true })), false, 'modifiers match exactly');
    const refused = keyEvent('r', { metaKey: true });
    assert.equal(registry.handle(refused), false);
    assert.equal(refused.prevented, 0, 'a run that returns false leaves the browser default alone');
    assert.equal(registry.handle(keyEvent('g', { metaKey: true })), false, 'when() gates a binding');
    assert.equal(registry.handle(keyEvent('e', { metaKey: true })), true);
    assert.equal(registry.handle(keyEvent('e', { ctrlKey: true })), true);
    assert.equal(registry.handle(keyEvent('e', { ctrlKey: true, metaKey: true })), false, 'ctrlormeta is exactly one of the two');
    assert.equal(registry.handle(keyEvent('7', { metaKey: true })), true);
    assert.equal(registry.handle(keyEvent('Ω', { metaKey: true, code: 'Digit3' })), true, 'digit falls back to the physical key');
    assert.deepEqual(calls.splice(0), ['either', 'either', 'digit:7', 'digit:Ω']);
    assert.equal(registry.handle(keyEvent('g')), true);
    assert.deepEqual(calls, [], 'the first step of a sequence only arms it');
    assert.equal(registry.handle(keyEvent('i')), true);
    assert.deepEqual(calls.splice(0), ['inbox']);
    assert.equal(registry.handle(keyEvent('i')), false);
    registry.handle(keyEvent('g'));
    assert.equal(registry.handle(keyEvent('x')), false, 'an abandoned sequence is dropped');
    const input = { closest: selector => (/input|textarea/.test(selector) ? {} : null) };
    assert.equal(registry.handle(keyEvent('?', { target: input })), false, 'bare keys never fire while typing');
    assert.equal(registry.handle(keyEvent('s', { metaKey: true, target: input })), true, 'modified chords fire in a field');
    calls.length = 0;
    assert.equal(registry.handle(keyEvent('?', { shiftKey: true })), true, 'a symbol key already encodes shift');
    assert.deepEqual(calls.splice(0), ['help']);
    assert.equal(registry.handle(keyEvent('s', { metaKey: true, isComposing: true })), false, 'nothing fires during IME composition');
    assert.equal(registry.handle(keyEvent('s', { metaKey: true, defaultPrevented: true })), false, 'nor after another handler prevented the event');
    assert.equal(registry.handle(keyEvent('Meta', { metaKey: true })), false);
    const info = registry.list().find(entry => entry.id === 'file.save');
    assert.deepEqual(info, { id: 'file.save', label: 'Save', group: 'File', chords: ['mod+s'], defaults: ['mod+s'], rebindable: true, hidden: false, overridden: false, unassigned: false });
    assert.deepEqual(registry.groups().map(group => [group.title, group.items.map(item => item.id)]), [['File', ['file.save']], ['Navigation', ['go.inbox']], ['General', ['help', 'digit', 'refuse', 'gated']]], 'hidden entries are left out of help');
    const versions = [registry.version()];
    const unsubscribe = registry.subscribe(() => versions.push(registry.version()));
    registry.setOverrides({ 'file.save': 'mod+shift+s', help: null });
    assert.deepEqual(registry.overrides(), { 'file.save': 'mod+shift+s', help: null });
    assert.equal(registry.chordFor('file.save'), 'mod+shift+s');
    assert.equal(registry.chordFor('help'), undefined);
    assert.equal(registry.list().find(entry => entry.id === 'help').unassigned, true);
    assert.equal(registry.handle(keyEvent('s', { metaKey: true })), false);
    assert.equal(registry.handle(keyEvent('s', { metaKey: true, shiftKey: true })), true);
    assert.equal(registry.handle(keyEvent('?')), false, 'an unbound shortcut no longer fires');
    unsubscribe();
    assert(versions.length >= 2 && versions.at(-1) > versions[0]);
    registry.setOverrides({});
    registry.register({ id: 'dup', chord: 'mod+s', label: 'Duplicate', run: noop });
    registry.register({ id: 'g', chord: 'g', label: 'Plain g', run: noop });
    assert.deepEqual(registry.conflicts(), [{ chord: 'mod+s', ids: ['file.save', 'dup'], kind: 'exact' }, { chord: 'g', ids: ['g', 'go.inbox'], kind: 'prefix' }]);
    registry.suspend();
    assert.equal(registry.isSuspended(), true);
    assert.equal(registry.handle(keyEvent('s', { metaKey: true })), false, 'suspended: no binding fires');
    registry.resume();
    assert.equal(registry.isSuspended(), false);
    assert.equal(registry.invoke('help'), true);
    assert.equal(registry.invoke('gated'), false);
    assert.equal(registry.invoke('missing'), false);
    offSave();
    assert.equal(registry.list().some(entry => entry.id === 'file.save'), false);
    calls.length = 0;
    const windows = shell.createShortcutRegistry({ platform: 'windows' });
    windows.register({ id: 'save', chord: 'mod+s', label: 'Save', run: () => { calls.push('win-save'); } });
    assert.equal(windows.handle(keyEvent('s', { ctrlKey: true })), true);
    assert.equal(windows.handle(keyEvent('s', { metaKey: true })), false);
    const loose = shell.createShortcutRegistry({ platform: 'windows' });
    loose.register({ id: 'esc', chord: 'esc', label: 'Escape', ignoreExtraModifiers: true, run: () => { calls.push('esc'); } });
    loose.register({ id: 'composing', chord: 'mod+c', label: 'Composing', allowComposing: true, allowDefaultPrevented: true, run: () => { calls.push('composing'); } });
    assert.equal(loose.handle(keyEvent('Escape', { shiftKey: true })), true);
    assert.equal(loose.handle(keyEvent('c', { ctrlKey: true, isComposing: true, defaultPrevented: true })), true);
    assert.deepEqual(calls.splice(0), ['win-save', 'esc', 'composing']);
    const target = new FakeTarget();
    const attached = shell.createShortcutRegistry({ platform: 'windows' });
    attached.register({ id: 'x', chord: 'mod+x', label: 'X', run: () => { calls.push('attached'); } });
    const detach = attached.attach(target);
    target.dispatchEvent({ type: 'keydown', ...keyEvent('x', { ctrlKey: true }) });
    detach();
    target.dispatchEvent({ type: 'keydown', ...keyEvent('x', { ctrlKey: true }) });
    assert.deepEqual(calls, ['attached']);
    assert.equal(target.size, 0);
    return { conflicts: registry.conflicts().map(conflict => conflict.kind), platforms: ['macos', 'windows'] };
  });

  await run('chord-vocabulary-keymap-store-and-prefix-helpers', async () => {
    assert.equal(shell.canonicalChord('Shift+Mod+K'), 'mod+shift+k');
    assert.equal(shell.canonicalChord('Cmd+option+esc'), 'meta+alt+escape');
    assert.equal(shell.canonicalChord('CtrlOrMeta+p'), 'ctrlormeta+p');
    assert.equal(shell.canonicalChord('g  i'), 'g i');
    assert.deepEqual(shell.chordSteps('  g   i '), ['g', 'i']);
    assert.deepEqual(shell.chordSteps(''), []);
    const eventFor = (key, extra) => ({ key, code: '', ctrlKey: false, metaKey: false, altKey: false, shiftKey: false, ...extra });
    assert.equal(shell.chordFromEvent(eventFor('k', { metaKey: true }), 'macos'), 'mod+k');
    assert.equal(shell.chordFromEvent(eventFor('k', { ctrlKey: true }), 'macos'), 'ctrl+k');
    assert.equal(shell.chordFromEvent(eventFor('k', { ctrlKey: true }), 'windows'), 'mod+k');
    assert.equal(shell.chordFromEvent(eventFor('k', { metaKey: true }), 'windows'), 'meta+k');
    assert.equal(shell.chordFromEvent(eventFor('P', { ctrlKey: true, shiftKey: true }), 'linux'), 'mod+shift+p');
    assert.equal(shell.chordFromEvent(eventFor('?', { shiftKey: true }), 'linux'), '?', 'shift is omitted for symbol keys');
    assert.equal(shell.chordFromEvent(eventFor('Ω', { altKey: true, code: 'KeyZ' }), 'macos'), 'alt+z', 'alt uses the physical key');
    assert.equal(shell.chordFromEvent(eventFor(' '), 'linux'), 'space');
    for (const key of ['Shift', 'Control', 'Alt', 'Meta', 'Dead', 'Unidentified', '']) assert.equal(shell.chordFromEvent(eventFor(key), 'linux'), null, key);
    const storage = memoryStorage();
    const keymap = shell.createKeymapStore({ storageKey: 'fx.keys', storage });
    assert.deepEqual(keymap.load(), {});
    keymap.save({ 'file.save': 'mod+shift+s', help: null });
    assert.equal(storage.values.get('fx.keys'), '{"file.save":"mod+shift+s","help":null}');
    assert.deepEqual(keymap.load(), { 'file.save': 'mod+shift+s', help: null });
    keymap.save({});
    assert.equal(storage.values.has('fx.keys'), false, 'saving no overrides removes the key');
    storage.setItem('fx.keys', JSON.stringify({ ok: 'mod+k', bad: 5, worse: { x: 1 } }));
    assert.deepEqual(keymap.load(), { ok: 'mod+k' }, 'only string and null bindings are read back');
    storage.setItem('fx.keys', '{not json');
    assert.deepEqual(keymap.load(), {});
    assert.deepEqual(shell.createKeymapStore({ storageKey: 'k', storage: null }).load(), {});
    assert.doesNotThrow(() => shell.createKeymapStore({ storageKey: 'k', storage: null }).save({ a: 'b' }));
    assert.equal(shell.isShortcutPrefixHeld('mod', new Set(['meta']), 'macos'), true);
    assert.equal(shell.isShortcutPrefixHeld('mod', new Set(['control']), 'macos'), false);
    assert.equal(shell.isShortcutPrefixHeld('mod+shift', new Set(['control', 'shift']), 'windows'), true);
    assert.equal(shell.isShortcutPrefixHeld('mod+k', new Set(['control', 'k']), 'windows'), true);
    assert.equal(shell.isShortcutPrefixHeld('g i', new Set(['g']), 'windows'), false, 'a sequence is never a held prefix');
    assert.equal(shell.eventMatchesShortcutPrefix({ metaKey: true, ctrlKey: false, shiftKey: false, altKey: false }, 'mod', undefined, 'macos'), true);
    assert.equal(shell.eventMatchesShortcutPrefix({ metaKey: false, ctrlKey: true, shiftKey: false, altKey: false }, 'mod', undefined, 'macos'), false);
    assert.equal(shell.eventMatchesShortcutPrefix({ metaKey: false, ctrlKey: true, shiftKey: true, altKey: false }, 'mod+shift', undefined, 'windows'), true);
    assert.equal(shell.eventMatchesShortcutPrefix({ metaKey: false, ctrlKey: true, shiftKey: false, altKey: false }, 'mod+k', new Set(['k']), 'windows'), true);
    assert.equal(shell.isTypingTarget(null), false);
    assert.equal(shell.isTypingTarget({}), false);
    assert.equal(shell.isTypingTarget({ closest: selector => (selector.includes('textarea') ? {} : null) }), true);
    assert.equal(shell.isTypingTarget({ closest: () => null }), false);
    return { canonical: shell.canonicalChord('Shift+Mod+K'), steps: shell.chordSteps('g i'), keymapKeys: ['file.save', 'help'] };
  });

  await run('command-and-toast-registries', async () => {
    const commands = shell.createCommandRegistry();
    const ran = [];
    const changes = [];
    const off = commands.subscribe(() => changes.push(commands.list().map(command => command.id)));
    const first = [{ id: 'a', label: 'A', run: () => ran.push('a1') }, { id: 'b', label: 'B', disabled: true, run: () => ran.push('b') }];
    const second = [{ id: 'a', label: 'A (later)', run: () => ran.push('a2') }, { id: 'c', label: 'C', run: () => ran.push('c') }];
    const unregisterFirst = commands.register(first);
    const unregisterSecond = commands.register(second);
    assert.deepEqual(commands.list().map(command => [command.id, command.label]), [['a', 'A (later)'], ['c', 'C']], 'later registrations win on id collisions and disabled commands are hidden');
    assert.equal(commands.run('a'), true);
    assert.equal(commands.run('b'), false);
    assert.equal(commands.run('missing'), false);
    assert.deepEqual(ran, ['a2']);
    unregisterSecond();
    assert.deepEqual(commands.list().map(command => command.label), ['A']);
    unregisterFirst(); off();
    assert.deepEqual(commands.list(), []);
    assert.deepEqual(changes, [['a'], ['a', 'c'], ['a'], []], 'each register and unregister notifies subscribers once');
    const bus = shell.createToastBus({ ttl: { error: 1 } });
    const events = [];
    bus.subscribe(event => events.push(event.type));
    const id = bus.push('Saved', 'success');
    bus.dismiss(id);
    assert.deepEqual(events, ['push', 'dismiss']);
    assert.deepEqual(shell.DEFAULT_TOAST_TTL, { info: 3000, success: 3000, warning: 5000, error: 6000 });
    return { toastEvents: events };
  });

  await run('provider-context-keybindings-and-hooks-on-the-server', async () => {
    assert.deepEqual(ui.resolveShellKeybindings({}), { palette: 'mod+k', help: '?', sidebar: 'mod+\\' });
    assert.deepEqual(ui.resolveShellKeybindings({ paletteChord: 'mod+p', helpChord: false }), { palette: 'mod+p', help: false, sidebar: 'mod+\\' });
    assert.deepEqual(ui.resolveShellKeybindings({ paletteChord: 'mod+p', keybindings: { palette: false, sidebar: 'mod+b' } }), { palette: false, help: '?', sidebar: 'mod+b' }, 'keybindings win over the legacy chord props');
    const store = shell.createThemeStore({ storageKey: 'fx.theme', storage: memoryStorage({ 'fx.theme': 'dark' }), root: attributeRoot(), systemMedia: null });
    const registry = shell.createShortcutRegistry({ platform: 'windows' });
    const bus = shell.createToastBus();
    const events = [];
    bus.subscribe(event => events.push(event.type));
    let context;
    let theme;
    let toast;
    let optional;
    const Probe = () => { context = ui.useShell(); theme = ui.useTheme(); toast = ui.useToast(); optional = ui.useShellOptional(); return h('i', null, 'probe'); };
    assert.equal(render(ui.ShellProvider, { themeStore: store, registry, toastBus: bus, platform: 'windows', mountOverlays: false, paletteChord: 'mod+p', keybindings: { help: false } }, h(Probe)), '<i>probe</i>');
    assert.equal(context.platform, 'windows');
    assert.deepEqual(context.keybindings, { palette: 'mod+p', help: false, sidebar: 'mod+\\' });
    assert.equal(context.registry, registry);
    assert.equal(context.toasts, bus);
    assert.equal(context.themeStore, store);
    assert.equal(optional, context);
    assert.deepEqual([context.palette.open, context.help.open, typeof context.palette.show, typeof context.confirm, typeof context.prompt, typeof context.commands.register], [false, false, 'function', 'function', 'function', 'function']);
    assert.deepEqual({ choice: theme.choice, resolved: theme.resolved }, { choice: 'dark', resolved: 'dark' });
    theme.setChoice('light');
    assert.equal(store.read(), 'light', 'setChoice persists through the provider store');
    assert.equal(typeof toast('Saved', 'success'), 'number');
    assert.deepEqual(events, ['push']);
    let seen = 'unset';
    const Outside = () => { seen = ui.useShellOptional(); return h('i'); };
    render(Outside, {});
    assert.equal(seen, null);
    assert.throws(() => render(() => { ui.useShell(); return h('i'); }, {}), /useShell must be used inside <ShellProvider> \(or <AppShell>\)\./);
    const withOverlays = render(ui.ShellProvider, { themeStore: store, platform: 'macos' }, h('main', null, 'page'));
    assert.equal(withOverlays, '<main>page</main><div class="rk-toasts" aria-live="polite" aria-atomic="false" data-rk-toasts="true"></div>', 'closed overlays render nothing but the toast live region');
    const placed = render(ui.ShellProvider, { themeStore: store, platform: 'macos', toastPlacement: 'top-center', toastOptions: { maxVisible: 2, dedupe: 'newest' } }, 'x');
    assert(placed.includes('data-placement="top-center"'));
    let persisted;
    const persistedStorage = memoryStorage({ 'fx.width': '300' });
    const Persisted = () => { persisted = [ui.usePersistedState('fx.width', 220, { storage: persistedStorage }), ui.usePersistedState(null, 'memory'), ui.usePersistedState('fx.missing', 7, { storage: persistedStorage }), ui.usePersistedState('fx.bad', 'init', { storage: memoryStorage({ 'fx.bad': '{' }) })].map(([value]) => value); return h('i'); };
    render(Persisted, {});
    assert.deepEqual(persisted, [300, 'memory', 7, 'init'], 'stored JSON is read once at mount and a corrupt value falls back');
    let media;
    render(() => { media = ui.useMediaQuery('(max-width: 719px)'); return h('i'); }, {});
    assert.equal(media, false);
    return { keybindingVariants: 3, hooks: ['useShell', 'useShellOptional', 'useTheme', 'useToast', 'usePersistedState', 'useMediaQuery'] };
  });

  await run('app-shell-fused-layout-markup', async () => {
    const store = shell.createThemeStore({ storageKey: 'fx.theme', storage: memoryStorage(), root: attributeRoot(), systemMedia: null, defaultChoice: 'light' });
    const props = { groups, activeId: 'home', onNavigate: noop, title: 'Home', platform: 'windows', wordmark: 'Fixture', onOpenSettings: noop, windowControls: adapter(), shell: { themeStore: store } };
    const app = render(ui.AppShell, props, h('p', null, 'content'));
    const html = plain(app);
    assert(html.startsWith('<div class="rk-shell rk-shell--fused" data-platform="windows"><header class="rk-top" data-tauri-drag-region="true">'));
    assert(html.includes('<button type="button" aria-label="Toggle sidebar" title="Toggle sidebar" aria-pressed="true" class="rk-icon-btn is-open rk-top__toggle"><svg/></button>'));
    assert(html.includes('<div class="rk-top__title" data-tauri-drag-region="true">Home</div>'));
    assert(html.includes('<div class="rk-caption"><button type="button" class="rk-caption__btn" title="Minimize window" aria-label="Minimize window"><svg/></button>'), 'Windows draws caption buttons from the adapter');
    assert(html.includes('<aside class="rk-side" aria-label="Sections"><button type="button" class="rk-search rk-search--trigger" aria-label="Jump to (command palette)" aria-haspopup="dialog"><svg/><span class="rk-search__text">Jump to…</span><span class="rightkit-kbd-chord" aria-label="Ctrl + K"'));
    assert(html.includes('<nav class="rk-nav" aria-label="Sections"><div class="rk-group"><h6>Library</h6><button type="button" data-nav-id="home" class="rk-item is-active" aria-current="page" tabindex="0"><span class="rk-item__label">Home</span><svg/><svg/></button>'), 'the active row carries the fuse curves');
    assert(html.includes('<button type="button" data-nav-id="docs" class="rk-item" tabindex="-1"><span class="rk-item__label">Documents</span><span class="rk-item__badge">3</span></button>'));
    assert(html.includes('<button type="button" data-nav-id="about" class="rk-item" aria-label="About this app" tabindex="-1"><span class="rk-item__label">About</span></button>') && !html.includes('Hidden'), 'hidden items are not rendered');
    assert(html.includes('<footer class="rk-brand-foot"><span class="rk-wordmark">Fixture</span><button type="button" aria-label="Settings" title="Settings" class="rk-icon-btn"><svg/></button><button type="button" aria-label="Appearance" title="Appearance" aria-pressed="false" class="rk-icon-btn" aria-haspopup="menu"><svg/></button></footer>'));
    assert(html.endsWith('<section class="rk-plane" data-rk-plane="true"><p>content</p></section></div></div><div class="rk-toasts" aria-live="polite" aria-atomic="false" data-rk-toasts="true"></div>'));
    const mac = plain(render(ui.AppShell, { ...props, platform: 'macos', windowControls: undefined }, 'x'));
    assert(mac.includes('<span class="rk-top__lights" data-tauri-drag-region="true"></span>') && !mac.includes('rk-caption'), 'macOS reserves space for the native traffic lights and draws no caption buttons');
    assert(mac.includes('aria-label="⌘ + K"'));
    const filter = plain(render(ui.AppShell, { ...props, searchMode: 'filter' }, 'x'));
    assert(filter.includes('<label class="rk-search"><svg/><input aria-label="Jump to" placeholder="Jump to…" value=""/>') && !filter.includes('rk-search--trigger'));
    const none = plain(render(ui.AppShell, { ...props, searchMode: 'none', sidebarTop: h('em', null, 'top'), statusFooter: h(ui.StatusRow, { tone: 'ok', label: 'Sync', value: 'idle' }) }, 'x'));
    assert(!none.includes('rk-search') && none.includes('<em>top</em>'));
    assert(none.includes('<div class="rk-status"><div class="rk-status__row"><span><i class="rk-dot rk-dot--ok"></i>Sync</span><span>idle</span></div></div>'));
    const collapsed = plain(render(ui.AppShell, { ...props, sidebarCollapsed: true }, 'x'));
    assert(collapsed.includes('rk-shell--fused is-collapsed') && collapsed.includes('<aside class="rk-side" aria-label="Sections" hidden="">') && collapsed.includes('aria-pressed="false" class="rk-icon-btn rk-top__toggle"'));
    const bare = plain(render(ui.AppShell, { ...props, sidebar: false, titlebar: false, sidebarToggle: false }, 'x'));
    assert(bare.startsWith('<div class="rk-shell rk-shell--fused no-sidebar" data-platform="windows"><div class="rk-body"><section class="rk-plane"'), 'sidebar and titlebar can both be removed');
    const custom = plain(render(ui.AppShell, { ...props, titlebar: h('b', null, 'own bar'), sidebar: h('aside', null, 'own side'), className: 'host', slots: { root: 'slot-root', plane: 'slot-plane', body: 'slot-body' }, sidebarWidth: 300 }, 'x'));
    assert(custom.startsWith('<div class="rk-shell rk-shell--fused host slot-root" data-platform="windows" style="--rk-sidebar-width:300px"><b>own bar</b><div class="rk-body slot-body"><aside>own side</aside><section class="rk-plane slot-plane"'));
    const resizable = plain(render(ui.AppShell, { ...props, sidebarResizable: { min: 180, max: 400 }, sidebarWidth: 240 }, 'x'));
    assert(resizable.includes('role="separator" aria-orientation="vertical" aria-label="Resize sidebar" aria-valuemin="180" aria-valuemax="400" aria-valuenow="240" tabindex="0" class="rk-side-resizer" data-rk-resizer="true" style="left:237px"'));
    assert(resizable.includes('style="--rk-sidebar-width:240px"'));
    const labelled = plain(render(ui.AppShell, { ...props, labels: { toggleSidebar: 'Seitenleiste', jumpTo: 'Springe zu', sections: 'Bereiche' } }, 'x'));
    assert(labelled.includes('aria-label="Seitenleiste"') && labelled.includes('aria-label="Springe zu (command palette)"') && labelled.includes('<nav class="rk-nav" aria-label="Bereiche">'));
    assert.deepEqual(ui.DEFAULT_SHELL_LABELS, { jumpTo: 'Jump to', sections: 'Sections', settings: 'Settings', appearance: 'Appearance', toggleSidebar: 'Toggle sidebar', goTo: 'Go to', theme: { system: 'System', light: 'Light', dark: 'Dark' }, resizeSidebar: 'Resize sidebar' });
    assert.deepEqual(ui.DEFAULT_PALETTE_LABELS, { placeholder: 'Search or run a command…', title: 'Command palette', empty: 'No matches.', results: 'Results' });
    return { layouts: ['windows', 'macos', 'filter search', 'no search', 'collapsed', 'bare', 'custom slots', 'resizable', 'relabelled'] };
  });

  await run('frame-titlebar-navigation-and-page-primitives-markup', async () => {
    const win = adapter();
    const mac = plain(render(ui.Titlebar, { platform: 'macos', title: 'Doc', start: h('i', null, 'S'), end: h('i', null, 'E'), windowControls: win }));
    assert.equal(mac, '<header class="rk-top" data-tauri-drag-region="true"><div class="rk-top__side" data-tauri-drag-region="true"><span class="rk-top__lights" data-tauri-drag-region="true"></span><i>S</i></div><div class="rk-top__title" data-tauri-drag-region="true">Doc</div><div class="rk-top__drag" data-tauri-drag-region="true"></div><div class="rk-top__end" data-tauri-drag-region="true"><i>E</i></div></header>');
    const windows = plain(render(ui.Titlebar, { platform: 'windows', title: 'Doc', windowControls: win, maximized: true, captionGlyphs: { restore: h('b', null, 'R'), close: h('b', null, 'X') }, captionTitles: { close: 'Quit', minimize: false }, captionButtonClasses: { close: 'cc' }, slots: { root: 'sr', side: 'ss', caption: 'sc' }, as: 'div', className: 'c', startWidth: 120 }));
    assert.equal(windows, '<div class="rk-top c sr" data-tauri-drag-region="true"><div class="rk-top__side ss" style="width:120px" data-tauri-drag-region="true"></div><div class="rk-top__title" data-tauri-drag-region="true">Doc</div><div class="rk-top__drag" data-tauri-drag-region="true"></div><div class="rk-caption sc"><button type="button" class="rk-caption__btn" aria-label="Minimize window"><svg/></button><button type="button" class="rk-caption__btn" title="Maximize or restore window" aria-label="Maximize or restore window"><b>R</b></button><button type="button" class="rk-caption__btn rk-caption__close cc" title="Quit" aria-label="Close window"><b>X</b></button></div></div>');
    assert(!plain(render(ui.Titlebar, { platform: 'windows', title: 'Doc' })).includes('rk-caption'), 'no adapter, no caption buttons');
    assert(!plain(render(ui.Titlebar, { platform: 'linux', windowControls: win, captionTitles: false })).includes('title='), 'captionTitles false omits every tooltip');
    const clicks = [];
    const click = ui.blurThen(async () => { clicks.push('ran'); }, true);
    let blurred = 0;
    click({ currentTarget: { blur: () => { blurred += 1; } } });
    ui.blurThen(async () => { throw new Error('window gone'); }, false)({ currentTarget: { blur: () => { blurred += 1; } } });
    await new Promise(resolve => setTimeout(resolve, 10));
    assert.deepEqual([clicks, blurred], [['ran'], 1], 'a rejecting window action is swallowed and blur is optional');
    assert.equal(plain(render(ui.AppFrame, { platform: 'linux', className: 'extra', style: { height: 10 } }, 'in')), '<div class="rk-shell extra" data-platform="linux" style="height:10px">in</div>');
    assert.equal(render(ui.Plane, { className: 'p' }, 'c'), '<section class="rk-plane p" data-rk-plane="true">c</section>');
    const nav = plain(render(ui.NavList, { groups: [{ title: 'Lib', items: [{ id: 'a', label: 'A', icon: h('i', null, 'i') }, { id: 'b', label: 'B' }] }, { items: [{ id: 'c', label: 'C' }] }], activeId: 'b', onNavigate: noop, onToggleGroup: noop, collapsedGroups: ['Lib'], itemCheck: item => (item.id === 'c' ? { checked: true, onChange: noop } : undefined), itemProps: item => ({ className: `x-${item.id}`, 'data-drop': item.id }) }));
    assert.equal(nav, '<nav class="rk-nav" aria-label="Sections"><div class="rk-group"><h6><button type="button" class="rk-group__toggle" aria-expanded="false"><span aria-hidden="true">▸</span>Lib</button></h6></div><div class="rk-group"><div class="rk-item-row"><input type="checkbox" class="rk-item__check" aria-label="C" checked=""/><button class="rk-item x-c" data-drop="c" type="button" data-nav-id="c" tabindex="-1"><span class="rk-item__label">C</span></button></div></div></nav>');
    const open = plain(render(ui.NavList, { groups: [{ title: 'Lib', items: [{ id: 'a', label: 'A' }] }], activeId: 'zzz', onNavigate: noop, onToggleGroup: noop, collapsedGroups: [], renderTwisty: isOpen => h('u', null, isOpen ? 'v' : '>'), renderItem: (item, state, row) => h('div', { 'data-active': String(state.active) }, row) }));
    assert(open.includes('aria-expanded="true"><u>v</u>Lib') && open.includes('<div data-active="false"><button') && open.includes('tabindex="0"'), 'with no active item the first row is the tab stop');
    assert.equal(plain(render(ui.PaletteTrigger, { onOpen: noop, platform: 'macos', className: 'pt' })), '<button type="button" class="rk-search rk-search--trigger pt" aria-label="Jump to (command palette)" aria-haspopup="dialog"><svg/><span class="rk-search__text">Jump to…</span><span class="rightkit-kbd-chord" aria-label="⌘ + K" style="--rightkit-kbd-gap:3px"><kbd class="rightkit-kbd-key">⌘</kbd><kbd class="rightkit-kbd-key">K</kbd></span></button>');
    assert(!plain(render(ui.PaletteTrigger, { onOpen: noop, chord: false })).includes('rightkit-kbd-chord'));
    assert(plain(render(ui.JumpTo, { groups, query: 'doc', onQueryChange: noop, onNavigate: noop, platform: 'windows' })).includes('<input aria-label="Jump to" placeholder="Jump to…" value="doc"/>'));
    assert.equal(plain(render(ui.PageShell, { title: 'T', subtitle: 'S', actions: h('b', null, 'act'), header: h('i', null, 'band'), maxWidth: 0, scroll: false }, 'body')), '<div class="rk-page"><div class="rk-page__band"><div class="rk-page__container"><i>band</i></div></div><div class="rk-page__scroll is-fixed"><div class="rk-page__container"><header class="rk-page__head"><div><h1>T</h1><p>S</p></div><div class="rk-page__actions"><b>act</b></div></header>body</div></div></div>');
    assert.equal(render(ui.PageShell, {}, 'only'), '<div class="rk-page"><div class="rk-page__scroll"><div class="rk-page__container" style="max-width:1100px">only</div></div></div>');
    const fuse = render(ui.FuseCurves, { side: 'left', size: 8, fill: '#fff' });
    assert(fuse.includes('rk-fuse rk-fuse--top rk-fuse--left') && fuse.includes('left:0;top:-8px;width:8px;height:8px;pointer-events:none;transform:scaleX(-1);fill:#fff') && fuse.includes('transform:scaleX(-1) scaleY(-1)'));
    assert.equal(render(ui.Pill, { tone: 'warn' }, 'Beta'), '<span class="rk-pill rk-pill--warn">Beta</span>');
    assert.equal(render(ui.Pill, { onClick: noop }, 'Go'), '<button type="button" class="rk-pill">Go</button>');
    for (const icon of ['SearchIcon', 'GearIcon', 'ContrastIcon', 'SidebarIcon']) assert(render(ui[icon], {}).startsWith('<svg viewBox="0 0 16 16"'), icon);
    const appearance = plain(render(ui.AppearanceMenu, { value: 'dark', onChange: noop, presets: [{ id: 'p', label: 'Paper', swatches: ['#fff'] }], preset: 'p', fonts: [{ id: 'f', label: 'Serif', family: 'serif' }], font: 'f' }));
    assert.equal(appearance, '<button type="button" aria-label="Appearance" title="Appearance" aria-pressed="false" class="rk-icon-btn" aria-haspopup="menu"><svg/></button>', 'the menu opens on click, so the closed trigger is all that renders');
    return { components: ['Titlebar', 'AppFrame', 'Plane', 'NavList', 'PaletteTrigger', 'JumpTo', 'PageShell', 'FuseCurves', 'Pill', 'AppearanceMenu'] };
  });

  await run('ui-primitives-render-accessible-markup', async () => {
    const cases = [
      [ui.Button, { variant: 'primary', size: 'sm' }, ['Go'], '<button type="button" class="rk-btn rk-btn--primary rk-btn--sm">Go</button>'],
      [ui.Button, { type: 'submit', className: 'k' }, ['Send'], '<button type="submit" class="rk-btn rk-btn--secondary k">Send</button>'],
      [ui.IconButton, { label: 'Close', pressed: true }, ['x'], '<button type="button" aria-label="Close" title="Close" aria-pressed="true" class="rk-icon-btn is-open">x</button>'],
      [ui.Input, { className: 'k', placeholder: 'p' }, [], '<input class="rk-input k" placeholder="p"/>'],
      [ui.Toggle, { checked: true, onChange: noop, label: 'Wi-Fi', className: 'tg' }, [], '<button type="button" role="switch" aria-checked="true" aria-label="Wi-Fi" class="rk-toggle tg"><span class="rk-toggle__knob"></span></button>'],
      [ui.SegmentedControl, { value: 'b', options: [{ value: 'a', label: 'A' }, { value: 'b', label: 'B', title: 'bee' }], onChange: noop, label: 'Mode' }, [], '<div class="rk-seg" role="radiogroup" aria-label="Mode"><button type="button" role="radio" aria-checked="false" class="rk-seg__item">A</button><button type="button" role="radio" aria-checked="true" title="bee" class="rk-seg__item is-on">B</button></div>'],
      [ui.Card, { onClick: noop, selected: true, label: 'Card', flush: true }, ['c'], '<div class="rk-card rk-card--interactive is-flush is-selected" role="button" tabindex="0" aria-label="Card" aria-pressed="true">c</div>'],
      [ui.Card, { as: 'section' }, ['plain'], '<section class="rk-card">plain</section>'],
      [ui.Skeleton, { width: 10 }, [], '<span aria-hidden="true" class="rk-skeleton" style="width:10px;height:12px"></span>'],
      [ui.SkeletonRows, { rows: 2, label: 'Loading rows' }, [], '<div role="status" aria-label="Loading rows" aria-busy="true" class="rk-skeleton-rows"><span aria-hidden="true" class="rk-skeleton" style="width:100%;height:12px"></span><span aria-hidden="true" class="rk-skeleton" style="width:60%;height:12px"></span></div>'],
      [ui.Checkbox, { checked: true, onChange: noop, label: 'Opt' }, [], '<label class="rk-check"><input type="checkbox" class="rk-check__input" checked=""/><span class="rk-check__box" aria-hidden="true">✓</span><span class="rk-check__label">Opt</span></label>'],
      [ui.Select, { value: 'b', options: [{ value: 'a', label: 'A' }, { value: 'b', label: 'B', disabled: true }], onChange: noop, label: 'Pick' }, [], '<select class="rk-select" aria-label="Pick"><option value="a">A</option><option value="b" disabled="" selected="">B</option></select>'],
      [ui.Slider, { value: 42, onChange: noop, label: 'Vol', format: value => `${value} percent` }, [], '<input type="range" class="rk-slider" aria-label="Vol" min="0" max="100" step="1" aria-valuetext="42 percent" value="42"/>'],
      [ui.Notice, { tone: 'bad', title: 'Err', action: h('a', null, 'Fix') }, ['Went wrong'], '<div role="alert" class="rk-notice rk-notice--bad"><div class="rk-notice__body"><strong class="rk-notice__title">Err</strong><span class="rk-notice__text">Went wrong</span></div><div class="rk-notice__action"><a>Fix</a></div></div>'],
      [ui.Notice, {}, ['fine'], '<div role="status" class="rk-notice rk-notice--info"><div class="rk-notice__body"><span class="rk-notice__text">fine</span></div></div>'],
      [ui.ProgressBar, { value: 150, max: 100, tone: 'ok', label: 'Up' }, [], '<div role="progressbar" aria-label="Up" aria-valuemin="0" aria-valuemax="100" aria-valuenow="100" class="rk-progress rk-progress--ok"><span class="rk-progress__fill" style="width:100%"></span></div>'],
      [ui.Badge, { tone: 'ok' }, ['ok'], '<span class="rk-badge rk-badge--ok">ok</span>'],
      [ui.EmptyState, { icon: '!', title: 'None', action: h('button', null, 'Add') }, ['nothing'], '<div class="rk-empty"><span class="rk-empty__icon" aria-hidden="true">!</span><strong>None</strong><p>nothing</p><button>Add</button></div>'],
      [ui.Field, { label: 'Name', note: 'hint' }, [props => h('input', props)], '<div class="rk-field"><label for="ID">Name</label><input id="ID" aria-describedby="ID-note"/><small id="ID-note">hint</small></div>'],
      [ui.StatusRow, { tone: 'warn', label: 'Queue', value: '3' }, [], '<div class="rk-status__row"><span><i class="rk-dot rk-dot--warn"></i>Queue</span><span>3</span></div>'],
    ];
    for (const [component, props, children, expected] of cases) assert.equal(render(component, props, ...children), expected);
    assert.equal(ui.cx('a', false, null, undefined, 'b'), 'a b');
    assert.equal(render(ui.ProgressBar, { value: -5, label: 'x' }), '<div role="progressbar" aria-label="x" aria-valuemin="0" aria-valuemax="100" aria-valuenow="0" class="rk-progress rk-progress--accent"><span class="rk-progress__fill" style="width:0%"></span></div>');
    assert.equal(render(ui.Kbd, { chord: 'mod+k', platform: 'windows' }), '<span class="rightkit-kbd-chord" aria-label="Ctrl + K" style="--rightkit-kbd-gap:4px"><kbd class="rightkit-kbd-key">Ctrl</kbd><kbd class="rightkit-kbd-key">K</kbd></span>', 'Kbd is the platform-ui component');
    return { primitives: cases.length };
  });

  await run('dialogs-palette-help-toasts-table-and-recorder-markup', async () => {
    const danger = plain(render(ui.ConfirmDialog, { title: 'Delete?', description: 'Cannot undo.', danger: true, onConfirm: noop, onCancel: noop }));
    assert.equal(danger, '<div class="rk-overlay rk-overlay--center"><div role="alertdialog" aria-modal="true" aria-labelledby="ID" aria-describedby="ID" tabindex="-1" style="width:440px" class="rk-modal"><h2 id="ID" class="rk-modal__title">Delete?</h2><p id="ID" class="rk-modal__desc">Cannot undo.</p><div class="rk-modal__actions"><button type="button" class="rk-btn rk-btn--secondary" autofocus="">Cancel</button><button type="button" class="rk-btn rk-btn--danger">Confirm</button></div></div></div>');
    const safe = plain(render(ui.ConfirmDialog, { title: 'Save?', confirmLabel: 'Save', onConfirm: noop, onCancel: noop }));
    assert(safe.includes('role="dialog"') && safe.includes('<button type="button" class="rk-btn rk-btn--primary" autofocus="">Save</button>'), 'a non-destructive confirm focuses the confirm button');
    const secret = plain(render(ui.PromptDialog, { title: 'Rename', label: 'Name', initialValue: 'a', secret: true, onSubmit: noop, onCancel: noop }));
    assert(secret.includes('<form class="rk-modal__form"><label for="ID" class="rk-modal__label">Name</label><input class="rk-input" id="ID" autofocus="" type="password" autoComplete="off" spellCheck="false" value="a"/>') && secret.includes('<button type="submit" class="rk-btn rk-btn--primary">OK</button>'));
    const actions = plain(render(ui.ActionDialog, { title: 'Three', onClose: noop, role: 'alertdialog', width: 300, actions: [{ id: 'a', label: 'A', onClick: noop }, { id: 'b', label: 'B', variant: 'danger', onClick: noop, autoFocus: true, disabled: true }] }));
    assert(actions.includes('role="alertdialog"') && actions.includes('style="width:300px"') && actions.includes('<button type="button" class="rk-btn rk-btn--danger" disabled="" autofocus="">B</button>'));
    const modal = plain(render(ui.ModalShell, { label: 'Settings', onClose: noop, align: 'top', width: 500, className: 'surface', overlayClassName: 'scrim', slots: { root: 'sr', overlay: 'so' }, ariaLabel: 'Named' }, 'body'));
    assert.equal(modal, '<div class="rk-overlay rk-overlay--top scrim so"><div role="dialog" aria-modal="true" aria-label="Named" tabindex="-1" style="width:500px" class="rk-modal surface sr">body</div></div>');
    const dialog = plain(render(ui.Dialog, { title: 'Title', description: 'Desc', onClose: noop, actions: h('b', null, 'act'), slots: { body: 'bodyslot', footer: 'foot', header: 'head' } }, 'content'));
    assert(dialog.includes('<h2 id="ID" class="rk-modal__title head">Title</h2><p id="ID" class="rk-modal__desc">Desc</p><div class="rk-modal__body bodyslot">content</div><div class="rk-modal__actions foot"><b>act</b></div>'));

    const registry = shell.createShortcutRegistry({ platform: 'macos' });
    registry.register({ id: 'go.home', chord: 'g h', label: 'Go home', group: 'Navigation', run: noop });
    registry.register({ id: 'file.save', chord: 'mod+s', label: 'Save', run: noop });
    registry.register({ id: 'hid', chord: 'mod+h', label: 'Hidden', hidden: true, run: noop });
    const commands = [{ id: 'a', label: 'Open file', group: 'File', hint: 'recent', shortcut: 'file.save', run: noop }, { id: 'b', label: 'Close tab', kind: 'View', sublabel: '/tmp', run: noop }, { id: 'c', label: 'Reload', chord: 'mod+r', run: noop }];
    const palette = plain(render(ui.CommandPalette, { commands, registry, onClose: noop }));
    assert(palette.startsWith('<div class="rk-overlay rk-overlay--top"><div role="dialog" aria-modal="true" aria-label="Command palette" style="width:640px" class="rk-palette"><div class="rk-palette__field"><svg/><input role="combobox" aria-label="Search or run a command…" aria-expanded="true" aria-controls="ID-list" aria-activedescendant="ID-opt-0" placeholder="Search or run a command…" value=""/></div><div id="ID-list" role="listbox" aria-label="Results" class="rk-palette__list">'));
    assert(palette.includes('<div role="group" aria-label="File"><div class="rk-palette__group" aria-hidden="true">File</div><div id="ID-opt-0" role="option" aria-selected="true" data-command="a" class="rk-palette__opt is-active"><span class="rk-palette__label">Open file</span><span class="rightkit-kbd-chord" aria-label="⌘ + S"'), 'a command shows its live registered chord');
    assert(palette.includes('<div role="group" aria-label="Commands">') && palette.includes('<span class="rk-palette__sub">/tmp</span><span class="rk-palette__type">View</span>') && palette.includes('aria-label="⌘ + R"'));
    const empty = plain(render(ui.CommandPalette, { commands, registry, onClose: noop, query: 'zzz', labels: { empty: 'Nothing', title: 'Palette' } }));
    assert(empty.includes('aria-label="Palette"') && empty.includes('<div class="rk-palette__empty">Nothing</div>') && !empty.includes('aria-activedescendant'));
    const controlled = plain(render(ui.CommandPalette, { commands, registry, onClose: noop, query: 'o', activeIndex: 1, groupOrder: ['Commands', 'File'], scopeLabel: '/' }));
    assert(controlled.includes('<span class="rk-palette__scope">/</span>') && controlled.includes('id="ID-opt-1" role="option" aria-selected="true"') && controlled.indexOf('aria-label="Commands"') < controlled.indexOf('aria-label="File"'), 'controlled query, highlight and group order');
    const help = plain(render(ui.ShortcutHelp, { registry, onClose: noop }));
    assert(help.includes('<h2 id="ID" class="rk-modal__title">Keyboard shortcuts</h2>') && help.includes('<li data-shortcut="go.home"><span>Go home</span>') && help.includes('<em>then</em>') && help.includes('<li data-shortcut="file.save">') && !help.includes('Hidden'));

    const bus = shell.createToastBus();
    assert.equal(render(ui.ToastViewport, { bus }), '<div class="rk-toasts" aria-live="polite" aria-atomic="false" data-rk-toasts="true"></div>');
    assert.equal(render(ui.ToastViewport, { bus, placement: 'top-end', className: 'tv', slots: { root: 'sr' } }), '<div class="rk-toasts tv sr" aria-live="polite" aria-atomic="false" data-rk-toasts="true" data-placement="top-end"></div>');
    const table = render(ui.Table, { label: 'Log', dense: true, pending: true }, h(ui.TableCaption, null, 'Cap'), h(ui.TableHead, null, h(ui.TableRow, null, h(ui.TableHeaderCell, { sort: 'ascending', onSort: noop }, 'Time'), h(ui.TableHeaderCell, { align: 'end' }, 'Result'))), h(ui.TableBody, null, h(ui.TableMessage, { colSpan: 2, tone: 'error' }, 'Failed'), h(ui.TableRow, { selected: true, onActivate: noop }, h(ui.TableCell, { rowHeader: true }, 'r1'), h(ui.TableCell, { numeric: true }, '42')), h(ui.TableRow, { disabled: true, error: true }, h(ui.TableCell, null, 'x'), h(ui.TableCell, null, 'y'))));
    assert.equal(table, '<table aria-label="Log" aria-busy="true" class="rk-table rk-table--dense"><caption class="rk-table__caption">Cap</caption><thead class="rk-table__head"><tr class="rk-table__row"><th scope="col" aria-sort="ascending" class="rk-table__th"><button type="button" class="rk-table__sort"><span>Time</span><span class="rk-table__sort-mark" aria-hidden="true">↑</span></button></th><th scope="col" class="rk-table__th rk-table__cell--end">Result</th></tr></thead><tbody class="rk-table__body"><tr class="rk-table__row rk-table__message rk-table__message--error"><td class="rk-table__cell" colSpan="2"><div role="alert">Failed</div></td></tr><tr class="rk-table__row rk-table__row--interactive is-selected" tabindex="0" aria-selected="true"><th scope="row" class="rk-table__cell rk-table__rowhead">r1</th><td class="rk-table__cell rk-table__cell--numeric rk-table__cell--end">42</td></tr><tr class="rk-table__row is-disabled is-error" aria-disabled="true" data-error="true"><td class="rk-table__cell">x</td><td class="rk-table__cell">y</td></tr></tbody></table>');
    assert.equal(render(ui.ErrorBoundary, { fallback: () => h('p', null, 'fallback'), resetKey: 1 }, h('i', null, 'fine')), '<i>fine</i>', 'a healthy subtree renders through the boundary');
    const recorder = plain(render(ui.ShortcutRecorder, { value: 'mod+k', onChange: noop, label: 'Palette', platform: 'macos' }));
    assert.equal(recorder, '<button type="button" class="rk-recorder" aria-label="Palette" aria-pressed="false"><span class="rightkit-kbd-chord" aria-label="⌘ + K" style="--rightkit-kbd-gap:1px"><kbd class="rightkit-kbd-key">⌘</kbd><kbd class="rightkit-kbd-key">K</kbd></span></button>');
    assert.equal(plain(render(ui.ShortcutRecorder, { value: null, onChange: noop, label: 'Palette', platform: 'windows', emptyLabel: 'None', disabled: true })), '<button type="button" class="rk-recorder" aria-label="Palette" aria-pressed="false" disabled=""><span class="rk-recorder__empty">None</span></button>');
    return { surfaces: ['ConfirmDialog', 'PromptDialog', 'ActionDialog', 'ModalShell', 'Dialog', 'CommandPalette', 'ShortcutHelp', 'ToastViewport', 'Table', 'ErrorBoundary', 'ShortcutRecorder'] };
  });

  await run('headless-toggle-and-segmented-control-props', async () => {
    let captured;
    const Probe = () => {
      const on = headless.useToggle({ checked: true, onChange: noop, label: 'Wi-Fi' });
      const off = headless.useToggle({ checked: false, onChange: noop, label: 'Locked', disabled: true });
      const radio = headless.useSegmentedControl({ value: 'b', onChange: noop, label: 'Mode', options: [{ value: 'a' }, { value: 'b' }, { value: 'c', disabled: true }] });
      const toggle = headless.useSegmentedControl({ value: 'a', onChange: noop, label: 'Format', mode: 'toggle', options: [{ value: 'a' }, { value: 'b' }] });
      const optionView = (control, value) => { const { ref, onClick, onKeyDown, ...rest } = control.getOptionProps(value); return [rest, typeof ref, typeof onClick, typeof onKeyDown]; };
      captured = { on, off, radio: radio.groupProps, radioOptions: ['a', 'b', 'c'].map(value => optionView(radio, value)), toggle: toggle.groupProps, toggleOptions: ['a', 'b'].map(value => optionView(toggle, value)) };
      return h('i');
    };
    render(Probe, {});
    const { onClick, onKeyDown, ...onProps } = captured.on;
    assert.deepEqual(onProps, { role: 'switch', 'aria-checked': true, 'aria-label': 'Wi-Fi', 'aria-disabled': false, tabIndex: 0 });
    assert.deepEqual([typeof onClick, typeof onKeyDown], ['function', 'function']);
    assert.deepEqual([captured.off['aria-disabled'], captured.off.tabIndex, captured.off['aria-checked']], [true, -1, false]);
    assert.deepEqual(captured.radio, { role: 'radiogroup', 'aria-label': 'Mode', 'aria-disabled': false });
    assert.deepEqual(captured.radioOptions.map(([props]) => [props.role, props['aria-checked'], props.tabIndex, props['aria-disabled']]), [['radio', false, -1, false], ['radio', true, 0, false], ['radio', false, -1, true]], 'a roving tab stop sits on the selected option');
    assert.deepEqual(captured.radioOptions.map(([, ref, click, key]) => [ref, click, key]), [['function', 'function', 'function'], ['function', 'function', 'function'], ['function', 'function', 'function']]);
    assert.deepEqual(captured.toggle, { role: 'group', 'aria-label': 'Format', 'aria-disabled': false });
    assert.deepEqual(captured.toggleOptions.map(([props]) => [props['aria-pressed'], props.tabIndex]), [[true, 0], [false, 0]], 'toggle mode makes every enabled button a tab stop');
    const calls = [];
    const live = headless.useToggle;
    let handlers;
    render(() => { handlers = live({ checked: false, onChange: value => calls.push(value), label: 'X' }); return h('i'); }, {});
    handlers.onClick();
    handlers.onKeyDown({ key: ' ', preventDefault: () => calls.push('prevented') });
    handlers.onKeyDown({ key: 'Enter', preventDefault: () => calls.push('prevented') });
    handlers.onKeyDown({ key: 'a', preventDefault: () => calls.push('wrong') });
    assert.deepEqual(calls, [true, 'prevented', true, 'prevented', true]);
    return { hooks: ['useToggle', 'useSegmentedControl'] };
  });

  await run('hover-guard-and-stylesheets-ship-consistently', async () => {
    const previous = { window: globalThis.window, document: globalThis.document };
    const win = new FakeTarget();
    const doc = Object.assign(new FakeTarget(), { visibilityState: 'visible' });
    globalThis.window = win;
    globalThis.document = doc;
    try {
      const el = attributeRoot();
      const detach = shell.installHoverGuard(el);
      assert.equal(el.attrs.get('data-rk-pointer'), 'off', 'hover is suppressed from install');
      win.dispatchEvent({ type: 'pointermove', isTrusted: false });
      assert.equal(el.attrs.get('data-rk-pointer'), 'off', 'synthetic movement is ignored');
      win.dispatchEvent({ type: 'pointermove', isTrusted: true });
      assert.equal(el.attrs.get('data-rk-pointer'), 'on');
      doc.dispatchEvent({ type: 'pointerout', relatedTarget: null });
      assert.equal(el.attrs.get('data-rk-pointer'), 'off', 'leaving the window rearms the guard');
      win.dispatchEvent({ type: 'pointermove', isTrusted: true });
      win.dispatchEvent({ type: 'blur' });
      assert.equal(el.attrs.get('data-rk-pointer'), 'off');
      win.dispatchEvent({ type: 'pointermove', isTrusted: true });
      doc.visibilityState = 'hidden';
      doc.dispatchEvent({ type: 'visibilitychange' });
      assert.equal(el.attrs.get('data-rk-pointer'), 'off');
      detach();
      assert.equal(el.attrs.has('data-rk-pointer'), false, 'detach removes the attribute');
      assert.equal(win.size + doc.size, 0);
    } finally {
      if (previous.window === undefined) delete globalThis.window; else globalThis.window = previous.window;
      if (previous.document === undefined) delete globalThis.document; else globalThis.document = previous.document;
    }
    assert.equal(typeof shell.installHoverGuard(null), 'function');
    const strip = text => text.replace(/\/\*[\s\S]*?\*\//g, '');
    const squash = text => strip(text).replace(/\s+/g, '');
    assert.equal(squash(css['shell.css']), squash(css['shell.structure.css'] + css['shell.visual.css']), 'shell.css is the structure and visual files in one');
    assert.equal((strip(css['shell.structure.css']).match(/#[0-9a-fA-F]{3,8}\b|rgba?\(/g) ?? []).length, 0, 'the structure file carries no colour');
    assert(css['shell.visual.css'].includes(':root[data-theme="dark"]') && css['shell.visual.css'].includes('data-rk-motion'));
    assert(css['shell.structure.css'].includes('.rk-overlay') && css['shell.structure.css'].includes('.rk-toasts'));
    assert(css['shell.layered.css'].includes('@import "./shell.structure.css" layer(rk);') && css['shell.layered.css'].includes('@import "./shell.visual.css" layer(rk);'));
    const aliases = [...css['suite-aliases.css'].matchAll(/(--[a-z0-9-]+):\s*var\((--rk-[a-z0-9-]+)\)/g)].map(match => [match[1], match[2]]);
    assert(aliases.length > 30 && aliases.every(([alias, target]) => target === alias.replace(/^--/, '--rk-') || alias === '--wordmark' || alias === '--danger'), 'each unprefixed alias points at its --rk- token');
    assert(aliases.some(([alias, target]) => alias === '--ink' && target === '--rk-ink'));
    return { stylesheets: stylesheets.length, aliases: aliases.length, guardStates: ['off', 'on'] };
  });

  assert.equal(result.scenarios.length, 13);
  result.ok = true;
} catch (error) {
  result.errors.push(failure(error));
  process.exitCode = 1;
} finally {
  delete globalThis.__RIGHTKIT_PLATFORM__;
}
process.stdout.write(`${JSON.stringify(result)}\n`);
