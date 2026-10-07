# rightkit-browser

Shared CDP browser runtime (Chrome/Edge via chromiumoxide). See the lane API notes
for the CodeRight migration. Real `Input.dispatch*` input, multi-page sessions,
named persistent profiles, headless or headed, `--mute-audio` by default.

Launch resolves Chrome for Testing (`chrome_for_testing_path`): `LaunchOptions::chrome_path`,
then `RIGHTKIT_CHROME_FOR_TESTING`, then app installs and the newest Puppeteer/Playwright cache
build. Missing Chrome for Testing is `BrowserError::ChromeForTestingNotFound` with install
instructions; the system browser (`find_chrome`) is used only with `LaunchOptions::system_chrome()`.
Regular macOS Chrome leaks a code-sign bundle clone per killed instance.
`shutdown()` closes gracefully: CDP `Browser.close`, then SIGTERM to the process group, then
SIGKILL, each with a 5 s bound. `Drop` sends SIGTERM, waits up to 5 s, then SIGKILL. The
temporary profile is removed once Chrome has exited.
