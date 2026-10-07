// Headless-Chrome e2e for @rightkit/media-ui: real media decode/playback,
// compat-fallback, subtitles, single-active-player exclusivity, and a real
// canvas export round-trip (crop + rotate + text → pixel check). Chrome for Testing is
// launched with --mute-audio — nothing reaches the speakers.
//
// Requires Chrome for Testing (see @rightkit/qa resolveChromeForTesting). Optional env:
//   RIGHTKIT_CHROME_FOR_TESTING=/path/to/chrome-for-testing   override detection
//   MEDIA_UI_FFMPEG=/path/to/ffmpeg   fixture regeneration (else reuse committed fixtures)
import { execFileSync } from "node:child_process";
import { findChromeForTesting, launchChromeForTesting } from "@rightkit/qa/chrome";
import { createServer } from "node:http";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const outDir = path.join(root, "test", ".e2e-out");
mkdirSync(outDir, { recursive: true });
const fixtures = path.join(root, "test", "fixtures");

// ── Fixtures ──────────────────────────────────────────────────────────────
const ffmpeg = process.env.MEDIA_UI_FFMPEG ?? "ffmpeg";
function ensureFixture(name, args) {
  const target = path.join(fixtures, name);
  if (existsSync(target)) return target;
  try {
    execFileSync(ffmpeg, args.concat(target), { stdio: "pipe" });
  } catch {
    throw new Error(`fixture ${name} missing and ffmpeg (${ffmpeg}) failed`);
  }
  return target;
}
ensureFixture("clip.mp4", [
  "-y", "-f", "lavfi", "-i", "testsrc=duration=3:size=320x180:rate=15",
  "-f", "lavfi", "-i", "sine=frequency=440:duration=3",
  "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest",
]);
ensureFixture("tone.wav", ["-y", "-f", "lavfi", "-i", "sine=frequency=330:duration=5"]);
ensureFixture("picture.png", ["-y", "-f", "lavfi", "-i", "color=c=red:size=96x64", "-frames:v", "1"]);
writeFileSync(
  path.join(fixtures, "subs.vtt"),
  "WEBVTT\n\n00:00.000 --> 00:02.500\nhello subtitles\n",
);

// ── Bundle the harness (dist output + real deps, TSX → ESM) ───────────────
const { buildSync } = await import("esbuild");
buildSync({
  entryPoints: [path.join(root, "test", "harness.tsx")],
  bundle: true,
  format: "esm",
  jsx: "automatic",
  outfile: path.join(outDir, "harness.js"),
  loader: { ".css": "empty" },
  // Development React, as every app's dev server runs it: StrictMode then
  // really mounts → cleans up → remounts effects, which production skips.
  define: { "process.env.NODE_ENV": '"development"' },
  logLevel: "silent",
});

const html = `<!doctype html><html><head><meta charset="utf-8"></head>
<body><div id="root"></div><script type="module" src="/harness.js"></script></body></html>`;

const server = createServer((req, res) => {
  const url = new URL(req.url, "http://x").pathname;
  if (url === "/") {
    res.end(html);
  } else if (url === "/harness.js") {
    res.setHeader("content-type", "text/javascript");
    res.end(readFileSync(path.join(outDir, "harness.js")));
  } else if (url.startsWith("/fixtures/")) {
    const file = path.join(fixtures, decodeURIComponent(url.slice(10)));
    if (!file.startsWith(fixtures) || !existsSync(file)) {
      res.statusCode = 404;
      return res.end();
    }
    // Byte-range support so Chrome can seek.
    const data = readFileSync(file);
    const range = req.headers.range?.match(/bytes=(\d+)-(\d*)/);
    if (range) {
      const start = Number(range[1]);
      const end = range[2] ? Number(range[2]) : data.length - 1;
      res.statusCode = 206;
      res.setHeader("content-range", `bytes ${start}-${end}/${data.length}`);
      res.setHeader("accept-ranges", "bytes");
      return res.end(data.subarray(start, end + 1));
    }
    res.setHeader("accept-ranges", "bytes");
    res.end(data);
  } else {
    res.statusCode = 404;
    res.end();
  }
});
await new Promise((r) => server.listen(Number(process.env.MEDIA_UI_PORT ?? 0), "127.0.0.1", r));
const port = server.address().port;

// ── Chrome for Testing (@rightkit/qa; RIGHTKIT_CHROME_FOR_TESTING overrides) ─
const cft = findChromeForTesting();
if (!cft.path) {
  console.error(`SKIP: ${cft.reason}`);
  server.close();
  process.exit(0);
}
const browser = await launchChromeForTesting({
  executablePath: cft.path,
  profilePrefix: "rk-media-ui-cft-",
  args: ["--autoplay-policy=no-user-gesture-required"],
});
const wsUrl = browser.wsEndpoint;

// ── Minimal CDP client ────────────────────────────────────────────────────
const ws = new WebSocket(wsUrl);
await new Promise((r) => (ws.onopen = r));
let seq = 0;
const pending = new Map();
ws.onmessage = (e) => {
  const msg = JSON.parse(e.data);
  if (msg.id && pending.has(msg.id)) {
    pending.get(msg.id)(msg);
    pending.delete(msg.id);
  }
};
const send = (method, params = {}, sessionId) =>
  new Promise((resolve, reject) => {
    const id = ++seq;
    pending.set(id, (msg) =>
      msg.error ? reject(new Error(`${method}: ${msg.error.message}`)) : resolve(msg.result),
    );
    ws.send(JSON.stringify({ id, method, params, sessionId }));
  });

const { targetId } = await send("Target.createTarget", { url: `http://127.0.0.1:${port}/` });
const { sessionId } = await send("Target.attachToTarget", { targetId, flatten: true });
await send("Runtime.enable", {}, sessionId);
await send("Page.enable", {}, sessionId);

const evalJson = async (expr) => {
  const { result, exceptionDetails } = await send(
    "Runtime.evaluate",
    { expression: expr, returnByValue: true, awaitPromise: true },
    sessionId,
  );
  if (exceptionDetails) throw new Error(`eval failed: ${exceptionDetails.text} ${expr.slice(0, 80)}`);
  return result.value;
};
const until = async (expr, timeout = 15000, what = expr) => {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    if (await evalJson(expr)) return;
    await new Promise((r) => setTimeout(r, 150));
  }
  throw new Error(`timeout waiting for ${what}`);
};

const failures = [];
const check = async (name, expr, timeout) => {
  try {
    await until(expr, timeout, name);
    console.log(`  ok    ${name}`);
  } catch (e) {
    failures.push(name);
    console.log(`  FAIL  ${name}: ${e.message}`);
  }
};

try {
  await until("!!window.__harness && !!document.querySelector('video')", 15000, "harness mount");

  // Before any interaction: a mounted player loads its source by itself, even
  // after StrictMode's mount → cleanup → remount stripped the element's src.
  await check("player loads its source untouched after StrictMode remount", `
    (() => { const a = document.querySelector('[data-testid=plain-player] audio');
      return !!a && !!a.getAttribute('src') && a.readyState >= 1 && a.duration > 4; })()`, 8000);

  // Video: play → currentTime advances → seek → ended.
  await check("video plays", `
    (() => { const v = document.querySelectorAll('video')[0]; v.play();
      return new Promise(r => setTimeout(() => r(v.currentTime > 0.1 && !v.paused), 800)); })()`);
  await check("video seeks", `
    (() => { const v = document.querySelectorAll('video')[0]; v.currentTime = 1.5;
      return new Promise(r => setTimeout(() => r(v.currentTime >= 1.4), 500)); })()`);
  await check("video ends", `
    (() => { const v = document.querySelectorAll('video')[0]; v.currentTime = v.duration - 0.05;
      return new Promise(r => setTimeout(() => r(v.ended), 1500)); })()`);
  await check("subtitle track registered", `
    (() => { const v = document.querySelectorAll('video')[0];
      return v.textTracks.length >= 1 && v.textTracks[0].mode === 'showing'; })()`);
  await check("audio player element exists and can play", `
    (() => { const a = document.querySelector('audio'); a.play();
      return new Promise(r => setTimeout(() => r(!a.paused), 500)); })()`);
  await check("single-active-player exclusivity", `
    (() => { const v = document.querySelectorAll('video')[0]; const a = document.querySelector('audio');
      v.currentTime = 0; const p1 = v.play().then(() => a.play());
      return p1.then(() => new Promise(r => setTimeout(() => r(v.paused && !a.paused), 500))); })()`);
  // Compat fallback: bad.mkv fails → preparePlayback returns clip.mp4 → plays.
  await check("compat fallback converts and plays", `
    (() => new Promise(r => { const t = setInterval(() => {
      // The player remounts its element when the compat source lands, so
      // re-query by container rather than holding a stale element.
      const wraps = [...document.querySelectorAll('.rk-media-video')];
      const v = wraps[wraps.length - 1];
      const wrap = v?.closest('.rk-media');
      if (v && wrap?.dataset.playerStatus === 'ready' && v.paused) v.play().catch(() => {});
      if (v && v.currentTime > 0.05 && !v.paused) { clearInterval(t); r(true); }
      if (wrap && wrap.dataset.playerStatus === 'error') { clearInterval(t); r(false); }
    }, 200); setTimeout(() => { clearInterval(t); r(false); }, 9000); }))()`, 12000);
  // Image export round-trip: crop 64×32 rotated 90° → 32×64 PNG decoded
  // in-page; text overlay must produce non-red pixels.
  await check("image export bakes crop+rotate+text (pixels verified)", `
    (() => new Promise(r => { const t = setInterval(() => {
      const res = window.__harness.exportResult;
      if (res) {
        clearInterval(t);
        r(res.ok && res.width === 32 && res.height === 64 && res.bytes > 300 && res.nonRedPixels > 10);
      }
    }, 150); setTimeout(() => r(false), 8000); }))()`);

  // H16: replacing src/kind on a mounted player must show the NEW media and
  // release the old element (no remount of the MediaPlayer itself).
  await check("source replacement shows new media and releases old element", `
    (() => new Promise(r => {
      const probe = () => document.querySelector('[data-probe=swap]');
      const t0 = setInterval(() => { const v = probe()?.querySelector('video');
        if (v && v.readyState >= 1) { clearInterval(t0); go(v); } }, 100);
      function go(oldVideo) {
        window.__swap({ src: '/fixtures/tone.wav', kind: 'audio' });
        const t = setInterval(() => { const a = probe()?.querySelector('audio');
          if (a && a.readyState >= 1) { clearInterval(t);
            r(a.currentSrc.endsWith('tone.wav') && !probe().querySelector('video') &&
              !oldVideo.isConnected && Math.abs(a.duration - 5) < 0.3 &&
              probe().querySelector('.rk-media').dataset.playerStatus === 'ready'); } }, 100);
        setTimeout(() => { clearInterval(t); r(false); }, 8000);
      }
      setTimeout(() => r(false), 12000);
    }))()`, 15000);
  // A compat copy still being prepared for the OLD source must not replace
  // the NEW source when it resolves late.
  await check("late compat copy of replaced source is discarded", `
    (() => new Promise(r => {
      window.__swap({ src: '/fixtures/missing-old.mkv', kind: 'video', playbackId: 'slow.mkv' });
      setTimeout(() => {
        window.__swap({ src: '/fixtures/tone.wav', kind: 'audio' });
        setTimeout(() => {
          const p = document.querySelector('[data-probe=swap]');
          const a = p.querySelector('audio');
          r(!!a && a.currentSrc.endsWith('tone.wav') && !p.querySelector('video') &&
            p.querySelector('.rk-media').dataset.playerStatus !== 'error');
        }, 2500);
      }, 500);
    }))()`, 8000);
  // H16: crop chosen on a 100x50 preview must export the same region of the
  // 400x200 original (top-right quadrant: green, 200x100).
  await check("preview crop exports matching original-resolution region (pixels)", `
    (async () => {
      const deadline = Date.now() + 8000;
      while (!window.__exportProbe.fn || !document.querySelector('[data-probe=quad] img')) {
        if (Date.now() > deadline) return false;
        await new Promise(r => setTimeout(r, 100));
      }
      await new Promise(r => setTimeout(r, 300));
      const p = await window.__exportProbe.fn('png');
      const bmp = await createImageBitmap(new Blob([p.buffer], { type: p.mime }));
      const c = new OffscreenCanvas(bmp.width, bmp.height); const g = c.getContext('2d');
      g.drawImage(bmp, 0, 0);
      const px = (x, y) => Array.from(g.getImageData(x, y, 1, 1).data.slice(0, 3));
      const green = a => a[0] < 40 && a[1] > 215 && a[2] < 40;
      return bmp.width === 200 && bmp.height === 100 &&
        [[2,2],[197,2],[2,97],[197,97],[100,50]].every(([x,y]) => green(px(x,y)));
    })()`, 12000);

  await check("ImageViewer mounted and measured stage", `
    (() => !!document.querySelector('[data-rk-image-stage]') &&
       !!document.querySelector('[data-rk-image-stage] img'))()`);

  await check("toolbarContainer portals only the capsule; panels stay in viewer", `
    (async () => {
      const host = document.querySelector('[data-probe="portal-host"]');
      const viewer = document.querySelector('[data-probe="portal-viewer"]');
      if (!host.querySelector('.rk-iv-bar [role="toolbar"][aria-label="Image tools"]')) return false;
      if (viewer.querySelector('.rk-iv-bar')) return false;
      const crop = host.querySelector('button[aria-label="Crop"]');
      if (crop.getAttribute('aria-pressed') !== 'true') crop.dispatchEvent(new MouseEvent('mousedown', { bubbles: true, cancelable: true, button: 0 }));
      await new Promise((r) => setTimeout(r, 200));
      return !!viewer.querySelector('.rk-iv-panel') && !host.querySelector('.rk-iv-panel');
    })()`);
} finally {
  ws.close();
  const closed = await browser.close();
  server.close();
  if (!closed.exited || !closed.profileRemoved) failures.push(`Chrome for Testing did not close cleanly: ${JSON.stringify(closed)}`);
}

if (failures.length) {
  console.error(`\n${failures.length} failure(s): ${failures.join(", ")}`);
  process.exit(1);
}
console.log("\nmedia-ui browser e2e: all checks passed");
