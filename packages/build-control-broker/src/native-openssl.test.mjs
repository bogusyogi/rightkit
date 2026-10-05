import assert from 'node:assert/strict';
import { mkdir, mkdtemp, readFile, rm, stat, utimes, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { NativeOpenSslCache, OPENSSL_OPT_OUT_ENV, deriveKey, detectOpenSsl, findOverrides, injectionEnvironment, parseMetadataSelection, renderHelperCrate } from './native-openssl.mjs';
import { ThinState } from './thin-state.mjs';

const TRIPLE = 'x86_64-pc-windows-msvc';
const lock = () => 'version = 4\n\n[[package]]\nname = "cc"\nversion = "1.4.4"\nchecksum = "aa"\n\n[[package]]\nname = "openssl-src"\nversion = "300.6.1+3.6.3"\nchecksum = "bb"\n\n[[package]]\nname = "openssl-sys"\nversion = "0.9.117"\nchecksum = "cc"\n';
const toolchain = { msvc: { toolset: '14.51', banner: '19.51 x64' }, sdk: '10.0.26100.0', perl: { path: 'p', version: '5.42' }, nasm: { present: true, version: '2.16' } };
const base = { opensslSrc: { version: '300.6.1+3.6.3', checksum: 'bb' }, features: [], targetTriple: TRIPLE, toolchain, cc: '1.4.4' };
const metadataJson = JSON.stringify({ packages: [{ id: 'a', name: 'openssl-src', version: '300.6.1+3.6.3' }, { id: 'b', name: 'openssl-sys', version: '0.9.117' }], resolve: { nodes: [{ id: 'a', features: [] }, { id: 'b', features: [] }] } });

test('lock detection finds openssl-src only for windows-msvc', () => {
  assert.equal(detectOpenSsl({ lockText: lock(), targetTriple: TRIPLE }).opensslSrc.checksum, 'bb');
  assert.equal(detectOpenSsl({ lockText: lock(), targetTriple: 'aarch64-apple-darwin' }).reason, 'unsupported-target');
  assert.equal(detectOpenSsl({ lockText: 'version = 4\n\n[[package]]\nname = "x"\nversion = "1"\n', targetTriple: TRIPLE }).reason, 'no-openssl-src');
  assert.equal(detectOpenSsl({ lockText: `${lock()}\n[[package]]\nname = "openssl-src"\nversion = "111.0.0"\n`, targetTriple: TRIPLE }).reason, 'multiple-openssl-src-versions');
});

test('key derivation changes with every recipe input and ignores feature order', () => {
  const key = (patch) => deriveKey({ ...base, ...patch }).key;
  const reference = key({});
  assert.equal(key({ features: ['b', 'a'] }), key({ features: ['a', 'b'] }));
  const patches = [{ features: ['legacy'] }, { targetTriple: 'i686-pc-windows-msvc' }, { opensslSrc: { version: '300.6.1+3.6.3', checksum: 'zz' } }, { opensslSrc: { version: '300.7.0+3.7.0', checksum: 'bb' } }, { toolchain: { ...toolchain, msvc: { toolset: '14.52', banner: 'x' } } }, { toolchain: { ...toolchain, sdk: '10.0.1.0' } }, { toolchain: { ...toolchain, perl: { path: 'p', version: '5.40' } } }, { toolchain: { ...toolchain, nasm: { present: false, version: null } } }, { cc: '1.4.5' }];
  for (const patch of patches) assert.notEqual(key(patch), reference, JSON.stringify(patch));
  assert.equal(deriveKey(base).key16.length, 16);
});

test('override detection covers prefixed and unprefixed names', () => {
  assert.deepEqual(findOverrides({ OPENSSL_DIR: 'x' }, TRIPLE), ['OPENSSL_DIR']);
  assert.deepEqual(findOverrides({ x86_64_pc_windows_msvc_openssl_lib_dir: 'x' }, TRIPLE), ['x86_64_pc_windows_msvc_openssl_lib_dir']);
  assert.deepEqual(findOverrides({ OPENSSL_INCLUDE_DIR: 'x', PATH: 'y' }, TRIPLE), ['OPENSSL_INCLUDE_DIR']);
  assert.deepEqual(findOverrides({ OPENSSL_STATIC: '1' }, TRIPLE), []);
});

test('injection uses uppercase target-prefixed names', () => {
  assert.deepEqual({ ...injectionEnvironment(TRIPLE, 'D:\\n\\abc') }, { X86_64_PC_WINDOWS_MSVC_OPENSSL_DIR: 'D:\\n\\abc', X86_64_PC_WINDOWS_MSVC_OPENSSL_NO_VENDOR: '1', X86_64_PC_WINDOWS_MSVC_OPENSSL_STATIC: '1' });
});

test('metadata selection extracts enabled openssl-src features', () => {
  const metadata = { packages: [{ id: 'a', name: 'openssl-src', version: '1' }, { id: 'b', name: 'openssl-sys', version: '0.9.117' }], resolve: { nodes: [{ id: 'a', features: ['legacy', 'idea'] }, { id: 'b', features: [] }] } };
  assert.deepEqual(parseMetadataSelection(metadata), { features: ['idea', 'legacy'], opensslSysVersion: '0.9.117' });
  assert.equal(parseMetadataSelection({ packages: [], resolve: { nodes: [] } }), null);
  assert.match(renderHelperCrate({ opensslSrcVersion: '1.0.0+3', features: ['legacy'], ccVersion: '1.4.4' })['Cargo.toml'], /openssl-src = \{ version = "=1\.0\.0\+3", features = \["legacy"\] \}[\s\S]*cc = "=1\.4\.4"/);
});

const fakeProducer = async ({ env }) => {
  const dest = env.RK_OPENSSL_DEST;
  await mkdir(join(dest, 'lib'), { recursive: true }); await mkdir(join(dest, 'include', 'openssl'), { recursive: true });
  await writeFile(join(dest, 'lib', 'libssl.lib'), 'ssl'); await writeFile(join(dest, 'lib', 'libcrypto.lib'), 'crypto');
  await writeFile(join(dest, 'include', 'openssl', 'opensslv.h'), '#define OPENSSL_VERSION_STR "3.6.3"');
};

async function harness(producerFn = fakeProducer) {
  const root = await mkdtemp(join(tmpdir(), 'rk-native-'));
  const lockRoot = join(root, 'ws'); await mkdir(lockRoot); await writeFile(join(lockRoot, 'Cargo.lock'), lock());
  let produced = 0;
  const make = (extra = {}) => new NativeOpenSslCache({ root: join(root, 'n'), exec: async () => ({ stdout: metadataJson }), producer: async (o) => { produced += 1; return producerFn(o); }, probe: async () => toolchain, environment: {}, ...extra });
  const args = { lockRoot, targetTriple: TRIPLE, environment: {}, argv: ['build'], cargoPath: 'cargo', producerEnvironment: { PATH: '' } };
  return { root, make, cache: make(), args, produced: () => produced, close: () => rm(root, { recursive: true, force: true }) };
}

test('miss produces a verified entry; second call hits; env names are injected', async () => {
  const h = await harness();
  try {
    const first = await h.cache.prepare(h.args);
    assert.equal(first.status, 'miss'); assert.equal(h.produced(), 1);
    assert.equal(first.environment.X86_64_PC_WINDOWS_MSVC_OPENSSL_DIR, first.path);
    assert.equal(first.path, join(h.root, 'n', first.key));
    const second = await h.cache.prepare(h.args);
    assert.equal(second.status, 'hit'); assert.equal(h.produced(), 1); assert.equal(second.key, first.key);
    const manifest = JSON.parse(await readFile(join(first.path, 'manifest.json'), 'utf8'));
    assert.ok(manifest.key.startsWith(first.key)); assert.ok(manifest.files['lib/libssl.lib'].sha256);
  } finally { await h.close(); }
});

test('concurrent preparations share one producer', async () => {
  const h = await harness();
  try {
    const results = await Promise.all([h.cache.prepare(h.args), h.cache.prepare(h.args), h.cache.prepare(h.args)]);
    assert.equal(h.produced(), 1); assert.ok(results.every((r) => r.path === results[0].path));
  } finally { await h.close(); }
});

test('user override and opt-out skip injection', async () => {
  const h = await harness();
  try {
    const overridden = await h.cache.prepare({ ...h.args, environment: { OPENSSL_DIR: 'C:\\ssl' } });
    assert.equal(overridden.status, 'skipped'); assert.equal(overridden.skipped, 'user-override'); assert.deepEqual({ ...overridden.environment }, {});
    const off = h.make({ environment: { [OPENSSL_OPT_OUT_ENV]: '0' } });
    assert.equal((await off.prepare(h.args)).skipped, 'opt-out');
    assert.equal(h.produced(), 0);
  } finally { await h.close(); }
});

test('corrupt entry is quarantined and rebuilt', async () => {
  const h = await harness();
  try {
    const first = await h.cache.prepare(h.args);
    await writeFile(join(first.path, 'lib', 'libcrypto.lib'), 'XXXXXX');
    const again = await h.make().prepare(h.args);
    assert.equal(again.status, 'miss'); assert.match(again.recovered, /quarantined/);
    await assert.doesNotReject(stat(join(h.root, 'n', 'q')));
    assert.equal(await readFile(join(again.path, 'lib', 'libcrypto.lib'), 'utf8'), 'crypto');
    assert.equal(h.produced(), 2);
  } finally { await h.close(); }
});

test('producer failure is fail-open (skipped, no environment)', async () => {
  const h = await harness(async () => { throw new Error('boom'); });
  try {
    const result = await h.cache.prepare(h.args);
    assert.equal(result.skipped, 'producer-failed'); assert.deepEqual({ ...result.environment }, {});
  } finally { await h.close(); }
});

test('GC never evicts pinned entries and keeps the newest unpinned ones', async () => {
  const h = await harness();
  try {
    const keys = ['aaaaaaaaaaaaaaaa', 'bbbbbbbbbbbbbbbb', 'cccccccccccccccc', 'dddddddddddddddd'];
    for (const [index, key] of keys.entries()) { await mkdir(join(h.root, 'n', key), { recursive: true }); const when = new Date(Date.now() - (10 - index) * 60_000); await utimes(join(h.root, 'n', key), when, when); }
    const result = await h.cache.gc({ pinned: new Set(['aaaaaaaaaaaaaaaa']), keepUnpinned: 1 });
    assert.deepEqual(result.evicted.sort(), ['bbbbbbbbbbbbbbbb', 'cccccccccccccccc']);
    await assert.doesNotReject(stat(join(h.root, 'n', 'aaaaaaaaaaaaaaaa')));
    await assert.doesNotReject(stat(join(h.root, 'n', 'dddddddddddddddd')));
  } finally { await h.close(); }
});

test('state pins native entries only for non-retired generations', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rk-native-state-'));
  try {
    const workspace = join(root, 'w'); await mkdir(workspace);
    const state = await ThinState.open({ externalRoot: root });
    await state.recordNativeReference(workspace, 1, 'openssl', 'k1111111111111111');
    assert.deepEqual([...await state.pinnedNativeKeys('openssl')], ['k1111111111111111']);
    await state.rotate(workspace, 'test');
    assert.deepEqual([...await state.pinnedNativeKeys('openssl')], []);
    await state.recordNativeReference(workspace, 2, 'openssl', 'k2222222222222222');
    assert.deepEqual([...await state.pinnedNativeKeys('openssl')], ['k2222222222222222']);
  } finally { await rm(root, { recursive: true, force: true }); }
});
