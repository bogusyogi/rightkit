# Changelog

## 0.3.3

Added:
- `@rightkit/settings-ui/shell-adapter` subpath: `createShellBackends`, `METHODS`, `EVENTS`, `EngineShell`, `SettingsShell` and `ShellBackends`. The module describes the engine structurally and imports nothing from `@rightkit/shell`.
- `src/blob-adapter` test and README example: an app-supplied `SettingsBackend` over a single blob.

Changed:
- The shell-backed adapter moved out of `backends.ts`. The root `@rightkit/settings-ui` entry still re-exports it, marked `@deprecated`, so existing imports keep working.
- `@rightkit/shell` is an optional peer (`peerDependenciesMeta`). No file in the package imports it at runtime or in its types.

## 0.3.2

- Republish with `dist/` present. 0.3.1 was published from an unbuilt tree and cannot be imported (exports point at ./dist/index.js); do not pin 0.3.1. `prepack` now builds.

## 0.3.1 — 2026-10-10

- Test-only: the real-browser e2e harness imports `@rightkit/qa-chrome` directly; the devDependency moves from the `@rightkit/qa` facade (`@rightkit/qa/chrome` re-exports it). No runtime change.

## 0.2.7 — 2026-10-08

- `useLicenseSection` returns `loaded`: false until the first read settles (or a read fails, or an action sets the licence), so apps can tell "not read yet" from Free without a flash.
- `useDoctor` accepts `probe: () => Promise<P>`: one shared in-flight call per run, whose report is passed to every check's `run(report)`; a rejection fails the checks that awaited it. The result exposes `report` (the latest settled probe). Without `probe`, behaviour is unchanged. Requested by ScrapeRight.

## 0.2.6 — 2026-10-07

- Test-only: the real-browser e2e harness launches Chrome for Testing through `@rightkit/qa/chrome` (`RIGHTKIT_CHROME_FOR_TESTING` overrides; skips with install instructions when missing, never regular Chrome) and closes gracefully (CDP `Browser.close`, SIGTERM, SIGKILL last) with its temp profile removed. No runtime change.

## 0.2.5 — 2026-10-07

- Model fit kinds `rental_only` and `api` (rightkit-models 0.2.0): labelled "Runs on a rented GPU" / "Runs through an API", and Download is disabled because there is no local backend.

## 0.2.4

Headless (behaviour-only) hooks for apps that keep their own markup. The built-in licence and doctor sections are now views over the same hooks; their rendered output is unchanged (pinned by snapshot tests).

### Added
- `useLicenseSection({ client, beforeDeactivate?, confirmDeactivate?, graceDays?, notAcceptedMessage?, onChange? })` returns `{ license, pro, tier, busy, activating, error, activate(key), deactivate(), requestDeactivate(), confirmPending, cancelDeactivate(), refresh() }`. `pro` is `isPro` from `@rightkit/license`. `deactivate()` always runs (`beforeDeactivate`, then `client.deactivate()`, then `onChange(null)`); `requestDeactivate()` is the confirmation-aware two-step call (first call sets `confirmPending`, which disarms after 4 s or on `cancelDeactivate()`; the second call deactivates; with `confirmDeactivate: false` the first call deactivates). A throwing `beforeDeactivate` aborts and sets `error`. Exports `UseLicenseSectionOptions` and `UseLicenseSectionResult`.
- `useDoctor(checks, { autoRun?, onResult? }?)` returns `{ status, summary: { passed, total, lastRun }, running, runAll(), runOne(id) }` with `DoctorStatus` per check id. A throw or rejection is a failed check; a stale run never overwrites a newer one or fires `onResult`. Exports `UseDoctorOptions`, `UseDoctorResult` and `DoctorStatus`.
- Tests: hook behaviour (`src/hooks.test.tsx`) and snapshots of the built-in licence and doctor markup (`src/output.test.tsx`).

### Changed
- `LicenseClientLike`, `DoctorCheck` and `DoctorResult` now live next to the hooks; the public exports are unchanged.
- The built-in Deactivate button now renders from the hook's `confirmPending` state instead of a private two-step button (same markup and 4 s timeout).

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
