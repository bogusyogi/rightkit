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
| `rightkit-qa-chrome` (bin) | `install`, `path`, `list` |

`install` puts the pinned, SHA-256-checked Chrome for Testing at `<shared>/chrome-for-testing/current` (one `current.previous` kept for rollback), the same layout `rightkit-browser`'s `install` feature uses. It refuses while a pin has no recorded digest.

Resolution order: explicit path, then `RIGHTKIT_CHROME_FOR_TESTING`, then the RightKit-managed `current` folder (then `current.previous`, then legacy per-version folders; `RIGHTKIT_CHROME_FOR_TESTING_ROOT` moves the root), then `/Applications` and `~/Applications` (macOS only), then the newest Puppeteer and Playwright cache builds. An explicit path or `RIGHTKIT_CHROME_FOR_TESTING` that names a missing file throws without trying the rest. A miss throws `ChromeForTestingNotFoundError` with install steps.

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

`launchChromeForTesting(...).close()` returns `ChromeCloseReport`. One monotonic `budgetMs` (default 60 s) caps shutdown, scans, kills & removal. Individual defaults remain 5 s grace, 15 s forced kill, 4 s settle & 10 s profile removal. Each phase receives at most remaining budget, with one quarter of budget (at most 5 s) reserved for final observation. External calls start only when at least one whole millisecond remains.

Windows scans find candidate profile processes through CIM, open minimal-rights native handles, then refresh command-line ownership while each handle retains its process identity. Full creation FILETIME comes from that handle, independent of CIM timestamp precision. Termination reopens each scanned PID, compares full FILETIME exactly & calls `TerminateProcess` on that same handle. Native declarations use Reflection.Emit in memory, without compiler children or temporary DLLs. Unknown ownership or identity fails closed. Launcher termination uses its owned ChildProcess. POSIX signals its process group only while launcher remains unconfirmed gone; surviving profile processes are reported.

After shutdown, profile removal retries EBUSY-class errors within its phase. Removal runs in an owned Node child, aborts at its bound & reserves up to 1 s within that same phase for confirmed exit. Unconfirmed removers release their event-loop handles after 1 s. `profileRemoval` reports `removed`, `not-attempted`, `failed`, `timed-out-terminated` or `timed-out-running`. `profileRemoved` means directory absence was verified.

`leftoverPids` records final known profile holders; an unavailable final scan retains last known leftovers. `observedExitingPids` records holders seen during settle & absent later. `exited` requires observed launcher exit plus a successful empty final scan. `livenessUnknown` means cleanup could not establish process exit. `step` records last shutdown phase attempted; `already-exited` means launcher was observed gone before shutdown, while `not-attempted` means no shutdown phase ran for an unconfirmed launcher. `durationMs` is whole milliseconds.
