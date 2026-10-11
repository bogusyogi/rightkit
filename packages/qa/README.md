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
| `@rightkit/qa` | `export *` from evidence, frames, guard, loopback, ownership, paint, panes, process, workspace, storage, page and chrome modules. Includes `QaBrowserSession`, `QaPage`, `QaElement`, `QaKeyboard`, `QaMouse`, `TimeoutError`, `ElementOccludedError`, `FillMismatchError`, `freePort`. |
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

## Console, occlusion, fill, wheel, user agent and failure artifacts

- `page.consoleEntries()` returns the newest 200 console calls and uncaught exceptions of the page, oldest first, as `{ type, text, url?, line?, timestamp }`. `type` is the console method (`log`, `error`, ...) or `exception`. Capture is always on; `clearConsoleEntries()` empties the buffer. Entries may contain page data, so do not write them to shared logs.
- `page.click(target, { checkOccluded: true })` checks, after the pointer moves and before it presses, that `elementFromPoint` at the click point is the target or one of its descendants. Otherwise it throws `ElementOccludedError`, whose `occluder` names the element on top (tag, id, class; never text). The check walks into open shadow roots to name the element on top; a cover inside a closed shadow root is named by its host. The default is `false`.
- `page.fill(target, value, { replace?, verify? })`. The default inserts at the caret, as `type()` does. `replace: true` selects the whole field first; an empty value then deletes it. `verify: true` (requires `replace`) reads the value back once and throws `FillMismatchError` on a difference. The error gives lengths, not values.
- `page.mouse.wheel({ deltaX?, deltaY? })` sends one `mouseWheel` event at the tracked pointer position. The pointer does not move.
- `page.setUserAgent(ua, { platform?, acceptLanguage? })` overrides the user agent for the page and its later navigations. Each call replaces the whole override. `page.resetUserAgent()` restores the `navigator.userAgent` captured before the first override.
- `waitForFunction(fn, { failureArtifacts: { directory, label?, extra?, allowInsideRunRoot? } })` and `waitForSelector(...)` write a screenshot and a JSON state dump on timeout: url, title, bounded page text (`pageText`), the focused element's descriptor without its value (`activeElement`), console entries, and `extra()` if given (`extra`, or `extraError` when it throws, never settles or cannot serialise). Paths are on `error.artifacts` (`json`, `screenshot?`). `directory` must be absolute and must not lie inside `RIGHTKIT_RUN_ROOT`, which `disposeQaWorkspace` deletes; `allowInsideRunRoot: true` opts in, and those files then go with the workspace, so copy them out before dispose. A failed write is appended to the error message; the timeout still throws.

- `page.send(method, params?, timeout?)` is a page-scoped raw CDP escape hatch: unsupported surface, with Chrome's names, params and results. Prefer the typed methods. `page.setFocusEmulation(enabled)` sends `Emulation.setFocusEmulationEnabled`.
- Navigation and reloads: `evaluate`, `waitForFunction` and `waitForSelector` re-resolve the main-frame context when Chrome reports it missing, and retry within the caller's timeout.
- `onPoll: () => void | Promise<void>` on `waitForFunction` and `waitForSelector` runs once per poll, before the predicate. A throw rejects the wait with that error.
- `click(target, { scroll: { inline: "nearest" } })` and `hover(target, { scroll })` choose horizontal alignment; the default is `center`, as before.
- `fill(target, value, { replace: true, verify: { timeoutMs } })` polls the readback until it matches or the window ends; `verify: true` reads once.

`spawnOwnedProcess(command, args, options)` starts a process in its own group (POSIX) and records it. `stopOwnedProcess(handle)` stops only a handle that `spawnOwnedProcess` returned, signalling the whole group. Bare pids and other objects are refused. `ownedProcessReceipt()` lists launched processes still running.

`freePort()` returns a free `127.0.0.1` port by binding port 0 and releasing it. Another process can take the port before use.

## Keyboard keys

`page.keyboard.press(chord)` accepts named keys (`Enter`, `Tab`, `Escape`, arrows, `Home`, `End`, `PageUp`, `PageDown`, `Backspace`, `Delete`, `Space`, `Shift`, `Control`, `Alt`, `Meta`), function keys `F1` through `F24`, letters, digits, and every printable US-layout symbol, including backslash.

- Shifted symbols send Shift. `?` is Shift+`/`, `+` is Shift+`=`, `!` is Shift+`1`. Such a key carries `modifiers` 8 and its shifted `key` and `text`; no separate Shift key-down is sent.
- Chords join parts with `+`. A literal plus key goes last: `+` and `Control++`.
- `code` follows the US layout (`?` has code `Slash`). Other layouts are not modelled.

## Browser escape hatch (unsupported surface)

These methods expose Chrome's own DevTools protocol. Their names, params and results follow the browser version, not this package, and may break when Chrome changes. Use the typed `QaPage` API where it exists.

- `browser.send(method, params?)` sends one browser-scoped command with no `sessionId` and resolves with its raw result.
- `browser.version()` returns `Browser.getVersion`.
- `browser.commandLine()` returns the browser argv from `Browser.getBrowserCommandLine` as `string[]`.

`commandLine()` needs the browser launched with `--enable-automation`. `launchChromeForTesting` does not pass that flag, so launch with `launchChromeForTesting({ args: ["--enable-automation"] })`. Without it, the call rejects with an error that names the requirement; the browser is unchanged.

## Run root and workspace disposal

`createQaWorkspace` sets `RIGHTKIT_RUN_ROOT` to the workspace `home`. `disposeQaWorkspace` deletes that home, and deletes the workspace root too unless `keepEvidence` is set. A host must never map a directory that holds artifacts it wants to keep onto `RIGHTKIT_RUN_ROOT`, or onto the workspace root or home. Copy artifacts out before disposal, or use `keepEvidence: true` and keep anything written under `evidence` or `captures`.

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

- `@rightkit/qa-chrome` is pinned exactly to `0.1.5`. A qa-chrome release therefore needs a `@rightkit/qa` republish to reach consumers (npm audit 2026-10-08, qa finding).
- Three consumers pin three different versions of `@rightkit/qa-chrome` as dev dependencies (npm audit 2026-10-08).
