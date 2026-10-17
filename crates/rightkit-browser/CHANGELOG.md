# Changelog

## 0.3.4

Consented real-session mode (#286, #287). New items on a public enum (`ProfileSpec::ImportedFrom`, `BrowserError::Consent`, `BrowserError::Unsupported`): code that matches those enums exhaustively must add arms. Nothing else changed or removed.

- `default_browser() -> BrowserKind`: default web browser on Windows (registry `UserChoice`), macOS (LaunchServices handlers; Safari when none is recorded) and Linux (`xdg-settings`). `BrowserKind` is Chrome, Edge, Chromium, Brave, Safari, Firefox or Unknown.
- `UserConsentReceipt`: serde receipt the app persists and shows: id, browser, source profile, domains (`None` = all; each covers its subdomains), granted, expiry and revoked times. `revoke`, `is_active_at`, `permits_url`, `to_json` / `from_json`. `ConsentRevocation` is a live switch for a running session.
- `ProfileSpec::ImportedFrom { root, name, import: ImportSpec }` (and `LaunchOptions::imported_profile`): a named, locked RightKit profile seeded from a Chromium-family profile. Consent, browser and profile match, and domain narrowing are checked before any browser data is read. The cookie database, local storage and `Local State` are copied; `Login Data`, `Web Data` and history never are. A marker file records the consent id, so a later launch does not copy again unless `ImportSpec::reimport()` is set (re-import on demand). `purge_imported_profile` deletes an imported profile when consent is revoked. `list_profiles`, `user_data_dir` and `browser_executable` serve the consent screen.
- Every action of an imported session goes through a scope check ahead of the caller's admission hook: URLs outside the consented domains, non-http(s) pages, typing into a field whose target names a password, and any action after revocation or expiry are denied.
- `BrowserSession::attach_user_browser(consent, root, opts)` and `attach_user_browser_revocable`: visible session on a private copy of the consented profile, launched from the SAME browser executable so its sign-in key applies (Chrome 136+ ignores a debugging port on the default user-data directory and a running profile is locked, so the live browser is not driven in place). Chrome and Edge. Safari and Firefox return `BrowserError::Unsupported` (Safari goes through rightkit-automation).
- `LaunchOptions::headless(bool)` and `visible()`: headless or visible per session; the action API is identical.
- Not implemented: per-domain cookie counts (`ImportReport::domain_counts` is `None`; counting needs the cookie database opened) and cookie filtering by domain at copy time (the database is copied whole; actions are confined to the consented domains). A browser-extension / native-messaging route for the user's live browser is not built.
- Not compiled or run yet. Needs a live run: Windows Chrome 127+ binds cookies with app-bound encryption to the Chrome install and may not decrypt in a copied profile; macOS Chrome keeps its key in the keychain under the same name the copy's browser uses (same executable), which a live run must confirm.

## 0.3.3

- Chrome for Testing lives at ONE fixed path: `<shared>/chrome-for-testing/current/`, with a single `current.previous` kept for rollback (Adrian's rule: no new id, no new folder, for every app). The resolver prefers `current`. An explicit path, RIGHTKIT_CHROME_FOR_TESTING and the Puppeteer/Playwright caches stay as dev fallbacks (#267).
- New `install` feature: `install::ensure_chrome_for_testing` for installer-time and updater-time delivery. It downloads through rightkit-models, checks the pinned SHA-256 before extracting, extracts to staging, swaps into `current` and keeps `current.previous`. Google does not publish digests for Chrome for Testing, so each pin is computed once when its version is pinned. Until then every pin is `Sha256Pin::Todo`, and the installer refuses to download with `DigestNotPinned`.
- Recorded measurement for the Chrome sandbox-access check (Windows AppContainer grants), from GenRight on the Dell: 34 s with the managed Chrome for Testing, which already carries the grants, against 152 s with a Playwright copy that lacks them and must be granted on first launch (#221).
- Not compiled or run yet.

## 0.3.2

Additive: no existing item changed or removed. New items on `BrowserPage`, so a minor bump.

- Request interception hook: `BrowserPage::set_request_interceptor(Option<RequestInterceptor>)`. The hook receives each paused request (`InterceptedRequest`: url, method, headers, resource type, frame id, redirect marker, stage) and answers `InterceptDecision::{Continue, ContinueWith(ContinueOverrides), Fulfill, Fail(InterceptFailure)}`. Redirect hops arrive as their own requests with `redirected_from` set, so an origin pin revalidates each hop. It runs after `NetworkPolicy`, whose refusals stay final; a hook can only narrow what the policy allows. A hook silent for 30 s fails the request. Works on unrestricted sessions too (Fetch interception starts on first use). Not available on fixture pages, which own `Fetch`.
- `BrowserPage::accessibility_tree()` returns `Vec<AxNode>` from `Accessibility.getFullAXTree` (role, name, description, value, properties, parent/child ids, backend DOM node id, frame id; ignored nodes flagged).
- Exception events: `BrowserPage::exceptions()`, `clear_exceptions()` and `subscribe_exceptions()` (a `tokio::sync::broadcast` receiver) carry `PageException` (text, description, url, line, column, timestamp, stack frames) from `Runtime.exceptionThrown`. They are collected from page creation, before the first navigation. At most `MAX_EXCEPTIONS` (200) are kept. The existing console log still gets its `exception` line.
- `BrowserPage::set_page_scale(f64)` (`Emulation.setPageScaleFactor`; positive and finite).
- `BrowserPage::set_download_behavior(&DownloadBehavior)` (`Browser.setDownloadBehavior`): `Deny`, `Allow { dir }`, `AllowAndName { dir }`, `Default`. A directory must be absolute and exist. Admitted through the admission hook as an `Eval`-kind effect with detail `download_behavior ...`.
- Unit tests decode recorded CDP wire messages (requestPaused incl. a redirect hop, getFullAXTree, exceptionThrown) and check the exact commands built for continue, fulfil and fail, with no browser.



## 0.3.1

- Republished on the renamed dependencies: `rightkit-child-process` (was `rightkit-process`). The 0.3.0 on crates.io was packaged before the renames and still named the old crates.
- Requires `rightkit-sandbox` 0.4.0 (was 0.3.5), so a fresh resolution no longer reaches a release that names renamed-away crates.
## 0.3.0

- Windows fixture: runs `chrome-headless-shell.exe` (same Chrome for Testing version, `chrome-headless-shell-win64/` beside `chrome-win64/`; override `RIGHTKIT_CHROME_HEADLESS_SHELL`) instead of `chrome.exe`, because `chrome.exe`'s browser process unconditionally starts the crashpad handler (`components/crash/core/app/crashpad_win.cc`), whose `CreateNamedPipe` is denied inside an AppContainer (CHECK, exit 0x80000003); the shell never starts it on Windows (`headless/lib/headless_content_main_delegate.cc`). `--headless=new` is not passed to the shell. Fixture only; production launches and macOS are unchanged.

- **Breaking (0.x minor):** the public exhaustive `BrowserError` gained the `ChromeSandboxAccess` variant; exhaustive matches must handle it.

- Tests: the three real-Chrome test files say in their headers that they run the installed Chrome for Testing on a
  temporary profile that is removed, and open no real browser profile.
- Tests: the managed-folder discovery test points `PLAYWRIGHT_BROWSERS_PATH` at an empty temporary folder, so it no
  longer lists the user's own `ms-playwright` folder. No product change.
- Launch passes `--disable-component-update`, `--disable-domain-reliability` and `--no-pings`. With `--disable-background-networking` alone Chrome still asked for `clients2.google.com` (the component updater), which reached a session's resolver and network policy as if a page had asked. A session now sees only its pages' requests.
- Windows: `BrowserSession::launch` checks that Chrome's sandbox can read the browser before starting it and returns the new `BrowserError::ChromeSandboxAccess { directory, principals, fix }` when it cannot. A Chrome unpacked from an archive outside `Program Files` has no read and execute grant for `ALL APPLICATION PACKAGES` (`S-1-15-2-1`) and `ALL RESTRICTED APPLICATION PACKAGES` (`S-1-15-2-2`); Chrome then logs "Sandbox cannot access executable", its network service crashes at startup, and a first navigation hangs or aborts. This was the cause of the Windows navigation timeouts on CI. The error names the directory and the `icacls` command that grants the access. A launch with `no-sandbox` in `extra_args` is not checked, and an access list that cannot be read does not stop a launch. `BrowserError` gains a variant: an exhaustive `match` on it needs a new arm.

## 0.2.4

- Trim the dependency tree: tokio no longer enables `fs`, `macros` or `rt-multi-thread` for production builds (the e2e tests move them to dev-dependencies), and `chromiumoxide` is used with `default-features = false`. No public API change.

## 0.2.1

- Add `LaunchOptions::profile_dir(path)` & `ProfileSpec::Directory(PathBuf)` for
  existing absolute user-data directories. Preserve spaces, Unicode & native
  path bytes; validate directory existence/type without named-profile rules,
  creation or temporary-profile cleanup.
- Add `LaunchOptions::system_chrome_only()` for exclusive `find_chrome()`
  resolution with `ChromeNotFound` on absence. Keep default CfT selection &
  `system_chrome()` fallback behavior.
- Search RightKit-managed CfT versions before existing install/cache locations,
  newest numeric version first; support `RIGHTKIT_CHROME_FOR_TESTING_ROOT`.
  Explicit executable overrides retain priority.
- Add browser-free unit coverage for profile validation/preservation, native
  argv handling, Chrome selection & managed discovery. Defaults unchanged.
