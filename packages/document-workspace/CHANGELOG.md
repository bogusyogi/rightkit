# Changelog

## 0.2.1

- Fix (security): the legal gate failed open. `AppRoot` skipped the gate (`needsGate = legal !== null && ...`) and opened the workspace when the host passed no `firstRun`, and an unconfigured `configureLegal` left nothing to check. It now fails closed: without `firstRun` or without `configureLegal`, `AppRoot` renders a blocking "Legal configuration missing" screen (role `alert`, a Quit button) and never the workspace. New exports: `legalConfigured()` (`state/legal`), `missingLegalConfiguration()` (`app/AppRoot`); `legalAcceptanceCurrent` is false until `configureLegal` runs. Hosts that configure both (the documented contract) see no change.

## 0.2.0 (unpublished)

Shape review 2026-11-11 (`docs/audits/2026-11-11-shape-matrix/reports/family-documents.md`; design in `reports/documents-redraw-design.md`). Taken on the recommended option, pending the owner's confirmation.

The review judged this package the wrong shape: it was the ViewRight application, with ViewRight's legal gate (window size, palette, wordmark font, agreement copy), its licence and update wiring (app key `viewright`, `viewright.cc` URLs), a module-load name bug and `viewright:*` names built in. 0.2.0 makes it a shell the host configures.

Added:
- `configureDocumentWorkspace(config)` and `workspaceConfig()` (`config` module, root export): `appName`, `id`, `appVersion`, `supportEmail`, `firstRun` (`legal`: the gate's `LegalStepConfig`; `gateWindow`: its fixed size), `settings.about`, `settings.sections`, `backgroundUpdateCheck`. See the README. Values are read where used, so the trust line, agreement sentence and gate app name no longer read the name at module load (the gate said "App" when the host imported the package before naming itself).
- From `@rightkit/document-core`: the PDF editor (`documents/pdf/*`, with the PDF-editor native commands in `documents/pdf/pdfBridge` and the PDF print path in `documents/pdf/pdfPrinting`), the settings sections (`settings/sections/{Editor,FileAssociations,MacFileAssociationsBlock,Theme,Typography}Section`, `onOff`) and `settings/rk-settings.css`, and the workspace-only hooks `app/useCloseFindOnSurfaceChange`, `app/useEditText`, `documents/FullscreenIcon`, `documents/txt/cmScroll`, `documents/useMirroredEditText`, `documents/useSplitResize`. All are on the root barrel except `documents/pdf/pdfBridge`.
- `styles.css` imports pdf.js's `pdf_viewer.css` and `@rightkit/pdf-ui/styles.css`. New dependencies `@rightkit/pdf-ui` and `pdfjs-dist` 6.4.299.

Removed (breaking; these are the host's composition and move to the host):
- `state/updateLicense` (update client, licence client, `APP_VERSION`, `UPDATE_LICENSE_URL`, background update check), `settings/publishedSections` (`UpdatesPanel`, `AboutFrame`, `PublishedLicense`, `LegalPanel`), `settings/sections/UpdateLicenseBlock` and `settings/sections/AboutSection`. A host passes its About page as `settings.about` and its check as `backgroundUpdateCheck`. Dependencies `@rightkit/app-update`, `@rightkit/license`, `@rightkit/legal-ui` and `@tauri-apps/plugin-process` removed.
- The built-in legal gate copy, palette (`#0C6AA5` accent), Tanker wordmark URL and 960x560 size: the host passes them in `firstRun`. Without `firstRun` there is no gate (fail-open; fixed in 0.2.1, which blocks instead).
- "Email to support" sends to `supportEmail`; without it the action is not shown (was `hello@viewright.cc`).

Changed (breaking):
- Every `viewright:*` window and Tauri event is `<id>:*` (`documentEvent`), the print-preview window flag is `?<id>-print-preview`, QA globals are `__<camelId>ResourceCensus` and `__<camelId>ResourceMatrixStatus`, the macOS native PDF owner id is `<id>-native-pdf-<n>` and the bench download `<id>-pdf-bench-<time>.json`. ViewRight keeps its names with `id: "viewright"`.
- `appName` re-exports the one store in `@rightkit/document-core/host`.

One PDF viewer, duplicates removed:
- The range transport is `@rightkit/pdf-ui`'s (`createPdfRangeTransport`, `pdfRangeDocumentInit`); the copy in `pdfBenchTransport` (`HostPdfRangeTransport`, `formatPdfRangeHeader`, `parseContentRangeTotal`, the length probe) is deleted.
- The password dialog is `@rightkit/pdf-ui`'s `PdfPasswordPrompt` (same copy and keys); the local copy is deleted. It is styled by `rk-pdf-password*` and the `--rk-pdf-*` variables instead of the shell's `ActionDialog`, so it looks different until the host sets those variables.
- Not done: the PDF pane still renders pages with its own viewer (`documents/pdf/PdfViewer`) instead of `@rightkit/pdf-ui`'s `PdfViewer` with `pageLayers`. Rebasing it changes how ViewRight's editor draws, edits and saves pages and needs a ViewRight run against the cases in pdf-ui 0.3.0's CHANGELOG; recorded in the design note.

Not verified here: no host run; `pnpm --filter @rightkit/document-workspace build` passes (it failed before this change in `state/updateLicense.ts`, now removed).

## 0.1.2 (unpublished)

- Removed the browser-QA fixtures: `state/explorer` fixture locations and listings, `state/library` fixture recents and pins, the `qaLegal` legal-gate bypass in `AppRoot` and the `isBrowserQaMode()` gates in `SettingsPanel`. The legal gate shows until acceptance is current; the host bridge is native only.
- `getLicenseClient()` always returns the native licence client (`createLicenseClient(nativeIpc)`); the browser-QA mock branch is gone with `@rightkit/license/testing`.
- Type-checks against `@rightkit/app-update` 0.4.3, whose `install` can refuse with `deferred` (#1019): a deferred install is not a licence refusal and still reports the native message, as before.

## 0.1.1

- Ask 5: host contract documented in `README.md`: legal data is passed in with `configureLegal({ manifest, contract })`, product name with `setHostAppName()`, the DOCX host-toolbar patch requirement, and `VITE_RIGHTKIT_BROWSER_QA`. The `@rightkit-host` alias is documented as the test-fixture alias only; the README previously described it as the host's data path, which the code does not use.
- Ask 4: pins `prosemirror-view` 1.42.8 (was 1.42.6).
- Wording fix in the published section comments; no behaviour change.

## 0.1.0

- First release. Multi-format document workspace shell: tabs, sidebar, document router, loaders, savers and actions.
