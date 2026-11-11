# HeardRight vs RightKit: settings, onboarding, shell

Read-only comparison by reading source. Nothing was built or run, so every "verified" below means "verified by reading the code", never by running the app.

Path prefixes used throughout:

- `LIVE/` = `<scratch>/survey/heardright-live/tauri-app-next/` (0.1.213, commit 12b7d8ce)
- `MAIN/` = `<scratch>/survey/heardright-main/tauri-app-next/` (0.1.231, unreleased)
- `RK/` = `/home/user/rightkit/`

## Verdicts

| Package | Verdict | Main reason |
|---|---|---|
| `rightkit-app-settings` | Right shape for HeardRight | `FileFormat::Flat` keeps the existing `settings.json` byte-compatible; the app's schema, migration and patch allowlist stay app-side. One read-path gap. |
| `@rightkit/settings-ui` | Right shape with listed gaps | Used as a frame and row primitives only; every HeardRight section is still app code. Revision type and theme vocabulary need an app adapter. |
| `@rightkit/onboarding` | Right shape with listed gaps | The frame fits (ordered steps, gates, resume, legal gate) but per-step dynamic button labels and hints are lost, and the pinned version lacks the card frame HeardRight's design needs. |
| `rightkit-tauri-shell` | Right shape with listed gaps | Used lifecycle-only; single instance, autostart and tray carry explicit migrations from the Tauri plugins. Tray left-click and the close-behaviour setting need app workarounds. |
| `@rightkit/react-shell` | Right shape with listed gaps | `AppShell` replaced about 800 lines of app frame. Theme toggle moved out of the title bar; maximise button has to be hidden with CSS. |
| `@rightkit/theme` | Right shape (web) / wrong target (iOS) | Web use is a thin token writer and fits. The inventory maps three iOS Swift files to it, but the package has no Swift output. |

## Version skew (applies to every section)

The RightKit source tree is ahead of what HeardRight main pins. I read the tree, not the published artifacts.

| Package | Main pins | RK tree |
|---|---|---|
| `rightkit-app-settings` | =0.3.1 (`MAIN/Cargo.lock:4322`) | 0.3.5 |
| `rightkit-tauri-shell` | =0.7.0 | 0.7.3 (unpublished) |
| `@rightkit/settings-ui` | 0.7.0 | 0.7.1 |
| `@rightkit/onboarding` | 0.6.3 | 0.6.4 (unpublished) |
| `@rightkit/react-shell` | 0.7.2 | 0.7.3 |
| `@rightkit/theme` | 0.3.1 | 0.3.2 |

Where a finding depends on something the changelog places after the pinned version, it says so. Whether main compiles against the pinned versions is not verified.

---

## 1. `rightkit-app-settings` (settings storage)

### 1.1 What live does

- Schema, defaults, migration: `LIVE/heardright_core/src/settings/blob.rs`. `SettingsBlob` (`:148-298`) is a flat struct of optional fields with `#[serde(flatten)] extras` for unknown keys (`:296-297`). `CURRENT_SCHEMA_VERSION = 13` (`:302`), `migrate()` runs v0 to v13 transforms (`:312-412`).
- Patch allowlist (renderer input): `LIVE/heardright_core/src/settings/patch.rs` (`apply_patch_to`).
- Persistence: `LIVE/src-tauri/src/settings.rs`. One in-process `Mutex<SettingsBlob>` loaded once (`:22`, `:59-64`); `snapshot()` is a memory clone (`:468-475`).
- Load (`:24-57`): any read error is treated as first run (`:26-31`); a parse error renames the file to `settings.json.corrupt` and resets to defaults (`:41-55`).
- Save (`:66-145`): temp file, fsync, atomic replace (`ReplaceFileW` on Windows `:162-207`, `rename` elsewhere), directory fsync on Unix.
- Surface the app calls: typed accessors (`close_behavior`, `asr_backend`, `push_to_talk_key`, ...), `apply_patch`, `claim_screenshot_prompt`, `claim_cancel_hint`, `set_pill_position_for_monitor`.

Note: the inventory files `settings.rs` under `rightkit-tauri-shell`; functionally it is settings storage, so it is covered here.

### 1.2 What RightKit provides

`RK/crates/rightkit-app-settings/src/` exports `Store<T>`, `StoreOptions`, `FileBackend`, `FileFormat::{Envelope, Flat}`, `OnCorrupt`, `QuarantineName`, `flat_migration`, `update_at` (compare-and-swap), `.bak`/`.lock` sidecars, per-field repair (`store.rs:1160-1293`), manifest codegen, change-request queue, optional SQL backend. Platforms: file backend is cross-platform; durability differs per OS (`options.rs:145-161`).

### 1.3 Differences

| # | Difference | Live | Main / RightKit | Status |
|---|---|---|---|---|
| S1 | File format | Flat JSON object | Same: `FileFormat::Flat`, trailing newline (`MAIN/src-tauri/src/settings.rs:40-44`). Undeclared keys and unchanged bytes are kept (`RK/.../options.rs:25-28`) | Verified |
| S2 | Schema version | 13 | 13, unchanged. `migrate()` runs as the store's `flat_migration` (`MAIN/.../settings.rs:48-63`) | Verified |
| S3 | Existing users' files | n/a | Read as-is; no envelope conversion. A live build can still read a file main wrote (rollback safe) | Verified by reading; not run |
| S4 | One field with a wrong type | Whole file renamed to `.corrupt`, all settings reset (`LIVE/.../settings.rs:41-55`) | Only that key is dropped, its default applies (`RK/.../store.rs:1175-1178`) | Verified. Gain |
| S5 | Unparseable JSON | Rename to `settings.json.corrupt`, defaults | Same name (`QuarantineName::Fixed(".corrupt")`, `MAIN/.../settings.rs:44`), then falls back to `settings.json.bak` if present (`RK/.../options.rs:123-128`) | Verified. Gain |
| S6 | File unreadable (permissions, lock) | Treated as first run; the next save overwrites the user's file (`LIVE/.../settings.rs:26-31`) | Runs on defaults and refuses every save until the file opens (`MAIN/.../settings.rs:30-32`, `:119-127`) | Verified. Gain |
| S7 | New files beside settings.json | None | `settings.json.bak` and `settings.json.lock` (`sidecars: true` default, `RK/.../options.rs:257`) | Verified |
| S8 | Read cost | Memory clone | One `stat` per `snapshot()`; re-reads only when inode/mtime/length changed (`RK/.../store.rs:445-468`) | Verified. External edits are now picked up without restart. Cost on hot paths (hotkey hook) is a suspicion, not measured |
| S9 | Concurrency | In-process mutex only | Cross-process lock file plus revision compare-and-swap (`MAIN/.../settings.rs:112-150`) | Verified. Gain |
| S10 | Windows replace | `ReplaceFileW` | `rename` (default `ReplaceMode::Rename`; not write-through on Windows, `RK/.../options.rs:154`) | Verified. Power-loss behaviour not verified on either side |
| S11 | Snapshot for the UI | n/a | `snapshot_with_revision()` decodes the raw file with no per-field repair, so a file with one bad field makes `get_settings_snapshot` fail until the next write repairs it (`MAIN/.../settings.rs:416-438`, the comment names the gap) | Verified. Gap: `Store::snapshot()` closes it but is in 0.3.3+, after the pinned 0.3.1 (`RK/.../CHANGELOG.md`, "Value and revision from one read (#1100)") |
| S12 | Key order of `HashMap` fields | struct order | RightKit warns `HashMap` fields serialise in unstable order (`RK/.../options.rs:83-85`); `extras`, `snippets`, `replacements` are `HashMap` | Suspicion: cosmetic file churn only |

### 1.4 What main calls

RightKit for persistence: `MAIN/src-tauri/src/settings.rs:10-13` (imports), `:40-78` (open), `:102-150` (transactions), `:406-414` (`Store::get`). Own code for schema, migration, patch allowlist, typed accessors (`MAIN/heardright_core/src/settings/blob.rs`, `patch.rs`). Both, by design.

### 1.5 Changes after live

Deliberate:
- New keys `sync_provider`, `streaming_insertion` and `onboarding.state` (`MAIN/heardright_core/src/settings/patch.rs:174-180`, `:194-196`, `:287-325`).
- `telemetry_usage` removed from the struct and allowlist; an existing value survives in `extras`.
- `lalt` added as a push-to-talk key (`patch.rs:261`).
- Date validation moved to `rightkit_time` (`blob.rs:85-94`).

Possible regression: S11 only.

### 1.6 Verdict

**Right shape for HeardRight.** The flat format, fixed quarantine name and flat-migration hook reproduce the shipped file contract exactly, and failure handling is strictly better than live. The one gap (S11) is already fixed in the RightKit tree and needs a published version at or above 0.3.3.

---

## 2. `@rightkit/settings-ui` (settings screens)

### 2.1 What live does

- `LIVE/src/pages/Settings.tsx` (83 lines): one scrolling page, no tabs, no search. Sections in order: Speech recognition, Mic and audio, Application, Apple sync, Developer (`:50-77`).
- Section components under `LIVE/src/components/settings/` (Account, AppleSync, Application, Developer, DeviceManagement, License, Replacements, Snippets, Updater, speech/*) plus pages `About.tsx`, `Controls.tsx`, `Shortcuts.tsx`.
- State through the app's `useSettings` hook with optimistic patching; no revision.
- Theme is not in Settings: a light/dark toggle sits in the title bar (`LIVE/src/components/WindowControls.tsx:30-41`).

### 2.2 What RightKit provides

`RK/packages/settings-ui/src/`: `SettingsPanel` (nav, search, section frame), primitives `SettingsGroup`, `SettingRow`, `SelectControl`, `PathControl`; `standardSections()` with built-ins appearance, updates, models, license, legal, api-keys, diagnostics, doctor, about (`standard.ts`); `SettingsBackend` port with `load`, `patch(patch, expectedRevision)`, `subscribe` (`types.ts:13-20`); headless search hooks; a shell adapter for `rightkit_settings_*` commands. React, any webview.

### 2.3 Differences

| # | Difference | Status |
|---|---|---|
| U1 | Layout changed from one scrolling page to tabs plus a search box. Main hides the panel's own nav and search and draws its own tabs (`MAIN/src/pages/Settings.tsx:199-227`, `hideNav`, `hideSearch`, `hideSectionHeading`, `paint={false}`) | Verified. Deliberate redesign |
| U2 | Of nine standard sections main uses one (appearance); eight are switched off and `about` is built then hidden (`MAIN/src/pages/Settings.tsx:147-160`). License, updates, models, legal, diagnostics stay HeardRight's own components | Verified. The standard sections are not the shape HeardRight uses |
| U3 | Revision type: the port types `revision` as a JS `number` (`RK/packages/settings-ui/src/types.ts:8`); the flat store's revision is a 64-bit file fingerprint. Main maps it through a 64-entry handle table (`MAIN/src/lib/rightkitSettingsBackend.ts:9-32`) | Verified. Package gap; the app works around it |
| U4 | Theme vocabulary: the appearance section reads and writes `appearance.theme` = system/light/dark (`RK/packages/settings-ui/src/appearance.tsx:32`, `:57`); HeardRight stores `app_theme` = auto/ember/ember-light. Main translates both ways (`rightkitSettingsBackend.ts:34-40`, `:53-61`) | Verified. Works through the adapter |
| U5 | Font choice: live and main both store `app_font`, neither tree has a font picker in Settings | Verified (grep) |
| U6 | The standard models section (`ModelsBackend` with install/cancel/remove) is not used; HeardRight keeps its own `ModelCard` and `SpeechRecognitionSettingsImpl` | Verified |

### 2.4 What main calls

RightKit: `SettingsPanel`, `standardSections`, `mergeSections`, `orderSections`, `revealSetting` (`MAIN/src/pages/Settings.tsx:1-9`); `SettingsGroup` and `SettingRow` wrapped by the app's row components (`MAIN/src/components/settings/SettingsSection.tsx:6`, `SettingRow.tsx:8`, `CompactSettingsGroup.tsx:2`); stylesheet (`MAIN/src/index.css:4`). Own code: every section body, the tab strip (`SettingsTabs.tsx`), the search registry (`MAIN/src/lib/settingsRegistry.ts`, 637 lines), the backend adapter. Both.

### 2.5 Changes after live

Deliberate: tabs and search (U1); Appearance section in Settings; "Open at Login" wording on macOS and row sublabels (`MAIN/src/components/settings/ApplicationSettings.tsx:29`, `:68`, `:90`); history key rotation row; sync provider section behind a flag; streaming-insertion developer toggle.

No regression found by reading. Rendering was not checked.

### 2.6 Verdict

**Right shape with listed gaps.** As a frame plus row primitives it fits and HeardRight loses nothing, because all product sections stay app-owned. The gaps are U3 (revision as a number) and U2: the standard sections do not match HeardRight's license, update and model screens, so the 4.4k lines of live settings UI mostly do not move.

---

## 3. `@rightkit/onboarding` (first run)

### 3.1 What live does

- Controller: `LIVE/src/pages/onboarding/OnboardingImpl.tsx`. Nine steps (`constants.ts:3-13`): Own it, Language, Download, Mic, Shortcuts, Commands, Pro trial, App lives, Finish setup. Step index is React state only; every launch starts at step 0 (`OnboardingImpl.tsx:22-35`).
- Footer per step: `Actions.tsx`. Dynamic labels: "Get started" (`:109`), "Continue without microphone" (`:147`), trial "Skip" vs "Continue" (`:176`), "Checking recognition…" vs "Finish setup" (`:192`), dynamic Keychain hint (`:93-96`).
- Model: `useOnboardingModels.ts`. Download starts on entering step 2 (`:116-151`); auto-advances 900 ms after install, including when the model was already installed (`:123-133`); elapsed-seconds counter while preparing (`:103-113`).
- Permissions: `useOnboardingPermissions.ts` (460 lines; macOS Keychain, microphone, Accessibility). Mic check and live shortcut detection: `useOnboardingRecording.ts`. Recognition calibration: `useOnboardingRecognitionSetup.ts`.
- Legal: separate page before onboarding, `LIVE/src/pages/LegalAcceptance.tsx`, routed in `LIVE/src/App.tsx:306-317`.
- Native side: `LIVE/src-tauri/src/onboarding_mic.rs`, `onboarding_calibration.rs` and submodules.
- QA entry `?qa&qa-step=N` (`OnboardingImpl.tsx:22-30`).
- Platforms: macOS and Windows branches throughout; iOS strings present.

### 3.2 What RightKit provides

`RK/packages/onboarding/src/`: `Onboarding` component (`Onboarding.tsx:154-350`) with legal gate first, then built-in `permissionsStep`, `modelsStep`, `licenseStep`, `signInStep` (`steps.tsx`), then app steps. Persists progress under settings key `onboarding.state` (`Onboarding.tsx:8`). Step contract: `canContinue`, static `blockedHint`, static `continueLabel`, `onContinue` veto, `skippable` (`types.ts:217-236`). Legal receipt port with native `create` or legacy `write` (`types.ts:149-157`). React only; README states "Rust pair: None".

### 3.3 Differences

| # | Difference | Status |
|---|---|---|
| O1 | Steps: live 9, main 8. Language and Download merged into one Model step (`MAIN/src/pages/onboarding/flow.ts:7-16`) | Verified. Deliberate |
| O2 | Dynamic footer labels lost. `continueLabel` and `blockedHint` are one static string per step (`RK/packages/onboarding/src/types.ts:225-228`). Main sets no `continueLabel` (`MAIN/src/pages/onboarding/OnboardingImpl.tsx:291-300`), so "Get started", "Continue without microphone", trial "Skip", "Checking recognition…" all become "Continue" / "Finish setup". The hint is a fixed sentence (`flow.ts:52-61`, comment states the limit) | Verified. Loss; package gap |
| O3 | Frame: HeardRight's approved design is one centred column with no rail. Pinned 0.6.3 only has the rail frame, so main hides the rail with CSS and redraws "Step N of M" because the frame's count includes the hidden Agreement step (`MAIN/src/index.css:283-303`, `flow.ts:23-31`). `frame="card"` exists in 0.6.4, unpublished (`RK/packages/onboarding/CHANGELOG.md:13`) | Verified. Gap closes with 0.6.4 |
| O4 | About 20 CSS rules override package styles (buttons, badges, padding) (`MAIN/src/index.css:283-400`) | Verified. Fragile against package class renames |
| O5 | Resume: main persists step progress in `settings.json` and resumes at the saved step after relaunch; live restarted at step 0 | Verified. Gain |
| O6 | Model already installed: live auto-advanced after 900 ms; main never auto-advances in that case, only when the download completes while the step is open (`flow.ts:94-106`) | Verified. Deliberate |
| O7 | Model row: live showed elapsed seconds during "preparing"; RightKit row shows state badge and progress bar only (`RK/packages/onboarding/src/steps.tsx:107-120`). Main hides the raw-state badge except installed/error (`index.css`, "RightKit badges the raw model state") | Verified. Minor loss |
| O8 | No cancel for the download on either side; the package shows Pause only if the backend has `cancel` (`steps.tsx:116`), main supplies none (`OnboardingImpl.tsx:92`) | Verified. Same as live |
| O9 | Permissions: RightKit's `permissionsStep` (list plus Allow button) is not used. HeardRight's Keychain, microphone-with-signal-check and Accessibility flows stay app steps (`OnboardingImpl.tsx:164-237`) | Verified. The built-in step cannot express mic signal verification or live shortcut capture |
| O10 | Trial: RightKit's `licenseStep` is not used; HeardRight's email-then-key `TrialStep` stays | Verified |
| O11 | Legal acceptance time: both live and main stamp `new Date().toISOString()` in the webview. Main uses the deprecated `write` port, not native `create` (`OnboardingImpl.tsx:127-155`) | Verified. Same as live; package's preferred native path not adopted |
| O12 | First-run legal now inside the RightKit gate; re-acceptance for onboarded users still uses the standalone page (`MAIN/src/App.tsx:327-334`) | Verified. Two code paths for one screen |
| O13 | Setup forced to light theme regardless of saved theme (`OnboardingImpl.tsx:267-280`) | Verified. Deliberate; live onboarding was also drawn light with hard-coded colours |
| O14 | `?qa&qa-step=N` entry removed (no match in `MAIN/src`) | Verified |
| O15 | Mic step Continue rule is equivalent to live (`flow.ts:34-45` vs `LIVE/.../Actions.tsx:84-92`) | Verified |
| O16 | Finish sequence identical: same settings patch, shortcut handoff, pill reveal, hotkey registration (`MAIN/.../OnboardingImpl.tsx:352-367` vs `LIVE/.../OnboardingImpl.tsx:83-116`) | Verified |
| O17 | Native onboarding Rust (`onboarding_mic.rs`, `onboarding_calibration.rs`, about 1.9k lines) is mapped to `@rightkit/onboarding`, which is a React package with no Rust pair | Verified. Inventory target has nowhere to hold this code |

### 3.4 What main calls

RightKit: `Onboarding`, `modelsStep`, types, stylesheet (`MAIN/src/pages/onboarding/OnboardingImpl.tsx:2-12`, `:325-335`, `:370-394`); stylesheet again in `MAIN/src/pages/LegalAcceptance.tsx:20`. Own code: all eight step bodies, all four hooks, model backend adapter, legal receipt adapter, native Rust. Both: RightKit is the frame, HeardRight is the content.

### 3.5 Changes after live

Deliberate: O1, O5, O6, O13; Windows caption buttons on the setup window (`OnboardingImpl.tsx:393`); model size text per platform (`ModelStep.tsx:21`).

Look like regressions against live: O2 (button labels and hints), O7 (minor), O14 (QA entry).

### 3.6 Verdict

**Right shape with listed gaps.** The frame reproduces live's flow, gates and finish sequence and adds resume. HeardRight loses state-dependent button labels and hints (O2), which needs `continueLabel`/`blockedHint` to accept a function of the step context, and it needs 0.6.4's card frame published to drop the CSS overrides (O3, O4). The "~10% line-matched" figure is consistent with this: the package holds the frame, not HeardRight's steps.

---

## 4. `rightkit-tauri-shell` (windows, tray, single instance, autostart)

### 4.1 What live does

- Single instance: `tauri-plugin-single-instance`; a second launch shows the hub, deferred on macOS until startup completes (`LIVE/src-tauri/src/app_run.rs:840-862`).
- Autostart: `tauri-plugin-autostart` with a LaunchAgent on macOS (`app_run.rs:867-870`); after each enable, PlistBuddy adds `LowPriorityIO` and a `--login-launch` argument (`LIVE/src-tauri/src/commands/updates.rs:10-60`). Login launch detection: `login_launch.rs`.
- Tray (Windows): `LIVE/src-tauri/src/tray.rs:98-181`. Menu: Toggle Recording, Open Hub, Check for Updates, Help & Feedback, Quit. Left click opens the hub from the pill origin (`:163-174`). Per-state icons (`:73-88`). macOS uses an out-of-process status item (`menubar_helper.rs`).
- Close: `app_run.rs:484-535`. `close_behavior == "quit"` exits; otherwise the hub animates to the pill, hides and its WebView2 is suspended by hand-written COM calls (`hub_window.rs:11-117`).
- Hub window: `hub_window.rs` (1194 lines): surface sizing, show/hide choreography, splash.
- Deep links: none in either tree (grep).

### 4.2 What RightKit provides

`RK/crates/rightkit-tauri-shell/src/`: `Shell` builder and `ShellPlugin` (close behaviour, menu, frameless style, idle suspend), `windows::builder`, `tray` (`TraySpec`, `LeftClick::{ToggleWindow, ShowMenu, Show}`), `SingleInstance` (re-export of `rightkit-instance`, with legacy-plugin handoff), `autostart` (re-export of `rightkit-autostart`, with plugin-entry migration), `watchdog`, `harden`, `host` services (license, settings, legal), `update`, `window_state`. Windows, macOS, Linux.

### 4.3 Differences

| # | Difference | Status |
|---|---|---|
| H1 | Single instance moved to RightKit with a legacy handoff: `ClaimOptions::with_legacy(id).holding_legacy()` so a new build and a still-running 0.1.213 arbitrate (`MAIN/src-tauri/src/app_run.rs:322-352`) | Verified by reading; cross-version handoff not run |
| H2 | Failure policy is new: a live copy that does not accept the launch makes the new process exit 1; if arbitration cannot run, the app starts anyway (`app_run.rs:340-350`) | Verified. Deliberate |
| H3 | Autostart moved to RightKit. Args and `LowPriorityIO` are options instead of PlistBuddy edits (`MAIN/src-tauri/src/commands/updates.rs:38-57`). Existing users' plugin entry is migrated once, carrying the Windows "disabled in Startup apps" state (`updates.rs:60-76`, `RK/crates/rightkit-autostart/src/autostart.rs:607-634`) | Verified by reading; migration not run |
| H4 | Autostart now refuses outside an installed location (build folder, mounted DMG, translocated copy) (`updates.rs:26-36`) and rewrites a stale path on launch (`:68-74`) | Verified. Deliberate gain |
| H5 | Tray left click: the RightKit tray has no custom left-click hook. Main declares `LeftClick::ShowMenu`, then clears the menu-on-left-click flag on the raw icon and listens for the shell's tray event to open the hub (`MAIN/src-tauri/src/tray.rs:153-159`, `:182`, `:229-232`, `:239-257`) | Verified. Package gap; workaround depends on the shell emitting the click event in `ShowMenu` mode |
| H6 | Tray menu gained "Microphone Settings…"; tooltip and left click change when Windows blocks the microphone (`tray.rs:70-125`, `:176`, `:247-251`). "Help & Feedback" became "Help and Feedback" | Verified. Deliberate |
| H7 | Tray icon: main passes `.ico` bytes in the field named `icon_png` (`tray.rs:167-170`); decoding works because Tauri is built with `image-ico` (`MAIN/src-tauri/Cargo.toml:81`) | Verified by reading |
| H8 | Close: the hide now belongs to the shell (`CloseBehavior::HideToTray`, `app_run.rs:25`); the app's handler still runs the animation first and the shell's `on_visibility(Hidden)` does the rest (`app_run.rs:57-69`, `:802-818`) | Verified by reading. Ordering between the app handler and the plugin handler not verified at runtime |
| H9 | `close_behavior` setting ("minimize", "quit", "destroy"; default "destroy") is still read by the app handler for "quit" (`MAIN/src-tauri/src/pill_overlays.rs:641-662`), but the shell is fixed to hide-to-tray. No UI sets the value in either tree | Verified. Same effective behaviour as live |
| H10 | WebView2 suspend/resume replaced by the shell's idle machine with zero delay (`app_run.rs:31-35`, `MAIN/src-tauri/src/hub_window.rs:3-30`). Telemetry event names changed (`webview2_try_suspend_*` to `webview2_idle_*`) | Verified. About 110 lines of COM code removed |
| H11 | Windows are built through `windows::builder`, which adds new-window routing and frameless style; the builder is Wry-only so main downcasts the app handle (`hub_window.rs:955-960`, `:1112-1117`) | Verified |
| H12 | Shell runs "lifecycle-only": its own single-instance plugin, hardening, navigation policy, window-state restore and watchdog are off (`app_run.rs:3-11`, `:75`). Hub geometry and show/hide stay app code (`hub_window.rs`, 1223 lines) | Verified. Most of the 3.3k live shell lines do not move |
| H13 | macOS app menu now built by the shell; Tauri default menu disabled (`app_run.rs:69-74`, `:1170-1176`) | Verified by reading |
| H14 | External links: a shell hardening script intercepts link clicks and routes them to an app allowlist (`app_run.rs:78-103`); native context menu suppressed outside editable fields | Verified. New behaviour |
| H15 | `menubar_helper.rs`, `helper_process.rs`, `platform_setup.rs`, `background_updates.rs` are mapped to the shell crate but call `rightkit-child-process`, `rightkit-fs`, `rightkit-onnx` instead; no shell API is used in them | Verified (grep) |

### 4.4 What main calls

RightKit: `Shell`, `ShellPlugin`, idle, lifecycle (`MAIN/src-tauri/src/app_run.rs:12-76`); hardening (`:82-103`); watchdog (`:126`); `SingleInstance` (`:322-352`, `commands/updates.rs:119`); tray (`tray.rs:160-257`); autostart (`commands/updates.rs:38-84`, `:176`); `windows::builder` (`hub_window.rs:962`, `:1117`); host license commands (`app_run.rs:1321-1325`); `rk_shell_open_external` (`:1379`). Own code: hub lifecycle, splash, macOS status item helper, update flow, login-launch detection. Both.

### 4.5 Changes after live

Deliberate: H1 to H4, H6, H10, H13, H14. The Tauri plugins for single instance, autostart, global shortcut and HTTP are gone from `MAIN/src-tauri/Cargo.toml`.

Risks rather than confirmed regressions: H1 and H3 depend on migrations from the plugins that I read but did not run; H5 depends on shell behaviour in a mode not meant for it; H8 depends on handler ordering.

### 4.6 Verdict

**Right shape with listed gaps.** Single instance and autostart are the right shape and include the upgrade paths a live 0.1.213 user needs. The tray needs a custom left-click action (H5) so the app stops reaching into the raw icon. The hub's window choreography is HeardRight-specific and correctly stays out, which means the "adopt" label on `hub_window.rs` and the helper files overstates what can move.

---

## 5. `@rightkit/react-shell` (app frame)

### 5.1 Live

Own frame: `LIVE/src/components/TitleBar.tsx` (386 lines), `Sidebar.tsx` (259), `WindowControls.tsx` (138, including a light/dark toggle at `:30-41`), `CommandPalette.tsx` (180), composed in `LIVE/src/App.tsx:329-357`.

### 5.2 RightKit

`RK/packages/react-shell/src/react/`: `AppShell`, `Titlebar`, `CaptionControls`, `PageShell`, `CommandPalette`, `ErrorBoundary`, tables, tabs, toasts, headless `useToggle` and `useSegmentedControl`, `createThemeStore`; `shell.css`.

### 5.3 Differences

| # | Difference | Status |
|---|---|---|
| R1 | Frame is now `AppShell` with nav groups, title bar slots, palette and status footer (`MAIN/src/App.tsx:353-370`). App files shrank: TitleBar 386 to 81, WindowControls 138 to 29, CommandPalette 180 to 3 | Verified |
| R2 | Title-bar theme toggle is gone from the app's WindowControls; theme is changed in Settings, Appearance, or through `AppShell`'s `onThemeChange` (`App.tsx:166-179`, `:365`). Whether `AppShell` draws its own toggle was not read | Partly verified |
| R3 | `CaptionControls` always draws Maximise; HeardRight windows are not maximisable, so main hides it with a CSS class (`MAIN/src/components/WindowControls.tsx:20-25`) | Verified. Package gap |
| R4 | Per-page title-bar tools (history filters, push-to-talk, pill visibility) kept as app code in the `titlebarEnd` slot (`MAIN/src/components/TitleBar.tsx:73-76`) | Verified. No loss |
| R5 | `--ink` meaning: RightKit uses it for text, HeardRight used it for chrome, so product surfaces were renamed to `--hr-ink` (`MAIN/src/lib/theme.ts:7-8`, `:59`) | Verified. Any missed `--ink` use would invert; not audited |

### 5.4 Verdict

**Right shape with listed gaps.** It replaces the generic frame and leaves HeardRight's title-bar tools in slots. Gaps: a `maximizable` option on caption controls (R3) and the `--ink` token clash (R5), which is a migration hazard rather than a missing feature.

---

## 6. `@rightkit/theme`

### 6.1 Live

`LIVE/src/lib/theme.ts`: two presets ("ember" dark, "ember-light"), writes about 20 CSS variables, `data-theme="ember"|"ember-light"`, default font "hanken" (`:28-68`, `:125-175`).

### 6.2 RightKit

`RK/packages/theme/src/`: brand file to CSS tokens, `applyBrand`, `applyTokens`, `prefersDark`, contrast checks, CLI. README: "Rust pair: None"; no Swift output.

### 6.3 Differences

| # | Difference | Status |
|---|---|---|
| T1 | Palette changed. Example: dark panel `#221E1A` to `#211B18`/`#2D2521`, dark line `#352F2B` to `#4E4036`, light line `#BDB2A4` to `#CDC1B0` (`LIVE/src/lib/theme.ts:35-66` vs `MAIN/src/lib/theme.ts:15-24`, `:56-63`) | Verified. Deliberate or not cannot be told from code |
| T2 | `data-theme` value changed from `ember`/`ember-light` to `dark`/`light` (`MAIN/src/lib/theme.ts:54`). CSS in both trees keys on `data-theme-mode` only (`MAIN/src/index.css:514-518`) | Verified. No selector breaks |
| T3 | Default font when unset: "hanken" to "system" in the web layer (`MAIN/src/lib/theme.ts:31`), while Rust still declares `DEFAULT_APP_FONT = "hanken"` and the patch allowlist rejects "system" (`MAIN/heardright_core/src/settings/blob.rs:31`, `LIVE/.../patch.rs:43-49` unchanged in main) | Verified. Inconsistent; existing users with no stored font see a different typeface |
| T4 | Inventory maps `ios-native/HeardRight/AppMark.swift`, `Color+HR.swift`, `Font+HR.swift` to `@rightkit/theme`, which cannot emit Swift | Verified. Wrong target for those three files |

### 6.4 What main calls

`applyBrand`, `applyTokens`, `prefersDark` (`MAIN/src/lib/theme.ts:2`, `:53`, `:65`), `onPrefersDarkChange` (`MAIN/src/App.tsx:39`). The token values themselves stay in the app.

### 6.5 Verdict

**Right shape for the web app; wrong target for the iOS files.** On the web it is a small helper and HeardRight keeps its own palette. T1 and T3 are visible changes from live that should be confirmed as intended.

---

## Top gaps, in order

1. Onboarding: state-dependent Continue labels and hints are lost (O2). Needs a package change.
2. Onboarding: card frame is in unpublished 0.6.4; main carries about 20 CSS overrides until then (O3, O4).
3. Settings storage: UI snapshot read has no per-field repair on the pinned 0.3.1 (S11). Fixed in tree at 0.3.3+.
4. Shell tray: no custom left-click action; main reaches into the raw tray icon (H5).
5. Settings UI: revision typed as a JS number against a 64-bit store revision (U3).
6. Theme: default font and palette differ from live without a matching Rust default (T1, T3).
7. Inventory targets with no home: native onboarding Rust (O17), iOS Swift tokens (T4), hub and helper files (H12, H15).

## Not verified

- Nothing was compiled or run; all runtime claims are from reading.
- The published 0.3.1 / 0.7.0 / 0.6.3 / 0.7.2 artifacts were not read; the RightKit tree is newer.
- Single-instance and autostart migration from the Tauri plugins on a real 0.1.213 install.
- Visual result of the settings tabs, onboarding CSS overrides and palette change.
- Linux behaviour for any package.
