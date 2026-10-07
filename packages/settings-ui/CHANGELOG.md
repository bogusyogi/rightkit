# Changelog

## 0.2.3

Update-stable seams for the licence panel and a new engine doctor section. All additive; default output is unchanged.

### Added
- Licence: `LicenseOptions.labels` (`LicenseLabels`) covers every visible string: section/status/active-status titles, free/not-activated/invalid/expired badges, trial/subscription/lifetime kind names, free/rejected/active/expired details (the last two are functions of `(kind, until)`), the Deactivate row (title, hint, button, confirm), the Activate row (title, hint, aria label, placeholder, button, in-flight `activating`, `notAccepted`), the purchase row and the search entries.
- Licence: `confirmDeactivate?: boolean` (default true; false deactivates on the first click; `beforeDeactivate` still runs and can abort).
- Licence: `LicenseOptions.ids` (`LicenseIds`): `section` (wraps plan slot and rows in `<div id>`), `licenseKey`, `form`, `activate`, `deactivate`, `status`. Nothing is rendered unless set, so apps can keep `#settings-pro` and `#settings-license-key`.
- `doctorSection(options)` (section id `doctor`, also `standardSections({ doctor })` and `StandardSectionId`): app-supplied `DoctorCheck { id, label, description?, run(): Promise<{ ok, detail?, fix? }> }`, run-all plus per-check re-run, status badges (not run / checking / ok / needs attention), summary, `autoRun`, `onResult`, `DoctorLabels` (all strings, optional `detailsLabel` to wrap the list in `<details>`), `DoctorIds` (`section`, `summary`, `runAll`, `list`, `check(id)`, `rerun(id)`) and `data-check-id` / `data-status` on each row. A throwing check is a failed check; a stale run never overwrites a newer one. Styles in `settings.css` (`.rk-doctor__*`).

## 0.2.2

Update-stable seams so apps keep their own nav, update, licence and About UI. All additive; defaults render as in 0.2.1.

### Added
- `SettingsPanel`: `navFooter` (bottom of the nav), `navHeading` (`ReactNode | false`; takes precedence over `title`, `false` omits the heading), `renderNavItem(section, { active })` (replaces the content of each nav button; the button, `data-section-id`, keyboard navigation and click handling stay) and `hideNav` (no nav; select with `activeId`; adds `rk-settings--no-nav`). Nav buttons now carry `data-active="true"` on the active item (alongside `aria-current="page"` and `.is-active`).
- Updates: `UpdatesClient = AutoUpdateClient | UpdatesAdapter`; function-style `UpdatesAdapter { check(); install(); status? }` (`status` seeds the display on mount, value or function). New exports `UpdatesControls` (the inner status/Install/Restart/Check controls, no Settings row, for embedding in another card; takes `client` or a shared `controller`) and `useUpdates(client)`.
- `UpdatesLabels`: `sectionLabel`, `searchCheck`, `checking`, `installing` (in-flight button text; unset keeps the normal label) and `statusBadge`. Defaults unchanged (`checkNow` "Check now", `installUpdate` "Install update"); set them to "Check for Updates" / "Install and restart" as needed.
- Licence: `LicenseOptions.client` is now the minimal `LicenseClientLike` (`activate`, `deactivate`, and `status()` or `load()`); `createLicenseClient()` still satisfies it. New `beforeDeactivate` hook, awaited before `client.deactivate()`; if it throws the deactivation is aborted and the error is shown.
- About: `card` (replaces the default card; `panels` still render) and `render({ card, panels, info })` (replaces the whole section body).
- Unit tests with jsdom + Testing Library (`src/slots.test.tsx`); devDependencies `jsdom`, `@testing-library/react`, `@testing-library/user-event`, `@testing-library/jest-dom`.

### Fixed
- A failed licence deactivation now shows its error in the Deactivate row (it was set but never rendered while the licence was active).

### Changed
- While an install runs, the update status is no longer replaced by "checking" (the Install button is disabled instead); the section shows the same controls otherwise.

## 0.2.1 — 2026-10-07

- `ModelEntry.removable` (false hides Remove) and `ModelEntry.shared` (shared Orthic store model: "Shared" badge, never removable).

## 0.2.0

### Fixed
- Updates: "Install updates automatically" is shown only when `UpdatesOptions.supportsAutoInstall` is true (default false). It was always shown, even when the update client never auto-installs. Apps whose client sets `shouldAutoInstall` must now pass `supportsAutoInstall: true`.

### Added
- `SettingsSection.badge` (nav pill) and `SettingsSection.heading` (page heading distinct from the nav label); `SectionFrame` export.
- Nav icons for all built-in sections (`sectionIcons`), including Appearance and Legal.
- `LicenseOptions.planSlot`: plan/upsell panel above the licence rows.
- `AboutOptions.panels`: full panels rendered outside the About card (`extra` stays in-card).
- `UpdatesOptions.labels`: override Updates strings.
- `standardSections`: `about` is optional; `include`, `omit` and `override` select/adjust built-ins by id.
- Unit tests (`src/seams.test.tsx`).

### Changed
- Nav label is wrapped in `.rk-settings__label`; CSS tokens use the `--rk-*` prefix.
