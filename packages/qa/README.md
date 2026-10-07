# `@rightkit/qa`

> **Which QA tool?** To drive a Right Suite desktop app under test, use the Rust **`rightkit-qa`** crate with **`rightkit-control`**; that is the one supported engine. For a test browser only, use the zero-dependency **`@rightkit/qa-chrome`**. For an agent driving an installed app, use **`right-ctl`**. This package's WebdriverIO native lane is deprecated and will be removed once its last consumer (Cockpit hub) has moved; do not adopt it in new apps.

Shared native QA primitives for Right Suite desktop apps. The package owns run locking, bearer-authenticated loopback control, deterministic evidence bundles, exact owned-process cleanup, and the guard that keeps development WebDriver access out of production builds.

This package is intentionally private until the Windows and macOS phase gate passes. App repositories must never consume it through a Git, path, `file:`, `link:`, or `workspace:` dependency; adoption happens only after a registry version is published.

## Chrome for Testing (`@rightkit/qa/chrome`)

Web/CDP harnesses launch Chrome for Testing, never regular Google Chrome: on macOS each regular Chrome instance clones its app bundle under `<DARWIN_USER_TEMP_DIR>/../X/com.google.Chrome.code_sign_clone/`, and a killed instance leaks that clone. The native lane (`native.ts`, Tauri WebDriver) is unaffected.

- `resolveChromeForTesting({ executablePath? })` resolves the explicit option, then `RIGHTKIT_CHROME_FOR_TESTING`, then `/Applications` and `~/Applications` app installs, then the newest Puppeteer (`~/.cache/puppeteer`, `PUPPETEER_CACHE_DIR`) and Playwright (`~/Library/Caches/ms-playwright`, `PLAYWRIGHT_BROWSERS_PATH`) builds. It throws `ChromeForTestingNotFoundError` with install instructions; it never falls back. `findChromeForTesting()` returns `{ path }` or `{ reason }` for test skips.
- `launchChromeForTesting(options)` starts it in its own process group with a temp profile and returns `{ wsEndpoint, port, pid, profileDir, close() }`. Launch one browser per suite and open targets per test.
- `close()` / `closeChromeGracefully()` sends CDP `Browser.close`, waits up to 5 s, sends SIGTERM to the process group (`taskkill /T` on Windows), waits again, sends SIGKILL only last, then removes the profile. It reports the step that ended the tree.
- `codeSignCloneEntries()` lists the macOS clone entries so tests can assert that none appear.
