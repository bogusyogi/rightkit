# Changelog

## Unreleased

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
