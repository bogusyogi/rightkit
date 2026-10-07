# Changelog

## 0.1.3 — 2026-10-07

- Works under a strict `style-src 'self'` CSP: import `@rightkit/legal-ui/legal-ui.css` and the gate skips its runtime `<style>` injection (detected via `--rk-legal-ui-css`). Without the import the gate still injects, which needs `'unsafe-inline'`; a strict CSP silently blocked that and the gate lost its layout (CutRight).
- The Tanker wordmark font loads through the FontFace API (governed by `font-src`, not `style-src`).

## 0.1.2 — 2026-10-07

- The gate always renders `color-scheme: light`, so native radios and checkboxes stay light under a dark app theme.
- `LegalGateDocument.onOpen` opens a bundled copy for apps without a hosted legal site; `url` is now optional.
