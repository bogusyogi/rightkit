# rightkit-browser

Shared Chrome DevTools Protocol browser runtime for Right Suite products. `BrowserSession` owns one Chrome process and any number of pages. Input is real `Input.dispatch*Event` input, so `event.isTrusted`, `:hover`, focus and default actions behave as they do for a user. Sessions run headless, muted and on a temporary profile by default; headed and persistent profiles are opt-in.

Catalog capability: `browser-automation`.

## What it owns

- `BrowserSession`: one Chrome process, multi-page sessions, graceful shutdown and drop.
- `ProfileSpec`: a temporary profile, a named persistent profile (`named_profile(root, name)`), or a caller-owned directory (`profile_dir(path)`).
- `NetworkPolicy`: blocks loopback, link-local, private, reserved and `file:` targets, including redirects, subresources and proxied connections. Denials are typed (`BrowserError::Blocked`).
- `AdmissionHook`: gates each effect before it runs. `BrowserEvent` reports start, stop, crash and denied events.
- `BrowserPage::observe`: returns element refs that are checked for staleness before use.
- Chrome for Testing resolution (`chrome_for_testing_path`) and the explicit system-browser modes.

## When to use it

- Use it to drive a real Chromium page from Rust with trusted input, for QA, scraping or product automation.
- Use it when pages must be reached through an SSRF-safe network policy.

Do not use it to drive Safari or Firefox, or desktop apps (see `rightkit-automation` and `rightkit-qa-driver`).

## Cargo features

`install` (off by default): `install::ensure_chrome_for_testing`; pulls `rightkit-models`, `rightkit-fs` and `zip`. Dependencies include `chromiumoxide` 0.9, `tokio`, `url`, and `rightkit-child-process` for owned child processes.

`cookie-export` (off by default): `CookieExport`, which keeps the cookies of declared domains and publishes them as a private Netscape `cookies.txt` through `rightkit-fs`; an empty or invalid export never replaces the previous file, and `CookieExport::clear_with` removes the file and then runs a callback so the caller can invalidate an engine authentication snapshot. Build cookies with `NetscapeCookie::new`.

## Usage

Taken from the crate README and from `LaunchOptions` in `src/config.rs`. Illustrative: it is a `no_run` example.

```rust,no_run
use rightkit_browser::{BrowserSession, LaunchOptions};

# async fn example() -> rightkit_browser::Result<()> {
let session = BrowserSession::launch(
    LaunchOptions::default()
        .system_chrome_only()
        .profile_dir("/absolute/path/ScrapeRight profile with spaces"),
).await?;
session.shutdown().await;
# Ok(())
# }
```

Loopback fixtures are blocked by default. `tests/e2e.rs` opts in for a local test server with `LaunchOptions::default().network(NetworkPolicy::default().allow_host("127.0.0.1"))`.

## Interception, accessibility, exceptions, scale, downloads

- `set_request_interceptor(Some(hook))`: `hook(InterceptedRequest) -> BoxFuture<InterceptDecision>` runs for every request after `NetworkPolicy` (policy refusals are final). Answer `Continue`, `ContinueWith(ContinueOverrides)`, `Fulfill { status, headers, body, phrase }` or `Fail(InterceptFailure)`. Redirect hops are separate requests (`redirected_from` set): pin an origin by checking `url` on each. No answer in 30 s fails the request.
- `accessibility_tree()` returns the root frame's `Vec<AxNode>`; `accessibility_tree_with(depth)` cuts it off below `depth`.
- `frames()` lists CDP frame ids; `target_id()` is the tab's CDP target id (stable tab identity; `id()` is the session's own sequence).
- `scroll_by(target, dx, dy)` runs `scrollBy` on an element, a point or the window, for cases where a wheel event at the element is wrong.
- `set_transparent_background(true)` with `screenshot_with(ScreenshotOptions { omit_background: true })` captures a transparent PNG.
- `BrowserSession::new_page_with(url, PageSetup { interceptor, download })` installs the interceptor and download behaviour before the first request.
- `observe` nodes carry `sensitive`, `frame_id` (the main frame; iframe content is not observed), and `stable_id`; the observation carries `ax`, a depth-bounded accessibility summary.
- `design_mode` holds the element-picker primitives (isolated-world binding bridge, one-shot native-gesture submit gate, capture geometry), `build_design_mode_script(nonce)` and `BrowserPage::enable_design_mode`. Observation covers the main document only; iframe observation is not implemented.
- `exceptions()` / `subscribe_exceptions()` return `PageException`s from `Runtime.exceptionThrown`, collected from page creation.
- `set_page_scale(f64)` sets the pinch-zoom page scale.
- `set_download_behavior(&DownloadBehavior)` sets deny, allow into an explicit directory, allow named by GUID, or default.

## Chrome resolution

Launch resolves Chrome for Testing (`chrome_for_testing_path`) in this order: `LaunchOptions::chrome_path`, then the `RIGHTKIT_CHROME_FOR_TESTING` environment variable, then the RightKit-managed `current` folder (one fixed path, with a single `current.previous` kept for rollback; legacy per-version folders are tried after it, newest first), app installs, and the newest Puppeteer or Playwright cache builds.

| Platform | Managed executable |
|---|---|
| macOS ARM64 | `~/Library/Application Support/Orthic Labs/shared/chrome-for-testing/current/chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing` |
| macOS x64 | `~/Library/Application Support/Orthic Labs/shared/chrome-for-testing/current/chrome-mac-x64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing` |
| Windows | `%LOCALAPPDATA%\Orthic Labs\shared\chrome-for-testing\current\chrome-win64\chrome.exe` |
| Linux | `$XDG_DATA_HOME/Orthic Labs/shared/chrome-for-testing/current/chrome-linux64/chrome` (`XDG_DATA_HOME` defaults to `~/.local/share`) |

The `install` cargo feature adds `install::ensure_chrome_for_testing(&ManagedRoot, &PinnedChrome)` for an app's installer or updater step: it downloads through rightkit-models, checks the pinned SHA-256 before extracting, extracts to `current.staging`, swaps into `current` and keeps `current.previous`. A pin whose digest is `Sha256Pin::Todo` is refused.

- `RIGHTKIT_CHROME_FOR_TESTING_ROOT` overrides only the managed root. An explicit executable override keeps priority.
- If Chrome for Testing is missing, launch returns `BrowserError::ChromeForTestingNotFound` with install instructions.
- `LaunchOptions::system_chrome()` keeps the Chrome for Testing preference and falls back to `find_chrome()` only when that resolution fails.
- `LaunchOptions::system_chrome_only()` resolves only through `find_chrome()`, including its `CHROME` override. It ignores `chrome_path` and Chrome for Testing discovery. If no system Chrome, Chromium or Edge exists, it returns `BrowserError::ChromeNotFound`.
- The last selection builder wins.
- The Windows fixture (AppContainer) launches `chrome-headless-shell.exe` from `<version>/chrome-headless-shell-win64/` beside the resolved Chrome (else any managed version; `RIGHTKIT_CHROME_HEADLESS_SHELL` names it explicitly), not `chrome.exe`: Chrome's browser process always starts the crashpad handler, which cannot create its named pipe inside an AppContainer. Fixture only.
- On macOS, a killed Chrome instance can leave a code-sign clone of its bundle behind when it does not shut down cleanly (`src/config.rs`).

## Profiles

- `profile_dir(path)` selects `ProfileSpec::Directory`. Launch checks that the path is absolute, exists and is a directory, without creating it. Any existing directory is accepted, including real Chrome user-data directories, paths with spaces, and Unicode names. Named-profile validation stays exclusive to `named_profile(root, name)`.
- The caller owns that directory. This mode creates no RightKit profile lock, and shutdown, cancellation, failed launch or drop never removes it.
- Downloads go to `<profile>/rightkit-downloads` for named and caller-owned profiles, and to `<profile>/downloads` for a temporary profile (`src/session.rs:61`, `:81`, `:89`), or to `LaunchOptions::download_dir` when it is set. The directory is created at launch.

## Real session (the user's own sign-ins, with consent)

`BrowserSession::attach_user_browser(consent, root, opts)` opens a visible Chrome or Edge window on a private copy of the user's consented profile, so the agent starts signed in. It never drives the live browser in place (a running profile is locked, and Chrome 136+ ignores a debugging port on the default user-data directory).

- `default_browser()` detects the default browser; `list_profiles(kind)` feeds the consent screen.
- `UserConsentReceipt` (browser, profile, domains, grant/expiry/revoke times) is what the app persists and shows. `ConsentRevocation` stops a running session at its next action; `purge_imported_profile` deletes the copy.
- `ProfileSpec::ImportedFrom` (`LaunchOptions::imported_profile`) does the copy without attaching: cookie database, local storage and `Local State` only; never `Login Data`, `Web Data` or history. `ImportSpec::reimport()` copies again.
- Actions are confined to the consented domains and refuse password fields: passwords stay in the browser's own password manager.
- Chrome and Edge only. Safari and Firefox return `BrowserError::Unsupported` (drive Safari through `rightkit-automation`).
- Not verified live: whether a copied profile's cookies decrypt (Windows app-bound encryption, macOS keychain key). Per-domain import counts are not available.

## Shutdown

`shutdown()` closes gracefully: CDP `Browser.close`, then SIGTERM to the process group, then SIGKILL, each step bounded at 5 s. `Drop` sends SIGTERM, waits up to 5 s, then sends SIGKILL. A temporary profile is removed once Chrome has exited.

## Platform support

Chrome or Edge through chromiumoxide. Platform-specific paths are in the resolution table above. Process-group signalling uses Unix signals, so the shutdown sequence described above applies on Unix hosts.

## Tests

- `tests/e2e.rs`: real headless Chrome against a local static page. Assertions read page state produced by trusted input.
- `tests/policy_e2e.rs`: network-policy behaviour.
- `tests/cft_close.rs`: Chrome for Testing close behaviour.

## Version and changes

Published version: see [INDEX.md](../../INDEX.md). Version history: [CHANGELOG.md](CHANGELOG.md).
