# Changelog

## 0.4.7 — 2026-10-11

Additive.

- Add `startDevServer(options)` (dev-server launcher on the `spawnOwnedProcess` registry). It starts the command hidden (`windowsHide` default true), sets the chosen port under `portEnv` (default `PORT`) and substitutes `{port}` in `args`, then polls every readiness URL (or a `check` predicate) with a per-attempt timeout until each answers 2xx, within `totalTimeoutMs`. It returns `{ pid, port, url, attempts, stop(), output() }`.
- Ports: `ports` are tried in order, and a listed port that is already held is skipped before anything is spawned on it (a foreign listener would otherwise answer the readiness probe); without them, `portFloor` probes upward from the floor for a free port (the bind test `freePort` uses), otherwise `freePort()`. A port collision (EADDRINUSE in the output) retries the next candidate, up to `maxPortAttempts` (default 5).
- Every failed launch, including a collision, stops the whole process tree it started before the error is thrown: the POSIX process group, or `taskkill /T /F` on Windows. `DevServerError` carries `reason` (`exited`, `timeout`, `port-in-use`, `check-failed`, `start-failed`), `pid`, `port`, `attempts`, `leftoverPids` and a bounded output tail (16 KiB; the last 2 KB go in the message).
- `stop()` stops the tree and reports `leftoverPids`; it is idempotent.
- `spawnOwnedProcess` takes `shell?: boolean` (for .cmd shims such as npm on Windows).

## 0.4.6 — 2026-10-11

Additive. Depends on `@rightkit/qa-chrome` 0.1.5 (settled close report, `observedExitingPids`).

- `failureArtifacts` gains `extra: () => unknown | Promise<unknown>`: its result is embedded in the JSON dump under `extra`. Bounded: JSON over 64 KiB is previewed and marked `truncated`; a callback that throws, rejects, never settles (cut off after 1 s) or returns something unserialisable is recorded under `extraError`. None of these masks the timeout. A non-function is refused before any CDP work.
- The JSON dump gains `pageText` (`{ text, truncated, length }`, text cut at 20,000 characters) and `activeElement` (tag, id, name, type, role, aria-label of `document.activeElement`; never its value).
- `failureArtifacts.allowInsideRunRoot: true` opts in to a directory under `RIGHTKIT_RUN_ROOT`. The default still refuses. These files are deleted with the workspace by `disposeQaWorkspace`: copy them out before dispose.

## 0.4.5 — 2026-10-11

Additive; defaults are unchanged. Depends on `@rightkit/qa-chrome` 0.1.4 (POSIX leftover scan, `leftoverPids`).

- Context re-resolution: `evaluate`, `waitForFunction` and `waitForSelector` survive a navigation or reload. A call that fails with CDP's "Cannot find context with specified id" (the command did not run) re-resolves the main-frame context and retries until the caller's timeout; when the budget runs out the missing-context error is thrown. `waitForFunction` already retried a missing context per poll and still does.
- `page.send(method, params?, timeout?)` is public: a page-scoped raw CDP escape hatch, unsupported surface like `session.send`.
- `page.setFocusEmulation(enabled)` sends `Emulation.setFocusEmulationEnabled`.
- `onPoll` option on `waitForFunction` and `waitForSelector`: called once per poll before the predicate; a throw rejects the wait with that same error, after failure artifacts are written when configured. Artifact paths are attached to the error only for a TimeoutError.
- `click`/`hover` take `scroll: { inline: 'center' | 'nearest' }`; default `center` as before. `hover(target, options)` and `QaElement.hover(options)` are new.
- `fill(..., { verify: { timeoutMs } })` polls the readback every 25 ms until it matches or the window ends. `verify: true` is still one read.
- Adds `spawnOwnedProcess(command, args?, { stdio?, cwd?, env?, windowsHide? })` (process group on POSIX), `stopOwnedProcess(handle)` (refuses any handle it did not return, including a bare pid; signals the whole group: SIGTERM, then SIGKILL after 1 s; idempotent) and `ownedProcessReceipt()` (`leftoverPids` of launched processes still running).

## 0.4.4 — 2026-10-11

Additive; existing methods and defaults are unchanged.

- Add console capture: `page.consoleEntries()` returns the newest 200 console calls and uncaught exceptions of a page, typed `{ type, text, url?, line?, timestamp }` (`line` is 1-based; `type` is `exception` for uncaught errors). `page.clearConsoleEntries()` empties the buffer. Capture is always on and needs no start/stop call.
- Add `click(target, { checkOccluded: true })`. Before pressing, it checks with `elementFromPoint` that the element at the click point is the target or its descendant, and throws `ElementOccludedError` naming the element on top (`occluder`, e.g. `<div id="toast">`). Default `false`, unchanged. The check walks into open shadow roots to name the element on top; a cover inside a closed shadow root is named by its host.
- Add `fill(target, value, { replace?, verify? })`. The default inserts at the caret, exactly like `type()`. `replace: true` selects the whole field first (an empty value then deletes it). `verify: true` (requires `replace`) reads the value back once and throws `FillMismatchError` on a difference; its message gives lengths, not values.
- Add `page.mouse.wheel({ deltaX?, deltaY? })`: one `mouseWheel` event at the tracked pointer position. At least one delta must be non-zero.
- Add `page.setUserAgent(userAgent, { platform?, acceptLanguage? })` (`Emulation.setUserAgentOverride`) and `page.resetUserAgent()`. Reset restores the `navigator.userAgent` captured before the first override and is a no-op when none was set.
- Add failure artifacts: `waitForFunction` and `waitForSelector` take `failureArtifacts: { directory, label? }`. On timeout they write `<label>-<time>-<n>.png` and `.json` (url, title, predicate text, timeout, console entries) into the directory and attach paths to the error as `error.artifacts`. The directory must be absolute and must not lie inside `RIGHTKIT_RUN_ROOT`, which `disposeQaWorkspace` deletes; it is validated before any CDP work.
- Add `freePort()`: a free 127.0.0.1 TCP port (bind to 0, then release). Another process may take it before use.

## 0.4.3 — 2026-10-11

- `keyboard.press` now sends F1-F24 (Windows virtual keys 112-135) and every printable US-layout symbol, including shifted ones (`?`, `+`, `!`, `|`, and so on) and backslash. Shifted symbols carry the Shift modifier and text. Previously `F3`, `F5`, `?`, `\` and other symbols threw `Unsupported key`, and a bare `+` failed chord validation.
- Add `QaBrowserSession.send(method, params?)`, an unsupported browser-scoped CDP escape hatch with no `sessionId`.
- Add `QaBrowserSession.version()` (`Browser.getVersion`) and `commandLine()` (`Browser.getBrowserCommandLine`). `commandLine()` needs the browser launched with `--enable-automation`; without it, it rejects with an error that names the requirement.
- Document that `RIGHTKIT_RUN_ROOT` points inside the workspace home that `disposeQaWorkspace` deletes; hosts must not map artifact directories onto it.

## 0.4.2 — 2026-10-11

- Add `QaBrowserSession.attach({ port | httpEndpoint, host?, allowRemoteHost? })` to attach to an app that already runs with a debugging port (reads `/json/version`; loopback only unless `allowRemoteHost: true`), `pages()` to list page targets, and `attachPage(targetId | predicate)` to attach to one without creating a target.
- An attached `QaPage` detaches on `close()` and never closes the app's target; closing an attached session detaches its pages only.
- Add `page.mouse` (`QaMouse`): `move(x, y, { steps? })`, `down({ button?, clickCount? })`, `up({ button? })`, with tracked position and held-button `buttons` masks. `page.click` and `page.hover` keep their event sequence and refuse to run while a mouse button is held.
- Document `evaluate` throwing on JavaScript exceptions (unchanged).

## 0.4.1 — 2026-10-10

- Depend on the published `@rightkit/qa-chrome` `0.1.3` exactly instead of `workspace:*`, so the source manifest matches the tarball dependency. No runtime change.

## 0.4.0 — 2026-10-08

- Add `QaBrowserSession`, `QaPage`, `QaElement`, `QaKeyboard` & typed `TimeoutError` using raw CDP over Node 26's native WebSocket.
- Support navigation, viewport/DPR, init scripts, JSON evaluation, selector/predicate waits, remote handles, real input & viewport PNG capture.
- Add mock transport regressions & real Chrome for Testing fixture coverage.

## 0.3.0 — 2026-10-08

- Breaking: WebdriverIO native lane & `right-qa` CLI are removed.
- Drive apps under test with Rust `rightkit-qa` + `rightkit-control` crates.
- Use `@rightkit/qa-chrome` for a test browser.
