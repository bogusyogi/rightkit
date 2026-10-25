import { validateInstalledArtifact } from './provenance.mjs';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import vm from 'node:vm';

const recipeId = 'theme-brand-tokens-cli-consumer';
const result = { recipeId, schemaVersion: 1, ok: false, host: { platform: process.platform, arch: process.arch, node: process.version }, artifact: null, scenarios: [], errors: [], nativeHostQualified: false };
const failure = error => ({ name: error?.name ?? 'Error', message: String(error?.message ?? error) });
const required = name => { assert(process.env[name], `${name} is required`); return process.env[name]; };
const rootSurface = ['BrandError', 'DEFAULT_APPROVED_CONTEXTS', 'DEFAULT_APPROVED_VALUES', 'OVERRIDABLE_TOKENS', 'SUITE_ALIASES', 'SUITE_DEFAULTS', 'aliasCss', 'aliasTokens', 'applyBrand', 'applyThemeAttribute', 'applyTokens', 'assertCustomProperty', 'brandFromAccent', 'checkBrandContrast', 'checkGeneratedCss', 'contrastRatio', 'ensureContrast', 'fontTokens', 'generateCss', 'isHex', 'luminance', 'mix', 'modeVars', 'onPrefersDarkChange', 'parseBrand', 'parseHex', 'prefersDark', 'presetCss', 'presetTokens', 'readableOn', 'requiredVars', 'resolveBrand', 'scanHardcodedColors', 'shade', 'tailwindThemeCss', 'themeAttributeValue', 'themeBootScript', 'toHex'];
const nodeSurface = ['checkBrandFile', 'checkGeneratedCss', 'checkLogo', 'generateFile', 'readBrand', 'scanFiles'];

// A synthetic brand: invented name, colours and font; nothing here is a real product.
const brandJson = () => ({ name: 'Fixture Suite', code: 'FS', colors: { brand: '#0C6AA5', 'brand-ink': '#0A5A8C' }, wordmark: 'FixtureSuite', wordmarkFont: '"Fixture Sans", sans-serif', logo: { master: 'logo.svg' } });
const fakeRoot = () => {
  const props = new Map();
  return { props, style: { setProperty: (name, value) => { props.set(name, value); }, removeProperty: name => { const old = props.get(name) ?? ''; props.delete(name); return old; }, getPropertyValue: name => props.get(name) ?? '' } };
};
const fakeAttrRoot = () => {
  const attrs = new Map();
  return { attrs, setAttribute: (name, value) => { attrs.set(name, value); }, removeAttribute: name => { attrs.delete(name); }, getAttribute: name => attrs.get(name) ?? null };
};
function bootAttribute(script, { stored = {}, prefersDark = false, throwStorage = false } = {}) {
  const root = fakeAttrRoot();
  const context = {
    localStorage: { getItem: key => { if (throwStorage) throw new Error('storage blocked'); return Object.hasOwn(stored, key) ? stored[key] : null; } },
    matchMedia: () => ({ matches: prefersDark }),
    document: { documentElement: root },
  };
  vm.runInNewContext(script, context, { timeout: 5000 });
  return root.attrs.get('data-theme') ?? null;
}

let scratch;
try {
  const archive = path.resolve(required('RIGHTKIT_CONSUMER_ARTIFACT'));
  const sha256 = required('RIGHTKIT_CONSUMER_ARTIFACT_SHA256');
  const version = required('RIGHTKIT_CONSUMER_EXPECTED_VERSION');
  const resolution = required('RIGHTKIT_CONSUMER_RESOLUTION');
  assert(['packed', 'fetched'].includes(resolution));
  assert.match(sha256, /^[a-f0-9]{64}$/);
  assert.equal(createHash('sha256').update(readFileSync(archive)).digest('hex'), sha256);
  const root = import.meta.resolve('@rightkit/theme');
  const provenance = validateInstalledArtifact({ fixtureUrl: import.meta.url, archive, sha256, version, resolution, packageName: '@rightkit/theme', rootResolvedUrl: root, entries: { '@rightkit/theme': root, '@rightkit/theme/node': import.meta.resolve('@rightkit/theme/node') } });
  result.artifact = provenance.artifact;
  const packageRoot = path.join(path.dirname(provenance.artifact.modulePath), '..');
  const manifest = JSON.parse(readFileSync(path.join(packageRoot, 'package.json'), 'utf8'));
  const theme = await import(provenance.entries['@rightkit/theme']);
  const node = await import(provenance.entries['@rightkit/theme/node']);
  const binPath = path.join(packageRoot, 'bin', 'rightkit-theme.mjs');

  async function run(name, operation) {
    const entry = { name, ok: false, evidence: null, error: null };
    result.scenarios.push(entry);
    try { entry.evidence = await operation(); entry.ok = true; }
    catch (error) { entry.error = failure(error); throw error; }
  }

  await run('export-surface-exact-and-bin-declared', async () => {
    assert.deepEqual(Object.keys(theme).sort(), rootSurface);
    assert.deepEqual(Object.keys(node).sort(), nodeSurface);
    assert.deepEqual(Object.keys(manifest.exports), ['.', './node']);
    assert.deepEqual(manifest.bin, { 'rightkit-theme': 'bin/rightkit-theme.mjs' });
    assert.equal(manifest.sideEffects, false);
    assert.equal(manifest.peerDependencies, undefined);
    assert.equal(node.checkGeneratedCss, theme.checkGeneratedCss, 'the node entry re-exports the browser-safe checker');
    return { rootExports: rootSurface.length, nodeExports: nodeSurface.length, bin: 'rightkit-theme' };
  });

  await run('colour-maths-and-contrast', async () => {
    assert.deepEqual(theme.parseHex('#fff'), { r: 255, g: 255, b: 255 });
    assert.deepEqual(theme.parseHex('  #0c6aa5 '), { r: 12, g: 106, b: 165 });
    assert.deepEqual(theme.parseHex('#0C6AA5FF'), { r: 12, g: 106, b: 165 }, 'alpha is ignored');
    assert.throws(() => theme.parseHex('blue'), /Invalid hex colour/);
    assert.equal(theme.toHex({ r: 12, g: 106, b: 165 }), '#0C6AA5');
    assert.equal(theme.toHex({ r: 300, g: -4, b: 0.4 }), '#FF0000');
    assert.equal(theme.isHex('#abc'), true);
    assert.equal(theme.isHex('abc'), false);
    assert.equal(theme.isHex(12), false);
    assert.equal(theme.contrastRatio('#000000', '#FFFFFF'), 21);
    assert.equal(theme.contrastRatio('#FFFFFF', '#FFFFFF'), 1);
    assert.equal(theme.luminance('#FFFFFF'), 1);
    assert.equal(theme.luminance('#000000'), 0);
    assert.equal(theme.mix('#000000', '#FFFFFF', 0.5), '#808080');
    assert.equal(theme.mix('#102030', '#102030', 0.7), '#102030');
    assert.equal(theme.readableOn('#0C6AA5'), '#FFFFFF');
    assert.equal(theme.readableOn('#FFEE00'), '#1A1208');
    assert.equal(theme.ensureContrast('#000000', '#FFFFFF', 4.5), '#000000', 'a passing colour is returned unchanged');
    const lifted = theme.ensureContrast('#777788', '#FFFFFF', 4.5);
    assert(theme.contrastRatio(lifted, '#FFFFFF') >= 4.5 && lifted !== '#777788');
    assert.equal(theme.ensureContrast('#FFFF00', '#FFFFFF', 21), '#000000', 'the extreme is returned when nothing in between reaches the minimum');
    assert.equal(theme.shade('#808080', 0.1), '#9a9a9a');
    assert.equal(theme.shade('808080', -0.1), '#676767');
    assert.equal(theme.shade('#FFFFFF', 0.5), '#ffffff', 'channels are clamped');
    assert.equal(theme.shade('not-a-colour', 0.1), 'not-a-colour');
    return { blackOnWhite: theme.contrastRatio('#000000', '#FFFFFF'), midpoint: theme.mix('#000000', '#FFFFFF', 0.5) };
  });

  await run('brand-file-validation-and-resolution', async () => {
    const parsed = theme.parseBrand(brandJson());
    assert.equal(parsed.name, 'Fixture Suite');
    const refuse = (mutate, pattern) => {
      const input = brandJson(); mutate(input);
      assert.throws(() => theme.parseBrand(input), error => error instanceof theme.BrandError && pattern.test(error.message), String(pattern));
    };
    assert.throws(() => theme.parseBrand(null), theme.BrandError);
    assert.throws(() => theme.parseBrand([]), /must be a JSON object/);
    refuse(b => { delete b.name; }, /name: required string/);
    refuse(b => { b.name = '  '; }, /name: required string/);
    refuse(b => { delete b.colors; }, /colors: required object/);
    refuse(b => { delete b.colors.brand; }, /colors\.brand: required/);
    refuse(b => { b.colors.accent = '#000000'; }, /colors\.accent: names must match/);
    refuse(b => { b.colors['brand-ink'] = 'blue'; }, /expected #RRGGBB/);
    refuse(b => { b.dark = { brand: 'nope' }; }, /dark\.brand/);
    refuse(b => { b.tokens = { sepia: {} }; }, /tokens\.sepia: only light and dark/);
    refuse(b => { b.tokens = { light: { sparkle: '#000000' } }; }, /tokens\.light\.sparkle: unknown name/);
    refuse(b => { b.tokens = { light: { ink: 'red' } }; }, /expected a hex or rgb\(\) colour/);
    refuse(b => { b.wordmark = 7; }, /wordmark: expected a string/);
    refuse(b => { b.wordmarkFont = 'a; background: url(x)'; }, /plain font-family string/);
    const resolved = theme.resolveBrand(parsed);
    assert.deepEqual(resolved.palette.light, { brand: '#0C6AA5', 'brand-ink': '#0A5A8C', 'brand-action': '#0C6AA5' });
    assert(theme.contrastRatio(resolved.palette.dark.brand, resolved.effective.dark.plane) >= 4.5, 'the dark accent is lifted to 4.5:1 on the dark plane');
    assert.equal(resolved.palette.dark['brand-action'], resolved.palette.dark.brand);
    assert.equal(resolved.overrides.light['accent-ink'], '#FFFFFF');
    assert.deepEqual(Object.keys(resolved.effective.light), [...Object.keys(theme.SUITE_DEFAULTS.light), 'accent-ink']);
    const explicit = theme.resolveBrand(theme.parseBrand({ ...brandJson(), dark: { brand: '#66AADD', 'brand-soft': '#112233' }, tokens: { dark: { plane: '#000000' } } }));
    assert.equal(explicit.palette.dark.brand, '#66AADD');
    assert.equal(explicit.palette.dark['brand-soft'], '#112233');
    assert.equal(explicit.effective.dark.plane, '#000000');
    assert.equal(theme.SUITE_DEFAULTS.light.ink, '#231F1A');
    assert.equal(theme.SUITE_DEFAULTS.dark.ink, '#F2F0EC');
    assert.equal(theme.OVERRIDABLE_TOKENS.length, 17);
    assert.deepEqual(theme.brandFromAccent('Accent only', '#336699'), { name: 'Accent only', colors: { brand: '#336699' } });
    assert.throws(() => theme.brandFromAccent('x', 'teal'), /Invalid accent colour/);
    return { refusedBrandFiles: 14, overridableTokens: 17 };
  });

  await run('css-generation-is-deterministic-and-self-checking', async () => {
    const parsed = theme.parseBrand(brandJson());
    const css = theme.generateCss(parsed, { source: 'assets/brand/brand.json' });
    assert.equal(theme.generateCss(parsed, { source: 'assets/brand/brand.json' }), css);
    assert(css.startsWith('/* Generated by @rightkit/theme from assets/brand/brand.json; edit the brand file, then run `rightkit-theme generate`. */\n'));
    assert(css.includes(':root {\n  --rk-brand: #0C6AA5;\n  --rk-brand-dark: '));
    assert(css.includes('  --rk-wordmark-font: "Fixture Sans", sans-serif;\n'));
    assert(css.includes('@media (prefers-color-scheme: dark) {\n  :root:not([data-theme="light"]) {'));
    assert(css.includes(':root[data-theme="dark"] {'));
    assert.deepEqual(theme.checkGeneratedCss(css, parsed), []);
    assert.deepEqual(theme.checkGeneratedCss(css.replaceAll('--rk-brand-ink', '--rk-brand-gone'), parsed), ['missing --rk-brand-ink', 'missing --rk-brand-ink in dark block']);
    assert.deepEqual(theme.checkGeneratedCss('/* empty */', parsed).slice(0, 2), ['missing --rk-brand', 'missing --rk-brand-dark']);
    assert.deepEqual(theme.requiredVars(parsed), { light: ['--rk-brand', '--rk-brand-dark', '--rk-brand-mark', '--rk-brand-ink', '--rk-accent-ink'], dark: ['--rk-brand-mark', '--rk-brand-ink', '--rk-accent-ink'] });
    assert.deepEqual(theme.requiredVars(parsed, 'tailwind'), { light: ['--color-brand', '--color-brand-ink', '--color-brand-action'], dark: [] });
    const light = theme.modeVars(parsed, 'light');
    assert.equal(light['--rk-brand'], '#0C6AA5');
    assert.equal(light['--rk-brand-mark'], '#0C6AA5');
    assert.equal(light['--rk-accent-ink'], '#FFFFFF');
    assert.equal('--rk-brand-dark' in theme.modeVars(parsed, 'dark'), false, 'the accent pair is carried by the light block only');
    const tailwind = theme.generateCss(parsed, { format: 'tailwind', tailwindPrefix: '--tw-' });
    assert(tailwind.includes('@theme {\n  --tw-brand: #0C6AA5;\n  --tw-brand-ink: #0A5A8C;\n  --tw-brand-action: #0C6AA5;\n}\n'));
    assert(!tailwind.includes(':root'));
    const both = theme.generateCss(parsed, { format: 'both' });
    assert(both.includes(':root {') && both.includes('@theme {'));
    assert.equal(theme.tailwindThemeCss({ ink: '#101216', '--paper': '#F2F3F5' }), '@theme {\n  --color-ink: #101216;\n  --color-paper: #F2F3F5;\n}\n');
    assert.throws(() => theme.tailwindThemeCss({ ink: '#000' }, { prefix: 'color-' }), /Invalid Tailwind theme prefix/);
    assert.throws(() => theme.tailwindThemeCss({ 'in k': '#000' }), /Invalid Tailwind theme token name/);
    const presets = [{ id: 'paper', mode: 'light', tokens: { '--ink': '#101216', '--paper': '#F2F3F5' } }, { id: 'night', mode: 'dark', tokens: { '--ink': '#F2F3F5' } }];
    assert.equal(theme.presetCss(presets), ':root[data-palette="paper"] {\n  --ink: #101216;\n  --paper: #F2F3F5;\n}\n\n:root[data-palette="night"] {\n  --ink: #F2F3F5;\n}\n');
    assert(theme.presetCss(presets, { attribute: 'data-skin' }).includes(':root[data-skin="paper"]'));
    assert.throws(() => theme.presetCss(presets, { attribute: '9bad' }), /Invalid preset attribute/);
    assert.throws(() => theme.presetCss([{ id: 'a b', mode: 'light', tokens: {} }]), /Invalid preset id/);
    assert.throws(() => theme.presetCss([{ id: 'a', mode: 'light', tokens: { ink: '#000' } }]), /Invalid CSS custom property name/);
    assert.equal(theme.aliasCss({ '--ink': '--rk-ink' }), ':root {\n  --ink: var(--rk-ink);\n}\n');
    assert.equal(theme.aliasCss({ '--ink': '--rk-ink' }, '.app'), '.app {\n  --ink: var(--rk-ink);\n}\n');
    assert.throws(() => theme.aliasCss({ ink: '--rk-ink' }), /Invalid CSS custom property name/);
    return { cssBytes: css.length, formats: 3 };
  });

  await run('contrast-gate-and-hardcoded-colour-scan', async () => {
    const parsed = theme.parseBrand(brandJson());
    const good = theme.checkBrandContrast(parsed);
    assert.equal(good.pass, true);
    assert.deepEqual(good.failures, []);
    assert.equal(good.results.length, 28);
    assert.deepEqual(Object.keys(good.results[0]), ['id', 'mode', 'fg', 'bg', 'ratio', 'min', 'pass']);
    assert.equal(good.results[0].id, 'ink on plane');
    const weak = theme.parseBrand({ ...brandJson(), tokens: { light: { ink: '#EEEEEE' } } });
    const bad = theme.checkBrandContrast(weak);
    assert.equal(bad.pass, false);
    assert(bad.failures.some(f => f.id === 'ink on plane' && f.mode === 'light' && f.min === 4.5 && f.pass === false));
    assert.equal(theme.checkBrandContrast(weak, { text: 1, ancillary: 1 }).pass, true, 'thresholds are caller-controlled');
    assert.equal(theme.DEFAULT_APPROVED_CONTEXTS.length, 7);
    assert.deepEqual(theme.DEFAULT_APPROVED_VALUES, ['#fff', '#ffffff', '#000', '#000000', 'transparent']);
    const source = ['.a { color: #123456; }', ':root { --x: #123456; }', '.b { background: #fff; }', '.c { border: 1px solid rgb(1, 2, 3); }', '.d { color: #abcdef; } /* rightkit-theme: allow */'].join('\r\n');
    assert.deepEqual(theme.scanHardcodedColors('a.css', source), [
      { file: 'a.css', line: 1, value: '#123456', text: '.a { color: #123456; }' },
      { file: 'a.css', line: 4, value: 'rgb(1, 2, 3)', text: '.c { border: 1px solid rgb(1, 2, 3); }' },
    ]);
    assert.deepEqual(theme.scanHardcodedColors('a.css', '.a { color: #123456; }', { approvedValues: ['#123456'] }), []);
    assert.deepEqual(theme.scanHardcodedColors('a.css', '.a { color: #123456; }', { approvedContexts: [/color:/] }), []);
    return { contrastChecks: good.results.length, failuresReported: bad.failures.length, scannedFindings: 2 };
  });

  await run('token-helpers-apply-and-undo-on-fake-root', async () => {
    const el = fakeRoot();
    el.props.set('--ink', 'previous-ink');
    const undo = theme.applyTokens({ '--ink': '#101216', '--paper': '#F2F3F5', '--skipped': undefined }, el);
    assert.deepEqual([...el.props], [['--ink', '#101216'], ['--paper', '#F2F3F5']]);
    undo();
    assert.deepEqual([...el.props], [['--ink', 'previous-ink']], 'undo restores the previous inline value and removes the rest');
    assert.throws(() => theme.applyTokens({ ink: '#000' }, el), /Invalid CSS custom property name/);
    assert.equal(typeof theme.applyTokens({ '--a': '1' }, null), 'function');
    assert.doesNotThrow(() => theme.applyTokens({ '--a': '1' }, null)());
    assert.doesNotThrow(() => theme.assertCustomProperty('--rk-ink'));
    assert.throws(() => theme.assertCustomProperty('rk-ink'), /Invalid CSS custom property name/);
    assert.deepEqual(theme.aliasTokens({ '--focus': '--rk-focus' }), { '--focus': 'var(--rk-focus)' });
    assert.equal(Object.keys(theme.SUITE_ALIASES).length, 40);
    assert.equal(theme.SUITE_ALIASES['--ink'], '--rk-ink');
    assert.equal(theme.SUITE_ALIASES['--wordmark'], '--rk-wordmark-font');
    const preset = { id: 'paper', mode: 'light', tokens: { '--ink': '#101216', '--copper': '#C07040' }, accents: { blue: { '--copper': '#0C6AA5' } } };
    assert.deepEqual(theme.presetTokens(preset), { '--ink': '#101216', '--copper': '#C07040' });
    assert.deepEqual(theme.presetTokens(preset, 'blue'), { '--ink': '#101216', '--copper': '#0C6AA5' });
    assert.deepEqual(theme.presetTokens(preset, 'missing'), { '--ink': '#101216', '--copper': '#C07040' });
    assert.deepEqual(theme.fontTokens({ mono: 'Menlo, monospace', body: 'Inter', wordmark: '"Fixture Sans"' }), { '--rk-font-mono': 'Menlo, monospace', '--rk-font-body': 'Inter', '--rk-wordmark-font': '"Fixture Sans"' });
    assert.deepEqual(theme.fontTokens({}), {});
    const parsed = theme.parseBrand(brandJson());
    const branded = fakeRoot();
    const remove = theme.applyBrand(parsed, 'dark', branded);
    assert.equal(branded.props.get('--rk-brand'), '#0C6AA5', 'the accent pair stays complete whichever mode is applied');
    assert.equal(branded.props.get('--rk-brand-dark'), theme.resolveBrand(parsed).palette.dark['brand-action']);
    assert.equal(branded.props.get('--rk-accent-ink'), '#1A1208');
    assert.equal(branded.props.get('--rk-wordmark-font'), '"Fixture Sans", sans-serif');
    assert.equal(branded.props.has('--ink'), false, 'suite aliases are off by default');
    remove();
    assert.equal(branded.props.size, 0);
    const aliased = fakeRoot();
    theme.applyBrand(parsed, 'light', aliased, { applySuiteAliases: true });
    assert.equal(aliased.props.get('--ink'), 'var(--rk-ink)');
    assert.equal(typeof theme.applyBrand(parsed, 'light'), 'function', 'no document: a no-op unsubscribe');
    return { aliases: 40, appliedProperties: aliased.props.size };
  });

  await run('runtime-boot-script-theme-attribute-and-media-listener', async () => {
    const script = theme.themeBootScript('fixture.theme');
    assert(script.startsWith('(function(){try{var k="fixture.theme"'));
    assert.equal(bootAttribute(script, { stored: { 'fixture.theme': 'dark' } }), 'dark');
    assert.equal(bootAttribute(script, { stored: { 'fixture.theme': 'light' }, prefersDark: true }), 'light');
    assert.equal(bootAttribute(script, { prefersDark: true }), 'dark', 'system resolves through matchMedia');
    assert.equal(bootAttribute(script, { prefersDark: false }), 'light');
    assert.equal(bootAttribute(script, { stored: { 'fixture.theme': 'surprise' } }), 'light', 'an unknown stored value falls back to the default choice');
    assert.equal(bootAttribute(script, { throwStorage: true, prefersDark: true }), 'dark', 'a throwing storage never throws out of the script');
    assert.equal(bootAttribute(theme.themeBootScript('k', { defaultChoice: 'dark' })), 'dark');
    assert.equal(bootAttribute(theme.themeBootScript('k', { darkRemovesAttribute: true }), { stored: { k: 'dark' } }), null);
    assert.equal(bootAttribute(theme.themeBootScript('k', { darkRemovesAttribute: true }), { stored: { k: 'light' } }), 'light');
    assert.equal(bootAttribute(theme.themeBootScript('k', { alwaysPaintDataTheme: false }), { prefersDark: true }), null, 'system paints nothing when alwaysPaintDataTheme is false');
    assert(!theme.themeBootScript('</script><b>').includes('</script>'), 'the storage key cannot close the script tag');
    assert(theme.themeBootScript('a b').includes('\\u2028'));
    assert.equal(theme.themeAttributeValue('dark'), 'dark');
    assert.equal(theme.themeAttributeValue('dark', { darkRemovesAttribute: true }), null);
    assert.equal(theme.themeAttributeValue('light', { darkRemovesAttribute: true }), 'light');
    const el = fakeAttrRoot();
    theme.applyThemeAttribute('dark', el);
    assert.equal(el.attrs.get('data-theme'), 'dark');
    theme.applyThemeAttribute('dark', el, { darkRemovesAttribute: true });
    assert.equal(el.attrs.has('data-theme'), false);
    assert.doesNotThrow(() => theme.applyThemeAttribute('light', null));
    assert.equal(theme.prefersDark(), false, 'no window: false');
    assert.equal(typeof theme.onPrefersDarkChange(() => {}), 'function');
    const handlers = new Set();
    const list = { matches: true, addEventListener: (type, handler) => { assert.equal(type, 'change'); handlers.add(handler); }, removeEventListener: (type, handler) => { handlers.delete(handler); } };
    const legacy = { matches: false, listeners: new Set(), addListener(handler) { this.listeners.add(handler); }, removeListener(handler) { this.listeners.delete(handler); } };
    globalThis.window = { matchMedia: query => { assert.equal(query, '(prefers-color-scheme: dark)'); return list; } };
    try {
      assert.equal(theme.prefersDark(), true);
      const seen = [];
      const stop = theme.onPrefersDarkChange(dark => seen.push(dark));
      assert.equal(handlers.size, 1);
      for (const handler of handlers) handler({ matches: false });
      assert.deepEqual(seen, [false]);
      stop(); stop();
      assert.equal(handlers.size, 0);
      globalThis.window = { matchMedia: () => legacy };
      const stopLegacy = theme.onPrefersDarkChange(() => {});
      assert.equal(legacy.listeners.size, 1);
      stopLegacy();
      assert.equal(legacy.listeners.size, 0);
      globalThis.window = { matchMedia: () => { throw new Error('blocked'); } };
      assert.equal(theme.prefersDark(), false, 'a throwing matchMedia reads as light');
    } finally { delete globalThis.window; }
    return { bootCases: 10, listenerPaths: 2 };
  });

  await run('node-entry-files-and-rightkit-theme-bin', async () => {
    scratch = mkdtempSync(path.join(os.tmpdir(), 'rk-theme-consumer-'));
    const brandPath = path.join(scratch, 'assets', 'brand', 'brand.json');
    mkdirSync(path.dirname(brandPath), { recursive: true });
    writeFileSync(brandPath, `${JSON.stringify(brandJson(), null, 2)}\n`);
    writeFileSync(path.join(path.dirname(brandPath), 'logo.svg'), '<svg xmlns="http://www.w3.org/2000/svg"><path fill="#0c6aa5" d="M0 0h1v1z"/></svg>\n');
    assert.equal(node.readBrand(brandPath).name, 'Fixture Suite');
    writeFileSync(path.join(scratch, 'broken.json'), '{"name":"x"}');
    assert.throws(() => node.readBrand(path.join(scratch, 'broken.json')), theme.BrandError);
    const checked = node.checkBrandFile(brandPath);
    assert.equal(checked.report.pass, true);
    assert.deepEqual(checked.logo, []);
    assert.deepEqual(node.checkLogo(brandPath, { ...checked.brand, logo: { master: 'missing.svg' } }), ['logo master not found: missing.svg']);
    writeFileSync(path.join(path.dirname(brandPath), 'other.svg'), '<svg><path fill="#ffffff"/></svg>');
    assert.deepEqual(node.checkLogo(brandPath, { ...checked.brand, logo: { master: 'other.svg' } }), ['logo colour differs from the approved brand colour']);
    assert.deepEqual(node.checkLogo(brandPath, { ...checked.brand, logo: undefined }), []);

    const out = path.join(scratch, 'out', 'theme.css');
    const check = node.generateFile(brandPath, out, { check: true });
    assert.equal(check.changed, true);
    assert.throws(() => readFileSync(out), { code: 'ENOENT' }, 'check mode never writes');
    const first = node.generateFile(brandPath, out);
    assert.equal(first.changed, true);
    assert.equal(readFileSync(out, 'utf8'), first.css);
    assert(first.css.includes('from assets/brand/brand.json;'));
    assert.equal(node.generateFile(brandPath, out).changed, false, 'unchanged bytes are not rewritten');
    assert.deepEqual(theme.checkGeneratedCss(first.css, checked.brand), []);
    writeFileSync(path.join(scratch, 'styles.css'), '.a { color: #123456; }\n:root { --x: #123456; }\n');
    assert.deepEqual(node.scanFiles([path.join(scratch, 'styles.css')]).map(f => [path.basename(f.file), f.line, f.value]), [['styles.css', 1, '#123456']]);

    let invocations = 0;
    const cli = (...args) => {
      invocations += 1;
      const child = spawnSync(process.execPath, [binPath, ...args], { cwd: scratch, encoding: 'utf8', timeout: 30_000, env: { ...process.env, NODE_OPTIONS: '' } });
      assert.notEqual(child.status, null, `rightkit-theme did not exit: ${child.error}`);
      const lines = child.stdout.trim().split('\n');
      assert.equal(lines.length, 1, 'machine output is exactly one JSON line');
      return { status: child.status, json: JSON.parse(lines[0]) };
    };
    assert.deepEqual(cli(), { status: 1, json: { event: 'usage', ok: false, usage: 'rightkit-theme generate|contrast|tokens ...' } });
    const cliOut = path.join(scratch, 'cli.css');
    const generated = cli('generate', brandPath, '--out', cliOut);
    assert.deepEqual(generated, { status: 0, json: { event: 'brand_theme_synced', ok: true, brand: 'FS', out: cliOut, changed: true, contrastPass: true } });
    assert.equal(readFileSync(cliOut, 'utf8'), node.generateFile(brandPath, cliOut).css);
    assert.equal(cli('generate', brandPath, '--out', cliOut, '--check').json.changed, false);
    writeFileSync(cliOut, '/* hand edited */\n');
    assert.deepEqual(cli('generate', brandPath, '--out', cliOut, '--check'), { status: 1, json: { event: 'brand_theme_drift', ok: false, out: cliOut, hint: 'run rightkit-theme generate' } });
    assert.equal(cli('generate', brandPath, '--out', path.join(scratch, 'tw.css'), '--format', 'tailwind').json.event, 'brand_theme_synced');
    assert(readFileSync(path.join(scratch, 'tw.css'), 'utf8').includes('@theme {'));
    assert.deepEqual(cli('contrast', brandPath), { status: 0, json: { event: 'brand_contrast_ok', ok: true, brand: 'FS', checks: 28 } });
    const weakPath = path.join(scratch, 'weak.json');
    writeFileSync(weakPath, JSON.stringify({ ...brandJson(), logo: undefined, tokens: { light: { ink: '#EEEEEE' } } }));
    const weak = cli('contrast', weakPath);
    assert.equal(weak.status, 1);
    assert.equal(weak.json.event, 'brand_contrast_failed');
    assert(weak.json.failures.some(f => f.id === 'ink on plane'));
    assert.equal(cli('generate', weakPath, '--out', path.join(scratch, 'weak.css')).json.event, 'brand_contrast_failed');
    assert.equal(cli('generate', weakPath, '--out', path.join(scratch, 'weak.css'), '--allow-contrast-failures').status, 0);
    const tokens = cli('tokens', path.join(scratch, 'styles.css'));
    assert.equal(tokens.status, 1);
    assert.deepEqual([tokens.json.event, tokens.json.count], ['hardcoded_colors', 1]);
    writeFileSync(path.join(scratch, 'clean.css'), ':root { --x: #123456; }\n');
    assert.deepEqual(cli('tokens', path.join(scratch, 'clean.css')), { status: 0, json: { event: 'tokens_ok', ok: true, files: 1 } });
    assert.equal(cli('generate', path.join(scratch, 'missing.json'), '--out', cliOut).json.event, 'error');
    return { cliInvocations: invocations, generatedBytes: first.css.length };
  });

  assert.equal(result.scenarios.length, 8);
  result.ok = true;
} catch (error) {
  result.errors.push(failure(error));
  process.exitCode = 1;
} finally {
  if (scratch) rmSync(scratch, { recursive: true, force: true });
}
process.stdout.write(`${JSON.stringify(result)}\n`);
