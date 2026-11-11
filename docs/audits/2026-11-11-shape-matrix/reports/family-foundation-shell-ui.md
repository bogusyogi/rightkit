# Family review: Foundation, shell and UI

Read-only. Nothing was built or run. "Verified" means read in source; "suspicion" means inferred; "not verified" means not read.

Trees: `S` = `<scratch>`, apps under `S/survey/<app>`, RightKit at `/home/user/rightkit` (`RK/`).

HeardRight verdicts are taken as given from `S/hr-review-settings-onboarding-shell.md` (cited as HR-S) and `S/hr-review-crash-logs-license.md` (HR-L). They were not redone.

## Limits of this review (read first)

- The run was interrupted and resumed with an instruction to finish economically. Depth is uneven and is stated per package. Deep: `rightkit-app-settings`, `@rightkit/settings-ui`. Medium (import surface plus header and call-site reading, not line by line): `rightkit-tauri-shell`, `@rightkit/react-shell`, `@rightkit/onboarding`, `rightkit-license`. Light (README, exports, grep of app usage): everything else.
- "One of the others" was GenRight and MailRight, read only through their import and call surface.
- ViewRight live (0.1.123) was read for settings only. For the other packages the live-versus-survey comparison for ViewRight was not done.
- Planned-line figures ("feeds") are the sums of `loc` per app and target in `S/coverage-rows.json`.

## What the survey trees show (applies to every section)

The 1 November survey trees are not pre-RightKit code. ViewRight, Pulse, CodeRight, GenRight, MailRight and ScrapeRight already depend on the same pinned set (`rightkit-tauri-shell =0.6.0`, `rightkit-app-settings =0.3.0`, `rightkit-license =0.2.5`, `@rightkit/react-shell 0.7.2`, `@rightkit/settings-ui 0.7.0`, `@rightkit/onboarding 0.6.3`, `@rightkit/theme 0.3.1`, and so on; verified in each tree's `Cargo.toml` and `package.json`). ViewRight live pins only `rightkit-license =0.1.3`, `rightkit-logs =0.1.0`, `rightkit-tauri =0.1.1` and five npm packages.

So the under-10% line match does not mean the apps have a rival implementation of the package. It mostly means the app files mapped to these packages are (a) the app's configuration of the package and (b) product screens and schemas that should never move. The plan's line counts for this family overstate what can move. The real cross-app question is narrower: where does an app still carry something generic beside the package.

---

## 1. `rightkit-app-settings` (cargo)

Depth: deep.

1. Exists: `RK/crates/rightkit-app-settings` 0.3.5 (apps pin 0.3.0; HeardRight main 0.3.1).
2. Feeds: coderight 10,622; pulse 7,270; heardright 1,470; mailright 1,032; genright 222; scraperight 163; postright 66. ViewRight has no rows but uses it (`viewright/src-tauri/src/commands/settings.rs:4`).
3. Basis: two unrelated things in one crate.
   - The settings store (`store.rs`, `backend.rs`, `options.rs`, `document.rs`, `manifest.rs`, `request.rs`, `sql.rs`; 4,993 lines): a fresh implementation, shaped by HeardRight's flat file (HR-S 1) and Pulse's manifest and change-request needs (CHANGELOG 0.3.3, issue #423).
   - `src/agent_config/` (3,775 lines, feature `agent_config`): CodeRight's `engine/crates/config` moved in (CHANGELOG 0.3.1: "moved from CodeRight `coderight-config`"). This is the whole 47% line match: `okf.rs` 358/361 lines, `types.rs` 457/509, `merge.rs` 204/230, `accounts.rs` 199/203, `instructions.rs` 301/398 found in the package. Verified.
4. Side by side (verified unless marked):
   - **ViewRight live**: no settings file. `localStorage` is primary, mirrored to a SQLite `settings_kv` table (`viewright-live/src-tauri/src/commands/settings.rs:1-38`, `persistence.rs:82-87,405-431`). Schema, defaults and migration live in TypeScript (`src/settings/settings.tsx`).
   - **ViewRight survey**: `Document` (opaque JSON object, envelope format) at `<data>/<ns>/settings.json`, with `put`/`patch` and `expected_revision` compare-and-swap (`commands/settings.rs:89-97,150-190`). TypeScript still owns the shape; a second `Document` holds the explorer record (`:38-50`). The renderer seeds an empty store from the localStorage boot copy (`src/settings/settingsBackend.ts:71-93`). The live SQLite mirror is not read by the survey build (grep of `settings_kv` in the survey tree finds nothing), so an upgraded user whose WebView profile was wiped loses the mirror's protection once. Suspicion on impact; the code path is verified.
   - **Pulse**: `Store` with `FileFormat::Flat`, `OnCorrupt`, `OnRefused`, `WriteSecurity` in the Windows notch (`pulse/windows/src/settings.rs:55-58`), plus read-only stores and `read_with_options` in core (`pulse/core/src/store.rs:331-378`) and a custom `SettingsBackend` for `policy.json` (`pulse/core/src/chat/store.rs:597-660`). 3,744 lines of `windows/src/settings.rs` are the field-by-field schema and clamps: product. Pulse keeps its own manifest (`settings/manifest.toml`, 2,671 lines) and generator (`scripts/gen-settings.mjs`, 395 lines) writing Swift, Rust, TypeScript and docs. The package's `Manifest::parse`/`generate` does the same job from a JSON document (CHANGELOG 0.3.3). Pulse's TOML carries per-platform defaults (`mac`, `win`), `win_switch`, `mac_store`, ranges and `published_by` (`manifest.toml:13-27`), which the package manifest (`key, kind, default, protected, label, section, note`) cannot express. Two manifest dialects for one job.
   - **CodeRight**: does not use `Store` for its configuration. Config is layered TOML (defaults, user, workspace, env, session) in `engine/crates/config`, which calls only `rightkit_app_settings::atomic_write` (`loader.rs:180,264`, `accounts.rs:210`). `Document` is used for the onboarding and legal records (`apps/coderight-tauri/src-tauri/src/onboarding.rs:16`, `legal_host.rs:13`). The survey tree still has the whole config crate, so the code exists twice (app and `agent_config`).
   - **GenRight**: typed `Store<AppSettings>` plus `SqlSettings` (`genright-local-image/src/settings.rs:24-59`, `genright-gen/src/store.rs:627`). **MailRight**: `Document` plus `SqlSettings`. **ScrapeRight**: drives `FileBackend` directly for revision and patch (`scraperight/src-tauri/src/commands.rs:57-110`), re-implementing what `Document::patch` does. Verified by grep; bodies not read.
5. Best shape: the store API (typed `Store<T>`, opaque `Document`, `Flat`/`Envelope`, `SqlSettings`, manifest, change requests) already serves every app read. It is not the best shape as a crate because `agent_config` does not belong in it. `agent_config` is a layered TOML agent-host configuration with `HostConfig` (models, routing, orchestration, budgets, TUI), instruction-file loader, OKF bundles, AI accounts, workspace trust and `licence_env`; it pulls `rightkit-daemon`, `rightkit-agent-policy`, `toml`, `dirs`. No other app's settings resemble it. It should be its own crate (for example `rightkit-agent-config`), in the agent family.
6. Per app: heardright fits (HR-S 1.6; needs 0.3.3+ for S11). viewright fits. pulse gaps (manifest dialect). coderight wrong as mapped: its 10.6k lines are agent configuration, not app settings; for the small part that is app settings it fits. genright, mailright fit. scraperight fits (should drop its hand-driven `FileBackend`). postright fits.
7. Boundary:
   - `agent_config` is one app's product schema inside a mandatory foundation crate.
   - `agent_config/licence_env.rs` and `accounts.rs` belong with licence and AI-account crates, not settings. Suspicion (file names and module docs only).
   - CodeRight inventory rows `engine/bins/coderight/src/api_settings/*` (handlers, openapi, provider keys; about 3.5k lines) are HTTP API handlers, not settings storage.
   - Pulse `windows/src/i18n.rs` is mapped elsewhere; `windows/src/settings.rs` is schema, stays app-side.
8. Actions:
   - Move `agent_config` out to its own crate before publishing; keep a re-export for one version if CodeRight already depends on the path.
   - Extend the manifest to cover Pulse's per-platform defaults, ranges and platform store key, or state that Pulse keeps its generator and delete the package generator's claim to be the one tool. Pulse is the only user of either; pick one.
   - Publish 0.3.3+ (`Store::snapshot`) for HeardRight S11.
   - Document `Document` versus `Store<T>` choice; three apps picked `Document` because the renderer owns the schema.

## 2. `@rightkit/settings-ui` (npm)

Depth: deep on ViewRight, medium on Pulse and CodeRight.

1. Exists: `RK/packages/settings-ui` 0.7.1.
2. Feeds: heardright 6,175; pulse 4,666; genright 3,617; mailright 3,465; viewright 2,684; scraperight 2,072; postright 522. CodeRight has no rows but uses it.
3. Basis: fresh implementation (frame, row primitives, standard sections, backend port).
4. Side by side (verified from imports and the cited files):
   - **HeardRight** (HR-S 2): `SettingsPanel` with nav, search and heading hidden; one of nine standard sections used; own tabs and search registry.
   - **ViewRight**: does not use `SettingsPanel` at all. Imports `SettingsGroup`, `SettingRow`, `UpdatesControls`, `aboutSection`, `legalSection`, the licence section body, and the `SettingsBackend` type. Its own panel (`src/settings/SettingsPanel.tsx`, 227 lines) hosts package section bodies inside a `.vr-rk` token-map wrapper (`src/settings/publishedSections.tsx:44-51`) and fakes a `SettingsContext` that maps one key, `updates.autoInstall`, onto its own `autoCheckUpdates` (`:22-41`). Theme, typography, editor, privacy and Mac file associations are product sections.
   - **Pulse**: primitives and search only (`SettingRow`, `SettingsGroup`, `SettingsSearchProvider`, `useSettingsSearch`, `UpdatesControls`). `hub/src/views/Settings.tsx` (2,056 lines) is the product. No `SettingsPanel`, no standard sections.
   - **CodeRight**: `SettingsPanel`, `appearanceSection`, `licenseSection`, `createShellBackends`. **GenRight**: `SettingsPanel`, `standardSections`, `mergeSections`. **MailRight**: `SettingsPanel`, `standardSections`, `licenseSection`, `legalSection`, `updatesSection`, `createShellBackends`. These three use the package as designed.
   - Pattern: the three priority apps use primitives and individual section bodies and reject the panel frame; the lower-priority apps use the full panel. The package serves both, but the priority apps each wrote an adapter to do it.
5. Best shape: three layers, each usable alone: (a) primitives and search, (b) section bodies that take their own typed props (updates, licence, legal, about) with no dependency on a `SettingsContext` key namespace, (c) the optional `SettingsPanel` frame. Today (a) and (c) are right; (b) is only half there: `UpdatesControls` is prop-driven, but the section factories read fixed keys (`updates.autoInstall`, `appearance.theme`), which is why ViewRight fakes a context and HeardRight translates theme names (HR-S U4).
6. Per app: heardright gaps (HR-S 2.6: U2, U3). viewright gaps (key-namespace adapter). pulse fits (primitives). coderight, genright, mailright fit. scraperight fits (uses `doctorSection`, `useLicenseSection`). postright not verified.
7. Boundary: none found in the package. Mapping problem: most of the 23k planned lines are product sections (Pulse `Devices.tsx`, `NearbySettings.tsx`, `PhonePairing.tsx`; ViewRight `MacFileAssociationsBlock.tsx`; HeardRight speech sections) and will not move.
8. Actions:
   - Let section factories take a key map or direct value/setter props so an app with its own key names needs no fake context.
   - Revision type: accept a string or opaque token (HR-S U3); `types.ts:8` is `number`, `types.ts:34` already has a string revision elsewhere.
   - Theme vocabulary option on the appearance section (HR-S U4).

## 3. `@rightkit/onboarding` (npm)

Depth: medium.

1. Exists: `RK/packages/onboarding` 0.6.4 (apps pin 0.6.3).
2. Feeds: heardright 4,328; pulse 920; coderight 789; genright 662; scraperight 384. ViewRight and MailRight use it without rows.
3. Basis: fresh frame (legal gate, ordered steps, resume, built-in permission, model, licence and sign-in steps).
4. Side by side:
   - **HeardRight** (HR-S 3): eight app steps, loses dynamic labels (O2), needs the card frame (O3).
   - **Pulse**: four app steps (`hub/src/onboarding/OnboardingApp.tsx:419`), no built-in steps. It already needs a state-dependent label and works around it by rebuilding the step object each render: `continueLabel: busy ? "Saving…" : failedKeys.length > 0 ? "Try again" : undefined` (`:334`). So the static-string contract (`RK/packages/onboarding/src/types.ts:226-228`) can be driven dynamically if the app re-creates steps. That makes HR-S O2 partly an app-side omission in HeardRight main, not only a package gap. Verified for Pulse; whether the frame re-reads a replaced step's label on every render was not verified in the package.
   - **CodeRight**: three app steps plus the package `licenseStep` wrapped as `proStep()` (`apps/coderight-tauri/src/OnboardingApp.tsx:166-232`), own titlebar slot (`:283`). Completion is a Rust-owned `onboarding.json` (`src-tauri/src/onboarding.rs:1-30`) because two windows race; the package persists `onboarding.state` through the settings backend instead. CodeRight supplies a settings service for the wizard, so both records exist. Suspicion that they can disagree; not traced.
   - **ViewRight**: `Onboarding` used as the legal gate only (`src/app/AppRoot.tsx:182`; imports `LegalReceiptPort`, `LegalStepConfig`). No steps.
   - **GenRight**, **ScrapeRight**: use `modelsStep`. Nobody read uses `permissionsStep` or `signInStep`.
5. Best shape: the frame is right and one API serves all. Needed: `continueLabel` and `blockedHint` as a string or a function of step context (documented, so apps stop depending on re-render behaviour); `frame="card"` published; a documented "completion flag owned natively" port so multi-window apps do not keep a second record.
6. Per app: heardright gaps (HR-S 3.6). viewright fits. pulse fits. coderight gaps (native completion record). genright, scraperight fit. mailright fits (legal gate only).
7. Boundary:
   - HeardRight native onboarding Rust mapped here has no home (HR-S O17).
   - Pulse `hub/src-tauri/src/onboarding.rs` and CodeRight `src-tauri/src/onboarding.rs` are mapped to `rightkit-tauri-shell`; both are onboarding-state hosts. If a native completion record is wanted it belongs in the shell's `host` services next to `host::legal`.
   - `permissionsStep` and `signInStep` have no user in the trees read. Suspicion: unused surface.
8. Actions: function-valued labels and hints; publish 0.6.4; native completion port; decide whether unused built-in steps stay.

## 4. `rightkit-tauri-shell` (cargo)

Depth: medium (call surface per app; package module list; no module read line by line).

1. Exists: `RK/crates/rightkit-tauri-shell` 0.7.3, about 25.5k lines in `src/*.rs` plus `host/`, `update/`, `clipboard/`. Apps pin 0.6.0, HeardRight main 0.7.0.
2. Feeds: heardright 5,658; coderight 4,608; viewright 2,641; pulse 2,033; mailright 902; scraperight 901; genright 280; cutright 276.
3. Basis: fresh implementation that has absorbed each app's host needs as modules: `preview`, `site`, `stream`, `broker`, `pdf_host`, `associations`, `print` (ViewRight); `overlay_host`, `idle`, `tray` (HeardRight, Pulse); `engine` (GenRight); `notify`, `presence`, `window_state` (CodeRight, MailRight); `host::{license, settings, legal, account, models, app_update, secrets, app_lock}` (all).
4. Side by side (verified by call-site grep):
   - **ViewRight** uses `preview`, `site`, `stream::serve`, `broker`, `webview`, `opener`, `nav`, `frameless::button_frames`, `devtools`, `clipboard`, `pdf_host`, `recovery`, `open_url`, `windows`, `update`, `host`. The 18 mapped app files (`html_assets.rs`, `native_menu.rs`, `dialogs.rs`, `site_root.rs`, `preview_protocol.rs`, `path_broker.rs`, ...) are now configuration of those modules plus the viewer's menus. Still on `tauri-plugin-dialog`, `-process`, `-updater`.
   - **Pulse** hub uses `windows::builder`, `windows::reveal`, `lifecycle`, `idle`, `handler`, `SingleInstanceGuard`, `host::legal`. The Windows notch (`pulse/windows`) is a native Win32 app, not Tauri, and uses none of it: it hand-rolls autostart against the registry (`windows/src/autostart.rs:1-25`, 342 lines) and its own updater (`windows/src/update.rs`, 1,298 lines: GitHub release feed over WinHTTP, SHA-256, Authenticode with pinned signer, silent NSIS).
   - **CodeRight** uses `host::{settings, license, legal}`, `update::rightkit_update_*`, `logs_commands`, `notify`, `presence`, `window_state`, `deep_link::register_scheme`, `windows`. `lib.rs` (2,421 lines) is command registration and product wiring; `ipc_policy.rs` (654 lines) is its own per-window command allowlist beside the shell's `caller_gate`/`host::policy`. Still on `tauri-plugin-http`, `-dialog`, `-process`, `-updater`.
   - **MailRight** uses `instance`, `open_url`, `notify`, `idle`, `webview`, `nav`. **GenRight** uses `Shell::builder`, `engine`, `handler`.
   - Only GenRight (and HeardRight, lifecycle-only, HR-S H12) use the `Shell` builder. The others call modules à la carte. Both work.
5. Best shape: a thin Tauri binding crate over runtime-neutral crates, with app-specific host kinds as separate crates. The split has started (`rightkit-instance`, `rightkit-autostart`, `rightkit-deep-link`, rightkit#956) but the crate still holds document-viewer hosting (`preview`, `site`, `stream`, `pdf_host`, `print`, `associations`), overlay hosting and engine supervision in one mandatory package. Features keep the compile cost optional (Cargo features verified to exist; which module each gates was not checked), but the public surface every app must track is one 25k-line crate. Right design, wrong granularity for "mandatory".
6. Per app: heardright gaps (HR-S 4.6: tray left click H5, close-behaviour setting H9). viewright fits. pulse hub fits; pulse notch n/a for this crate (see `rightkit-autostart`, `rightkit-app-update`). coderight gaps (`ipc_policy.rs` versus `caller_gate`: two allowlists; not compared line by line). mailright, genright fit. scraperight, cutright not verified.
7. Boundary:
   - Viewer hosting (`preview`, `site`, `stream`, `broker`, `pdf_host`, `print`) is used by ViewRight only in the trees read. It passes the "second app could adopt" test in principle (MailRight uses `webview` and PDF UI) but should be a feature group or sibling crate, not core. Suspicion on second-app use.
   - `engine.rs` (1,224 lines) overlaps `rightkit-engine-host` and `rightkit-sidecar` by name; not compared. Suspicion.
   - Mapped here but not shell code: HeardRight helper files (HR-S H15); Pulse `hub/src-tauri/src/{license,legal,onboarding}.rs` (host configuration); CodeRight `engine/bins/coderight/src/api_notifications.rs` (HTTP API); ViewRight `error.rs`.
   - Apps still carrying Tauri plugins the shell is meant to replace: updater and process (ViewRight, CodeRight, GenRight, MailRight), dialog (ViewRight, CodeRight), http (CodeRight), os (Pulse). The shell wraps the updater plugin itself (`Cargo.toml:71`), so that one is expected; the rest are accidental. Not verified whether the shell has a dialog replacement.
8. Actions: custom tray left-click action (HR-S H5); state which modules are core and which are optional host kinds, and gate or split the viewer and overlay hosts; settle one IPC allowlist model with CodeRight's `ipc_policy.rs` as the test case; publish 0.7.x.

## 5. `@rightkit/tauri-shell` (npm)

Depth: light.

1. Exists: `RK/packages/tauri-shell` 0.1.4 (977 lines).
2. Feeds: none in the inventory. Used by ViewRight, Pulse, CodeRight, GenRight, ScrapeRight (`createShell`, `tauriTransport`, `EVENTS`, `PREVIEW_COMMANDS`).
3. Basis: fresh; the typed renderer bridge for the crate.
4. All apps call `createShell`; no divergence seen. MailRight does not import it and uses `@rightkit/tauri` instead.
5. Shape: right. 6. viewright, pulse, coderight, genright, scraperight fit; mailright n/a today; heardright not covered by HR-S (not verified).
7. Boundary: overlaps `@rightkit/tauri` (see next).
8. Actions: none beyond the merge decision below.

## 6. `@rightkit/tauri` (npm)

Depth: light.

1. Exists: `RK/packages/tauri` 0.3.1 (700 lines).
2. Feeds: mailright 559; scraperight 248; genright 192; postright 133.
3. Basis: fresh; typed adapters for device, logs and diagnostics commands, plus `createNativeFetch`.
4. Pulse uses `createRightKitTauriClient`; MailRight uses `createMailRightTauriClient`. ViewRight live pinned 0.1.1; ViewRight survey no longer imports it.
5. Shape: wrong boundary. The package exports one constructor per product (`createViewRightTauriClient`, `createHeardRightTauriClient`, `createCodeRightTauriClient`, `createMailRightTauriClient`, `createScrapeRightTauriClient`; `src/index.ts:213-217`) and a table `rightSuiteTauriAppConfigs`. A shared library that names its consumers has to be republished for each new app. The generic `createRightKitTauriClient` plus a command map is the right API and already exists.
6. pulse fits (generic constructor). mailright gaps (uses the per-app constructor). viewright, coderight n/a today. genright, scraperight, postright not verified.
7. Boundary: two renderer bridge packages (`tauri`, `tauri-shell`) for one native shell. Device, logs and diagnostics commands are registered by the shell crate (`logs_commands`), so their renderer side belongs in `@rightkit/tauri-shell` or `@rightkit/logs`.
8. Actions: remove per-app constructors; merge into `@rightkit/tauri-shell` or keep only `defineTauriCommands`/`createNativeFetch` here and say so.

## 7. `@rightkit/react-shell` (npm)

Depth: medium (import surface, HR-S 5).

1. Exists: `RK/packages/react-shell` 0.7.3 (about 7.4k lines with CSS).
2. Feeds: scraperight 4,155; coderight 4,054; mailright 2,464; genright 788; postright 350. ViewRight (35 files import it) and Pulse (26) have no rows.
3. Basis: fresh component set.
4. Side by side (verified by imports):
   - Frame: HeardRight, Pulse, GenRight, MailRight, ScrapeRight use `AppShell`. ViewRight uses `AppFrame` + `Titlebar` + `Plane` (a document viewer with no nav rail). CodeRight uses `Titlebar` + `CaptionControls` under its own `App.tsx` (1,219 lines), own `Sidebar.tsx` (410) and a wrapper around `CommandPalette` (367).
   - Theme: three mechanisms are in use for one job. `createThemeStore` (CodeRight, GenRight, MailRight, ScrapeRight), `useTheme` (Pulse, CodeRight), and app-owned `applyBrand`/`applyTokens` from `@rightkit/theme` (HeardRight, Pulse). ViewRight keeps its own `themes.ts`.
   - Controls (`Button`, `Toggle`, `Select`, `ModalShell`, `ContextMenu`, `ConfirmDialog`, ...) are used uniformly.
5. Shape: right design with gaps. One API serves all three frame styles through `AppShell`, `AppFrame` and bare `Titlebar`. CodeRight's own sidebar and task switcher are a session-tree UI, a real product need. Its `PaneErrorBoundary`, `PaneLoading`, `useConfirmationDialog`, `Icon.tsx` and `usePaletteRecents` duplicate package parts (`ErrorBoundary`, `Skeleton`, `ConfirmDialog`) in name; bodies not compared. Suspicion: accidental.
6. heardright gaps (HR-S 5.4: maximise option R3, `--ink` clash R5). viewright fits. pulse fits. coderight gaps (palette recents, pane boundary wrappers). genright, mailright, scraperight fit.
7. Boundary: theme state lives in both `react-shell` (`createThemeStore`, `useTheme`, `applyStoredThemeSync`, `applyBodyFont`) and `@rightkit/theme` (`applyBrand`, `applyTokens`, `themeBootScript`). Toast is correctly a re-export of `@rightkit/platform-ui` (`src/toast.ts:1`), not a copy.
8. Actions: `maximizable` on `CaptionControls` (R3); document one theme path (store in react-shell, token maths in theme) and drop the other entry points; palette recents hook if CodeRight's is generic.

## 8. `@rightkit/platform-ui` (npm)

Depth: light.

1. Exists: 0.4.1 (1,895 lines). 2. Feeds: viewright 160. Used by every app read (ViewRight 52 files).
3. Basis: fresh; headless helpers (platform detection, chord formatting, list navigation, dismiss, focus trap, toast bus, window-control labels).
4. Uniform use. ViewRight's `src/platform.ts` (42 lines, 28 of 29 distinctive lines found in RightKit) is a leftover copy.
5. Shape: best. 6. All apps fit. 7. None. 8. None; ViewRight drops `src/platform.ts`.

## 9. `@rightkit/theme` (npm) and `rightkit-theme` (cargo)

Depth: light.

1. Exist: `RK/packages/theme` 0.3.2; `RK/crates/rightkit-theme` 0.1.0 (670 lines, unpublished; the TypeScript resolver redone in Rust, emitting W3C design-token JSON).
2. Feeds (`@rightkit/theme`): coderight 4,133; pulse 445; viewright 290; heardright 271; genright 199; scraperight 166; mailright 74; postright 10. `rightkit-theme`: none.
3. Basis: fresh.
4. Side by side: HeardRight keeps its palette and calls `applyBrand`/`applyTokens` (HR-S 6). Pulse the same plus `brandFromAccent`, `fontTokens`. CodeRight and MailRight use the colour maths (`mix`, `ensureContrast`, `luminance`, `readableOn`, `presetTokens`). ViewRight uses only `TokenPreset` and `mix` and keeps `themes.ts` (221 lines) as product presets. ScrapeRight uses `themeBootScript`.
5. Shape: the npm package is right for web. The mapping is wrong: CodeRight's 4.1k lines are mostly check scripts (`check-a11y-*.mjs`, `check-contrast.mjs`, `check-motion.mjs`, `check-target-size.mjs`), unrelated tools (`tools/context/*.py`) and iOS Swift theme files; Pulse's are an icon build script; ViewRight's is `check-brand-contract.mjs`. Of these only the contrast check overlaps the package (it ships contrast checks and a CLI). The Swift theme files (HeardRight T4, CodeRight `apps/coderight-ios/CodeRight/Theme/*`) need the Rust crate's token JSON plus a Swift consumer; neither app uses that yet.
6. heardright fits on web, wrong target for iOS (HR-S 6.5). viewright, pulse, genright, mailright, scraperight fit. coderight fits on web; iOS and scripts are mis-mapped.
7. Boundary: a11y, motion and target-size check scripts belong to a QA package, not theme. `rightkit-theme` and `@rightkit/theme` are two resolvers for one algorithm; parity is asserted in the README, not verified here.
8. Actions: publish `rightkit-theme` with a parity test against the npm resolver; add a Swift token output (or generated Swift) so the iOS files have a target; re-map the scripts.

## 10. `@rightkit/legal`, `@rightkit/legal-ui` (npm)

Depth: light.

1. Exist: `legal` 0.6.3 (1,104 lines), `legal-ui` 0.4.2 (1,152 lines).
2. Feeds: `legal`: mailright 564; cutright 395; scraperight 313; viewright 312; coderight 111. `legal-ui`: scraperight 237; mailright 201.
3. Basis: fresh; manifest, acceptance receipt, re-acceptance; `LegalGate` screen.
4. ViewRight live already used `legal` 0.3.1 and `legal-ui` 0.1.1; the survey tree uses the gate through `@rightkit/onboarding` and `LegalGateTheme`. CodeRight uses `LegalGate` directly plus `materialAcceptanceDigestAsync`; Pulse uses `policyGateDocuments`; GenRight builds receipts with `createAcceptanceReceipt`. Receipt writing: CodeRight, MailRight and Pulse use `createNativeLegalReceiptPort` (native timestamp); HeardRight and ViewRight use the older renderer-stamped `write` port (HR-S O11; ViewRight imports `LegalReceiptPort` only). The two shipped apps are on the deprecated path.
5. Shape: best for both packages. 6. All fit; heardright and viewright should move to the native receipt port.
7. Boundary: the legal gate is reachable three ways (`legal-ui` `LegalGate`, `onboarding`'s legal step, HeardRight's standalone page; HR-S O12). One screen, so `onboarding` should compose `legal-ui`; not verified that it does. Mapped scripts (`check-legal-contracts.mjs`) are release checks.
8. Actions: remove the deprecated `write` port once HeardRight and ViewRight move.

## 11. `rightkit-license` (cargo) and `@rightkit/license` (npm)

Depth: medium (HR-L 3 plus host files of ViewRight, CodeRight, Pulse).

1. Exist: crate 0.2.9 (14.3k lines), npm 0.3.1.
2. Feeds (crate): coderight 3,623; heardright 1,840; scraperight 1,468; mailright 858; viewright 320; genright 301; cutright 126. npm: scraperight 192.
3. Basis: built from HeardRight's licence code (HR-L verdict).
4. Side by side (verified from the cited headers):
   - **ViewRight**: `license_host.rs` (320 lines) is configuration only: device id source, keychain names kept from live (`cc.viewright.updates`/`activation`), public keys, update authority (`viewright/src-tauri/src/license_host.rs:1-40`). Licence gates update eligibility (`UpdateAuthProvider`), not features.
   - **CodeRight**: `license_host.rs` (332) is configuration plus a daemon handoff of non-secret locators and a Windows link-code command (`apps/coderight-tauri/src-tauri/src/license_host.rs:8-22`). `pro_bridge.rs` (914 lines) is "Ported from HeardRight `src-tauri/src/pro_bridge.rs`" (`pro_bridge.rs:10`): the same iCloud Keychain helper driver exists in two apps. `engine/bins/coderight/src/license.rs` (622) verifies in the daemon.
   - **Pulse**: `hub/src-tauri/src/license.rs` (148): configuration read from the OS credential store at start, `LicenseSetup::Disabled` when absent; no feature is gated.
   - **HeardRight** (HR-L 3.4): about 600 lines still app-side for trial with platform, removal queue, account and link commands.
   - One `LicenseClient` plus the shell's five commands serves all four. Differences are configuration (lease versus key activation, what the licence gates), which the client already parameterises.
5. Shape: right design with gaps. 
6. heardright gaps (HR-L 3.6). viewright fits. pulse fits. coderight gaps (link-code command and Pro bridge helper still app code). mailright gaps (still calls `verify_token` directly six times beside `LicenseClient`; not read). genright fits. scraperight, cutright not verified.
7. Boundary:
   - The macOS Pro bridge helper driver is duplicated in HeardRight and CodeRight. `rightkit-license` has the wire calls (`client/bridge.rs`) and RightKitSwift has `RightKitAppleCredentials` (`AppleCredentialHelperHost.swift`), but the Rust code that spawns and talks to the helper is in neither. That is the missing piece.
   - CodeRight rows `engine/bins/coderight/src/onboarding.rs` (1,198), `auth.rs`, `startup.rs` mapped to licence are not licence code by name. Not read.
   - Link-code redemption is app code in both HeardRight and CodeRight while the shell source has link commands (HR-L K15).
8. Actions: publish 0.2.9 and the shell link and account commands (HR-L 3.6); add the helper-process driver for the Apple credential bridge to RightKit (crate side) so both apps drop `pro_bridge.rs`; resolve lease-expiry disagreement (HR-L K8).

## 12. `@rightkit/app-update` (npm) and `rightkit-app-update` (cargo)

Depth: light.

1. Exist: npm 0.4.3 (2,298 lines); crate 0.1.0 (658 lines: boot probation and native fetch bridge only). Check, download, verify and install are `rightkit-tauri-shell`'s `update` module (1,704 lines plus `update/`).
2. Feeds: npm: scraperight 218. Crate: none.
3. Basis: fresh.
4. ViewRight uses the shell `update` host plus `publicInstallerUrl`, with patch-versus-update eligibility tied to the update licence (`publishedSections.tsx:53-78`). CodeRight uses the five shell update commands plus `UpdateConsentCard`, `UpdateOverlay`, `createBootProbation`. GenRight uses `createAutoUpdateClient`. HeardRight keeps its own update flow (HR-S 4.4 "own code: update flow"). Pulse's Windows notch has its own complete updater (see section 4) and the Mac notch its own `Updater.swift` (named in `windows/src/update.rs:1-2`; not read).
5. Shape: gaps. For Tauri apps the three-part split (shell host, crate, npm) works. For a non-Tauri native app there is nothing: the verified-installer path (feed, digest, signature, pinned signer, silent install) exists only inside the Tauri shell or inside Pulse.
6. viewright, coderight, genright fit. heardright not verified (own flow). pulse wrong for the notch (cannot adopt without Tauri), fits for the hub.
7. Boundary: update logic is spread over three packages and the name `rightkit-app-update` holds the smallest part. Pulse's Authenticode and pinned-signer check overlaps what `@rightkit/release` gates at build time (commit `ca06bfb` mentions an Authenticode gate); runtime verification has no shared home.
8. Actions: move the runtime-neutral update core (feed, digest, signature and signer verification, state machine) out of the shell's `update` into `rightkit-app-update`, leaving the Tauri commands in the shell, as was done for instance and autostart. Pulse's `windows/src/update.rs` is the reference for the Windows verification rules.

## 13. `@rightkit/i18n` (npm) and `rightkit-i18n` (cargo)

Depth: light.

1. Exist: npm 0.2.1 (237 lines); crate 0.1.0 (189 lines, unpublished), reading the same JSON catalogs.
2. Feeds: npm: pulse 7,215; postright 150. Crate: none.
3. Basis: fresh.
4. Pulse hub uses `createI18n`. Pulse `windows/src/i18n.rs` (7,215 lines) is a compiled-in table keyed by the English string, copied from the Mac's `Localizable.xcstrings`, and says "RightKit ships no Rust runtime, so this table is the Windows side" (`:6-8`). That is now out of date: `rightkit-i18n` exists. CodeRight has its own `src/i18n/index.ts` and `catalogs/en.ts` (mapped to react-shell) and does not import `@rightkit/i18n` in source although it is a dependency. Other apps are English-only.
5. Shape: gaps. The runtimes are right. Pulse's three surfaces use three catalog formats (xcstrings on Mac, a Rust table on Windows, JSON in the hub); the package defines only the JSON one. The 7.2k lines are translations (data), not code, and are mapped to the npm package when their consumer would be the crate.
6. pulse gaps. coderight gaps (own i18n module; accidental). postright not verified. Others n/a.
7. Boundary: catalog data mapped as package code; no xcstrings-to-JSON conversion, so Mac and Windows copy stay hand-synchronised.
8. Actions: publish `rightkit-i18n`; add an xcstrings import (or make JSON the source and generate xcstrings); re-map Pulse's table to the crate.

## 14. `rightkit-instance`, `rightkit-autostart`, `rightkit-deep-link` (cargo)

Depth: light.

1. Exist: `rightkit-instance` 0.1.1 (1,930 lines), `rightkit-autostart` 0.1.0 (1,046), `rightkit-deep-link` 0.1.1 (169). All unpublished, split from the shell (rightkit#956) and re-exported by it.
2. Feeds: none in the inventory (their app code is under `rightkit-tauri-shell`).
3. Basis: the shell's code, moved. Autostart carries HeardRight's plugin-entry migration (HR-S H3); instance carries the legacy-plugin handoff (HR-S H1).
4. HeardRight: verified right shape for both (HR-S H1 to H4). Pulse hub uses `SingleInstanceGuard`. MailRight uses `instance`. CodeRight uses `deep_link::register_scheme`. Pulse's Windows notch hand-rolls autostart with one extra behaviour: it reads and honours Task Manager's Startup-approved state (`windows/src/autostart.rs:24-25`). HR-S H3 says the package migration carries the "disabled in Startup apps" state, so the capability is present; API parity with Pulse's file was not checked.
5. Shape: best as a boundary (runtime-neutral, usable by Pulse's non-Tauri notch). Internals not read.
6. heardright fits. pulse fits for instance; gaps-or-fits for autostart pending the parity check (recorded as gaps). mailright, coderight fit for what they use. viewright n/a (no autostart or single-instance call found; file associations and `open_url` are in the shell).
7. Boundary: none found.
8. Actions: publish all three; check `rightkit-autostart` against `pulse/windows/src/autostart.rs` (Startup-approved key, notification settings keys at `:18-20`) so the notch can drop its copy.

## 15. `rightkit-app-host` (cargo)

Depth: light (README only).

1. Exists: 0.1.1, 129 lines. 2. Feeds: none.
3. Basis: extracted for crates "moved out of Pulse" (chat bridge, AI accounts, agent gateway, approvals, web session): a process-global `install` giving state folder, account home, product name.
4. No app call sites were checked.
5. Shape: not judged. A process-global host registry is a seam created by moving Pulse code, not a capability an app adopts for its own sake. It does not belong in the "shell and UI" mandatory set unless every app uses the Pulse-derived companion crates.
6. All apps: not verified. 7. Suspicion: overlaps `agent_config::state_root`/`set_app_name` in `rightkit-app-settings` (both set a product name and state folder globally). 8. Decide whether it is mandatory; reconcile with `agent_config::state_root`.

## 16. `rightkit-engine-host` (cargo)

Depth: light (README only).

1. Exists: 0.1.2, 2,470 lines. 2. Feeds: none.
3. Basis: GenRight's `genright-engine` and `genright-protocol` framework pieces (README, 2026-11-01).
4. GenRight is tier 4 in the priority order. HeardRight, CodeRight and Pulse each have an engine or daemon process with their own protocol; none was compared. The shell's `engine.rs` and `rightkit-sidecar`, `rightkit-daemon`, `rightkit-job-queue` cover adjacent ground.
5. Shape: not judged; reference app is the lowest-priority tier, so it should not be mandatory until checked against HeardRight's engine supervisor and CodeRight's daemon.
6. genright fits (source). Others not verified. 7. Suspicion: overlap with shell `engine.rs`. 8. Compare with HeardRight and CodeRight engine hosting before publication as mandatory.

## 17. `RightKitUI` (Swift)

Depth: light.

1. Exists: `RK/swift/RightKitSwift/Sources/RightKitUI`, two files, 230 lines: `AdaptiveGlassSurface`, `RightKitContrast`.
2. Feeds: pulse 2,745; heardright 1,563; coderight 835; scraperight 422.
3. Basis: fresh, minimal. Line match 0%.
4. Mapped code: Pulse `mac/Notch/Sources/Settings/*` (`Preferences.swift` 1,747 lines, `GeneratedSettings.swift`, enums such as `NotchSize`, `AccentColor`) is the Mac notch's settings model and product enums. CodeRight's rows are the iOS app entry, File Provider extension and widgets. None of it is UI-kit code. HeardRight's rows were covered in `hr-review-ios-swift.md` (not read here).
5. Shape: wrong as a target. The module is fine for what it holds, but about 5.5k planned lines are mapped to it that are settings models (`RightKitStore.AppSettingsStore` exists for that), app entry points and extensions.
6. pulse wrong (settings model belongs with `RightKitStore` plus generated settings; stays on UserDefaults per `manifest.toml` `mac_store`). coderight wrong (File Provider, widgets are not UI kit). heardright: see `hr-review-ios-swift.md`, not verified here. scraperight not verified.
7. Boundary: mapping error throughout; `RightKitUI` overlaps `@rightkit/theme` contrast maths in a third language (TypeScript, Rust, Swift).
8. Actions: re-map; if Swift apps are to share tokens, generate them from `rightkit-theme` output instead of hand-written contrast code.

---

## Family summary

### The cross-app answer

- **Does one API serve all apps?** Yes for storage, licence, legal, platform helpers, instance, autostart and the React controls. The apps already call the same pinned versions; differences are configuration.
- **What apps do that the package cannot** (real needs): Pulse's non-Tauri notch needs update and autostart without Tauri; Pulse needs per-platform defaults in the settings manifest and one catalog across Swift, Rust and web; HeardRight and CodeRight need the Apple credential helper driver; CodeRight needs layered TOML agent configuration (a different capability from app settings) and a natively owned onboarding-complete flag; HeardRight needs a tray left-click action and non-maximisable caption controls; ViewRight and HeardRight need settings section bodies that do not assume the package's key names.
- **Accidental divergence the app should drop**: ViewRight `src/platform.ts`; ScrapeRight driving `FileBackend` by hand; CodeRight's own i18n module, pane error boundary and confirm-dialog hook; CodeRight and HeardRight each carrying `pro_bridge.rs`; MailRight's per-app `@rightkit/tauri` constructor; HeardRight and ViewRight on the deprecated legal receipt port; three ways to set the theme; leftover Tauri plugins (dialog, http, os, process).
- **Product, stays in the app**: every settings section body and schema, every onboarding step body, ViewRight's menus and theme presets, HeardRight's hub window, CodeRight's sidebar and task switcher, Pulse's notch settings model.

### `rightkit-app-settings` and the 47%

The match is CodeRight's `coderight-config` crate, moved into the package as feature `agent_config` (3,775 of 8,768 lines). It is not evidence that CodeRight's settings are the reference implementation. The store itself is an independent implementation that HeardRight, ViewRight, Pulse, GenRight and MailRight already use. `agent_config` should leave the crate.

### Counts by shape (24 packages)

- best: 7 (`@rightkit/platform-ui`, `@rightkit/legal`, `@rightkit/legal-ui`, `@rightkit/tauri-shell`, `rightkit-instance`, `rightkit-deep-link`, `rightkit-theme`)
- gaps: 12 (`rightkit-tauri-shell`, `@rightkit/react-shell`, `@rightkit/theme`, `@rightkit/settings-ui`, `@rightkit/onboarding`, `rightkit-license`, `@rightkit/license`, `@rightkit/app-update`, `rightkit-app-update`, `@rightkit/i18n`, `rightkit-i18n`, `rightkit-autostart`)
- wrong: 5 (`rightkit-app-settings` because of `agent_config`; `@rightkit/tauri` per-app constructors; `RightKitUI` as a mapping target; `rightkit-app-host` and `rightkit-engine-host` as members of the mandatory set, both low confidence)
- missing: 0

### Not verified

- Nothing was compiled or run.
- Package internals of the shell, licence client, onboarding frame and react-shell were not read line by line in this run.
- ViewRight live versus survey for anything except settings.
- `rightkit-app-host` and `rightkit-engine-host` call sites; `rightkit-autostart` parity with Pulse's notch; `rightkit-theme` parity with the npm resolver.
- ScrapeRight, CutRight, PostRight usage beyond import lists.
