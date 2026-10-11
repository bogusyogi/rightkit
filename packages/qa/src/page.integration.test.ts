import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, it } from 'vitest';
import { createServer, type Server } from 'node:http';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { launchChromeForTesting, type ChromeForTestingBrowser } from '@rightkit/qa-chrome';
import { QaBrowserSession, type QaPage, TimeoutError } from './index.js';

// Deliberately no skip: this integration lane requires installed Chrome for Testing.
describe('QaPage with real Chrome for Testing', () => {
  let chrome: ChromeForTestingBrowser;
  let browser: QaBrowserSession;
  let page: QaPage;
  let server: Server;
  let url: string;
  let output: string;
  beforeAll(async () => {
    const html = await readFile(new URL('../test/fixtures/page.html', import.meta.url));
    output = await mkdtemp(path.join(tmpdir(), 'rk-page-test-'));
    server = createServer((_request, response) => {
      response.writeHead(200, { 'Content-Type': 'text/html' }); response.end(html);
    });
    await new Promise<void>(resolve => server.listen(0, '127.0.0.1', resolve));
    url = `http://127.0.0.1:${(server.address() as { port: number }).port}`;
    chrome = await launchChromeForTesting();
    browser = await QaBrowserSession.connect(chrome.wsEndpoint, { timeout: 5000 });
  }, 30_000);
  beforeEach(async () => {
    page = await browser.newPage();
    await page.setViewport({ width: 640, height: 480, deviceScaleFactor: 2 });
    await page.evaluateOnNewDocument((value: string) => { (window as any).beforeScripts = value; }, 'initialized');
    await page.goto(url);
  });
  afterEach(async () => { await page?.close(); });
  afterAll(async () => {
    try { await browser?.close(); }
    finally {
      try {
        if (chrome) {
          const report = await chrome.close();
          expect(report.exited).toBe(true); expect(report.profileRemoved).toBe(true);
          expect(existsSync(chrome.profileDir)).toBe(false);
        }
      } finally {
        if (server) { server.closeAllConnections(); await new Promise<void>(resolve => server.close(() => resolve())); }
        if (output) await rm(output, { recursive: true, force: true });
      }
    }
  }, 20_000);

  it('navigates, reloads & observes DOMContentLoaded, load, network idle & same-document navigation', async () => {
    const events: string[] = [];
    const off = page.on('Page.lifecycleEvent', event => events.push(event.name));
    await page.goto(`${url}/second`, { waitUntil: 'domcontentloaded' });
    expect(await page.evaluate('location.pathname')).toBe('/second');
    await page.reload({ waitUntil: 'load' });
    const start = Date.now();
    await page.goto(`${url}/idle`, { waitUntil: 'networkidle0' });
    expect(Date.now() - start).toBeGreaterThanOrEqual(500);
    await page.goto(`${url}/idle#hash`);
    expect(await page.evaluate('location.hash')).toBe('#hash');
    expect(events).toContain('DOMContentLoaded'); expect(events).toContain('load'); off();
  });

  it('sets viewport, DPR & mobile metrics; init scripts precede page scripts', async () => {
    expect(await page.evaluate(() => [innerWidth, innerHeight, devicePixelRatio, (window as any).initSeen])).toEqual([640, 480, 2, 'initialized']);
    await page.evaluateOnNewDocument('window.fromSource = 42');
    await page.reload();
    expect(await page.evaluate('window.fromSource')).toBe(42);
    await page.setViewport({ width: 360, height: 640, deviceScaleFactor: 3, mobile: true });
    expect(await page.evaluate(() => [innerWidth, innerHeight, devicePixelRatio])).toEqual([360, 640, 3]);
  });

  it('evaluates JSON, async functions, strings, element callbacks & exceptions in main default context', async () => {
    expect(await page.evaluate(async (n: number) => ({ result: await Promise.resolve(n * 2), child: (window as any).childOnly ?? false }), 4)).toEqual({ result: 8, child: false });
    expect(await page.evaluate('document.title')).toBe('QaPage fixture');
    expect(await page.$eval('h1', (element, suffix: string) => element.textContent + suffix, '!')).toBe('QaPage fixture!');
    expect(await page.$$eval('.item', (elements, separator: string) => elements.map(element => element.textContent).join(separator), '/')).toBe('one/two');
    expect(await page.evaluate({ result(n: number) { return n + 1; } }.result, 2)).toBe(3);
    await expect(page.evaluate(() => { throw new Error('fixture boom'); })).rejects.toThrow('fixture boom');
    await expect(page.evaluate('throw "string boom"')).rejects.toThrow('string boom');
    await expect(page.$eval('#missing', element => element.id)).rejects.toThrow('#missing');
    await expect(page.evaluate(() => new Promise(() => {}))).rejects.toBeInstanceOf(TimeoutError);
  }, 10_000);

  it('waits for present, visible, hidden & missing selectors; predicates poll by RAF & interval', async () => {
    await (await page.waitForSelector('#hidden'))!.dispose();
    expect(await page.waitForSelector('#hidden', { hidden: true })).toBeNull();
    expect(await page.waitForSelector('#missing', { hidden: true })).toBeNull();
    await page.evaluate(() => { setTimeout(() => { document.querySelector<HTMLElement>('#later')!.hidden = false; }, 40); });
    await (await page.waitForSelector('#later', { visible: true }))!.dispose();
    await page.waitForFunction((text: string) => document.querySelector('#later')?.textContent === text, { polling: 'raf' }, 'Later');
    await page.evaluate(() => { setTimeout(() => { (window as any).ready = true; }, 40); });
    await page.waitForFunction('window.ready', { polling: 10 });
    await expect(page.waitForSelector('#missing', { timeout: 60 })).rejects.toThrow(/Selector #missing/);
    await expect(page.waitForFunction(() => false, { timeout: 60 })).rejects.toThrow(/Predicate.*false/);
  });

  it('creates, evaluates, measures & disposes handles; navigation invalidates references', async () => {
    expect(await page.$('#missing')).toBeNull();
    const handles = await page.$$('.item');
    expect(handles).toHaveLength(2);
    expect(await handles[1].evaluate(element => element.textContent)).toBe('two');
    expect(await handles[0].boundingBox()).toEqual(await handles[0].evaluate(element => {
      const { x, y, width, height } = element.getBoundingClientRect(); return { x, y, width, height };
    }));
    await handles[0].dispose(); await handles[0].dispose();
    await expect(handles[0].evaluate(element => element.id)).rejects.toThrow('disposed');
    const hidden = (await page.$('#hidden'))!;
    expect(await hidden.boundingBox()).toBeNull(); await hidden.dispose();
    await page.reload();
    await expect(handles[1].evaluate(element => element.id)).rejects.toThrow('navigation');
    expect(await page.evaluate('document.title')).toBe('QaPage fixture');
  });

  it('clicks centres after scrolling, supports handles, double/right/middle clicks & trusted events', async () => {
    await page.click('#click', { clickCount: 2, delay: 5 });
    expect(await page.$eval('#click', element => element.textContent)).toBe('Clicked');
    const far = (await page.$('#far'))!;
    await page.click(far);
    expect(await far.evaluate(element => element.textContent)).toBe('Far clicked');
    expect(await page.evaluate('scrollY')).toBeGreaterThan(0);
    await far.click({ button: 'right' }); await far.click({ button: 'middle' }); await far.dispose();
    const events = await page.evaluate('window.events');
    expect(events).toEqual(expect.arrayContaining([
      expect.objectContaining({ type: 'dblclick', id: 'click', trusted: true }),
      expect.objectContaining({ type: 'click', id: 'far', trusted: true }),
      expect.objectContaining({ type: 'contextmenu', id: 'far', trusted: true }),
    ]));
  });

  it('types Unicode, presses editing/navigation keys & modifiers, hovers & focuses through page/handles', async () => {
    await page.type('#text', 'hello', { delay: 2 });
    await page.keyboard.press('Backspace'); await page.keyboard.press('End'); await page.keyboard.press('Space');
    await page.keyboard.press('Shift+a'); await page.keyboard.press('1');
    expect(await page.$eval('#text', element => (element as HTMLInputElement).value)).toBe('hell A1');
    await page.keyboard.press('Home'); await page.keyboard.press('Delete');
    expect(await page.$eval('#text', element => (element as HTMLInputElement).value)).toBe('ell A1');
    await page.keyboard.press('Escape');
    await (await page.waitForSelector('#hidden', { visible: true }))!.dispose();
    const text = (await page.$('#text'))!;
    await text.type('🙂', { delay: 2 }); await text.focus();
    expect(await text.evaluate(element => element === document.activeElement)).toBe(true);
    await page.keyboard.press('Tab');
    expect(await page.evaluate('document.activeElement.id')).toBe('next');
    await page.focus('#text');
    await page.keyboard.press('Shift+ArrowLeft');
    await page.keyboard.press('Enter');
    const hover = (await page.$('#hover'))!;
    await page.hover('#hover'); await hover.hover();
    expect(await hover.evaluate(element => element.textContent)).toBe('Hovered');
    const events = await page.evaluate('window.events');
    expect(events).toEqual(expect.arrayContaining([
      expect.objectContaining({ type: 'input', id: 'text', trusted: true }),
      expect.objectContaining({ type: 'keydown', id: 'text', key: 'Escape', trusted: true }),
      expect.objectContaining({ type: 'mouseover', id: 'hover', trusted: true }),
    ]));
    expect(await text.evaluate(element => (element as HTMLInputElement).value)).toContain('🙂');
    await text.dispose(); await hover.dispose();
  });

  it('attaches to the running page by target id, moves & holds the mouse, and detaches without closing it', async () => {
    const attached = await QaBrowserSession.attach({ port: chrome.port, timeout: 5000 });
    try {
      expect((await attached.pages()).map(info => info.targetId)).toContain(page.targetId);
      const view = await attached.attachPage(page.targetId);
      expect(await view.evaluate('location.href')).toContain(url);
      await page.evaluate(() => {
        const log: string[] = [];
        (window as any).pointer = log;
        for (const type of ['mousedown', 'mousemove', 'mouseup']) document.addEventListener(type, (event: MouseEvent) => log.push(`${type}:${event.buttons}`));
      });
      const centre = await page.$eval('#hover', element => {
        const { x, y, width, height } = element.getBoundingClientRect(); return { x: x + width / 2, y: y + height / 2 };
      });
      await view.mouse.move(centre.x, centre.y, { steps: 3 });
      expect(await page.$eval('#hover', element => element.textContent)).toBe('Hovered');
      await view.mouse.down();
      await view.mouse.move(centre.x + 4, centre.y);
      await view.mouse.up();
      const log = (await page.evaluate('window.pointer')) as string[];
      expect(log.filter(entry => entry !== 'mousemove:0')).toEqual(['mousedown:1', 'mousemove:1', 'mouseup:0']);
      await view.close();
      expect(await page.evaluate('document.title')).toBe('QaPage fixture');
      expect((await attached.pages()).map(info => info.targetId)).toContain(page.targetId);
    } finally { await attached.close(); }
    expect(await page.evaluate('1 + 1')).toBe(2);
  });

  it('selects single/multiple values & dispatches input/change with empty selection support', async () => {
    expect(await page.select('#select', 'two', 'one')).toEqual(['one']);
    expect(await page.select('#select', 'two')).toEqual(['two']);
    expect(await page.$eval('#select', element => (element as HTMLSelectElement).value)).toBe('two');
    expect(await page.select('#select', 'missing')).toEqual([]);
    expect(await page.$eval('#select', element => (element as HTMLSelectElement).selectedIndex)).toBe(-1);
    expect(await page.select('#multi', 'one', 'two')).toEqual(['one', 'two']);
    expect(await page.select('#multi')).toEqual([]);
    const events = await page.evaluate('window.events');
    expect(events).toEqual(expect.arrayContaining([
      expect.objectContaining({ type: 'input', id: 'select' }), expect.objectContaining({ type: 'change', id: 'multi' }),
    ]));
    await expect(page.select('#text', 'one')).rejects.toThrow('not a select');
  });

  it('captures viewport PNG at expected DPR size & writes identical bytes', async () => {
    const file = path.join(output, 'viewport.png');
    const png = await page.screenshot({ path: file, type: 'png', captureBeyondViewport: false, fullPage: false });
    expect(Buffer.isBuffer(png)).toBe(true);
    expect(png.subarray(0, 8)).toEqual(Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]));
    expect([png.readUInt32BE(16), png.readUInt32BE(20)]).toEqual([1280, 960]);
    expect(await readFile(file)).toEqual(png);
  });
});
