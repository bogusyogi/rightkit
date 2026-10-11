# @rightkit/release

Portable release CLI and SDK for Right Suite products: builds, signs, and publishes to public GitHub Releases or to private R2 with RightApps distribution. It also runs the ownership catalog and lockfile checks and the release gates.

| Item | Value |
|---|---|
| Where it runs | Build and release tooling. Native signing runs on the build host. |
| Rust pair | None. The Rust build broker files in this package (`managed-cargo.mjs`, `managed-roots.mjs`) are moving to the separate RustRight product. Do not extend them here. |
| Version | see INDEX.md |
| License | MIT OR Apache-2.0 |

Signing and publication rules are in the workspace file `docs/rules/release-signing.md`. Public releases use RightKit CI. Private builds run on the native host.

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

It mounts the image read-only and hidden (`-nobrowse`), verifies the app (`codesign --verify --deep --strict`, `spctl --assess`, `stapler validate`, bundle id, and Team ID against the installed copy; a Team ID change is refused) before touching the installed copy, quits the running instance with SIGTERM (15 s, then SIGKILL), moves the old bundle to `~/.Trash/<App> <timestamp>.app`, copies the new one with `ditto` (restoring the old bundle on failure), re-verifies, and relaunches only if it was running or `--relaunch` is given, using `open -g -j -b` (background, hidden, never activated). `--json` emits one lifecycle event per line (`start`, `verified`, `quit`, `replaced`, `relaunched`, `done`/`failed`). Win32 exits with "not implemented on win32". Exit codes: 0 ok, 1 failed, 2 usage, 3 unsupported platform.

Development builds (from `@rightkit/git` `devArtifact`, Developer ID-signed but not notarized) install with `--dev` and exactly one of `--dmg`, `--zip <path.zip>` (extracted with `ditto -x -k`), or `--app <path.app>`: `right-release install-app --dev --zip dev-mac.zip`. `--dev` still requires `codesign --verify --deep --strict`, a Team ID, no Team ID change, and a bundle-id match; it skips only `spctl` and `stapler`, removes `com.apple.quarantine` from the new bundle only, prints `DEV BUILD (not notarized)`, and sets `dev: true` on every JSON event.

## Runtime budget gate (crates)

`publish cargo` runs the `runtime budget` step (`runtime-budgets.mjs`) right after the secret scan. Full contract, schema and refusal table: `docs/RUNTIME-BUDGETS.md`.

What the gate enforces, exactly:
- **Three classes, fail-closed.** always-on (measured budget at publish), `continuous = "held-resource"` with a `resources` table (structural check at publish, installed-app measurement at adoption via `appRuntimeBudgetAdoptionCheck`, wired into `collectPreflight`), and `continuous = false` (bounded per call).
- **Membership is fail-closed.** `Cargo.toml` `[package].name` must equal `--crate`. Every crate needs `runtime-budget.json` or `[package.metadata.rightkit] continuous = false` with a `continuous_reason`; neither is a refusal. The 11 crates in `RUNTIME_BUDGET_CRATES` cannot declare `continuous = false`. A `continuous = false` crate is refused when its non-test `src` matches `thread-spawn`, `task-spawn`, `sleep`, `timer`, `listener` or `watcher` and the reason does not contain that id.
- **A continuous crate needs a measured budget.** Declared ceilings and `requiredScale`; a status pointer to a `pub` item exposing resident memory; per-host measurements of the published version, each backed by committed raw bench output (`runtime-budgets.mjs record`) whose sha256 and `RESULT budget` line the gate re-checks; every observed value within its ceiling and non-zero resident memory; a fresh, in-ceiling, at-scale measurement on each required host (macOS and Windows, or macOS alone only when the release evidence records `hostPolicy: "darwin-only"` with a reason, which the budget file may not contradict); a README section equal to the one generated from the JSON.
- **Freshness** covers the crate's `src`, `benches`, `Cargo.toml`, `build.rs`, the same paths of every workspace `path` dependency, new untracked files there, and the crate's `Cargo.lock` dependency closure, against the measured commit (which must be in the checkout and an ancestor of HEAD).

What it does not do: run the bench, prove the bench ran, detect `[profile]`, `[patch]` or toolchain changes, or find continuous work outside the scanned patterns. `runtimeBudgetAdoptionCheck` adds the need for a fresh `installed` measurement naming the app; crates with `publish = false` never reach `publish cargo`, so their adoption must call it.

A real `publish cargo --crate <name>` refuses with `RUNTIME_BUDGET_MISSING` listing every problem; `--dry-run` prints `[FAIL] runtime-budget: ...` and continues.

## Entry points

Bin: `right-release` (`cli/right-release.mjs`). The package has no `exports` map. The CLI is the supported surface.

## Dependencies

| Dependency | Range | Note |
|---|---|---|
| `smol-toml` | `1.7.0` (exact) | Used to parse lock and manifest files |

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

Never cancel an MSIX deployment from app or updater code. Cancelling `AddPackageAsync` while it is in `RegistrationChanged` leaves the package `Modified, NeedsRemediation` (0x80073CF8, AppX event 751); the app then cannot activate, so the OnLaunch feed check never runs and cannot self-heal. Measured recovery (SwiftRight SliceApp R5 W08 tuple 08, 2026-10-10, one run per method, user data intact in every recovering case): Settings › Apps › Advanced options › Repair recovers a feed install (with or without a newer version on the feed) and a path install with no feed; `Add-AppxPackage -Path <same-version .msix>` recovers; `Add-AppxPackage -AppInstallerFile <feed uri>` recovers with or without a newer version on the feed. `Reset-AppxPackage`, `-RegisterByFamilyName`, the platform's own `RegisterByPackageFullName` (runs on every launch attempt, reports success, package stays NeedsRemediation) and relaunching do not. Not measured: Repair with the install source gone.

macOS Swift apps use `right-release swift-package mac-app` (Info.plist from config, resources, frameworks, entitlements on the bundle only; nested code signed inside-out before the app).

Build a SwiftPM app with `right-release build --platform mac|win` (add `--config <path>` when the config is not `right-release.config.mjs`). Build is tier-neutral and never uploads, so it rejects `--tier` and takes no `--upload`/`--no-upload`; the old `release --tier patch --no-upload` form no longer works. Pick the tier only at upload: `right-release upload --release <sealed-id> --platform mac|win --tier patch|update`.

Set `targets.mac.notarize.appBeforePackage: true` (with `notarize.app` naming the `.app`) to notarize and staple the app itself right after signing and before the package step copies it into the DMG, so an app copied out of the DMG passes `stapler validate` offline. The DMG named by `notarize.file` is still notarized and stapled afterward; the app ticket receipt is `macos-app-notarization.json`. Without the flag, behavior is unchanged.

For `projectType: "swiftpm"` on mac with `appBeforePackage: true` and no `targets.mac.package`, right-release builds the DMG itself (same `dmg-build.mjs` as Tauri) at the package step, after the `.app` is signed, notarized and stapled: it runs `xcrun stapler validate` on the app (refusing to pack an unstapled one), builds the DMG at `notarize.file` (must end `.dmg`), Developer ID signs it (receipt `macos-dmg-signing.json`), then notarizes and staples the DMG as before. Options go in `targets.mac.dmg`: `{ volname, background, iconSize, appPosition, applicationsPosition, window, volumeIcon, out }` (`volname` defaults to the app name). A prePackage hook must no longer build the DMG: preflight fails when its command or arguments mention `dmg` or `hdiutil` (heuristic), because that DMG would carry the unstapled app. Keep a custom DMG flow with `targets.mac.dmg: false` or a `targets.mac.package` step (both run after the staple and skip the automatic build).

`sign.prePackageFiles` entries may be `.app` bundles (directories). Nested code (Mach-O files, frameworks, XPC services, plug-ins) is signed first, inside-out and without entitlements, then the bundle is signed; the receipt records a tree digest of the bundle. Any other directory is rejected with a clear error.

### Per-file signing matrix (macOS)

By default every code item is signed with the hardened runtime and no entitlements, so a bundle that needs entitlements on some of its code (an app, a sandboxed extension) declares them per file. Both forms below are optional and backwards compatible; a config of plain path strings signs exactly as before.

- `sign.prePackageFiles[]` entries are a path string or `{ file, entitlements?, hardenedRuntime?, options? }`. `entitlements` is a plist path relative to the release config directory. For a `.app` entry the settings apply to the bundle signature.
- `sign.nestedCode` is a map from a path inside the bundle (`Contents/...`, forward slashes) to `{ entitlements?, hardenedRuntime?, options? }`. It needs exactly one `.app` entry in `sign.prePackageFiles` to resolve against. Nested code the map does not name keeps the defaults.
- `options` are extra codesign `--options` names (`expires`, `hard`, `host`, `kill`, `library`; `runtime` is always on). `hardenedRuntime` may be `true` or omitted: `false` is a config error because a notarized target cannot ship code without it.

Signing order stays inside-out: all nested code, deepest path first (a nested bundle after everything inside it), then the `sign.prePackageFiles` entries in declared order. A `nestedCode` path is signed once, with its own settings, in that sequence; it must exist, must not be a symlink or leave the bundle, and must not be the bundle's main executable (that is signed with the bundle). Every problem found is reported at once, before anything is signed.

`right-release doctor` and `--dry-run` validate the matrix: unknown keys, malformed paths, unsupported option names, entitlement files that are missing, outside the config directory or not property lists. Whether a `nestedCode` path exists inside the built bundle is checked at sign time, before the first codesign call. The receipt records, per signed item, the entitlements file digest and the `--options` value, and the build refuses a receipt that disagrees with the plan. Steps that carry settings reach `sign-macos.mjs` as a JSON manifest (`--manifest`, written beside the receipt); steps with only defaults keep the positional form.

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

`right-release dmg build --app <.app> --out <dmg> --spec <appdmg.json>` (and `targets.mac.dmg.spec` for SwiftPM apps that right-release packs itself) renders an appdmg-style JSON file through the same headless hdiutil pipeline; explicit flags and `targets.mac.dmg` options win over the spec, and the spec's `title` is the volume name. Relative `background` and `icon` paths resolve against the spec file's directory. Pulse's `release/appdmg.json` works unchanged:

```json
{ "title": "Pulse", "icon-size": 128, "window": { "size": { "width": 640, "height": 420 } },
  "contents": [ { "x": 180, "y": 210, "type": "file", "path": "Pulse.app" },
                { "x": 460, "y": 210, "type": "link", "path": "/Applications" } ] }
```

Supported: `title`, `icon` (`.icns` volume icon), `background` (PNG or TIFF), `icon-size`, `window.size`, `window.position`, `format: "UDZO"`, `filesystem: "HFS+"`, and `contents` limited to the `.app` (`file`, its name must match `--app`) and a `link` to `/Applications`, where `x`/`y` are icon centres. Unsupported fields (`background-color`, `code-sign`, `license`, other formats or filesystems, extra files or links, `position` entries, anything unknown) fail with `DMG_SPEC_UNSUPPORTED` instead of being ignored. `code-sign` is refused because the release pipeline signs the DMG in its own step (receipt `macos-dmg-signing.json`).

## Known issues

- `smol-toml` is pinned exactly. A consumer that adds another `smol-toml` gets a second copy (npm audit 2026-10-08, release finding).
- Eleven apps pin this package at different versions, from 0.2.109 to 0.2.135 (npm audit 2026-10-08).
- Prepublish runs the test suite, the Windows signer integration test and the registry parity check.
