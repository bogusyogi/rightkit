# Changelog

## 0.1.3 — 2026-10-10

- Test-only: the real-browser e2e harness imports `@rightkit/qa-chrome` directly; the devDependency moves from the `@rightkit/qa` facade (`@rightkit/qa/chrome` re-exports it). No runtime change.

## 0.1.2 — 2026-10-07

- Test-only: the real-browser e2e harness launches Chrome for Testing through `@rightkit/qa/chrome` (`RIGHTKIT_CHROME_FOR_TESTING` overrides; skips with install instructions when missing, never regular Chrome) and closes gracefully (CDP `Browser.close`, SIGTERM, SIGKILL last) with its temp profile removed. No runtime change.

## 0.1.1 — 2026-10-07

- BREAKING for `PdfViewer` users: `dist/PdfViewer.js` no longer imports `./styles.css` (CSS in a JS entry breaks Node ESM hosts). Import `@rightkit/pdf-ui/styles.css` once in the app. Helper-only consumers are unaffected.
