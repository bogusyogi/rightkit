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
| `@rightkit/qa` | `export *` from evidence, frames, guard, loopback, ownership, paint, panes, process, workspace, storage, page and chrome modules. Includes `QaBrowserSession`, `QaPage`, `QaElement`, `QaKeyboard`, `TimeoutError`. |
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

## Known issues

- `@rightkit/qa-chrome` is pinned exactly to `0.1.3`. A qa-chrome release therefore needs a `@rightkit/qa` republish to reach consumers (npm audit 2026-10-08, qa finding).
- Three consumers pin three different versions of `@rightkit/qa-chrome` as dev dependencies (npm audit 2026-10-08).
