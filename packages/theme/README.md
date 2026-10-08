# @rightkit/theme

Brand file to CSS-token generator for Right Suite apps. It handles light, dark and system modes, a derived dark accent, WCAG contrast checks, hard-coded colour scanning and runtime brand application.

| Item | Value |
|---|---|
| Where it runs | Build tooling (Node CLI and `./node` helpers), and app webview UI (`applyBrand`, runtime) |
| Rust pair | None |
| Version | see INDEX.md |
| License | MIT OR Apache-2.0 |

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/theme` | Browser-safe modules: colour helpers (`parseHex`, `contrastRatio`, `readableOn`, `ensureContrast`), brand (`parseBrand`, `resolveBrand`, `SUITE_DEFAULTS`), CSS (`generateCss`, `modeVars`), checks (`checkBrandContrast`, `scanHardcodedColors`, `checkGeneratedCss`), runtime (`applyBrand`, `brandFromAccent`, `themeBootScript`) |
| `@rightkit/theme/node` | Node-only: `readBrand`, `generateFile`, `checkBrandFile`, `checkLogo`, `scanFiles`. Uses `node:fs` and `node:path`. Do not import it in a browser bundle. |
| `rightkit-theme` (bin) | `generate <brand.json> --out <file.css> [--format suite\|tailwind\|both] [--check]`, `contrast`, `tokens` |

Peer dependencies: none.

## Example

```ts
import { readFileSync } from "node:fs";
import { parseBrand, generateCss } from "@rightkit/theme";

const brand = parseBrand(JSON.parse(readFileSync("brand.json", "utf8")));
const css = generateCss(brand, { format: "suite" });
```

```sh
pnpm exec rightkit-theme generate brand.json --out src/theme.css --check
```

## Known issues

- `./node` imports Node built-ins. Browser bundles must import only the root entry.
