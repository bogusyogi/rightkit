# Tauri Developer ID dev install (`install.kind: "tauri-devid"`)

One fixed install path, one stable Developer ID, one `.previous` rollback copy. macOS keys privacy grants (TCC)
and Keychain code requirements to the path and the code identity, so a dev build that keeps both never loses them.
Implemented in `packages/release/install-tauri-devid.mjs`; tracked in bogusyogi/rightkit#345.

## Config

`development.targets.mac.install` in the app's release config:

```json
{
  "kind": "tauri-devid",
  "productName": "ScrapeRight",
  "targetTriple": "aarch64-apple-darwin",
  "manifestPath": "src-tauri/Cargo.toml",
  "destination": "/Applications/ScrapeRight.app",
  "bundleId": "com.orthiclabs.scraperight",
  "verifyBinaries": ["Contents/MacOS/scraperight", "Contents/MacOS/scraperight-engine"]
}
```

| Field | Required | Meaning |
| --- | --- | --- |
| `productName`, `targetTriple`, `manifestPath`, `bundleDirectory` | yes / yes / no / no | Same as `kind: "tauri"`: locate the built `.app`. |
| `destination` | yes | The one absolute install path, ending `.app`. Never per-version or per-pid. |
| `bundleId` | no | Passed to the installer to refuse a bundle-id change; needed for `resetPrivacy`. |
| `verifyBinaries` | no | Bundle-relative executables whose SHA-256 must equal the signed staging copy after install. |
| `signing` | forbidden | Ad hoc signing resets grants every build; refused. |

Entitlements and provisioning profile come from `targets.mac.sign` and the Tauri config, exactly as in
`development-signing.mjs`. Identity is Developer ID Application only (no Apple Development fallback).

## What runs

1. Copy the built `.app` to a private staging copy named like the destination.
2. `signMacosDevelopmentBundle`: nested code first (deepest first), then the app, with the stable Developer ID; strict deep verify; refuses an ad hoc or cdhash-bound designated requirement.
3. `assertStableDesignatedRequirement` against the installed copy; a mismatch stops with `DEV_MAC_SIGNING_REQUIREMENT_CHANGED` before anything changes.
4. `installApp` in dev mode: verify, quit the running app, atomic swap, old copy kept as `<destination>.previous`.
5. Verify the installed copy: `codesign --verify --strict --deep`, designated requirement equals the signed one, `verifyBinaries` digests match. Failure restores `.previous` (or removes a first install) and throws `TAURI_DEVID_VERIFY_FAILED` with `rolledBack`.

## Reset

`resetTauriDevId({ appRoot, config, mode })`: `restore` (default) makes `.previous` the install again;
`remove` deletes the install and `.previous` (add `resetPrivacy: true` to run `tccutil reset All <bundleId>`). Quit the app first.

## Injection

`installTauriDevId({ appRoot, config, built, deps, env })`. `deps` may replace `run`, `sign`, `assertRequirement`,
`installApp`, `exists`, `makeTemp`, `copyApp`, `rename`, `remove`, `readFile`, `log`. Tests use fakes only and `env: {}`.

## Migrating the apps

Replace each app's `install-macos-development.mjs` / `sign-macos-bundle.mjs` with the descriptor above and `right-release dev --platform mac`.

| App | destination | verifyBinaries | Notes |
| --- | --- | --- | --- |
| scraperight | `/Applications/ScrapeRight.app` | `Contents/MacOS/scraperight`, `scraperight-engine` | Delete both scripts. |
| genright | its current fixed path | its main and engine binaries | Delete both scripts. |
| cutright | `apps/cutright` Tauri app path | its main and sidecar binaries | `sign-macos-bundle.mjs` is under `apps/cutright/scripts`; entitlement variants go beside the main entitlements file (`entitlements-<suffix>.plist`). |
| coderight | its current fixed path | its main binary | Delete both scripts. |
| HeardRight | `install-macos-app.mjs`, `prepare-macos-dev-app.mjs` | its main binary | App preparation (embedding its provisioning profile) must run as a build step before install; signing reuses an embedded profile. `reset-macos-dev-install.mjs` becomes `resetTauriDevId` (the app's reset command calls it). |

Before deleting an app's script, confirm its destination, bundle id and entitlement files match what the script uses today, or its first install changes the designated requirement and resets grants once.
