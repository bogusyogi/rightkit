# macOS package recipe (`macPackage`)

> **In `right-release.config.mjs` (what `right-release release --platform mac` runs).** `targets.mac.macPackage`
> is expanded by `packages/release/package-recipe-targets.mjs` into the hook shape release.mjs already gates, so
> every existing check (QA-control scans, Developer ID signing of the .app and nested code, app notarization and
> stapling, the signed DMG and its notarization) runs unchanged. The release-target schema is:
>
> ```js
> mac: {
>   signed: true, signingContract: "macos-developer-id-notarized-portable-v1",
>   macPackage: { universal: true /* default */, tauriCwd: "." /* optional */, features: [] /* optional */ },
>   notarize: { app: "src-tauri/target/universal-apple-darwin/release/bundle/macos/App.app", file: "dist/App_universal.dmg", profile: "apple-dev-notary" },
>   dmg: { volname: "App", background: "assets/dmg.png" /* optional branding, as for SwiftPM */ },
>   sign: { nestedCode: { /* optional, as today */ } },   // prePackageFiles defaults to [notarize.app]
> }
> ```
>
> Migration: delete `prePackage` and `package` (the mac-dmg.mjs hook) and add `macPackage`. Not yet in the
> expansion: the updater tarball (keep `create-mac-updater` in `postSign` for now) and per-app native-helper
> embedding (keep that in the Tauri build). The standalone `runMacPackageRecipe` below is for direct callers.

One RightKit entrypoint packages every macOS app (bogusyogi/rightkit#345). A mac target that declares `macPackage` and
has no `prePackage` / `package` hook scripts is signed, packed, notarized and given an updater by
`packages/release/mac-package-recipe.mjs` (`runMacPackageRecipe`). It replaces the per-app `mac-dmg.mjs` /
`mac-payload.mjs` hooks.

The recipe starts from a built `.app`. Building it (Tauri, SwiftPM, the engine) stays the app's own build step; the recipe
owns everything after that.

## Config schema

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

## Step order

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

## Migrating an app

1. Delete `targets.mac.prePackage` and `targets.mac.package` from `right-release.config.mjs`.
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
