# Changelog

## 0.1.0 (unpublished)

Issue #960 (resolved theme tokens for non-web UIs).

- New crate. `parse_brand`, `resolve_brand` and the colour maths are ported from `@rightkit/theme` (`brand.ts`, `color.ts`): derived dark accent, 4.5:1 contrast repair, `accent-ink`.
- `design_tokens` and `design_tokens_json` emit per-mode W3C design-token JSON; the `rightkit-theme generate --json` binary writes it, with `--out` and `--check`.
- Not covered: the stylesheet generator, the contrast and hard-coded-colour checks and the runtime `applyBrand`, which stay in `@rightkit/theme`.
