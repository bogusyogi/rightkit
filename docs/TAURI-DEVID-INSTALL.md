# Tauri Developer ID dev install (`install.kind: "tauri-devid"`)

One fixed install path, one stable Developer ID, one `.previous` rollback copy. macOS keys privacy grants (TCC)
and Keychain code requirements to the path and the code identity, so a dev build that keeps both never loses them.
Implemented in `packages/release/install-tauri-devid.mjs`; tracked in bogusyogi/rightkit#345.

## Config

`development.targets.mac.install` in the app's release config:

```json
{
  "kind": "tauri-devid",
  "productName": "Demo",
  "targetTriple": "aarch64-apple-darwin",
  "manifestPath": "src-tauri/Cargo.toml",
  "destination": "/Applications/Demo.app",
  "bundleId": "com.example.demo",
  "verifyBinaries": ["Contents/MacOS/demo", "Contents/MacOS/demo-engine"]
}
```

| Field | Required | Meaning |
| --- | --- | --- |
| `productName`, `targetTriple`, `manifestPath`, `bundleDirectory` | yes / yes / no / no | Same as `kind: "tauri"`: locate the built `.app`. |
| `destination` | yes | The one absolute install path, ending `.app`. Never per-version or per-pid. |
| `bundleId` | no | Passed to the installer to refuse a bundle-id change; needed for `resetPrivacy`; the `<bundleId>` of `identifierTemplate`. Must equal the app's `CFBundleIdentifier`. |
| `verifyBinaries` | no | Bundle-relative executables whose SHA-256 must equal the signed staging copy after install. |
| `identifiers` | no | `{ "<path or glob>": "<identifier>" }`: `codesign --identifier` per binary. See below. |
| `identifierTemplate` | no | Default identifier for the remaining plain Mach-O files, for example `"<bundleId>.runtime.<relpath>"`. |
| `entitlements` | no | `{ "<path or glob>": "<plist path>" }`: entitlements per binary. |
| `timestamp` | no | `true` adds `--timestamp` (secure timestamp) to every dev signing call. Default: none. |
| `assert` | no | `[{ "path", "identifier"?, "teamIdentifier"?, "authority"? }]`: identity assertions via `codesign -dvv`. |
| `assertTeamSignedNested` | no | `true`: every Mach-O in the bundle (dylibs included) must carry the app's TeamIdentifier. |
| `signing` | forbidden | Ad hoc signing resets grants every build; refused. |

Entitlements and provisioning profile come from `targets.mac.sign` and the Tauri config, exactly as in
`development-signing.mjs`. Identity is Developer ID Application only (no Apple Development fallback).

### Per-binary identifiers

A designated requirement names the code's signing identifier (`identifier "…" and anchor apple generic and … leaf[subject.OU] = "TEAM"`).
`codesign` derives the identifier from the file name unless told otherwise, so an app that pins Keychain ACLs or privacy grants to a
fixed identifier (an engine that reads credentials) must declare it.

- Paths are relative to the `.app`, `/` separated (`Contents/MacOS/demo-engine`); `.` is the bundle itself.
- `*` and `?` stay inside one path segment, `**` crosses segments. An exact key beats a glob; among globs the first declared wins.
- `identifierTemplate` tokens: `<bundleId>`, `<relpath>` (bundle-relative path) and `<basename>`; runs of characters outside `[A-Za-z0-9.-]` become `.`. It applies only to plain Mach-O files that no `identifiers` key names, never to bundles (`.app`, `.framework`, `.xpc`, …) and never to the app itself.
- The app keeps the identifier from its `Info.plist` unless `identifiers["."]` is set. `"."` applies to dev installs only; release signs the app with its `Info.plist` identifier, so keep `"."` equal to `CFBundleIdentifier`.
- `sign.identifiers` / `sign.identifierTemplate` in `targets.mac.sign` apply to dev as well. Precedence across both lists: an exact path beats any glob, then a glob beats the template; for the same kind of key, `install` wins. One declaration in `sign` therefore one declaration in `sign` keeps dev and release identifiers identical.
- Identifiers must match `[A-Za-z0-9][A-Za-z0-9.-]*` (no `_`), the same rule release signing and the signing agent apply.

### Per-path entitlements

`install.entitlements` (and an object-valued `targets.mac.sign.entitlements`; a string there is still the app-level file) gives a binary its
own entitlements file. It beats the `<main>-<suffix>.plist` variant matching, which still applies to every path the map does not name.
Without a map nothing changes. In release, per-path `sign.entitlements` applies to nested code the same way; with nothing declared nested code
is signed without entitlements, as before.

### Timestamp

Dev signing does not request a secure timestamp by default: it needs Apple's timestamp server (slow, and fails offline) and it does not
change the designated requirement. Release signing always timestamps (notarization requires it). ScrapeRight and GenRight used one script
for both, so their old dev signing passed `--timestamp`; set `"timestamp": true` to keep that, or leave it off for faster offline dev installs.

### Identity assertions

After signing, `install.assert` is checked on the signed staging copy (before anything is installed) and again on the installed copy (a failure
restores `.previous`, or removes a first install, and throws `TAURI_DEVID_VERIFY_FAILED`). `codesign -dvv` is read from stdout and stderr (codesign
prints to stderr). `path` may be a glob; a glob that matches no file fails, so a typo cannot pass. `authority` must be one of the `Authority=` lines.
`assertTeamSignedNested: true` additionally inspects every Mach-O in the bundle, including bundled dylibs such as
`Contents/Frameworks/libonnxruntime*.dylib` or `Contents/Resources/runtime/*.dylib`; one that is ad hoc or signed by another team fails. Errors use
`DEV_MAC_SIGNING_ASSERT_FAILED` (staging) and `TAURI_DEVID_VERIFY_FAILED` (installed).

## What runs

1. Copy the built `.app` to a private staging copy named like the destination.
2. `signMacosDevelopmentBundle`: nested code first (deepest first), then the app, with the stable Developer ID; strict deep verify; refuses an ad hoc or cdhash-bound designated requirement; runs the declared identity assertions.
3. `assertStableDesignatedRequirement` against the installed copy; a mismatch stops with `DEV_MAC_SIGNING_REQUIREMENT_CHANGED` before anything changes.
4. `installApp` in dev mode: verify, quit the running app, atomic swap, old copy kept as `<destination>.previous`.
5. Verify the installed copy: `codesign --verify --strict --deep`, designated requirement equals the signed one, declared identity assertions, `verifyBinaries` digests match. Failure restores `.previous` (or removes a first install) and throws `TAURI_DEVID_VERIFY_FAILED` with `rolledBack`.

## Reset

`resetTauriDevId({ appRoot, config, mode })`: `restore` (default) makes `.previous` the install again;
`remove` deletes the install and `.previous` (add `resetPrivacy: true` to run `tccutil reset All <bundleId>`). Quit the app first.

## Injection

`installTauriDevId({ appRoot, config, built, deps, env })`. `deps` may replace `run`, `sign`, `assertRequirement`, `assertIdentities`,
`installApp`, `exists`, `makeTemp`, `copyApp`, `rename`, `remove`, `readFile`, `log`, plus `listFiles(bundle)` (bundle-relative file paths) and
`isMachO(file)` for the identity assertions. Tests use fakes only and `env: {}`.

## Example: ScrapeRight

Reproduces `scripts/install-macos-development.mjs` + `sign-macos-bundle.mjs`: app `app.scraperight`, engine `app.scraperight.engine`
(`scraperight-engine*`), every other Mach-O `app.scraperight.runtime.<relpath>`, `--timestamp`, `entitlements.engine.plist` on the engine and
`yt-dlp`, and the post-sign identity checks. The bundle id is the Tauri `identifier` (`app.scraperight`), not a reverse-DNS guess: the designated
requirement changes if it differs from what the old script signed.

```json
{
  "development": { "targets": { "mac": { "install": {
    "kind": "tauri-devid",
    "productName": "ScrapeRight",
    "targetTriple": "aarch64-apple-darwin",
    "manifestPath": "src-tauri/Cargo.toml",
    "destination": "/Applications/ScrapeRight.app",
    "bundleId": "app.scraperight",
    "verifyBinaries": ["Contents/MacOS/scraperight", "Contents/MacOS/scraperight-engine"],
    "timestamp": true,
    "identifiers": {
      ".": "app.scraperight",
      "Contents/MacOS/scraperight-engine*": "app.scraperight.engine"
    },
    "identifierTemplate": "<bundleId>.runtime.<relpath>",
    "entitlements": {
      "Contents/MacOS/scraperight-engine*": "src-tauri/entitlements.engine.plist",
      "Contents/Resources/runtime/yt-dlp": "src-tauri/entitlements.engine.plist"
    },
    "assert": [
      { "path": ".", "identifier": "app.scraperight", "teamIdentifier": "6KLGD3LLKF", "authority": "Developer ID Application: Adrian D'souza (6KLGD3LLKF)" },
      { "path": "Contents/MacOS/scraperight-engine", "identifier": "app.scraperight.engine", "teamIdentifier": "6KLGD3LLKF", "authority": "Developer ID Application: Adrian D'souza (6KLGD3LLKF)" }
    ],
    "assertTeamSignedNested": true
  } } } },
  "targets": { "mac": { "sign": {
    "entitlements": "src-tauri/entitlements.plist",
    "identifiers": { ".": "app.scraperight", "Contents/MacOS/scraperight-engine*": "app.scraperight.engine" },
    "identifierTemplate": "<bundleId>.runtime.<relpath>"
  } } }
}
```

The `sign.identifiers` / `sign.identifierTemplate` copy keeps release nested code on the same identifiers (the install keys alone are enough for dev).
`app.scraperight.engine` is what the engine's Keychain access is bound to: do not rename it.

## Example: GenRight

Reproduces `scripts/sign-macos-bundle.mjs`: app `app.orthiclabs.genright` (the Tauri `identifier`), engine `app.orthiclabs.genright.engine`
(`genright-engine*`, so the qts worker and the engine keep their old identifier exactly as the old script did), every other Mach-O
`app.orthiclabs.genright.runtime.<basename>` (GenRight used the file name, not the path, so `libonnxruntime.dylib` stays
`app.orthiclabs.genright.runtime.libonnxruntime.dylib`), `--timestamp`, the bundled ONNX Runtime re-signed with the team.

```json
{
  "kind": "tauri-devid",
  "productName": "GenRight",
  "targetTriple": "aarch64-apple-darwin",
  "manifestPath": "src-tauri/Cargo.toml",
  "destination": "/Applications/GenRight.app",
  "bundleId": "app.orthiclabs.genright",
  "verifyBinaries": ["Contents/MacOS/genright", "Contents/MacOS/genright-engine"],
  "timestamp": true,
  "identifiers": {
    ".": "app.orthiclabs.genright",
    "Contents/MacOS/genright-engine*": "app.orthiclabs.genright.engine"
  },
  "identifierTemplate": "<bundleId>.runtime.<basename>",
  "assert": [
    { "path": ".", "identifier": "app.orthiclabs.genright", "teamIdentifier": "6KLGD3LLKF" },
    { "path": "Contents/MacOS/genright-engine", "identifier": "app.orthiclabs.genright.engine", "teamIdentifier": "6KLGD3LLKF" },
    { "path": "Contents/Resources/runtime/libonnxruntime*.dylib", "teamIdentifier": "6KLGD3LLKF" }
  ],
  "assertTeamSignedNested": true
}
```

Stability checks before the first install: the `identifier` in `tauri.conf.json` must equal `bundleId`; the installed copy's engine must report
`Identifier=app.orthiclabs.genright.engine`; and the installed copy's designated requirement is compared by `assertStableDesignatedRequirement`, so a
mistyped identifier stops the install with `DEV_MAC_SIGNING_REQUIREMENT_CHANGED` instead of resetting Keychain grants. The ONNX Runtime `assert` entry fails if no dylib matches its glob (the old script checked `Contents/Resources/runtime`; use `Contents/Frameworks/libonnxruntime*.dylib` for an app that bundles it there).
GenRight's old script gave helpers no entitlements; rightkit's dev signing gives unsigned helper executables the app's hardened-runtime relaxations
(`com.apple.security.cs.*`, audio input) when the app entitlements declare them. That does not change any designated requirement.

## Migrating the apps

Replace each app's `install-macos-development.mjs` / `sign-macos-bundle.mjs` with the descriptor above and `right-release dev --platform mac`.

| App | destination | verifyBinaries | Notes |
| --- | --- | --- | --- |
| scraperight | `/Applications/ScrapeRight.app` | `Contents/MacOS/scraperight`, `scraperight-engine` | See the example above, then delete both scripts. |
| genright | `/Applications/GenRight.app` | its main and engine binaries | See the example above; it has no dev install script today, only `sign-macos-bundle.mjs`. |
| cutright | `apps/cutright` Tauri app path | its main and sidecar binaries | `sign-macos-bundle.mjs` is under `apps/cutright/scripts`; entitlement variants go beside the main entitlements file (`entitlements-<suffix>.plist`) or use `entitlements`. |
| coderight | its current fixed path | its main binary | Delete both scripts. |
| HeardRight | `install-macos-app.mjs`, `prepare-macos-dev-app.mjs` | its main binary | App preparation (embedding its provisioning profile) must run as a build step before install; signing reuses an embedded profile. `reset-macos-dev-install.mjs` becomes `resetTauriDevId` (the app's reset command calls it). |

Before deleting an app's script, confirm its destination, bundle id, identifiers and entitlement files match what the script uses today, or its first install changes the designated requirement and resets grants once.
