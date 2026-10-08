# `@rightkit/qa`

> **Which QA tool?** To drive a Right Suite desktop app under test, use the Rust **`rightkit-qa`** crate with **`rightkit-control`**; that is the one supported engine. For a test browser only, use the zero-dependency **`@rightkit/qa-chrome`**. For an agent driving an installed app, use **`right-ctl`**. This package's WebdriverIO native lane & `right-qa` CLI were removed in 0.3.0.

Shared QA primitives for Right Suite desktop apps. The package owns run locking, bearer-authenticated loopback control, deterministic evidence bundles, exact owned-process cleanup, and the guard that keeps development WebDriver access out of production builds.

This package is intentionally private until the Windows and macOS phase gate passes. App repositories must never consume it through a Git, path, `file:`, `link:`, or `workspace:` dependency; adoption happens only after a registry version is published.

## Run storage

Run roots, harness evidence, captures & summaries remain under `<managed>/runs/<app>/<run-id>`. App homes, profiles, service/port files, live logs & app-provided output paths use an owned internal system-temp companion recorded as `appRoot` in `.rightkit-run.json`. On macOS, companions always use `/private/tmp/rightkit-qa/<app-prefix>-<random>` with short names so launchd can open app files & control socket paths fit `sockaddr_un`.

Finish copies app logs to managed `evidence/`, app-written evidence to `evidence/app/` & app captures to `captures/` before releasing ownership. Disposal removes temp companions; failed copies or cleanup preserve ownership metadata for recovery. Retention & orphan pruning validate companion bindings & protect live runs.

Workspace env sets `RIGHTKIT_RUN_ROOT`, `HOME` & `USERPROFILE` to `home`; `RIGHTKIT_SUITE_ROOT` to `home/suite`; data/profile keys to `home/data`; `XDG_CACHE_HOME` to `home/cache`; `TMPDIR`, `TMP` & `TEMP` to `home/tmp`. `RIGHTKIT_QA_EVIDENCE_DIR` & `RIGHTKIT_QA_CAPTURE_DIR` use `home/evidence` & `home/captures`. The JS package supplies workspace env; native UI bundle staging & unique per-launch suite/service identity belong to Rust `control::launch`.

## Chrome for Testing (`@rightkit/qa/chrome`)

Web/CDP harnesses launch Chrome for Testing, never regular Google Chrome: on macOS each regular Chrome instance clones its app bundle under `<DARWIN_USER_TEMP_DIR>/../X/com.google.Chrome.code_sign_clone/`, and a killed instance leaks that clone.

- `resolveChromeForTesting({ executablePath? })` resolves the explicit option, then `RIGHTKIT_CHROME_FOR_TESTING`, then `/Applications` and `~/Applications` app installs, then the newest Puppeteer (`~/.cache/puppeteer`, `PUPPETEER_CACHE_DIR`) and Playwright (`~/Library/Caches/ms-playwright`, `PLAYWRIGHT_BROWSERS_PATH`) builds. It throws `ChromeForTestingNotFoundError` with install instructions; it never falls back. `findChromeForTesting()` returns `{ path }` or `{ reason }` for test skips.
- `launchChromeForTesting(options)` starts it in its own process group with a temp profile and returns `{ wsEndpoint, port, pid, profileDir, close() }`. Launch one browser per suite and open targets per test.
- `close()` / `closeChromeGracefully()` sends CDP `Browser.close`, waits up to 5 s, sends SIGTERM to the process group (`taskkill /T` on Windows), waits again, sends SIGKILL only last, then removes the profile. It reports the step that ended the tree.
- `codeSignCloneEntries()` lists the macOS clone entries so tests can assert that none appear.
