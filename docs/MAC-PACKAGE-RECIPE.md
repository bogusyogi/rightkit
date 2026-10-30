# macOS package recipe

Tracking: bogusyogi/rightkit#345 (every product builds through one RightKit entrypoint).

There are two different schemas in this repository. They share a name and nothing else, so keep them apart:

| | Release target `targets.mac.macPackage` | Standalone `runMacPackageRecipe` |
| --- | --- | --- |
| Run by | `right-release release --platform mac` | a direct caller importing `mac-package-recipe.mjs` |
| Lives in | `right-release.config.mjs` | the caller's own config object |
| Keys `dmg` / `notarize` / `updater` / `nested` / `entitlements` | **not** `macPackage` keys (see "Where each concern lives") | `macPackage.dmg`, `.notarize`, `.updater`, `.nested`, `.entitlements` |

Everything from "Release target" down to "Standalone runMacPackageRecipe" is about the release target. If you are writing
`right-release.config.mjs`, stop reading at that heading.

## Release target: `targets.mac.macPackage`

`targets.mac.macPackage` is expanded by `packages/release/package-recipe-targets.mjs` into the hook shape release.mjs already
gates (`prePackage`, `sign`, `notarize`, `dmg`), so every existing check runs unchanged: QA-control and release-switch scans,
Developer ID signing of the .app and its nested code, app notarization and stapling before packing, the signed DMG and its
notarization.

```js
mac: {
  signed: true, signingContract: "macos-developer-id-notarized-portable-v1",
  macPackage: {
    universal: true,                       // default; false = this host's arch (process.arch)
    triple: "aarch64-apple-darwin",        // optional explicit arch, beats `universal`
    tauriCwd: ".", features: [],           // optional
    profile: "release", tauriConfig: "src-tauri/tauri.release.json",   // optional
    prepare: [], postBuild: [],            // optional argv commands
    cleanup: { staleApps: true, staleDmgVolumes: true, lsclean: true },// optional built-in pre-build cleanup
    cargoTargetRoot: true,                 // optional, see "Managed Cargo target roots"
    stageSidecars: true,                   // optional, see "Universal sidecars (externalBin)"
    nativeHelpers: [/* ... */],            // optional, see "Native Swift helpers"
  },
  notarize: { app: "src-tauri/target/universal-apple-darwin/release/bundle/macos/App.app", file: "dist/App_universal.dmg", profile: "apple-dev-notary" },
  dmg: { volname: "App", background: "assets/dmg.png" },          // DMG settings live HERE
  sign: { nestedCode: {}, identifiers: {}, entitlements: {} },     // prePackageFiles defaults to [notarize.app]
  externalBin: { /* sidecars, signed automatically */ },
  postSign: { cmd: "right-release", args: ["create-mac-updater", /* ... */] },  // the updater lives HERE
}
```

Unknown `macPackage` keys are refused (`universal, triple, tauriCwd, features, profile, tauriConfig, prepare, postBuild,
cleanup, cargoTargetRoot, updater, stageSidecars, nativeHelpers`). `prePackage` and `package` must be absent, and `dmg: false` is refused: the target always
builds a DMG.

### Where each concern lives

| Concern | Where | Notes |
| --- | --- | --- |
| Nested code to sign | `sign.nestedCode` | optional; `.framework`, `.xpc`, `.dylib` and Mach-O files under `Contents` are discovered automatically |
| codesign `--identifier` | `sign.identifiers` (path or glob map) and `sign.identifierTemplate` | per-path, bundle-relative; the a59df383 behaviour |
| Per-path entitlements | `sign.entitlements` as an object of bundle path/glob to plist | nested code stays unentitled unless a path is listed; a string value is the app's own entitlements |
| `externalBin` sidecars | `externalBin` | nested-code signing is automatic: release.mjs merges `externalBinNestedCode` (each `<bundleDir>/<name>` with hardened runtime) into the signing config, then your own `sign.nestedCode` entries win. Nothing to declare |
| DMG volume name, background, layout | `targets.mac.dmg` (`volname`, `background`, or `spec: "assets/dmg.json"`) | the standalone `macPackage.dmg` block does not apply here |
| Updater tarball and signature | `postSign`: `create-mac-updater` (a single command or an array of commands) | the recipe does not create an updater |
| Notarization | `notarize` (`app`, `file`, `profile`, `staple`) | `appBeforePackage` is set for you |
| Raw .app signing set | `sign.prePackageFiles` | defaults to `[notarize.app]`; if set it must contain `notarize.app` |

### Architecture

`universal: true` (default) builds `--target universal-apple-darwin`, and Tauri lipos both slices itself. `universal: false` builds
this host's arch (`process.arch`: `aarch64-apple-darwin` on arm64, else `x86_64-apple-darwin`). `triple` names an arch
explicitly (`aarch64-apple-darwin` or `x86_64-apple-darwin`) and overrides `universal`; `notarize.app` must then sit under that
triple's directory (a path under a different triple is refused).

### Signing, `--no-sign` and the DMG

The Tauri build runs with `--no-sign` and `signingIdentity: null`: Tauri never signs. release.mjs signs the .app and nested code
after the build, so that its QA-control and release-switch scans run on unsigned bytes and the signing receipt is one set. Do not
remove this.

`notarize.file` is the DMG right-release builds from the stapled app. It is an app-relative path and may point anywhere inside
the app (for example `dist/App_universal.dmg`). release.mjs also finds the file by basename in `RIGHT_RELEASE_ARTIFACT_DIR` when
storage governance is on. A repo-root DMG mirror is not part of the recipe and is not needed: `mirror-root-artifact`
(`right-release mirror-root-artifact --file <dmg> --package-root <app>`) only copies one regular file into
`RIGHT_RELEASE_ARTIFACT_DIR` and refuses to write repository-root output. Use it only if a downstream step needs the DMG in the
managed directory; do not reintroduce root mirrors.

### Sign-only, identity override, resume

- Identity: release.mjs has no `sign.identity` key. The Developer ID identity comes from `APPLE_DEVELOPER_ID` (the exact
  `Developer ID Application: ...` string) when set, else the single Developer ID Application identity in the keychain. Several
  identities and no `APPLE_DEVELOPER_ID` is an error. The notarytool credentials are `notarize.profile`, or
  `APPLE_API_KEY_PATH` + `APPLE_API_KEY` + `APPLE_API_ISSUER`.
- Sign-only / resume: `right-release release --platform mac --resume-from <phase>` with phase one of `prepackage`, `sign`,
  `package`, `post-sign`, `harden`. `--resume-from sign` skips the `prePackage` (build and cleanup) step and signs the app that
  is already on disk; later phases run as usual.
- Icon verification: release.mjs has no icon check. Keep it in `postSign` or the app's own test.

### Built-in cleanup (`macPackage.cleanup`)

`cleanup: { staleApps, staleDmgVolumes, lsclean }` (booleans, all optional). When any is true, `prePackage` runs through
`recipe-build`, which performs them as built-in steps before the build; there is no app script:

| Key | Step |
| --- | --- |
| `staleDmgVolumes` | detach `/Volumes/<name>` and `/Volumes/<name> <N>` (the macOS duplicate suffix) for the product name (the `notarize.app` basename without `.app`) or `dmg.volname`, but only a volume that `hdiutil info -plist` reports as a mounted disk image; a plain `hdiutil detach` first, `-force` only on retry. A real disk, share or similarly named volume is never touched |
| `lsclean` | `lsregister -u <path>` for every stale `*.app` in the bundle directory (the directory holding `notarize.app`); never `lsregister -kill` |
| `staleApps` | delete every stale `*.app` in the bundle directory |

When both `lsclean` and `staleApps` are set the app is unregistered first, then deleted. A detach or unregister that fails is
logged and skipped; the build's own gates decide pass or fail. The volume and `lsregister` steps are skipped off macOS.

### Profile, extra Tauri config, prepare and postBuild

`macPackage` takes `profile` (the trailing `-- --profile <p>` form), `tauriConfig` (an app-relative `.json`) and `prepare`
(same meaning as in WIN-PACKAGE-RECIPE.md) and `postBuild`: `[{ cmd, args?, cwd? }]` argv commands run after `tauri build` and
before release.mjs signs, for example embedding a dylib with a symlink-preserving copy (`ditto`, or `cp -a`).

```js
macPackage: {
  universal: true, profile: "release",
  tauriConfig: "src-tauri/tauri.release.json",
  prepare: [{ cmd: "node", args: ["scripts/stage-pdfium.mjs"] }, { cmd: "right-release", args: ["stage-sidecar", "--target", "universal-apple-darwin"] }],
  postBuild: [{ cmd: "ditto", args: ["libpdfium.dylib", "src-tauri/target/universal-apple-darwin/release/bundle/macos/App.app/Contents/Frameworks/libpdfium.dylib"] }],
}
```

With `prepare`, `postBuild` or `cleanup`, `prePackage` is `node recipe-build.mjs --plan <json>`: cleanup, prepare, the Tauri build,
postBuild, in order, failing closed on the first non-zero exit. Without them it is the plain `pnpm exec tauri build`.

### `right-release` inside prepare, build and postBuild

A step whose `cmd` is `right-release` (also `pnpm exec right-release ...` and `pnpm right-release ...`) always launches THIS
package's own CLI (`process.execPath <package>/cli/right-release.mjs <args>`), never the copy in the app's `node_modules`, which
`pnpm exec` would otherwise put first on PATH. It is the same rule `development.targets.*.build` steps follow. Other `pnpm` steps
still run through the pinned pnpm CLI.

### Universal sidecars (externalBin): `macPackage.stageSidecars`

Tauri's universal build looks for every `externalBin` as `binaries/<name>-universal-apple-darwin`, one fat Mach-O. The release
config's declarative `sidecars` (`cargo-bin`, `command`, `prebuilt`, `ffmpeg`, `native-library`, ...) already stage a universal
target: `right-release stage-sidecar --target universal-apple-darwin` builds the aarch64 and x86_64 slices of each `cargo-bin`
separately, `lipo -create`s them into `<name>-universal-apple-darwin`, and refuses a result that lacks `arm64` or `x86_64`.
`stageSidecars` is the declaration that runs it for the release lane:

```js
macPackage: {
  universal: true,
  stageSidecars: true,   // or { config: "right-release.config.mjs", receipt: "src-tauri/binaries/stage-sidecar.json" }
}
```

It prepends `right-release stage-sidecar --target <triple> [--config <file>]` as the first `prepare` step (so it runs before your
own `prepare` steps and before the Tauri build) and, for `universal-apple-darwin`, adds a check that runs right before the build:
the stage receipt must be for `universal-apple-darwin`, and every `externalBin` file in it must be named
`<name>-universal-apple-darwin` and report exactly `arm64 x86_64` from `lipo -archs` (a thin file, an extra architecture, an
unreadable receipt or a failing `lipo` stops the release before `tauri build`). Without `stageSidecars` nothing is staged for
you and a manual `right-release stage-sidecar` step in `prepare` stays valid. Non-`externalBin` outputs (resource-directory
dylibs, ffmpeg, onnxruntime) are staged per slice and lipo-merged by stage-sidecar itself and are checked there; the receipt check
covers `externalBin` only.

### Native Swift helpers: `macPackage.nativeHelpers`

Mac apps that ship Swift helpers inside the .app (menu bar helper, Shortcuts/App Intents helper, CloudKit helper, credentials
helper) declare them once. The list is shared by the dev lane and the release lane (`nativeHelpersFromConfig(config)` reads
`targets.mac.macPackage.nativeHelpers`). Each entry:

| Key | Meaning |
| --- | --- |
| `name` | unique label |
| `package` | app-relative SwiftPM package directory, committed in the app. A shared helper (cloudkit) is declared with `product: "rightkit-cloudkit-helper"` and pinned by URL and tag through that package's own `Package.swift`; `sharedPackage` and `RIGHTKIT_SWIFT_ROOT` are retired (rightkit#1302) |
| `product` | SwiftPM executable product |
| `kind` | `app` (wrap the product as a helper .app) or `executable` (embed the bare binary) |
| `embed` | path inside the .app, under `Contents/Library/`, `Helpers/`, `XPCServices/`, `PlugIns/` or (executables only) `Contents/MacOS/<file>`; kind `app` ends in `.app` |
| `universal` | required boolean: `arm64` + `x86_64` when true, the host arch when false |
| `entitlements` | optional app-relative plist, applied to this helper only |
| `executable` | kind `app` only: file name in `Contents/MacOS` (default `product`), so a product can be renamed |
| `info` | kind `app` only, required: `{ bundleId, name?, displayName?, uiElement?, minimumSystemVersion? }`; `uiElement` is `LSUIElement`; version comes from the release config |
| `resources` | kind `app` only: `[{ from: <app-relative>, to: <path in Contents/Resources> }]` |
| `appIntents` | kind `app` only: `{ moduleName, deploymentTarget? }` runs `appintentsmetadataprocessor` and requires `Metadata.appintents/extract.actionsdata` |
| `provisioningProfile` | kind `app` only: app-relative profile copied to `<helper>.app/Contents/embedded.provisionprofile` |

```js
macPackage: {
  universal: true, stageSidecars: true,
  nativeHelpers: [
    { name: "menubar", package: "macos-native/Pkg", product: "MenuBar", kind: "app", embed: "Contents/Library/LoginItems/MenuBar.app", universal: true,
      info: { bundleId: "com.example.statusbar", uiElement: true }, resources: [{ from: "src-tauri/icons/tray", to: "tray" }] },
    { name: "cloud", package: "macos-native/Cloud", product: "rightkit-cloudkit-helper", kind: "app", embed: "Contents/Library/LoginItems/CloudHelper.app", universal: true,
      executable: "CloudHelper", info: { bundleId: "com.example.next", name: "CloudHelper", uiElement: true },
      entitlements: "src-tauri/entitlements-cloud.plist", provisioningProfile: "src-tauri/provisioning/App.provisionprofile" },
  ],
}
```

Unknown keys, absolute paths, `..`, backslashes, an `embed` outside the roots above, duplicate names, and nested or overlapping embed
paths are refused at expansion. On a universal build (`universal-apple-darwin`) every helper must declare `universal: true`; on a thin
triple every helper is built for that triple's arch.

Order in the release lane: (1) `tauri build` and the managed-target copy make the unsigned .app; (2) recipe-build runs the
`nativeHelpers` section: per helper in declared order, `swift build -c release --package-path <pkg> --product <product> --arch arm64
--arch x86_64` (one `--arch` per slice, always explicit, so SwiftPM uses its xcbuild layout and App Intents inputs are found), then
`swift build ... --show-bin-path`, a `lipo -archs` check that the slices are exactly the expected set, and the embed (Info.plist,
renamed executable, resources, App Intents metadata, provisioning profile). An embed path that already exists in the .app is an error.
(3) your `postBuild` steps; (4) release.mjs signs inside-out: the expansion adds each helper to `sign.nestedCode`, so signing walks
deepest path first (code inside a helper, then the helper .app with its own entitlements and the hardened runtime), then the
engine/dylibs/other nested code, then the app last; (5) notarization and the DMG as before. The helper's entitlements plist must
exist when `right-release doctor` validates the signing config, so an app that renders it must do so in an earlier step of its
own pipeline, not in `prepare`. `sign.nestedCode` / `sign.entitlements` must not redeclare a helper's `embed` path.

Dev lane (not wired into `development.mjs` yet): `native-helpers.mjs` exports `buildAndEmbedNativeHelpers({ appRoot, app,
helpers, version, forceArch, configuration: "debug", replaceExisting: true })`, `nativeHelperEntitlementMap(helpers)` and
`nativeHelpersFromConfig(config)`. Call the first between the dev `tauri build` and `installTauriDevId` (it embeds into the freshly
built .app; the dev signer already signs nested bundles deepest first), and merge the entitlement map into `install.entitlements`
inside `readDevSigningSettings`.

### Managed Cargo target roots

Artifact paths in the target are the paths you declare (`notarize.app`, `sign.prePackageFiles`), normally under
`src-tauri/target/<triple>/...`. With `cargoTargetRoot: true` (manifest `<tauriCwd>/src-tauri/Cargo.toml`) or
`cargoTargetRoot: "<app-relative Cargo.toml>"`, the expansion asks `cargo metadata` for `target_directory` and moves every declared
path that begins with `<manifest dir>/target/` there. When `CARGO_TARGET_DIR` is set and `<tauriCwd>/src-tauri/Cargo.toml`
exists, the same lookup runs without the key. What works:

- target directory inside the app (for example `CARGO_TARGET_DIR=.cargo-target`): paths are rewritten, and the build, signing
  set, cleanup bundle dir and `notarize.app` all agree;
- target directory outside the app but under a managed root (`resolveManagedRoots`, the broker-managed target directory on a
  brokered host; rightkit#924, release 0.6.2): declared paths stay at their default app-relative location under
  `src-tauri/target/`, and the built `.app` is copied there after the build (sha256-verified, stale destination deleted first,
  atomic rename). The signing set, `notarize.app`, stapling, DMG, updater, cleanup and `lsregister` entries all see that
  app-relative copy, so nothing is written outside the app root or the managed target directory;
- target directory outside the app and outside every managed root: fails closed with a clear message. Paths you wrote outside the
  default `target/` prefix are never touched;
- `RIGHT_RELEASE_ARTIFACT_DIR` is a separate mechanism: release.mjs falls back to a file's basename there only when the declared
  path does not exist. It is not a build output location.

Your own `prepare` and `postBuild` commands are not rewritten; point them at the real directory.

### Scans that run for you

Before signing, release.mjs scans the macOS signing set (and the staged app before it is packed into the DMG) with
`assertNoQaControlBinaries`: the QA-control marker, the wdio webdriver plugin, the shell dev-inspect marker, remote-debugging
switches and Tauri devtools (allow-listed apps only) in every Mach-O, then the D4 release-switch binary layer
(`assertNoReleaseSwitches`: the test-mode marker and registered S1-S4 selector names, plus `config.releaseSwitches.forbid`).
`config.releaseSwitches` additionally drives the source layer (feature closure through `cargo tree`, and the `switches.json`
inventory), and the frontend `dist` is scanned for browser-QA mocks. That is the whole release-switch and QA-control gate; an
app's own `check-release-test-switches` script is redundant for what these cover. What it does not cover: a compressed DMG after
packing (the raw app is scanned before), symlinks, browser JS (source layer only) and any app-specific string rule that is not in
`switches.json` or `releaseSwitches.forbid`.

The recipes never touch `development.targets.mac.build` (the development lane in `development.mjs` does not call
`expandPackageRecipeTarget`; only release.mjs does). `--features dev-control` there is unaffected.

### Other release.mjs keys

- Post-sign smoke: `targets.mac.postSign` (`{ cmd, args }`, or an array), run after signing and before notarization of the DMG
  (for example `verify-macos-release-artifacts.mjs`).
- DMG layout: `targets.mac.dmg: { spec: "assets/dmg.json" }`, an appdmg-style JSON read by `dmg-spec.mjs`: `title`, `icon`,
  `background`, `icon-size`, `window.size {width,height}`, `window.position {x,y}`, and `contents` with the `.app` position and
  one `/Applications` link. Extra files, licences and other formats are refused by that parser.
- notarytool variants: `notarize.profile` selects a Keychain profile (`--keychain-profile`); without it `notary-auth.mjs`
  uses `APPLE_API_KEY_PATH` + `APPLE_API_KEY` + `APPLE_API_ISSUER` together (`--key --key-id --issuer`), else the default
  `apple-dev-notary` profile. `notarize.staple: false` and `notarize.appBeforePackage` exist. There is no key to
  pass arbitrary notarytool flags.

Migration: delete `prePackage` and `package` (the mac-dmg.mjs hook), add `macPackage`, move DMG branding to `targets.mac.dmg`,
keep `create-mac-updater` in `postSign`, and keep per-app native-helper embedding in the Tauri build or `postBuild`.

---

## Standalone runMacPackageRecipe

Everything below is for **direct callers** of `runMacPackageRecipe` (`packages/release/mac-package-recipe.mjs`). Its `macPackage`
object has different keys (`dmg`, `notarize`, `updater`, `nested`, `entitlements`) from the release target above; do not copy them
into `right-release.config.mjs`.

One RightKit entrypoint packages every macOS app (bogusyogi/rightkit#345). A mac target that declares `macPackage` and
has no `prePackage` / `package` hook scripts is signed, packed, notarized and given an updater by
`packages/release/mac-package-recipe.mjs` (`runMacPackageRecipe`). It replaces the per-app `mac-dmg.mjs` /
`mac-payload.mjs` hooks.

The recipe starts from a built `.app`. Building it (Tauri, SwiftPM, the engine) stays the app's own build step; the recipe
owns everything after that.

### Config schema

```js
targets: {
  mac: {
    signingContract: "...",            // unchanged
    notarize: {
      app: "target/universal-apple-darwin/release/bundle/macos/HeardRight.app",   // the .app the recipe signs and packs
      file: "HeardRight_1.2.3_universal.dmg",                                     // the .dmg it produces
    },
    macPackage: {
      universal: true,                 // optional, default false
      dmg: {
        name: "HeardRight",            // required: volume name (no NUL, CR, LF, "/" or ":")
        background: "assets/dmg-bg.png",        // optional: PNG or TIFF, relative to the app root
        windowSize: [660, 400],                  // optional: [width, height], positive integers
        iconPositions: { app: [158, 231], applications: [492, 231] },  // optional: [x, y]
      },
      notarize: true,                  // required: true | false
      updater: true,                   // required: true | false
      nested: ["Contents/Frameworks/**/*.dylib"],   // optional: extra code to sign, globs relative to the .app
      entitlements: "src-tauri/entitlements.plist", // optional: applied to the app only, never to nested code
    },
  },
}
```

Validation is strict and fails closed with `MacPackageRecipeError` (`code: "MAC_PACKAGE_CONFIG_INVALID"`):

- unknown keys are rejected at every level;
- `notarize` and `updater` must be booleans (no default, so the intent is always written down);
- `prePackage`, `package` and the legacy `targets.mac.dmg` block must be absent;
- `notarize.app` must end in `.app` and `notarize.file` in `.dmg`;
- `nested` globs are bundle-relative, forward-slash, with only `*`, `**` and `?`; no `..`, no absolute paths; every
  glob must match at least one file or the run stops.

Other error codes: `MAC_PACKAGE_HOST_UNSUPPORTED` (not macOS), `MAC_PACKAGE_INPUT_MISSING`,
`MAC_PACKAGE_UNIVERSAL_FAILED`, `MAC_PACKAGE_SIGN_FAILED`, `MAC_PACKAGE_DMG_FAILED`, `MAC_PACKAGE_NOTARIZE_FAILED`,
`MAC_PACKAGE_UPDATER_FAILED`.

### Step order

| # | Step | Does | Skipped when |
| - | ---- | ---- | ------------ |
| 1 | `universal` | `lipo -archs` on the main executable and every nested Mach-O; each must hold `x86_64` and `arm64` | `universal` is false |
| 2 | `sign-nested` | Developer ID, hardened runtime, timestamp, no entitlements, deepest path first (auto-discovered nested code plus `nested` globs) | no nested code |
| 3 | `sign-app` | Developer ID, hardened runtime, timestamp, `entitlements` if set | never |
| 4 | `dmg` | branded DMG from the signed app (`dmg-build`) | never |
| 5 | `sign-dmg` | Developer ID with a secure timestamp, no hardened runtime | never |
| 6 | `notarize` | submit the DMG, wait, staple and validate the DMG and the app (`notarizeMacosArtifact`) | `notarize` is false |
| 7 | `updater` | `tar.gz` of the stapled app's `Contents` as `<app>.tar.gz`, then its updater signature `<app>.tar.gz.sig` | `updater` is false |

Inside-out signing means the code a bundle contains is always signed before the bundle. The universal step only
verifies: a thin slice compiles, signs and notarizes, then fails on half the users' Macs. Merging per-arch trees
(`tauri build --target universal-apple-darwin`) stays in the app's build step.

Fail closed: after the DMG is built, any failure removes the DMG and any updater files, so a later seal cannot pick up an
artifact that skipped a step. Every step failure is a `MacPackageRecipeError` with the original error as `cause`.

Signing and notarization route through the existing session logic (`macos-signing-session`), so an ssh session uses the
GUI signing agent and the keychain profile as before. The notarization receipt is written to
`$RIGHT_RELEASE_STATE_ROOT/receipts/<app>-mac-notarization.json`, or `<appRoot>/.right-release/receipts/` when unset.

### Migrating an app to the standalone recipe

1. Delete `targets.mac.prePackage` and `targets.mac.package` from the config.
2. Delete `targets.mac.dmg` if present; its options move to `macPackage.dmg`.
3. Add `macPackage` as above. Copy the DMG volume name, background, window size and icon positions from the old hook
   (`brandedDmg` / `bundle_dmg.sh` arguments), and set `notarize` and `updater` to what the hook did on a release build.
4. Keep `notarize.app` and `notarize.file` (they now name the recipe's input and output).
5. Move nested signing paths the old hook listed explicitly into `nested`; the common `.framework`, `.xpc`, `.dylib` and
   Mach-O cases under `Contents` are found automatically.
6. Delete the app's `scripts/mac-dmg.mjs` (or `mac-payload.mjs`) and its `lib/branded-dmg.mjs`, `sign-macos-bundle.mjs`
   copies once nothing else imports them. Anything the hook did besides packaging (legal-fact checks, embedding helpers,
   provisioning profiles) belongs in the app's build step that produces the `.app`, not in the recipe.
7. Run the release on the Mac and compare the signing records and receipt with the last hook-built release.

For a `right-release.config.mjs` target, use the release target schema at the top of this document instead.
