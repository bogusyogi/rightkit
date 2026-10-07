# rightkit-browser

Version **0.2.1**. Shared CDP browser runtime (Chrome/Edge via chromiumoxide).
Real `Input.dispatch*` input, multi-page sessions, persistent profiles, headless
or headed. Defaults remain headless, muted & temporary-profile-based.

Launch resolves Chrome for Testing (`chrome_for_testing_path`): `LaunchOptions::chrome_path`,
then `RIGHTKIT_CHROME_FOR_TESTING`, then RightKit-managed versions, app installs
& newest Puppeteer/Playwright cache builds. Managed versions use numeric version
order, newest first, matching `@rightkit/qa-chrome` 0.1.2:

| Platform | Managed executable |
|---|---|
| macOS ARM64 | `~/Library/Application Support/Orthic Labs/shared/chrome-for-testing/<version>/chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing` |
| macOS x64 | `~/Library/Application Support/Orthic Labs/shared/chrome-for-testing/<version>/chrome-mac-x64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing` |
| Windows | `%LOCALAPPDATA%\Orthic Labs\shared\chrome-for-testing\<version>\chrome-win64\chrome.exe` |
| Linux | `$XDG_DATA_HOME/Orthic Labs/shared/chrome-for-testing/<version>/chrome-linux64/chrome` (data home defaults to `~/.local/share`) |

`RIGHTKIT_CHROME_FOR_TESTING_ROOT` overrides only managed root; explicit
executable overrides retain priority. `CHROME_FOR_TESTING_ROOT_ENV` exports its name.
Missing CfT returns `BrowserError::ChromeForTestingNotFound` with install instructions.

`LaunchOptions::system_chrome()` retains CfT preference & falls back to
`find_chrome()` only when CfT resolution fails. New `system_chrome_only()`
resolves exclusively through `find_chrome()` (including its `CHROME` override),
ignores `chrome_path` & CfT discovery, & returns `BrowserError::ChromeNotFound`
if no system Chrome/Chromium/Edge exists. Last selection builder wins.
Regular macOS Chrome leaks a code-sign bundle clone per killed instance.

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

`profile_dir(path)` selects `ProfileSpec::Directory(PathBuf)`. Launch validates
absolute path, existence & directory type without creating it. Any existing
directory is accepted, including real Chrome user-data directories, spaces,
Unicode & filesystem-supported characters; named-profile validation stays
exclusive to `named_profile(root, name)`. Paths remain native `OsString` argv
elements for `--user-data-dir`, with OS quoting handled by `Command`.

Caller owns directory access; this mode creates no RightKit profile lock.
Default downloads use `<profile>/rightkit-downloads` (created at launch), or
set `LaunchOptions::download_dir` explicitly. Profile directory never receives
temporary-profile cleanup on shutdown, cancellation, failed launch or drop.

`shutdown()` closes gracefully: CDP `Browser.close`, then SIGTERM to the process group, then
SIGKILL, each with a 5 s bound. `Drop` sends SIGTERM, waits up to 5 s, then SIGKILL. The
temporary profile is removed once Chrome has exited.
