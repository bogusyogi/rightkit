# @rightkit/qa

Shared browser and QA primitives for Right Suite apps: run storage and evidence bundles, bearer-authenticated loopback control, owned-process cleanup, pane and frame checks, paint checks, workspace environment, a raw Chrome DevTools Protocol page driver, and the Chrome for Testing re-export.

| Item | Value |
|---|---|
| Where it runs | Dev and test tooling (Node, and browser-driven test harnesses) |
| Rust pair | None declared. For native desktop QA use the Rust `rightkit-qa` crate with `rightkit-qa-driver`. |
| Version | see INDEX.md |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/qa` | `export *` from evidence, devserver, frames, guard, loopback, ownership, paint, panes, process, workspace, storage, userdata, page and chrome modules. Includes `QaBrowserSession`, `QaPage`, `QaElement`, `QaKeyboard`, `QaMouse`, `TimeoutError`, `ElementOccludedError`, `FillMismatchError`, `freePort`. From 0.4.8 also `CdpClient`, `connectCdp`, `freePortSync`, `waitForHttp`, `stopDevServer`, `readDetachedRecord`, `DevServerIdentityError`. |
| `@rightkit/qa/chrome` | Re-export of `@rightkit/qa-chrome` |

Dependency: `@rightkit/qa-chrome` (exact `0.1.6`).

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

`QaBrowserSession.attach` connects to an app that is already running with a debugging port, such as an installed WebView2 app started with `--remote-debugging-port`. Give exactly one of `port` or `httpEndpoint` (a full http or https origin). It reads the browser socket from `http://<host>:<port>/json/version` (from `/json/version` on `httpEndpoint` when that is given). It creates no target, and nothing it attaches to is closed.

- `pages()` lists page targets (`targetId`, `url`, `title`).
- `attachPage(targetId)` or `attachPage(info => ...)` (the predicate must match exactly one page) returns a `QaPage` attached with `Target.attachToTarget`, flattened.
- `close()` on an attached page detaches it. Closing the session detaches every attached page. Neither sends `Target.closeTarget`.
- The host defaults to `127.0.0.1`. A non-loopback host (loopback is `localhost`, `::1` and `127.x.x.x`), and a browser socket on a non-loopback host, is refused unless `allowRemoteHost: true`. A debugging port controls the whole app, so only allow a machine you control.

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

`spawnOwnedProcess(command, args, options)` starts a process in its own group (POSIX) and records it. `stopOwnedProcess(handle)` stops only a handle that `spawnOwnedProcess` returned, signalling the whole group. Bare pids and other objects are refused. `ownedProcessReceipt()` returns `{ leftoverPids }`, the pids of launched processes still running.

`freePort()` returns a free `127.0.0.1` port by binding port 0 and releasing it. Another process can take the port before use.

## Dev-server launcher

`startDevServer({ command, args?, cwd?, env?, shell?, windowsHide?, ports?, portFloor?, portEnv?, maxPortAttempts?, readiness })` starts a dev server hidden, gives it a port, and waits until it answers.

- The port goes to the process as the environment variable `portEnv` (default `PORT`), and every `{port}` in `args` is replaced with it.
- `readiness`: `urls` (an array, or a function of the port) that must each answer 2xx, or a `check(port)` predicate. `attemptTimeoutMs` bounds one request, `totalTimeoutMs` one launch's wait, `intervalMs` the delay between rounds.
- Port choice: `ports` in order (a held port is skipped before anything is spawned on it); otherwise the first free port at or above `portFloor`, probed by binding; otherwise `freePort()`. A collision (EADDRINUSE in the output) moves to the next candidate.
- Result: `{ pid, port, url, attempts, stop(), output() }`. `stop()` kills the whole process tree the launch started and reports `leftoverPids`.
- An installed app is refused: a `command` under an installed-apps location throws `InstalledAppRefused` before anything is spawned, unless installed-QA mode is on (`RIGHT_RELEASE_QA_STAGE=installed`, set by `right-release qa --stage installed`).
- A failed launch stops its whole tree before it throws `DevServerError` (`reason`, `pid`, `port`, `attempts`, `leftoverPids`, and a bounded `output` tail). The message includes the last 2 KB of output.

```ts
const dev = await startDevServer({
  command: process.execPath,
  args: [viteBin, "--host", "127.0.0.1", "--port", "{port}", "--strictPort"],
  portFloor: 5173,
  cwd: root,
  env: { VITE_MAILRIGHT_QA: "1" },
  readiness: { urls: (port) => [`http://127.0.0.1:${port}/`], attemptTimeoutMs: 2500, totalTimeoutMs: 20_000, intervalMs: 300 },
});
// ... run the tests against dev.url ...
await dev.stop();
```

## Detached dev servers (0.4.8)

By default a dev server's output goes to this process's pipes, and only `stop()` stops it; this package registers no exit hook. `startDevServer({ ..., detached: true, recordPath })` starts one that outlives the caller, so a "start, return, stop later" pair of scripts works.

- The server gets its own process group (POSIX), or a detached process (Windows). Its stdio is the log file `${recordPath}.log`, not this process's pipes, and the child is unreferenced, so the caller can exit.
- Once the server is ready, a JSON record is written to `recordPath`: `{ format, pid, startTime, command, url, port, logPath, recordedAt }`. `startTime` and `command` are read from the OS process table, and a record is never replaced. A call whose `recordPath` already exists refuses before it spawns anything.
- The handle exposes `pid`, `url`, `port`, `recordPath`, `logPath`, `startTime`, `command`, and `output()` (the tail of the log).
- `stopDevServer({ recordPath })` reads the record and stops the server's whole process tree, but only when the live process still has the recorded start time and command. `stopDevServer({ pid })` does the same for a server this process started. Any other process is refused with `DevServerIdentityError`, and nothing is signalled. It returns `true` when a running server was stopped, and `false` when nothing was running (a missing record is a no-op). A record is removed once its server is gone.

```ts
const dev = await startDevServer({
  command: process.execPath, args: [viteBin, "--port", "{port}", "--strictPort"], portFloor: 5173,
  detached: true, recordPath: "/abs/qa/dev.json",
  readiness: { urls: (port) => [`http://127.0.0.1:${port}/`], attemptTimeoutMs: 2500, totalTimeoutMs: 20_000, intervalMs: 300 },
});
// later, in another process:
await stopDevServer({ recordPath: "/abs/qa/dev.json" });
```

The package has no CLI entry, so there are no `devserver start` or `devserver stop` commands; call these functions from a script.

## TypeScript suites under Node's runner (ts-register)

`node --import @rightkit/qa/ts-register --test <files>.ts` runs `.ts` test sources whose imports are extensionless (`./x` finds `x.ts`, `x.tsx`, `x/index.ts`, `x/index.tsx`) or JSON without import attributes. Node strips the types; the hook only resolves. `@rightkit/qa/ts-hooks` is the same hook for `module.register`.

## Raw CDP, page sockets and labelled waits (0.4.8)

- `connectCdp(endpoint, { timeout? })` returns a `CdpClient` on a browser endpoint or on a page-level `webSocketDebuggerUrl`. `send(method, params?, sessionId?, timeoutMs?)` sends one command. `on(method, listener, sessionId?)` subscribes to one session's events, and `onAny(method, listener)` subscribes to every session's, with the listener getting `(params, sessionId)`. Both return an unsubscribe function. `close()` is synchronous, so it needs no await.
- `QaBrowserSession.connectPage(wsUrl, { timeout?, allowRemoteHost? })` returns a `QaPage` on one page's socket, with no browser session. Its commands are the root session's. `close()` closes that socket and sends no `Target` command. Loopback only unless `allowRemoteHost: true`.
- `page.send(method, params, { timeout?, sessionId? })` addresses a flattened child session. A number as the third argument still sets the timeout.
- `evaluate("expr", { label: "name" })` prefixes a thrown error as `name: <message>`. The label applies to string expressions only.
- `waitForFunction(expr, { label, timeout?, pollMs? })`: a labelled wait times out with exactly `Timed out waiting for <label>`. Its defaults are 45 s and 250 ms, and the predicate is coerced with `Boolean()`. Without `label`, `waitForFunction` is unchanged.
- `page.waitForAnimationFrames(frames = 2)` resolves after that many animation frames.
- `page.screenshot({ path, returnPath: true })` writes the file and resolves with its path. Without `returnPath` it resolves with the Buffer, as before.
- `keyboard.tap({ key, code?, windowsVirtualKeyCode?, modifiers? })` sends keyDown and then keyUp, with no text and no modifier presses.
- `mouse.click(x, y, { button?, clickCount? })` presses and releases without a preceding `mouseMoved`. `mouse.doubleClick(x, y)` is a click, then a press and release with clickCount 2. `mouse.drag(points, { moveDelayMs? })` presses the left button at the first point, moves with it held, and releases at the last point. It needs at least two points, and it releases the button if a move fails. These three refuse while a button is held.
- `freePortSync()` is `freePort` for module scope. It runs a tiny inline `node -e` child through `spawnSync`, and that child only binds and releases a port.
- `waitForHttp(url, { timeoutMs?, intervalMs? })` polls with GET until the URL answers 2xx, and throws `Timed out waiting for <url>` on timeout.

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

## QA runs never touch real user data

A test run may never start an app on the user's real data or on a location that can raise a system dialog. `spawnQaApp(workspace, command, args, options)` calls `assertQaLaunchSafe` before it spawns anything; call `assertQaLaunchSafe(workspace, { cwd, env, dirs })` yourself when you launch by other means. There is no opt-out.

- The workspace home must be a QA root this package created: directly under a `rightkit-qa` directory inside a system temp root on the internal disk, with the `.rightkit-app.json` binding. The working directory and every absolute path in the environment (and any `dirs`) must resolve, symlinks followed, inside it.
- Refused anywhere: the user's home directory itself; Documents, Desktop, Downloads, Pictures, Music, Movies, Videos; iCloud Drive (`~/Library/Mobile Documents`, `~/Library/CloudStorage`), OneDrive, Dropbox, Google Drive; the user's real application-support / AppData / `.config` directory of the app under test; any `/Volumes/*` volume; any UNC path.
- A violation throws `UserDataViolation` with `path`, `rule` and `detail`. Nothing is launched.

Seed state with generated fixtures; that is the default. `seedFrom(workspace, source, { destRel, transform, scrub })` copies through your `transform(source, dest)` into `<dataDir>/<destRel>` and refuses a `source` in a forbidden location (including the real per-user data of the app) unless `scrub` is `{ declaredScrubbed: true, verify }` and `verify(dest)` returns true before launch ("zero user rows, else no launch"). A false result deletes the copy and throws.

`assertNoPermissionDialog(pid, frontmostNames)` fails on a macOS dialog agent among sampled foreground names. The live check is a no-op: this package has no window-by-owner enumeration (TODO in `livePermissionDialogCheck`).

### QA home hygiene

Each home carries `.rightkit-home.json` (owner pid, start time). `withQaWorkspace(options, body)` removes the home when `body` ends, pass or fail; `disposeQaWorkspace(ws, { failed, keepFailedHome })` or `RIGHTKIT_QA_KEEP_FAILED_HOME=1` keeps only a failed run's home and prints its path and size. `createQaWorkspace` first removes homes whose owner is dead and that are older than 2 hours (`RIGHTKIT_QA_HOME_MAX_AGE_SECS`; a live pid is never touched), then throws `QaRootOverCapError` naming the largest homes if this product's homes still hold more than 4 GB (`RIGHTKIT_QA_ROOT_CAP_BYTES`, `0` disables; "QA root limits" below adds the root-wide ceiling; `RIGHTKIT_QA_HOME_SWEEP=0` skips the sweep). Both are off under vitest unless `preflight: { enabled: true }`. `cleanQaHomes({ dryRun })` runs the sweep by hand. Deletion happens only inside the `rightkit-qa` root, never by a caller's path, and never follows a symlink out of it. `disposeQaWorkspace` refuses a home that is not runner-created.

## Ownership: this package versus the Rust `rightkit-qa` crate

Both runners stay. Pick by what the test drives.

| Surface | Owner | Evidence in code |
|---|---|---|
| Browser page (any web app in Chrome for Testing) | `@rightkit/qa` and `@rightkit/qa-chrome` | `src/page.ts` (`QaBrowserSession`, `QaPage`), `@rightkit/qa-chrome` `launchChromeForTesting` |
| Pane, frame and paint checks on a browser page | `@rightkit/qa` | `src/panes.ts`, `src/frames.ts`, `src/paint.ts` |
| macOS native app | Rust `rightkit-qa` (`control::launch`, in-app `rightkit-qa-driver` server) | `crates/rightkit-qa/src/control.rs` |
| Windows WebView2 (Tauri) app | Rust `rightkit-qa` (embedded control server on a named pipe) | `crates/rightkit-qa/src/control.rs`, `suite.rs` |
| Engine (CLI words, framed stdio sidecar) | Rust `rightkit-qa` (`engine::EngineTarget`) | `crates/rightkit-qa/src/engine.rs` |

The pane, frame and paint helpers take a driver object with `execute` (paint also needs `executeAsync`; frames also need `switchToFrame` and `switchToParentFrame`). `QaPage` has none of these methods, so it needs an adapter.

This package drives no native app and no WebView2 window (`spawnQaApp` only starts a guarded process in a QA workspace); it never replaces the Rust crate for them.

**QA is a development tool, not a user automation surface.** The native rows drive only builds compiled with `rightkit-qa-driver`'s `qa-control` feature (the app's `qa-native` build). A shipped app contains no such server and `right-release` refuses to seal or upload one that does; this package's `QaBrowserSession.attach` connects only to a debugging port that the browser or app was already started with. The only supported automation surface for users is an app's own command line or plugin. See `crates/rightkit-qa-driver/README.md`, "QA control never ships".

## Known issues

- `@rightkit/qa-chrome` is pinned exactly to `0.1.9` (the published 0.5.1 pins `0.1.8`). A qa-chrome release therefore needs a `@rightkit/qa` republish to reach consumers.

## QA root limits

Homes live under `<tmp>/rightkit-qa/` (`/private/tmp/rightkit-qa` on macOS). Two limits are checked when a run starts, after the dead-home sweep.

- **Per-product cap** (`RIGHTKIT_QA_ROOT_CAP_BYTES`, default 4 GB, `0` disables). A run for `appKey` X is refused only when X's own homes hold more than the cap. A home belongs to the product named by the `app` in its `.rightkit-app.json` binding, else by the label in its directory name (`<app cut to 16 characters>-<suffix>`). Other products' homes never count.
- **Root ceiling** (`RIGHTKIT_QA_ROOT_CEILING_BYTES`, default 32 GB, `0` disables). Every run is refused when all homes together exceed it. The refusal names the largest homes with their product. The cap is checked first.

A refusal is a `QaRootOverCapError`. Its message names the product, the total, the largest homes, and says `cleanQaHomes()` (or `rightkit-qa clean`) removes the homes of dead runs; `error.detail` has `kind` (`'product-cap'` or `'root-ceiling'`), `app`, `totalBytes`, `homeCount`, `limitBytes` and `largest` (`{ path, bytes, product }`).

**Setting limits in code.** The variables do not always reach the test process, so pass the limits to the call: `createQaWorkspace({ appKey, rootCapBytes: 8_000_000_000, rootCeilingBytes: 0 })`, or the same options on `withQaWorkspace` (`0` disables a limit; a value that is not a non-negative number throws `TypeError`). `preflight.capBytes` and `preflight.ceilingBytes` do the same and lose to the top-level options. A value set in code wins over the variable; a limit left unset falls back to the variable, then the default. Both limits are off under vitest unless `preflight: { enabled: true }`.

**The sweep removes more.** `cleanQaHomes` (run at every start) removes a home that has no `.rightkit-home.json` owner marker (written before it existed, or missing a file) when all of these hold: it is a plain directory directly inside `rightkit-qa` (never a symlink, never outside it); its name is `<label>-<6 letters or digits>` or `<label>-<16 lowercase hex>` (the two runners' patterns); no pid its records name is alive (the `ownerPid` of the run record its binding points at, and the pid of a home marker that has no binding); and nothing in it, the directory included, was modified within `RIGHTKIT_QA_HOME_MAX_AGE_SECS` (default 2 hours). Not checked: which processes have the home as working directory or an open file (there is no process-table walk), and the run record's lease (a 24 hour timer, not evidence of a live process; it is what kept cancelled runs counted). An app that holds a home and writes nothing for two hours, or a recorded pid reused by an unrelated live process, is the residual risk: the first is removed, the second is kept. Homes with an owner marker keep the earlier rule (pid dead and run older than the window).
