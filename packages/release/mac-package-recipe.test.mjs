import assert from 'node:assert/strict';
import path from 'node:path';
import test from 'node:test';
import {
  globToRegExp, MacPackageRecipeError, runMacPackageRecipe, validateMacPackageConfig, validateMacPackageTarget,
} from './mac-package-recipe.mjs';

const IDENTITY = 'Developer ID Application: Orthic Labs (TEAM123456)';
const appRoot = path.resolve('/work/demo');
const app = path.join(appRoot, 'build', 'Demo.app');
const dmg = path.join(appRoot, 'build', 'Demo_1.0.0_universal.dmg');
const updater = `${app}.tar.gz`;
const frameworkBin = path.join(app, 'Contents', 'Frameworks', 'Lib.framework', 'Versions', 'A', 'Lib');
const framework = path.join(app, 'Contents', 'Frameworks', 'Lib.framework');
const helper = path.join(app, 'Contents', 'Helpers', 'tool');
const entitlements = path.join(appRoot, 'entitlements.plist');
const background = path.join(appRoot, 'dmg-bg.png');

const macPackage = (over = {}) => ({
  universal: true,
  dmg: { name: 'Demo', background: 'dmg-bg.png', windowSize: [660, 400], iconPositions: { app: [158, 231], applications: [492, 231] } },
  notarize: true,
  updater: true,
  entitlements: 'entitlements.plist',
  ...over,
});
const targetFor = (over = {}) => ({
  macPackage: macPackage(),
  notarize: { app: 'build/Demo.app', file: 'build/Demo_1.0.0_universal.dmg' },
  ...over,
});

function harness({ target = targetFor(), failAt, thin = [], accept = 'Accepted', globHits = [], nestedFound = [framework, frameworkBin, helper] } = {}) {
  const events = [];
  const existing = new Set([app, path.join(app, 'Contents', 'Info.plist'), entitlements, background]);
  const calls = { sign: [], dmg: null, notarize: null, run: null, signUpdater: null };
  const fail = (step) => { if (failAt === step) throw new Error(`${step} exploded`); };
  const deps = {
    platform: 'darwin',
    env: {},
    resolveIdentity: ({ env }) => { assert.deepEqual(env, {}); events.push('identity'); return IDENTITY; },
    mainName: () => 'Demo',
    discover: () => nestedFound,
    listFiles: () => globHits,
    evidence: () => ({ sha256: '0', sizeBytes: 1 }),
    hashFile: () => '0',
    fs: {
      existsSync: (file) => existing.has(file),
      statSync: (file) => ({ isFile: () => file !== framework }),
      rmSync: (file) => { events.push(`rm:${path.basename(file)}`); existing.delete(file); },
    },
    inspect: (cmd, args) => {
      if (cmd === 'lipo') {
        const file = args.at(-1);
        events.push(`lipo:${path.basename(file)}`);
        if (file === helper) throw new Error('not a Mach-O');
        return thin.includes(file) ? 'arm64\n' : 'x86_64 arm64\n';
      }
      return args.includes('--verify') ? 'valid on disk\nsatisfies its designated requirement\n' : `Authority=${IDENTITY}\nTimestamp=Oct 17 2026\n`;
    },
    signOne: (args) => {
      const file = args.at(-1);
      events.push(`sign:${path.basename(file)}`);
      calls.sign.push(args);
      if (failAt === `sign:${path.basename(file)}`) return { status: 1, stderr: 'boom' };
      return { status: 0, stdout: '', stderr: '' };
    },
    buildDmg: (options) => { events.push('dmg'); calls.dmg = options; fail('dmg'); existing.add(dmg); return { out: dmg }; },
    notarize: (options) => {
      events.push('notarize'); calls.notarize = options; fail('notarize');
      return { schema: 1, notarization: { status: accept, id: 'sub-1' }, stapling: [] };
    },
    run: (cmd, args, options) => { events.push(`run:${cmd}`); calls.run = { cmd, args, options }; fail('tar'); existing.add(updater); return { status: 0, stdout: '', stderr: '' }; },
    signUpdater: (files, options) => { events.push('updater-sign'); calls.signUpdater = { files, options }; fail('updater-sign'); existing.add(`${updater}.sig`); },
  };
  return { events, deps, calls, existing, run: () => runMacPackageRecipe({ appRoot, config: { app: 'demo' }, target, deps }) };
}

test('validateMacPackageConfig normalizes a full config and defaults universal/nested', () => {
  const full = validateMacPackageConfig(macPackage({ nested: ['Contents/Frameworks/**/*.dylib', 'Contents/Helpers/*'] }));
  assert.equal(full.universal, true);
  assert.deepEqual(full.nested, ['Contents/Frameworks/**/*.dylib', 'Contents/Helpers/*']);
  assert.deepEqual(full.dmg.windowSize, [660, 400]);
  const minimal = validateMacPackageConfig({ dmg: { name: 'Demo' }, notarize: false, updater: false });
  assert.deepEqual(minimal, { universal: false, dmg: { name: 'Demo' }, notarize: false, updater: false, nested: [] });
});

test('validateMacPackageConfig fails closed with MAC_PACKAGE_CONFIG_INVALID', () => {
  const bad = (value, pattern) => assert.throws(() => validateMacPackageConfig(value), (error) => {
    assert.ok(error instanceof MacPackageRecipeError);
    assert.equal(error.name, 'MacPackageRecipeError');
    assert.equal(error.code, 'MAC_PACKAGE_CONFIG_INVALID');
    assert.match(error.message, pattern);
    return true;
  });
  bad(undefined, /must be an object/);
  bad([], /must be an object/);
  bad({ ...macPackage(), surprise: 1 }, /unknown key\(s\): surprise/);
  bad(macPackage({ notarize: undefined }), /notarize is required/);
  bad(macPackage({ updater: 'yes' }), /updater is required/);
  bad(macPackage({ universal: 1 }), /universal/);
  bad(macPackage({ dmg: undefined }), /dmg is required/);
  bad(macPackage({ dmg: { name: 'a/b' } }), /dmg\.name/);
  bad(macPackage({ dmg: { name: '' } }), /dmg\.name/);
  bad(macPackage({ dmg: { name: 'Demo', extra: true } }), /dmg has unknown key/);
  bad(macPackage({ dmg: { name: 'Demo', background: 'bg.jpg' } }), /PNG or TIFF/);
  bad(macPackage({ dmg: { name: 'Demo', windowSize: [0, 400] } }), /windowSize/);
  bad(macPackage({ dmg: { name: 'Demo', windowSize: [660] } }), /windowSize/);
  bad(macPackage({ dmg: { name: 'Demo', iconPositions: { app: [1.5, 2] } } }), /iconPositions\.app/);
  bad(macPackage({ dmg: { name: 'Demo', iconPositions: { docs: [1, 2] } } }), /iconPositions has unknown key/);
  bad(macPackage({ nested: [] }), /non-empty array/);
  bad(macPackage({ nested: ['/abs/path'] }), /bundle-relative/);
  bad(macPackage({ nested: ['Contents\\Frameworks'] }), /bundle-relative/);
  bad(macPackage({ nested: ['../escape'] }), /'\.\.'/);
  bad(macPackage({ nested: ['Contents/{a,b}'] }), /wildcards/);
  bad(macPackage({ nested: ['Contents/a**b'] }), /whole path segment/);
  bad(macPackage({ nested: ['Contents/*', 'Contents/*'] }), /unique/);
  bad(macPackage({ entitlements: '' }), /entitlements/);
});

test('validateMacPackageTarget refuses hook scripts, a legacy dmg block and missing app/file paths', () => {
  assert.deepEqual(validateMacPackageTarget(targetFor()).app, 'build/Demo.app');
  const bad = (target, pattern) => assert.throws(() => validateMacPackageTarget(target), (error) => error.code === 'MAC_PACKAGE_CONFIG_INVALID' && pattern.test(error.message));
  bad(targetFor({ prePackage: { cmd: 'node' } }), /prePackage or package/);
  bad(targetFor({ package: { cmd: 'node' } }), /prePackage or package/);
  bad(targetFor({ dmg: { spec: 'x.json' } }), /targets\.mac\.dmg/);
  bad(targetFor({ notarize: { file: 'a.dmg' } }), /notarize\.app/);
  bad(targetFor({ notarize: { app: 'a.app' } }), /notarize\.file/);
  bad(targetFor({ notarize: { app: 'a.app', file: 'a.zip' } }), /notarize\.file/);
});

test('globToRegExp: * stays in a segment, ** spans segments, ? is one character', () => {
  const dylib = globToRegExp('Contents/Frameworks/**/*.dylib');
  assert.ok(dylib.test('Contents/Frameworks/libx.dylib'));
  assert.ok(dylib.test('Contents/Frameworks/a/b/libx.dylib'));
  assert.ok(!dylib.test('Contents/Frameworks/a/b/libx.so'));
  assert.ok(!dylib.test('Contents/Other/libx.dylib'));
  assert.ok(globToRegExp('Contents/Helpers/*').test('Contents/Helpers/tool'));
  assert.ok(!globToRegExp('Contents/Helpers/*').test('Contents/Helpers/a/tool'));
  assert.ok(globToRegExp('Contents/Res/?.bin').test('Contents/Res/a.bin'));
  assert.ok(!globToRegExp('Contents/Res/?.bin').test('Contents/Res/ab.bin'));
  assert.ok(!globToRegExp('Contents/a.b').test('Contents/aXb'));
  assert.ok(globToRegExp('Contents/Frameworks/**').test('Contents/Frameworks/x/y'));
});

test('full run: lipo check, nested (deepest first), app, DMG, DMG sign, notarize+staple, then updater', () => {
  const h = harness();
  const result = h.run();
  assert.deepEqual(result.steps, ['universal', 'sign-nested', 'sign-app', 'dmg', 'sign-dmg', 'notarize', 'updater']);
  const order = h.events.filter((event) => !event.startsWith('lipo:') && !event.startsWith('rm:'));
  assert.deepEqual(order, ['identity', 'sign:Lib', 'sign:Lib.framework', 'sign:tool', 'sign:Demo.app', 'dmg', 'sign:Demo_1.0.0_universal.dmg', 'notarize', 'run:tar', 'updater-sign'].map((event) => event));
  assert.equal(result.app, app);
  assert.equal(result.dmg, dmg);
  assert.equal(result.updater, updater);
  assert.equal(result.receipt.notarization.status, 'Accepted');
});

test('nested code is signed deepest first and without entitlements; the app gets runtime and entitlements; the DMG gets neither runtime nor entitlements', () => {
  const h = harness();
  h.run();
  const argsFor = (name) => h.calls.sign.find((args) => path.basename(args.at(-1)) === name);
  const nestedNames = h.calls.sign.slice(0, 3).map((args) => path.basename(args.at(-1)));
  assert.deepEqual(nestedNames, ['Lib', 'Lib.framework', 'tool'].sort((a, b) => nestedNames.indexOf(a) - nestedNames.indexOf(b)));
  assert.ok(nestedNames.indexOf('Lib') < nestedNames.indexOf('Lib.framework'), 'the binary inside a framework is signed before the framework');
  for (const name of ['Lib', 'Lib.framework', 'tool']) {
    const args = argsFor(name);
    assert.ok(!args.includes('--entitlements'), `${name} must not carry entitlements`);
    assert.deepEqual(args.slice(args.indexOf('--options'), args.indexOf('--options') + 2), ['--options', 'runtime']);
    assert.ok(args.includes('--timestamp'));
  }
  const appArgs = argsFor('Demo.app');
  assert.deepEqual(appArgs.slice(appArgs.indexOf('--options'), appArgs.indexOf('--options') + 2), ['--options', 'runtime']);
  assert.equal(appArgs[appArgs.indexOf('--entitlements') + 1], entitlements);
  assert.equal(appArgs[appArgs.indexOf('--sign') + 1], IDENTITY);
  const dmgArgs = argsFor('Demo_1.0.0_universal.dmg');
  assert.deepEqual(dmgArgs, ['--force', '--timestamp', '--sign', IDENTITY, dmg]);
});

test('DMG options, notarize options and updater commands carry the configured values', () => {
  const h = harness();
  h.run();
  assert.deepEqual(h.calls.dmg, { app, out: dmg, volname: 'Demo', background, window: [660, 400], appPosition: [158, 231], applicationsPosition: [492, 231] });
  assert.equal(h.calls.notarize.archive, dmg);
  assert.equal(h.calls.notarize.app, app);
  assert.ok(h.calls.notarize.receiptPath.endsWith('demo-mac-notarization.json'));
  assert.deepEqual(h.calls.notarize.env, {});
  assert.equal(h.calls.run.cmd, 'tar');
  assert.deepEqual(h.calls.run.args, ['-czf', updater, '-C', path.dirname(app), path.join('Demo.app', 'Contents')]);
  assert.equal(h.calls.run.options.env.COPYFILE_DISABLE, '1');
  assert.deepEqual(h.calls.signUpdater.files, [updater]);
  assert.deepEqual(h.calls.signUpdater.options.env, {});
});

test('notarize:false and updater:false skip those steps and never call them', () => {
  const target = targetFor({ macPackage: macPackage({ notarize: false, updater: false, universal: false }) });
  const h = harness({ target });
  const result = h.run();
  assert.deepEqual(result.steps, ['sign-nested', 'sign-app', 'dmg', 'sign-dmg']);
  assert.equal(h.calls.notarize, null);
  assert.equal(h.calls.run, null);
  assert.equal(result.updater, null);
  assert.equal(result.receipt, null);
  assert.ok(!h.events.some((event) => event.startsWith('lipo:')));
});

test('an app with no nested code skips the nested step', () => {
  const h = harness({ nestedFound: [] });
  assert.deepEqual(h.run().steps, ['universal', 'sign-app', 'dmg', 'sign-dmg', 'notarize', 'updater']);
});

test('nested globs add files, de-duplicate against discovery and still sign deepest first', () => {
  const dylib = path.join(app, 'Contents', 'Frameworks', 'extra', 'deep', 'libz.dylib');
  const target = targetFor({ macPackage: macPackage({ nested: ['Contents/Frameworks/**/*.dylib'] }) });
  const h = harness({ target, globHits: ['Contents/Frameworks/extra/deep/libz.dylib', 'Contents/Helpers/tool'], nestedFound: [framework, frameworkBin, helper] });
  h.deps.listFiles = () => ['Contents/Frameworks/extra/deep/libz.dylib'];
  h.run();
  const signed = h.events.filter((event) => event.startsWith('sign:'));
  assert.deepEqual(signed.filter((event) => event !== 'sign:Demo.app' && !event.endsWith('.dmg')).sort(), ['sign:Lib', 'sign:Lib.framework', 'sign:libz.dylib', 'sign:tool']);
  assert.ok(signed.indexOf('sign:libz.dylib') < signed.indexOf('sign:Lib.framework'), 'deeper path first');
  assert.equal(signed.filter((event) => event === 'sign:tool').length, 1);
  assert.equal(h.calls.sign.every((args) => !args.includes('--entitlements') || path.basename(args.at(-1)) === 'Demo.app'), true);
});

test('a nested glob that matches nothing fails before any signing', () => {
  const target = targetFor({ macPackage: macPackage({ nested: ['Contents/Nope/*'] }) });
  const h = harness({ target });
  h.deps.listFiles = () => ['Contents/Other/x'];
  assert.throws(() => h.run(), (error) => error.code === 'MAC_PACKAGE_INPUT_MISSING' && /matched nothing/.test(error.message));
  assert.ok(!h.events.some((event) => event.startsWith('sign:')));
  assert.ok(!h.events.includes('identity'));
});

test('missing app, entitlements or background are reported before any effect', () => {
  for (const [file, pattern] of [[app, /app bundle not found/], [entitlements, /entitlements file not found/], [background, /background file not found/]]) {
    const h = harness();
    h.existing.delete(file);
    assert.throws(() => h.run(), (error) => error.code === 'MAC_PACKAGE_INPUT_MISSING' && pattern.test(error.message));
    assert.deepEqual(h.events, []);
  }
});

test('a thin Mach-O fails the universal check before anything is signed', () => {
  const h = harness({ thin: [frameworkBin] });
  assert.throws(() => h.run(), (error) => error.code === 'MAC_PACKAGE_UNIVERSAL_FAILED' && /not universal.*missing: x86_64/.test(error.message));
  assert.ok(!h.events.some((event) => event.startsWith('sign:') || event === 'dmg'));
  const main = harness({ thin: [path.join(app, 'Contents', 'MacOS', 'Demo')] });
  assert.throws(() => main.run(), (error) => error.code === 'MAC_PACKAGE_UNIVERSAL_FAILED');
});

test('an unreadable main executable fails the universal check instead of being skipped', () => {
  const h = harness();
  const inspect = h.deps.inspect;
  h.deps.inspect = (cmd, args) => { if (cmd === 'lipo' && args.at(-1).endsWith(path.join('MacOS', 'Demo'))) throw new Error('lipo: cannot read'); return inspect(cmd, args); };
  assert.throws(() => h.run(), (error) => error.code === 'MAC_PACKAGE_UNIVERSAL_FAILED');
  assert.ok(!h.events.some((event) => event.startsWith('sign:')));
});

test('a nested signing failure stops the run: no app signing, no DMG, no notarization', () => {
  const h = harness({ failAt: 'sign:Lib.framework' });
  assert.throws(() => h.run(), (error) => error.code === 'MAC_PACKAGE_SIGN_FAILED' && /codesign failed/.test(error.message));
  assert.ok(!h.events.includes('sign:Demo.app'));
  assert.ok(!h.events.includes('dmg'));
  assert.ok(!h.events.includes('notarize'));
});

test('an app signing failure stops before the DMG is built', () => {
  const h = harness({ failAt: 'sign:Demo.app' });
  assert.throws(() => h.run(), (error) => error.code === 'MAC_PACKAGE_SIGN_FAILED');
  assert.ok(!h.events.includes('dmg'));
});

test('an identity lookup failure is a typed signing error and nothing is signed', () => {
  const h = harness();
  h.deps.resolveIdentity = () => { throw new Error('no Developer ID Application signing identity found in Keychain'); };
  assert.throws(() => h.run(), (error) => error.code === 'MAC_PACKAGE_SIGN_FAILED' && /identity lookup/.test(error.message));
  assert.ok(!h.events.some((event) => event.startsWith('sign:')));
});

test('a DMG build failure removes the DMG and never notarizes', () => {
  const h = harness({ failAt: 'dmg' });
  assert.throws(() => h.run(), (error) => error.code === 'MAC_PACKAGE_DMG_FAILED' && /dmg exploded/.test(error.message) && error.cause instanceof Error);
  assert.ok(!h.events.includes('notarize'));
  assert.ok(!h.existing.has(dmg));
});

test('a DMG signing failure removes the unsigned DMG and never notarizes', () => {
  const h = harness({ failAt: 'sign:Demo_1.0.0_universal.dmg' });
  assert.throws(() => h.run(), (error) => error.code === 'MAC_PACKAGE_SIGN_FAILED');
  assert.ok(!h.events.includes('notarize'));
  assert.ok(!h.existing.has(dmg));
});

test('a notarization failure removes the DMG and never builds the updater', () => {
  const h = harness({ failAt: 'notarize' });
  assert.throws(() => h.run(), (error) => error.code === 'MAC_PACKAGE_NOTARIZE_FAILED' && /notarize exploded/.test(error.message));
  assert.ok(!h.events.includes('run:tar'));
  assert.ok(!h.events.includes('updater-sign'));
  assert.ok(!h.existing.has(dmg));
});

test('a receipt that is not Accepted fails the run', () => {
  const h = harness({ accept: 'Invalid' });
  assert.throws(() => h.run(), (error) => error.code === 'MAC_PACKAGE_NOTARIZE_FAILED' && /Accepted/.test(error.message));
  assert.ok(!h.existing.has(dmg));
});

test('an updater tar failure removes both the DMG and the half-built updater', () => {
  const h = harness({ failAt: 'tar' });
  assert.throws(() => h.run(), (error) => error.code === 'MAC_PACKAGE_UPDATER_FAILED');
  assert.ok(!h.events.includes('updater-sign'));
  assert.ok(!h.existing.has(dmg));
});

test('an updater signing failure removes the DMG and the tarball', () => {
  const h = harness({ failAt: 'updater-sign' });
  assert.throws(() => h.run(), (error) => error.code === 'MAC_PACKAGE_UPDATER_FAILED' && /updater signing/.test(error.message));
  assert.ok(!h.existing.has(dmg));
  assert.ok(!h.existing.has(updater));
});

test('an updater signer that leaves no .sig fails closed', () => {
  const h = harness();
  h.deps.signUpdater = () => {};
  assert.throws(() => h.run(), (error) => error.code === 'MAC_PACKAGE_INPUT_MISSING' && /updater artifact not found/.test(error.message));
  assert.ok(!h.existing.has(dmg));
});

test('a non-macOS host is refused before any effect', () => {
  const h = harness();
  h.deps.platform = 'win32';
  assert.throws(() => h.run(), (error) => error.code === 'MAC_PACKAGE_HOST_UNSUPPORTED');
  assert.deepEqual(h.events, []);
});

test('invalid config is refused before the host check or any effect', () => {
  const h = harness({ target: targetFor({ macPackage: { ...macPackage(), extra: true } }) });
  assert.throws(() => h.run(), (error) => error.code === 'MAC_PACKAGE_CONFIG_INVALID');
  assert.deepEqual(h.events, []);
});

test('a relative appRoot is refused', () => {
  assert.throws(() => runMacPackageRecipe({ appRoot: 'relative/app', target: targetFor(), deps: { platform: 'darwin', env: {} } }), (error) => error.code === 'MAC_PACKAGE_CONFIG_INVALID');
});
