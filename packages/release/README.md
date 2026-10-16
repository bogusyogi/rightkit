# @rightkit/release

Portable release CLI and SDK for Right Suite products: builds, signs, and publishes to public GitHub Releases or to private R2 with RightApps distribution. It also runs the ownership catalog and lockfile checks and the release gates.

| Item | Value |
|---|---|
| Where it runs | Build and release tooling. Native signing runs on the build host. |
| Rust pair | None. The Rust build broker files in this package (`managed-cargo.mjs`, `managed-roots.mjs`) are moving to the separate RustRight product. Do not extend them here. |
| Version | see INDEX.md |
| License | MIT OR Apache-2.0 |

Signing and publication rules are in the workspace file `docs/rules/release-signing.md`. Public releases use RightKit CI. Private builds run on the native host.

## No test-automation surface ships

Release and update builds (`right-release build`, `publish-update`) must not contain any test-automation or debugging surface; the development lane may. Layers, in the order they run:

1. **Source (before anything is built; `right-release build --dry-run` only loads the config and does not reach this layer)** - `qa-build-features.mjs` refuses a build command, a package.json script it runs, or an app `Cargo.toml` that enables `devtools`, `qa-native`, `qa-control`, `dev-native` or `dev-inspect` (also `--all-features`, and `devtools` on the `tauri` dependency, `qa-control` on a non-optional `rightkit-qa-driver` (or `control` on a non-optional `rightkit-qa`; `rightkit-qa/control` is also refused as a feature item), `dev-inspect` on a non-optional `rightkit-tauri-shell`, a non-optional `tauri-plugin-wdio-webdriver`, or any of them reachable from the default features).
2. **Pre-pack payload scan** - `qa-control-scan.mjs` over the raw EXE (Windows NSIS and portable), the macOS signing set and `notarize.app` (the `.app` going into the DMG; `dmg build` also scans its staging directory), the MSIX `stageDir` before packing, and `target.qaControlScan`. Mach-O, PE and ELF files are matched against five rules: `qa-control-server` (marker `RIGHTKIT-QA-CONTROL-SERVER-PRESENT`), `shell-dev-inspect` (marker `RIGHTKIT-SHELL-DEV-INSPECT-COMPILED`, written by rightkit-tauri-shell's `dev-inspect` feature; no allow-list lifts it), `wdio-webdriver` (2 of 3 of `tauri-plugin-wdio-webdriver`, `tauri_plugin_wdio_webdriver`, `wdio-webdriver:default`), `remote-debugging` (`--remote-debugging-port`, `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS`) and `tauri-devtools` (`plugin:webview|internal_toggle_devtools`, `toggleDevtoolsHotkey`); each signature also as UTF-16LE. A scan that finds no native binary fails.

   Web-bundle mock scan: `release` also runs `assertNoWebBundleMocks` over the frontend dist (`target.webBundleScan`, else `build.frontendDist` of `src-tauri/tauri.conf.json`) before packaging when it already exists (also under `--dry-run`) and after the package command, where a missing or empty dist fails. `right-release bundle-scan` (Node/Python runtime roots) and `identity-scan` (needs the app's declared cdhash/team/identifier) are not automatic: both need app-specific inputs, so each app runs them from its own `checks`.
3. **Packed MSIX** - the finished `.msix` is read through its ZIP central directory (stored and deflate entries, nested ZIPs to depth 3, 1 GiB per entry, 8 GiB inflated per container; encrypted, unsupported-method or oversized entries are findings).
4. **Hardening targets** (final artifacts) and **every sealed file** (`verifySealedRelease`, used by every seal and upload path) run the same rules, including ZIP containers.

Not coverable after packing: an NSIS exe (LZMA payload) and a compressed DMG. They are covered only by layer 2, so the release fails when no pre-pack scan saw a native binary, and the log names each unreadable installer. Not coverable at all: assets embedded compressed in the app, resources an NSIS installer packs that were never in the scanned set (list them in `target.qaControlScan`), and any surface without a rule in `QA_RULES`. The `wdio-webdriver` rule is weaker than the marker rule (signatures chosen without the plugin source). Full list: `crates/rightkit-qa-driver/README.md`, "QA control never ships".

### DevTools policy (owner's fixed design)

Every Right app has absolutely NO access to DevTools; the single exception is ViewRight, which may inspect the HTML content it opens, never its own UI or shell. The rules above therefore refuse `devtools` (and rightkit-tauri-shell's `content-devtools`) for every app, with one narrow allowance (`devtools-policy.mjs`): an app whose right-release config declares `devtools: "content-only"`, **and** that is listed in `devtools-policy.json` (`contentOnlyApps`, today only `viewright`), **and** whose Cargo manifest depends on `rightkit-tauri-shell` with `content-devtools`, **and** whose `build.rs` declares an application ACL manifest. Then the source layer accepts `devtools` / `content-devtools`, and the binary layer accepts the `tauri-devtools` signatures only in a binary that also carries `RIGHTKIT-SHELL-DEVTOOLS-CONTENT-ONLY-COMPILED` (written by rightkit-tauri-shell when `content-devtools` is compiled). `--all-features`, `qa-native`, `qa-control`, `dev-native`, `dev-inspect` and every other rule stay refused. The allowance is recorded in the sealed manifest (`devtools`) and re-checked against the allow-list when a sealed file is verified. Any other app declaring it is refused (`devtools-policy.test.mjs`).

## Release switch scan (M21/M23, owner decision D4, ratified)

`release-switch-scan.mjs` proves that release code cannot use an environment variable, argument, settings key or build flag to select a test mode, data root, executable, library, model or identity. The runtime half is `rightkit_daemon::mode`: a build without the `test-mode` cargo feature reads no override selector at all. Registry: `release-switch-registry.json` (forbidden features, the `RIGHTKIT-TEST-MODE-COMPILED` marker, deny names with their class). Apps add their own deny names under `releaseSwitches.forbid`. Selector classes: S1 test activation, S2 authority or identity override, S3 executable/library/model, S4 internal state or data root (S1-S4: forbidden, no allow-list), S5 user-content location (allowed, user gesture), S6 diagnostics/tuning (allowed, bounded value).

1. **Feature closure (authoritative)** - `release.mjs` runs `cargo tree -e features,no-dev --locked --target <triple>` in the app repository (`resolveFeatureClosure`; forwards the build command's `--features`/`--no-default-features`; manifest resolves `targets.<os>.cargoManifest` -> `releaseSwitches.manifest` -> `src-tauri/Cargo.toml` -> `Cargo.toml` and must exist; features are also read from package.json scripts the hooks run (up to three levels) plus `releaseSwitches.features`; no manifest fails with `release-switch-manifest-missing` unless the app sets `releaseSwitches.rust: false` (the only skip); a cargo failure fails a real release, a dry run reports it) and passes it to `featureClosureFindings(...)` and `commandSwitchFindings(argv)` refuse `test-mode`, `qa-native`, `dev-native`, `qa-control`, `dev-inspect` anywhere in the resolved graph (finding id `forbidden-feature <name>`). The binary marker below is secondary evidence only. `qa-build-features.mjs` also lists `test-mode` as a forbidden feature, including on a non-optional `rightkit-daemon` dependency.
2. **Inventory completeness** - `sourceSwitchFindings` walks app source (Rust, TS/JS, Swift, C; not tests, fixtures, `node_modules`, `target`) and rejects an environment read whose name is not in `switches.json` (`unregistered-read`), a non-literal read in a file not listed under `dynamicReads` (`dynamic-read`), an S1-S4 entry without `compiledOut` (`not-compiled-out`), and a malformed inventory (`inventory-invalid`). Argument flags are enforced for deny-listed flags only.
3. **Binary** - `scanSwitchPaths` / `bufferSwitchFindings` over Mach-O, PE and ELF files: the marker (`test-mode-capability`) and any deny name from S1-S4 (`release-switch-names`), each as Latin-1 and UTF-16LE. The receipt holds the sha256 and size of every scanned file; the caller adds source commit, target triple and resolved features.

`assertNoReleaseSwitches({ treeText, commands, sourceRoot, inventoryText, binaryPaths, appForbid })` runs every layer it is given input for. Limits: packed NSIS/DMG and compressed MSIX members are covered only by the pre-pack scan of the staged payload, symlinks are skipped, and browser JS is checked at the source layer only. `release.mjs` runs the source layer once before packaging (inventory at `releaseSwitches.inventory` or `switches.json`; a missing inventory fails a non-dry-run release) and the binary layer wherever it runs the QA-control binary scan; a finding stops the release. Trust model: a release build reads no override switch at all; the test capability is compiled out (`test-mode` cargo feature, refused by `qa-build-features.mjs`), never hidden behind a flag.

### `switches.json` (one per app)

```json
{
  "schema": 1,
  "app": "viewright",
  "switches": [
    { "name": "HEARDRIGHT_ENGINE_TEST_MODE", "class": "S1", "channel": "env",
      "anchor": "src/test_mode.rs:97", "compiledOut": "feature:test-mode", "note": "" },
    { "name": "LIGHTCRAFT_LOG", "class": "S6", "channel": "env", "valueType": "enum" },
    { "name": "--library", "class": "S5", "channel": "argv", "userGesture": true }
  ],
  "dynamicReads": [ { "file": "src/x.rs", "reason": "why a literal name is impossible" } ]
}
```

- `name` (required, unique): environment variable, argument flag, settings key or `VITE_*` name.
- `class` (required): `S1`..`S6`. `channel` (required): `env`, `argv`, `settings`, `preference` or `frontend`.
- S1-S4 need `compiledOut`: `feature:<name>`, `cfg:test` or `cfg:debug_assertions` (the read exists only there). S5 needs `userGesture: true`. S6 needs `valueType` of `bool`, `int` or `enum`.
- `anchor` and `note` are informational. Fixtures: `fixtures/release-switches/` (`app/` is a clean example).

## Commands

`right-release` maps these subcommands to the modules in this package:

| Group | Subcommands |
|---|---|
| Build | `release`, `build`, `package` |
| Build (SwiftPM apps) | `swift-package` (`windows-stage`, `msix`, `mac-app`) (config schema 1: `windows.{exe,out,searchDirs,include,dependencies,publisher,assets}`, `mac.{executable,bundleId,out,frameworks,resources,entitlements}`) |
| Install | `install-app` |
| Publish | `publish`, `upload`, `publish-update`, `github`, `mirror-root-artifact` |
| Signing | `sign-windows`, `sign-updater`, `signing-agent`, `create-mac-updater` |
| Checks | `deps`, `hardening`, `harden`, `hardeningscan`, `cargo-target` |
| Ownership | `ownership`, `ownership-approval`, `ownership-report` |
| Other | `dev`, `doctor`, `suite-doctor`, `qa`, `dmg`, `cache`, `apple`, `inventory`, `model`, `addon`, `hooks`, `lsclean`, `generate-dmg-background`, `publish cargo`, `publish npm`, `publish swift` |

## QA receipts (warn-and-record)

`right-release qa --stage source|installed --platform mac|win` still runs `rightkit-qa` and writes a signed receipt, and receipt verification is unchanged. A missing or invalid receipt no longer blocks a release. `build`, `upload`, `publish-update` and `github` print `[right-release] QA warning: ...` to stderr, continue, and record the outcome:

- Source stage: sealed into the manifest as `qa: { status: "passed", sourceReceiptDigest, floorDigest }` or `qa: { status: "missing" | "rejected", reason }`.
- Installed stage: written to `.right-release/state/<releaseId>/<platformDir>/qa-installed.json` as `{ status, reason?, receiptDigest?, checkedAt, provider }`. It is written when publication starts, so a later upload failure still leaves a record.

Audit ad hoc or on a weekly schedule with `right-release qa audit [--app <dir>] [--json] [--since <YYYY-MM-DD>]`. It reads the app repo's sealed manifests and state and lists source and installed QA status per release and platform (passed, missing, rejected, not-uploaded). It exits 0 when every uploaded build has passing QA and 1 when any uploaded build lacks it. An installed receipt can only chain to a build sealed with a passing source receipt, so a build sealed with `qa.status: "missing"` needs source QA and a rebuild before installed QA can pass.

## Local app install (macOS)

Agents must not run `osascript`, `open`, `screencapture`, or `notifyutil -p` directly. Install a signed build through:

```sh
right-release install-app --dmg <path.dmg> [--app-name Pulse.app] [--bundle-id dev.orthic.pulse] [--dest /Applications] [--relaunch|--no-relaunch] [--json]
```

It mounts the image read-only and hidden (`-nobrowse`), verifies the app (`codesign --verify --deep --strict`, `spctl --assess`, `stapler validate`, bundle id, and Team ID against the installed copy; a Team ID change is refused) before touching the installed copy, stages a `ditto` copy beside it in the destination and verifies that copy, quits the running instance with SIGTERM (15 s, then SIGKILL), swaps the staged copy in (one atomic exchange when the `rightkit-swap` helper is found, otherwise two journaled renames; the old bundle is restored on failure), re-verifies the installed copy, only then moves the old bundle to `~/.Trash/<App> <timestamp>.app`, and relaunches only if it was running or `--relaunch` is given, using `open -g -j -b` (background, hidden, never activated). `--json` emits one lifecycle event per line (`start`, `verified`, `quit`, `replaced`, `relaunched`, `done`/`failed`, among others). `right-release install-app --recover [--dest <dir>] [--app-name <App.app>] [--json]` rolls an interrupted install forward or back from its journal; every install runs that recovery first. Win32 exits with "not implemented on win32". Exit codes: 0 ok, 1 failed, 2 usage, 3 unsupported platform.

Development builds (from `@rightkit/ci` `devArtifact`, Developer ID-signed but not notarized) install with `--dev` and exactly one of `--dmg`, `--zip <path.zip>` (extracted with `ditto -x -k`), or `--app <path.app>`: `right-release install-app --dev --zip dev-mac.zip`. `--dev` still requires `codesign --verify --deep --strict`, a Team ID, no Team ID change, and a bundle-id match; it skips only `spctl` and `stapler`, removes `com.apple.quarantine` from the new bundle only, prints `DEV BUILD (not notarized)`, and sets `dev: true` on every JSON event.

## Runtime budget gate (crates)

`publish cargo` runs the `runtime budget` step (`runtime-budgets.mjs`) right after the secret scan. Full contract, schema and refusal table: `docs/RUNTIME-BUDGETS.md`.

What the gate enforces, exactly:
- **Three classes, fail-closed.** always-on (measured budget at publish), `continuous = "held-resource"` with a `resources` table (structural check at publish, installed-app measurement at adoption via `appRuntimeBudgetAdoptionCheck`, wired into `collectPreflight`), and `continuous = false` (bounded per call).
- **Membership is fail-closed.** `Cargo.toml` `[package].name` must equal `--crate`. Every crate needs `runtime-budget.json`, `[package.metadata.rightkit] continuous = false` with a `continuous_reason`, or `continuous = "held-resource"` with a `resources` table; a crate with none of them is refused. The 11 crates in `RUNTIME_BUDGET_CRATES` cannot declare `continuous = false`. A `continuous = false` crate is refused when its non-test `src` matches a pattern in `CONTINUOUS_PATTERNS` (`thread-spawn`, `task-spawn`, `runtime-builder`, `sleep`, `timer`, `listener`, `watcher`, `event-loop`, `audio-stream`, `onnx-session`, `ggml`, `conn-pool`, `resident-static`, `detached-child`) and the reason does not contain that id.
- **A continuous crate needs a measured budget.** Declared ceilings and `requiredScale`; a status pointer to a `pub` item exposing resident memory; per-host measurements of the published version, each backed by raw bench output under `runtime-budget.evidence/` (`runtime-budgets.mjs record`) whose sha256 and `RESULT budget` line the gate re-checks; every observed value within its ceiling and non-zero resident memory; a fresh, in-ceiling, at-scale measurement on each required host (macOS and Windows, or macOS alone only when the release evidence records `hostPolicy: "darwin-only"` with a reason, which the budget file may not contradict); a README section equal to the one generated from the JSON.
- **Freshness** covers the crate's `src`, `benches`, `Cargo.toml`, `build.rs`, the `src`, `Cargo.toml` and `build.rs` of every workspace `path` dependency, new untracked files there, and the crate's `Cargo.lock` dependency closure, against the measured commit (which must be in the checkout and an ancestor of HEAD).

What it does not do: run the bench, prove the bench ran, detect `[profile]`, `[patch]` or toolchain changes, or find continuous work outside the scanned patterns. `runtimeBudgetAdoptionCheck` adds the need for a fresh `installed` measurement naming the app; crates with `publish = false` never reach `publish cargo`, so their adoption must call it.

A real `publish cargo --crate <name>` refuses with `RUNTIME_BUDGET_MISSING` listing every problem; `--dry-run` prints `[FAIL] runtime-budget: ...` and continues.

## QA adoption (mandatory)

RightKit QA is a mandatory component: every Orthic Labs app adopts it. `assertQaAdoption` (`qa-contract.mjs`, next to the native-QA backdoor contract) runs in `release.mjs` beside the package and Cargo contracts on every release, `--dry-run` and `doctor` included, SwiftPM apps without a `package.json` too.

- **Refused:** the app has no QA lane. A lane is the native wdio lane (`right-qa.config.mjs`, validated by `validateQaBackdoorContract`), `@rightkit/qa` in any non-`node_modules` `package.json` (dependencies or devDependencies), or `rightkit-qa` in any `Cargo.toml` (dependencies, dev-dependencies, build-dependencies, `[workspace.dependencies]`; path and workspace declarations count). Message: `RightKit QA is mandatory for every Orthic Labs app, and <app> has no QA lane ... Adopt it: ...`.
- **Warned:** a `@rightkit/qa`, `@rightkit/qa-chrome` or `rightkit-qa` pin behind the latest published version in `rightkit-versions.json` (`[right-release] QA warning: <app> runs RightKit QA behind the latest published version: <pin> -> <latest> (<file>)`). **Refused** when the lag is a breaking bump (`breakingBump` of `rightkit-drift`: a minor bump below 1.0, a major from 1.0). `@rightkit/qa-chrome` is lag-checked but its presence is not enforced.
- **Exempt:** only a repository that is not an app. `qa-adoption-exemptions.json` (`nonAppRepositories`: `{ repositoryIdentity, kind: "library" | "tooling", reason }`, matched against the git `origin`) is RightKit-owned, so a change needs a RightKit release; the RightKit source tree is recognised by `packages/release/package.json` named `@rightkit/release`. There is no per-app opt-out, config key or environment switch.

## Entry points

Bin: `right-release` (`cli/right-release.mjs`). The package has no `exports` map. The CLI is the supported surface.

## Dependencies

| Dependency | Range | Note |
|---|---|---|
| `smol-toml` | `1.9.0` (exact) | Used to parse lock and manifest files |

Peer dependencies: none.

## Example

Illustrative. Dependency checks, as described in the shared-packages doc:

```sh
pnpm add -D @rightkit/release
pnpm exec right-release deps --check
```

## MSIX target and Swift app type

Swift (SwiftPM) apps are a first-class project type. `projectType: "swiftpm"` (config or target; auto-detected from a `Package.swift` with no Cargo manifest) replaces the cargo/rustc requirement with `swift`, and skips the Rust target-bridge, sccache and Perl checks. Tauri/Rust targets are unchanged. `makeappx` and `signtool` are required only when `targets.win.msix` is configured.

`targets.win` with `signingContract: "windows-msix-signed-v1"` packages an MSIX (schema in `msix-config.mjs`): `prePackage` builds and stages a flat payload (`right-release swift-package windows-stage`), then the msix step runs in the immutable order: verify the declared runtime-DLL inventory and PE import closure (fails closed), sign inner DLLs then EXEs through `sign-windows`, `makeappx pack`, sign the `.msix`, verify the signature and that the signer subject equals `Identity/@Publisher`, write the App Installer route, and write `msix-seal.json`. `msix.virtualization.fileSystemWrite|registryWrite` (`enabled|disabled`) are mandatory and emitted as `desktop6` manifest properties. List the `.msix` under `installer.artifacts` so it seals.

`updater.kind: "appinstaller"` (default remains the Tauri minisign flow) generates `msix.appInstaller.out` (https `uri`/`msixUri`); trust is the MSIX Authenticode signature, so no `.sig` is produced and `publish-update` records the sealed package digest.

Portable macOS targets (`macos-developer-id-notarized-portable-v1`) whose `notarize.file` is an archive of bare executables set `notarize.staple: false`: the archive is notarized (Accepted) and recorded, nothing is stapled. Portable Windows and macOS targets with no installer or updater block seal their archives from `targets.<platform>.artifacts`.

Never cancel an MSIX deployment from app or updater code. Cancelling `AddPackageAsync` while it is in `RegistrationChanged` leaves the package `Modified, NeedsRemediation` (0x80073CF8, AppX event 751); the app then cannot activate, so the OnLaunch feed check normally does not run and the package does not self-heal (in the tuple 08 runs one launch attempt in five was followed by a feed update to the newer version; the trigger was not isolated). Measured recovery (SwiftRight SliceApp R5 W08 tuple 08, 2026-10-10, one run per method, user data intact in every recovering case): Settings › Apps › Advanced options › Repair recovers a feed install (with or without a newer version on the feed) and a path install with no feed; `Add-AppxPackage -Path <same-version .msix>` recovers; `Add-AppxPackage -AppInstallerFile <feed uri>` recovers with or without a newer version on the feed. `Reset-AppxPackage`, `-RegisterByFamilyName`, the platform's own `RegisterByPackageFullName` (runs on every launch attempt, reports success, package stays NeedsRemediation) and relaunching do not. Not measured: Repair with the install source gone.

macOS Swift apps use `right-release swift-package mac-app` (Info.plist from config, resources, frameworks, entitlements on the bundle only; nested code signed inside-out before the app).

Build a SwiftPM app with `right-release build --platform mac|win` (add `--config <path>` when the config is not `right-release.config.mjs`). Build is tier-neutral and never uploads, so it rejects `--tier` and takes no `--upload`/`--no-upload`; the old `release --tier patch --no-upload` form no longer works. Pick the tier only at upload: `right-release upload --release <sealed-id> --platform mac|win --tier patch|update` (`--tier` is required; it never builds). `upload` (and `publish`, its alias) requires `distribution.provider: "github-releases"` and `distribution.repository` in the release config; a product distributed through R2 and RightApps publishes with `right-release publish-update --release <sealed-id> --platform mac|win --tier patch|update` (or `RIGHT_RELEASE_TIER`), which requires `distribution.provider: "r2-rightapps"`.

Set `targets.mac.notarize.appBeforePackage: true` (with `notarize.app` naming the `.app`) to notarize and staple the app itself right after signing and before the package step copies it into the DMG, so an app copied out of the DMG passes `stapler validate` offline. The DMG named by `notarize.file` is still notarized and stapled afterward; the app ticket receipt is `macos-app-notarization.json`. Without the flag, behavior is unchanged.

For `projectType: "swiftpm"` on mac with `appBeforePackage: true` and no `targets.mac.package`, right-release builds the DMG itself (same `dmg-build.mjs` as Tauri) at the package step, after the `.app` is signed, notarized and stapled: it runs `xcrun stapler validate` on the app (refusing to pack an unstapled one), builds the DMG at `notarize.file` (must end `.dmg`), Developer ID signs it (receipt `macos-dmg-signing.json`), then notarizes and staples the DMG as before. Options go in `targets.mac.dmg`: `{ volname, background, iconSize, appPosition, applicationsPosition, window, volumeIcon, out }` (`volname` defaults to the app name). A prePackage hook must no longer build the DMG: the config is refused (at build time and by `doctor`) when its command or arguments mention `dmg` or `hdiutil` (heuristic), because that DMG would carry the unstapled app. Keep a custom DMG flow with `targets.mac.dmg: false` or a `targets.mac.package` step (both run after the staple and skip the automatic build).

`sign.prePackageFiles` entries may be `.app` bundles (directories). Nested code (Mach-O files, frameworks, XPC services, plug-ins) is signed first, inside-out and without entitlements, then the bundle is signed; the receipt records a tree digest of the bundle. Any other directory is rejected with a clear error.

### Per-file signing matrix (macOS)

By default every code item is signed with the hardened runtime and no entitlements, so a bundle that needs entitlements on some of its code (an app, a sandboxed extension) declares them per file. Both forms below are optional and backwards compatible; a config of plain path strings signs exactly as before.

- `sign.prePackageFiles[]` entries are a path string or `{ file, entitlements?, hardenedRuntime?, options? }`. `entitlements` is a plist path relative to the release config directory. For a `.app` entry the settings apply to the bundle signature.
- `sign.nestedCode` is a map from a path inside the bundle (`Contents/...`, forward slashes) to `{ entitlements?, hardenedRuntime?, options? }`. It needs exactly one `.app` entry in `sign.prePackageFiles` to resolve against. Nested code the map does not name keeps the defaults.
- `options` are extra codesign `--options` names (`expires`, `hard`, `host`, `kill`, `library`; `runtime` is always on). `hardenedRuntime` may be `true` or omitted: `false` is a config error because a notarized target cannot ship code without it.

Signing order stays inside-out: all nested code, deepest path first (a nested bundle after everything inside it), then the `sign.prePackageFiles` entries in declared order. A `nestedCode` path is signed once, with its own settings, in that sequence; it must exist, must not be a symlink or leave the bundle, and must not be the bundle's main executable (that is signed with the bundle). Every configuration problem found is reported at once, before anything is signed.

`right-release doctor` validates the matrix (unknown keys, malformed paths, unsupported option names, entitlement files that are missing, outside the config directory or not property lists); `right-release build --dry-run` only loads the config and does not. Whether a `nestedCode` path exists inside the built bundle is checked at sign time, before the first codesign call. The receipt records, per signed item, the entitlements file digest and the `--options` value, and the build refuses a receipt that disagrees with the plan. Steps that carry settings reach `sign-macos.mjs` as a JSON manifest (`--manifest`, written beside the receipt); steps with only defaults keep the positional form.

Worked example, the Pulse bundle (hardened-runtime helpers with no entitlements, a sandboxed Finder Sync extension with its own entitlements, the app with the app entitlements; replaces `@electron/osx-sign` `optionsForFile`):

```js
sign: {
  prePackageFiles: [
    { file: "dist/staging/Pulse.app", entitlements: "release/entitlements.plist", hardenedRuntime: true },
  ],
  nestedCode: {
    "Contents/Helpers/PulseHelper":   { hardenedRuntime: true },
    "Contents/Helpers/pulse-elevate": { hardenedRuntime: true },
    "Contents/Helpers/smartctl":      { hardenedRuntime: true },
    "Contents/PlugIns/PulseFinder.appex": { hardenedRuntime: true, entitlements: "mac/Notch/FinderExtension/PulseFinder.entitlements" },
  },
},
```

Nested code that the map does not name (frameworks, other helpers) is signed first with the defaults; the appex's own `Contents/MacOS` executable is signed before the appex, and the appex signature then carries the extension entitlements. The Finder extension's entitlements never reach the app and the app's Apple Events entitlement never reaches the extension.

### DMG from an appdmg-style spec

`right-release dmg build --app <.app> --out <dmg> --spec <appdmg.json>` (and `targets.mac.dmg.spec` for SwiftPM apps that right-release packs itself) renders an appdmg-style JSON file through the same headless hdiutil pipeline; explicit flags and `targets.mac.dmg` options win over the spec, and the spec's `title` is the volume name (on the command line, `--volname` is required when the spec has no `title`). Relative `background` and `icon` paths resolve against the spec file's directory. Pulse's `release/appdmg.json` works unchanged:

```json
{ "title": "Pulse", "icon-size": 128, "window": { "size": { "width": 640, "height": 420 } },
  "contents": [ { "x": 180, "y": 210, "type": "file", "path": "Pulse.app" },
                { "x": 460, "y": 210, "type": "link", "path": "/Applications" } ] }
```

Supported: `title`, `icon` (`.icns` volume icon), `background` (PNG or TIFF), `icon-size`, `window.size`, `window.position`, `format: "UDZO"`, `filesystem: "HFS+"`, and `contents` limited to the `.app` (`file`, its name must match `--app`) and a `link` to `/Applications`, where `x`/`y` are icon centres. Unsupported fields (`background-color`, `code-sign`, `license`, other formats or filesystems, extra files or links, `position` entries, anything unknown) fail with `DMG_SPEC_UNSUPPORTED` instead of being ignored. `code-sign` is refused because the release pipeline signs the DMG in its own step (receipt `macos-dmg-signing.json`).

## One gate for GitHub uploads

`right-release upload` and `right-release github --release` share one gate (`github-upload-gate.mjs`): both require `--tier patch|update`, the primary checkout with a clean tree, a sealed release built from the current HEAD with matching ownership evidence, and a release config that declares `distribution.provider: "github-releases"` and `distribution.repository` (`--repo` may only repeat it; the `origin` remote is never consulted). The gate makes no network call and also runs on `--dry-run`. Private products publish with `publish-update` and `distribution.provider: "r2-rightapps"`. `github --addon` publishes headless add-ons and takes no tier. Not gated the same way: `github --addon` (no tier), `addon upload` (no provider requirement), and the direct R2 uploaders `upload-large.mjs` and `pub-upload.mjs` (no tier, provider or clean-tree check).

## Known issues

- `smol-toml` is pinned exactly. A consumer that depends on a different `smol-toml` version gets a second copy.
- Apps pin this package at different versions; `docs/ADOPTION.md` lists each pin and marks the stale ones.
- Prepublish runs the test suite, the Windows signer integration test and the registry parity check.
