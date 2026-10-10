# Changelog

## 0.2.0 — 2026-10-09

- Peer range widened: `react` `>=18 <20` to `>=18 <21` (`ca41ba38`). Widening only; consumers change nothing.
- No breaking change in code. Between 0.1.3 and 0.2.0 no export, prop, type or CSS token changed: `git diff 01b8174f ca41ba38 -- packages/legal-ui` touches only `README.md`, `package.json` and `rightkit-ownership.json`. The minor bump is a version label (`ca41ba38`).
- README rewritten (`30e300e4`): entry points table, and `LegalGate` documented as display-only. It renders the `documents` and `agreementStatement` the host passes in and has no dependency on `@rightkit/legal`.
- New `rightkit-ownership.json`: a declaration-only capability record for `legal-ui`, with no implementation fingerprint (`7acf96a8`).

## 0.1.3 — 2026-10-07

- Works under a strict `style-src 'self'` CSP: import `@rightkit/legal-ui/legal-ui.css` and the gate skips its runtime `<style>` injection (detected via `--rk-legal-ui-css`). Without the import the gate still injects, which needs `'unsafe-inline'`; a strict CSP silently blocked that and the gate lost its layout (CutRight).
- The Tanker wordmark font loads through the FontFace API (governed by `font-src`, not `style-src`).

## 0.1.2 — 2026-10-07

- The gate always renders `color-scheme: light`, so native radios and checkboxes stay light under a dark app theme.
- `LegalGateDocument.onOpen` opens a bundled copy for apps without a hosted legal site; `url` is now optional.
