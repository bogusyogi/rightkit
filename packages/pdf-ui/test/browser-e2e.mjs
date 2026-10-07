// Real-browser smoke test for pdf.js rendering plus doc-render XSS boundaries, on Chrome for
// Testing (@rightkit/qa). Always muted; no media APIs are used.
import { findChromeForTesting, launchChromeForTesting } from "@rightkit/qa/chrome";
import { createServer } from "node:http";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { buildSync } from "esbuild";

const packageRoot = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const outDir = path.join(packageRoot, "test", ".e2e-out");
mkdirSync(outDir, { recursive: true });
buildSync({ entryPoints: [path.join(packageRoot, "test", "harness.tsx")], bundle: true, format: "esm", jsx: "automatic", outfile: path.join(outDir, "harness.js"), loader: { ".css": "empty", ".wasm": "file" }, logLevel: "silent" });
const pdfWorker = path.join(packageRoot, "node_modules", "pdfjs-dist", "build", "pdf.worker.min.mjs");
const fixtures = path.join(packageRoot, "test", "fixtures");
const pdfCss = path.join(packageRoot, "src", "styles.css");
const docCss = path.join(packageRoot, "..", "doc-render", "src", "styles.css");
const server = createServer((req, res) => {
  const url = new URL(req.url ?? "/", "http://127.0.0.1").pathname;
  if (url === "/") { res.setHeader("content-type", "text/html"); return res.end('<!doctype html><meta charset="utf-8"><link rel="stylesheet" href="/pdf-ui.css"><link rel="stylesheet" href="/doc-render.css"><div id="root"></div><script type="module" src="/harness.js"></script>'); }
  if (url === "/harness.js") { res.setHeader("content-type", "text/javascript"); return res.end(readFileSync(path.join(outDir, "harness.js"))); }
  if (url === "/pdf-ui.css") { res.setHeader("content-type", "text/css"); return res.end(readFileSync(pdfCss)); }
  if (url === "/doc-render.css") { res.setHeader("content-type", "text/css"); return res.end(readFileSync(docCss)); }
  if (url === "/pdf.worker.min.mjs") { res.setHeader("content-type", "text/javascript"); return res.end(readFileSync(pdfWorker)); }
  if (url === "/fixtures/c:1" || url === "/fixtures/c") { res.setHeader("content-type", "application/pdf"); return res.end(readFileSync(path.join(fixtures, url === "/fixtures/c:1" ? "report.pdf" : "memo.pdf"))); }
  if (url.startsWith("/fixtures/")) { const file = path.join(fixtures, path.basename(url)); if (existsSync(file)) { res.setHeader("content-type", "application/pdf"); return res.end(readFileSync(file)); } }
  res.statusCode = 404; res.end();
});
await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
const port = server.address().port;
const cft = findChromeForTesting();
if (!cft.path) { console.log(`SKIP: ${cft.reason}`); server.close(); process.exit(0); }
const browser = await launchChromeForTesting({ executablePath: cft.path, profilePrefix: "rk-pdf-ui-cft-" });
const ws = new WebSocket(browser.wsEndpoint);
await new Promise((resolve, reject) => { ws.onopen = resolve; ws.onerror = reject; });
let sequence = 0; const pending = new Map();
ws.onmessage = ({ data }) => { const message = JSON.parse(data); const callback = pending.get(message.id); if (callback) { pending.delete(message.id); callback(message); } };
const send = (method, params = {}, sessionId) => new Promise((resolve, reject) => { const id = ++sequence; pending.set(id, (message) => message.error ? reject(new Error(message.error.message)) : resolve(message.result)); ws.send(JSON.stringify({ id, method, params, sessionId })); });
const { targetId } = await send("Target.createTarget", { url: `http://127.0.0.1:${port}/` });
const { sessionId } = await send("Target.attachToTarget", { targetId, flatten: true });
await send("Runtime.enable", {}, sessionId); await send("Page.enable", {}, sessionId);
const evaluate = async (expression) => { const result = await send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true }, sessionId); if (result.exceptionDetails) throw new Error(result.exceptionDetails.text); return result.result.value; };
const until = async (expression, label, timeout = 20000) => { const deadline = Date.now() + timeout; while (Date.now() < deadline) { if (await evaluate(expression)) return; await new Promise((resolve) => setTimeout(resolve, 150)); } throw new Error(`timeout: ${label}`); };
try {
  await until("window.__harness === true", "harness");
  await until("document.querySelectorAll('.rk-pdf-page canvas').length >= 1", "first PDF canvas");
  await until("document.querySelectorAll('.rk-pdf-page').length === 3", "all PDF slots");
  await until("document.querySelectorAll('.rk-pdf-page .textLayer span').length > 0", "PDF text layer");
  await until("document.querySelector('button[aria-label=\\\"Next page\\\"]')?.disabled === false", "page navigation");
  await evaluate("document.querySelector('button[aria-label=\\\"Next page\\\"]').click()");
  await until("document.querySelector('input[aria-label=\\\"Page number\\\"]').value === '2'", "navigate to page 2");
  const zoomBefore = await evaluate("Number(document.querySelector('input[aria-label=\\\"Zoom percentage\\\"]')?.value || 0)");
  await evaluate("document.querySelector('button[aria-label=\\\"Zoom in\\\"]').click()");
  await until(`Number(document.querySelector('input[aria-label=\\\"Zoom percentage\\\"]').value) > ${zoomBefore}`, "zoom in");
  await evaluate("(() => { const input = document.querySelector('input[aria-label=\\\"Find in PDF\\\"]'); input.focus(); input.select(); })()");
  await send("Input.insertText", { text: "Quarterly" }, sessionId);
  await evaluate("document.querySelector('input[aria-label=\\\"Find in PDF\\\"]').form.requestSubmit()");
  await until("document.querySelector('.rk-pdf-find button[type=submit]') && !document.querySelector('.rk-pdf-find button:last-child').disabled", "find result");
  await evaluate("[...document.querySelectorAll('.rk-pdf-toolbar > button')].find((button) => button.textContent === 'Pages')?.click()");
  await until("document.querySelector('.rk-pdf-thumbs button[aria-label=\\\"Page 3\\\"]') !== null", "thumbnail panel");
  await evaluate("document.querySelector('.rk-pdf-thumbs button[aria-label=\\\"Page 3\\\"]').click()");
  await until("document.querySelector('input[aria-label=\\\"Page number\\\"]').value === '3'", "thumbnail navigation");
  // usePdfSearch from a consumer toolbar control reaches the viewer's search controller.
  await evaluate("(() => { const input = document.querySelector('#external-find-input'); input.focus(); input.select(); })()");
  await send("Input.insertText", { text: "Quarterly" }, sessionId);
  await evaluate("document.querySelector('#external-find-go').click()");
  await until("/^[1-9]\\d*:0$/.test(document.querySelector('#external-find-result').textContent)", "external find result via usePdfSearch");
  const externalFind = await evaluate("document.querySelector('#external-find-result').textContent");
  await evaluate("document.querySelector('#external-find-next').click()");
  await until("document.querySelector('#external-find-result').textContent.split(':')[1] !== '0' || document.querySelector('#external-find-result').textContent.startsWith('1:')", "external find next");
  await evaluate("document.querySelector('#replace-source').click()");
  await until("document.querySelectorAll('.rk-pdf-page').length === 1", "source replacement");
  await until("(window.__loadingEvents || []).filter(Boolean).length >= 2", "source cleanup/loading lifecycle");
  await evaluate("document.querySelector('#replace-source').click()");
  await until("document.querySelectorAll('.rk-pdf-page').length === 3", "source restore");
  await until("(window.__loadingEvents || []).length >= 4", "restored loading lifecycle");
  const checks = await evaluate(`(() => ({
    pages: document.querySelectorAll('.rk-pdf-page').length,
    canvases: document.querySelectorAll('.rk-pdf-page canvas').length,
    text: document.querySelectorAll('.rk-pdf-page .textLayer span').length,
    scripts: document.querySelectorAll('#doc-fixtures script').length,
    handlers: [...document.querySelectorAll('#doc-fixtures *')].some((n) => [...n.attributes].some((a) => /^on/i.test(a.name))),
    unsafeHref: [...document.querySelectorAll('#doc-fixtures a')].some((a) => /^javascript:/i.test(a.getAttribute('href') || '')),
    safeHref: [...document.querySelectorAll('#doc-fixtures a')].some((a) => a.href === 'https://example.com/')
    ,zoom: document.querySelector('input[aria-label="Zoom percentage"]')?.value
    ,loadingEvents: (window.__loadingEvents || []).length
  }))()`);
  if (checks.pages !== 3 || checks.canvases < 1 || checks.text < 1 || checks.scripts !== 0 || checks.handlers || checks.unsafeHref || !checks.safeHref || Number(checks.zoom) <= zoomBefore || checks.loadingEvents < 4) throw new Error(`browser assertions failed: ${JSON.stringify(checks)}`);
  const screenshot = await send("Page.captureScreenshot", { format: "png", captureBeyondViewport: true }, sessionId);
  writeFileSync(path.join(outDir, "pdf-viewer.png"), Buffer.from(screenshot.data, "base64"));
  // Replacing a PDF with a different same-sized PDF (bytes, then identical-text loader closures) shows the new one.
  await send("Page.navigate", { url: `http://127.0.0.1:${port}/?case=replace` }, sessionId);
  await until("window.__sizes && document.querySelectorAll('.rk-pdf-page .textLayer span').length > 0", "replace harness first doc");
  const sizes = await evaluate("window.__sizes");
  if (sizes.alpha !== sizes.omega) throw new Error(`fixtures must be same size: ${JSON.stringify(sizes)}`);
  const pageText = () => evaluate("[...document.querySelectorAll('.rk-pdf-page .textLayer')].map((n) => n.textContent).join('|')");
  await until("document.querySelector('.rk-pdf-page .textLayer')?.textContent.includes('ALPHA')", "bytes ALPHA shown");
  await evaluate("document.querySelector('#swap-bytes').click()");
  await until("document.querySelector('.rk-pdf-page .textLayer')?.textContent.includes('OMEGA')", "bytes replaced with same-sized OMEGA");
  const bytesText = await pageText();
  if (bytesText.includes("ALPHA")) throw new Error(`stale bytes document: ${bytesText}`);
  const bytesShot = await send("Page.captureScreenshot", { format: "png" }, sessionId);
  writeFileSync(path.join(outDir, "pdf-replace-bytes.png"), Buffer.from(bytesShot.data, "base64"));
  await evaluate("document.querySelector('#mode-loader').click()");
  await until("document.querySelector('.rk-pdf-page .textLayer')?.textContent.includes('ALPHA')", "loader ALPHA shown");
  await evaluate("document.querySelector('#swap-loader').click()");
  await until("document.querySelector('.rk-pdf-page .textLayer')?.textContent.includes('OMEGA')", "loader replaced with identical-text closure OMEGA");
  const loaderText = await pageText();
  if (loaderText.includes("ALPHA")) throw new Error(`stale loader document: ${loaderText}`);
  // URL/revision pairs that concatenate identically ('a:1'+'2' vs 'a'+'1:2') still load the new document.
  await send("Page.navigate", { url: `http://127.0.0.1:${port}/?case=collide` }, sessionId);
  await until("document.querySelectorAll('.rk-pdf-page').length === 3", "collide first doc (report, 3 pages)");
  await evaluate("document.querySelector('#swap-collide').click()");
  await until("document.querySelectorAll('.rk-pdf-page').length === 1", "colliding URL/revision shows the new document (memo, 1 page)");
  console.log(`external-find=${externalFind} replace=bytes+loader(size ${sizes.alpha})`);
  console.log(`PASS: pdf pages=${checks.pages}, canvases=${checks.canvases}, textSpans=${checks.text}, navigation=page+thumbs, zoom=${zoomBefore}->${checks.zoom}, find=Quarterly, source-replace=clean, sanitizer=clean`);
} finally {
  ws.close();
  const closed = await browser.close();
  server.close();
  if (!closed.exited || !closed.profileRemoved) throw new Error(`Chrome for Testing did not close cleanly: ${JSON.stringify(closed)}`);
}
