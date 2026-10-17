# Changelog

## 0.3.0 — 2026-10-17

Additive (Orthic-Labs/rightkit#266). `LegalGate` is unchanged.

- `policyGateDocuments(documents, urls?)`: turns the default policy texts rendered by `@rightkit/legal` (`renderDefaultPolicies`) into `LegalGate` documents that render inline as Markdown. Structural input type `DefaultPolicyDocumentLike`; no dependency on `@rightkit/legal`.

## 0.2.3 — 2026-10-16

Additive. Documents without `content` render identically, and the default legend is unchanged.

- `eligibilityLabels.legendVisible` (#174): shows the eligibility legend as visible text (class `rk-legal-gate__legend`, bold 13px, theme text colour) instead of screen-reader only. Default false keeps the `rk-sr-only` legend, byte-identical.

- `LegalGateDocument.content` (string) and `contentFormat` (`"markdown"` default, or `"text"`): the gate renders a bundled document inline in an expandable region under its link. Precedence: `onOpen`, then `content`, then `url`. The built-in Markdown renderer covers headings, paragraphs, lists, rules, bold, italic, code and http(s)/mailto links, and never injects HTML.

## 0.2.2 — 2026-10-11

Additive. With no new prop set, the gate renders the same markup as 0.2.1 (the first-run, re-acceptance and save-error renders are checked against captured 0.2.1 markup in `src/default-render.test.tsx`). The `onAgree` payload and the theme behaviour are unchanged.

- New optional `LegalGate` props:
  - `agreeLabel` (default `"Agree & continue"`) sets the idle text of the primary button. The pending text stays `"Saving…"`.
  - `showQuit` (default `true`) and `quitLabel` (default `"Quit"`) control the Quit button.
  - `tradingNameNotice: string | false` (default: the trading-name line, or `dbaLine` when that is set). A string replaces the line. `false` hides it. The text is legal copy: hiding or replacing it is the app's decision.
  - `eyebrow` (`ReactNode`, absent by default) renders a small line above the heading.
  - `heading` (`ReactNode`) replaces the wordmark heading. Absent keeps the wordmark.
  - `renderError(error: unknown) => ReactNode` replaces the default save-failure text. The result stays inside the same `role="alert"` container. Default: `"Couldn’t save your acceptance. Try again."`.
  - `onError(error: unknown)` is called with the rejection value when `onAgree` rejects or throws, before the error is shown.
  - `footer` (`ReactNode`) renders below the actions and any error message.
- An eligibility option whose `description` is `""` renders no description element. Omitted fields in `eligibilityLabels` still keep the default text, as in 0.2.1.
- Bumped `version` in `package.json` to `0.2.2`.

## 0.2.1

- `onAgree` now receives `{ version, acceptedAt, manifestSha256, eligibilityBasis }`. Chosen compatibility: the object stays the FIRST and only argument (it already was an object, `{ eligibilityBasis }`), so existing callbacks that read `eligibilityBasis` keep working; the three new keys are added. There are no positional arguments to preserve. `acceptedAt` is a UTC ISO-8601 string (`toISOString()`, `YYYY-MM-DDTHH:mm:ss.sssZ`) taken when the user presses Agree (a retry after a failed save takes a fresh one). `version` and `manifestSha256` come from the new optional props of the same name and are `undefined` when the host passes none.
- New optional prop `eligibilityLabels` (`legend`, `individual.{label,description}`, `enterprise.{label,description}`); omitted fields keep the previous text, which is the default.
- New optional prop `lockup` (`ReactNode`) replaces the publisher lockup; `null` hides it; absent keeps the Orthic Labs mark and name.
- Removed the forced light theme. The gate no longer sets `color-scheme: light` (it sets no `color-scheme` and no `data-theme`), so native radios and checkboxes follow the host's `data-theme`/`color-scheme`. `theme` and each of its fields are now optional and accept CSS values or `var(--token)` references; an omitted `bg` leaves the background transparent, an omitted `text` inherits the host `color`, and the other colours derive from the text colour or the platform accent. Hosts passing a full theme render as before apart from the native control scheme. `legal-ui.css` has no hard-coded colours.
- New exports: types `LegalGateAgreement`, `LegalGateEligibilityLabels`.

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
