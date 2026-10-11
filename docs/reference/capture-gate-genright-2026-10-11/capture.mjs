// Hidden headless Chrome capture via CDP. Usage: node capture.mjs <url> <outDir>
import { spawn } from 'node:child_process';
import { mkdirSync, writeFileSync, existsSync, rmSync } from 'node:fs';
import { join } from 'node:path';

const [,, url, outDir] = process.argv;
const scratch = '/private/tmp/claude-501/-Volumes-D-claude-genright/fa1450cb-0be5-40b4-8c64-7c625a35aa51/scratchpad';
const port = 9333 + Math.floor(Math.random() * 500);
const profile = join(scratch, 'chrome-profile-' + port);
rmSync(outDir, { recursive: true, force: true });
mkdirSync(outDir, { recursive: true });
const chrome = spawn('/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', [
  '--headless=new', `--remote-debugging-port=${port}`, `--user-data-dir=${profile}`,
  '--no-first-run', '--no-default-browser-check', '--disable-gpu', '--hide-scrollbars',
  '--disable-features=Translate', 'about:blank',
], { stdio: 'ignore', detached: false });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let targets;
for (let i = 0; i < 100; i++) {
  try { targets = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json(); if (targets.find((t) => t.type === 'page')) break; } catch {}
  await sleep(150);
}
const page = targets.find((t) => t.type === 'page');
const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise((r, j) => { ws.onopen = r; ws.onerror = j; });
let id = 0; const pending = new Map(); const logs = [];
ws.onmessage = (ev) => {
  const msg = JSON.parse(ev.data);
  if (msg.id && pending.has(msg.id)) { const { res, rej } = pending.get(msg.id); pending.delete(msg.id); msg.error ? rej(new Error(JSON.stringify(msg.error))) : res(msg.result); }
  else if (msg.method === 'Runtime.exceptionThrown') logs.push('EXC ' + JSON.stringify(msg.params.exceptionDetails.exception?.description ?? msg.params.exceptionDetails.text));
};
const send = (method, params = {}) => new Promise((res, rej) => { const i = ++id; pending.set(i, { res, rej }); ws.send(JSON.stringify({ id: i, method, params })); });
const evalJs = async (expression) => (await send('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true })).result.value;
await send('Page.enable'); await send('Runtime.enable');
await send('Emulation.setDeviceMetricsOverride', { width: 1280, height: 800, deviceScaleFactor: 1, mobile: false });
await send('Emulation.setEmulatedMedia', { media: 'screen', features: [{ name: 'prefers-reduced-motion', value: 'reduce' }] });
await send('Page.addScriptToEvaluateOnNewDocument', { source: `(() => { const FIXED = Date.parse('2026-10-11T12:00:00Z'); const Real = Date; class FixedDate extends Real { constructor(...a) { if (a.length === 0) super(FIXED); else super(...a); } static now() { return FIXED; } } globalThis.Date = FixedDate; })();` });
await send('Page.navigate', { url });
const waitFor = async (expr, ms = 20000) => { const end = Date.now() + ms; while (Date.now() < end) { if (await evalJs(expr)) return true; await sleep(200); } return false; };
await waitFor("document.body && document.body.innerText.includes('GenRight')");
const css = `*,*::before,*::after{animation:none!important;transition:none!important;caret-color:transparent!important}`;
await evalJs(`(() => { const s = document.createElement('style'); s.id='qa-freeze'; s.textContent = ${JSON.stringify(css)}; document.head.appendChild(s); return true; })()`);
const labels = await evalJs(`[...document.querySelectorAll('nav button')].map(b => b.textContent.trim())`);
const shots = [];
const shot = async (name) => { await sleep(1500); const { data } = await send('Page.captureScreenshot', { format: 'png', captureBeyondViewport: false }); const file = join(outDir, name + '.png'); writeFileSync(file, Buffer.from(data, 'base64')); shots.push(name); };
await shot('00-initial');
for (let i = 0; i < (labels ?? []).length; i++) {
  const label = labels[i];
  const slug = label.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '') || 'item';
  const clicked = await evalJs(`(() => { const b = [...document.querySelectorAll('nav button')][${i}]; if (!b) return false; b.click(); return true; })()`);
  if (!clicked) continue;
  await sleep(700);
  await shot(String(i + 1).padStart(2, '0') + '-' + slug);
}
writeFileSync(join(outDir, 'manifest.json'), JSON.stringify({ url, labels, shots, logs }, null, 2));
ws.close();
const exited = new Promise((r) => chrome.on('exit', r));
chrome.kill('SIGTERM');
await Promise.race([exited, sleep(5000)]);
await sleep(500);
try { rmSync(profile, { recursive: true, force: true }); } catch {}
console.log(JSON.stringify({ shots: shots.length, labels, errors: logs.length }));
process.exit(0);
