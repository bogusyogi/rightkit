# Changelog

## 0.1.2 (unpublished)

- Removed (breaking, Adrian, 2026-11-03: "no dummy data, no fucking made up shit. real data."): the browser-QA fixture layer. `BROWSER_QA_BUILD`, `isBrowserQaMode()`, `qaReloadPdfBytes()`, every fixture branch of `state/tauriBridge` and the `state/browserQa`, `browserQaText`, `browserQaDocx` and `browserQaPdf` modules (and their `index` exports) are gone. Outside the Tauri webview the bridge keeps its plain no-op returns.
- Removed (breaking, Adrian, 2026-11-03: no dummy data): the in-memory browser-QA licence client. `browserQaLicenseClient()` is gone from `state/tauriBridge`, `state/browserQaLicense` is deleted, and the `@rightkit/license` dependency with it (`@rightkit/license/testing` no longer exists).

## 0.1.1

- Ask 1: the host bridge (`state/tauriBridge`), diagnostics (`state/diagnostics`) and platform modules are the single copy the document editors import. Hosts delete their own copies. Host-facing changes a host must adopt:
  - Browser-QA build switch is `VITE_RIGHTKIT_BROWSER_QA=1` (was the host-named variable). A host QA build sets this variable; without it the QA fixtures are dropped.
  - `parseViewRightError` is now `parseHostError`.
  - The file-association field `viewrightIsPreferred` is now `hostIsPreferred`.
  - Diagnostics allowlist adds `pdf-editor:viewer-load`.
- Ask 2 and 3 are in `@rightkit/markdown-editor` 0.1.1; ask 4 pins are in `@rightkit/docx-editor` 0.1.1 and `@rightkit/html-editor` 0.1.1.
- The accent preset `blue` reads its name from `hostAppName()` when displayed, so `setHostAppName()` at startup takes effect.
- Wording fix in the toolbar primitives comment; no behaviour change.

## 0.1.0

- First release. Shared base for the document editors: platform bridge, settings, state, PDF helpers and the format registry.
