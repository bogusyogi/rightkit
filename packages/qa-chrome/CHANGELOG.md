# Changelog

## 0.1.2 — 2026-10-08

- `rightkit-qa-chrome install` (bin) downloads the current Chrome for Testing channel build straight from Google's Chrome for Testing release list into the RightKit-managed folder (`<app data>/Orthic Labs/shared/chrome-for-testing/<version>/`, override `RIGHTKIT_CHROME_FOR_TESTING_ROOT`); `install.json` records the archive digest. No Puppeteer or Playwright needed. Also `path` and `list`.
- The resolver checks the managed folder first, newest version first; app installs and the Puppeteer/Playwright caches remain legacy fallbacks. Found by ViewRight: a stale cached 152 sat three majors behind system Chrome 155 and changed srcdoc behaviour.
