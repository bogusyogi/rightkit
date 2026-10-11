import { afterEach, describe, expect, it } from 'vitest';
import { QaBrowserSession, TimeoutError } from './index.js';
import { CdpClient } from './cdp.js';
import { keyDefinition, QaKeyboard } from './keyboard.js';
import { mockCdp, pageResponder } from '../test/mock-cdp.js';
const cleanup: (() => Promise<unknown>)[] = [];
afterEach(async () => { for (const close of cleanup.splice(0).reverse()) await close(); });

async function fixture(handler = pageResponder, timeout = 500) {
  const server = await mockCdp(handler); cleanup.push(server.close);
  const browser = await QaBrowserSession.connect(server.endpoint, { timeout });
  cleanup.push(() => browser.close().catch(() => {}));
  const page = await browser.newPage();
  return { server, browser, page };
}

describe('raw CDP', () => {
  it('routes concurrent out-of-order responses & subscriptions by session', async () => {
    let first: any;
    const server = await mockCdp((message, send) => {
      if (!first) first = message;
      else {
        send({ id: message.id, result: { value: 'second' } }); send({ id: first.id, result: { value: 'first' } });
        send({ method: 'event', sessionId: 's2', params: { n: 2 } }); send({ method: 'event', sessionId: 's1', params: { n: 1 } });
      }
    }); cleanup.push(server.close);
    const client = await CdpClient.connect(server.endpoint); cleanup.push(async () => client.close());
    const events: number[] = []; const other: number[] = [];
    // Events arrive as separate socket frames after the responses; wait for delivery instead of assuming it.
    let delivered!: () => void; const seen = new Promise<void>((resolve) => { delivered = resolve; });
    const off = client.on('event', ({ n }) => { events.push(n); delivered(); }, 's1');
    const offOther = client.on('event', ({ n }) => other.push(n), 's2');
    expect(await Promise.all([client.send('one', {}, 's1'), client.send('two', {}, 's2')])).toEqual([{ value: 'first' }, { value: 'second' }]);
    await seen; // the s2 event was sent before the s1 event on the same ordered socket, so both have been routed
    expect(events).toEqual([1]); expect(other).toEqual([2]); off(); offOther();
  });
  it('times out commands, ignores late replies & rejects pending calls on disconnect', async () => {
    const server = await mockCdp(() => {}); cleanup.push(server.close);
    const client = await CdpClient.connect(server.endpoint, { timeout: 30 }); cleanup.push(async () => client.close());
    await expect(client.send('never')).rejects.toBeInstanceOf(TimeoutError);
    server.emit({ id: server.messages[0].id, result: { late: true } });
    const pending = client.send('pending'); const rejected = expect(pending).rejects.toThrow(/CDP socket (closed|error)/);
    server.disconnect(); await rejected;
  });
  it('throws protocol errors with their message', async () => {
    const server = await mockCdp((message, send) => send({ id: message.id, error: { message: 'bad command', code: -1 } })); cleanup.push(server.close);
    const client = await CdpClient.connect(server.endpoint); cleanup.push(async () => client.close());
    await expect(client.send('bad')).rejects.toThrow('bad command');
  });
  it('creates flattened targets, selects main default context & closes owned targets', async () => {
    const { page, server, browser } = await fixture();
    expect(await page.evaluate((n) => n, 4)).toBe(true);
    expect(server.messages.find((m) => m.method === 'Runtime.callFunctionOn').params).toMatchObject({ executionContextId: 13, returnByValue: true, awaitPromise: true, arguments: [{ value: 4 }] });
    expect(server.messages.find((m) => m.method === 'Target.attachToTarget').params.flatten).toBe(true);
    await browser.close(); await browser.close();
    expect(server.messages.filter((m) => m.method === 'Target.closeTarget')).toHaveLength(1);
  });
  it('throws JavaScript exceptionDetails', async () => {
    const { page } = await fixture((message, send) => {
      if (message.method === 'Runtime.callFunctionOn') send({ id: message.id, result: { exceptionDetails: { exception: { description: 'Error: fixture boom' } } } });
      else pageResponder(message, send);
    });
    await expect(page.evaluate(() => 1)).rejects.toThrow('fixture boom');
  });
  it('tracks destroyed contexts & waits for replacement main context', async () => {
    let update: ((message: any) => void) | undefined;
    const { page, server } = await fixture((message, send) => { update = send; pageResponder(message, send); });
    update!({ method: 'Runtime.executionContextsCleared', sessionId: 'session', params: {} });
    await new Promise((r) => setTimeout(r, 10));
    const pending = page.evaluate(() => 1);
    update!({ method: 'Runtime.executionContextCreated', sessionId: 'session', params: { context: { id: 20, auxData: { frameId: 'main', isDefault: true } } } });
    await pending;
    expect(server.messages.filter((m) => m.method === 'Runtime.callFunctionOn').at(-1).params.executionContextId).toBe(20);
  });
  it('correlates navigation lifecycle to main frame & new loader', async () => {
    const { page } = await fixture((message, send) => {
      if (message.method === 'Page.navigate') {
        for (const [frameId, loaderId] of [['child', 'new'], ['main', 'old'], ['main', 'new']]) send({ method: 'Page.lifecycleEvent', sessionId: 'session', params: { frameId, loaderId, name: 'load' } });
        send({ id: message.id, result: { frameId: 'main', loaderId: 'new' } });
      } else pageResponder(message, send);
    });
    await page.goto('https://fixture');
  });
  it('navigation & predicate deadlines throw typed named timeouts', async () => {
    const { page } = await fixture((message, send) => {
      if (message.method === 'Page.navigate') send({ id: message.id, result: { loaderId: 'new' } });
      else if (message.method === 'Runtime.callFunctionOn') send({ id: message.id, result: { result: { value: false } } });
      else pageResponder(message, send);
    });
    await expect(page.goto('https://fixture', { timeout: 35 })).rejects.toBeInstanceOf(TimeoutError);
    await expect(page.waitForSelector('#missing', { timeout: 35 })).rejects.toThrow('Selector #missing');
    await expect(page.waitForFunction(() => false, { timeout: 35, polling: 5 })).rejects.toThrow('Predicate');
  });
  it('rejects waits promptly when page closes', async () => {
    const { page } = await fixture((message, send) => {
      if (message.method === 'Runtime.callFunctionOn') send({ id: message.id, result: { result: { value: false } } });
      else pageResponder(message, send);
    });
    const waiting = page.waitForFunction(() => false); const rejected = expect(waiting).rejects.toThrow('Page closed');
    await page.close(); await rejected;
  });
  it('dispatches real key commands & validates keys', async () => {
    const { page, server } = await fixture();
    await page.keyboard.press('Control+A'); await page.keyboard.press('Enter');
    const keys = server.messages.filter((m) => m.method === 'Input.dispatchKeyEvent').map((m) => m.params);
    expect(keys[1]).toMatchObject({ key: 'a', code: 'KeyA', modifiers: 2, type: 'rawKeyDown' });
    expect(keys[4]).toMatchObject({ key: 'Enter', text: '\r', type: 'keyDown' });
    await expect(page.keyboard.press('Unknown')).rejects.toThrow('Unsupported key');
  });
  it('maps every required special key, letters, digits & modifiers', () => {
    for (const key of ['Escape', 'Enter', 'Tab', 'ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown', 'Backspace', 'Delete', 'Home', 'End', 'PageUp', 'PageDown', 'Space', 'Shift', 'Control', 'Alt', 'Meta', 'a', 'Z', '0', '9']) {
      expect(keyDefinition(key, 0).windowsVirtualKeyCode).toBeGreaterThan(0);
    }
    expect(keyDefinition('a', 8).text).toBe('A'); expect(keyDefinition('1', 8).text).toBe('!');
    expect(keyDefinition('a', 8).unmodifiedText).toBe('a');
  });

  it('debug stays opt-in & logs method names without source or input', async () => {
    const server = await mockCdp(pageResponder); cleanup.push(server.close);
    const logs: string[] = [];
    const browser = await QaBrowserSession.connect(server.endpoint, { debug: message => logs.push(message) });
    cleanup.push(() => browser.close());
    const page = await browser.newPage();
    await page.evaluate(() => 'private-source', 'private-argument');
    expect(logs).toContain('CDP Runtime.callFunctionOn');
    expect(logs.join('\n')).not.toMatch(/private-source|private-argument/);
  });

  it('isolates throwing event listeners & removes subscriptions', async () => {
    const server = await mockCdp((message, send) => {
      send({ method: 'event', params: {} }); send({ id: message.id, result: {} });
    }); cleanup.push(server.close);
    const client = await CdpClient.connect(server.endpoint); cleanup.push(async () => client.close());
    let observed = 0;
    client.on('event', () => { throw new Error('observer'); });
    const off = client.on('event', () => observed++);
    await client.send('first'); off(); await client.send('second');
    expect(observed).toBe(1);
  });

  it('rejects invalid deadlines, wait modes & polling before issuing work', async () => {
    const { page } = await fixture();
    await expect(page.goto('fixture', { timeout: -1 })).rejects.toThrow('Timeout');
    await expect(page.goto('fixture', { waitUntil: 'unknown' as any })).rejects.toThrow('waitUntil');
    await expect(page.waitForFunction(() => true, { polling: 0 })).rejects.toThrow('Polling');
    await expect(page.waitForSelector('x', { visible: true, hidden: true })).rejects.toThrow('visible');
    await expect(page.screenshot({ fullPage: true as any })).rejects.toThrow('viewport PNG');
  });

  it('cleans up a target whose initialization fails', async () => {
    const server = await mockCdp((message, send) => {
      if (message.method === 'Network.enable') send({ id: message.id, error: { message: 'initialize failed' } });
      else pageResponder(message, send);
    }); cleanup.push(server.close);
    const browser = await QaBrowserSession.connect(server.endpoint); cleanup.push(() => browser.close());
    await expect(browser.newPage()).rejects.toThrow('initialize failed');
    expect(server.messages.filter(message => message.method === 'Target.closeTarget')).toHaveLength(1);
  });

  it('closes in-flight newPage targets & shares close completion', async () => {
    let created!: () => void;
    const creating = new Promise<void>(resolve => created = resolve);
    const server = await mockCdp((message, send) => {
      if (message.method === 'Target.createTarget') {
        created(); setTimeout(() => pageResponder(message, send), 20);
      } else pageResponder(message, send);
    }); cleanup.push(server.close);
    const browser = await QaBrowserSession.connect(server.endpoint); cleanup.push(() => browser.close());
    const pending = browser.newPage(); const rejection = expect(pending).rejects.toThrow('session closed');
    await creating;
    const closing = browser.close(); expect(browser.close()).toBe(closing);
    await rejection; await closing;
    expect(server.messages.filter(message => message.method === 'Target.closeTarget')).toHaveLength(1);
    await expect(browser.newPage()).rejects.toThrow('session closed');
  });

  it('rejects pending page commands & waits on target detach', async () => {
    const { page, server } = await fixture((message, send) => {
      if (message.method !== 'Runtime.callFunctionOn') pageResponder(message, send);
    });
    const pending = page.evaluate(() => 1); const rejection = expect(pending).rejects.toThrow('Page closed');
    server.emit({ method: 'Target.detachedFromTarget', params: { sessionId: 'session' } });
    await rejection;
    await expect(page.evaluate('1')).rejects.toThrow('Page closed');
  });

  it('shares concurrent page close completion & clears user subscriptions', async () => {
    const { page, server } = await fixture();
    let observed = 0;
    page.on('custom', () => observed++);
    const closing = page.close(); expect(page.close()).toBe(closing); await closing;
    server.emit({ method: 'custom', sessionId: 'session', params: {} });
    await new Promise(resolve => setTimeout(resolve, 10));
    expect(observed).toBe(0);
    expect(server.messages.filter(message => message.method === 'Target.closeTarget')).toHaveLength(1);
    expect(() => page.on('custom', () => {})).toThrow('Page closed');
  });

  it('keeps main-frame context through iframe creation/destruction & frame swaps', async () => {
    const { page, server } = await fixture();
    server.emit({ method: 'Page.frameNavigated', sessionId: 'session', params: { frame: { id: 'new-main' } } });
    server.emit({ method: 'Runtime.executionContextCreated', sessionId: 'session', params: { context: { id: 20, auxData: { frameId: 'new-main', isDefault: true } } } });
    server.emit({ method: 'Runtime.executionContextCreated', sessionId: 'session', params: { context: { id: 21, auxData: { frameId: 'child', isDefault: true } } } });
    server.emit({ method: 'Runtime.executionContextDestroyed', sessionId: 'session', params: { executionContextId: 21 } });
    // Flush preceding events over the same transport before evaluating.
    await page.send('Page.getFrameTree');
    await page.evaluate('1');
    expect(server.messages.at(-1).params.contextId).toBe(20);
  });

  it('navigation ignores unrelated loaders, handles reload & propagates navigation errors', async () => {
    const { page } = await fixture((message, send) => {
      if (message.method === 'Page.reload') {
        send({ method: 'Page.lifecycleEvent', sessionId: 'session', params: { frameId: 'main', loaderId: 'old', name: 'load' } });
        send({ method: 'Page.lifecycleEvent', sessionId: 'session', params: { frameId: 'main', loaderId: 'reload', name: 'init' } });
        send({ id: message.id, result: {} });
        setTimeout(() => send({ method: 'Page.lifecycleEvent', sessionId: 'session', params: { frameId: 'main', loaderId: 'reload', name: 'DOMContentLoaded' } }), 10);
      } else if (message.method === 'Page.navigate') send({ id: message.id, result: { errorText: 'net::ERR_FAILED' } });
      else pageResponder(message, send);
    });
    await page.reload({ waitUntil: 'domcontentloaded' });
    await expect(page.goto('fixture')).rejects.toThrow('net::ERR_FAILED');
  });

  it('networkidle0 observes 500ms with zero requests & resets on brief bursts', async () => {
    const { page } = await fixture((message, send) => {
      if (message.method === 'Page.navigate') {
        send({ id: message.id, result: { loaderId: 'new' } });
        setTimeout(() => {
          send({ method: 'Network.requestWillBeSent', sessionId: 'session', params: { requestId: 'burst' } });
          send({ method: 'Network.loadingFinished', sessionId: 'session', params: { requestId: 'burst' } });
        }, 100);
      } else pageResponder(message, send);
    }, 1500);
    const start = Date.now();
    await page.goto('fixture', { waitUntil: 'networkidle0' });
    expect(Date.now() - start).toBeGreaterThanOrEqual(590);
  });

  it('forgets abandoned old-document requests while retaining new-loader requests', async () => {
    const { page } = await fixture((message, send) => {
      if (message.method === 'Page.navigate') {
        const event = (method: string, params: any) => send({ method, params, sessionId: 'session' });
        event('Network.requestWillBeSent', { requestId: 'favicon', frameId: 'main', loaderId: 'old' });
        event('Network.requestWillBeSent', { requestId: 'document', frameId: 'main', loaderId: 'new' });
        event('Page.frameNavigated', { frame: { id: 'main', loaderId: 'new' } });
        send({ id: message.id, result: { loaderId: 'new' } });
        setTimeout(() => event('Network.loadingFinished', { requestId: 'document' }), 100);
      } else pageResponder(message, send);
    }, 1500);
    const start = Date.now();
    await page.goto('fixture', { waitUntil: 'networkidle0' });
    expect(Date.now() - start).toBeGreaterThanOrEqual(590);
  });

  it('releases modifiers after dispatch failures without replacing original error', async () => {
    const calls: any[] = [];
    const keyboard = new QaKeyboard(async (_method, params) => {
      calls.push(params);
      if (params.key === 'a' || params.key === 'Shift' && params.type === 'keyUp') throw new Error('input failed');
      return {};
    });
    await expect(keyboard.press('Control+Shift+a')).rejects.toThrow('input failed');
    expect(calls.at(-1)).toMatchObject({ type: 'keyUp', key: 'Control', modifiers: 0 });
  });
});

describe('attach to an existing page & page.mouse', () => {
  const pageTargets = [
    { targetId: 'app-page', type: 'page', url: 'app://main/', title: 'Pulse' },
    { targetId: 'app-worker', type: 'service_worker', url: 'app://sw.js', title: '' },
  ];
  /** A debugging endpoint: /json/version points at the same port's browser socket. */
  async function appFixture(options: { targets?: unknown[]; webSocketDebuggerUrl?: string; handler?: typeof pageResponder } = {}) {
    let base = '';
    const targets = options.targets ?? pageTargets;
    const server = await mockCdp((message, send) => {
      if (message.method === 'Target.getTargets') send({ id: message.id, result: { targetInfos: targets } });
      else (options.handler ?? pageResponder)(message, send);
    }, {
      http: (path) => path === '/json/version'
        ? { Browser: 'fixture', webSocketDebuggerUrl: options.webSocketDebuggerUrl ?? `${base}/devtools/browser/app` }
        : undefined,
    });
    base = server.endpoint; cleanup.push(server.close);
    return { server, port: Number(new URL(server.endpoint).port) };
  }
  const methods = (server: { messages: any[] }) => server.messages.map((message) => message.method);
  const mouseEvents = (server: { messages: any[] }, from = 0) => server.messages.slice(from).filter((message) => message.method === 'Input.dispatchMouseEvent').map((message) => message.params);

  it('attaches via /json/version without creating a target & lists page targets only', async () => {
    const { server, port } = await appFixture();
    const browser = await QaBrowserSession.attach({ port, timeout: 500 }); cleanup.push(() => browser.close().catch(() => {}));
    expect(await browser.pages()).toEqual([{ targetId: 'app-page', url: 'app://main/', title: 'Pulse' }]);
    expect(server.httpRequests).toEqual(['/json/version']);
    expect(methods(server)).toEqual(['Target.getTargets']);
  });

  it('attachPage(targetId) attaches flattened without creating a target & runs page commands', async () => {
    const { server, port } = await appFixture();
    const browser = await QaBrowserSession.attach({ port, timeout: 500 }); cleanup.push(() => browser.close().catch(() => {}));
    const page = await browser.attachPage('app-page');
    expect(page.targetId).toBe('app-page');
    expect(await page.evaluate((n: number) => n, 4)).toBe(true);
    expect(methods(server)).not.toContain('Target.createTarget');
    expect(server.messages.find((message) => message.method === 'Target.attachToTarget').params).toEqual({ targetId: 'app-page', flatten: true });
  });

  it('attachPage(predicate) must match exactly one page; unknown ids attach nothing', async () => {
    const { server, port } = await appFixture({ targets: [...pageTargets, { targetId: 'other', type: 'page', url: 'app://other/', title: 'Other' }] });
    const browser = await QaBrowserSession.attach({ port, timeout: 500 }); cleanup.push(() => browser.close().catch(() => {}));
    await expect(browser.attachPage((info) => info.url.startsWith('app://'))).rejects.toThrow('matched 2 pages');
    await expect(browser.attachPage((info) => info.title === 'Missing')).rejects.toThrow('matched 0 pages');
    await expect(browser.attachPage('missing')).rejects.toThrow('No page target');
    expect(await browser.attachPage((info) => info.title === 'Other')).toMatchObject({ targetId: 'other' });
    expect(methods(server)).not.toContain('Target.createTarget');
    expect(server.messages.filter((message) => message.method === 'Target.attachToTarget')).toHaveLength(1);
  });

  it('closing an attached page detaches only & never closes the app page', async () => {
    const { server, port } = await appFixture();
    const browser = await QaBrowserSession.attach({ port, timeout: 500 }); cleanup.push(() => browser.close().catch(() => {}));
    const page = await browser.attachPage('app-page');
    await page.close(); await page.close();
    expect(server.messages.filter((message) => message.method.startsWith('Target.')).map((message) => message.method))
      .toEqual(['Target.getTargets', 'Target.attachToTarget', 'Target.detachFromTarget']);
    expect(server.messages.find((message) => message.method === 'Target.detachFromTarget').params).toEqual({ sessionId: 'session' });
    expect(methods(server)).not.toContain('Target.closeTarget');
    await expect(page.evaluate('1')).rejects.toThrow('Page closed');
  });

  it('closing the browser session detaches attached pages & never closes app targets', async () => {
    const { server, port } = await appFixture();
    const browser = await QaBrowserSession.attach({ port, timeout: 500 });
    await browser.attachPage('app-page');
    await browser.close();
    expect(methods(server)).toContain('Target.detachFromTarget');
    expect(methods(server)).not.toContain('Target.closeTarget');
    await expect(browser.attachPage('app-page')).rejects.toThrow('session closed');
  });

  it('refuses non-loopback hosts & endpoints unless allowRemoteHost, before any request', async () => {
    await expect(QaBrowserSession.attach({ port: 9333, host: '192.168.1.20' })).rejects.toThrow('non-loopback');
    await expect(QaBrowserSession.attach({ httpEndpoint: 'http://example.com:9333' })).rejects.toThrow('non-loopback');
    const { port } = await appFixture({ webSocketDebuggerUrl: 'ws://192.168.1.20:9222/devtools/browser/x' });
    await expect(QaBrowserSession.attach({ port, timeout: 500 })).rejects.toThrow('non-loopback');
  });

  it('validates the port, endpoint shape & discovery payload', async () => {
    await expect(QaBrowserSession.attach({ port: 0 })).rejects.toThrow('Port');
    await expect(QaBrowserSession.attach({ port: 70000 })).rejects.toThrow('Port');
    await expect(QaBrowserSession.attach({})).rejects.toThrow('port or httpEndpoint');
    await expect(QaBrowserSession.attach({ port: 9333, httpEndpoint: 'http://127.0.0.1:9333' })).rejects.toThrow('port or httpEndpoint');
    await expect(QaBrowserSession.attach({ httpEndpoint: 'ws://127.0.0.1:9333' })).rejects.toThrow('http');
    const { port } = await appFixture({ webSocketDebuggerUrl: '' });
    await expect(QaBrowserSession.attach({ port, timeout: 500 })).rejects.toThrow('webSocketDebuggerUrl');
  });

  it('page.mouse: move, down, move while held (buttons mask) & up send exact CDP params', async () => {
    const { page, server } = await fixture();
    const start = server.messages.length;
    await page.mouse.move(10, 20);
    await page.mouse.down();
    await page.mouse.move(30, 40);
    await page.mouse.up();
    expect(mouseEvents(server, start)).toEqual([
      { type: 'mouseMoved', x: 10, y: 20, button: 'none', buttons: 0 },
      { type: 'mousePressed', x: 10, y: 20, button: 'left', buttons: 1, clickCount: 1 },
      { type: 'mouseMoved', x: 30, y: 40, button: 'left', buttons: 1 },
      { type: 'mouseReleased', x: 30, y: 40, button: 'left', buttons: 0, clickCount: 1 },
    ]);
    expect(page.mouse.position()).toEqual({ x: 30, y: 40 });
  });

  it('page.mouse.move steps interpolate intermediate moves & end on the target', async () => {
    const { page, server } = await fixture();
    await page.mouse.move(0, 0);
    const start = server.messages.length;
    await page.mouse.move(10, 0, { steps: 2 });
    expect(mouseEvents(server, start).map((event) => [event.x, event.y])).toEqual([[5, 0], [10, 0]]);
  });

  it('page.mouse right & middle buttons use their own mask bits & clickCount', async () => {
    const { page, server } = await fixture();
    const start = server.messages.length;
    await page.mouse.down({ button: 'right', clickCount: 2 });
    await page.mouse.up({ button: 'right' });
    await page.mouse.down({ button: 'middle' });
    await page.mouse.up({ button: 'middle' });
    expect(mouseEvents(server, start).map((event) => [event.type, event.button, event.buttons, event.clickCount])).toEqual([
      ['mousePressed', 'right', 2, 2], ['mouseReleased', 'right', 0, 2],
      ['mousePressed', 'middle', 4, 1], ['mouseReleased', 'middle', 0, 1],
    ]);
  });

  it('page.mouse rejects double press, release without press, bad steps, bad coordinates & buttons without sending', async () => {
    const { page, server } = await fixture();
    await page.mouse.down();
    const start = server.messages.length;
    await expect(page.mouse.down()).rejects.toThrow('already pressed');
    await page.mouse.up();
    await expect(page.mouse.up()).rejects.toThrow('not pressed');
    await expect(page.mouse.move(1, 1, { steps: 0 })).rejects.toThrow('steps');
    await expect(page.mouse.move(Number.NaN, 0)).rejects.toThrow('finite');
    await expect(page.mouse.down({ button: 'back' as any })).rejects.toThrow('Unsupported button');
    expect(mouseEvents(server, start).map((event) => event.type)).toEqual(['mouseReleased']);
  });

  it('click & hover keep their exact event sequence and update the tracked mouse position', async () => {
    const { page, server } = await fixture((message, send) => {
      const declaration = String(message.params?.functionDeclaration ?? '');
      if (message.method === 'Runtime.callFunctionOn' && message.params?.objectId === 'el') {
        send({ id: message.id, result: { result: declaration.includes('getClientRects') ? { value: { x: 10, y: 20, width: 40, height: 20 } } : { value: undefined } } });
      } else if (message.method === 'Runtime.callFunctionOn' && declaration.includes('querySelector')) {
        send({ id: message.id, result: { result: { objectId: 'el' } } });
      } else pageResponder(message, send);
    });
    const start = server.messages.length;
    await page.click('#button');
    expect(mouseEvents(server, start)).toEqual([
      { type: 'mouseMoved', x: 30, y: 30 },
      { type: 'mousePressed', x: 30, y: 30, button: 'left', buttons: 1, clickCount: 1 },
      { type: 'mouseReleased', x: 30, y: 30, button: 'left', buttons: 0, clickCount: 1 },
    ]);
    expect(page.mouse.position()).toEqual({ x: 30, y: 30 });
    const hoverStart = server.messages.length;
    await page.hover('#button');
    expect(mouseEvents(server, hoverStart)).toEqual([{ type: 'mouseMoved', x: 30, y: 30 }]);
  });
});

describe('keyboard table: function keys, US symbols & chords', () => {
  /** Records every CDP command QaKeyboard sends, in order. */
  function recorder() {
    const calls: Array<{ method: string; params: any }> = [];
    const keyboard = new QaKeyboard(async (method, params) => { calls.push({ method, params }); return {}; });
    return { calls, keyboard };
  }
  it('sends exact rawKeyDown & keyUp events for F3 and F5', async () => {
    for (const [key, virtualKey] of [['F3', 114], ['F5', 116]] as const) {
      const { calls, keyboard } = recorder();
      await keyboard.press(key);
      expect(calls).toEqual([
        { method: 'Input.dispatchKeyEvent', params: { type: 'rawKeyDown', key, code: key, windowsVirtualKeyCode: virtualKey, modifiers: 0 } },
        { method: 'Input.dispatchKeyEvent', params: { type: 'keyUp', key, code: key, windowsVirtualKeyCode: virtualKey, modifiers: 0 } },
      ]);
    }
  });
  it('maps F1 through F24 to Windows virtual keys 112 through 135 and rejects the rest', () => {
    for (let n = 1; n <= 24; n++) expect(keyDefinition(`F${n}`, 0)).toMatchObject({ key: `F${n}`, code: `F${n}`, windowsVirtualKeyCode: 111 + n });
    for (const key of ['F0', 'F25', 'F01']) expect(() => keyDefinition(key, 0)).toThrow(`Unsupported key: ${key}`);
  });
  it('sends exact events for ? as Shift+Slash with shift modifier and text', async () => {
    const { calls, keyboard } = recorder();
    await keyboard.press('?');
    expect(calls).toEqual([
      { method: 'Input.dispatchKeyEvent', params: { type: 'keyDown', key: '?', code: 'Slash', windowsVirtualKeyCode: 191, modifiers: 8, text: '?', unmodifiedText: '/' } },
      { method: 'Input.dispatchKeyEvent', params: { type: 'keyUp', key: '?', code: 'Slash', windowsVirtualKeyCode: 191, modifiers: 8 } },
    ]);
  });
  it('sends exact events for backslash', async () => {
    const { calls, keyboard } = recorder();
    await keyboard.press('\\');
    expect(calls).toEqual([
      { method: 'Input.dispatchKeyEvent', params: { type: 'keyDown', key: '\\', code: 'Backslash', windowsVirtualKeyCode: 220, modifiers: 0, text: '\\', unmodifiedText: '\\' } },
      { method: 'Input.dispatchKeyEvent', params: { type: 'keyUp', key: '\\', code: 'Backslash', windowsVirtualKeyCode: 220, modifiers: 0 } },
    ]);
  });
  it('sends exact events for the chord Control+/ (no text while a command modifier is held)', async () => {
    const { calls, keyboard } = recorder();
    await keyboard.press('Control+/');
    expect(calls).toEqual([
      { method: 'Input.dispatchKeyEvent', params: { type: 'rawKeyDown', key: 'Control', code: 'ControlLeft', windowsVirtualKeyCode: 17, modifiers: 2 } },
      { method: 'Input.dispatchKeyEvent', params: { type: 'rawKeyDown', key: '/', code: 'Slash', windowsVirtualKeyCode: 191, modifiers: 2 } },
      { method: 'Input.dispatchKeyEvent', params: { type: 'keyUp', key: '/', code: 'Slash', windowsVirtualKeyCode: 191, modifiers: 2 } },
      { method: 'Input.dispatchKeyEvent', params: { type: 'keyUp', key: 'Control', code: 'ControlLeft', windowsVirtualKeyCode: 17, modifiers: 0 } },
    ]);
  });
  it('covers every printable US symbol, shifted and unshifted, with its base key code', () => {
    const table: Array<[string, string, string, number, number, string]> = [
      ['/', 'Slash', '/', 191, 0, '/'], ['?', 'Slash', '/', 191, 8, '?'],
      ['\\', 'Backslash', '\\', 220, 0, '\\'], ['|', 'Backslash', '\\', 220, 8, '|'],
      ['`', 'Backquote', '`', 192, 0, '`'], ['~', 'Backquote', '`', 192, 8, '~'],
      ['-', 'Minus', '-', 189, 0, '-'], ['_', 'Minus', '-', 189, 8, '_'],
      ['=', 'Equal', '=', 187, 0, '='], ['+', 'Equal', '=', 187, 8, '+'],
      [';', 'Semicolon', ';', 186, 0, ';'], [':', 'Semicolon', ';', 186, 8, ':'],
      ["'", 'Quote', "'", 222, 0, "'"], ['"', 'Quote', "'", 222, 8, '"'],
      [',', 'Comma', ',', 188, 0, ','], ['<', 'Comma', ',', 188, 8, '<'],
      ['.', 'Period', '.', 190, 0, '.'], ['>', 'Period', '.', 190, 8, '>'],
      ['[', 'BracketLeft', '[', 219, 0, '['], ['{', 'BracketLeft', '[', 219, 8, '{'],
      [']', 'BracketRight', ']', 221, 0, ']'], ['}', 'BracketRight', ']', 221, 8, '}'],
      ['1', 'Digit1', '1', 49, 0, '1'], ['!', 'Digit1', '1', 49, 8, '!'],
      ['0', 'Digit0', '0', 48, 0, '0'], [')', 'Digit0', '0', 48, 8, ')'],
    ];
    for (const [key, code, unmodifiedText, windowsVirtualKeyCode, modifiers, text] of table) {
      expect(keyDefinition(key, 0)).toMatchObject({ key: modifiers ? text : unmodifiedText, code, windowsVirtualKeyCode, modifiers: modifiers ? 8 : 0 });
      expect(keyDefinition(key, 0).unmodifiedText).toBe(unmodifiedText);
      expect(keyDefinition(key, 0).text).toBe(text);
    }
    // Shift held as its own modifier selects the shifted character of the same key.
    expect(keyDefinition('/', 8)).toMatchObject({ key: '?', code: 'Slash', text: '?', modifiers: 8 });
    expect(() => keyDefinition('€', 0)).toThrow('Unsupported key: €');
  });
  it('treats a trailing + as the plus key, alone and in a chord', async () => {
    const { calls, keyboard } = recorder();
    await keyboard.press('+');
    expect(calls[0].params).toMatchObject({ type: 'keyDown', key: '+', code: 'Equal', windowsVirtualKeyCode: 187, modifiers: 8, text: '+' });
    const chord = recorder();
    await chord.keyboard.press('Control++');
    expect(chord.calls.map((call) => call.params.key)).toEqual(['Control', '+', '+', 'Control']);
    expect(chord.calls[1].params).toMatchObject({ type: 'rawKeyDown', key: '+', code: 'Equal', modifiers: 10 });
  });
});

describe('browser-level escape hatch', () => {
  async function browserFixture(handler: (message: any, send: (message: any) => void) => void) {
    const server = await mockCdp(handler); cleanup.push(server.close);
    const browser = await QaBrowserSession.connect(server.endpoint, { timeout: 500 });
    cleanup.push(() => browser.close().catch(() => {}));
    return { server, browser };
  }
  it('send() forwards browser-scoped commands with no sessionId', async () => {
    const { server, browser } = await browserFixture((message, send) => {
      send({ id: message.id, result: message.method === 'Browser.getVersion' ? { product: 'HeadlessChrome/1.0', protocolVersion: '1.3' } : {} });
    });
    expect(await browser.send('Browser.getVersion')).toEqual({ product: 'HeadlessChrome/1.0', protocolVersion: '1.3' });
    expect(server.messages[0]).toEqual({ id: 1, method: 'Browser.getVersion', params: {} });
    await expect(browser.send('')).rejects.toThrow('CDP method must be a non-empty string');
  });
  it('version() returns Browser.getVersion without a sessionId', async () => {
    const { server, browser } = await browserFixture((message, send) => {
      send({ id: message.id, result: { product: 'HeadlessChrome/1.0', userAgent: 'x' } });
    });
    expect(await browser.version()).toMatchObject({ product: 'HeadlessChrome/1.0' });
    expect(server.messages.at(-1)).toEqual({ id: 1, method: 'Browser.getVersion', params: {} });
  });
  it('commandLine() returns the argv from Browser.getBrowserCommandLine', async () => {
    const { server, browser } = await browserFixture((message, send) => {
      send({ id: message.id, result: message.method === 'Browser.getBrowserCommandLine' ? { arguments: ['chrome', '--headless=new', '--enable-automation'] } : {} });
    });
    expect(await browser.commandLine()).toEqual(['chrome', '--headless=new', '--enable-automation']);
    expect(server.messages.at(-1)).toEqual({ id: 1, method: 'Browser.getBrowserCommandLine', params: {} });
  });
  it('commandLine() rejects with a clear --enable-automation error when the browser refuses', async () => {
    const { browser } = await browserFixture((message, send) => {
      send({ id: message.id, error: { code: -32601, message: "'Browser.getBrowserCommandLine' wasn't found" } });
    });
    await expect(browser.commandLine()).rejects.toThrow(/launched with --enable-automation/);
    await expect(browser.commandLine()).rejects.toThrow(/Browser\.getBrowserCommandLine was refused/);
  });
  it('commandLine() rejects a reply that is not an argument list', async () => {
    const { browser } = await browserFixture((message, send) => {
      send({ id: message.id, result: { arguments: 'chrome --headless' } });
    });
    await expect(browser.commandLine()).rejects.toThrow('did not return an argument list');
  });
  it('escape-hatch methods reject after close without sending', async () => {
    const { server, browser } = await browserFixture((message, send) => send({ id: message.id, result: {} }));
    await browser.close();
    const sent = server.messages.length;
    await expect(browser.send('Browser.getVersion')).rejects.toThrow('Browser session closed');
    await expect(browser.version()).rejects.toThrow('Browser session closed');
    await expect(browser.commandLine()).rejects.toThrow('Browser session closed');
    expect(server.messages.length).toBe(sent);
  });
});
