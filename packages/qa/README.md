# `@rightkit/qa`

> **Which QA tool?** To drive a Right Suite desktop app under test, use the Rust **`rightkit-qa`** crate with **`rightkit-control`**; that is the one supported engine. For a test browser only, use the zero-dependency **`@rightkit/qa-chrome`**. For an agent driving an installed app, use **`right-ctl`**. This package's WebdriverIO native lane & `right-qa` CLI were removed in 0.3.0.

Shared QA primitives for Right Suite desktop apps. The package owns run locking, bearer-authenticated loopback control, deterministic evidence bundles, exact owned-process cleanup, and the guard that keeps development WebDriver access out of production builds.

This package is intentionally private until the Windows and macOS phase gate passes. App repositories must never consume it through a Git, path, `file:`, `link:`, or `workspace:` dependency; adoption happens only after a registry version is published.

## Run storage

Run roots, harness evidence, captures & summaries remain under `<managed>/runs/<app>/<run-id>`. App homes, profiles, service/port files, live logs & app-provided output paths use an owned internal system-temp companion recorded as `appRoot` in `.rightkit-run.json`. On macOS, companions always use `/private/tmp/rightkit-qa/<app-prefix>-<random>` with short names so launchd can open app files & control socket paths fit `sockaddr_un`.

Finish copies app logs to managed `evidence/`, app-written evidence to `evidence/app/` & app captures to `captures/` before releasing ownership. Disposal removes temp companions; failed copies or cleanup preserve ownership metadata for recovery. Retention & orphan pruning validate companion bindings & protect live runs.

Workspace env sets `RIGHTKIT_RUN_ROOT`, `HOME` & `USERPROFILE` to `home`; `RIGHTKIT_SUITE_ROOT` to `home/suite`; data/profile keys to `home/data`; `XDG_CACHE_HOME` to `home/cache`; `TMPDIR`, `TMP` & `TEMP` to `home/tmp`. `RIGHTKIT_QA_EVIDENCE_DIR` & `RIGHTKIT_QA_CAPTURE_DIR` use `home/evidence` & `home/captures`. The JS package supplies workspace env; native UI bundle staging & unique per-launch suite/service identity belong to Rust `control::launch`.

## Chrome for Testing (`@rightkit/qa/chrome`)

Web/CDP harnesses launch Chrome for Testing, never regular Google Chrome: on macOS each regular Chrome instance clones its app bundle under `<DARWIN_USER_TEMP_DIR>/../X/com.google.Chrome.code_sign_clone/`, and a killed instance leaks that clone.

- `resolveChromeForTesting({ executablePath? })` resolves the explicit option, then `RIGHTKIT_CHROME_FOR_TESTING`, then `/Applications` and `~/Applications` app installs, then the newest Puppeteer (`~/.cache/puppeteer`, `PUPPETEER_CACHE_DIR`) and Playwright (`~/Library/Caches/ms-playwright`, `PLAYWRIGHT_BROWSERS_PATH`) builds. It throws `ChromeForTestingNotFoundError` with install instructions; it never falls back. `findChromeForTesting()` returns `{ path }` or `{ reason }` for test skips.
- `launchChromeForTesting(options)` starts it in its own process group with a temp profile and returns `{ wsEndpoint, port, pid, profileDir, close() }`. Launch one browser per suite and open targets per test.
- `close()` / `closeChromeGracefully()` sends CDP `Browser.close`, waits up to 5 s, sends SIGTERM to the process group (`taskkill /T` on Windows), waits again, sends SIGKILL only last, then removes the profile. It reports the step that ended the tree.
- `codeSignCloneEntries()` lists the macOS clone entries so tests can assert that none appear.

## Raw CDP page driver (Node 26)

`QaBrowserSession`, `QaPage`, `QaElement`, `QaKeyboard` & `TimeoutError` are exported from `@rightkit/qa`. Transport uses Node's built-in `WebSocket` directly; no Puppeteer, Playwright, or CDP wrapper dependency.

```ts
import { launchChromeForTesting, QaBrowserSession } from '@rightkit/qa';

const chrome = await launchChromeForTesting();
const browser = await QaBrowserSession.connect(chrome.wsEndpoint);
try {
  const page = await browser.newPage();
  await page.setViewport({ width: 1280, height: 720, deviceScaleFactor: 1 });
  await page.evaluateOnNewDocument(() => { window.localStorage.setItem('qa', '1'); });
  await page.goto('http://127.0.0.1:3000', { waitUntil: 'networkidle0', timeout: 10_000 });
  const button = await page.waitForSelector('button', { visible: true });
  await button?.click();
  await button?.dispose();
  await page.screenshot({ path: 'viewport.png', type: 'png', captureBeyondViewport: false });
} finally {
  try { await browser.close(); } finally { await chrome.close(); }
}
```

Session owns targets it creates; launcher owns browser process & profile. `connect(endpoint, { timeout, debug })` defaults to 30,000 ms per connection/command, with logging off. Positive finite deadlines apply to navigation & waits; failures use typed `TimeoutError`. `page.on(cdpMethod, listener)` subscribes to page-session events & returns an unsubscribe function. Debug callbacks receive method names only. Throwing event subscribers are isolated from transport state.

Evaluation uses main frame's default execution context, awaits promises & returns values by JSON. Functions run in Chrome, so pass external values as arguments rather than closing over Node variables. JavaScript exceptions become `Error` with Chrome's message. `evaluateOnNewDocument` accepts function + arguments or source text & runs before document scripts, including subsequent reloads.

`goto`/`reload` support `load`, `domcontentloaded` & `networkidle0` (zero outstanding requests for 500 ms). `waitForSelector` supports present/visible/hidden states; hidden waits return `null`. `waitForFunction` supports animation-frame or positive millisecond polling & resolves when predicate is truthy. Both name selector/predicate in timeout errors. `$`/`$$` return remote element handles, released by `dispose()`, navigation, or page closure.

Click/hover scroll elements into view & use box centres. Mouse, keyboard & text input use CDP `Input` commands; `select` dispatches DOM `input`/`change` after selecting values. Keyboard supports required navigation/editing keys, letters/digits & modifier chords such as `Control+A`, `Meta+A`, or `Shift+ArrowLeft`. Screenshots return `Buffer`, optionally write `path`, & capture viewport PNG at configured DPR; `fullPage` & `captureBeyondViewport` support `false` only.

| Puppeteer | QaPage equivalent |
|---|---|
| `puppeteer.connect({ browserWSEndpoint })` | `QaBrowserSession.connect(wsEndpoint)` |
| `browser.newPage()` / `browser.close()` | Same calls; session closes owned targets |
| `page.goto()` / `reload()` / `close()` | Same calls & navigation options |
| `page.setViewport()` | Same call; CDP device metrics |
| `page.evaluateOnNewDocument()` | Same call; function or source |
| `page.evaluate()` / `$eval()` / `$$eval()` | Same calls; JSON results & awaited promises |
| `page.waitForSelector()` | Same options; `QaElement` or `null` |
| `page.waitForFunction()` | Same predicate/options/arguments; returns `void` |
| `page.$()` / `page.$$()` | `QaElement` / `QaElement[]` |
| `element.click/type/hover/focus/boundingBox/evaluate/dispose()` | Same calls on `QaElement` |
| `page.click/type/hover/focus/select()` | Same calls; click accepts selector or handle |
| `page.keyboard.press()` | Same call; named key or modifier chord |
| `page.screenshot()` | Viewport PNG `Buffer`; optional `path` |

`pnpm --dir packages/qa test` runs mock-WebSocket unit tests & real Chrome for Testing integration against local static fixture. Install Chrome through `@rightkit/qa-chrome` or set `RIGHTKIT_CHROME_FOR_TESTING`; integration requires Chrome & does not silently skip.
