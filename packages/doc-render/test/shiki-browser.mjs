// Owner-only: pnpm exec node test/shiki-browser.mjs (after QA Chrome dependency build).
import { findChromeForTesting, launchChromeForTesting } from "@rightkit/qa-chrome";
import { buildSync } from "esbuild";
import { readFileSync, mkdirSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const out = process.env.RIGHTKIT_SHIKI_EVIDENCE_DIR ?? (process.platform === "win32" ? "D:/scratch/doc-render-shiki" : "/Volumes/D/scratch/doc-render-shiki");
mkdirSync(out, { recursive: true });
const bundle = buildSync({ entryPoints: [path.join(root, "test/shiki-browser.ts")], bundle: true, format: "iife", globalName: "shikiChecks", write: false, logLevel: "silent" }).outputFiles[0].text;
const css = readFileSync(path.join(root, "shiki.css"), "utf8");
const cft = findChromeForTesting();
if (!cft.path) throw new Error(`Chrome for Testing required: ${cft.reason}`);
const browser = await launchChromeForTesting({ executablePath: cft.path, profilePrefix: "rk-shiki-cft-" });
const ws = new WebSocket(browser.wsEndpoint);
await new Promise((resolve, reject) => { ws.onopen = resolve; ws.onerror = reject; });
let seq = 0;
const pending = new Map();
const requests = [];
let observing = false;
ws.onmessage = ({ data }) => {
  const message = JSON.parse(data);
  if (observing && message.method === "Network.requestWillBeSent") requests.push(message.params.request.url);
  const callback = pending.get(message.id);
  if (callback) { pending.delete(message.id); callback(message); }
};
const send = (method, params = {}, sessionId) => new Promise((resolve, reject) => {
  const id = ++seq;
  pending.set(id, (message) => message.error ? reject(new Error(message.error.message)) : resolve(message.result));
  ws.send(JSON.stringify({ id, method, params, sessionId }));
});
try {
  const { targetId } = await send("Target.createTarget", { url: "about:blank" });
  const { sessionId } = await send("Target.attachToTarget", { targetId, flatten: true });
  await send("Runtime.enable", {}, sessionId);
  await send("Network.enable", {}, sessionId);
  // Block outbound attempts while retaining Network.requestWillBeSent evidence.
  await send("Network.setBlockedURLs", { urls: ["http://*", "https://*", "file://*"] }, sessionId);
  const evaluate = async (expression) => {
    const result = await send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true }, sessionId);
    if (result.exceptionDetails) throw new Error(JSON.stringify(result.exceptionDetails));
    return result.result.value;
  };
  await evaluate(`document.head.appendChild(Object.assign(document.createElement('style'), {textContent:${JSON.stringify(css)}})); ${bundle}`);
  observing = true;
  const result = await evaluate("shikiChecks.runShikiBrowserChecks()");
  await evaluate("new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)))");
  const pwned = await evaluate("window.__shikiPwned === 1");
  if (pwned || requests.length) throw new Error(`Execution/egress: ${JSON.stringify({ pwned, requests })}`);
  for (const theme of ["light", "dark"]) {
    await evaluate(`document.querySelector('main').dataset.theme=${JSON.stringify(theme)}`);
    const shot = await send("Page.captureScreenshot", { format: "png", captureBeyondViewport: true }, sessionId);
    writeFileSync(path.join(out, `shiki-${theme}.png`), Buffer.from(shot.data, "base64"));
  }
  writeFileSync(path.join(out, "result.json"), JSON.stringify({ ...result, requests, pwned }, null, 2));
  console.log("PASS: Shiki parse-entry→mount zero egress/execution, source fallback, scoped fonts & themes");
} finally {
  ws.close();
  const closed = await browser.close();
  if (!closed.exited || !closed.profileRemoved) throw new Error(`Chrome cleanup failed: ${JSON.stringify(closed)}`);
}
