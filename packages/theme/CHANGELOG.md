# Changelog

## 0.2.4

Added:
- `prefersDark()` and `onPrefersDarkChange(cb)`: read the OS dark preference (`(prefers-color-scheme: dark)`) and subscribe to its changes. `onPrefersDarkChange` returns an unsubscribe, falls back to `addListener` on old Safari, and both are SSR-safe (`false` and a no-op when `window` or `matchMedia` is missing). Apps can drop their own `matchMedia` copies (ownership fingerprint `theme.runtime.prefers-dark`).

## Unreleased

Added (M2-G1, CodeRight dark-default contract):
- `themeAttributeValue`, `applyThemeAttribute` and `ThemeAttributeOptions { darkRemovesAttribute }`: with the option, a resolved dark theme removes `data-theme` and only light is written. `themeBootScript(storageKey, { darkRemovesAttribute })` follows the same rule. Default unchanged (`false`: dark writes `data-theme="dark"`; the boot script output is byte-identical without options).
- The persisted store itself (`createThemeStore`) lives in `@rightkit/app-shell`, outside this package: wiring the same option into its `apply` is a separate app-shell change (see the M1-M2 owner-fix report).

## 0.2.3

Added:
- `applyTokens(record, root?)`: sets arbitrary CSS custom properties on inline style, skips `undefined`, and returns an undo that restores previous inline values. Names are validated.
- `aliasTokens(map)`, and `SUITE_ALIASES`, the same map as `@rightkit/app-shell/suite-aliases.css` (a test keeps them identical). `--wordmark` maps to `--rk-wordmark-font` without the CSS file's `inherit` fallback.
- `presetTokens(preset, accentId?)` for theme preset x accent palettes, and `fontTokens({ mono, wordmark })` for the font token hook.
- `applyBrand(brand, mode, root, { applySuiteAliases })`. Default `false`: the flat suite aliases are written only when asked, so apps that own `--ink` or `--panel` keep them.

Fixed:
- `applyBrand` writes its tokens through `applyTokens`, so its undo behaviour is unchanged.

## 0.2.2 — 2026-10-10

- Test-only: the real-browser e2e harness imports `@rightkit/qa-chrome` directly; the devDependency moves from the `@rightkit/qa` facade (`@rightkit/qa/chrome` re-exports it). No runtime change.

## 0.2.1 — 2026-10-07

- Test-only: the real-browser e2e harness launches Chrome for Testing through `@rightkit/qa/chrome` (`RIGHTKIT_CHROME_FOR_TESTING` overrides; skips with install instructions when missing, never regular Chrome) and closes gracefully (CDP `Browser.close`, SIGTERM, SIGKILL last) with its temp profile removed. No runtime change.

## 0.2.0 — 2026-10-07

Breaking:
- Token overrides are emitted as `--rk-<name>` to match @rightkit/app-shell 0.2.0.
- `wordmark` is the product name as written (informational, never emitted). The font stack moved to `wordmarkFont`, emitted as `--rk-wordmark-font`.
