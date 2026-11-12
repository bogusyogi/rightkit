# Changelog

## 0.2.1

- Fix: the models panel no longer offers Rollback. The model store keeps one revision per model since `rightkit-models` 0.7, so the action could only fail with `no_rollback`. `installActions` never returns `'rollback'`; the value stays in its type and the panel still handles it, for hosts that name it.

## 0.2.0

- Changed (breaking): the package is the media-generation panels, not the GenRight app. Removed and left to the app: the app shell (`App`, `main`, `DesktopWorkspace`, `Settings`, `ComingSoon`, window controls, theme wiring, `brand.css`, `shell.css`), onboarding (`Onboarding`, `lib/onboarding`, `lib/setup-steps`, `lib/doctor`, `lib/platform`), legal documents (`legal/documents`, `LegalDocument`), licence (`lib/license`, `LicensePlan`), app updates (`lib/app-update*`) and the KDP workflow (`Kdp`, `kdp-*`). A host builds those from `@rightkit/react-shell`, `@rightkit/onboarding`, `@rightkit/settings-ui`, `@rightkit/legal-ui`, `@rightkit/license` and `@rightkit/app-update`; the dependencies on `@rightkit/app-update`, `legal`, `legal-ui`, `license`, `theme` and the Tauri process and updater plugins are gone.
- Changed (breaking): every panel reaches the engine through the host's client: `setMediaGenClient(client)` (`request`, `approveDraft`, `assetUrl`, `isDesktop`); the default is `tauriClient`. Removed: the 4,003-line browser mock of the engine (`lib/browser-mock-api`, `loadBrowserMock`) and the panel mocks (`production-mock`, `voices-mock`, `character-mock`, `image-quality-mock`, `video-quality-mock`); a browser preview passes its own client.
- Removed (breaking): `studio-data/` (the owner's brand list and copies of `rightkit-prompt-recipes` data). Brands are the host's: `setHostBrands(brands)`; the recipe catalogue reads node edges from the engine (`depends_on`, rightkit-prompt-recipes 0.2.0). `Brand` and the video `Venture` are strings.
- Removed (breaking): `components/MaskBrush` and the painting half of `lib/mask-export`, a copy of `@rightkit/media-ui` `MaskPainter`. `components/MaskEditor` is the mask step over `MaskPainter` (invert through `exportPng({ invert })`, media-ui 0.3.5); the param helpers are `lib/mask-params`.
- UI storage keys are `media-gen.*` (were `genright.*`); Production's fallback navigation event is `media-gen:navigate`.
- Taken on the recommended option of the 2026-11-11 shape review, pending the owner's confirmation (docs/audits/2026-11-11-shape-matrix/reports/genright-redraw.md).

## 0.1.0

- First release. Generative media studio UI: generate, jobs, gallery, recipes, voices and production panels.
