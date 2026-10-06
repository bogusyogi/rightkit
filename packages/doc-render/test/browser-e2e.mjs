// Real Chrome sanitizer boundary test. Chrome is launched muted for suite consistency.
import { spawn } from "node:child_process";
import { createServer } from "node:http";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { buildSync } from "esbuild";
import { createRequire } from "node:module";

const root = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const out = path.join(root, "test", ".e2e-out"); mkdirSync(out, { recursive: true });
buildSync({ entryPoints: [path.join(root, "test", "harness.tsx")], bundle: true, format: "esm", jsx: "automatic", outfile: path.join(out, "harness.js"), loader: { ".css": "empty" }, logLevel: "silent" });
// Real SSR output: bundled for node (no `document`), so the sanitizer must fail closed.
buildSync({ entryPoints: [path.join(root, "test", "ssr.tsx")], bundle: true, platform: "node", format: "cjs", jsx: "automatic", outfile: path.join(out, "ssr.cjs"), loader: { ".css": "empty" }, logLevel: "silent" });
const ssrHtml = createRequire(import.meta.url)(path.join(out, "ssr.cjs")).renderSsr();
if (/<img|<svg|<details|<script/i.test(ssrHtml.replace(/&lt;[^]*?&gt;/g, ""))) throw new Error("SSR output contains live raw HTML tags");
const page = `<!doctype html><meta charset="utf-8"><div id="root" data-ssr="1">${ssrHtml}</div><script>window.__pre={handlers:[...document.querySelectorAll('*')].some((n)=>[...n.attributes].some((a)=>/^on/i.test(a.name))),imgs:document.querySelectorAll('img,svg,details').length};</script><script type="module" src="/harness.js"></script>`;
const server = createServer((req, res) => { if (new URL(req.url ?? "/", "http://x").pathname === "/harness.js") { res.setHeader("content-type", "text/javascript"); res.end(readFileSync(path.join(out, "harness.js"))); } else { res.setHeader("content-type", "text/html"); res.end(page); } });
await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
const chrome = [process.env.RIGHTKIT_CHROME, "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome", "/Applications/Chromium.app/Contents/MacOS/Chromium", "C:/Program Files/Google/Chrome/Application/chrome.exe"].filter(Boolean).find((candidate) => existsSync(candidate));
if (!chrome) { console.log("SKIP: no Chrome binary found"); server.close(); process.exit(0); }
const proc = spawn(chrome, ["--headless=new", "--mute-audio", "--no-first-run", "--remote-debugging-port=0", `--user-data-dir=${path.join(out, "profile")}`, "about:blank"], { stdio: ["ignore", "ignore", "pipe"] });
const wsUrl = await new Promise((resolve, reject) => { let text = ""; const timer = setTimeout(() => reject(new Error("Chrome DevTools timeout")), 15000); proc.stderr.on("data", (chunk) => { text += chunk; const match = text.match(/DevTools listening on (ws:\/\/\S+)/); if (match) { clearTimeout(timer); resolve(match[1]); } }); proc.on("exit", () => reject(new Error("Chrome exited early"))); });
const ws = new WebSocket(wsUrl); await new Promise((resolve, reject) => { ws.onopen = resolve; ws.onerror = reject; }); let seq = 0; const pending = new Map(); ws.onmessage = ({ data }) => { const message = JSON.parse(data); const callback = pending.get(message.id); if (callback) { pending.delete(message.id); callback(message); } };
const send = (method, params = {}, sessionId) => new Promise((resolve, reject) => { const id = ++seq; pending.set(id, (message) => message.error ? reject(new Error(message.error.message)) : resolve(message.result)); ws.send(JSON.stringify({ id, method, params, sessionId })); });
const { targetId } = await send("Target.createTarget", { url: `http://127.0.0.1:${server.address().port}/` }); const { sessionId } = await send("Target.attachToTarget", { targetId, flatten: true }); await send("Runtime.enable", {}, sessionId);
const evaluate = async (expression) => { const result = await send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true }, sessionId); return result.result.value; };
try { for (let i = 0; i < 100 && !(await evaluate("window.__harness === true")); i++) await new Promise((resolve) => setTimeout(resolve, 100)); await new Promise((resolve) => setTimeout(resolve, 500)); const result = await evaluate(`(() => ({ pwned: window.__pwned === 1, pre: window.__pre, scripts: document.querySelectorAll('script').length - 2, handlers: [...document.querySelectorAll('*')].some((n) => [...n.attributes].some((a) => /^on/i.test(a.name))), unsafe: [...document.querySelectorAll('a')].some((a) => /^javascript:/i.test(a.getAttribute('href') || '')), safe: [...document.querySelectorAll('a')].some((a) => a.href === 'https://example.com/') }))()`); if (result.pwned || result.pre.handlers || result.pre.imgs !== 0 || result.scripts !== 0 || result.handlers || result.unsafe || !result.safe) throw new Error(`XSS assertion failed: ${JSON.stringify(result)}`); const screenshot = await send("Page.captureScreenshot", { format: "png", captureBeyondViewport: true }, sessionId); writeFileSync(path.join(out, "doc-render.png"), Buffer.from(screenshot.data, "base64")); console.log("PASS: doc-render browser sanitizer assertions"); } finally { ws.close(); proc.kill("SIGTERM"); server.close(); }
