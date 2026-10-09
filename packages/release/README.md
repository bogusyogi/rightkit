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

## Local app install (macOS)

Agents must not run `osascript`, `open`, `screencapture`, or `notifyutil -p` directly. Install a signed build through:

```sh
right-release install-app --dmg <path.dmg> [--app-name Pulse.app] [--bundle-id dev.orthic.pulse] [--dest /Applications] [--relaunch|--no-relaunch] [--json]
```

It mounts the image read-only and hidden (`-nobrowse`), verifies the app (`codesign --verify --deep --strict`, `spctl --assess`, `stapler validate`, bundle id, and Team ID against the installed copy; a Team ID change is refused) before touching the installed copy, quits the running instance with SIGTERM (15 s, then SIGKILL), moves the old bundle to `~/.Trash/<App> <timestamp>.app`, copies the new one with `ditto` (restoring the old bundle on failure), re-verifies, and relaunches only if it was running or `--relaunch` is given, using `open -g -j -b` (background, hidden, never activated). `--json` emits one lifecycle event per line (`start`, `verified`, `quit`, `replaced`, `relaunched`, `done`/`failed`). Win32 exits with "not implemented on win32". Exit codes: 0 ok, 1 failed, 2 usage, 3 unsupported platform.

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

macOS Swift apps use `right-release swift-package mac-app` (Info.plist from config, resources, frameworks, entitlements on the bundle only; nested code signed inside-out before the app).

## Known issues

- `smol-toml` is pinned exactly. A consumer that adds another `smol-toml` gets a second copy (npm audit 2026-10-08, release finding).
- Eleven apps pin this package at different versions, from 0.2.109 to 0.2.135 (npm audit 2026-10-08).
- Prepublish runs the test suite, the Windows signer integration test and the registry parity check.
