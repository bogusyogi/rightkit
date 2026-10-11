# Changelog

## 0.1.6 — 2026-10-11

- Found by HeardRight: Windows close reported `step: sigkill, exited: false, leftoverPids: [], observedExitingPids: []`, with the profile scan empty. Two code paths in `closeChromeGracefully` produce that shape, and the report alone does not show which fired. (a) A failed or timed-out profile scan (PowerShell non-zero exit, or the 15 s `spawnSync` timeout) returned `[]`, the same as an empty profile. (b) The launcher's exit is recorded only on an event-loop turn, and the close took no turn after a blocking `taskkill` or scan, so a read could be stale.
- `exited` is now true only when the launcher's exit is observed after a fresh turn (or the OS probe reports it gone) and the profile scan positively lists nothing. A stale read is refreshed before any verdict.
- A failed or timed-out profile scan or liveness probe is retried inside the bound. If it never answers, the report sets `livenessUnknown: true` (new, optional, present only when true) and `exited` is false. It is never read as clean or as alive.
- Once the launcher has exited and the profile scan is empty, close returns without waiting for the kill bound.
- A real leftover is unchanged: `exited: false` with its pid in `leftoverPids`.
- Windows: the profile scan script now stops on error and prints a completion marker. Output without the marker counts as a failed scan.
- `closeChromeGracefully({ seams })` accepts injected platform, launcher probe, signal, kill, scan and clock, for tests. `settleProfileProcesses` accepts a lister that returns `null` and reports `unknown` on a scan that never answers.

## 0.1.5 — 2026-10-11

- Close report built after the forced phase no longer treats processes that are still exiting as leftovers. A bounded settle (`settleMs`, default 4 s) rescans the profile's processes, waits between scans, and reports only what is still listed after the bound as `leftoverPids`. Pids listed at the last forced round and gone by the settle are reported as `observedExitingPids`, so slow exit can be told from a leak. `exited` is now `tree gone and leftoverPids empty` on every platform; a real leftover never reads as a pass.
- Export `settleProfileProcesses({ list, boundMs?, pollMs?, now?, sleep? })`, the settle with an injected lister and clock, for tests.
- Found by HeardRight: `step: sigkill, exited: false, leftoverPids: [7452, 54436]` on Windows under load, with both pids gone moments later.

## 0.1.4 — 2026-10-11

- `chromeProcessesForProfile` now scans the process list on macOS and Linux (`ps -axww -o pid= -o args=`) for processes whose argv carries `--user-data-dir=<profileDir>` as a whole argument. It returned `[]` off Windows, so a "no leftover processes" check could never fail there. Found by HeardRight.
- `ChromeCloseReport.leftoverPids: number[]`: pids still carrying the closed browser's profile after close, on every platform. Reported, not signalled, on POSIX.

## 0.1.3 — 2026-10-08

- Windows install extracts with `%SystemRoot%\System32	ar.exe`; a bare `tar` under Git Bash resolved to GNU tar, which reads `C:\…` as a remote host. Found by HeardRight.
- `closeChromeGracefully` keeps re-issuing the forced kill (tree and leftover profile processes) every 2 s until gone, bounded by the new `killTimeoutMs` (default 15 s); one 2 s round left hung renderers behind under load. Found by HeardRight.

## 0.1.2 — 2026-10-08

- `rightkit-qa-chrome install` (bin) downloads the current Chrome for Testing channel build straight from Google's Chrome for Testing release list into the RightKit-managed folder (`<app data>/Orthic Labs/shared/chrome-for-testing/<version>/`, override `RIGHTKIT_CHROME_FOR_TESTING_ROOT`); `install.json` records the archive digest. No Puppeteer or Playwright needed. Also `path` and `list`.
- The resolver checks the managed folder first, newest version first; app installs and the Puppeteer/Playwright caches remain legacy fallbacks. Found by ViewRight: a stale cached 152 sat three majors behind system Chrome 155 and changed srcdoc behaviour.
