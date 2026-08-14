# legal-ui preview harness (local-only, not published)

Renders the five first-run `LegalGate` variants — the five Right Suite apps,
LIGHT-only (locked 2026-07-17), each with its real light brand tokens from
`.claude/rules/brands.md` and the real Tanker wordmark font
(`preview/fonts/Tanker-400.woff2`, copied from `viewright/public/fonts`; body
falls back to Hanken Grotesk from heardright's public fonts) — for Adrian's
taste-gate approval before any app is wired to this package.

`preview/`, `.preview/`, and this README are excluded from the published
package (`package.json` `files` only lists `dist`, `README.md`, and the
license files).

## Verification

```bash
pnpm --dir tools/rightkit/packages/legal-ui preview:verify
```

This runs each light variant in default, pending, and rejected acceptance
states. It drives selection, acknowledgement, and submit through the shared
QA action engine; waits for the pending or rejected state; injects axe-core;
and requires zero WCAG 2.0/2.1/2.2 A/AA violations. It writes 15 PNGs,
`axe-report.json`, and `contact-sheet.html` to `.preview/verification/`.

## Build + serve

```bash
pnpm --dir tools/rightkit/packages/legal-ui build      # tsc: src -> dist
node tools/rightkit/packages/legal-ui/preview/build.mjs # esbuild: preview/entry.tsx -> preview/dist/entry.js
node tools/rightkit/packages/legal-ui/preview/serve.mjs # static server on 127.0.0.1:4310
```

Or via `.claude/launch.json` config `legal-ui-preview` (Browser pane
`preview_start`).

Open `http://127.0.0.1:4310/?id=<appKey>-light`, e.g. `?id=heardright-light`
(ids keep the `-light` suffix so PNG paths stay stable). Omitting `?id` lists
the five valid ids.

## Screenshot + contact sheet

```bash
node tools/rightkit/packages/legal-ui/preview/shoot.mjs
```

Shoots all five variants via the shared QA headless-Chrome/CDP engine
(`tools/skills/legion/lib/qa-engine/qa.mjs --shot`) into `.preview/<appKey>-light.png`,
then writes `.preview/contact-sheet.html` referencing the five PNGs in a
two-column grid.
