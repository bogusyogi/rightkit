# Changelog

## 0.7.0 — unpublished staging

BREAKING: the licence section and hooks move from `StoredLicense` to the `LicenseView` of `@rightkit/license` 0.3.

- BREAKING: peer `@rightkit/license` is `>=0.3.0 <0.4`. 0.2.x `StoredLicense` callers are no longer claimed.
- BREAKING: `licenseSection`, `useLicenseSection`, `LicenseOptions`, `UseLicenseSectionOptions`, `LicenseClientLike` and `onChange` take and return `LicenseView`. `entitlements.tier` and `entitlements.features` become `view.tier` and `view.features`.
- BREAKING: `LicenseClientLike.load` is removed; read the view with `status()`. `activate(key)` and `deactivate()` must resolve a `LicenseView`, and `refresh()` on the hook resolves the view (or null).
- BREAKING: `graceDays` is removed from `licenseSection` and `useLicenseSection`. Expiry and grace come from `view.lifecycle`, computed natively; the renderer no longer reads the local clock or calls `classifyLifecycle`.
- BREAKING: status and lifecycle text come from the package's fixed message codes (`licenseMessage`, `LICENSE_MESSAGES`). Server text is never rendered. `LicenseLabels.notActivated` is removed; `LicenseLabels.revoked` and `LicenseLabels.attention` are added.
- BREAKING: an error shows only the fixed text of a typed `LicenseClientError`, or the package's generic text for any other error. Raw error messages are no longer shown.
- Behaviour: `deactivate()` applies the view the client returns, so `onChange` receives that view (not `null`).
- Additive: "Verify purchase" (`labels.verify`, `ids.verify`) shows in `licenseSection` and `useLicenseSection.verify()` exposes the action when the client has `refresh()` and the view's lifecycle is `verification_required`, `migration_required`, `lifetime_provisional` or `clock_repair_required`. It calls `refresh()`, applies the returned `.view` like activate, disables while the call runs and shows only the fixed `LicenseClientError` text.
- Additive: `licenseSection` shows `Confirm by <date>` when `view.confirmationDueAt` is set and `Could not verify online since: <date>` when `view.staleSince` is set. Times are Unix milliseconds, shown as a short local date. `labels.confirmBy` and `labels.unverifiedSince` reword the prefixes.
- Behaviour: the Deactivate row shows while the stored revision is `active` (`view.revisionState`), so an expired or grace licence can still be removed.
- Tests use `LicenseView` fixtures (`src/__fixtures__/license-view.ts`, excluded from the build). The e2e harness routes the licence commands through the test host's rpc.
- BREAKING (`/shell-adapter`, PK04-6): the adapter targets the Tauri commands that `rightkit-tauri-shell` host services register, not engine methods. `createShellBackends` takes a `{ invoke, listen }` transport (`tauriTransport()` of `@rightkit/tauri-shell`) instead of an `engine` bridge. `EngineShell` is removed; `HostTransport` replaces it, `SettingsShell` is now its alias.
- BREAKING: `METHODS` now holds `rightkit_settings_{describe,get,patch,reset,subscribe,unsubscribe}` and `rightkit_secrets_{presence,set,clear}`. The `rightkit.settings.*`, `rightkit.secrets.*` and `rightkit.models.*` engine names are gone, and `EVENTS.settingsChanged` is `rightkit://settings-changed` (payload `{ keys, revision }`).
- BREAKING: `ShellBackends.models` and `EVENTS.modelsEntry` are removed. The host registers no models command, so the adapter has none; apps pass their own `ModelsBackend` to the Models section.
- Additive: `ShellBackends.settings` also has `describe()` and `reset(keys, expectedRevision?)`; `subscribe` attaches the listener, subscribes the invoking window, and its unsubscribe reverses both.
- Additive: `SecretsBackend.set(ref, value, intent?)` with `WriteIntent` (`"create"` refuses a stored key, `"replace"` overwrites; the shell adapter defaults to `"replace"`). There is no secret getter or export; `createMemorySecrets` honours `intent`.
- Additive: `SettingsSnapshot` has optional `schemaVersion` and `warning`. `errText` shows the `message` of a host error envelope instead of `[object Object]`.
- Tests: the adapter test and the e2e host use the registered command names; the e2e harness supplies its own models backend over the test host.

## 0.6.1 (2026-10-19)

- Makes theme peer optional for React-only core consumers.
- `useSettings`: each effect run now cancels its own load, load failure and subscription callback, and a load that is no longer the newest (or that predates a completed write) is discarded, so a StrictMode replay, an out-of-order reply, or a load that settles after a completed write can no longer overwrite newer values, roll back the revision or show a stale error.

## 0.6.0 (2026-10-17)

- Updates section: show the new @rightkit/app-update `staged` status (downloaded, waiting for the install go-ahead).

## 0.5.0

Additive (Orthic-Labs/rightkit#285). The root and `/standard` entries export what they did.

- New entry `@rightkit/settings-ui/appearance` (`appearanceSection`, `AppearanceOptions`). It imports `@rightkit/react-shell` and `@rightkit/theme` but not `@rightkit/license`, `logs` or `app-update`, so an app without licensing uses `/core` plus `/appearance`. `appearanceSection` moved to `src/appearance.tsx`; `sections.tsx` re-exports it, so `/standard` and the root are unchanged.
- `@rightkit/license` is now an optional peer. Only the root and `/standard` entries import it (the licence section and licence hooks); `/core` and `/appearance` never do.

## 0.4.5

Additive (#174). Defaults equal 0.4.4: the default DOM and classes are unchanged.

- `SettingsPanel` `keepMounted`: sections the user has opened stay mounted, hidden, when another is selected, so their state survives switching. Kept sections sit in `<div class="rk-settings__kept">`.
- `SettingsPanel` `paint={false}`: adds `rk-settings--no-paint` (`color: inherit; background: none`), so the frame no longer paints `--rk-ink` and `--rk-plane`.
- `SettingsPanel` `nativePreSize`: adds `rk-settings--native-pre` (`font-size: medium` on `<pre>`, except `.rk-diag`), restoring the browser's monospace size that a length `--rk-settings-font-size` disables.

## 0.4.4

Additive. Defaults equal 0.4.3; the default DOM, classes and search results are unchanged, pinned by the existing snapshot tests and by `src/primitives-hooks.test.tsx`.

Added:
- `SettingRow`: `lead` (rendered before the text, in its own column, adds `rk-sr--lead`), `titleClassName`, `hintClassName`, `bareControl` (render the control without the `rk-sr__control` wrapper).
- `SettingsGroup`: `titleClassName`, `hintClassName` (the description), `rowsClassName` (the rows container).
- Custom properties, each defaulting to the 0.4.3 value: `--rk-sg-margin`, `--rk-sg-background`, `--rk-sg-border-radius`, `--rk-sg-shadow`, `--rk-sr-gap`, `--rk-sr-padding`, `--rk-sr-border`.
- `enabled` (default `true`) on `SettingsSearchProvider` and on `useSettingsRow`, `useSettingsItem`, `useSettingsHit`, `useSettingsGroup` and `useSettingsSection`. `false` makes the subtree inert: no registration, no store update, no subscriber render.
- `useSettingsGroup({ hideOnlyWhileSearching })` and `useSettingsSection({ hideOnlyWhileSearching })`, default `false`. `true` hides an empty group or section only while a query is active.
- A development-build console warning, once per page, for a row whose `label` is not a string and has no `searchText`.
- Type `SettingsGroupOptions` on the headless entry.

Changed:
- `SettingsPanel` skips search registration while search is off: no built-in field and no controlled `query`. Nothing it renders changes.

Headless check: the existing hooks already let a host render its own `<b>` for a row title and `<small>` for a hint (`mark()` adds no element), so no headless hook was added for that.

## 0.4.3

Headless follow-up from Pulse's port of its hub onto the headless hooks. Additive: the styled parts, every default and the 0.4.1 default markup (pinned by `src/default-markup-0.4.1.test.tsx`) are unchanged.

Added:
- `useSettingsGroup()` and `useSettingsSection()` return `hidden` (true while empty, for a host that keeps one fixed tree), `props` (`hidden`, `style: { display: "none" }` and `data-empty` while empty, spread on the host element) and `mark(text)` (highlights a title without a second hook).
- `useSettingsHit({ text, keywords? })` returns `{ matches, mark }` for a match that is neither a row nor an item. It counts in `count`, `results` (kind `"hit"`) and the status sentence, and toward the visible count of the group or section it sits in. `SettingsSearchKind` and `SettingsSearchResult["kind"]` gain `"hit"`.
- `SettingsSearchProvider` `wrapHighlight` (default `true`). `false` renders a function `highlight`'s output unwrapped, in a keyed fragment; `true` keeps the `display: contents` span, which is documented as the keyed wrapper for each piece.
- Types `SettingsHiddenProps`, `SettingsItemOptions`, `SettingsHitOptions`, `SettingsHitState`.

Docs: the README stable-tree example replaces the wrapper-swapping one, and states that swapping wrappers remounts children. The "Open section" example uses `Button` from `@rightkit/app-shell/react`.

## 0.4.2

Headless search for hosts that keep their own markup and classes, and four additive options on the styled parts. Requested by Pulse, whose settings markup is fixed by its locked UI and cannot adopt the styled search parts. Every addition is opt-in: the default DOM, classes, attributes and search results are byte-identical to 0.4.1, pinned by the existing snapshot tests (unedited) and by `src/default-markup-0.4.1.test.tsx`, which asserts literal HTML captured from 0.4.1.

Added:
- `@rightkit/settings-ui/headless` (new entry; imports only `react`, no stylesheet, no styled component): `SettingsSearchProvider`, `useSettingsSearch`, `useSettingsRow` (`{ matches, hidden, props, mark }`), `useSettingsItem` (the same, for content that is not a row), `useSettingsGroup` (`{ key, visibleRowCount, empty }`) with `SettingsGroupScope`, `useSettingsSection` (`{ key, visibleRowCount, empty }`) with `SettingsSectionScope` (`scopeKey`), `useSettingsSearchField` (`{ value, onChange, clear, active, count, status }`), `useSettingsSearchStatus`, `useSearchKeys`, `highlightText`, `parseSearchTerms`, `defaultAnnouncement`, and their types.
- `SettingsGroup`: `className` (added after `rk-sg`), `headingLevel` (default 3), and `hideWhenEmpty="unmount"`, which renders no section and no heading while the group is empty. The rows stay mounted, so a later match brings the group back. `hideWhenEmpty={true}` is unchanged (`display: none`).
- `SettingRow`: `className`, added after the row's own classes.
- `SettingsSearchSection`: `passthroughWhenInactive` (opt-in). With no search active it renders its children without its own section, heading or action. `className` already existed.
- `SettingsSectionScope`: optional `scopeKey`, so a section's count can be read with `useSettingsSection()`.

Changed:
- Internal: the search context, provider, hooks and keyboard logic moved from `search.tsx` to `search-core.tsx`, which has no markup or stylesheet. `search.tsx` re-exports it, so every existing import keeps working.
- `useSearchEntry` also reports `matches` (the entry's own text matches the query), which the headless row hook uses.

## 0.4.1

- Fix `useSettings`: a `load()` that finished while a write was pending (the mount-time load, or a reload) overwrote the optimistic value with the state from before the write. Such a load is now not applied; it is repeated once the writes have drained, so values stored elsewhere still arrive, including after a failed write.

## 0.4.0

Search that rows, groups and items take part in themselves, an all-sections results mode, a backend-free controlled panel, a lean `/core` entry, and keyboard options. Requested by Pulse (gap ledger #104). Every addition is optional: the default DOM, classes, attributes and search results are identical to 0.3.4, pinned by `src/default-output.test.tsx` (snapshots written before the change) next to the existing `default-markup` and `output` snapshots.

Added:
- `@rightkit/shell` is no longer a peer or a dev dependency (audit D01): no source, type or emitted file referenced it; the `shell-adapter` subpath still needs only a structurally compatible `engine`.
- Search context: `SettingsSearchProvider` (`query`, `filter`, `highlight`), `useSettingsSearch()` (`query`, `terms`, `active`, `results`, `count`, `register`, `highlight`), `SettingsItem` (arbitrary content searchable with supplied `text`, `description`, `keywords`), `SettingsSectionScope`, `SettingsSearchSection` (heading, "Open section" action, hides when empty), `HighlightedText`, `highlightText`, `parseSearchTerms`. `SettingRow`, `SettingsGroup` and `SettingsItem` register on mount and unregister on unmount, so conditional rows that are not rendered never appear and late rows appear without a `MutationObserver`.
- `SettingRow` `keywords` and `searchText`; `SettingsGroup` `hideWhenEmpty`, re-evaluated when registrations change.
- `SettingsPanel`: `searchMode` (`"navigate"` default, `"all-sections"`), `highlight`, `openSectionLabel`, `resultHeadingLevel`, `announceResults`, `formatAnnouncement`, `query`/`onQueryChange`, `activeSection`/`onActiveSectionChange`, `hideSearch`, `searchLabel`, `findShortcut`, `escapeClears` (`"always" | "when-search-focused" | "unless-in-input-or-dialog"`, IME composition respected). `useSearchKeys` exposes the keyboard options to hosts that own their field.
- `backend` is optional on `SettingsPanel`: without it the panel keeps an in-memory store and never shows "Loading settings…".
- Entries `@rightkit/settings-ui/core` (panel, primitives, search, backends, types; imports only `react`) and `@rightkit/settings-ui/standard` (standard sections and hooks). The root entry re-exports both, so every 0.3.x import keeps working.
- Classes in `settings.css` for the new markup only: `rk-settings__searchbar`, `__results`, `__result`, `__result-head`, `__result-title`, `__mark`, `__sr`.

Changed:
- `@rightkit/license`, `@rightkit/logs`, `@rightkit/theme` and `@rightkit/updates` are optional peers (`peerDependenciesMeta`). They are still needed by `/standard` and the root entry. The repository lists them as `workspace:*` devDependencies for the build and tests.
- `hideNav` no longer disables search by itself: with `hideNav`, `search={true}` renders the field above the content and a controlled `query` works with no field. To keep every existing app unchanged, `hideNav` with `search` unset still renders no field.
- `SectionFrame` wraps the section body in a context-only `SettingsSectionScope` (no DOM) so rows know their section.

Unchanged by design: in `"navigate"` mode no row is ever hidden and no live region is rendered unless `announceResults` is set; `Escape` and the find shortcut do nothing unless enabled.

## 0.3.4

Added:
- `hideSectionHeading` on `SettingsPanel` and on `SettingsSection`. When set, the section renders no `<h1 class="rk-settings__h1">`, and its article is labelled by the section `label`. The section value overrides the panel value. `SectionFrame` takes the same flag.
- `initial?: SettingsSnapshot` on `SettingsPanel` and `useSettings(backend, { initial })`. When the caller already holds the state, for example `createMemorySettings(...).snapshot()`, the first render is loaded and "Loading settings…" never appears. The background `load()` still runs.
- `--rk-settings-padding` (body padding, default `24px 32px 40px`; the narrow-screen default `18px 16px 32px` applies when unset) and `--rk-settings-font-size` (default `13.5px`). Both are read from `.rk-settings` and inherit from `:root`.

Changed:
- `SettingsSection` has a new optional `hideSectionHeading` field. Existing sections are unaffected.

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
