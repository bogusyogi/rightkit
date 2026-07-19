# @rightkit/legal-ui

The one first-run / re-acceptance legal gate screen for Right Suite desktop apps.

`LegalGate` is a themeable, self-contained React component: full-window app-base
background, a calm single column with a centered Orthic Labs + app-wordmark
header stack, a quiet list of document links, an eligibility choice (individual
vs enterprise), one agreement checkbox, and Agree / Quit. No external CSS or UI
framework — theming comes entirely from the `theme` prop, and fonts are supplied
by the host app (via `theme.fontDisplay` / `theme.fontBody`).

**Light-only (locked 2026-07-17):** the gate always renders on the app's LIGHT
base tokens from brands.md, regardless of the app's own theme setting. The
theme type has no mode switch.

**Wordmark face is LOCKED to Tanker suite-wide.** Hosts must ship Tanker and
list it first in `theme.fontDisplay` — or pass `wordmarkFontUrl` (URL of the
Tanker woff2 in the host bundle) and the component registers the @font-face
itself. System-font fallbacks for the wordmarks are a defect, not a degrade.

`parentLogoSrc` is the drop-in slot for the Orthic Labs logo mark (renders
centered above the "ORTHIC LABS" wordmark). No mark exists yet — omit it until
one ships.

This package contains no legal prose, no manifest validation, and no entitlement
logic. It renders whatever `documents` / `agreementStatement` the host app passes
in. Manifest shape and acceptance-receipt logic live in `@rightkit/legal`; this
package is display-only and knows nothing about that package's types.

## Usage

```tsx
import { LegalGate } from "@rightkit/legal-ui";

<LegalGate
  theme={{
    bg: "#F7F3EC",
    surface: "#FFFFFF",
    border: "#E4DCDA",
    text: "#272322",
    muted: "#686160",
    accent: "#DF3400",
    accentContrast: "#FFFFFF",
    fontDisplay: "Tanker, sans-serif",
    fontBody: "'Hanken Grotesk', sans-serif",
  }}
  wordmarkFontUrl="/fonts/Tanker-400.woff2"
  appName="HeardRight"
  trustLine="HeardRight transcribes and listens for its wake word on your device — AI features run only if you add your own API key."
  documents={[
    { title: "License", url: "https://heardright.app/legal/license/" },
    { title: "End User License Agreement", url: "https://heardright.app/legal/eula/" },
  ]}
  agreementStatement="I have read and agree to the HeardRight License, End User License Agreement, Acceptable Use Policy, and Product Schedule."
  variant="first-run"
  onAgree={({ eligibilityBasis }) => submitAcceptance(eligibilityBasis)}
  onQuit={() => quitApp()}
/>
```

## Design bar

- Full-window solid `theme.bg`. No gradients, no cards-on-cards, no progress bar, no step counter.
- One centered column, `max-width: 560px`. The Orthic Labs + wordmark header
  stack is horizontally centered; everything below it stays left-aligned.
- `theme.accent` appears in exactly two places: the wordmark's "Right" and the Agree button.
- `variant="re-acceptance"` swaps the headline to a "terms changed" statement — no onboarding/welcome framing.

## Preview

`preview/` is a local-only static harness (not published — excluded from `files`)
that renders all five Right Suite apps, light-only, against their real brand
tokens with the real Tanker wordmark font. See `preview/README.md`.
