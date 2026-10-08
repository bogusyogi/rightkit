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
