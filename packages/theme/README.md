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
| `@rightkit/theme` | Browser-safe modules, including the token helpers `applyTokens`, `aliasTokens`, `SUITE_ALIASES`, `presetTokens`, `fontTokens` (see Token helpers below): colour helpers (`parseHex`, `contrastRatio`, `readableOn`, `ensureContrast`), brand (`parseBrand`, `resolveBrand`, `SUITE_DEFAULTS`), CSS (`generateCss`, `modeVars`), checks (`checkBrandContrast`, `scanHardcodedColors`, `checkGeneratedCss`), runtime (`applyBrand`, `brandFromAccent`, `themeBootScript`) |
| `@rightkit/theme/node` | Node-only: `readBrand`, `generateFile`, `checkBrandFile`, `checkLogo`, `scanFiles`, and a re-export of `checkGeneratedCss`. Uses `node:fs` and `node:path`. Do not import it in a browser bundle. |
| `rightkit-theme` (bin) | `generate <brand.json> --out <file.css> [--format suite\|tailwind\|both] [--check] [--allow-contrast-failures]`, `contrast <brand.json>`, `tokens <file...>` (hard-coded colour scan) |

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

## Token helpers

Apps keep their own CSS variable names (ViewRight's `--ink`, `--paper`, a theme preset x accent palette, a font stack) and set them without touching anything else:

```ts
import { applyTokens, aliasTokens, presetTokens, fontTokens, SUITE_ALIASES } from "@rightkit/theme";

const undo = applyTokens({
  ...presetTokens(activePreset, accentId),        // { "--ink": "#101216", "--copper": "#0C6AA5", ... }
  ...fontTokens({ mono: "Menlo, monospace" }),    // --rk-font-mono
  ...aliasTokens({ "--focus": "--rk-focus" }),    // unprefixed name -> suite token (a name the preset does not set; later spreads win)
});
undo(); // restores the previous inline value of each name it set
```

- `applyTokens(record, root?)` writes custom properties on `root`'s inline style, skips `undefined` values and returns an undo function. Names must match `--name`.
- `aliasTokens(map)` returns `var(--target)` references for unprefixed names. `SUITE_ALIASES` is the same map as `@rightkit/app-shell/suite-aliases.css`; a test in this package keeps them in step.
- `presetTokens(preset, accentId?)` merges an accent over a preset's base tokens.
- `fontTokens({ mono, wordmark })` maps font stacks onto `--rk-font-mono` and `--rk-wordmark-font`.

`applyBrand(brand, mode, root, { applySuiteAliases: true })` also writes the flat suite aliases. It is off by default, because writing `--ink` or `--panel` would overwrite an app that owns those names.

## Known issues

- `./node` imports Node built-ins. Browser bundles must import only the root entry.
