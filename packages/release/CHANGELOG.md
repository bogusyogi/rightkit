# Changelog

## 0.2.143

- `install-app` bundle swap can be truly atomic. When a `rightkit-swap` helper (rightkit-fs 0.4.0, `renamex_np(RENAME_SWAP)`) is found, the staged bundle is exchanged with the installed one in a single kernel step, so `<dest>/<App>.app` is never absent, even under SIGKILL. The old bundle then sits at the staged path and is moved to the Trash only after the new one re-verifies; on re-verify failure (or SIGINT/SIGTERM) the exchange is reversed. Helper lookup order: `RIGHTKIT_SWAP`, `<package>/bin/darwin/rightkit-swap`, then `PATH`. With no helper, or `ENOTSUP`/`EINVAL` (non-APFS volume), it falls back to the previous two-rename + journal path and emits a `swap-fallback` event. The `replaced` and `done` JSON events record `swap: "atomic"` or `swap: "two-rename"` (`"rename"` for a first install with nothing to exchange). The recovery journal records the mode (`swap`, plus the staged inode for atomic) and `--recover` handles both. The helper is not yet bundled in this package, so installs use the two-rename path unless `RIGHTKIT_SWAP` or `PATH` provides it.

## 0.2.142

- SwiftPM/macOS DMG ordering: with `notarize.appBeforePackage: true` and no `targets.mac.package`, right-release builds the DMG itself (`dmg-build.mjs`) after the `.app` is notarized and stapled, refusing to pack an app that fails `stapler validate`, then Developer ID signs the DMG (receipt `macos-dmg-signing.json`) before the existing DMG notarization. Options via `targets.mac.dmg` (`dmg: false` or a `package` step keeps a custom flow). Preflight now fails when a prePackage hook builds a DMG while `appBeforePackage` is on (heuristic: `dmg`/`hdiutil` in its command or arguments).
- `sign.prePackageFiles` no longer crashes with EISDIR on a directory. `.app` bundles are signed inside-out (nested code without entitlements, then the bundle) and the signing receipt is verified with a bundle tree digest (shared `macos-file-evidence.mjs`); other directories fail with a clear error.

## 0.2.141

- `install-app` probes `xcrun --find stapler` first. On clean macOS hosts without Xcode/CLT it no longer fails: the ticket is accepted when `spctl -a -t exec -vv` reports Notarized Developer ID (and, with `--dmg`, `spctl -a -t open --context context:primary-signature` accepts the DMG), and the `verified` event records `ticket: "spctl-only"`. With stapler present, and with `--dev`, checks are unchanged.

- `install-app` bundle replacement is crash-safe. The new bundle is `ditto`-staged on the destination volume (`.<App>.rightkit-install-<pid>.app`) and verified (codesign --verify --deep --strict, Team ID, bundle id) before anything installed is touched. A JSON journal (`.<App>.rightkit-install.json`, atomic write + fsync) records installed, backup, staged and mount paths and phase. The swap is two `rename(2)` calls (old to `.<App>.rightkit-backup-<pid>.app`, staged to `<App>.app`); the old bundle goes to the Trash only after the new one is re-verified, and a failed re-verify now rolls back to the backup. A kill between the renames can no longer leave the app absent.
- Recovery runs at the start of every `install-app` and as `install-app --recover [--dest <dir>] [--app-name X.app]`: missing app plus backup restores the backup; app plus backup rolls forward if the new bundle verifies, else back; leftover staging is removed and any stale DMG mount recorded in the journal is detached (retrying helper). Idempotent; a journal owned by a live installer is skipped.
- SIGINT/SIGTERM during install roll back an unfinished swap, detach the DMG, and exit 130/143. A ditto failure now happens during staging, so the message reads "previous app untouched".

## 0.2.140

- Ownership: Swift `//` and `/* */` comments in Package.swift no longer declare dependencies.
- `install-app --dev` installs a Developer ID-signed, NOT notarized build from `--dmg`, `--zip` (`ditto -x -k`), or `--app`. It still requires `codesign --verify --deep --strict`, a Team ID (and no team change versus the installed copy), and a bundle-id match; it skips only `spctl` and `stapler`, runs `xattr -dr com.apple.quarantine` on the newly installed bundle only, prints `DEV BUILD (not notarized)`, and adds `dev: true` to every JSON event. Quit-by-signal, Trash move, `ditto`, and `open -g -j` relaunch are unchanged. Without `--dev`, only `--dmg` is accepted.
- SwiftPM/macOS: new opt-in `targets.mac.notarize.appBeforePackage: true` notarizes and staples the `.app` (receipt `macos-app-notarization.json`) right after signing and before the package step builds the DMG, so a copied-out app passes offline `stapler validate`; the DMG is then notarized and stapled as before. `notarize.file` is unchanged (no breaking config change); new helper `notarizeAppBundle` in `macos-notarization.mjs`.
- Every `hdiutil detach` (DMG build, install-app) retries "Resource busy" 5 times with 0.5s to 4s backoff, logging each retry, and uses `-force` only as the last resort (`hdiutil-detach.mjs`).
- Clean-tree gates (`assertCleanSource`, committed-checkout ownership check) ignore right-release's own untracked files in the `.right-release/locks/` directory (every platform), so repos that do not ignore `.right-release/` no longer fail preflight. Nothing else in the vault is exempt.
- `install-app --dmg`: when the app has no stapled ticket (standard Tauri flow staples only the DMG), accept it if `stapler validate` passes on the DMG, `spctl -a -t open --context context:primary-signature` accepts the DMG, and `spctl -a -t exec -vv` reports the mounted app as Notarized Developer ID (may go online). codesign --deep --strict, Team ID continuity and bundle-id checks are unchanged. The JSON `verified` event records `ticket: "app-ticket" | "dmg-ticket"`.
- Docs: SwiftPM builds use `right-release build --platform mac|win`; `release --tier patch --no-upload` is rejected because build is tier-neutral (tier is chosen only at `upload`). README and the `release.mjs` usage text corrected.

## 0.2.139

- GitHub Actions runners without a RightKit runtime use the toolchain's cargo for Cargo metadata on macOS; agent hosts still never fall back. Fixes macOS release-candidate signing ("installed RightKit runtime client is unavailable") since 0.2.136.
- Add project-type-aware preflight (`project-type.mjs`): SwiftPM targets need `swift`, not cargo/rustc; `makeappx`/`signtool` are required only for msix targets. Add the `windows-msix-signed-v1` contract and `targets.win.msix` schema (`msix-config.mjs`, `msix-target.mjs`, `msix-plan.mjs`): explicit virtualization settings, runtime-DLL inventory verification, signer/Publisher match, msix seal record, and `updater.kind: "appinstaller"` (`msix-appinstaller.mjs`) with sealing and `publish-update` support. Tauri updater remains the default.

## 0.2.138

- Hosts without a protected ownership trust location (Linux CI admission runners) load trust as pending and absent instead of failing, so `ownership --advisory-when-pending` reports diagnostics and exits 0 there.
- Ships the `swift-package` command below as dormant code; it is not yet qualified on native hosts.

## Unreleased (swift-package qualification)

- Add `right-release swift-package <windows-stage|msix|mac-app> --config <swift-package.json>` for SwiftPM apps. Windows: stage exe plus app-local DLLs with a PE-import transitive closure check (fails closed on anything not staged, system, or provided by a declared package dependency), generate `AppxManifest.xml`, sign PE files, `makeappx pack`, sign and verify the `.msix` through `sign-windows`. macOS: assemble a `.app` (Info.plist, Frameworks rpath, resources), sign nested code inside-out through `sign-macos` (entitlements on the bundle only), and optionally notarize and staple through `notarize-macos`. Build-only; never uploads. Needs native Windows/Mac runs to qualify.

## 0.2.137

- `right-release ownership --advisory-when-pending` exits 0 only while approval-key enrollment is pending (findings still print as diagnostics; JSON keeps `ok: false`, `enrollmentPending: true`). Used by right-git's `ci` lane; release lanes never pass it.

## 0.2.136 — 2026-10-09

- Add `right-release install-app --dmg <path.dmg>`: sanctioned, non-intrusive macOS install of a signed DMG. Verifies signature, Gatekeeper, notarization staple, bundle id and Team ID before touching the installed app; quits via signals (no AppleEvents); replaces via Trash with rollback; relaunches hidden and in the background only. Windows exits "not implemented on win32".
