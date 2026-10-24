import { validateInstalledArtifact, fixtureEntry } from './provenance.mjs';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync, realpathSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const recipeId = 'i18n-catalog-provider-ssr-consumer';
const result = {
  schemaVersion: 1, recipeId, ok: false, nativeHostQualified: false,
  host: { platform: process.platform, arch: process.arch, node: process.version },
  artifact: null, scenarios: [], errors: [],
};
const required = name => { assert(process.env[name], `${name} is required`); return process.env[name]; };
// Fixture failures report fixed scenario names only: never serialize an untrusted error.
let stage = 'artifact-provenance';
let dom = null;
let cleanup = null;
try {
  const archive = path.resolve(required('RIGHTKIT_CONSUMER_ARTIFACT'));
  assert(path.isAbsolute(archive), 'artifact must be absolute');
  const sha256 = required('RIGHTKIT_CONSUMER_ARTIFACT_SHA256');
  const version = required('RIGHTKIT_CONSUMER_EXPECTED_VERSION');
  const resolution = required('RIGHTKIT_CONSUMER_RESOLUTION');
  assert.match(sha256, /^[a-f0-9]{64}$/);
  assert(['packed', 'fetched'].includes(resolution));
  assert.equal(createHash('sha256').update(readFileSync(archive)).digest('hex'), sha256);
  const root = import.meta.resolve('@rightkit/i18n');
  const provenance = validateInstalledArtifact({
    fixtureUrl: import.meta.url, archive, sha256, version, resolution, packageName: '@rightkit/i18n', rootResolvedUrl: root,
    entries: { '@rightkit/i18n': root },
  });
  result.artifact = provenance.artifact;
  const packageRoot = realpathSync(path.resolve(path.dirname(realpathSync(fileURLToPath(root))), '..'));
  const installed = JSON.parse(readFileSync(path.join(packageRoot, 'package.json'), 'utf8'));
  const dependency = specifier => import(fixtureEntry(import.meta.url, import.meta.resolve(specifier)));
  const React = await dependency('react');
  const { renderToStaticMarkup } = await dependency('react-dom/server');
  const h = React.createElement;
  const i18nModule = await import(provenance.entries['@rightkit/i18n']);
  const { createI18n, mergeCatalog } = i18nModule;
  const run = async (name, body) => { stage = name; const evidence = await body(); result.scenarios.push({ name, ok: true, evidence }); };
  const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
  const gate = () => { let resolve, reject; const promise = new Promise((res, rej) => { resolve = res; reject = rej; }); return { promise, resolve, reject }; };

  await run('installed-manifest-types-and-exact-export-surface', async () => {
    assert.equal(installed.name, '@rightkit/i18n');
    assert.equal(installed.version, version);
    assert.equal(installed.type, 'module');
    assert.deepEqual(installed.exports, { '.': { types: './dist/index.d.ts', import: './dist/index.js' } });
    assert.deepEqual(installed.peerDependencies, { react: '>=18 <21' });
    assert.equal(installed.sideEffects, false);
    for (const field of ['dependencies', 'optionalDependencies']) assert.equal(installed[field], undefined);
    assert.deepEqual(Object.keys(i18nModule).sort(), ['createI18n', 'mergeCatalog']);
    assert.equal(typeof createI18n, 'function');
    assert.equal(typeof mergeCatalog, 'function');
    // The type declarations ship and name every public type and function.
    const declarations = readFileSync(path.join(packageRoot, 'dist', 'index.d.ts'), 'utf8');
    const names = [...declarations.matchAll(/^export (?:declare )?(?:function|interface|type|const) (\w+)/gm)].map(match => match[1]).sort();
    assert.deepEqual(names, ['Catalog', 'I18nOptions', 'I18nValue', 'Locale', 'LocaleInfo', 'LocaleLoader', 'LocalePersistence', 'Message', 'TranslateOptions', 'createI18n', 'mergeCatalog']);
    for (const file of ['dist/index.js', 'dist/index.d.ts', 'README.md', 'LICENSE-MIT', 'LICENSE-APACHE']) assert.ok(readFileSync(path.join(packageRoot, ...file.split('/'))).length > 0, file);
    // react is the one peer: the module imports it, the API surface of createI18n is fixed.
    const instance = createI18n({ a: 'A' });
    assert.deepEqual(Object.keys(instance).sort(), ['I18nProvider', 'Provider', 'defaultLocale', 'isLoaded', 'loadLocale', 'locales', 'mergeCatalog', 'normalizeLocale', 't', 'useI18n']);
    assert.equal(instance.Provider, instance.I18nProvider);
    assert.equal(instance.defaultLocale, 'en');
    assert.deepEqual(instance.locales, [{ id: 'en', name: 'en', englishName: 'en' }]);
    return { exports: ['.'], moduleExports: 2, instanceKeys: 10, declaredTypes: names.length - 2 };
  });

  await run('merge-catalog-recursion-and-immutability', async () => {
    const english = { language: { label: 'Language' }, nested: { keep: 'Keep', replace: 'Replace' }, list: ['a', 'b'], count: 3 };
    const frozen = structuredClone(english);
    const merged = mergeCatalog(english, { nested: { replace: 'neu', added: 'Added' }, list: ['c'], count: 'three', extra: { deep: { value: 1 } } });
    assert.deepEqual(merged, { language: { label: 'Language' }, nested: { keep: 'Keep', replace: 'neu', added: 'Added' }, list: ['c'], count: 'three', extra: { deep: { value: 1 } } });
    assert.deepEqual(english, frozen, 'the base catalog is never mutated');
    assert.notEqual(merged, english);
    assert.equal(mergeCatalog(english, undefined), english, 'a non-object override returns the base');
    assert.equal(mergeCatalog(english, null), english);
    assert.equal(mergeCatalog(english, 'text'), english);
    assert.equal(mergeCatalog(english, ['x']), english, 'an array override is not a catalog');
    assert.deepEqual(mergeCatalog(english, {}), english);
    assert.deepEqual(mergeCatalog({ a: { b: 1 } }, { a: 'flat' }), { a: 'flat' }, 'a leaf replaces a branch');
    assert.deepEqual(mergeCatalog({ a: 'flat' }, { a: { b: 1 } }), { a: { b: 1 } }, 'a branch replaces a leaf');
    const instance = createI18n(english);
    assert.deepEqual(instance.mergeCatalog({ nested: { replace: 'x' } }).nested, { keep: 'Keep', replace: 'x' }, 'the instance form merges over its own catalog');
    return { recursive: true, arraysReplace: true, inputsUnchanged: true };
  });

  await run('synchronous-lookup-fallbacks-and-function-messages', async () => {
    const sync = createI18n(
      { greeting: 'Hello', 'Open Log Folder.': 'Open Log Folder.', menu: { open: 'Open', quit: 'Quit' }, count: 3, common: { tryHint: name => `Try ${name}` }, theme: { installed: name => `${name} installed` } },
      { loaders: { fr: () => ({ greeting: 'Bonjour', 'Open Log Folder.': 'Ouvrir le dossier.', menu: { open: 'Ouvrir' }, common: { tryHint: name => `Essayez ${name}` } }) } },
    );
    // The default locale and unloaded locales read the default catalog; nothing loads.
    assert.equal(sync.t('en', 'greeting'), 'Hello');
    assert.equal(sync.isLoaded('en'), true);
    assert.equal(sync.isLoaded('fr'), false);
    assert.equal(sync.t('fr', 'greeting'), 'Hello');
    assert.equal(sync.isLoaded('fr'), false, 't never loads');
    const loaded = await sync.loadLocale('fr');
    assert.equal(sync.isLoaded('fr'), true);
    assert.equal(loaded.greeting, 'Bonjour');
    assert.equal(loaded.menu.quit, 'Quit', 'loadLocale returns the translated catalog merged over the default one');
    assert.equal(sync.t('fr', 'greeting'), 'Bonjour');
    // Dotted paths, and an exact key that itself contains a dot.
    assert.equal(sync.t('fr', 'menu.open'), 'Ouvrir');
    assert.equal(sync.t('fr', 'Open Log Folder.'), 'Ouvrir le dossier.');
    // Missing in the locale falls back key by key; missing everywhere uses the fallback option, else the key.
    assert.equal(sync.t('fr', 'menu.quit'), 'Quit');
    assert.equal(sync.t('fr', 'Save as...', { fallback: 'Save' }), 'Save');
    assert.equal(sync.t('fr', 'Save as...'), 'Save as...');
    // A non-string value is missing, and so are inherited properties.
    for (const key of ['count', 'menu', 'toString', 'constructor', 'menu.toString']) assert.equal(sync.t('fr', key), key, key);
    // An empty translation is a translation.
    const blank = createI18n({ x: 'X' }, { loaders: { de: () => ({ x: '' }) } });
    await blank.loadLocale('de');
    assert.equal(blank.t('de', 'x'), '');
    // Function messages get their args in lookups; plain strings ignore them.
    assert.equal(sync.t('en', 'common.tryHint', { args: ['settings'] }), 'Try settings');
    assert.equal(sync.t('fr', 'common.tryHint', { args: ['settings'] }), 'Essayez settings');
    assert.equal(sync.t('fr', 'theme.installed', { args: ['Dark'] }), 'Dark installed');
    assert.equal(sync.t('en', 'greeting', { args: ['ignored'] }), 'Hello');
    assert.equal(sync.t('en', 'missing.key', { fallback: 'Nope' }), 'Nope');
    // Loaders may be asynchronous, are called with the locale, and their result is cached.
    let calls = 0;
    const lazy = createI18n({ a: 'A' }, { loaders: { it: async locale => { calls += 1; return { a: `${locale}:Uno` }; } } });
    assert.equal((await lazy.loadLocale('it')).a, 'it:Uno');
    await lazy.loadLocale('it');
    assert.equal(calls, 1, 'a loaded catalog is not loaded again');
    assert.equal((await lazy.loadLocale('xx')).a, 'A', 'a locale without a loader is the default catalog');
    await assert.rejects(createI18n({ a: 'A' }, { loaders: { bad: () => { throw new Error('no catalog'); } } }).loadLocale('bad'), /no catalog/);
    return { lookups: 16, functionMessages: 4, loaderCached: true };
  });

  await run('locale-registry-normalization-and-aliases', async () => {
    const locales = [
      { id: 'en', name: 'English', englishName: 'English' },
      { id: 'zh', name: '中文', englishName: 'Chinese' },
      { id: 'zh-hant', name: '繁體中文', englishName: 'Chinese (Traditional)', aliases: ['zh-TW', 'zh_HK'] },
      { id: 'ar', name: 'العربية', englishName: 'Arabic' },
    ];
    const i18n = createI18n({ x: 'X' }, { locales });
    const expected = { 'zh-CN': 'zh', 'zh-TW': 'zh-hant', zh_HK: 'zh-hant', 'zh-hant-TW': 'zh-hant', AR: 'ar', ' ar ': 'ar', 'en-GB': 'en', 'fr-CA': undefined, '': undefined, '---': undefined };
    for (const [tag, id] of Object.entries(expected)) assert.equal(i18n.normalizeLocale(tag), id, JSON.stringify(tag));
    assert.deepEqual(i18n.locales, locales);
    // Without a registry the loader keys are the registry, and the default locale always resolves.
    const loaderOnly = createI18n({ x: 'X' }, { defaultLocale: 'de', loaders: { fr: () => ({}) } });
    assert.equal(loaderOnly.defaultLocale, 'de');
    assert.equal(loaderOnly.normalizeLocale('fr-CA'), 'fr');
    assert.equal(loaderOnly.normalizeLocale('DE'), 'de');
    assert.equal(loaderOnly.normalizeLocale('ja'), undefined);
    assert.deepEqual(loaderOnly.locales, [{ id: 'de', name: 'de', englishName: 'de' }]);
    return { tagsChecked: 10, registryFromLoaders: true };
  });

  await run('server-rendered-markup-from-loaded-catalogs', async () => {
    const i18n = createI18n({ language: { label: 'Language' }, nav: { home: 'Home', help: 'Help' }, greet: name => `Hello ${name}` }, {
      loaders: { fr: () => ({ language: { label: 'Langue' }, nav: { home: 'Accueil' } }), ar: () => ({ language: { label: 'اللغة' } }) }, rtlLocales: ['ar'],
    });
    const Probe = () => {
      const { locale, t, loading, error, saving, saveError, locales } = i18n.useI18n();
      return h('section', { lang: locale }, h('output', { id: 'label' }, `${locale}:${t.language.label}`), h('nav', null, `${t.nav.home}|${t.nav.help}`), h('p', null, t.greet('Ada')), h('i', null, `${loading}/${error}/${saving}/${saveError}/${locales.length}`));
    };
    const render = props => renderToStaticMarkup(h(i18n.Provider, props, h(Probe)));
    const english = render({});
    assert.equal(english, '<section lang="en"><output id="label">en:Language</output><nav>Home|Help</nav><p>Hello Ada</p><i>false/null/false/null/1</i></section>');
    // A Provider can only start in a locale whose catalog is loaded; the others render the default catalog until loaded.
    assert.equal(render({ initialLocale: 'fr' }), english.replace('lang="en"', 'lang="fr"').replace('en:Language', 'fr:Language'), 'an unloaded locale shows the default catalog');
    await i18n.loadLocale('fr');
    const french = render({ initialLocale: 'fr' });
    assert.equal(french, '<section lang="fr"><output id="label">fr:Langue</output><nav>Accueil|Help</nav><p>Hello Ada</p><i>false/null/false/null/1</i></section>', 'translated keys, English for the rest');
    assert.equal(render({ initialLocale: 'fr' }), french, 'rendering is deterministic');
    assert.equal(i18n.I18nProvider, i18n.Provider);
    await i18n.loadLocale('ar');
    assert.match(render({ initialLocale: 'ar' }), /ar:اللغة<\/output><nav>Home\|Help<\/nav>/);
    // Without a Provider, the hook reads the default catalog.
    assert.equal(renderToStaticMarkup(h(Probe)), english);
    // A different instance does not share catalogs or context.
    const other = createI18n({ language: { label: 'Other' } });
    const Foreign = () => h('b', null, other.useI18n().t.language.label);
    assert.equal(renderToStaticMarkup(h(i18n.Provider, null, h(Foreign))), '<b>Other</b>');
    return { markup: 4, fallbackMerged: true, rtlCatalog: true, noProvider: true, isolatedInstances: true };
  });

  await run('provider-effects-in-a-dom-set-locale-direction-and-errors', async () => {
    const { JSDOM } = await dependency('jsdom');
    dom = new JSDOM('<!doctype html><html><body></body></html>', { url: 'https://consumer.invalid/', pretendToBeVisual: true });
    for (const key of ['window', 'document', 'navigator', 'HTMLElement', 'Element', 'Node', 'MutationObserver', 'getComputedStyle']) {
      Object.defineProperty(globalThis, key, { configurable: true, writable: true, value: key === 'getComputedStyle' ? dom.window.getComputedStyle.bind(dom.window) : dom.window[key] });
    }
    globalThis.IS_REACT_ACT_ENVIRONMENT = true;
    const testing = await dependency('@testing-library/react');
    cleanup = testing.cleanup;
    const { render, screen, waitFor, act } = testing;
    const i18n = createI18n({ language: { label: 'Language' }, nested: { keep: 'Keep', replace: 'Replace' } }, {
      loaders: { ar: () => ({ language: { label: 'اللغة' } }), broken: () => { throw new Error('catalog unavailable'); } }, rtlLocales: ['ar'],
    });
    let api;
    const Probe = () => {
      api = i18n.useI18n();
      return h('div', null, h('output', { 'data-testid': 'locale' }, api.locale), h('output', { 'data-testid': 'label' }, api.t.language.label), h('output', { 'data-testid': 'keep' }, api.t.nested.keep), h('output', { 'data-testid': 'flags' }, `${api.loading}/${api.error?.message ?? ''}`));
    };
    render(h(i18n.Provider, null, h(Probe)));
    assert.equal(screen.getByTestId('label').textContent, 'Language');
    assert.equal(document.documentElement.lang, 'en');
    assert.equal(document.documentElement.dir, 'ltr');
    // A selection loads the catalog, applies language and direction, and keeps the English keys it does not translate.
    await act(async () => { await api.setLocale('ar'); });
    await waitFor(() => assert.equal(screen.getByTestId('label').textContent, 'اللغة'));
    assert.equal(screen.getByTestId('locale').textContent, 'ar');
    assert.equal(screen.getByTestId('keep').textContent, 'Keep');
    await waitFor(() => assert.equal(document.documentElement.dir, 'rtl'));
    assert.equal(document.documentElement.lang, 'ar');
    assert.equal(screen.getByTestId('flags').textContent, 'false/');
    assert.equal(i18n.isLoaded('ar'), true);
    // A failed load rejects, records the error, and leaves the language as it was.
    await act(async () => { await assert.rejects(api.setLocale('broken'), /catalog unavailable/); });
    await waitFor(() => assert.equal(screen.getByTestId('flags').textContent, 'false/catalog unavailable'));
    assert.equal(screen.getByTestId('locale').textContent, 'ar');
    await act(async () => { await api.setLocale('en'); });
    await waitFor(() => assert.equal(screen.getByTestId('label').textContent, 'Language'));
    await waitFor(() => assert.equal(document.documentElement.dir, 'ltr'));
    assert.equal(screen.getByTestId('flags').textContent, 'false/', 'the next selection clears the error');
    // initialLocale loads its catalog after mount without a selection.
    testing.cleanup();
    const second = createI18n({ language: { label: 'Language' } }, { loaders: { fr: () => ({ language: { label: 'Langue' } }) } });
    let secondApi;
    const Show = () => { secondApi = second.useI18n(); return h('output', { 'data-testid': 'out' }, `${secondApi.locale}:${secondApi.t.language.label}`); };
    render(h(second.Provider, { initialLocale: 'fr' }, h(Show)));
    await waitFor(() => assert.equal(screen.getByTestId('out').textContent, 'fr:Langue'));
    return { loadedAndApplied: true, directionToggled: true, failedLoadKeepsLocale: true, initialLocaleLoads: true };
  });

  await run('persistence-restore-save-rollback-and-ordering', async () => {
    const { render, screen, waitFor, act, cleanup: clean } = await dependency('@testing-library/react');
    clean();
    const locales = [
      { id: 'en', name: 'English', englishName: 'English' },
      { id: 'zh', name: '中文', englishName: 'Chinese' },
      { id: 'zh-hant', name: '繁體中文', englishName: 'Chinese (Traditional)', aliases: ['zh-TW', 'zh_HK'] },
      { id: 'ar', name: 'العربية', englishName: 'Arabic' },
    ];
    // The stored tag is restored after mount, through the registry, and is not saved again.
    const saved = [];
    const restoring = createI18n({ language: { label: 'Language' } }, { locales, persistence: { load: async () => 'zh-TW', save: async locale => { saved.push(locale); } }, loaders: { 'zh-hant': () => ({ language: { label: '語言' } }) } });
    const Show = () => { const { locale, t } = restoring.useI18n(); return h('output', { 'data-testid': 'out' }, `${locale}:${t.language.label}`); };
    render(h(restoring.Provider, null, h(Show)));
    await waitFor(() => assert.equal(screen.getByTestId('out').textContent, 'zh-hant:語言'));
    assert.deepEqual(saved, []);
    clean();
    // A stored tag that no longer resolves is ignored.
    const stale = createI18n({ language: { label: 'Language' } }, { locales, persistence: { load: async () => 'xx-YY', save: async () => undefined } });
    const Stale = () => h('output', { 'data-testid': 'stale' }, stale.useI18n().locale);
    render(h(stale.Provider, null, h(Stale)));
    await act(async () => { await sleep(30); });
    assert.equal(screen.getByTestId('stale').textContent, 'en');
    clean();

    // A chosen language is saved; a failed save rolls back, sets saveError and rejects; the next try succeeds.
    let failSave = true;
    const stored = [];
    const persistence = { load: async () => undefined, save: async locale => { if (failSave) throw new Error('disk full'); stored.push(locale); } };
    const i18n = createI18n({ language: { label: 'Language' } }, { locales, persistence, loaders: { ar: () => ({ language: { label: 'اللغة' } }) }, rtlLocales: ['ar'] });
    let api;
    const Probe = () => { api = i18n.useI18n(); return h('output', { 'data-testid': 'locale' }, `${api.locale}/${api.saving}/${api.saveError?.message ?? ''}`); };
    render(h(i18n.Provider, null, h(Probe)));
    await act(async () => { await assert.rejects(api.setLocale('ar'), /disk full/); });
    await waitFor(() => assert.equal(api.saveError?.message, 'disk full'));
    assert.equal(api.locale, 'en', 'the shown language rolled back');
    assert.equal(api.saving, false);
    failSave = false;
    await act(async () => { await api.setLocale('ar'); });
    await waitFor(() => assert.equal(screen.getByTestId('locale').textContent, 'ar/false/'));
    assert.deepEqual(stored, ['ar']);
    assert.equal(api.saveError, null);
    clean();

    // Saves run in call order, and an older failure cannot roll back a newer selection.
    const pending = [gate(), gate()];
    const order = [];
    let calls = 0;
    const ordered = createI18n({ language: { label: 'Language' } }, {
      persistence: { load: async () => undefined, save: locale => { order.push(locale); return pending[calls++].promise; } },
      loaders: { ar: () => ({ language: { label: 'اللغة' } }), fr: () => ({ language: { label: 'Langue' } }) },
    });
    let select;
    const Gen = () => { const value = ordered.useI18n(); select = value.setLocale; return h('output', { 'data-testid': 'gen' }, value.locale); };
    render(h(ordered.Provider, null, h(Gen)));
    let first; let second;
    await act(async () => { first = select('ar').then(() => null, cause => cause); });
    await waitFor(() => assert.equal(screen.getByTestId('gen').textContent, 'ar'));
    await act(async () => { second = select('fr'); });
    await waitFor(() => assert.equal(screen.getByTestId('gen').textContent, 'fr'));
    await act(async () => { pending[0].reject(new Error('disk full')); assert.ok((await first) instanceof Error); });
    assert.equal(screen.getByTestId('gen').textContent, 'fr', 'a superseded failure must not roll the language back');
    await act(async () => { pending[1].resolve(); await second; });
    assert.equal(screen.getByTestId('gen').textContent, 'fr');
    assert.deepEqual(order, ['ar', 'fr']);
    clean();

    // A delayed startup restore cannot overwrite an explicit selection.
    const restore = gate();
    const delayed = createI18n({ language: { label: 'Language' } }, {
      persistence: { load: () => restore.promise, save: async () => undefined },
      loaders: { ar: () => ({ language: { label: 'اللغة' } }), fr: () => ({ language: { label: 'Langue' } }) },
    });
    let choose;
    const Late = () => { const value = delayed.useI18n(); choose = value.setLocale; return h('output', { 'data-testid': 'late' }, value.locale); };
    render(h(delayed.Provider, null, h(Late)));
    await act(async () => { await choose('ar'); });
    assert.equal(screen.getByTestId('late').textContent, 'ar');
    await act(async () => { restore.resolve('fr'); await sleep(20); });
    assert.equal(screen.getByTestId('late').textContent, 'ar');
    return { restored: true, staleTagIgnored: true, saveFailureRollsBack: true, savesOrdered: true, supersededFailureIgnored: true, lateRestoreIgnored: true };
  });

  stage = 'qualification-complete';
  result.ok = true;
} catch (error) {
  result.errors.push({ stage, code: 'consumer-assertion-failed' });
  process.exitCode = 1;
} finally {
  try { cleanup?.(); } catch { /* the DOM is being discarded */ }
  try { dom?.window.close(); } catch { /* already closed */ }
}
process.stdout.write(`${JSON.stringify(result)}\n`);
