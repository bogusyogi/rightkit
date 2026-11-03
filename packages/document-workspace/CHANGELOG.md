# Changelog

## 0.1.2 (unpublished)

- Removed the browser-QA fixtures: `state/explorer` fixture locations and listings, `state/library` fixture recents and pins, the `qaLegal` legal-gate bypass in `AppRoot` and the `isBrowserQaMode()` gates in `SettingsPanel`. The legal gate shows until acceptance is current; the host bridge is native only.
- `getLicenseClient()` always returns the native licence client (`createLicenseClient(nativeIpc)`); the browser-QA mock branch is gone with `@rightkit/license/testing`.

## 0.1.1

- Ask 5: host contract documented in `README.md`: legal data is passed in with `configureLegal({ manifest, contract })`, product name with `setHostAppName()`, the DOCX host-toolbar patch requirement, and `VITE_RIGHTKIT_BROWSER_QA`. The `@rightkit-host` alias is documented as the test-fixture alias only; the README previously described it as the host's data path, which the code does not use.
- Ask 4: pins `prosemirror-view` 1.42.8 (was 1.42.6).
- Wording fix in the published section comments; no behaviour change.

## 0.1.0

- First release. Multi-format document workspace shell: tabs, sidebar, document router, loaders, savers and actions.
