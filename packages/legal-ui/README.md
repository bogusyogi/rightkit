# @rightkit/legal-ui

The first-run and re-acceptance legal gate screen for Right Suite desktop apps. `LegalGate` renders the document links, an eligibility choice (individual or enterprise), one agreement checkbox, and Agree and Quit buttons.

| Item | Value |
|---|---|
| Where it runs | App webview UI (React) |
| Rust pair | None |
| Version | see INDEX.md |
| License | MIT OR Apache-2.0 |

`LegalGate` is display-only. It renders the `documents` and `agreementStatement` the host passes in. It has no dependency on `@rightkit/legal` and does no manifest validation or receipt logic.

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/legal-ui` | `LegalGate`; types `LegalGateDocument`, `LegalGateEligibilityBasis`, `LegalGateProps`, `LegalGateTheme`, `LegalGateVariant` |
| `@rightkit/legal-ui/legal-ui.css` | Stylesheet. Import it under a strict CSP. |

Peer dependency: `react` `>=18 <20`.

## Example

```tsx
import { LegalGate } from "@rightkit/legal-ui";
import "@rightkit/legal-ui/legal-ui.css";

<LegalGate
  theme={{
    bg: "#F7F3EC", surface: "#FFFFFF", border: "#E4DCDA", text: "#272322",
    muted: "#686160", accent: "#DF3400", accentContrast: "#FFFFFF",
    fontDisplay: "Tanker, sans-serif", fontBody: "'Hanken Grotesk', sans-serif",
  }}
  appName="Example App"
  trustLine="Example trust statement."
  documents={[{ title: "License", url: "https://example.com/license/" }]}
  agreementStatement="I have read and agree to the License."
  variant="first-run"
  onAgree={({ eligibilityBasis }) => saveAcceptance(eligibilityBasis)}
  onQuit={() => quit()}
/>
```

Props: `theme`, `appName`, `trustLine`, `documents`, `agreementStatement`, `variant`, `onAgree`, `onQuit`, and optional `dbaLine`, `parentLogoSrc`, `wordmarkFontUrl`. `onAgree` may return a Promise. While it is pending the button shows `aria-busy`. A rejection or throw shows an alert and lets the user retry.

## Known issues

- Peer React is `<20`, while `doc-render`, `i18n`, `media-ui` and `pdf-ui` accept `<21` (npm audit 2026-10-08).
- Under a strict CSP without `'unsafe-inline'`, the gate injects no style element only when `legal-ui.css` is imported. Without the import, the gate injects its layout at runtime and needs `style-src 'unsafe-inline'`.
