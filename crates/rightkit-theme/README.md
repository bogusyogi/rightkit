# rightkit-theme

Resolved theme tokens for UIs that are not web views. `@rightkit/theme` resolves a `brand.json` into CSS variables, but its resolver (the derived dark accent and the WCAG contrast repair) exists only in TypeScript, so overlay-win, Ember and the SwiftRight hosts hand-coded their colours. This crate is that resolver in Rust, with per-mode W3C design-token JSON as its output.

Catalog capability: `theme-tokens`. Not yet published (0.1.0, unreleased). Rightkit#960.

## Command

```sh
rightkit-theme generate --json brand.json                 # tokens to stdout
rightkit-theme generate --json brand.json --out tokens.json
rightkit-theme generate --json brand.json --out tokens.json --check   # exit 1 when the file differs
```

The output is a W3C design-token (DTCG) document:

```json
{
  "$description": "...",
  "light": { "ink": { "$type": "color", "$value": "#231F1A" }, "accent": { "$type": "color", "$value": "#..." } },
  "dark": { "...": {} },
  "motion": { "$type": "duration", "dur-1": { "$value": "120ms" } }
}
```

Each mode group holds every suite colour (`chrome`, `plane`, `elevated`, `panel`, `sunk`, `ink`, `ink-2`, `ink-3`, `ok`, `warn`, `bad`, `stage`, `success*`, `warning*`, `star`, `accent-ink`, plus any `tokens.<mode>` override of the brand file) and the brand palette (`brand-mark`, `brand-action`, `brand-ink`, `brand-soft`, `brand-border`, other `brand-*` keys, and `accent`, an alias of `brand-action`). Translucent fills keep the `rgba(...)` text the suite defaults use. Names are the CSS names without `--rk-`.

## Library

- `parse_brand(json)`: validates the same rules as `parseBrand` (names `brand(-word)*`, hex values, `tokens` limited to the overridable suite names).
- `resolve_brand(&brand)`: `ResolvedBrand { overrides, effective, palette }` per `Mode`. Light accent = `brand-action` else `brand`. Dark accent = `dark["brand-action"]`, else `dark.brand`, else the brand colour lifted to 4.5:1 on the dark plane (a passing brand is kept). `accent-ink` is the better of white and `#1A1208` on the accent.
- `design_tokens(&brand)` / `design_tokens_json(&brand)`: the document above, deterministic (sorted keys).
- `color`: `parse_hex`, `to_hex`, `luminance`, `contrast_ratio`, `mix`, `readable_on`, `ensure_contrast` (the 24-step bisection with hex rounding at every step, as in TypeScript).

## Scope

The brand file is the one source. `@rightkit/theme` stays the owner for web CSS; both read the same `brand.json` and the resolution rules are a direct port of `packages/theme/src/brand.ts` and `color.ts`. A port drifts if one side changes alone: change both in one commit. Contrast checking of a finished stylesheet, hard-coded colour scanning and the runtime `applyBrand` are web-only and stay in TypeScript. No unsafe code.
