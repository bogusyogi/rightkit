import { test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';

const SOURCE = fs.readFileSync(new URL('./ipc_fixture.js', import.meta.url), 'utf8');

const PREFIXES = { windows: 'http://ipc.localhost/', macos: 'ipc://localhost/' };

class FakeResponse {
  constructor(body, init = {}) {
    this.body = body;
    const h = {};
    for (const [k, v] of Object.entries(init.headers || {})) h[k.toLowerCase()] = v;
    this.headers = { get: (k) => h[k.toLowerCase()] ?? null };
  }
  json() {
    return Promise.resolve(JSON.parse(this.body));
  }
}

function setup(platform = 'windows') {
  const prefix = PREFIXES[platform];
  const passed = [];
  const realFetch = async (input, init) => {
    passed.push({ input, init });
    return 'REAL';
  };
  const internals = {
    convertFileSrc: (p, proto) => {
      assert.equal(p, '');
      assert.equal(proto, 'ipc');
      return prefix;
    },
  };
  const invoke = () => 'INVOKE';
  Object.defineProperty(internals, 'invoke', { value: invoke });
  const win = { fetch: realFetch, __TAURI_INTERNALS__: internals };
  win.window = win;
  const ctx = vm.createContext({
    window: win,
    Response: FakeResponse,
    Promise,
    JSON,
    Object,
    Array,
    Number,
    Math,
    Error,
    String,
    decodeURIComponent,
  });
  vm.runInContext(SOURCE, ctx);
  const call = (cmd, args) =>
    win.fetch(prefix + encodeURIComponent(cmd), {
      method: 'POST',
      body: JSON.stringify(args ?? {}),
      headers: { 'Tauri-Callback': '1', 'Tauri-Error': '2', 'Tauri-Invoke-Key': 'k' },
    });
  return { win, prefix, passed, call, realFetch, invoke, internals };
}

const read = async (resp) => ({
  kind: resp.headers.get('Tauri-Response'),
  type: resp.headers.get('Content-Type'),
  value: await resp.json(),
});

for (const platform of ['windows', 'macos']) {
  test(`ok reply on ${platform} prefix`, async () => {
    const t = setup(platform);
    t.win.__RK_QA_IPC__.add({ cmd: 'get_thing', replies: [{ kind: 'ok', value: { a: 1 } }] });
    const r = await read(await t.call('get_thing'));
    assert.deepEqual(r, { kind: 'ok', type: 'application/json', value: { a: 1 } });
    assert.equal(t.passed.length, 0);
  });
}

test('error reply', async () => {
  const t = setup();
  t.win.__RK_QA_IPC__.add({ cmd: 'boom', replies: [{ kind: 'error', value: 'nope' }] });
  const r = await read(await t.call('boom'));
  assert.deepEqual(r, { kind: 'error', type: 'application/json', value: 'nope' });
});

test('encoded command names are decoded', async () => {
  const t = setup();
  t.win.__RK_QA_IPC__.add({ cmd: 'plugin:x|do it', replies: [{ kind: 'ok', value: 1 }] });
  const r = await read(await t.call('plugin:x|do it'));
  assert.equal(r.value, 1);
});

test('replies run in sequence and the last repeats', async () => {
  const t = setup();
  t.win.__RK_QA_IPC__.add({
    cmd: 'seq',
    replies: [
      { kind: 'ok', value: 1 },
      { kind: 'error', value: 'two' },
      { kind: 'ok', value: 3 },
    ],
  });
  const got = [];
  for (let i = 0; i < 5; i++) got.push(await read(await t.call('seq')));
  assert.deepEqual(
    got.map((g) => [g.kind, g.value]),
    [['ok', 1], ['error', 'two'], ['ok', 3], ['ok', 3], ['ok', 3]],
  );
});

test('times bounds a rule, then requests pass through', async () => {
  const t = setup();
  t.win.__RK_QA_IPC__.add({ cmd: 'once', times: 1, replies: [{ kind: 'ok', value: 'x' }] });
  assert.equal((await read(await t.call('once'))).value, 'x');
  assert.equal(await t.call('once'), 'REAL');
  assert.equal(t.passed.length, 1);
});

test('args are a subset match (nested objects recurse)', async () => {
  const t = setup();
  const ipc = t.win.__RK_QA_IPC__;
  ipc.add({ cmd: 'q', args: { id: 7, opts: { deep: true } }, replies: [{ kind: 'ok', value: 'hit' }] });
  assert.equal((await read(await t.call('q', { id: 7, opts: { deep: true, more: 1 }, extra: 'e' }))).value, 'hit');
  assert.equal(await t.call('q', { id: 8, opts: { deep: true } }), 'REAL');
  assert.equal(await t.call('q', { id: 7, opts: {} }), 'REAL');
  assert.equal(await t.call('q'), 'REAL');
});

test('first rule with a matching command and args wins', async () => {
  const t = setup();
  const ipc = t.win.__RK_QA_IPC__;
  ipc.add({ cmd: 'q', args: { id: 1 }, replies: [{ kind: 'ok', value: 'one' }] });
  ipc.add({ cmd: 'q', replies: [{ kind: 'ok', value: 'any' }] });
  assert.equal((await read(await t.call('q', { id: 1 }))).value, 'one');
  assert.equal((await read(await t.call('q', { id: 2 }))).value, 'any');
});

test('unmatched IPC and non-IPC requests pass through to the real fetch', async () => {
  const t = setup();
  t.win.__RK_QA_IPC__.add({ cmd: 'known', replies: [{ kind: 'ok', value: 1 }] });
  assert.equal(await t.call('other'), 'REAL');
  assert.equal(await t.win.fetch('https://example.com/known', { method: 'GET' }), 'REAL');
  assert.equal(await t.win.fetch({ url: 'https://example.com/y' }), 'REAL');
  assert.equal(t.passed.length, 3);
  assert.equal(t.passed[1].input, 'https://example.com/known');
});

test('a prefix from the other platform is not an IPC request', async () => {
  const t = setup('windows');
  t.win.__RK_QA_IPC__.add({ cmd: 'known', replies: [{ kind: 'ok', value: 1 }] });
  assert.equal(await t.win.fetch(PREFIXES.macos + 'known', { body: '{}' }), 'REAL');
});

test('log records cmd and args for every IPC call; clear resets rules and log', async () => {
  const t = setup();
  const ipc = t.win.__RK_QA_IPC__;
  ipc.add({ cmd: 'a', replies: [{ kind: 'ok', value: 1 }] });
  await t.call('a', { x: 1 });
  await t.call('b');
  await t.win.fetch('https://example.com/');
  assert.deepEqual(JSON.parse(JSON.stringify(ipc.log())), [
    { cmd: 'a', args: { x: 1 } },
    { cmd: 'b', args: {} },
  ]);
  ipc.clear();
  assert.deepEqual(JSON.parse(JSON.stringify(ipc.log())), []);
  assert.equal(await t.call('a'), 'REAL');
});

test('non-JSON bodies log null args', async () => {
  const t = setup();
  await t.win.fetch(t.prefix + 'raw', { method: 'POST', body: new Uint8Array([1, 2]) });
  assert.deepEqual(JSON.parse(JSON.stringify(t.win.__RK_QA_IPC__.log())), [{ cmd: 'raw', args: null }]);
});

test('invalid rules are rejected', () => {
  const t = setup();
  const ipc = t.win.__RK_QA_IPC__;
  assert.throws(() => ipc.add({ replies: [{ kind: 'ok', value: 1 }] }));
  assert.throws(() => ipc.add({ cmd: 'a', replies: [] }));
  assert.throws(() => ipc.add({ cmd: 'a', replies: [{ kind: 'maybe', value: 1 }] }));
  assert.throws(() => ipc.add({ cmd: 'a', times: 0, replies: [{ kind: 'ok', value: 1 }] }));
});

test('undefined ok value answers JSON null', async () => {
  const t = setup();
  t.win.__RK_QA_IPC__.add({ cmd: 'u', replies: [{ kind: 'ok' }] });
  assert.equal((await read(await t.call('u'))).value, null);
});

test('never touches invoke, ipc or postMessage; replaces only fetch', () => {
  const t = setup();
  assert.equal(t.internals.invoke, t.invoke);
  assert.equal('ipc' in t.win, false);
  assert.equal('postMessage' in t.internals, false);
  assert.notEqual(t.win.fetch, t.realFetch);
});

test('installing twice keeps the first instance', () => {
  const t = setup();
  const first = t.win.__RK_QA_IPC__;
  const ctxFetch = t.win.fetch;
  const ctx = Object.values({}).length; // no-op, keep lint quiet
  void ctx;
  vm.runInContext(SOURCE, vm.createContext({ window: t.win, Response: FakeResponse, Promise, JSON, Object, Array, Number, Math, Error, String, decodeURIComponent }));
  assert.equal(t.win.__RK_QA_IPC__, first);
  assert.equal(t.win.fetch, ctxFetch);
});
