# Changelog

## 0.1.1

- Appearance tab reads the palette list from `listPalettes()` and falls back to the `defaultPaletteLabel` host option (default "Default"). The sandbox tier panel names the `sandboxTierEnv` host option (default `RIGHTKIT_SANDBOX_TIER`). The root model picker lists only `direct` catalog groups.
- `appName` re-exports `@rightkit/agent-ui-core/appName`; `invoke` goes through `@rightkit/agent-ui-core/host/tauriHost`.

## 0.1.0

- First release. Settings panes for coding-agent UIs.
