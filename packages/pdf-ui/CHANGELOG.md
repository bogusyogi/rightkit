# Changelog

## 0.1.1 — 2026-10-07

- BREAKING for `PdfViewer` users: `dist/PdfViewer.js` no longer imports `./styles.css` (CSS in a JS entry breaks Node ESM hosts). Import `@rightkit/pdf-ui/styles.css` once in the app. Helper-only consumers are unaffected.
