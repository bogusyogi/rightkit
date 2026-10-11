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
| `@rightkit/qa-chrome` | `resolveChromeForTesting`, `findChromeForTesting`, `chromeForTestingCandidates`, `launchChromeForTesting`, `closeChromeGracefully`, `chromeProcessesForProfile`, `ChromeForTestingNotFoundError`, `CHROME_FOR_TESTING_ENV`, `codeSignCloneRoots`, `codeSignCloneEntries`, and install helpers |
| `rightkit-qa-chrome` (bin) | `install [--channel stable\|beta]`, `path`, `list` |

Resolution order: explicit path, then `RIGHTKIT_CHROME_FOR_TESTING`, then app folders, then the newest Puppeteer and Playwright cache builds. A miss throws `ChromeForTestingNotFoundError` with install steps.

Peer dependencies: none.

## Example

```js
import { launchChromeForTesting } from "@rightkit/qa-chrome";

const browser = await launchChromeForTesting({ args: ["--headless=new"] });
// connect to browser.wsEndpoint
await browser.close(); // CDP Browser.close, then SIGTERM, then SIGKILL; profile removed
```

## Known issues

- `codeSignCloneRoots` and `codeSignCloneEntries` are macOS code-sign clone checks inside a QA package. Their ownership is unresolved (npm audit 2026-10-08, qa-chrome finding).
- Three consumers pin three different versions of this package as dev dependencies (npm audit 2026-10-08).

## Leftover processes

`chromeProcessesForProfile(profileDir)` returns the pids still carrying `--user-data-dir=<profileDir>` as a whole argument. It scans the process list on Windows (PowerShell), macOS and Linux (`ps`), so it can find a helper orphaned by a killed parent. A sibling profile whose path only starts with the same text does not match.

`launchChromeForTesting(...).close()` returns a `ChromeCloseReport`. After the forced phase it runs `settleProfileProcesses`: it rescans the profile's processes and waits, up to `settleMs` (default 4 s), for listed pids to exit. `leftoverPids` lists what is still listed after that bound; `observedExitingPids` lists pids seen at the last forced round that were gone by the settle. `exited` is true only when the launcher's exit is observed and `leftoverPids` is empty. `livenessUnknown` (optional, present only when true) means a liveness query or the final profile scan failed or timed out, or the launcher's exit was never seen while the scan listed nothing, so `exited` is false and the close cannot say the browser is gone. POSIX signals the process group only, never a single leftover pid; Windows forces the leftover pids before the settle.

