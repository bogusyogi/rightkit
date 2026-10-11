# @rightkit/qa-chrome

Chrome for Testing resolver, launcher and graceful closer for repository QA harnesses. No runtime dependencies. Regular Google Chrome is never used as a fallback.

| Item | Value |
|---|---|
| Where it runs | Dev and test tooling (Node) |
| Rust pair | None |
| Version | see INDEX.md |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/qa-chrome` | `resolveChromeForTesting`, `findChromeForTesting`, `chromeForTestingCandidates`, `launchChromeForTesting`, `closeChromeGracefully`, `settleProfileProcesses`, `chromeProcessesForProfile`, `ChromeForTestingNotFoundError`, `CHROME_FOR_TESTING_ENV`, `codeSignCloneRoots`, `codeSignCloneEntries`, install helpers, and Windows sandbox-access helpers (`sandboxAccessProblem`, `grantSandboxAccess`, `ChromeSandboxAccessError`) |
| `rightkit-qa-chrome` (bin) | `install [--channel stable\|beta]`, `path`, `list` |

Resolution order: explicit path, then `RIGHTKIT_CHROME_FOR_TESTING`, then the RightKit-managed folder (newest installed version first; `rightkit-qa-chrome install` fills it and `RIGHTKIT_CHROME_FOR_TESTING_ROOT` moves it), then `/Applications` and `~/Applications` (macOS only), then the newest Puppeteer and Playwright cache builds. An explicit path or `RIGHTKIT_CHROME_FOR_TESTING` that names a missing file throws without trying the rest. A miss throws `ChromeForTestingNotFoundError` with install steps.

On Windows, `launchChromeForTesting` throws `ChromeSandboxAccessError` before starting the browser when Chrome's sandbox is known to lack read and execute access to the executable's folder, unless `--no-sandbox` is passed in `args`.

Peer dependencies: none.

## Example

```js
import { launchChromeForTesting } from "@rightkit/qa-chrome";

const browser = await launchChromeForTesting({ args: ["--headless=new"] });
// connect to browser.wsEndpoint
await browser.close(); // CDP Browser.close, then SIGTERM (not on Windows), then SIGKILL; profile removed
```

## Known issues

- `codeSignCloneRoots` and `codeSignCloneEntries` are macOS code-sign clone checks inside a QA package. Their ownership is unresolved (npm audit 2026-10-08, qa-chrome finding).
- App consumers pin different versions of this package as dev dependencies (npm audit 2026-10-08).

## Leftover processes

`chromeProcessesForProfile(profileDir)` returns the pids of processes whose command line carries the profile directory. On macOS and Linux (`ps`) it matches `--user-data-dir=<profileDir>` as a whole argument; on Windows (PowerShell) it matches the profile path as a whole path in any case, so a child that names a file inside the profile also matches. It scans the whole process list, so it can find a helper orphaned by a killed parent. A sibling profile whose path only starts with the same text does not match. A scan that fails or times out returns `[]`, which means unknown, not none.

`launchChromeForTesting(...).close()` returns a `ChromeCloseReport`. After the close steps and the profile removal it runs `settleProfileProcesses`: it rescans the profile's processes and waits, up to `settleMs` (default 4 s), for listed pids to exit. `leftoverPids` lists what is still listed after that bound; `observedExitingPids` lists pids the first settle scan listed that a later scan no longer listed. `exited` is true only when the launcher's exit is observed and `leftoverPids` is empty. `livenessUnknown` (optional, present only when true) means a liveness query or the final profile scan failed or timed out, or the launcher's exit was never seen while the scan listed nothing, so `exited` is false and the close cannot say the browser is gone. POSIX signals the process group only, never a single leftover pid; Windows forces the leftover pids before the settle.

