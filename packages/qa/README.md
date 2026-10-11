# @rightkit/qa

Shared browser and QA primitives for Right Suite apps: run storage and evidence bundles, bearer-authenticated loopback control, owned-process cleanup, pane and frame checks, paint checks, workspace environment, a raw Chrome DevTools Protocol page driver, and the Chrome for Testing re-export.

| Item | Value |
|---|---|
| Where it runs | Dev and test tooling (Node, and browser-driven test harnesses) |
| Rust pair | None declared. For native desktop QA use the Rust `rightkit-qa` crate with `rightkit-control`. |
| Version | see INDEX.md |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/qa` | `export *` from evidence, frames, guard, loopback, ownership, paint, panes, process, workspace, storage, page and chrome modules. Includes `QaBrowserSession`, `QaPage`, `QaElement`, `QaKeyboard`, `QaMouse`, `TimeoutError`. |
| `@rightkit/qa/chrome` | Re-export of `@rightkit/qa-chrome` |

Dependency: `@rightkit/qa-chrome` (exact `0.1.3`).

Peer dependencies: none.

## Example

```ts
import { launchChromeForTesting, QaBrowserSession } from "@rightkit/qa";

const chrome = await launchChromeForTesting();
const browser = await QaBrowserSession.connect(chrome.wsEndpoint);
try {
  const page = await browser.newPage();
  await page.setViewport({ width: 1280, height: 720, deviceScaleFactor: 1 });
  await page.goto("http://127.0.0.1:3000", { waitUntil: "networkidle0", timeout: 10_000 });
  const button = await page.waitForSelector("button", { visible: true });
  await button?.click();
  await page.screenshot({ path: "viewport.png" });
} finally {
  try { await browser.close(); } finally { await chrome.close(); }
}
```

The page driver uses Node's built-in `WebSocket`. It has no Puppeteer or Playwright dependency. Method names mirror Puppeteer's for the common calls (`goto`, `click`, `evaluate`, `waitForSelector`, `screenshot`).

## Attach to a running app and drive its mouse

`QaBrowserSession.attach` connects to an app that is already running with a debugging port, such as an installed WebView2 app started with `--remote-debugging-port`. It reads the browser socket from `http://<host>:<port>/json/version`. It creates no target, and nothing it attaches to is closed.

- `pages()` lists page targets (`targetId`, `url`, `title`).
- `attachPage(targetId)` or `attachPage(info => ...)` (the predicate must match exactly one page) returns a `QaPage` attached with `Target.attachToTarget`, flattened.
- `close()` on an attached page detaches it. Closing the session detaches every attached page. Neither sends `Target.closeTarget`.
- The host defaults to `127.0.0.1`. Any other host, and any browser socket on another host, is refused unless `allowRemoteHost: true`. A debugging port controls the whole app, so only allow a machine you control.

`page.mouse` holds the pointer position and pressed buttons. A move while a button is held sends the matching `buttons` mask. `page.click` and `page.hover` send their own events, so release any held mouse button before calling them.

Hover, pressed and after-click capture of a control in a running app:

```ts
import { QaBrowserSession } from "@rightkit/qa";

const settle = () => new Promise((resolve) => setTimeout(resolve, 120));
const browser = await QaBrowserSession.attach({ port: 9333 }); // host defaults to 127.0.0.1
try {
  const [target] = await browser.pages();
  if (!target) throw new Error("the app has no page target");
  const page = await browser.attachPage(target.targetId);
  const box = await (await page.$("#save"))?.boundingBox();
  if (!box) throw new Error("#save is not visible");
  const x = box.x + box.width / 2, y = box.y + box.height / 2;

  await page.mouse.move(x, y, { steps: 4 });   // pointer rests on the control
  await settle();
  await page.screenshot({ path: "save-hover.png" });

  await page.mouse.down();                     // held press, not yet released
  await settle();
  await page.screenshot({ path: "save-pressed.png" });

  await page.mouse.up();                       // release on the control: this is the click
  await settle();
  await page.screenshot({ path: "save-after.png" });
  await page.close();                          // detaches; the app's page stays open
} finally {
  await browser.close();                       // detaches attached pages; never closes the app
}
```

`evaluate` throws the page's JavaScript exception (or a `TimeoutError`) rather than returning an error value. Wrap the call if you need the error as data.

## Ownership: this package versus the Rust `rightkit-qa` crate

Both runners stay. Pick by what the test drives.

| Surface | Owner | Evidence in code |
|---|---|---|
| Browser page (any web app in Chrome for Testing) | `@rightkit/qa` and `@rightkit/qa-chrome` | `src/page.ts` (`QaBrowserSession`, `QaPage`), `@rightkit/qa-chrome` `launchChromeForTesting` |
| Pane, frame and paint checks on a browser page | `@rightkit/qa` | `src/panes.ts`, `src/frames.ts`, `src/paint.ts` |
| macOS native app | Rust `rightkit-qa` (`control::launch`, in-app `rightkit-control` server) | `crates/rightkit-qa/src/control.rs` |
| Windows WebView2 (Tauri) app | Rust `rightkit-qa` (embedded control server on a named pipe) | `crates/rightkit-qa/src/control.rs`, `suite.rs` |
| Engine (CLI words, framed stdio sidecar) | Rust `rightkit-qa` (`engine::EngineTarget`) | `crates/rightkit-qa/src/engine.rs` |

This package launches no native app and no WebView2 window; it never replaces the Rust crate for them.

## Known issues

- `@rightkit/qa-chrome` is pinned exactly to `0.1.3`. A qa-chrome release therefore needs a `@rightkit/qa` republish to reach consumers (npm audit 2026-10-08, qa finding).
- Three consumers pin three different versions of `@rightkit/qa-chrome` as dev dependencies (npm audit 2026-10-08).
